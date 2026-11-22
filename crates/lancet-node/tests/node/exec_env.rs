// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use std::collections::BTreeMap;
use std::io::{Read as _, Write as _};
use std::net::{TcpListener, TcpStream};
use std::os::unix::fs::PermissionsExt as _;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use lancet_protocol::{
    CatalogDigest, EffectEntry, EffectKind, ExecuteOutcome, Frame, Labels, OutcomeKind,
    ReceiptLookup, ReceiptOutcome, RejectCode, Request, Response, StepSummary, read_frame,
    write_frame,
};
use serde_json::json;
use tempfile::TempDir;

use super::{BIN, NodeProcess, describe, hello_v1, run_expect_failure, send_request};

const WAIT_TIMEOUT: Duration = Duration::from_secs(20);

fn temp_var_env() -> (TempDir, PathBuf) {
    let root = tempfile::Builder::new()
        .prefix("ln-env-")
        .tempdir_in("/var/tmp")
        .expect("在 /var/tmp 创建临时目录失败");
    let home = root.path().join("home");
    std::fs::create_dir(&home).expect("创建合成 home 失败");
    (root, home)
}

fn connect_ready(node: &NodeProcess) -> (UnixStream, CatalogDigest) {
    let mut stream = node.connect();
    stream
        .set_read_timeout(Some(WAIT_TIMEOUT))
        .expect("设置读取超时失败");
    stream
        .set_write_timeout(Some(WAIT_TIMEOUT))
        .expect("设置写入超时失败");
    let _ = hello_v1(&mut stream, 1);
    let (digest, _, _) = describe(&mut stream, 2);
    (stream, digest)
}

fn exec_req(
    invocation_id: &str,
    digest: &CatalogDigest,
    script: &str,
    remaining_ms: u64,
    output_limit: u64,
) -> Request {
    Request::Execute {
        invocation_id: invocation_id.to_owned(),
        catalog_digest: digest.clone(),
        tool: "act".to_owned(),
        arguments: json!({ "script": script }),
        remaining_ms,
        output_limit,
        labels: Labels::new(),
    }
}

fn run_exec(stream: &mut UnixStream, req_id: u64, req: Request) -> ExecuteOutcome {
    let resp = send_request(stream, req_id, req);
    assert_eq!(resp.request_id, req_id);
    match resp.message {
        Response::ExecuteResult { outcome } => outcome,
        other => panic!("期望 ExecuteResult，实际为：{other:?}"),
    }
}

fn run_act(
    stream: &mut UnixStream,
    req_id: u64,
    inv_id: &str,
    digest: &CatalogDigest,
    script: &str,
) -> ExecuteOutcome {
    run_exec(
        stream,
        req_id,
        exec_req(inv_id, digest, script, 20_000, 128 * 1024),
    )
}

fn query_receipt(stream: &mut UnixStream, req_id: u64, inv_id: &str) -> ReceiptLookup {
    let resp = send_request(
        stream,
        req_id,
        Request::Receipt {
            invocation_id: inv_id.to_owned(),
        },
    );
    assert_eq!(resp.request_id, req_id);
    match resp.message {
        Response::ReceiptResult { lookup } => lookup,
        other => panic!("期望 ReceiptResult，实际为：{other:?}"),
    }
}

fn send_only(stream: &mut UnixStream, request_id: u64, message: Request) {
    write_frame(
        stream,
        &Frame {
            request_id,
            message,
        },
    )
    .expect("发送请求帧失败");
}

fn read_reply(stream: &mut UnixStream) -> Frame<Response> {
    read_frame(stream)
        .expect("读取回复帧失败")
        .expect("期望收到回复帧，却遇到 EOF")
}

/// 读取节点进程所属的委派根 `D`（即 `<scope>`，节点自身位于 `D/node`）。
fn node_cgroup_root(node_pid: u32) -> PathBuf {
    let raw = std::fs::read_to_string(format!("/proc/{node_pid}/cgroup"))
        .expect("读取节点 /proc/<pid>/cgroup 失败");
    let rel = raw
        .lines()
        .find_map(|l| l.strip_prefix("0::"))
        .expect("缺少 cgroup v2 0:: 条目")
        .trim();
    let node_cg = Path::new("/sys/fs/cgroup").join(rel.trim_start_matches('/'));
    assert_eq!(
        node_cg.file_name().and_then(|s| s.to_str()),
        Some("node"),
        "节点进程应位于委派根下的 node 子组：{}",
        node_cg.display()
    );
    node_cg
        .parent()
        .expect("node 子组应有父目录 D")
        .to_path_buf()
}

fn list_call_leaves(cg_root: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(cg_root) else {
        return Vec::new();
    };
    entries
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| {
            p.is_dir()
                && p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with("call-"))
        })
        .collect()
}

fn cgroup_procs(leaf: &Path) -> Vec<u32> {
    let Ok(text) = std::fs::read_to_string(leaf.join("cgroup.procs")) else {
        return Vec::new();
    };
    text.lines()
        .filter_map(|l| l.trim().parse::<u32>().ok())
        .collect()
}

fn pid_target_cmdline_contains(pid: u32, needle: &str) -> bool {
    let Ok(bytes) = std::fs::read(format!("/proc/{pid}/cmdline")) else {
        return false;
    };
    let argv0 = bytes.split(|&b| b == 0).next().unwrap_or(&[]);
    if argv0.ends_with(b"bwrap") || argv0.ends_with(b"lancet-init") {
        return false;
    }
    let text = String::from_utf8_lossy(&bytes);
    text.contains(needle)
}

fn pid_alive(pid: u32) -> bool {
    // SAFETY: kill(pid, 0) 仅检测进程是否存在。
    let rc = unsafe { libc::kill(pid as libc::pid_t, 0) };
    if rc == 0 {
        return true;
    }
    std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
}

fn wait_for_file(path: &Path, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    while !path.exists() {
        assert!(
            Instant::now() < deadline,
            "等待文件 {} 出现超时",
            path.display()
        );
        thread::sleep(Duration::from_millis(10));
    }
}

fn wait_for_leaf_with_marker(
    cg_root: &Path,
    marker: &str,
    timeout: Duration,
) -> (PathBuf, u32, Vec<u32>) {
    let deadline = Instant::now() + timeout;
    loop {
        for leaf in list_call_leaves(cg_root) {
            let pids = cgroup_procs(&leaf);
            if let Some(&matched) = pids
                .iter()
                .find(|&&pid| pid_target_cmdline_contains(pid, marker))
            {
                return (leaf, matched, pids);
            }
        }
        assert!(
            Instant::now() < deadline,
            "在 cgroup {} 下等待包含标记 `{marker}` 的叶组超时",
            cg_root.display()
        );
        thread::sleep(Duration::from_millis(15));
    }
}

fn wait_for_leaf_empty_or_gone(leaf: &Path, tracked_pids: &[u32], timeout: Duration) {
    let deadline = Instant::now() + timeout;
    loop {
        let leaf_clear = if !leaf.exists() {
            true
        } else {
            std::fs::read_to_string(leaf.join("cgroup.events"))
                .map(|s| s.lines().any(|l| l.trim() == "populated 0"))
                .unwrap_or(true)
        };
        let all_dead = tracked_pids.iter().all(|&pid| !pid_alive(pid));
        if leaf_clear && all_dead {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "等待叶组 {} 清空且进程 {:?} 退出超时（leaf_exists={}）",
            leaf.display(),
            tracked_pids,
            leaf.exists()
        );
        thread::sleep(Duration::from_millis(15));
    }
}

fn wait_child_expect_failure(mut child: std::process::Child) -> String {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if let Some(status) = child.try_wait().expect("检查子进程状态失败") {
            assert!(!status.success(), "节点应当启动校验失败退出");
            let mut stderr = String::new();
            child
                .stderr
                .take()
                .expect("缺少 stderr")
                .read_to_string(&mut stderr)
                .expect("读取 stderr 失败");
            return stderr;
        }
        thread::sleep(Duration::from_millis(20));
    }
    let _ = child.kill();
    let _ = child.wait();
    panic!("节点应当启动失败，却未在时限内退出");
}

fn run_delegated_expect_failure(config_path: &Path, toml_content: &str, home: &Path) -> String {
    std::fs::write(config_path, toml_content).expect("写入测试配置文件失败");
    let child = Command::new("systemd-run")
        .args([
            "--user",
            "--scope",
            "-p",
            "Delegate=yes",
            "--quiet",
            "--",
            BIN,
        ])
        .arg(config_path)
        .env("HOME", home)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("启动委派测试节点失败");
    wait_child_expect_failure(child)
}

