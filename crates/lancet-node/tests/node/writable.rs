// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use lancet_protocol::{
    CatalogDigest, EffectEntry, EffectKind, ExecuteOutcome, Frame, Labels, ReceiptLookup,
    ReceiptOutcome, RejectCode, Request, Residue, Response, write_frame,
};
use serde_json::json;
use tempfile::TempDir;

use super::{NodeProcess, describe, hello_v1, send_request, temp_env};

const WAIT_TIMEOUT: Duration = Duration::from_secs(10);

const BUSY_SCRIPT: &str = r#"
acc = 0
for i in range(100000):
    for j in range(100000):
        acc = (acc + i + j) % 97
return acc
"#;

fn start_node_with(
    timeout: Duration,
    dir_defs: impl FnOnce(&Path) -> String,
) -> (TempDir, PathBuf, PathBuf, PathBuf, NodeProcess) {
    let (root, home) = temp_env();
    let state_dir = root.path().join("state");
    let config_path = root.path().join("node.toml");
    let dirs_toml = dir_defs(root.path());
    let toml_content = format!(
        "[node]\ndisplay_name = \"可写测试节点\"\nstate_dir = {state_dir:?}\n\n{dirs_toml}",
        state_dir = state_dir.to_str().unwrap(),
    );
    let process =
        NodeProcess::start_with_timeout(&config_path, &toml_content, &state_dir, &home, timeout);
    (root, home, config_path, state_dir, process)
}

