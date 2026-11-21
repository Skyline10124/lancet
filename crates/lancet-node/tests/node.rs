// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use std::collections::BTreeMap;
use std::io::Read as _;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::Duration;

use lancet_protocol::{
    Capabilities, CatalogDigest, EffectKind, EnvironmentSummary, Frame, Mode, ProtocolVersion,
    Request, Response, Seam, SeamDirection, SeamPolicy, ToolSpec, read_frame, write_frame,
};
use tempfile::TempDir;

const BIN: &str = env!("CARGO_BIN_EXE_lancet-node");

fn temp_env() -> (TempDir, PathBuf) {
    let root = tempfile::Builder::new()
        .prefix("ln-")
        .tempdir_in("/tmp")
        .expect("在 /tmp 创建临时目录失败");
    let home = root.path().join("home");
    std::fs::create_dir(&home).expect("创建 home 失败");
    (root, home)
}

struct NodeProcess {
    child: Child,
    sock_path: PathBuf,
}

impl NodeProcess {
    fn start(config_path: &Path, toml_content: &str, state_dir: &Path, home: &Path) -> Self {
        Self::start_with_timeout(
            config_path,
            toml_content,
            state_dir,
            home,
            Duration::from_secs(10),
        )
    }

    fn start_with_timeout(
        config_path: &Path,
        toml_content: &str,
        state_dir: &Path,
        home: &Path,
        timeout: Duration,
    ) -> Self {
        std::fs::write(config_path, toml_content).expect("写入测试配置文件失败");
        let mut child = Command::new(BIN)
            .arg(config_path)
            .env("HOME", home)
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .expect("启动 lancet-node 子进程失败");

        let sock_path = state_dir.join("node.sock");
        let deadline = std::time::Instant::now() + timeout;
        loop {
            match UnixStream::connect(&sock_path) {
                Ok(stream) => {
                    drop(stream);
                    return Self { child, sock_path };
                }
                Err(_) => {
                    if let Some(status) = child.try_wait().expect("检查子进程状态失败") {
                        let mut stderr = String::new();
                        if let Some(mut pipe) = child.stderr.take() {
                            let _ = pipe.read_to_string(&mut stderr);
                        }
                        panic!("节点提前退出（状态 {status}），标准错误：\n{stderr}");
                    }
                    if std::time::Instant::now() >= deadline {
                        break;
                    }
                    thread::sleep(Duration::from_millis(20));
                }
            }
        }

        let _ = child.kill();
        let _ = child.wait();
        panic!("在规定时间内未能连上套接字：{}", sock_path.display());
    }

    fn connect(&self) -> UnixStream {
        UnixStream::connect(&self.sock_path).expect("连接节点套接字失败")
    }
}

impl Drop for NodeProcess {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn run_expect_failure(config_path: &Path, toml_content: &str, home: &Path) -> String {
    std::fs::write(config_path, toml_content).expect("写入测试配置文件失败");
    let mut child = Command::new(BIN)
        .arg(config_path)
        .env("HOME", home)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("启动 lancet-node 子进程失败");

    // 有限等待：节点若意外启动成功会一直监听，不能让测试挂起。
    for _ in 0..100 {
        if let Some(status) = child.try_wait().expect("检查子进程状态失败") {
            assert!(!status.success(), "节点应当启动失败");
            let mut stderr = String::new();
            child
                .stderr
                .take()
                .expect("缺少标准错误管道")
                .read_to_string(&mut stderr)
                .expect("读取标准错误失败");
            return stderr;
        }
        thread::sleep(Duration::from_millis(20));
    }