#[test]
fn visibility_ro_nested_submount_and_live_direct_write() {
    let (root, home) = temp_var_env();
    let rw = root.path().join("rw");
    let ro = root.path().join("ro");
    let ro_nested = ro.join("nested");
    let nested_src = root.path().join("nested_src");
    let unmounted = root.path().join("unmounted");
    let tool_dir = home.join(".mytool");
    let tool_file = home.join("single_tool.txt");
    let state_dir = root.path().join("state");

    for d in [&rw, &ro_nested, &nested_src, &unmounted, &tool_dir] {
        std::fs::create_dir_all(d).expect("创建目录失败");
    }

    std::fs::write(ro.join("hello.txt"), "ro-content").unwrap();
    std::fs::write(nested_src.join("inner.txt"), "nested-content").unwrap();
    std::fs::write(rw.join("seed.txt"), "rw-seed").unwrap();
    std::fs::write(tool_dir.join("info.txt"), "tool-dir-content").unwrap();
    std::fs::write(&tool_file, "tool-file-content").unwrap();
    std::fs::write(home.join("host_secret.txt"), "host-home-secret").unwrap();
    std::fs::write(root.path().join("outside_secret.txt"), "outside-secret").unwrap();
    std::fs::write(unmounted.join("secret.txt"), "unmounted-secret").unwrap();

    let config_path = root.path().join("node.toml");
    let toml_content = format!(
        r#"
[node]
display_name = "可见性与直写测试"
state_dir = {state_dir:?}

[[dir]]
name = "rw"
path = {rw:?}
access = "write"

[[dir]]
name = "ro"
path = {ro:?}
access = "read"

[[dir]]
name = "unmounted"
path = {unmounted:?}
access = "read"

[[env]]
name = "dev"
mounts = ["rw", "ro"]
tools = ["~/.mytool", "~/single_tool.txt"]
"#,
        state_dir = state_dir.to_str().unwrap(),
        rw = rw.to_str().unwrap(),
        ro = ro.to_str().unwrap(),
        unmounted = unmounted.to_str().unwrap(),
    );
    std::fs::write(&config_path, &toml_content).expect("写入配置文件失败");

    // 在独立的委派 cgroup 与 user+mount 命名空间内先建立真实嵌套子挂载 ro/nested，再 exec 节点。
    let child = Command::new("systemd-run")
        .args([
            "--user",
            "--scope",
            "-p",
            "Delegate=yes",
            "--quiet",
            "--",
            "unshare",
            "--user",
            "--map-root-user",
            "--mount",
            "--",
            "/bin/sh",
            "-c",
            r#"mount --bind "$1" "$2" && exec "$3" "$4""#,
            "--",
        ])
        .arg(&nested_src)
        .arg(&ro_nested)
        .arg(BIN)
        .arg(&config_path)
        .env("HOME", &home)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("启动带嵌套子挂载的测试节点失败");
    let node = NodeProcess::wait_ready(child, state_dir.join("node.sock"), Duration::from_secs(15));
    let (mut stream, digest) = connect_ready(&node);

    // 1. 可见性、未授权 ENOENT、只读目录与嵌套子挂载写入均失败、默认 cwd 为私有家目录。
    let py_probe = format!(
        r#"
import errno, json, os

def try_open_missing(path):
    try:
        open(path, "r").read()
        return "UNEXPECTED_READ"
    except OSError as e:
        return e.errno

def try_write_fail(path):
    try:
        with open(path, "w") as f:
            f.write("mutated")
        return "UNEXPECTED_WRITE"
    except OSError as e:
        return e.errno

out = {{
    "cwd": os.getcwd(),
    "ro": open({ro_hello:?}).read(),
    "ro_nested": open({ro_nested_file:?}).read(),
    "rw": open({rw_seed:?}).read(),
    "tool_dir": open({tool_dir_file:?}).read(),
    "tool_file": open({tool_file:?}).read(),
    "miss_home_secret": try_open_missing({home_secret:?}),
    "miss_outside": try_open_missing({outside_secret:?}),
    "miss_unmounted": try_open_missing({unmounted_secret:?}),
    "ro_new": try_write_fail({ro_new:?}),
    "ro_overwrite": try_write_fail({ro_hello:?}),
    "nested_new": try_write_fail({ro_nested_new:?}),
    "nested_overwrite": try_write_fail({ro_nested_file:?}),
    "tool_dir_write": try_write_fail({tool_dir_new:?}),
    "tool_file_write": try_write_fail({tool_file:?}),
    "enoent": errno.ENOENT,
}}
print(json.dumps(out))
"#,
        ro_hello = ro.join("hello.txt").to_str().unwrap(),
        ro_nested_file = ro_nested.join("inner.txt").to_str().unwrap(),
        rw_seed = rw.join("seed.txt").to_str().unwrap(),
        tool_dir_file = tool_dir.join("info.txt").to_str().unwrap(),
        tool_file = tool_file.to_str().unwrap(),
        home_secret = home.join("host_secret.txt").to_str().unwrap(),
        outside_secret = root.path().join("outside_secret.txt").to_str().unwrap(),
        unmounted_secret = unmounted.join("secret.txt").to_str().unwrap(),
        ro_new = ro.join("new.txt").to_str().unwrap(),
        ro_nested_new = ro_nested.join("new.txt").to_str().unwrap(),
        tool_dir_new = tool_dir.join("new.txt").to_str().unwrap(),
    );

    let script = format!("res = dev.run([\"python3\", \"-c\", {py_probe:?}])\nreturn res.stdout\n");
    let outcome = run_act(&mut stream, 10, "inv-vis", &digest, &script);
    let stdout = match outcome {
        ExecuteOutcome::Succeeded { content, .. } => content["value"].as_str().unwrap().to_owned(),
        other => panic!("可见性探测应成功，实际：{other:?}"),
    };
    let data: serde_json::Value = serde_json::from_str(&stdout).expect("解析探测 JSON 失败");
    let enoent = data["enoent"].as_i64().unwrap();
    assert_eq!(data["cwd"], home.to_str().unwrap());
    assert_eq!(data["ro"], "ro-content");
    assert_eq!(data["ro_nested"], "nested-content");
    assert_eq!(data["rw"], "rw-seed");
    assert_eq!(data["tool_dir"], "tool-dir-content");
    assert_eq!(data["tool_file"], "tool-file-content");
    assert_eq!(data["miss_home_secret"].as_i64(), Some(enoent));
    assert_eq!(data["miss_outside"].as_i64(), Some(enoent));
    assert_eq!(data["miss_unmounted"].as_i64(), Some(enoent));
    for key in [
        "ro_new",
        "ro_overwrite",
        "nested_new",
        "nested_overwrite",
        "tool_dir_write",
        "tool_file_write",
    ] {
        assert!(
            data[key].is_i64(),
            "对只读目标 `{key}` 写入应失败并返回 errno，实际：{}",
            data[key]
        );
    }
    assert_eq!(
        std::fs::read_to_string(ro.join("hello.txt")).unwrap(),
        "ro-content"
    );
    assert_eq!(
        std::fs::read_to_string(nested_src.join("inner.txt")).unwrap(),
        "nested-content"
    );
    assert!(!nested_src.join("new.txt").exists());

    // 未挂载的目录名作为 cwd 应报错，且不产生 Exec。
    let bad_cwd = run_act(
        &mut stream,
        11,
        "inv-bad-cwd",
        &digest,
        "return dev.run([\"true\"], cwd = \"unmounted\")",
    );
    assert!(matches!(bad_cwd, ExecuteOutcome::Failed { .. }));
    let bad_rec = match query_receipt(&mut stream, 12, "inv-bad-cwd") {
        ReceiptLookup::Settled { receipt } => receipt,
        other => panic!("期望 Settled：{other:?}"),
    };
    assert!(
        bad_rec.effects.is_empty(),
        "非法 cwd 未启动进程，不应记 Exec"
    );

    // 2. 直写同步点：程序写可写目录后停在同步点，宿主在调用返回前直接读到内容。
    let live_py = r#"
import os, time
with open("live.txt", "w") as f:
    f.write("live-before-return")
    f.flush()
    os.fsync(f.fileno())
with open("ready.flag", "w") as f:
    f.write(os.getcwd())
while not os.path.exists("release.flag"):
    time.sleep(0.01)
"#;
    let live_script = format!(
        "res = dev.run([\"python3\", \"-c\", {live_py:?}], cwd = \"rw\")\nreturn res.code\n"
    );
    send_only(
        &mut stream,
        20,
        exec_req("inv-live-write", &digest, &live_script, 15_000, 65_536),
    );

    wait_for_file(&rw.join("ready.flag"), Duration::from_secs(5));
    assert_eq!(
        std::fs::read_to_string(rw.join("live.txt")).unwrap(),
        "live-before-return",
        "调用返回前宿主就应直接读到可写目录的新内容"
    );
    assert_eq!(
        std::fs::read_to_string(rw.join("ready.flag")).unwrap(),
        rw.to_str().unwrap(),
        "cwd = \"rw\" 应解析为 rw 的真实绝对路径"
    );
    assert_eq!(
        query_receipt(&mut stream, 21, "inv-live-write"),
        ReceiptLookup::Pending,
        "停在同步点时调用仍应处于 Pending"
    );

    std::fs::write(rw.join("release.flag"), "go").unwrap();
    let frame = read_reply(&mut stream);
    assert_eq!(frame.request_id, 20);
    assert!(
        matches!(
            frame.message,
            Response::ExecuteResult {
                outcome: ExecuteOutcome::Succeeded { .. }
            }
        ),
        "放行同步点后调用应成功：{:?}",
        frame.message
    );
}