fn connect_ready(process: &NodeProcess) -> (UnixStream, CatalogDigest) {
    let mut stream = process.connect();
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
        exec_req(inv_id, digest, script, 10_000, 65_536),
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

fn assert_direct_write_entries(effects: &[EffectEntry], handle: &str, expected_targets: &[&str]) {
    assert_eq!(
        effects.len(),
        expected_targets.len(),
        "Write 条目数应等于实际写入次数：{effects:?}"
    );
    for (entry, expected_target) in effects.iter().zip(expected_targets) {
        assert_eq!(entry.handle(), handle);
        assert_eq!(entry.kind(), EffectKind::Write);
        assert_eq!(
            entry.changes().len(),
            1,
            "每条 Write 应恰好包含一个变更：{entry:?}"
        );
        let change = &entry.changes()[0];
        assert_eq!(change.target, *expected_target);
        assert_eq!(change.action, "write");
        assert_eq!(change.residue, Residue::NotRetained);
        assert_eq!(change.diff, None);
        assert!(!change.diff_truncated);
    }
}

#[test]
fn write_tools_obey_paths_and_edit_cardinality() {
    let (root, _home, _cfg, state_dir, process) =
        start_node_with(Duration::from_secs(10), |root| {
            let w = root.join("w");
            let ro = root.join("ro");
            std::fs::create_dir_all(w.join("build")).unwrap();
            std::fs::create_dir_all(w.join(".git")).unwrap();
            std::fs::create_dir_all(ro.join(".git")).unwrap();

            std::fs::write(root.join("outside.txt"), "outside-orig").unwrap();
            std::fs::write(w.join("keep.txt"), "alpha foo beta foo\n").unwrap();
            std::fs::set_permissions(w.join("keep.txt"), std::fs::Permissions::from_mode(0o640))
                .unwrap();
            std::fs::write(w.join("target.txt"), "target0\n").unwrap();
            std::fs::write(w.join(".env"), "KEY=old\n").unwrap();
            std::fs::write(w.join("build/cache.txt"), "seed0").unwrap();
            std::fs::write(w.join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
            std::fs::write(ro.join(".git/HEAD"), "ref: refs/heads/ro\n").unwrap();

            std::os::unix::fs::symlink("target.txt", w.join("rel_link")).unwrap();
            std::os::unix::fs::symlink("../outside.txt", w.join("escape_link")).unwrap();

            format!(
                r#"
[[dir]]
name = "w"
path = {w:?}
access = "write"

[[dir]]
name = "ro"
path = {ro:?}
access = "read"
"#,
                w = w.to_str().unwrap(),
                ro = ro.to_str().unwrap(),
            )
        });
    let host_w = root.path().join("w");
    let (mut stream, digest) = connect_ready(&process);

    // 1. write 直接写宿主：自动建上级目录、同内容空操作、替换保留权限 0o640、经相对链接写目标、.git/点文件/构建目录一视同仁。
    let r1 = run_act(
        &mut stream,
        10,
        "inv-write-ok",
        &digest,
        r#"
w.write("nested/dir/new.txt", "hello\n")
w.write("nested/dir/new.txt", "hello\n")
w.write("keep.txt", "one foo two foo\n")
w.write("rel_link", "via link\n")
w.write(".env", "KEY=new\n")
w.write(".git/HEAD", "ref: refs/heads/dev\n")
w.write("build/cache.txt", "seed1")
return {
    "new": w.read("nested/dir/new.txt"),
    "target": w.read("target.txt"),
    "env": w.read(".env"),
    "w_git": w.read(".git/HEAD"),
    "ro_git": ro.read(".git/HEAD"),
    "root_names": [e.name for e in w.list(".")],
    "git_hits": w.search("refs/heads/dev"),
}
"#,
    );
    let val1 = match r1 {
        ExecuteOutcome::Succeeded { content, .. } => content["value"].clone(),
        other => panic!("期望成功，实际：{other:?}"),
    };
    assert_eq!(val1["new"], json!("hello\n"));
    assert_eq!(val1["target"], json!("via link\n"));
    assert_eq!(val1["env"], json!("KEY=new\n"));
    assert_eq!(val1["w_git"], json!("ref: refs/heads/dev\n"));
    assert_eq!(val1["ro_git"], json!("ref: refs/heads/ro\n"));
    let root_names: Vec<&str> = val1["root_names"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert!(
        root_names.contains(&".git"),
        "list 应包含 .git：{root_names:?}"
    );
    assert!(
        root_names.contains(&".env"),
        "list 应包含 .env：{root_names:?}"
    );
    assert!(
        root_names.contains(&"build"),
        "list 应包含 build：{root_names:?}"
    );
    assert_eq!(
        val1["git_hits"],
        json!([{"path": ".git/HEAD", "line_no": 1, "text": "ref: refs/heads/dev"}])
    );

    // 宿主磁盘立即看到全部写入，且替换已有文件保留权限 0o640。
    assert_eq!(
        std::fs::read_to_string(host_w.join("nested/dir/new.txt")).unwrap(),
        "hello\n"
    );
    assert_eq!(
        std::fs::read_to_string(host_w.join("keep.txt")).unwrap(),
        "one foo two foo\n"
    );
    assert_eq!(
        std::fs::read_to_string(host_w.join("target.txt")).unwrap(),
        "via link\n"
    );
    assert_eq!(
        std::fs::read_to_string(host_w.join(".env")).unwrap(),
        "KEY=new\n"
    );
    assert_eq!(
        std::fs::read_to_string(host_w.join(".git/HEAD")).unwrap(),
        "ref: refs/heads/dev\n"
    );
    assert_eq!(
        std::fs::read_to_string(host_w.join("build/cache.txt")).unwrap(),
        "seed1"
    );
    let mode = std::fs::symlink_metadata(host_w.join("keep.txt"))
        .unwrap()
        .permissions()
        .mode()
        & 0o7777;
    assert_eq!(mode, 0o640, "替换已有普通文件时应保持原权限 0640");

    let rec1 = match query_receipt(&mut stream, 11, "inv-write-ok") {
        ReceiptLookup::Settled { receipt } => receipt,
        other => panic!("期望 Settled：{other:?}"),
    };
    assert_direct_write_entries(
        &rec1.effects,
        "w",
        &[
            "nested/dir/new.txt",
            "keep.txt",
            "target.txt",
            ".env",
            ".git/HEAD",
            "build/cache.txt",
        ],
    );

    // 2. edit 的 0 次、多次(all=None / False / True)、1 次(all=None / False)与出错前后字节不变。
    for (idx, bad_edit) in [
        "w.edit(\"keep.txt\", \"missing\", \"X\")",
        "w.edit(\"keep.txt\", \"foo\", \"bar\")",
        "w.edit(\"keep.txt\", \"foo\", \"bar\", False)",
        "w.edit(\"absent.txt\", \"a\", \"b\")",
        "w.edit(\"keep.txt\", \"\", \"x\")",
    ]
    .into_iter()
    .enumerate()
    {
        let inv = format!("inv-edit-err-{idx}");
        let res = run_act(&mut stream, 20 + idx as u64, &inv, &digest, bad_edit);
        assert!(
            matches!(res, ExecuteOutcome::Failed { .. }),
            "`{bad_edit}` 应失败，实际：{res:?}"
        );
        let rec = match query_receipt(&mut stream, 30 + idx as u64, &inv) {
            ReceiptLookup::Settled { receipt } => receipt,
            other => panic!("期望 Settled：{other:?}"),
        };
        assert!(rec.effects.is_empty(), "`{bad_edit}` 失败不得产生 ε");
        assert_eq!(
            std::fs::read_to_string(host_w.join("keep.txt")).unwrap(),
            "one foo two foo\n",
            "`{bad_edit}` 失败前后宿主文件字节不得变化"
        );
    }

    let r_edit_ok = run_act(
        &mut stream,
        40,
        "inv-edit-ok",
        &digest,
        r#"
w.edit("keep.txt", "foo", "bar", True)
w.edit("keep.txt", "one", "ONE")
w.edit("keep.txt", "two", "TWO", False)
w.write("empty.txt", "")
w.edit("empty.txt", "", "hello")
return {"keep": w.read("keep.txt"), "empty": w.read("empty.txt")}
"#,
    );
    let edit_summary = match r_edit_ok {
        ExecuteOutcome::Succeeded {
            content,
            receipt_summary,
        } => {
            assert_eq!(content["value"]["keep"], json!("ONE bar TWO bar\n"));
            assert_eq!(content["value"]["empty"], json!("hello"));
            receipt_summary
        }
        other => panic!("合法 edit 序列（含空文件空 old）应成功，实际：{other:?}"),
    };
    let expected_edit_targets = ["keep.txt", "keep.txt", "keep.txt", "empty.txt", "empty.txt"];
    assert_direct_write_entries(edit_summary.effects(), "w", &expected_edit_targets);
    let rec_edit_ok = match query_receipt(&mut stream, 41, "inv-edit-ok") {
        ReceiptLookup::Settled { receipt } => receipt,
        other => panic!("期望 Settled：{other:?}"),
    };
    assert_direct_write_entries(&rec_edit_ok.effects, "w", &expected_edit_targets);
    assert_eq!(
        std::fs::read_to_string(host_w.join("keep.txt")).unwrap(),
        "ONE bar TWO bar\n"
    );
    assert_eq!(
        std::fs::read_to_string(host_w.join("empty.txt")).unwrap(),
        "hello"
    );

    // 3. 叶名过长导致 write 失败时，不残留新建的空目录且不记录 ε。
    let long_name = "x".repeat(256);
    let r_long = run_act(
        &mut stream,
        401,
        "inv-write-nametoolong",
        &digest,
        &format!("w.write(\"created/{long_name}\", \"x\")"),
    );
    assert!(
        matches!(r_long, ExecuteOutcome::Failed { .. }),
        "超长叶名 write 应失败，实际：{r_long:?}"
    );
    assert!(
        !host_w.join("created").exists(),
        "write 失败后不得残留本次新建的空目录 `created`"
    );
    let rec_long = match query_receipt(&mut stream, 402, "inv-write-nametoolong") {
        ReceiptLookup::Settled { receipt } => receipt,
        other => panic!("期望 Settled：{other:?}"),
    };
    assert!(rec_long.effects.is_empty());

    // 4. 越界路径被拒，树外文件不被修改，无 ε。
    for (idx, script) in [
        "w.write(\"../outside.txt\", \"x\")",
        "w.write(\"/tmp/abs.txt\", \"x\")",
        "w.write(\"escape_link\", \"x\")",
    ]
    .into_iter()
    .enumerate()
    {
        let inv = format!("inv-oob-{idx}");
        let res = run_act(&mut stream, 50 + idx as u64, &inv, &digest, script);
        match res {
            ExecuteOutcome::Failed { error, .. } => {
                assert_eq!(error.code, "ScriptOrToolError");
                assert!(error.message.contains("越出"), "{}", error.message);
            }
            other => panic!("`{script}` 应被拒绝，实际：{other:?}"),
        }
        let rec = match query_receipt(&mut stream, 100 + idx as u64, &inv) {
            ReceiptLookup::Settled { receipt } => receipt,
            other => panic!("期望 Settled：{other:?}"),
        };
        assert!(rec.effects.is_empty(), "`{script}` 不应产生 ε");
    }
    assert_eq!(
        std::fs::read_to_string(root.path().join("outside.txt")).unwrap(),
        "outside-orig"
    );

    // 5. 非 UTF-8 文件调用 edit 应报编码错误且字节不变、无 ε。
    std::fs::write(host_w.join("bad.bin"), [0xffu8, 0xfe]).unwrap();
    let bad_utf8 = run_act(
        &mut stream,
        150,
        "inv-edit-bad-utf8",
        &digest,
        "w.edit(\"bad.bin\", \"a\", \"b\")",
    );
    match bad_utf8 {
        ExecuteOutcome::Failed { error, .. } => {
            assert_eq!(error.code, "ScriptOrToolError");
            assert!(error.message.contains("UTF-8"), "{}", error.message);
        }
        other => panic!("非 UTF-8 文件 edit 应失败，实际：{other:?}"),
    }
    let rec_utf8 = match query_receipt(&mut stream, 151, "inv-edit-bad-utf8") {
        ReceiptLookup::Settled { receipt } => receipt,
        other => panic!("期望 Settled：{other:?}"),
    };
    assert!(rec_utf8.effects.is_empty());
    assert_eq!(
        std::fs::read(host_w.join("bad.bin")).unwrap(),
        vec![0xffu8, 0xfe]
    );

    // 6. 已删除的旧工具与内置函数在脚本环境中不存在（check 阶段拦截）。
    for (idx, removed_script) in [
        "commit()",
        "discard()",
        "w.status()",
        "w.diff()",
        "w.reset(\"keep.txt\")",
        "ro.status()",
        "ro.diff()",
    ]
    .into_iter()
    .enumerate()
    {
        let res = run_act(
            &mut stream,
            200 + idx as u64,
            &format!("inv-removed-{idx}"),
            &digest,
            removed_script,
        );
        match res {
            ExecuteOutcome::Failed { error, .. } => {
                assert_eq!(error.code, "ScriptOrToolError");
                assert!(
                    error.message.contains("check"),
                    "已删除项 `{removed_script}` 应在 check 阶段失败，实际：{}",
                    error.message
                );
            }
            other => panic!("已删除项 `{removed_script}` 应失败，实际：{other:?}"),
        }
    }

    // 7. 节点不再建立 shadows/ 与 publish/ 目录。
    assert!(!state_dir.join("shadows").exists());
    assert!(!state_dir.join("publish").exists());
}

#[test]
fn write_before_fail_persists_on_host_and_in_receipt() {
    let (root, _home, _cfg, _state_dir, process) =
        start_node_with(Duration::from_secs(10), |root| {
            let w = root.join("w");
            std::fs::create_dir(&w).unwrap();
            format!(
                "[[dir]]\nname = \"w\"\npath = {:?}\naccess = \"write\"\n",
                w.to_str().unwrap()
            )
        });
    let host_w = root.path().join("w");
    let (mut stream, digest) = connect_ready(&process);

    let outcome = run_act(
        &mut stream,
        10,
        "inv-write-then-fail",
        &digest,
        "w.write(\"partial.txt\", \"already-on-host\")\nfail(\"boom\")\n",
    );
    let summary = match outcome {
        ExecuteOutcome::Failed {
            error,
            receipt_summary: Some(summary),
        } => {
            assert_eq!(error.code, "ScriptOrToolError");
            assert!(error.message.contains("boom"), "{}", error.message);
            summary
        }
        other => panic!("期望带摘要的 Failed，实际：{other:?}"),
    };

    assert_eq!(
        std::fs::read_to_string(host_w.join("partial.txt")).expect("宿主应读得到新写入内容"),
        "already-on-host"
    );
    assert_direct_write_entries(summary.effects(), "w", &["partial.txt"]);

    let receipt = match query_receipt(&mut stream, 11, "inv-write-then-fail") {
        ReceiptLookup::Settled { receipt } => receipt,
        other => panic!("期望 Settled 回执，实际：{other:?}"),
    };
    assert_direct_write_entries(&receipt.effects, "w", &["partial.txt"]);
}

#[test]
fn writable_output_limit_checks_exact_compact_json_bytes() {
    let (_root, _home, _cfg, _state_dir, process) =
        start_node_with(Duration::from_secs(10), |root| {
            let w = root.join("w");
            std::fs::create_dir(&w).unwrap();
            format!(
                "[[dir]]\nname = \"w\"\npath = {:?}\naccess = \"write\"\n",
                w.to_str().unwrap()
            )
        });
    let (mut stream, digest) = connect_ready(&process);

    // `return 1` 的紧凑 JSON content 为 `{"printed":"","value":1}`，恰好 24 字节。
    let ok = run_exec(
        &mut stream,
        10,
        exec_req("inv-limit-24", &digest, "return 1", 5_000, 24),
    );
    match ok {
        ExecuteOutcome::Succeeded { content, .. } => {
            assert_eq!(content, json!({"value": 1, "printed": ""}));
            assert_eq!(serde_json::to_vec(&content).unwrap().len(), 24);
        }
        other => panic!("有可写目录时 output_limit=24 应成功，实际：{other:?}"),
    }

    // 上限 23 字节：不拒收，受理后返回 OutputLimitExceeded。
    let over = run_exec(
        &mut stream,
        11,
        exec_req("inv-limit-23", &digest, "return 1", 5_000, 23),
    );
    match over {
        ExecuteOutcome::Failed {
            error,
            receipt_summary: Some(_),
        } => {
            assert_eq!(error.code, "OutputLimitExceeded");
        }
        other => panic!("output_limit=23 应受理后返回 OutputLimitExceeded，实际：{other:?}"),
    }
    match query_receipt(&mut stream, 12, "inv-limit-23") {
        ReceiptLookup::Settled { receipt } => match receipt.outcome {
            ReceiptOutcome::Failed { error } => assert_eq!(error.code, "OutputLimitExceeded"),
            other => panic!("回执应为 OutputLimitExceeded，实际：{other:?}"),
        },
        other => panic!("期望 Settled 回执，实际：{other:?}"),
    }
}

#[test]
fn restart_settles_writable_pending_as_indeterminate() {
    let (root, home, config_path, state_dir, process) =
        start_node_with(Duration::from_secs(10), |root| {
            let w = root.join("w");
            std::fs::create_dir(&w).unwrap();
            std::fs::write(w.join("a.txt"), "base").unwrap();
            format!(
                "[[dir]]\nname = \"w\"\npath = {:?}\naccess = \"write\"\n",
                w.to_str().unwrap()
            )
        });
    let host_a = root.path().join("w/a.txt");

    let (mut s1, digest) = connect_ready(&process);
    let crash_script = format!("w.write(\"a.txt\", \"before-kill\")\n{BUSY_SCRIPT}");
    write_frame(
        &mut s1,
        &Frame {
            request_id: 10,
            message: exec_req("inv-crash-w", &digest, &crash_script, 20_000, 65_536),
        },
    )
    .unwrap();

    // 等写入直接落到宿主文件且调用仍在 Pending 时杀掉节点进程。
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if std::fs::read_to_string(&host_a).ok().as_deref() == Some("before-kill") {
            break;
        }
        assert!(Instant::now() < deadline, "等待写入落盘超时");
        thread::sleep(Duration::from_millis(10));
    }
    // 保持连接打开，先杀节点进程并等待退出，避免先关连接触发 Cancelled。
    drop(process);
    drop(s1);

    // 重启节点；在首个 hello 之前检查账本文件已结算为 Indeterminate，且未建立 shadows/publish 目录。
    let toml_content = std::fs::read_to_string(&config_path).unwrap();
    let restarted = NodeProcess::start(&config_path, &toml_content, &state_dir, &home);

    assert!(!state_dir.join("shadows").exists());
    assert!(!state_dir.join("publish").exists());

    let calls_dir = state_dir.join("calls");
    let record_json = std::fs::read_dir(&calls_dir)
        .unwrap()
        .filter_map(Result::ok)
        .find(|e| e.file_name().to_str().is_some_and(|n| n.ends_with(".json")))
        .map(|e| std::fs::read_to_string(e.path()).unwrap())
        .expect("calls 目录下应存在调用记录文件");
    assert!(
        record_json.contains("\"state\":\"Settled\"")
            && record_json.contains("\"kind\":\"Indeterminate\""),
        "重启后首个 hello 之前记录应已结算为 Indeterminate，实际：\n{record_json}"
    );

    let (mut s2, digest2) = connect_ready(&restarted);
    assert_eq!(digest2, digest);
    match query_receipt(&mut s2, 11, "inv-crash-w") {
        ReceiptLookup::Settled { receipt } => match receipt.outcome {
            ReceiptOutcome::Indeterminate { reason } => {
                assert!(
                    reason.contains("节点在调用结算前重启"),
                    "Indeterminate 原因不符：{reason}"
                );
            }
            other => panic!("期望 Indeterminate，实际：{other:?}"),
        },
        other => panic!("期望 Settled：{other:?}"),
    }

    // 原 id 拒绝重放。
    let replay = send_request(
        &mut s2,
        12,
        exec_req("inv-crash-w", &digest2, "return 1", 5_000, 65_536),
    );
    match replay.message {
        Response::Rejected { code, .. } => assert_eq!(code, RejectCode::DuplicateInvocation),
        other => panic!("原 id 不应重放，实际：{other:?}"),
    }

    // 宿主上已完成的写入仍在，后续调用可直接读到。
    assert_eq!(std::fs::read_to_string(&host_a).unwrap(), "before-kill");
    let check = run_act(
        &mut s2,
        13,
        "inv-check-after-restart",
        &digest2,
        "return w.read(\"a.txt\")",
    );
    match check {
        ExecuteOutcome::Succeeded { content, .. } => {
            assert_eq!(content["value"], json!("before-kill"));
        }
        other => panic!("重启后应可直接读宿主文件，实际：{other:?}"),
    }
}

#[test]
fn trailing_slash_symlink_to_file_fails_read_list_and_write() {
    let (root, _home, _cfg, _state_dir, process) =
        start_node_with(Duration::from_secs(10), |root| {
            let w = root.join("w");
            std::fs::create_dir(&w).unwrap();
            std::fs::write(w.join("plain"), "original\n").unwrap();
            std::os::unix::fs::symlink("plain/", w.join("alias")).unwrap();
            format!(
                "[[dir]]\nname = \"w\"\npath = {:?}\naccess = \"write\"\n",
                w.to_str().unwrap()
            )
        });
    let plain_path = root.path().join("w/plain");
    let orig_ino = std::fs::symlink_metadata(&plain_path).unwrap().ino();
    let (mut stream, digest) = connect_ready(&process);

    for (idx, script) in [
        "return w.read(\"alias\")",
        "return w.read(\"alias\", missing_ok = True)",
        "return w.list(\"alias\")",
        "return w.list(\"alias\", missing_ok = True)",
        "w.write(\"alias\", \"overwritten\\n\")",
    ]
    .into_iter()
    .enumerate()
    {
        let inv = format!("inv-trailing-slash-{idx}");
        let outcome = run_act(&mut stream, 10 + idx as u64, &inv, &digest, script);
        match outcome {
            ExecuteOutcome::Failed { error, .. } => {
                assert_eq!(error.code, "ScriptOrToolError");
                assert!(
                    error.message.contains("不是目录"),
                    "`{script}` 应报不是目录，实际：{}",
                    error.message
                );
            }
            other => panic!("`{script}` 应失败，实际：{other:?}"),
        }
        let rec = match query_receipt(&mut stream, 30 + idx as u64, &inv) {
            ReceiptLookup::Settled { receipt } => receipt,
            other => panic!("期望 Settled：{other:?}"),
        };
        assert!(rec.effects.is_empty(), "`{script}` 不应产生 ε");
    }

    assert_eq!(
        std::fs::symlink_metadata(&plain_path).unwrap().ino(),
        orig_ino,
        "`plain` 的 inode 不应改变"
    );
    assert_eq!(
        std::fs::read_to_string(&plain_path).unwrap(),
        "original\n",
        "`plain` 内容必须保持原样"
    );

    // 父目录清单列出 `plain`，略去目标无效的 `alias`。
    match run_act(
        &mut stream,
        40,
        "inv-trailing-slash-list-root",
        &digest,
        "return [e.name for e in w.list(\".\")]",
    ) {
        ExecuteOutcome::Succeeded { content, .. } => {
            assert_eq!(content["value"], json!(["plain"]));
        }
        other => panic!("列举根目录应成功，实际：{other:?}"),
    }
}

#[test]
fn write_only_file_same_content_is_never_replaced() {
    let (root, _home, _cfg, _state_dir, process) =
        start_node_with(Duration::from_secs(10), |root| {
            let w = root.join("w");
            std::fs::create_dir(&w).unwrap();
            let target = w.join("writeonly.txt");
            std::fs::write(&target, "hello").unwrap();
            std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o200)).unwrap();
            format!(
                "[[dir]]\nname = \"w\"\npath = {:?}\naccess = \"write\"\n",
                w.to_str().unwrap()
            )
        });
    let target = root.path().join("w/writeonly.txt");
    let orig_ino = std::fs::symlink_metadata(&target).unwrap().ino();
    // 节点与测试同一身份：以映射 root 等身份运行时 0200 仍可读，同内容写入应成功且不写。
    let readable = std::fs::File::open(&target).is_ok();
    let (mut stream, digest) = connect_ready(&process);

    let outcome = run_act(
        &mut stream,
        10,
        "inv-unreadable-same-content",
        &digest,
        "w.write(\"writeonly.txt\", \"hello\")",
    );
    match (readable, outcome) {
        (false, ExecuteOutcome::Failed { error, .. }) => {
            assert_eq!(error.code, "ScriptOrToolError");
            assert!(
                error.message.contains("比较内容失败"),
                "不可读文件比较内容失败时应返回读取错误，实际：{}",
                error.message
            );
        }
        (true, ExecuteOutcome::Succeeded { .. }) => {}
        (readable, other) => {
            panic!("同内容写入的结局与可读性（{readable}）不符，实际：{other:?}")
        }
    }

    let rec = match query_receipt(&mut stream, 11, "inv-unreadable-same-content") {
        ReceiptLookup::Settled { receipt } => receipt,
        other => panic!("期望 Settled：{other:?}"),
    };
    assert!(rec.effects.is_empty(), "同内容写入不应产生 ε");
    assert_eq!(
        std::fs::symlink_metadata(&target).unwrap().ino(),
        orig_ino,
        "同内容写入不应替换文件，inode 应保持不变"
    );

    // 恢复权限以核对原内容并确保临时目录可清理。
    std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o600)).unwrap();
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "hello");
}