    let _ = child.kill();
    let _ = child.wait();
    panic!("节点应当启动失败，却在规定时间内仍未退出");
}

fn send_request(stream: &mut UnixStream, request_id: u64, message: Request) -> Frame<Response> {
    write_frame(
        stream,
        &Frame {
            request_id,
            message,
        },
    )
    .expect("发送请求帧失败");
    read_frame(stream)
        .expect("读取回复帧失败")
        .expect("期望收到回复帧，却遇到 EOF")
}

struct HelloInfo {
    version: ProtocolVersion,
    node_id: String,
    display_name: String,
    capabilities: Capabilities,
}

fn hello_v1(stream: &mut UnixStream, request_id: u64) -> HelloInfo {
    let resp = send_request(
        stream,
        request_id,
        Request::Hello {
            supported_versions: vec![ProtocolVersion::V1],
        },
    );
    assert_eq!(resp.request_id, request_id);
    match resp.message {
        Response::HelloOk {
            version,
            node_id,
            display_name,
            capabilities,
        } => HelloInfo {
            version,
            node_id,
            display_name,
            capabilities,
        },
        other => panic!("期望 HelloOk，实际为：{other:?}"),
    }
}

fn describe(
    stream: &mut UnixStream,
    request_id: u64,
) -> (CatalogDigest, EnvironmentSummary, Vec<ToolSpec>) {
    let resp = send_request(stream, request_id, Request::Describe);
    assert_eq!(resp.request_id, request_id);
    match resp.message {
        Response::DescribeOk {
            catalog_digest,
            environment_summary,
            tools,
        } => (catalog_digest, environment_summary, tools),
        other => panic!("期望 DescribeOk，实际为：{other:?}"),
    }
}

fn seam(from: &str, to: &str, direction: SeamDirection) -> Seam {
    Seam {
        from: from.to_owned(),
        to: to.to_owned(),
        direction,
        policy: SeamPolicy::Allow,
    }
}

#[test]
fn hello_describe_matches_config() {
    let (root, home) = temp_env();
    let app_dir = root.path().join("app");
    let lib_dir = root.path().join("lib");
    let docs_dir = root.path().join("docs");
    let state_dir = root.path().join("state");
    for d in [&app_dir, &lib_dir, &docs_dir] {
        std::fs::create_dir(d).expect("创建目录失败");
    }

    let config_path = root.path().join("node.toml");
    let toml_content = format!(
        r#"
[node]
display_name = "测试节点"
state_dir = {state_dir:?}
receipt_retention_ms = 86400000

[[dir]]
name = "app"
path = {app_dir:?}
access = "write"

[[dir]]
name = "lib"
path = {lib_dir:?}
access = "read"

[[dir]]
name = "docs"
path = {docs_dir:?}
access = "read"

[[env]]
name = "dev"
mounts = ["app", "lib"]
tools = ["~/.rustup"]
egress = "crates"

[[egress]]
name = "crates"
hosts = ["index.crates.io", "static.crates.io"]

[[egress]]
name = "pypi"
hosts = ["pypi.org"]
"#,
        state_dir = state_dir.to_str().unwrap(),
        app_dir = app_dir.to_str().unwrap(),
        lib_dir = lib_dir.to_str().unwrap(),
        docs_dir = docs_dir.to_str().unwrap(),
    );

    let node = NodeProcess::start(&config_path, &toml_content, &state_dir, &home);
    let mut stream = node.connect();

    let hello = hello_v1(&mut stream, 11);
    assert_eq!(hello.version, ProtocolVersion::V1);
    assert_eq!(hello.display_name, "测试节点");
    assert_eq!(
        hello.capabilities,
        Capabilities {
            settle_grace_ms: 5_000,
            receipt_retention_ms: 86_400_000,
        }
    );
    assert!(
        uuid::Uuid::parse_str(&hello.node_id).is_ok(),
        "node_id 应能解析为 UUID：{}",
        hello.node_id
    );

    let (_, environment_summary, tools) = describe(&mut stream, 22);
    assert_eq!(tools.len(), 1, "tools 应恰为一个 act 工具");
    let act_tool = &tools[0];
    assert_eq!(act_tool.name, "act");
    assert_eq!(
        act_tool.input_schema,
        serde_json::json!({
            "type": "object",
            "properties": {
                "script": { "type": "string" }
            },
            "required": ["script"],
            "additionalProperties": false
        }),
        "act 的 input_schema 不符"
    );
    assert!(act_tool.effects.is_empty(), "act 的 effects 应为空");
    assert!(act_tool.coeffects.is_empty(), "act 的 coeffects 应为空");
    for expected in [
        "sum",
        "regex",
        "match",
        "findall",
        "replace",
        "text",
        "TextView",
        "grep",
        "head",
        "tail",
        "splitlines",
        "json",
        "set",
        "struct",
        "map",
        "filter",
        "print",
        "before: int = 0",
        "after: int = 0",
        "keepends: bool = False",
    ] {
        assert!(
            act_tool.description.contains(expected),
            "act.description 应包含 `{expected}`，实际为：\n{}",
            act_tool.description
        );
    }
    assert!(
        !act_tool.description.contains("def len("),
        "紧凑存根不应包含标准内置 `def len(`，实际为：\n{}",
        act_tool.description
    );
    assert!(
        act_tool
            .description
            .lines()
            .any(|line| line == "    def head(self, n: int) -> TextView:"),
        "TextView 方法应带 self 且签名匹配，实际为：\n{}",
        act_tool.description
    );
    let set_block = act_tool
        .description
        .split("\n\n")
        .find(|block| block.starts_with("class set:\n"))
        .expect("存根应包含 `class set:` 块");
    assert!(
        set_block.contains("\n    def __init__(self"),
        "`class set:` 内应包含带 self 的 `__init__`，实际为：\n{set_block}"
    );
    assert!(
        !act_tool.description.contains("struct(..)"),
        "存根不应包含非法返回类型 `struct(..)`，实际为：\n{}",
        act_tool.description
    );
    assert!(
        !act_tool
            .description
            .lines()
            .any(|line| line.starts_with("def set(")),
        "存根行首不应再单独出现 `def set(`，实际为：\n{}",
        act_tool.description
    );
    for expected in [
        "class Entry:",
        "is_link: bool",
        "class SearchHit:",
        "line_no: int",
    ] {
        assert!(
            act_tool.description.contains(expected),
            "act.description 应包含 `{expected}`，实际为：\n{}",
            act_tool.description
        );
    }
    // 已删除的内置函数只按行首判定：Starlark `set` 自带的 `discard` 方法缩进出现在存根里，不是被删除的全局函数。
    for removed in ["def commit(", "def discard("] {
        assert!(
            !act_tool
                .description
                .lines()
                .any(|line| line.starts_with(removed)),
            "act.description 行首不应出现已删除的内置函数 `{removed}`，实际为：\n{}",
            act_tool.description
        );
    }
    for removed in [
        "class PendingChange:",
        "class PathDiff:",
        "def status(",
        "def diff(",
        "def reset(",
    ] {
        assert!(
            !act_tool.description.contains(removed),
            "act.description 不应包含已删除项 `{removed}`，实际为：\n{}",
            act_tool.description
        );
    }
    let read_dir_block = act_tool
        .description
        .split("\n\n")
        .find(|block| block.starts_with("class ReadDir:\n"))
        .expect("存根应包含 `class ReadDir:` 块");
    for method in ["def read(", "def list(", "def search("] {
        assert!(
            read_dir_block.contains(method),
            "`class ReadDir:` 应包含 `{method}`，实际为：\n{read_dir_block}"
        );
    }
    let write_dir_block = act_tool
        .description
        .split("\n\n")
        .find(|block| block.starts_with("class WriteDir:\n"))
        .expect("存根应包含 `class WriteDir:` 块");
    for method in [
        "def read(",
        "def list(",
        "def search(",
        "def write(",
        "def edit(",
    ] {
        assert!(
            write_dir_block.contains(method),
            "`class WriteDir:` 应包含 `{method}`，实际为：\n{write_dir_block}"
        );
    }

    let handles = environment_summary.handles();
    let names: Vec<&str> = handles.iter().map(|h| h.name.as_str()).collect();
    assert_eq!(names, ["app", "lib", "docs", "dev", "crates", "pypi"]);

    for h in handles {
        match h.kind.as_str() {
            "ReadDir" => {
                assert_eq!(
                    h.modes,
                    vec![Mode::Read],
                    "只读目录句柄 {} 的 modes 应为 [Read]",
                    h.name
                );
                assert!(
                    h.effects.is_empty(),
                    "只读目录句柄 {} 的 effects 应为空",
                    h.name
                );
            }
            "WriteDir" => {
                assert_eq!(
                    h.modes,
                    vec![Mode::Read, Mode::ReadWrite],
                    "可写目录句柄 {} 的 modes 应为 [Read, ReadWrite]",
                    h.name
                );
                assert_eq!(
                    h.effects,
                    vec![EffectKind::Write],
                    "可写目录句柄 {} 的 effects 应为 [Write]",
                    h.name
                );
            }
            _ => {
                assert!(h.modes.is_empty(), "未注入句柄 {} 的 modes 应为空", h.name);
                assert!(
                    h.effects.is_empty(),
                    "未注入句柄 {} 的 effects 应为空",
                    h.name
                );
            }
        }
    }

    assert_eq!(handles[0].kind, "WriteDir");
    assert_eq!(handles[0].attributes, BTreeMap::new());

    assert_eq!(handles[1].kind, "ReadDir");
    assert_eq!(handles[1].attributes, BTreeMap::new());

    assert_eq!(handles[2].kind, "ReadDir");
    assert_eq!(handles[2].attributes, BTreeMap::new());

    assert_eq!(handles[3].kind, "ExecEnv");
    let expanded_tool = home.join(".rustup").to_string_lossy().into_owned();
    assert_eq!(
        handles[3].attributes,
        BTreeMap::from([
            ("mounts".to_owned(), serde_json::json!(["app", "lib"])),
            ("tools".to_owned(), serde_json::json!([expanded_tool])),
        ])
    );

    assert_eq!(handles[4].kind, "Egress");
    assert_eq!(
        handles[4].attributes,
        BTreeMap::from([(
            "hosts".to_owned(),
            serde_json::json!(["index.crates.io", "static.crates.io"])
        )])
    );

    assert_eq!(handles[5].kind, "Egress");
    assert_eq!(
        handles[5].attributes,
        BTreeMap::from([("hosts".to_owned(), serde_json::json!(["pypi.org"]))])
    );

    let expected_seams = [
        seam("app", "dev", SeamDirection::Integrity),
        seam("app", "crates", SeamDirection::Confidentiality),
        seam("lib", "crates", SeamDirection::Confidentiality),
        seam("docs", "crates", SeamDirection::Confidentiality),
        seam("dev", "crates", SeamDirection::Confidentiality),
        seam("app", "pypi", SeamDirection::Confidentiality),
        seam("lib", "pypi", SeamDirection::Confidentiality),
        seam("docs", "pypi", SeamDirection::Confidentiality),
        seam("dev", "pypi", SeamDirection::Confidentiality),
    ];
    assert_eq!(environment_summary.seams(), &expected_seams);
}

#[test]
fn restart_preserves_identity_and_catalog() {
    let (root, home) = temp_env();
    let app_dir = root.path().join("app");
    let state_dir = root.path().join("state");
    std::fs::create_dir(&app_dir).expect("创建 app 失败");

    let config_path = root.path().join("node.toml");
    let toml_content = format!(
        r#"
[node]
display_name = "重启节点"
state_dir = {state_dir:?}

[[dir]]
name = "app"
path = {app_dir:?}
access = "write"
"#,
        state_dir = state_dir.to_str().unwrap(),
        app_dir = app_dir.to_str().unwrap(),
    );

    let node_a = NodeProcess::start(&config_path, &toml_content, &state_dir, &home);
    let mut stream_a = node_a.connect();
    let hello_a = hello_v1(&mut stream_a, 1);
    assert_eq!(
        hello_a.capabilities.receipt_retention_ms, 2_592_000_000,
        "默认回执保留期应为 30 天"
    );
    let (digest_a, _, _) = describe(&mut stream_a, 2);
    drop(stream_a);

    // 节点 A 运行期间启动节点 B，应因状态目录独占锁失败退出。
    let stderr_b = run_expect_failure(&config_path, &toml_content, &home);
    assert!(
        stderr_b.contains("正被另一个节点进程使用"),
        "节点 B 标准错误应说明状态目录被占用，实际为：\n{stderr_b}"
    );

    // 杀掉并回收 A，此时 state_dir 内留有 A 的 node.sock。
    drop(node_a);
    assert!(
        state_dir.join("node.sock").exists(),
        "杀掉 A 后应保留残留的 node.sock 供 C 清理"
    );

    let node_c = NodeProcess::start(&config_path, &toml_content, &state_dir, &home);
    let mut stream_c = node_c.connect();
    let hello_c = hello_v1(&mut stream_c, 1);
    let (digest_c, _, _) = describe(&mut stream_c, 2);

    assert_eq!(
        hello_c.node_id, hello_a.node_id,
        "重启后 node_id 应保持不变"
    );
    assert_eq!(digest_c, digest_a, "重启后 catalog_digest 应保持不变");
    let persisted_id = std::fs::read(state_dir.join("node_id")).expect("读取 node_id 失败");
    let expected_id = format!(
        "{}\n",
        uuid::Uuid::parse_str(&hello_a.node_id)
            .expect("node_id 应为合法 UUID")
            .hyphenated()
    );
    assert_eq!(
        persisted_id,
        expected_id.as_bytes(),
        "node_id 文件应为一行小写带连字符的 UUID 并以换行结束"
    );
    assert!(
        !state_dir.join("shadows").exists(),
        "节点不应建立 shadows/ 目录"
    );
    assert!(
        !state_dir.join("publish").exists(),
        "节点不应建立 publish/ 目录"
    );
}

#[test]
fn digest_tracks_non_displayed_grants() {
    let (root, home) = temp_env();
    let app_dir = root.path().join("app");
    let state_dir = root.path().join("state");
    std::fs::create_dir(&app_dir).expect("创建 app 失败");

    let make_toml = |egress: &str| {
        format!(
            r#"
[node]
display_name = "摘要节点"
state_dir = {state_dir:?}

[[dir]]
name = "app"
path = {app_dir:?}
access = "write"

[[env]]
name = "dev"
mounts = ["app"]
egress = {egress:?}

[[egress]]
name = "crates"
hosts = ["index.crates.io"]

[[egress]]
name = "pypi"
hosts = ["pypi.org"]
"#,
            state_dir = state_dir.to_str().unwrap(),
            app_dir = app_dir.to_str().unwrap(),
        )
    };

    let config_path = root.path().join("node.toml");
    let node_a = NodeProcess::start(&config_path, &make_toml("crates"), &state_dir, &home);
    let mut stream_a = node_a.connect();
    let hello_a = hello_v1(&mut stream_a, 1);
    let (digest_a, summary_a, tools_a) = describe(&mut stream_a, 2);
    drop(stream_a);
    drop(node_a);

    let node_b = NodeProcess::start(&config_path, &make_toml("pypi"), &state_dir, &home);
    let mut stream_b = node_b.connect();
    let hello_b = hello_v1(&mut stream_b, 1);
    let (digest_b, summary_b, tools_b) = describe(&mut stream_b, 2);

    assert_eq!(
        hello_a.node_id, hello_b.node_id,
        "同状态目录重启后 node_id 应相同"
    );
    assert_eq!(summary_a, summary_b, "两者的 environment_summary 应一致");
    assert_eq!(tools_a, tools_b, "两者的 tools 应一致");
    assert_ne!(
        digest_a, digest_b,
        "env.egress 变化未出现在环境简述中，但必须改变 catalog_digest"
    );
}

#[test]
fn startup_rejects_overlapping_dirs() {
    let (root, home) = temp_env();
    let x_dir = root.path().join("x");
    let dir_a = x_dir.join("a");
    let dir_sub = dir_a.join("sub");
    let dir_ab = x_dir.join("ab");
    let dir_link = x_dir.join("link_to_a");
    for d in [&x_dir, &dir_a, &dir_sub, &dir_ab] {
        std::fs::create_dir(d).expect("创建目录失败");
    }
    std::os::unix::fs::symlink(&dir_a, &dir_link).expect("创建符号链接失败");

    let cases = [
        ("nested", "outer", &dir_a, "inner", &dir_sub),
        ("symlink", "orig", &dir_a, "alias", &dir_link),
    ];

    for (case_name, name_1, path_1, name_2, path_2) in cases {
        let state_dir = root.path().join(format!("state-{case_name}"));
        let config_path = root.path().join(format!("{case_name}.toml"));
        let toml_content = format!(
            r#"
[node]
display_name = "重叠测试"
state_dir = {state_dir:?}

[[dir]]
name = {name_1:?}
path = {path_1:?}
access = "read"

[[dir]]
name = {name_2:?}
path = {path_2:?}
access = "write"
"#,
            state_dir = state_dir.to_str().unwrap(),
            path_1 = path_1.to_str().unwrap(),
            path_2 = path_2.to_str().unwrap(),
        );
        let stderr = run_expect_failure(&config_path, &toml_content, &home);
        assert!(
            stderr.contains("工作目录互不重叠")
                && stderr.contains(name_1)
                && stderr.contains(name_2),
            "用例 `{case_name}` 应报规则与两个目录名，实际标准错误：\n{stderr}"
        );
    }

    // 同前缀但分量不同的目录（x/a 与 x/ab）不重叠，应正常启动。
    let state_ok = root.path().join("state-ok");
    let cfg_ok = root.path().join("ok.toml");
    let toml_ok = format!(
        r#"
[node]
display_name = "前缀测试"
state_dir = {state_dir:?}

[[dir]]
name = "a"
path = {dir_a:?}
access = "write"

[[dir]]
name = "ab"
path = {dir_ab:?}
access = "read"
"#,
        state_dir = state_ok.to_str().unwrap(),
        dir_a = dir_a.to_str().unwrap(),
        dir_ab = dir_ab.to_str().unwrap(),
    );
    let node = NodeProcess::start(&cfg_ok, &toml_ok, &state_ok, &home);
    let mut stream = node.connect();
    let _ = hello_v1(&mut stream, 1);
}

#[test]
fn startup_keeps_config_and_state_outside_dirs() {
    let (root, home) = temp_env();
    let app_dir = root.path().join("app");
    std::fs::create_dir(&app_dir).expect("创建 app 失败");

    // 情形 1：配置文件位于工作目录内。
    let cfg_inside = app_dir.join("node.toml");
    let state_outside = root.path().join("state-outside");
    let err_cfg = run_expect_failure(
        &cfg_inside,
        &format!(
            r#"
[node]
display_name = "位置校验"
state_dir = {state_dir:?}

[[dir]]
name = "app"
path = {app_dir:?}
access = "write"
"#,
            state_dir = state_outside.to_str().unwrap(),
            app_dir = app_dir.to_str().unwrap(),
        ),
        &home,
    );
    let real_cfg_inside = std::fs::canonicalize(&cfg_inside).expect("规范化配置文件路径失败");
    assert!(
        err_cfg.contains("节点配置文件与节点状态目录在所有工作目录之外")
            && err_cfg.contains(real_cfg_inside.to_str().unwrap())
            && err_cfg.contains("app"),
        "应报规则、违规配置文件真实路径与目录名，实际标准错误：\n{err_cfg}"
    );

    // 情形 2：state_dir 指向工作目录内尚不存在的路径，失败后该路径仍不得存在。
    let cfg_outside = root.path().join("node.toml");
    let state_inside = app_dir.join("nested-state");
    let err_state = run_expect_failure(
        &cfg_outside,
        &format!(
            r#"
[node]
display_name = "位置校验"
state_dir = {state_dir:?}

[[dir]]
name = "app"
path = {app_dir:?}
access = "write"
"#,
            state_dir = state_inside.to_str().unwrap(),
            app_dir = app_dir.to_str().unwrap(),
        ),
        &home,
    );
    let real_state_inside = std::fs::canonicalize(&app_dir)
        .expect("规范化工作目录路径失败")
        .join("nested-state");
    assert!(
        err_state.contains("节点配置文件与节点状态目录在所有工作目录之外")
            && err_state.contains(real_state_inside.to_str().unwrap())
            && err_state.contains("app"),
        "应报规则、违规状态目录真实路径与目录名，实际标准错误：\n{err_state}"
    );
    assert!(
        !state_inside.exists(),
        "启动校验失败后，尚不存在的 state_dir 不应被创建"
    );

    // 情形 3：把已存在的状态目录本身授为可写工作目录，启动报错。
    let existing_state = root.path().join("existing-state");
    let state_calls = existing_state.join("calls");
    std::fs::create_dir_all(&state_calls).expect("创建状态目录与 calls 失败");
    let cfg_state_self = root.path().join("state-self.toml");
    let err_state_self = run_expect_failure(
        &cfg_state_self,
        &format!(
            r#"
[node]
display_name = "状态目录自身可写"
state_dir = {state_dir:?}

[[dir]]
name = "st"
path = {state_dir:?}
access = "write"
"#,
            state_dir = existing_state.to_str().unwrap(),
        ),
        &home,
    );
    assert!(
        err_state_self.contains("可写工作目录不能是节点状态目录或其子目录")
            && err_state_self.contains("st"),
        "状态目录本身授为可写工作目录应报启动校验失败，实际标准错误：\n{err_state_self}"
    );

    // 情形 4：把状态目录的子目录（如 state_dir/calls）授为可写工作目录，启动报错。
    let cfg_state_sub = root.path().join("state-sub.toml");
    let err_state_sub = run_expect_failure(
        &cfg_state_sub,
        &format!(
            r#"
[node]
display_name = "状态目录子目录可写"
state_dir = {state_dir:?}

[[dir]]
name = "calls_dir"
path = {calls_dir:?}
access = "write"
"#,
            state_dir = existing_state.to_str().unwrap(),
            calls_dir = state_calls.to_str().unwrap(),
        ),
        &home,
    );
    assert!(
        err_state_sub.contains("可写工作目录不能是节点状态目录或其子目录")
            && err_state_sub.contains("calls_dir"),
        "状态目录子目录授为可写工作目录应报启动校验失败，实际标准错误：\n{err_state_sub}"
    );
}

#[test]
fn startup_rejects_removed_dir_rule_keys() {
    let (root, home) = temp_env();
    let app_dir = root.path().join("app");
    std::fs::create_dir(&app_dir).expect("创建 app 失败");

    for (key, line) in [
        ("hidden", "hidden = [\".env\"]"),
        ("regenerable", "regenerable = [\"target/\"]"),
    ] {
        let state_dir = root.path().join(format!("state-{key}"));
        let cfg = root.path().join(format!("bad-{key}.toml"));
        let toml_content = format!(
            r#"
[node]
display_name = "旧键拒绝"
state_dir = {state_dir:?}

[[dir]]
name = "app"
path = {app_dir:?}
access = "write"
{line}
"#,
            state_dir = state_dir.to_str().unwrap(),
            app_dir = app_dir.to_str().unwrap(),
        );
        let stderr = run_expect_failure(&cfg, &toml_content, &home);
        assert!(
            stderr.contains(key),
            "配置中出现已删除键 `{key}` 应启动报错并指出该键，实际标准错误：\n{stderr}"
        );
    }
}

#[test]
fn startup_allows_writable_dir_on_different_filesystem() {
    let (xfs_root, home) = temp_env();
    let shm_root = tempfile::Builder::new()
        .prefix("ln-")
        .tempdir_in("/dev/shm")
        .expect("在 /dev/shm 创建临时目录失败");

    let shm_dir = shm_root.path().join("dir");
    let xfs_dir = xfs_root.path().join("dir");
    std::fs::create_dir(&shm_dir).expect("创建 shm dir 失败");
    std::fs::create_dir(&xfs_dir).expect("创建 xfs dir 失败");

    // 情形 1：可写目录在 /dev/shm，状态目录在 /tmp（不同文件系统且无 reflink）。
    let state_1 = xfs_root.path().join("state-1");
    let cfg_1 = xfs_root.path().join("cross-1.toml");
    let toml_1 = format!(
        r#"
[node]
display_name = "跨文件系统可写"
state_dir = {state_dir:?}

[[dir]]
name = "app"
path = {shm_dir:?}
access = "write"
"#,
        state_dir = state_1.to_str().unwrap(),
        shm_dir = shm_dir.to_str().unwrap(),
    );
    let node_1 = NodeProcess::start(&cfg_1, &toml_1, &state_1, &home);
    let mut s1 = node_1.connect();
    let _ = hello_v1(&mut s1, 1);
    let (digest_1, _, _) = describe(&mut s1, 2);
    let write_resp = send_request(
        &mut s1,
        3,
        Request::Execute {
            invocation_id: "inv-cross-fs".to_owned(),
            catalog_digest: digest_1,
            tool: "act".to_owned(),
            arguments: serde_json::json!({"script": "app.write(\"hi.txt\", \"ok\")"}),
            remaining_ms: 5_000,
            output_limit: 4096,
            labels: BTreeMap::new(),
        },
    );
    assert!(
        matches!(
            write_resp.message,
            Response::ExecuteResult {
                outcome: lancet_protocol::ExecuteOutcome::Succeeded { .. }
            }
        ),
        "跨文件系统可写目录应能正常写入：{:?}",
        write_resp.message
    );
    assert_eq!(
        std::fs::read_to_string(shm_dir.join("hi.txt")).expect("读取跨文件系统写入结果失败"),
        "ok"
    );

    // 情形 2：状态目录在 /dev/shm，可写目录在 /tmp。
    let state_2 = shm_root.path().join("state-2");
    let cfg_2 = xfs_root.path().join("cross-2.toml");
    let toml_2 = format!(
        r#"
[node]
display_name = "状态目录在 tmpfs"
state_dir = {state_dir:?}

[[dir]]
name = "app"
path = {xfs_dir:?}
access = "write"
"#,
        state_dir = state_2.to_str().unwrap(),
        xfs_dir = xfs_dir.to_str().unwrap(),
    );
    let node_2 = NodeProcess::start(&cfg_2, &toml_2, &state_2, &home);
    let mut s2 = node_2.connect();
    let _ = hello_v1(&mut s2, 1);
}

#[test]
fn connection_requires_hello() {
    let (root, home) = temp_env();
    let state_dir = root.path().join("state");

    let config_path = root.path().join("node.toml");
    let toml_content = format!(
        r#"
[node]
display_name = "握手测试"
state_dir = {state_dir:?}
"#,
        state_dir = state_dir.to_str().unwrap(),
    );

    let node = NodeProcess::start(&config_path, &toml_content, &state_dir, &home);

    // 1. 未握手先发 describe：连接被关闭、无回复。
    let mut s1 = node.connect();
    write_frame(
        &mut s1,
        &Frame {
            request_id: 1,
            message: Request::Describe,
        },
    )
    .expect("发送 describe 失败");
    let r1: Option<Frame<Response>> = read_frame(&mut s1).expect("读取 s1 失败");
    assert!(r1.is_none(), "未握手先发 describe 应直接关闭连接且无回复");

    // 2. hello 的版本列表不含 V1：连接被关闭、无回复。
    let mut s2 = node.connect();
    write_frame(
        &mut s2,
        &Frame {
            request_id: 2,
            message: Request::Hello {
                supported_versions: vec![ProtocolVersion::new(7)],
            },
        },
    )
    .expect("发送 hello([7]) 失败");
    let r2: Option<Frame<Response>> = read_frame(&mut s2).expect("读取 s2 失败");
    assert!(r2.is_none(), "版本列表不含 V1 时应直接关闭连接且无回复");

    // 3. 版本列表为 [9, 1]：选定版本 1。
    let mut s3 = node.connect();
    let hello_resp = send_request(
        &mut s3,
        3,
        Request::Hello {
            supported_versions: vec![ProtocolVersion::new(9), ProtocolVersion::V1],
        },
    );
    assert_eq!(hello_resp.request_id, 3);
    match hello_resp.message {
        Response::HelloOk { version, .. } => assert_eq!(version, ProtocolVersion::V1),
        other => panic!("期望 HelloOk，实际为：{other:?}"),
    }
}

#[test]
fn startup_rejects_unknown_references() {
    let (root, home) = temp_env();
    let app_dir = root.path().join("app");
    std::fs::create_dir(&app_dir).expect("创建 app 失败");

    // 情形 1：env.mounts 引用不存在的目录名。
    let cfg_bad_mount = root.path().join("bad-mount.toml");
    let state_1 = root.path().join("state-1");
    let err_mount = run_expect_failure(
        &cfg_bad_mount,
        &format!(
            r#"
[node]
display_name = "引用校验"
state_dir = {state_dir:?}

[[dir]]
name = "app"
path = {app_dir:?}
access = "write"

[[env]]
name = "dev"
mounts = ["missing_dir"]
"#,
            state_dir = state_1.to_str().unwrap(),
            app_dir = app_dir.to_str().unwrap(),
        ),
        &home,
    );
    assert!(
        err_mount.contains("dev") && err_mount.contains("missing_dir"),
        "应写明条目名与引用名，实际标准错误：\n{err_mount}"
    );

    // 情形 2：env.egress 引用不存在的出口名。
    let cfg_bad_egress = root.path().join("bad-egress.toml");
    let state_2 = root.path().join("state-2");
    let err_egress = run_expect_failure(
        &cfg_bad_egress,
        &format!(
            r#"
[node]
display_name = "引用校验"
state_dir = {state_dir:?}

[[dir]]
name = "app"
path = {app_dir:?}
access = "write"

[[env]]
name = "runner"
mounts = ["app"]
egress = "missing_egress"
"#,
            state_dir = state_2.to_str().unwrap(),
            app_dir = app_dir.to_str().unwrap(),
        ),
        &home,
    );
    assert!(
        err_egress.contains("runner") && err_egress.contains("missing_egress"),
        "应写明条目名与引用名，实际标准错误：\n{err_egress}"
    );
}

#[test]
fn startup_rejects_invalid_handle_names() {
    let (root, home) = temp_env();
    let ro_dir = root.path().join("ro");
    std::fs::create_dir(&ro_dir).expect("创建 ro 失败");

    let invalid_cases = [
        ("dir_1abc", "dir", "1abc"),
        ("dir_spaces", "dir", " app "),
        ("env_keyword", "env", "for"),
        ("env_builtin", "env", "len"),
        ("egress_ext", "egress", "regex"),
        ("egress_entry", "egress", "lancet_act_entry__"),
    ];

    for (case_name, table, bad_name) in invalid_cases {
        let state_dir = root.path().join(format!("state-{case_name}"));
        let config_path = root.path().join(format!("{case_name}.toml"));
        let extra = match table {
            "dir" => format!(
                "[[dir]]\nname = {bad_name:?}\npath = {ro_dir:?}\naccess = \"read\"\n",
                ro_dir = ro_dir.to_str().unwrap(),
            ),
            "env" => format!("[[env]]\nname = {bad_name:?}\n"),
            "egress" => format!("[[egress]]\nname = {bad_name:?}\nhosts = [\"example.com\"]\n"),
            _ => unreachable!(),
        };
        let toml_content = format!(
            "[node]\ndisplay_name = \"句柄名校验\"\nstate_dir = {state_dir:?}\n\n{extra}",
            state_dir = state_dir.to_str().unwrap(),
        );
        let stderr = run_expect_failure(&config_path, &toml_content, &home);
        assert!(
            stderr.contains(bad_name) && stderr.contains(table),
            "用例 `{case_name}` 报错应写明表 `{table}` 与条目名 `{bad_name}`，实际标准错误：\n{stderr}"
        );
    }

    // 正例：已移除的内置名 `dir`、`commit`、`discard` 可以作为句柄名正常启动。
    let commit_dir = root.path().join("commit_dir");
    let discard_dir = root.path().join("discard_dir");
    std::fs::create_dir(&commit_dir).expect("创建 commit_dir 失败");
    std::fs::create_dir(&discard_dir).expect("创建 discard_dir 失败");
    let ok_state = root.path().join("state-dir-ok");
    let ok_cfg = root.path().join("dir-ok.toml");
    let ok_toml = format!(
        r#"
[node]
display_name = "原保留名允许启动"
state_dir = {state_dir:?}

[[dir]]
name = "dir"
path = {ro_dir:?}
access = "read"

[[dir]]
name = "commit"
path = {commit_dir:?}
access = "read"

[[dir]]
name = "discard"
path = {discard_dir:?}
access = "write"
"#,
        state_dir = ok_state.to_str().unwrap(),
        ro_dir = ro_dir.to_str().unwrap(),
        commit_dir = commit_dir.to_str().unwrap(),
        discard_dir = discard_dir.to_str().unwrap(),
    );
    let node = NodeProcess::start(&ok_cfg, &ok_toml, &ok_state, &home);
    let mut stream = node.connect();
    let _ = hello_v1(&mut stream, 1);
}

#[path = "node/calls.rs"]
mod calls;
#[path = "node/writable.rs"]
mod writable;