#[test]
fn env_isolation_and_loopback_network_isolation() {
    let (root, home) = temp_var_env();
    let rw = root.path().join("rw");
    let state_dir = root.path().join("state");
    std::fs::create_dir(&rw).unwrap();

    // 启动宿主回环监听器，并先在宿主侧连通作正对照。
    let listener = TcpListener::bind("127.0.0.1:0").expect("绑定宿主回环端口失败");
    let port = listener.local_addr().unwrap().port();
    listener.set_nonblocking(false).unwrap();
    let srv = thread::spawn(move || {
        for mut sock in listener.incoming().take(2).flatten() {
            let _ = sock.write_all(b"host-loopback-ok");
        }
    });
    {
        let mut ctrl = TcpStream::connect(("127.0.0.1", port)).expect("宿主侧应能连上回环监听器");
        let mut buf = String::new();
        ctrl.read_to_string(&mut buf).unwrap();
        assert_eq!(buf, "host-loopback-ok");
    }

    let config_path = root.path().join("node.toml");
    let toml_content = format!(
        r#"
[node]
display_name = "环境与网络隔离测试"
state_dir = {state_dir:?}

[[dir]]
name = "rw"
path = {rw:?}
access = "write"

[[env]]
name = "dev"
mounts = ["rw"]
path = ["~/.mybin", "/opt/extra/bin"]
vars = {{ LANG = "C.UTF-8", CUSTOM_A = "hello=world", CUSTOM_B = "42" }}
"#,
        state_dir = state_dir.to_str().unwrap(),
        rw = rw.to_str().unwrap(),
    );

    let node = NodeProcess::start_delegated(
        &config_path,
        &toml_content,
        &state_dir,
        &home,
        &[("LANCET_SYNTHETIC_HOST_SECRET", "must-not-leak")],
    );
    let (mut stream, digest) = connect_ready(&node);

    // 1. env -0 精确核对全部环境变量。
    let env_res = run_act(
        &mut stream,
        10,
        "inv-env",
        &digest,
        "return dev.run([\"env\", \"-0\"]).stdout",
    );
    let env_raw = match env_res {
        ExecuteOutcome::Succeeded { content, .. } => content["value"].as_str().unwrap().to_owned(),
        other => panic!("运行 env -0 应成功，实际：{other:?}"),
    };
    let mut parsed_env = BTreeMap::new();
    for entry in env_raw.split('\0').filter(|s| !s.is_empty()) {
        let (k, v) = entry
            .split_once('=')
            .unwrap_or_else(|| panic!("env -0 条目缺少 `=`：{entry:?}"));
        assert!(
            parsed_env.insert(k.to_owned(), v.to_owned()).is_none(),
            "环境变量 `{k}` 重复出现"
        );
    }
    let expected_path = format!(
        "/usr/local/bin:/usr/bin:/bin:{}:/opt/extra/bin",
        home.join(".mybin").display()
    );
    let expected_env = BTreeMap::from([
        ("CUSTOM_A".to_owned(), "hello=world".to_owned()),
        ("CUSTOM_B".to_owned(), "42".to_owned()),
        ("HOME".to_owned(), home.to_str().unwrap().to_owned()),
        ("LANG".to_owned(), "C.UTF-8".to_owned()),
        ("PATH".to_owned(), expected_path),
    ]);
    assert_eq!(parsed_env, expected_env, "沙箱环境变量必须精确匹配配置");
    assert!(
        !parsed_env.contains_key("LANCET_SYNTHETIC_HOST_SECRET"),
        "宿主注入的合成变量不得泄漏进沙箱"
    );

    // 2. 沙箱内网络只有 lo，连接宿主回环端口失败。
    let net_py = format!(
        r#"
import json, socket
ifaces = []
with open("/proc/net/dev") as f:
    for line in f.readlines()[2:]:
        if ":" in line:
            ifaces.append(line.split(":", 1)[0].strip())
conn_err = None
try:
    s = socket.create_connection(("127.0.0.1", {port}), timeout=1.0)
    s.close()
except OSError as e:
    conn_err = e.errno
print(json.dumps({{"ifaces": ifaces, "conn_err": conn_err}}))
"#
    );
    let net_script = format!("return dev.run([\"python3\", \"-c\", {net_py:?}]).stdout");
    let net_res = run_act(&mut stream, 11, "inv-net", &digest, &net_script);
    let net_out = match net_res {
        ExecuteOutcome::Succeeded { content, .. } => content["value"].as_str().unwrap().to_owned(),
        other => panic!("网络探测应成功，实际：{other:?}"),
    };
    let net_json: serde_json::Value = serde_json::from_str(&net_out).unwrap();
    assert_eq!(net_json["ifaces"], json!(["lo"]));
    assert!(
        net_json["conn_err"].is_i64(),
        "沙箱内连接宿主回环端口 {port} 应当失败，实际：{net_json}"
    );

    // 结束宿主监听线程（补一次宿主机连接触发 take(2) 退出）。
    let _ = TcpStream::connect(("127.0.0.1", port));
    let _ = srv.join();
}

#[test]
fn private_home_cache_persistence_and_ephemeral_tmp() {
    let (root, home) = temp_var_env();
    let state_dir = root.path().join("state");
    let generic_cache = home.join("cache_seed");
    let cargo_reg = home.join(".cargo/registry");
    std::fs::create_dir_all(&generic_cache).unwrap();
    std::fs::create_dir_all(cargo_reg.join("cache")).unwrap();
    std::fs::create_dir_all(cargo_reg.join("index")).unwrap();
    std::fs::create_dir_all(cargo_reg.join("src")).unwrap();

    std::fs::write(generic_cache.join("seed.txt"), "host-seed-v0").unwrap();
    std::fs::write(cargo_reg.join("cache/demo.crate"), "crate-bytes").unwrap();
    std::fs::write(cargo_reg.join("index/idx.txt"), "index-bytes").unwrap();
    std::fs::write(cargo_reg.join("src/unpacked.rs"), "host-src-should-skip").unwrap();

    let config_path = root.path().join("node.toml");
    let toml_content = format!(
        r#"
[node]
display_name = "家目录与缓存测试"
state_dir = {state_dir:?}

[[env]]
name = "dev"
caches = ["~/cache_seed", "~/.cargo/registry"]
"#,
        state_dir = state_dir.to_str().unwrap(),
    );

    let node1 = NodeProcess::start_delegated(&config_path, &toml_content, &state_dir, &home, &[]);
    let (mut s1, digest1) = connect_ready(&node1);

    // 调用 1：写家目录、写私有缓存、写 /tmp，同时核对 Cargo registry 只播种 cache/ 与 index/。
    let py1 = r#"
import json, os
home = os.environ["HOME"]
with open(os.path.join(home, "persisted.txt"), "w") as f:
    f.write("home-v1")
with open(os.path.join(home, "cache_seed/seed.txt"), "w") as f:
    f.write("cache-mutated")
with open(os.path.join(home, "cache_seed/added.txt"), "w") as f:
    f.write("cache-added")
with open("/tmp/ephemeral.txt", "w") as f:
    f.write("tmp-v1")
print(json.dumps({
    "crate": open(os.path.join(home, ".cargo/registry/cache/demo.crate")).read(),
    "index": open(os.path.join(home, ".cargo/registry/index/idx.txt")).read(),
    "src_exists": os.path.exists(os.path.join(home, ".cargo/registry/src/unpacked.rs")),
}))
"#;
    let r1 = run_act(
        &mut s1,
        10,
        "inv-persist-1",
        &digest1,
        &format!("return dev.run([\"python3\", \"-c\", {py1:?}]).stdout"),
    );
    let out1: serde_json::Value = match r1 {
        ExecuteOutcome::Succeeded { content, .. } => {
            serde_json::from_str(content["value"].as_str().unwrap()).unwrap()
        }
        other => panic!("调用 1 应成功，实际：{other:?}"),
    };
    assert_eq!(out1["crate"], "crate-bytes");
    assert_eq!(out1["index"], "index-bytes");
    assert_eq!(out1["src_exists"], false, "Cargo registry 不应播种 src/");

    // 调用 2：读得到家目录与缓存修改，/tmp 文件已不在。
    let py_check = r#"
import json, os
home = os.environ["HOME"]
print(json.dumps({
    "home_file": open(os.path.join(home, "persisted.txt")).read(),
    "cache_seed": open(os.path.join(home, "cache_seed/seed.txt")).read(),
    "cache_added": open(os.path.join(home, "cache_seed/added.txt")).read(),
    "tmp_exists": os.path.exists("/tmp/ephemeral.txt"),
}))
"#;
    let r2 = run_act(
        &mut s1,
        11,
        "inv-persist-2",
        &digest1,
        &format!("return dev.run([\"python3\", \"-c\", {py_check:?}]).stdout"),
    );
    let out2: serde_json::Value = match r2 {
        ExecuteOutcome::Succeeded { content, .. } => {
            serde_json::from_str(content["value"].as_str().unwrap()).unwrap()
        }
        other => panic!("调用 2 应成功，实际：{other:?}"),
    };
    assert_eq!(out2["home_file"], "home-v1");
    assert_eq!(out2["cache_seed"], "cache-mutated");
    assert_eq!(out2["cache_added"], "cache-added");
    assert_eq!(
        out2["tmp_exists"], false,
        "/tmp 每次调用新建，前次文件不应残留"
    );

    // 宿主缓存种子内容不变。
    assert_eq!(
        std::fs::read_to_string(generic_cache.join("seed.txt")).unwrap(),
        "host-seed-v0"
    );
    assert!(!generic_cache.join("added.txt").exists());

    // 重启节点后，私有家目录与私有缓存副本依然保留且直接复用。
    drop(s1);
    drop(node1);

    let node2 = NodeProcess::start_delegated(&config_path, &toml_content, &state_dir, &home, &[]);
    let (mut s2, digest2) = connect_ready(&node2);
    assert_eq!(digest2, digest1);

    let r3 = run_act(
        &mut s2,
        12,
        "inv-persist-3",
        &digest2,
        &format!("return dev.run([\"python3\", \"-c\", {py_check:?}]).stdout"),
    );
    let out3: serde_json::Value = match r3 {
        ExecuteOutcome::Succeeded { content, .. } => {
            serde_json::from_str(content["value"].as_str().unwrap()).unwrap()
        }
        other => panic!("重启后调用 3 应成功，实际：{other:?}"),
    };
    assert_eq!(out3["home_file"], "home-v1");
    assert_eq!(out3["cache_seed"], "cache-mutated");
    assert_eq!(out3["cache_added"], "cache-added");
    assert_eq!(out3["tmp_exists"], false);
    assert_eq!(
        std::fs::read_to_string(generic_cache.join("seed.txt")).unwrap(),
        "host-seed-v0"
    );
}

fn setsid_spawn_script(ready_name: &str, exit_flag: Option<&str>, marker: &str) -> String {
    let wait_block = match exit_flag {
        Some(flag) => format!("while not os.path.exists({flag:?}):\n    time.sleep(0.01)\n"),
        None => "while True:\n    time.sleep(0.5)\n".to_owned(),
    };
    let child_code = format!(
        "import os, signal, time; os.setsid(); signal.signal(signal.SIGTERM, signal.SIG_IGN); open({child_ready:?}, 'w').write('ok'); time.sleep(120) # {marker}",
        child_ready = format!("{ready_name}.child"),
    );
    let py = format!(
        r#"
import os, subprocess, time
subprocess.Popen(["python3", "-c", {child_code:?}])
while not os.path.exists({child_ready:?}):
    time.sleep(0.01)
with open({ready_name:?}, "w") as f:
    f.write("ready")
{wait_block}
"#,
        child_ready = format!("{ready_name}.child"),
    );
    format!("return dev.run([\"python3\", \"-c\", {py:?}], cwd = \"rw\").code")
}

#[test]
fn normal_cancel_disconnect_deadline_clean_up_setsid_descendant() {
    let (root, home) = temp_var_env();
    let rw = root.path().join("rw");
    let state_dir = root.path().join("state");
    std::fs::create_dir(&rw).unwrap();

    let config_path = root.path().join("node.toml");
    let toml_content = format!(
        r#"
[node]
display_name = "四类结束与后代清理测试"
state_dir = {state_dir:?}

[[dir]]
name = "rw"
path = {rw:?}
access = "write"

[[env]]
name = "dev"
mounts = ["rw"]
"#,
        state_dir = state_dir.to_str().unwrap(),
        rw = rw.to_str().unwrap(),
    );

    let node = NodeProcess::start_delegated(&config_path, &toml_content, &state_dir, &home, &[]);
    let cg_root = node_cgroup_root(node.pid());
    let (mut stream, digest) = connect_ready(&node);

    // 1. 正常退出：前台返回 0，setsid 后代立即随命名空间退出，叶组删除。
    {
        let marker = "lancet-setsid-normal";
        let script = setsid_spawn_script("ready_normal", Some("go_normal"), marker);
        send_only(
            &mut stream,
            10,
            exec_req("inv-end-normal", &digest, &script, 15_000, 65_536),
        );
        wait_for_file(&rw.join("ready_normal"), Duration::from_secs(5));
        let (leaf, desc_pid, pids) =
            wait_for_leaf_with_marker(&cg_root, marker, Duration::from_secs(5));
        assert!(pid_alive(desc_pid));

        std::fs::write(rw.join("go_normal"), "exit").unwrap();
        let frame = read_reply(&mut stream);
        assert_eq!(frame.request_id, 10);
        assert!(
            matches!(
                frame.message,
                Response::ExecuteResult {
                    outcome: ExecuteOutcome::Succeeded { .. }
                }
            ),
            "正常退出应结算为 Succeeded：{:?}",
            frame.message
        );
        wait_for_leaf_empty_or_gone(&leaf, &pids, Duration::from_secs(5));
        assert!(!leaf.exists(), "正常结束后叶组应已删除：{}", leaf.display());
    }

    // 2. 协议 cancel：结算为 Cancelled，叶组清空删除，setsid 后代退出。
    {
        let marker = "lancet-setsid-cancel";
        let script = setsid_spawn_script("ready_cancel", None, marker);
        send_only(
            &mut stream,
            20,
            exec_req("inv-end-cancel", &digest, &script, 30_000, 65_536),
        );
        wait_for_file(&rw.join("ready_cancel"), Duration::from_secs(5));
        let (leaf, desc_pid, pids) =
            wait_for_leaf_with_marker(&cg_root, marker, Duration::from_secs(5));
        assert!(pid_alive(desc_pid));

        send_only(
            &mut stream,
            21,
            Request::Cancel {
                invocation_id: "inv-end-cancel".to_owned(),
            },
        );
        let mut saw_cancel_ok = false;
        let mut saw_cancelled = false;
        for _ in 0..2 {
            let f = read_reply(&mut stream);
            match (f.request_id, f.message) {
                (21, Response::CancelOk) => saw_cancel_ok = true,
                (
                    20,
                    Response::ExecuteResult {
                        outcome: ExecuteOutcome::Cancelled { .. },
                    },
                ) => saw_cancelled = true,
                other => panic!("cancel 阶段收到意外回复：{other:?}"),
            }
        }
        assert!(saw_cancel_ok && saw_cancelled);
        wait_for_leaf_empty_or_gone(&leaf, &pids, Duration::from_secs(5));
        assert!(!leaf.exists(), "取消后叶组应已删除：{}", leaf.display());
        let cancel_rec = match query_receipt(&mut stream, 22, "inv-end-cancel") {
            ReceiptLookup::Settled { receipt } => receipt,
            other => panic!("cancel 后期望 Settled：{other:?}"),
        };
        assert_eq!(cancel_rec.outcome, ReceiptOutcome::Cancelled);
        assert_eq!(
            cancel_rec.effects,
            vec![EffectEntry::new("dev", EffectKind::Exec, Vec::new()).unwrap()],
            "目标已启动后取消，回执应保留 Exec"
        );
    }

    // 3. 关掉所属连接：结算为 Cancelled，叶组清空删除，setsid 后代退出。
    {
        let (mut drop_conn, _) = connect_ready(&node);
        let marker = "lancet-setsid-disconnect";
        let script = setsid_spawn_script("ready_disc", None, marker);
        send_only(
            &mut drop_conn,
            30,
            exec_req("inv-end-disconnect", &digest, &script, 30_000, 65_536),
        );
        wait_for_file(&rw.join("ready_disc"), Duration::from_secs(5));
        let (leaf, desc_pid, pids) =
            wait_for_leaf_with_marker(&cg_root, marker, Duration::from_secs(5));
        assert!(pid_alive(desc_pid));

        drop(drop_conn);
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut req_id = 31;
        let receipt = loop {
            match query_receipt(&mut stream, req_id, "inv-end-disconnect") {
                ReceiptLookup::Settled { receipt } => break receipt,
                ReceiptLookup::Pending => {
                    assert!(Instant::now() < deadline, "断连后未在宽限内结算");
                    req_id += 1;
                    thread::sleep(Duration::from_millis(15));
                }
                other => panic!("断连后意外回执状态：{other:?}"),
            }
        };
        assert_eq!(receipt.outcome, ReceiptOutcome::Cancelled);
        assert_eq!(
            receipt.effects,
            vec![EffectEntry::new("dev", EffectKind::Exec, Vec::new()).unwrap()],
            "目标已启动后断连，回执应保留 Exec"
        );
        wait_for_leaf_empty_or_gone(&leaf, &pids, Duration::from_secs(5));
        assert!(!leaf.exists(), "断连后叶组应已删除：{}", leaf.display());
    }

    // 4. 耗尽 remaining_ms：结算为 Deadline，叶组清空删除，setsid 后代退出。
    {
        let marker = "lancet-setsid-deadline";
        let script = setsid_spawn_script("ready_dl", None, marker);
        send_only(
            &mut stream,
            60,
            exec_req("inv-end-deadline", &digest, &script, 1_200, 65_536),
        );
        wait_for_file(&rw.join("ready_dl"), Duration::from_secs(5));
        let (leaf, desc_pid, pids) =
            wait_for_leaf_with_marker(&cg_root, marker, Duration::from_secs(5));
        assert!(pid_alive(desc_pid));

        let f = read_reply(&mut stream);
        assert_eq!(f.request_id, 60);
        assert!(
            matches!(
                f.message,
                Response::ExecuteResult {
                    outcome: ExecuteOutcome::Deadline { .. }
                }
            ),
            "超时应结算为 Deadline：{:?}",
            f.message
        );
        wait_for_leaf_empty_or_gone(&leaf, &pids, Duration::from_secs(5));
        assert!(!leaf.exists(), "超时后叶组应已删除：{}", leaf.display());
        let dl_rec = match query_receipt(&mut stream, 61, "inv-end-deadline") {
            ReceiptLookup::Settled { receipt } => receipt,
            other => panic!("超时后期望 Settled：{other:?}"),
        };
        assert_eq!(dl_rec.outcome, ReceiptOutcome::Deadline);
        assert_eq!(
            dl_rec.effects,
            vec![EffectEntry::new("dev", EffectKind::Exec, Vec::new()).unwrap()],
            "目标已启动后超时，回执应保留 Exec"
        );
    }

    // 5. 下一次 proc.run 正常成功。
    let next = run_act(
        &mut stream,
        70,
        "inv-after-four-endings",
        &digest,
        "return dev.run([\"echo\", \"still-healthy\"]).stdout",
    );
    match next {
        ExecuteOutcome::Succeeded { content, .. } => {
            assert_eq!(content["value"], json!("still-healthy\n"));
        }
        other => panic!("四类结束后下一次 proc.run 应成功，实际：{other:?}"),
    }
}

#[test]
fn sigkill_node_empties_sandbox_and_recovers_indeterminate() {
    // 场景 A：带可写目录的执行环境，前台与 setsid 后代均就绪后 SIGKILL 节点。
    {
        let (root, home) = temp_var_env();
        let rw = root.path().join("rw");
        let state_dir = root.path().join("state");
        std::fs::create_dir(&rw).unwrap();

        let config_path = root.path().join("node.toml");
        let toml_content = format!(
            r#"
[node]
display_name = "SIGKILL 恢复测试 A"
state_dir = {state_dir:?}

[[dir]]
name = "rw"
path = {rw:?}
access = "write"

[[env]]
name = "dev"
mounts = ["rw"]
"#,
            state_dir = state_dir.to_str().unwrap(),
            rw = rw.to_str().unwrap(),
        );

        let node =
            NodeProcess::start_delegated(&config_path, &toml_content, &state_dir, &home, &[]);
        let cg_root = node_cgroup_root(node.pid());
        let (mut stream, digest) = connect_ready(&node);

        let marker = "lancet-sigkill-rw-desc";
        let script = setsid_spawn_script("ready_kill", None, marker);
        send_only(
            &mut stream,
            10,
            exec_req("inv-sigkill-rw", &digest, &script, 30_000, 65_536),
        );
        wait_for_file(&rw.join("ready_kill"), Duration::from_secs(5));
        let (leaf, desc_pid, pids) =
            wait_for_leaf_with_marker(&cg_root, marker, Duration::from_secs(5));
        assert!(pid_alive(desc_pid));

        // 保持连接打开，先对节点进程 SIGKILL（Drop 会 kill + wait）。
        drop(node);
        drop(stream);

        // 重启前由独立观察者确认沙箱与后代均已清空退出。
        wait_for_leaf_empty_or_gone(&leaf, &pids, Duration::from_secs(5));

        // 相同状态目录重启，检查恢复为 Indeterminate、不重放、新调用成功。
        let restarted =
            NodeProcess::start_delegated(&config_path, &toml_content, &state_dir, &home, &[]);
        let (mut s2, digest2) = connect_ready(&restarted);
        assert_eq!(digest2, digest);

        match query_receipt(&mut s2, 11, "inv-sigkill-rw") {
            ReceiptLookup::Settled { receipt } => match receipt.outcome {
                ReceiptOutcome::Indeterminate { reason } => {
                    assert_eq!(reason, "节点在调用结算前重启，可能已产生 effect");
                }
                other => panic!("期望 Indeterminate，实际：{other:?}"),
            },
            other => panic!("期望 Settled：{other:?}"),
        }

        let replay = send_request(
            &mut s2,
            12,
            exec_req("inv-sigkill-rw", &digest2, "return 1", 5_000, 65_536),
        );
        assert!(matches!(
            replay.message,
            Response::Rejected {
                code: RejectCode::DuplicateInvocation,
                ..
            }
        ));

        let ok = run_act(
            &mut s2,
            13,
            "inv-sigkill-rw-next",
            &digest2,
            "return dev.run([\"echo\", \"recovered\"]).stdout",
        );
        match ok {
            ExecuteOutcome::Succeeded { content, .. } => {
                assert_eq!(content["value"], json!("recovered\n"));
            }
            other => panic!("重启后新 proc.run 应成功，实际：{other:?}"),
        }
    }

    // 场景 B：只授予只读工作目录的执行环境（无任何可写目录），SIGKILL 重启后同样结算为 Indeterminate。
    {
        let (root, home) = temp_var_env();
        let ro = root.path().join("ro");
        let state_dir = root.path().join("state");
        std::fs::create_dir(&ro).unwrap();

        let config_path = root.path().join("node.toml");
        let toml_content = format!(
            r#"
[node]
display_name = "SIGKILL 只读执行环境测试 B"
state_dir = {state_dir:?}

[[dir]]
name = "ro"
path = {ro:?}
access = "read"

[[env]]
name = "dev"
mounts = ["ro"]
"#,
            state_dir = state_dir.to_str().unwrap(),
            ro = ro.to_str().unwrap(),
        );

        let node =
            NodeProcess::start_delegated(&config_path, &toml_content, &state_dir, &home, &[]);
        let cg_root = node_cgroup_root(node.pid());
        let (mut stream, digest) = connect_ready(&node);

        let child_code = r#"
import os, signal, time
os.setsid()
signal.signal(signal.SIGTERM, signal.SIG_IGN)
home = os.environ["HOME"]
tmp = os.path.join(home, "ready_ro_desc.tmp")
dst = os.path.join(home, "ready_ro_desc")
with open(tmp, "w") as f:
    f.write("ready")
os.replace(tmp, dst)
time.sleep(120)
"#;
        let py = format!(
            "import subprocess, time; subprocess.Popen(['python3', '-c', {child_code:?}]); time.sleep(120)"
        );
        let script = format!("return dev.run([\"python3\", \"-c\", {py:?}]).code");
        send_only(
            &mut stream,
            10,
            exec_req("inv-sigkill-ro", &digest, &script, 30_000, 65_536),
        );
        // 由 setsid 后代在私有家目录写出就绪标记，观察者确认后代已就绪再杀节点。
        let ro_ready = state_dir.join("envs/dev/home/ready_ro_desc");
        wait_for_file(&ro_ready, Duration::from_secs(5));
        let leaves = list_call_leaves(&cg_root);
        assert_eq!(leaves.len(), 1, "期望恰好存在一个活动叶组：{leaves:?}");
        let leaf = leaves[0].clone();
        let pids = cgroup_procs(&leaf);
        assert!(
            pids.len() >= 2 && pids.iter().all(|&p| pid_alive(p)),
            "后代就绪后叶组应包含存活进程：{pids:?}"
        );

        drop(node);
        drop(stream);

        wait_for_leaf_empty_or_gone(&leaf, &pids, Duration::from_secs(5));

        let restarted =
            NodeProcess::start_delegated(&config_path, &toml_content, &state_dir, &home, &[]);
        let (mut s2, digest2) = connect_ready(&restarted);
        match query_receipt(&mut s2, 11, "inv-sigkill-ro") {
            ReceiptLookup::Settled { receipt } => match receipt.outcome {
                ReceiptOutcome::Indeterminate { reason } => {
                    assert_eq!(reason, "节点在调用结算前重启，可能已产生 effect");
                }
                other => panic!("只读执行环境重启后也应结算为 Indeterminate，实际：{other:?}"),
            },
            other => panic!("期望 Settled：{other:?}"),
        }

        let replay = send_request(
            &mut s2,
            12,
            exec_req("inv-sigkill-ro", &digest2, "return 1", 5_000, 65_536),
        );
        assert!(matches!(
            replay.message,
            Response::Rejected {
                code: RejectCode::DuplicateInvocation,
                ..
            }
        ));

        let ok = run_act(
            &mut s2,
            13,
            "inv-sigkill-ro-next",
            &digest2,
            "return dev.run([\"echo\", \"ro-ok\"]).stdout",
        );
        assert!(matches!(ok, ExecuteOutcome::Succeeded { .. }));
    }
}

#[test]
fn cgroup_oom_isolation() {
    let (root, home) = temp_var_env();
    let rw = root.path().join("rw");
    let state_dir = root.path().join("state");
    std::fs::create_dir(&rw).unwrap();

    let config_path = root.path().join("node.toml");
    let toml_content = format!(
        r#"
[node]
display_name = "OOM 隔离测试"
state_dir = {state_dir:?}

[[dir]]
name = "rw"
path = {rw:?}
access = "write"

[[env]]
name = "dev"
mounts = ["rw"]
"#,
        state_dir = state_dir.to_str().unwrap(),
        rw = rw.to_str().unwrap(),
    );

    let node = NodeProcess::start_delegated(&config_path, &toml_content, &state_dir, &home, &[]);
    let cg_root = node_cgroup_root(node.pid());
    let (mut victim_conn, digest) = connect_ready(&node);
    let (mut sibling_conn, _) = connect_ready(&node);

    let sibling_py = r#"
import os, time
open("sibling_ready", "w").write("ok")
while not os.path.exists("sibling_go"):
    time.sleep(0.01)
print("sibling-ok", end="") # lancet-oom-sibling
"#;
    let victim_py = r#"
import os, time
open("victim_ready", "w").write("ok")
while not os.path.exists("victim_go"):
    time.sleep(0.01)
buf = bytearray(192 * 1024 * 1024)
for i in range(0, len(buf), 4096):
    buf[i] = 1
print(len(buf)) # lancet-oom-victim
"#;

    send_only(
        &mut sibling_conn,
        10,
        exec_req(
            "inv-oom-sibling",
            &digest,
            &format!("return dev.run([\"python3\", \"-c\", {sibling_py:?}], cwd = \"rw\").stdout"),
            30_000,
            65_536,
        ),
    );
    send_only(
        &mut victim_conn,
        20,
        exec_req(
            "inv-oom-victim",
            &digest,
            &format!("return dev.run([\"python3\", \"-c\", {victim_py:?}], cwd = \"rw\").stdout"),
            30_000,
            65_536,
        ),
    );

    wait_for_file(&rw.join("sibling_ready"), Duration::from_secs(5));
    wait_for_file(&rw.join("victim_ready"), Duration::from_secs(5));

    let (victim_leaf, _, victim_pids) =
        wait_for_leaf_with_marker(&cg_root, "lancet-oom-victim", Duration::from_secs(5));
    let (sibling_leaf, sibling_pid, _) =
        wait_for_leaf_with_marker(&cg_root, "lancet-oom-sibling", Duration::from_secs(5));
    assert_ne!(victim_leaf, sibling_leaf, "并行调用必须使用各自独立的叶组");

    // 核对产品叶组默认 memory.max 固定为 8 GiB（8589934592）。
    assert_eq!(
        std::fs::read_to_string(victim_leaf.join("memory.max"))
            .unwrap()
            .trim(),
        "8589934592"
    );
    assert_eq!(
        std::fs::read_to_string(sibling_leaf.join("memory.max"))
            .unwrap()
            .trim(),
        "8589934592"
    );

    // 外部仅把 victim 叶组下调至 64 MiB，再放行 victim 触碰 192 MiB。
    std::fs::write(victim_leaf.join("memory.max"), "67108864")
        .expect("调低 victim memory.max 失败");
    std::fs::write(rw.join("victim_go"), "go").unwrap();

    let victim_frame = read_reply(&mut victim_conn);
    assert_eq!(victim_frame.request_id, 20);
    match victim_frame.message {
        Response::ExecuteResult {
            outcome:
                ExecuteOutcome::Failed {
                    error,
                    receipt_summary: Some(summary),
                },
        } => {
            assert_eq!(error.code, "ScriptOrToolError");
            assert!(
                error.message.contains("OOM")
                    && (error.message.contains("8 GiB") || error.message.contains("8589934592")),
                "OOM 诊断应写明沙箱内存上限，实际：{}",
                error.message
            );
            assert_eq!(
                summary.effects(),
                &[EffectEntry::new("dev", EffectKind::Exec, Vec::new()).unwrap()]
            );
        }
        other => panic!("OOM 应结算为 Failed(ScriptOrToolError)，实际：{other:?}"),
    }

    // victim 叶组已清空删除，并行的兄弟调用仍存活。
    wait_for_leaf_empty_or_gone(&victim_leaf, &victim_pids, Duration::from_secs(5));
    assert!(!victim_leaf.exists(), "OOM 后叶组应已删除");
    assert!(pid_alive(sibling_pid), "并行的兄弟调用不得受 OOM 影响");

    // 放行兄弟调用并确认正常完成。
    std::fs::write(rw.join("sibling_go"), "go").unwrap();
    let sibling_frame = read_reply(&mut sibling_conn);
    assert_eq!(sibling_frame.request_id, 10);
    match sibling_frame.message {
        Response::ExecuteResult {
            outcome: ExecuteOutcome::Succeeded { content, .. },
        } => assert_eq!(content["value"], json!("sibling-ok")),
        other => panic!("并行兄弟调用应正常成功，实际：{other:?}"),
    }

    // 随后下一次调用成功。
    let next = run_act(
        &mut victim_conn,
        30,
        "inv-after-oom",
        &digest,
        "return dev.run([\"echo\", \"after-oom-ok\"]).stdout",
    );
    match next {
        ExecuteOutcome::Succeeded { content, .. } => {
            assert_eq!(content["value"], json!("after-oom-ok\n"));
        }
        other => panic!("OOM 后下一次调用应成功，实际：{other:?}"),
    }
}

#[test]
fn proc_run_check_output_window_and_receipt() {
    let (root, home) = temp_var_env();
    let rw = root.path().join("rw");
    let state_dir = root.path().join("state");
    std::fs::create_dir(&rw).unwrap();

    // 构造带可执行权限但格式无效的二进制，用于核对 ENOEXEC 独立诊断（且不回退 shell）。
    let bad_exec = rw.join("bad_exec");
    std::fs::write(&bad_exec, b"\x7fNOT_A_VALID_ELF\x00\x01\x02").unwrap();
    std::fs::set_permissions(&bad_exec, std::fs::Permissions::from_mode(0o755)).unwrap();

    let config_path = root.path().join("node.toml");
    let toml_content = format!(
        r#"
[node]
display_name = "工具行为与回执测试"
state_dir = {state_dir:?}

[[dir]]
name = "rw"
path = {rw:?}
access = "write"

[[env]]
name = "dev"
mounts = ["rw"]
"#,
        state_dir = state_dir.to_str().unwrap(),
        rw = rw.to_str().unwrap(),
    );

    let node = NodeProcess::start_delegated(&config_path, &toml_content, &state_dir, &home, &[]);
    let (mut stream, digest) = connect_ready(&node);

    // 1. 默认 check=True：非零退出报错并附 stdout 与 stderr 有界尾部，回执保留 Exec。
    let noisy_fail_py = r#"
import sys
sys.stdout.write("OUT_HEAD_" + "SENTINEL" + ("o" * 10000) + "OUT_TAIL_" + "SENTINEL")
sys.stderr.write("ERR_HEAD_" + "SENTINEL" + ("e" * 10000) + "ERR_TAIL_" + "SENTINEL")
sys.exit(7)
"#;
    let r_fail = run_act(
        &mut stream,
        10,
        "inv-check-default",
        &digest,
        &format!("return dev.run([\"python3\", \"-c\", {noisy_fail_py:?}])"),
    );
    match r_fail {
        ExecuteOutcome::Failed { error, .. } => {
            assert_eq!(error.code, "ScriptOrToolError");
            assert!(error.message.contains("7"), "{}", error.message);
            assert!(
                error.message.contains("OUT_TAIL_SENTINEL"),
                "{}",
                error.message
            );
            assert!(
                error.message.contains("ERR_TAIL_SENTINEL"),
                "{}",
                error.message
            );
            assert!(
                !error.message.contains("OUT_HEAD_SENTINEL")
                    && !error.message.contains("ERR_HEAD_SENTINEL"),
                "非零退出诊断只应保留有界尾部：{}",
                error.message
            );
        }
        other => panic!("默认 check 应对非零退出报错，实际：{other:?}"),
    }
    let rec_fail = match query_receipt(&mut stream, 11, "inv-check-default") {
        ReceiptLookup::Settled { receipt } => receipt,
        other => panic!("期望 Settled：{other:?}"),
    };
    assert_eq!(
        rec_fail.effects,
        vec![EffectEntry::new("dev", EffectKind::Exec, Vec::new()).unwrap()]
    );
    assert_eq!(
        rec_fail.steps,
        vec![StepSummary {
            tool: "proc.run".to_owned(),
            outcome: OutcomeKind::Failed,
        }]
    );

    // 2. check=False：返回退出码与两路输出。
    let r_nocheck = run_act(
        &mut stream,
        12,
        "inv-check-false",
        &digest,
        r#"
r = dev.run(["python3", "-c", "import sys; sys.stdout.write('out1'); sys.stderr.write('err2'); sys.exit(9)"], check = False)
return {"code": r.code, "stdout": r.stdout, "stderr": r.stderr}
"#,
    );
    match r_nocheck {
        ExecuteOutcome::Succeeded { content, .. } => {
            assert_eq!(
                content["value"],
                json!({"code": 9, "stdout": "out1", "stderr": "err2"})
            );
        }
        other => panic!("check=False 应成功返回结构，实际：{other:?}"),
    }

    // 3. 两路同时大量输出（1 MiB stdout + 1 MiB stderr 并行写，验证无管道死锁）。
    let dual_ok_py = r#"
import sys, threading
def w(stream, ch):
    chunk = ch * 65536
    for _ in range(16):
        stream.write(chunk)
        stream.flush()
t1 = threading.Thread(target=w, args=(sys.stdout, "A"))
t2 = threading.Thread(target=w, args=(sys.stderr, "B"))
t1.start(); t2.start(); t1.join(); t2.join()
"#;
    let r_dual_ok = run_act(
        &mut stream,
        13,
        "inv-dual-1m",
        &digest,
        &format!(
            "r = dev.run([\"python3\", \"-c\", {dual_ok_py:?}])\nreturn [len(r.stdout), len(r.stderr)]\n"
        ),
    );
    match r_dual_ok {
        ExecuteOutcome::Succeeded { content, .. } => {
            assert_eq!(content["value"], json!([1048576, 1048576]));
        }
        other => panic!("两路同时输出 1 MiB 应成功无死锁，实际：{other:?}"),
    }

    // 4. 两路同时超窗大量输出（各 6 MiB = 12 MiB > 8 MiB）：持续读完不死锁，报超窗错误并保留 Exec。
    let dual_over_py = r#"
import sys, threading
def w(stream, ch):
    chunk = ch * 65536
    for _ in range(96):
        stream.write(chunk)
        stream.flush()
t1 = threading.Thread(target=w, args=(sys.stdout, "X"))
t2 = threading.Thread(target=w, args=(sys.stderr, "Y"))
t1.start(); t2.start(); t1.join(); t2.join()
"#;
    let r_dual_over = run_act(
        &mut stream,
        14,
        "inv-dual-over",
        &digest,
        &format!("return dev.run([\"python3\", \"-c\", {dual_over_py:?}])"),
    );
    match r_dual_over {
        ExecuteOutcome::Failed { error, .. } => {
            assert_eq!(error.code, "ScriptOrToolError");
            assert!(
                error.message.contains("8 MiB") && error.message.contains("proc.pipe"),
                "两路超窗应报 8 MiB 与 proc.pipe 提示，实际：{}",
                error.message
            );
        }
        other => panic!("两路 12 MiB 应报超窗失败，实际：{other:?}"),
    }
    let rec_dual_over = match query_receipt(&mut stream, 15, "inv-dual-over") {
        ReceiptLookup::Settled { receipt } => receipt,
        other => panic!("期望 Settled：{other:?}"),
    };
    assert_eq!(
        rec_dual_over.effects,
        vec![EffectEntry::new("dev", EffectKind::Exec, Vec::new()).unwrap()]
    );

    // 5. 恰好 8 MiB 紧凑 JSON 成功，以及仅因 JSON 转义多 1 字节超窗失败。
    // `{"code":0,"stderr":"","stdout":""}` 为 34 字节。
    let skeleton = serde_json::to_string(&json!({"code": 0, "stderr": "", "stdout": ""})).unwrap();
    assert_eq!(skeleton.len(), 34);
    let exact_stdout_len = 8 * 1024 * 1024 - skeleton.len();
    let exact_script = format!(
        "r = dev.run([\"python3\", \"-c\", \"import sys; sys.stdout.write('a' * {exact_stdout_len})\"])\nreturn len(r.stdout)\n"
    );
    let r_exact = run_act(&mut stream, 16, "inv-exact-8m", &digest, &exact_script);
    match r_exact {
        ExecuteOutcome::Succeeded { content, .. } => {
            assert_eq!(content["value"], json!(exact_stdout_len));
        }
        other => panic!("恰好 8 MiB 紧凑 JSON 应成功，实际：{other:?}"),
    }

    // 原始字节数仍为 exact_stdout_len（< 8 MiB），但把 1 个 'a' 换成 '"'，JSON 转义成 `\"` 后变为 8 MiB + 1。
    let escaped_len = exact_stdout_len - 1;
    let escape_over_script = format!(
        "r = dev.run([\"python3\", \"-c\", \"import sys; sys.stdout.write('\\\"' + 'a' * {escaped_len})\"])\nreturn len(r.stdout)\n"
    );
    let r_esc_over = run_act(
        &mut stream,
        17,
        "inv-escape-over-8m",
        &digest,
        &escape_over_script,
    );
    match r_esc_over {
        ExecuteOutcome::Failed { error, .. } => {
            assert_eq!(error.code, "ScriptOrToolError");
            assert!(
                error.message.contains("8388609") && error.message.contains("proc.pipe"),
                "JSON 转义超窗应报 8388609 字节与 proc.pipe，实际：{}",
                error.message
            );
        }
        other => panic!("JSON 转义超窗应失败，实际：{other:?}"),
    }

    // 6. 单次运行写多个文件只记一条 Exec（不逐项记 Write）；先成功再失败的两步保留已发生的 Exec。
    let multi_write = run_act(
        &mut stream,
        18,
        "inv-single-exec-multi-file",
        &digest,
        r#"
dev.run(["python3", "-c", "open('f1', 'w').write('1'); open('f2', 'w').write('2')"], cwd = "rw")
return "ok"
"#,
    );
    assert!(matches!(multi_write, ExecuteOutcome::Succeeded { .. }));
    let rec_mw = match query_receipt(&mut stream, 19, "inv-single-exec-multi-file") {
        ReceiptLookup::Settled { receipt } => receipt,
        other => panic!("期望 Settled：{other:?}"),
    };
    assert_eq!(
        rec_mw.effects,
        vec![EffectEntry::new("dev", EffectKind::Exec, Vec::new()).unwrap()],
        "一次实际运行写多个文件也只产生一条 Exec，不逐项记录文件写"
    );

    let two_step_fail = run_act(
        &mut stream,
        20,
        "inv-two-exec-second-fails",
        &digest,
        "dev.run([\"true\"])\ndev.run([\"false\"])\n",
    );
    assert!(matches!(two_step_fail, ExecuteOutcome::Failed { .. }));
    let rec_ts = match query_receipt(&mut stream, 21, "inv-two-exec-second-fails") {
        ReceiptLookup::Settled { receipt } => receipt,
        other => panic!("期望 Settled：{other:?}"),
    };
    assert_eq!(
        rec_ts.steps,
        vec![
            StepSummary {
                tool: "proc.run".to_owned(),
                outcome: OutcomeKind::Succeeded,
            },
            StepSummary {
                tool: "proc.run".to_owned(),
                outcome: OutcomeKind::Failed,
            },
        ]
    );
    assert_eq!(
        rec_ts.effects,
        vec![
            EffectEntry::new("dev", EffectKind::Exec, Vec::new()).unwrap(),
            EffectEntry::new("dev", EffectKind::Exec, Vec::new()).unwrap(),
        ]
    );

    // 7. 启动失败（ENOENT 与 ENOEXEC）各有独立诊断，且不记录 Exec。
    let r_enoent = run_act(
        &mut stream,
        22,
        "inv-enoent",
        &digest,
        "dev.run([\"definitely_missing_binary_lancet_test\"])",
    );
    match r_enoent {
        ExecuteOutcome::Failed { error, .. } => {
            assert_eq!(error.code, "ScriptOrToolError");
            assert!(error.message.contains("ENOENT"), "{}", error.message);
        }
        other => panic!("ENOENT 应失败，实际：{other:?}"),
    }
    let rec_enoent = match query_receipt(&mut stream, 23, "inv-enoent") {
        ReceiptLookup::Settled { receipt } => receipt,
        other => panic!("期望 Settled：{other:?}"),
    };
    assert!(
        rec_enoent.effects.is_empty(),
        "ENOENT 未启动目标，不应记 Exec"
    );

    let r_enoexec = run_act(
        &mut stream,
        24,
        "inv-enoexec",
        &digest,
        &format!("dev.run([\"{}\"])", bad_exec.to_str().unwrap()),
    );
    match r_enoexec {
        ExecuteOutcome::Failed { error, .. } => {
            assert_eq!(error.code, "ScriptOrToolError");
            assert!(error.message.contains("ENOEXEC"), "{}", error.message);
        }
        other => panic!("ENOEXEC 应失败，实际：{other:?}"),
    }
    let rec_enoexec = match query_receipt(&mut stream, 25, "inv-enoexec") {
        ReceiptLookup::Settled { receipt } => receipt,
        other => panic!("期望 Settled：{other:?}"),
    };
    assert!(
        rec_enoexec.effects.is_empty(),
        "ENOEXEC 未启动目标，不应记 Exec"
    );

    // 8. 参数过长触发 E2BIG 归类为调用方错误（ScriptOrToolError），且不记录 Exec。
    let r_e2big = run_act(
        &mut stream,
        26,
        "inv-e2big",
        &digest,
        "dev.run([\"echo\", \"a\" * 200000])",
    );
    match r_e2big {
        ExecuteOutcome::Failed { error, .. } => {
            assert_eq!(error.code, "ScriptOrToolError");
            assert!(
                error.message.contains("E2BIG"),
                "E2BIG 应归类为调用方错误并带 E2BIG 诊断，实际：{}",
                error.message
            );
        }
        other => panic!("E2BIG 应结算为 ScriptOrToolError，实际：{other:?}"),
    }
    let rec_e2big = match query_receipt(&mut stream, 27, "inv-e2big") {
        ReceiptLookup::Settled { receipt } => receipt,
        other => panic!("期望 Settled：{other:?}"),
    };
    assert!(
        rec_e2big.effects.is_empty(),
        "E2BIG 未启动目标，不应记 Exec"
    );
}

#[test]
fn startup_validation_and_private_home_symlink_defense() {
    let (root, home) = temp_var_env();
    let app = root.path().join("app");
    let state_base = root.path().join("states");
    let tool_file = root.path().join("a_file.txt");
    let existing_state = root.path().join("existing_state");
    std::fs::create_dir_all(&app).unwrap();
    std::fs::create_dir_all(&state_base).unwrap();
    std::fs::create_dir_all(&existing_state).unwrap();
    std::fs::write(&tool_file, "file").unwrap();

    // 指向状态目录的符号链接工具，以及经 `link/../pkg` 绕过词法折叠指向状态目录内部的路径。
    let link_to_state = home.join("link_state");
    std::os::unix::fs::symlink(&existing_state, &link_to_state).unwrap();

    let state_sub = existing_state.join("sub");
    let state_pkg = existing_state.join("pkg");
    let harmless_pkg = home.join("pkg");
    std::fs::create_dir_all(&state_sub).unwrap();
    std::fs::create_dir_all(&state_pkg).unwrap();
    std::fs::create_dir_all(&harmless_pkg).unwrap();
    let link_state_sub = home.join("link_sub");
    std::os::unix::fs::symlink(&state_sub, &link_state_sub).unwrap();

    // 1. 不通过 systemd-run 启动授予 [[env]] 的节点，缺少 memory 委派或共享组时报错退出并含环境名。
    {
        let cfg = root.path().join("no-delegate.toml");
        let st = state_base.join("s-nodeleg");
        let toml = format!(
            "[node]\ndisplay_name = \"无委派\"\nstate_dir = {:?}\n\n[[env]]\nname = \"dev\"\n",
            st.to_str().unwrap()
        );
        let err = run_expect_failure(&cfg, &toml, &home);
        assert!(
            err.contains("dev") && err.contains("cgroup") && err.contains("systemd-run"),
            "缺少 cgroup 委派时应包含环境名、条件与 systemd-run 提示：\n{err}"
        );
    }

    // 1b. 委派根中已存在子组时，拒绝复用并提示委派根不是本节点专用的（限时等待，防止异常挂住）。
    {
        let cfg = root.path().join("dirty-cgroup.toml");
        let st = state_base.join("s-dirtycg");
        let toml = format!(
            "[node]\ndisplay_name = \"脏委派根\"\nstate_dir = {:?}\n\n[[env]]\nname = \"dev\"\n",
            st.to_str().unwrap()
        );
        std::fs::write(&cfg, &toml).unwrap();
        let child = Command::new("systemd-run")
            .args([
                "--user",
                "--scope",
                "-p",
                "Delegate=yes",
                "--quiet",
                "--",
                "/bin/sh",
                "-c",
                r#"cg="/sys/fs/cgroup$(sed -n 's/^0:://p' /proc/self/cgroup)"; mkdir "$cg/preexisting" || exit 99; exec "$1" "$2""#,
                "_",
                BIN,
            ])
            .arg(&cfg)
            .env("HOME", &home)
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .expect("启动脏委派根测试失败");
        let err = wait_child_expect_failure(child);
        assert!(
            err.contains("dev")
                && err.contains("不是本节点专用的")
                && err.contains("preexisting")
                && err.contains("systemd-run"),
            "脏委派根应报告环境名、非专用、子组名与 systemd-run 提示：\n{err}"
        );
    }

    // 2. tools / caches / mounts / vars 各类路径与配置校验。
    let shm_state = tempfile::Builder::new()
        .prefix("ln-shm-state-")
        .tempdir_in("/dev/shm")
        .expect("在 /dev/shm 创建状态目录父目录失败");
    let xfs_cache = home.join("xfs_cache_seed");
    let xfs_ro_sub = xfs_cache.join("ro_sub");
    let xfs_ro_nested = xfs_ro_sub.join("copied_nested");
    std::fs::create_dir_all(&xfs_ro_nested).unwrap();
    std::fs::write(xfs_ro_nested.join("item.bin"), b"data").unwrap();
    // 唯一路径 ro_sub/copied_nested/item.bin 保证只读（0555）子目录中已复制好子目录条目后，
    // 再由末级普通文件触发跨文件系统 FICLONE 失败，从而验证回滚能完整清理非空只读临时树。
    std::fs::set_permissions(&xfs_ro_nested, std::fs::Permissions::from_mode(0o555)).unwrap();
    std::fs::set_permissions(&xfs_ro_sub, std::fs::Permissions::from_mode(0o555)).unwrap();
    let shm_state_path = shm_state.path().join("state");

    let bad_cases = [
        (
            "missing-tool",
            format!(
                "[node]\ndisplay_name = \"t\"\nstate_dir = {st:?}\n[[env]]\nname = \"dev\"\ntools = [{missing:?}]\n",
                st = state_base.join("s1").to_str().unwrap(),
                missing = home.join("no_such_tool").to_str().unwrap(),
            ),
            vec!["dev", "tools"],
        ),
        (
            "tool-is-home",
            format!(
                "[node]\ndisplay_name = \"t\"\nstate_dir = {st:?}\n[[env]]\nname = \"dev\"\ntools = [{h:?}]\n",
                st = state_base.join("s2").to_str().unwrap(),
                h = home.to_str().unwrap(),
            ),
            vec!["dev", "家目录"],
        ),
        (
            "tool-overlaps-dir",
            format!(
                "[node]\ndisplay_name = \"t\"\nstate_dir = {st:?}\n[[dir]]\nname = \"app\"\npath = {app:?}\naccess = \"read\"\n[[env]]\nname = \"dev\"\ntools = [{app:?}]\n",
                st = state_base.join("s3").to_str().unwrap(),
                app = app.to_str().unwrap(),
            ),
            vec!["dev", "app"],
        ),
        (
            "tool-symlink-to-state",
            format!(
                "[node]\ndisplay_name = \"t\"\nstate_dir = {st:?}\n[[env]]\nname = \"dev\"\ntools = [{link:?}]\n",
                st = existing_state.to_str().unwrap(),
                link = link_to_state.to_str().unwrap(),
            ),
            vec!["dev", "节点状态目录"],
        ),
        (
            "tool-symlink-dotdot-to-state",
            format!(
                "[node]\ndisplay_name = \"t\"\nstate_dir = {st:?}\n[[env]]\nname = \"dev\"\ntools = [\"~/link_sub/../pkg\"]\n",
                st = existing_state.to_str().unwrap(),
            ),
            vec!["dev", "节点状态目录"],
        ),
        (
            "tool-covers-usr",
            format!(
                "[node]\ndisplay_name = \"t\"\nstate_dir = {st:?}\n[[env]]\nname = \"dev\"\ntools = [\"/usr/bin\"]\n",
                st = state_base.join("s5").to_str().unwrap(),
            ),
            vec!["dev", "/usr/bin"],
        ),
        (
            "cache-is-file",
            format!(
                "[node]\ndisplay_name = \"t\"\nstate_dir = {st:?}\n[[env]]\nname = \"dev\"\ncaches = [{f:?}]\n",
                st = state_base.join("s6").to_str().unwrap(),
                f = tool_file.to_str().unwrap(),
            ),
            vec!["dev", "caches", "目录"],
        ),
        (
            "mount-covers-home-root",
            format!(
                "[node]\ndisplay_name = \"t\"\nstate_dir = {st:?}\n[[dir]]\nname = \"h\"\npath = {h:?}\naccess = \"read\"\n[[env]]\nname = \"dev\"\nmounts = [\"h\"]\n",
                st = state_base.join("s7").to_str().unwrap(),
                h = home.to_str().unwrap(),
            ),
            vec!["dev", "私有家目录"],
        ),
        (
            "vars-reserved-home",
            format!(
                "[node]\ndisplay_name = \"t\"\nstate_dir = {st:?}\n[[env]]\nname = \"dev\"\nvars = {{ HOME = \"/tmp\" }}\n",
                st = state_base.join("s8").to_str().unwrap(),
            ),
            vec!["dev", "HOME"],
        ),
        (
            "vars-reserved-path",
            format!(
                "[node]\ndisplay_name = \"t\"\nstate_dir = {st:?}\n[[env]]\nname = \"dev\"\nvars = {{ PATH = \"/bin\" }}\n",
                st = state_base.join("s9").to_str().unwrap(),
            ),
            vec!["dev", "PATH"],
        ),
        (
            "cache-cross-fs-ficlone",
            format!(
                "[node]\ndisplay_name = \"t\"\nstate_dir = {st:?}\n[[env]]\nname = \"dev\"\ncaches = [\"~/xfs_cache_seed\"]\n",
                st = shm_state_path.to_str().unwrap(),
            ),
            vec!["dev", "FICLONE", "xfs_cache_seed", "/dev/shm"],
        ),
    ];

    for (name, toml, needles) in bad_cases {
        let cfg = root.path().join(format!("{name}.toml"));
        let err = run_delegated_expect_failure(&cfg, &toml, &home);
        for needle in needles {
            assert!(
                err.contains(needle),
                "启动校验用例 `{name}` 报错应包含 `{needle}`，实际：\n{err}"
            );
        }
    }

    // 确认跨文件系统播种失败后没有残留临时目录，并还原测试夹具权限以便清理。
    let shm_caches = shm_state_path.join("envs/dev/caches");
    if shm_caches.exists() {
        let leftover: Vec<_> = std::fs::read_dir(&shm_caches)
            .unwrap()
            .filter_map(Result::ok)
            .map(|e| e.file_name())
            .collect();
        assert!(
            leftover.is_empty(),
            "缓存播种失败后不应残留临时目录：{leftover:?}"
        );
    }
    let _ = std::fs::set_permissions(&xfs_ro_sub, std::fs::Permissions::from_mode(0o755));
    let _ = std::fs::set_permissions(&xfs_ro_nested, std::fs::Permissions::from_mode(0o755));

    // 3. 运行期私有家目录挂载点符号链接防护：私有家目录内挂载父路径出现符号链接时本次调用报错，无 Exec。
    let nested_tool = home.join(".nested/tool_dir");
    std::fs::create_dir_all(&nested_tool).unwrap();
    std::fs::write(nested_tool.join("ok.txt"), "ok").unwrap();

    let sym_state = state_base.join("s-sym");
    let sym_cfg = root.path().join("sym.toml");
    let sym_toml = format!(
        r#"
[node]
display_name = "私有家目录链接防护"
state_dir = {st:?}

[[env]]
name = "dev"
tools = ["~/.nested/tool_dir"]
"#,
        st = sym_state.to_str().unwrap(),
    );
    let node = NodeProcess::start_delegated(&sym_cfg, &sym_toml, &sym_state, &home, &[]);
    let (mut stream, digest) = connect_ready(&node);

    // 往该环境的私有家目录放入符号链接 `.nested -> /etc`。
    let priv_home = sym_state.join("envs/dev/home");
    std::os::unix::fs::symlink("/etc", priv_home.join(".nested")).unwrap();

    let res = run_act(
        &mut stream,
        10,
        "inv-home-symlink-attack",
        &digest,
        "return dev.run([\"true\"])",
    );
    match res {
        ExecuteOutcome::Failed { error, .. } => {
            assert_eq!(error.code, "ScriptOrToolError");
            assert!(
                error.message.contains("符号链接"),
                "遇到私有家目录挂载点符号链接应报错，实际：{}",
                error.message
            );
        }
        other => panic!("私有家目录出现符号链接应报错，实际：{other:?}"),
    }
    let rec = match query_receipt(&mut stream, 11, "inv-home-symlink-attack") {
        ReceiptLookup::Settled { receipt } => receipt,
        other => panic!("期望 Settled：{other:?}"),
    };
    assert!(rec.effects.is_empty(), "挂载前被拦截不应产生 Exec");
}
