// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use std::collections::BTreeMap;
use std::fs;
use std::io::Write as _;
use std::net::Shutdown;
use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use lancet_node::shadow::publish::{
    BatchPath, CallRecord, FileId, JournalOp, JournalRoot, MAX_DIAGNOSTIC_BYTES, ObjectState,
    PathDisposition, PathDispositionEntry, PublishFinish, PublishJournal, PublishOutcome,
    PublishPrep, StagedBase, call_record_file_name, call_record_hash, prepare_publish,
};
use lancet_node::shadow::{ShadowDir, compute_shadow_key};
use lancet_protocol::{
    CatalogDigest, ExecuteOutcome, Frame, Labels, MAX_RECEIPT_SUMMARY_BYTES, OutcomeKind, Receipt,
    ReceiptLookup, ReceiptOutcome, RejectCode, Request, Residue, Response, StepSummary, read_frame,
    write_frame,
};
use rustix::fs::{CWD, RenameFlags, renameat_with};
use serde_json::json;
use sha2::{Digest as _, Sha256};
use tempfile::TempDir;

use super::{NodeProcess, describe, hello_v1, run_expect_failure, send_request, temp_env};

const WAIT_TIMEOUT: Duration = Duration::from_secs(30);

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
        "[node]\ndisplay_name = \"发布测试节点\"\nstate_dir = {state_dir:?}\n\n{dirs_toml}",
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
        exec_req(inv_id, digest, script, 30_000, 65_536),
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

fn wait_settled(
    stream: &mut UnixStream,
    next_id: &mut u64,
    inv_id: &str,
    deadline: Instant,
) -> Receipt {
    loop {
        let id = *next_id;
        *next_id += 1;
        match query_receipt(stream, id, inv_id) {
            ReceiptLookup::Settled { receipt } => return receipt,
            ReceiptLookup::Pending => {
                assert!(Instant::now() < deadline, "等待 `{inv_id}` 结算超时");
                thread::sleep(Duration::from_millis(10));
            }
            other => panic!("等待 `{inv_id}` 结算时收到意外状态：{other:?}"),
        }
    }
}

fn sha256_bytes(data: &[u8]) -> [u8; 32] {
    Sha256::digest(data).into()
}

fn scan_dir_files(root: &Path) -> BTreeMap<String, (u32, [u8; 32])> {
    fn walk(base: &Path, cur: &Path, out: &mut BTreeMap<String, (u32, [u8; 32])>) {
        let Ok(rd) = fs::read_dir(cur) else {
            return;
        };
        for entry in rd.flatten() {
            let path = entry.path();
            let rel = path
                .strip_prefix(base)
                .unwrap()
                .to_string_lossy()
                .into_owned();
            let m = fs::symlink_metadata(&path).unwrap();
            if m.is_dir() {
                walk(base, &path, out);
            } else if m.is_file() {
                let bytes = fs::read(&path).unwrap();
                out.insert(rel, (m.mode() & 0o7777, sha256_bytes(&bytes)));
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(root, root, &mut out);
    out
}

fn file_id(path: &Path) -> FileId {
    let m = fs::symlink_metadata(path).unwrap();
    FileId {
        dev: m.dev(),
        ino: m.ino(),
    }
}

fn write_call_record(state_dir: &Path, inv_id: &str, record: &CallRecord) -> PathBuf {
    let calls_dir = state_dir.join("calls");
    fs::create_dir_all(&calls_dir).unwrap();
    let path = calls_dir.join(call_record_file_name(inv_id));
    fs::write(&path, serde_json::to_vec(record).unwrap()).unwrap();
    path
}

fn read_call_record(state_dir: &Path, inv_id: &str) -> Option<CallRecord> {
    let path = state_dir.join("calls").join(call_record_file_name(inv_id));
    match fs::read(&path) {
        Ok(bytes) => Some(serde_json::from_slice(&bytes).unwrap()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => panic!("读取调用记录失败：{e}"),
    }
}

// =============================================================================
// 1. commit_marks_final_pending_across_handles
// =============================================================================

#[test]
fn commit_marks_final_pending_across_handles() {
    let (root, _home, _cfg, state_dir, process) =
        start_node_with(Duration::from_secs(10), |root| {
            let w1 = root.join("w1");
            let w2 = root.join("w2");
            let ro = root.join("ro");
            fs::create_dir(&w1).unwrap();
            fs::create_dir(&w2).unwrap();
            fs::create_dir(&ro).unwrap();
            fs::write(w1.join("prev.txt"), "base-prev\n").unwrap();
            fs::write(w2.join("init.txt"), "base-init\n").unwrap();
            fs::write(ro.join("info.txt"), "readonly\n").unwrap();
            format!(
                r#"
[[dir]]
name = "w1"
path = {w1:?}
access = "write"

[[dir]]
name = "w2"
path = {w2:?}
access = "write"

[[dir]]
name = "ro"
path = {ro:?}
access = "read"
"#,
                w1 = w1.to_str().unwrap(),
                w2 = w2.to_str().unwrap(),
                ro = ro.to_str().unwrap(),
            )
        });
    let host_w1 = root.path().join("w1");
    let host_w2 = root.path().join("w2");
    let (mut stream, digest) = connect_ready(&process);

    // 1. 前一次调用只在 w1 留草稿，未调用 commit()，宿主不变。
    let r1 = run_act(
        &mut stream,
        10,
        "inv-prev-draft",
        &digest,
        "w1.write(\"prev.txt\", \"from-prev\\n\")\nreturn 1",
    );
    assert!(matches!(r1, ExecuteOutcome::Succeeded { .. }));
    assert_eq!(
        fs::read_to_string(host_w1.join("prev.txt")).unwrap(),
        "base-prev\n"
    );

    // 2. 本次调用先 commit()，再写 w2 与 w1，再重复 commit()；作为单个批次一起发布。
    let r2 = run_act(
        &mut stream,
        11,
        "inv-commit-multi",
        &digest,
        r#"
commit()
w2.write("after.txt", "from-w2\n")
w1.write("second.txt", "from-w1\n")
commit()
print("published")
return {"ok": True}
"#,
    );
    match &r2 {
        ExecuteOutcome::Succeeded {
            content,
            receipt_summary,
        } => {
            assert_eq!(content["value"], json!({"ok": true}));
            assert_eq!(
                content["printed"],
                json!("published\n"),
                "发布清零后不应附带待定提示"
            );
            assert_eq!(
                receipt_summary
                    .steps()
                    .iter()
                    .filter(|s| s.tool == "commit" && s.outcome == OutcomeKind::Succeeded)
                    .count(),
                2,
                "重复调用 commit() 应记录两条 commit 步骤"
            );
        }
        other => panic!("跨句柄 commit() 应成功，实际：{other:?}"),
    }

    assert_eq!(
        fs::read_to_string(host_w1.join("prev.txt")).unwrap(),
        "from-prev\n"
    );
    assert_eq!(
        fs::read_to_string(host_w1.join("second.txt")).unwrap(),
        "from-w1\n"
    );
    assert_eq!(
        fs::read_to_string(host_w2.join("after.txt")).unwrap(),
        "from-w2\n"
    );

    let rec2 = match query_receipt(&mut stream, 12, "inv-commit-multi") {
        ReceiptLookup::Settled { receipt } => receipt,
        other => panic!("期望 Settled：{other:?}"),
    };
    let shadow_writes: Vec<(&str, &str)> = rec2
        .effects
        .iter()
        .flat_map(|e| {
            e.changes()
                .iter()
                .filter(|c| c.residue == Residue::None)
                .map(move |c| (e.handle(), c.target.as_str()))
        })
        .collect();
    let published: Vec<(&str, &str, &str)> = rec2
        .effects
        .iter()
        .flat_map(|e| {
            e.changes()
                .iter()
                .filter(|c| c.residue == Residue::NotRetained)
                .map(move |c| (e.handle(), c.target.as_str(), c.action.as_str()))
        })
        .collect();
    assert_eq!(
        shadow_writes,
        vec![("w2", "after.txt"), ("w1", "second.txt")],
        "影子内写入应记为 Residue::None"
    );
    assert_eq!(
        published,
        vec![
            ("w1", "prev.txt", "publish"),
            ("w1", "second.txt", "publish"),
            ("w2", "after.txt", "publish"),
        ],
        "发布到宿主应按句柄与路径记为 action=\"publish\"、Residue::NotRetained"
    );

    // 确认发布后两目录 status 均为空。
    let r_st = run_act(
        &mut stream,
        13,
        "inv-status-empty",
        &digest,
        "return [w1.status(), w2.status()]",
    );
    match r_st {
        ExecuteOutcome::Succeeded { content, .. } => {
            assert_eq!(content["value"], json!([[], []]));
        }
        other => panic!("查询 status 失败：{other:?}"),
    }

    // 3. commit(); discard(); write B -> 丢弃旧草稿，仅发布 B。
    let r3 = run_act(
        &mut stream,
        14,
        "inv-commit-discard-write",
        &digest,
        r#"
w1.write("dropped.txt", "no\n")
commit()
discard()
w1.write("kept.txt", "yes\n")
"#,
    );
    assert!(matches!(r3, ExecuteOutcome::Succeeded { .. }));
    assert!(!host_w1.join("dropped.txt").exists());
    assert_eq!(
        fs::read_to_string(host_w1.join("kept.txt")).unwrap(),
        "yes\n"
    );

    // 4. 空批次 commit() 正常成功、记录 commit 步骤、不创建 publish 目录。
    let inv_empty = "inv-commit-empty";
    let r4 = run_act(&mut stream, 15, inv_empty, &digest, "commit()\nreturn 0");
    assert!(matches!(r4, ExecuteOutcome::Succeeded { .. }));
    let rec4 = match query_receipt(&mut stream, 16, inv_empty) {
        ReceiptLookup::Settled { receipt } => receipt,
        other => panic!("期望 Settled：{other:?}"),
    };
    assert_eq!(
        rec4.steps,
        vec![StepSummary {
            tool: "commit".to_owned(),
            outcome: OutcomeKind::Succeeded,
        }]
    );
    assert!(rec4.effects.is_empty());
    assert!(
        !state_dir
            .join("publish")
            .join(call_record_hash(inv_empty))
            .exists(),
        "空批次 commit() 不应创建发布事务目录"
    );

    // 5. 纯只读节点调用 commit() 同样成功并记步。
    let (_ro_root, _ro_home, _ro_cfg, _ro_state, ro_proc) =
        start_node_with(Duration::from_secs(10), |root| {
            let ro = root.join("ro");
            fs::create_dir(&ro).unwrap();
            format!(
                "[[dir]]\nname = \"ro\"\npath = {:?}\naccess = \"read\"\n",
                ro.to_str().unwrap()
            )
        });
    let (mut ro_stream, ro_digest) = connect_ready(&ro_proc);
    let ro_res = run_act(
        &mut ro_stream,
        10,
        "inv-ro-commit",
        &ro_digest,
        "commit()\nreturn \"ok\"",
    );
    match ro_res {
        ExecuteOutcome::Succeeded {
            content,
            receipt_summary,
        } => {
            assert_eq!(content["value"], json!("ok"));
            assert_eq!(
                receipt_summary.steps(),
                &[StepSummary {
                    tool: "commit".to_owned(),
                    outcome: OutcomeKind::Succeeded,
                }]
            );
            assert!(receipt_summary.effects().is_empty());
        }
        other => panic!("只读节点调用 commit() 应成功，实际：{other:?}"),
    }
}

// =============================================================================
// 2. commit_requires_valid_return_and_preserves_drafts
// =============================================================================

#[test]
fn commit_requires_valid_return_and_preserves_drafts() {
    let (root, _home, _cfg, state_dir, process) =
        start_node_with(Duration::from_secs(10), |root| {
            let w = root.join("w");
            fs::create_dir(&w).unwrap();
            fs::write(w.join("a.txt"), "base\n").unwrap();
            format!(
                "[[dir]]\nname = \"w\"\npath = {:?}\naccess = \"write\"\n",
                w.to_str().unwrap()
            )
        });
    let host_w = root.path().join("w");
    let canonical_w = fs::canonicalize(&host_w).unwrap();
    let shadow_key = compute_shadow_key(&canonical_w, &[], &[]);
    let shadow_tree = state_dir.join("shadows").join(&shadow_key).join("tree");
    let (mut stream, digest) = connect_ready(&process);

    // 先写入一份基础草稿。
    let init = run_act(
        &mut stream,
        10,
        "inv-seed-draft",
        &digest,
        "w.write(\"a.txt\", \"draft\\n\")",
    );
    assert!(matches!(init, ExecuteOutcome::Succeeded { .. }));

    #[derive(Clone, Copy)]
    enum Expected {
        Succeeded,
        FailedCode(&'static str),
        Deadline,
    }

    struct NonPublishCase<'a> {
        label: &'static str,
        script: &'a str,
        rem_ms: u64,
        limit: u64,
        expected: Expected,
        unique_draft: Option<&'static str>,
    }

    // 表驱动覆盖所有不应触发发布且保留草稿的情形：每行显式指定预期结局/错误码与本次独有草稿。
    let busy_deadline =
        format!("w.write(\"draft_deadline.txt\", \"dl\\n\")\ncommit()\n{BUSY_SCRIPT}");
    let non_publish_cases = [
        NonPublishCase {
            label: "未调用 commit",
            script: "w.write(\"draft_nocommit.txt\", \"nc\\n\")\nreturn 1",
            rem_ms: 5_000,
            limit: 65_536,
            expected: Expected::Succeeded,
            unique_draft: Some("draft_nocommit.txt"),
        },
        NonPublishCase {
            label: "静态检查失败",
            script: "commit()\nx = unknown_ident\n",
            rem_ms: 5_000,
            limit: 65_536,
            expected: Expected::FailedCode("ScriptOrToolError"),
            unique_draft: None,
        },
        NonPublishCase {
            label: "脚本运行期 fail",
            script: "w.write(\"draft_rt_fail.txt\", \"rt\\n\")\ncommit()\nfail(\"boom\")\n",
            rem_ms: 5_000,
            limit: 65_536,
            expected: Expected::FailedCode("ScriptOrToolError"),
            unique_draft: Some("draft_rt_fail.txt"),
        },
        NonPublishCase {
            label: "工具报错",
            script: "w.write(\"draft_tool_err.txt\", \"te\\n\")\ncommit()\nw.edit(\"absent.txt\", \"x\", \"y\")\n",
            rem_ms: 5_000,
            limit: 65_536,
            expected: Expected::FailedCode("ScriptOrToolError"),
            unique_draft: Some("draft_tool_err.txt"),
        },
        NonPublishCase {
            label: "返回值不可序列化",
            script: "def f():\n    return 1\nw.write(\"draft_unser.txt\", \"us\\n\")\ncommit()\nreturn f\n",
            rem_ms: 5_000,
            limit: 65_536,
            expected: Expected::FailedCode("ScriptOrToolError"),
            unique_draft: Some("draft_unser.txt"),
        },
        NonPublishCase {
            label: "截止时间耗尽",
            script: &busy_deadline,
            rem_ms: 250,
            limit: 65_536,
            expected: Expected::Deadline,
            unique_draft: Some("draft_deadline.txt"),
        },
    ];

    for (idx, case) in non_publish_cases.iter().enumerate() {
        let label = case.label;
        let inv = format!("inv-nopub-{idx}");
        let out = run_exec(
            &mut stream,
            20 + idx as u64,
            exec_req(&inv, &digest, case.script, case.rem_ms, case.limit),
        );
        match (case.expected, &out) {
            (Expected::Succeeded, ExecuteOutcome::Succeeded { .. }) => {}
            (Expected::FailedCode(code), ExecuteOutcome::Failed { error, .. })
                if error.code == code => {}
            (Expected::Deadline, ExecuteOutcome::Deadline { .. }) => {}
            _ => panic!("用例 `{label}` 结局不符合预期，实际：{out:?}"),
        }
        assert_eq!(
            fs::read_to_string(host_w.join("a.txt")).unwrap(),
            "base\n",
            "用例 `{label}` 不得改写宿主"
        );
        assert_eq!(
            fs::read_to_string(shadow_tree.join("a.txt")).unwrap(),
            "draft\n",
            "用例 `{label}` 必须保留此前影子草稿"
        );
        if let Some(draft_name) = case.unique_draft {
            assert!(
                shadow_tree.join(draft_name).is_file(),
                "用例 `{label}` 必须保留本次写入的独有草稿 `{draft_name}`"
            );
            assert!(
                !host_w.join(draft_name).exists(),
                "用例 `{label}` 不得把独有草稿 `{draft_name}` 发布到宿主"
            );
        }
    }

    // 交回载荷恰好超 output_limit - 84 一字节 -> OutputLimitExceeded，不发布、保留草稿。
    let payload_script = "commit()\nprint(\"hi\")\nreturn \"abc\"";
    let exact_bytes = serde_json::to_vec(&json!({
        "value": "abc",
        "printed": "hi\n",
    }))
    .unwrap()
    .len() as u64;

    let over = run_exec(
        &mut stream,
        40,
        exec_req(
            "inv-over-limit",
            &digest,
            "w.write(\"draft_over.txt\", \"ov\\n\")\ncommit()\nprint(\"hi\")\nreturn \"abc\"",
            5_000,
            exact_bytes + 83,
        ),
    );
    match over {
        ExecuteOutcome::Failed { error, .. } => {
            assert_eq!(error.code, "OutputLimitExceeded");
            assert!(error.message.contains("待定文件 "));
        }
        other => panic!("多 1 字节应报 OutputLimitExceeded，实际：{other:?}"),
    }
    assert_eq!(fs::read_to_string(host_w.join("a.txt")).unwrap(), "base\n");
    assert!(shadow_tree.join("draft_over.txt").is_file());
    assert!(!host_w.join("draft_over.txt").exists());

    // 显式取消（脚本忙循环中在 commit() 后被 Cancel）。
    let busy_commit = format!("w.write(\"flag_cancel.txt\", \"1\")\ncommit()\n{BUSY_SCRIPT}");
    write_frame(
        &mut stream,
        &Frame {
            request_id: 50,
            message: exec_req(
                "inv-cancel-after-commit",
                &digest,
                &busy_commit,
                20_000,
                65_536,
            ),
        },
    )
    .unwrap();
    let dl = Instant::now() + Duration::from_secs(5);
    while !shadow_tree.join("flag_cancel.txt").exists() {
        assert!(Instant::now() < dl, "等待 flag_cancel.txt 超时");
        thread::sleep(Duration::from_millis(5));
    }
    let (mut s_ctl, _) = connect_ready(&process);
    let c_resp = send_request(
        &mut s_ctl,
        10,
        Request::Cancel {
            invocation_id: "inv-cancel-after-commit".to_owned(),
        },
    );
    assert!(matches!(c_resp.message, Response::CancelOk));
    let exec_resp: Frame<Response> = read_frame(&mut stream).unwrap().unwrap();
    assert!(matches!(
        exec_resp.message,
        Response::ExecuteResult {
            outcome: ExecuteOutcome::Cancelled { .. }
        }
    ));
    assert_eq!(fs::read_to_string(host_w.join("a.txt")).unwrap(), "base\n");
    assert!(!host_w.join("flag_cancel.txt").exists());

    // 连接断开（脚本忙循环中在 commit() 后断连）。
    let (mut s_drop, _) = connect_ready(&process);
    let busy_drop = format!("w.write(\"flag_drop.txt\", \"1\")\ncommit()\n{BUSY_SCRIPT}");
    write_frame(
        &mut s_drop,
        &Frame {
            request_id: 60,
            message: exec_req("inv-drop-after-commit", &digest, &busy_drop, 20_000, 65_536),
        },
    )
    .unwrap();
    let dl2 = Instant::now() + Duration::from_secs(5);
    while !shadow_tree.join("flag_drop.txt").exists() {
        assert!(Instant::now() < dl2, "等待 flag_drop.txt 超时");
        thread::sleep(Duration::from_millis(5));
    }
    let _ = s_drop.shutdown(Shutdown::Both);
    drop(s_drop);
    let mut next_id = 20;
    let rec_drop = wait_settled(
        &mut s_ctl,
        &mut next_id,
        "inv-drop-after-commit",
        Instant::now() + Duration::from_secs(5),
    );
    assert_eq!(rec_drop.outcome, ReceiptOutcome::Cancelled);
    assert_eq!(fs::read_to_string(host_w.join("a.txt")).unwrap(), "base\n");

    // 边界正例：载荷恰好等于 output_limit - 84 时正常发布且无待定提示。
    let exact = run_exec(
        &mut stream,
        70,
        exec_req(
            "inv-exact-limit-commit",
            &digest,
            payload_script,
            5_000,
            exact_bytes + 84,
        ),
    );
    match exact {
        ExecuteOutcome::Succeeded { content, .. } => {
            assert_eq!(content["value"], json!("abc"));
            assert_eq!(content["printed"], json!("hi\n"));
        }
        other => panic!("恰好等于 output_limit - 84 的 commit() 应成功发布，实际：{other:?}"),
    }
    assert_eq!(fs::read_to_string(host_w.join("a.txt")).unwrap(), "draft\n");
    assert!(host_w.join("flag_cancel.txt").exists());
    assert!(host_w.join("flag_drop.txt").exists());
}

// =============================================================================
// 3. publish_reports_each_conflict_before_writing
// =============================================================================

#[test]
fn publish_reports_each_conflict_before_writing() {
    let (root, _home, _cfg, state_dir, process) =
        start_node_with(Duration::from_secs(10), |root| {
            let w1 = root.join("w1");
            let w2 = root.join("w2");
            fs::create_dir(&w1).unwrap();
            fs::create_dir(&w2).unwrap();
            fs::write(w1.join("was_visible"), "visible-file\n").unwrap();
            fs::write(w1.join("linked.txt"), "link-base\n").unwrap();
            fs::write(w1.join("edited.txt"), "edit-base\n").unwrap();
            fs::write(w2.join("clean.txt"), "w2-base\n").unwrap();
            format!(
                r#"
[[dir]]
name = "w1"
path = {w1:?}
access = "write"
hidden = [".env", "was_visible/", "both/"]
regenerable = ["both/"]

[[dir]]
name = "w2"
path = {w2:?}
access = "write"
"#,
                w1 = w1.to_str().unwrap(),
                w2 = w2.to_str().unwrap(),
            )
        });
    let host_w1 = root.path().join("w1");
    let host_w2 = root.path().join("w2");
    let canonical_w1 = fs::canonicalize(&host_w1).unwrap();
    let shadow_key_1 = compute_shadow_key(
        &canonical_w1,
        &[
            ".env".to_owned(),
            "was_visible/".to_owned(),
            "both/".to_owned(),
        ],
        &["both/".to_owned()],
    );
    let tree_w1 = state_dir.join("shadows").join(&shadow_key_1).join("tree");
    let (mut stream, digest) = connect_ready(&process);

    // 在 w2 留一份完全干净的草稿，在 w1 制造全部冲突类型。
    let r_prep = run_act(
        &mut stream,
        10,
        "inv-prep-conflicts",
        &digest,
        r#"
w2.write("clean.txt", "w2-draft\n")
w1.write("linked.txt", "link-draft\n")
w1.write("edited.txt", "edit-draft\n")
"#,
    );
    assert!(matches!(r_prep, ExecuteOutcome::Succeeded { .. }));

    // 1. 影子内直接造 hidden 文件、把可见文件换成 hidden 目录、造同时命中 hidden+regenerable 的项。
    fs::write(tree_w1.join(".env"), "SECRET=1").unwrap();
    fs::remove_file(tree_w1.join("was_visible")).unwrap();
    fs::create_dir(tree_w1.join("was_visible")).unwrap();
    fs::create_dir(tree_w1.join("both")).unwrap();
    fs::write(tree_w1.join("both/x.txt"), "secret-regen").unwrap();

    // 2. 宿主为 linked.txt 加外部硬链接（nlink == 2），并修改 edited.txt。
    fs::hard_link(host_w1.join("linked.txt"), root.path().join("ext_link")).unwrap();
    fs::write(host_w1.join("edited.txt"), "host-edited\n").unwrap();

    // 调用 commit()：必须在任何宿主写入前全部报出。
    let r_conf = run_act(&mut stream, 11, "inv-commit-conflicts", &digest, "commit()");
    match r_conf {
        ExecuteOutcome::Failed { error, .. } => {
            assert_eq!(error.code, "Conflict");
            for expected in [
                "w1 \".env\"：hidden 路径",
                "w1 \"both\"：hidden 路径",
                "w1 \"both/x.txt\"：hidden 路径",
                "w1 \"edited.txt\"：宿主已改",
                "w1 \"linked.txt\"：多个链接（2）",
                "w1 \"was_visible\"：hidden 路径",
                "待定文件 4 个。用 commit() 发布或 discard() 丢弃。",
            ] {
                assert!(
                    error.message.contains(expected),
                    "冲突消息缺少 `{expected}`，实际：\n{}",
                    error.message
                );
            }
        }
        other => panic!("期望 Failed(Conflict)，实际：{other:?}"),
    }

    // 两个宿主目录均完全未被改动，回执无发布 effect。
    assert_eq!(
        fs::read_to_string(host_w2.join("clean.txt")).unwrap(),
        "w2-base\n",
        "第二个可写目录不得被部分发布"
    );
    assert_eq!(
        fs::read_to_string(host_w1.join("linked.txt")).unwrap(),
        "link-base\n"
    );
    assert_eq!(
        fs::read_to_string(host_w1.join("edited.txt")).unwrap(),
        "host-edited\n"
    );
    assert!(!host_w1.join(".env").exists());
    let rec = match query_receipt(&mut stream, 12, "inv-commit-conflicts") {
        ReceiptLookup::Settled { receipt } => receipt,
        other => panic!("期望 Settled：{other:?}"),
    };
    assert!(
        rec.effects.iter().all(|e| e
            .changes()
            .iter()
            .all(|c| c.residue != Residue::NotRetained)),
        "发布前冲突拒绝不得记录 NotRetained effect"
    );

    // 3. 仅有 hidden 待定项（可见 status() 为空）时 commit() 仍须以 Conflict 拒绝。
    let r_disc = run_act(&mut stream, 13, "inv-clear-all", &digest, "discard()");
    assert!(matches!(r_disc, ExecuteOutcome::Succeeded { .. }));
    fs::write(tree_w1.join(".env"), "ONLY_HIDDEN=1").unwrap();
    let r_hidden_only = run_act(
        &mut stream,
        14,
        "inv-hidden-only-commit",
        &digest,
        "st = w1.status()\ncommit()\nreturn st",
    );
    match r_hidden_only {
        ExecuteOutcome::Failed { error, .. } => {
            assert_eq!(error.code, "Conflict");
            assert!(
                error.message.contains("w1 \".env\"：hidden 路径"),
                "仅含 hidden 待定项时应报 Conflict，实际：{}",
                error.message
            );
            assert!(
                !error.message.contains("待定文件"),
                "无可见待定项时不应附带待定提示：{}",
                error.message
            );
        }
        other => panic!("仅含 hidden 待定项的 commit() 应失败，实际：{other:?}"),
    }
    assert!(!host_w1.join(".env").exists());
}

// =============================================================================
// 4. conflict_message_truncates_at_16kib
// =============================================================================

#[test]
fn conflict_message_truncates_at_16kib() {
    let total = 600usize;
    let (root, _home, _cfg, state_dir, process) =
        start_node_with(Duration::from_secs(15), |root| {
            let w = root.join("w");
            fs::create_dir(&w).unwrap();
            for i in 0..total {
                let name = format!("conflict_file_with_long_name_{i:04}_padding.txt");
                fs::write(w.join(name), "base\n").unwrap();
            }
            format!(
                "[[dir]]\nname = \"w\"\npath = {:?}\naccess = \"write\"\n",
                w.to_str().unwrap()
            )
        });
    let host_w = root.path().join("w");
    let canonical_w = fs::canonicalize(&host_w).unwrap();
    let shadow_key = compute_shadow_key(&canonical_w, &[], &[]);
    let shadow_tree = state_dir.join("shadows").join(&shadow_key).join("tree");
    let (mut stream, digest) = connect_ready(&process);

    for i in 0..total {
        let name = format!("conflict_file_with_long_name_{i:04}_padding.txt");
        fs::write(shadow_tree.join(&name), "draft\n").unwrap();
        fs::write(host_w.join(&name), "host-changed\n").unwrap();
    }

    let res = run_act(&mut stream, 10, "inv-many-conflicts", &digest, "commit()");
    match res {
        ExecuteOutcome::Failed { error, .. } => {
            assert_eq!(error.code, "Conflict");
            assert!(
                error.message.len() <= MAX_DIAGNOSTIC_BYTES,
                "冲突总字节数（含待定提示）{} 超出 16 KiB 上限 {MAX_DIAGNOSTIC_BYTES}",
                error.message.len()
            );
            let expected_hint =
                format!("\n待定文件 {total} 个。用 commit() 发布或 discard() 丢弃。\n");
            assert!(
                error.message.ends_with(&expected_hint),
                "冲突消息末尾必须保留完整待定提示，实际末尾：{:?}",
                &error.message[error.message.len().saturating_sub(120)..]
            );
            let body = error.message.strip_suffix(&expected_hint).unwrap();
            let lines: Vec<&str> = body.lines().collect();
            let last_line = lines.last().copied().unwrap();
            let omitted: usize = last_line
                .strip_prefix("另有 ")
                .and_then(|s| s.strip_suffix(" 条未列出"))
                .and_then(|s| s.parse().ok())
                .unwrap_or_else(|| panic!("尾行应为 `另有 N 条未列出`，实际：{last_line:?}"));
            let kept_lines = &lines[..lines.len() - 1];
            assert_eq!(
                kept_lines.len() + omitted,
                total,
                "保留行数 {} + 省略数 {omitted} 必须等于总冲突数 {total}",
                kept_lines.len()
            );
            for line in kept_lines {
                assert!(
                    line.starts_with("w \"conflict_file_with_long_name_")
                        && line.ends_with("：宿主已改"),
                    "截断必须在整行边界，发现残行：{line:?}"
                );
            }
        }
        other => panic!("期望 Failed(Conflict)，实际：{other:?}"),
    }
}

// =============================================================================
// 5. commit_rechecks_host_after_script_started
// =============================================================================

#[test]
fn commit_rechecks_host_after_script_started() {
    let (root, _home, _cfg, state_dir, process) =
        start_node_with(Duration::from_secs(10), |root| {
            let w = root.join("w");
            let ro = root.join("ro");
            fs::create_dir(&w).unwrap();
            fs::create_dir(&ro).unwrap();
            fs::write(w.join("a.txt"), "base-a\n").unwrap();
            fs::write(w.join("other.txt"), "base-other\n").unwrap();
            fs::write(w.join("same_inplace.txt"), "base-s1\n").unwrap();
            fs::write(w.join("same_rename.txt"), "base-s2\n").unwrap();
            fs::write(w.join("keep_draft.txt"), "base-keep\n").unwrap();
            format!(
                r#"
[[dir]]
name = "w"
path = {w:?}
access = "write"
regenerable = ["signal/"]

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
    let host_ro = root.path().join("ro");
    let canonical_w = fs::canonicalize(&host_w).unwrap();
    let shadow_key = compute_shadow_key(&canonical_w, &[], &["signal/".to_owned()]);
    let shadow_tree = state_dir.join("shadows").join(&shadow_key).join("tree");
    let (mut stream, digest) = connect_ready(&process);

    // 同步辅助：脚本写出 regenerable 信号后轮询 ro，宿主线程改完文件后放行 ro 信号。
    let run_with_mid_script_host_edit =
        |stream: &mut UnixStream,
         req_id: u64,
         inv_id: &str,
         tag: &str,
         script_prefix: &str,
         host_edit: Box<dyn FnOnce() + Send>| {
            let sig_path = shadow_tree.join(format!("signal/{tag}"));
            let go_path = host_ro.join(format!("go_{tag}"));
            let helper = thread::spawn(move || {
                let dl = Instant::now() + Duration::from_secs(8);
                while !sig_path.exists() {
                    assert!(Instant::now() < dl, "等待脚本信号超时");
                    thread::sleep(Duration::from_millis(2));
                }
                host_edit();
                fs::write(&go_path, "go").unwrap();
            });
            let script = format!(
                r#"
{script_prefix}
w.write("signal/{tag}", "ready")
for _ in range(500000):
    if ro.read("go_{tag}", missing_ok = True) != None:
        break
commit()
return "ok"
"#
            );
            let out = run_act(stream, req_id, inv_id, &digest, &script);
            helper.join().unwrap();
            out
        };

    // 1. 脚本运行期间宿主改不同文件 other.txt -> 正常发布，双方改动都保留。
    let hw = host_w.clone();
    let r1 = run_with_mid_script_host_edit(
        &mut stream,
        10,
        "inv-recheck-other",
        "t1",
        "w.write(\"a.txt\", \"pub-a\\n\")",
        Box::new(move || {
            fs::write(hw.join("other.txt"), "host-other\n").unwrap();
        }),
    );
    assert!(
        matches!(r1, ExecuteOutcome::Succeeded { .. }),
        "改不同文件时 commit() 应成功：{r1:?}"
    );
    assert_eq!(fs::read_to_string(host_w.join("a.txt")).unwrap(), "pub-a\n");
    assert_eq!(
        fs::read_to_string(host_w.join("other.txt")).unwrap(),
        "host-other\n"
    );

    // 2. 脚本运行期间宿主原地改同一文件 same_inplace.txt -> Conflict。
    let hw = host_w.clone();
    let r2 = run_with_mid_script_host_edit(
        &mut stream,
        11,
        "inv-recheck-inplace",
        "t2",
        "w.write(\"same_inplace.txt\", \"draft-s1\\n\")\nw.write(\"keep_draft.txt\", \"draft-keep\\n\")",
        Box::new(move || {
            fs::write(hw.join("same_inplace.txt"), "host-inplace\n").unwrap();
        }),
    );
    match r2 {
        ExecuteOutcome::Failed { error, .. } => {
            assert_eq!(error.code, "Conflict");
            assert!(error.message.contains("w \"same_inplace.txt\"：宿主已改"));
        }
        other => panic!("同文件原地修改应报 Conflict，实际：{other:?}"),
    }
    assert_eq!(
        fs::read_to_string(host_w.join("keep_draft.txt")).unwrap(),
        "base-keep\n"
    );

    // 3. 脚本运行期间宿主 rename 原子替换同一文件 same_rename.txt -> Conflict。
    let hw = host_w.clone();
    let r3 = run_with_mid_script_host_edit(
        &mut stream,
        12,
        "inv-recheck-rename",
        "t3",
        "w.write(\"same_rename.txt\", \"draft-s2\\n\")",
        Box::new(move || {
            let tmp = hw.join(".editor-tmp");
            fs::write(&tmp, "host-renamed\n").unwrap();
            fs::rename(&tmp, hw.join("same_rename.txt")).unwrap();
        }),
    );
    match r3 {
        ExecuteOutcome::Failed { error, .. } => {
            assert_eq!(error.code, "Conflict");
            assert!(error.message.contains("w \"same_rename.txt\"：宿主已改"));
        }
        other => panic!("同文件 rename 替换应报 Conflict，实际：{other:?}"),
    }

    // 4. 对两个冲突路径执行 reset 后，剩余草稿 keep_draft.txt 正常发布；
    //    注意 commit() 只标记在脚本返回后发布，脚本内 status() 仍可见 keep_draft.txt，返回后才清空。
    let r4 = run_act(
        &mut stream,
        13,
        "inv-reset-then-commit",
        &digest,
        r#"
w.reset("same_inplace.txt")
w.reset("same_rename.txt")
commit()
return [w.read("same_inplace.txt"), w.read("same_rename.txt"), w.status()]
"#,
    );
    match r4 {
        ExecuteOutcome::Succeeded { content, .. } => {
            assert_eq!(
                content["value"],
                json!([
                    "host-inplace\n",
                    "host-renamed\n",
                    [{"path": "keep_draft.txt", "action": "modified", "conflict": false}]
                ])
            );
            assert_eq!(
                content["printed"],
                json!(""),
                "脚本返回后发布清空待定，不应附带待定提示"
            );
        }
        other => panic!("reset 冲突后提交剩余草稿应成功，实际：{other:?}"),
    }
    assert_eq!(
        fs::read_to_string(host_w.join("keep_draft.txt")).unwrap(),
        "draft-keep\n"
    );
    let r5 = run_act(
        &mut stream,
        14,
        "inv-status-after-reset-commit",
        &digest,
        "return w.status()",
    );
    match r5 {
        ExecuteOutcome::Succeeded { content, .. } => {
            assert_eq!(content["value"], json!([]));
        }
        other => panic!("发布后 status 应为空：{other:?}"),
    }
}

// =============================================================================
// 6. late_conflict_reverses_the_batch
// =============================================================================

/// 并发写者对末项写入时通过同一打开 fd 取得的独立 inode 证据（第 3 轮 B4）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LastWriteEvidence {
    /// 写者在批结束前未观测到首项交换，没有改写末项。
    DidNotRun,
    /// 写者用写入时的同一 fd 核对发现末项已不是交换前旧 inode（独立确证写晚了）。
    WrotePostExchangeInode,
    /// 写者用写入时的同一 fd 核对发现末项仍是交换前旧 inode（确证写进交换前对象，不得重试）。
    WrotePreExchangeInode { first_edited_new_inode: bool },
}

fn can_retry_as_missed_window(ev: LastWriteEvidence) -> bool {
    matches!(
        ev,
        LastWriteEvidence::DidNotRun | LastWriteEvidence::WrotePostExchangeInode
    )
}

/// 用同一个已打开的写入 fd 核对首项目标 inode 是否仍为 `before_id`，再经该 fd 原地写入内容。
fn write_with_inode_evidence(path: &Path, before_id: FileId, body: &[u8]) -> bool {
    let mut f = fs::OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(path)
        .unwrap();
    let meta = f.metadata().unwrap();
    let fd_id = FileId {
        dev: meta.dev(),
        ino: meta.ino(),
    };
    let wrote_old_inode = fd_id == before_id;
    f.write_all(body).unwrap();
    wrote_old_inode
}

/// 第 4 轮项 2：将同文件系统预准备的新内容通过 `RENAME_EXCHANGE` 原子换入 `target`，
/// 并检查被换出到 `staged` 的对象是否仍是交换前旧 inode `before_id`。
/// 内容替换与先后证据取得发生在同一次内核原子操作中，彻底消除 `open(O_TRUNC)` 与 `RENAME_EXCHANGE` 的竞态误判。
fn exchange_with_inode_evidence(target: &Path, staged: &Path, before_id: FileId) -> bool {
    renameat_with(CWD, staged, CWD, target, RenameFlags::EXCHANGE).unwrap();
    file_id(staged) == before_id
}

#[test]
fn late_conflict_reverses_the_batch() {
    let file_count = 500usize;
    let max_attempts = 10usize;
    let mut hit_attempt = None;

    for attempt in 1..=max_attempts {
        let (root, _home, _cfg, state_dir, process) =
            start_node_with(Duration::from_secs(15), |root| {
                let w1 = root.join("w1");
                let w2 = root.join("w2");
                fs::create_dir(&w1).unwrap();
                fs::create_dir(&w2).unwrap();
                for i in 0..file_count {
                    fs::write(w1.join(format!("f{i:04}.txt")), format!("old-{i}\n")).unwrap();
                }
                fs::write(w2.join("other.txt"), "w2-old\n").unwrap();
                format!(
                    r#"
[[dir]]
name = "w1"
path = {w1:?}
access = "write"

[[dir]]
name = "w2"
path = {w2:?}
access = "write"
"#,
                    w1 = w1.to_str().unwrap(),
                    w2 = w2.to_str().unwrap(),
                )
            });
        let host_w1 = root.path().join("w1");
        let host_w2 = root.path().join("w2");
        let canonical_w1 = fs::canonicalize(&host_w1).unwrap();
        let canonical_w2 = fs::canonicalize(&host_w2).unwrap();
        let tree_w1 = state_dir
            .join("shadows")
            .join(compute_shadow_key(&canonical_w1, &[], &[]))
            .join("tree");
        let tree_w2 = state_dir
            .join("shadows")
            .join(compute_shadow_key(&canonical_w2, &[], &[]))
            .join("tree");
        let (mut stream, digest) = connect_ready(&process);

        for i in 0..file_count {
            fs::write(tree_w1.join(format!("f{i:04}.txt")), format!("new-{i}\n")).unwrap();
        }
        fs::write(tree_w2.join("other.txt"), "w2-new\n").unwrap();
        // 先让影子核对完全部草稿，避免 commit 前的同步开销影响竞态窗口。
        let prep = run_act(
            &mut stream,
            10,
            "inv-reconcile",
            &digest,
            "return [len(w1.status()), len(w2.status())]",
        );
        match prep {
            ExecuteOutcome::Succeeded { content, .. } => {
                assert_eq!(content["value"], json!([file_count, 1]));
            }
            other => panic!("预核对失败：{other:?}"),
        }

        let stop = Arc::new(AtomicBool::new(false));
        let stop_bg = Arc::clone(&stop);
        let hw1 = host_w1.clone();
        let last_name = format!("f{:04}.txt", file_count - 1);
        let first_before_id = file_id(&host_w1.join("f0000.txt"));
        let last_before_id = file_id(&host_w1.join(&last_name));
        let writer_staged = root.path().join(format!("writer-late-{attempt}.tmp"));
        fs::write(&writer_staged, b"host-late-edit\n").unwrap();
        let writer_staged_bg = writer_staged.clone();
        let writer = thread::spawn(move || {
            let first = hw1.join("f0000.txt");
            let last = hw1.join(&last_name);
            while !stop_bg.load(Ordering::Relaxed) {
                if fs::symlink_metadata(&first).ok().is_some_and(|m| {
                    m.dev() != first_before_id.dev || m.ino() != first_before_id.ino
                }) && fs::read_to_string(&first).ok().as_deref() == Some("new-0\n")
                {
                    let wrote_old =
                        exchange_with_inode_evidence(&last, &writer_staged_bg, last_before_id);
                    return if wrote_old {
                        LastWriteEvidence::WrotePreExchangeInode {
                            first_edited_new_inode: false,
                        }
                    } else {
                        LastWriteEvidence::WrotePostExchangeInode
                    };
                }
            }
            LastWriteEvidence::DidNotRun
        });

        let inv_id = format!("inv-late-{attempt}");
        let out = run_act(&mut stream, 11, &inv_id, &digest, "commit()");
        stop.store(true, Ordering::Relaxed);
        let evidence = writer.join().unwrap();
        let _ = fs::remove_file(&writer_staged);

        match out {
            ExecuteOutcome::Succeeded { .. } if can_retry_as_missed_window(evidence) => {
                // 写者独立证据确认未在交换前写入末项旧 inode，作废重试。
                continue;
            }
            ExecuteOutcome::Failed { error, .. }
                if matches!(evidence, LastWriteEvidence::WrotePreExchangeInode { .. })
                    && error.code == "Conflict" =>
            {
                assert!(
                    error
                        .message
                        .contains(&format!("w1 \"f{:04}.txt\"：宿主已改", file_count - 1)),
                    "晚到冲突应报出最后一项，实际：{}",
                    error.message
                );
                // 前面已发布项应全部逆序撤回，并发写者的改动保留，w2 未变，跨目录草稿保留。
                for i in 0..(file_count - 1) {
                    assert_eq!(
                        fs::read_to_string(host_w1.join(format!("f{i:04}.txt"))).unwrap(),
                        format!("old-{i}\n"),
                        "第 {i} 项应已撤回为原宿主内容"
                    );
                }
                assert_eq!(
                    fs::read_to_string(host_w1.join(format!("f{:04}.txt", file_count - 1)))
                        .unwrap(),
                    "host-late-edit\n",
                    "并发写者在最后一项上的修改必须保留"
                );
                assert_eq!(
                    fs::read_to_string(host_w2.join("other.txt")).unwrap(),
                    "w2-old\n",
                    "w2 必须保持原样"
                );
                let st_after = run_act(
                    &mut stream,
                    12,
                    "inv-st-after",
                    &digest,
                    "return [len(w1.status()), len(w2.status())]",
                );
                match st_after {
                    ExecuteOutcome::Succeeded { content, .. } => {
                        assert_eq!(content["value"], json!([file_count, 1]));
                    }
                    other => panic!("撤回后草稿应完整保留：{other:?}"),
                }
                let rec = match query_receipt(&mut stream, 13, &inv_id) {
                    ReceiptLookup::Settled { receipt } => receipt,
                    other => panic!("期望 Settled：{other:?}"),
                };
                assert!(
                    rec.effects.iter().all(|e| e
                        .changes()
                        .iter()
                        .all(|c| c.residue != Residue::NotRetained)),
                    "完整撤回不应记出任何 NotRetained 发布 effect"
                );
                hit_attempt = Some(attempt);
                break;
            }
            other => {
                panic!("并发写者轮次 {attempt}（evidence={evidence:?}）收到意外结局：{other:?}");
            }
        }
    }

    let attempts_used = hit_attempt.expect("在限定重试次数内未能触发晚到冲突窗口");
    eprintln!("late_conflict_reverses_the_batch: hit on attempt {attempts_used}/{max_attempts}");
}

// =============================================================================
// 7. rollback_preserves_host_edit_and_lists_every_path
// =============================================================================

#[test]
fn rollback_preserves_host_edit_and_lists_every_path() {
    let file_count = 240usize;
    let max_attempts = 10usize;
    let mut hit_attempt = None;

    for attempt in 1..=max_attempts {
        let (root, _home, _cfg, state_dir, process) =
            start_node_with(Duration::from_secs(15), |root| {
                let w = root.join("w");
                fs::create_dir(&w).unwrap();
                for i in 0..file_count {
                    fs::write(w.join(format!("f{i:04}.txt")), format!("old-{i}\n")).unwrap();
                }
                format!(
                    "[[dir]]\nname = \"w\"\npath = {:?}\naccess = \"write\"\n",
                    w.to_str().unwrap()
                )
            });
        let host_w = root.path().join("w");
        let canonical_w = fs::canonicalize(&host_w).unwrap();
        let tree_w = state_dir
            .join("shadows")
            .join(compute_shadow_key(&canonical_w, &[], &[]))
            .join("tree");
        let (mut stream, digest) = connect_ready(&process);

        for i in 0..file_count {
            fs::write(tree_w.join(format!("f{i:04}.txt")), format!("new-{i}\n")).unwrap();
        }
        let prep = run_act(
            &mut stream,
            10,
            "inv-reconcile",
            &digest,
            "return len(w.status())",
        );
        match prep {
            ExecuteOutcome::Succeeded { content, .. } => {
                assert_eq!(content["value"], json!(file_count));
            }
            other => panic!("预核对失败：{other:?}"),
        }

        let stop = Arc::new(AtomicBool::new(false));
        let stop_bg = Arc::clone(&stop);
        let hw = host_w.clone();
        let last_name = format!("f{:04}.txt", file_count - 1);
        let first_before_id = file_id(&host_w.join("f0000.txt"));
        let last_before_id = file_id(&host_w.join(&last_name));
        let writer_staged = root.path().join(format!("writer-indet-last-{attempt}.tmp"));
        fs::write(&writer_staged, b"host-late-last\n").unwrap();
        let writer_staged_bg = writer_staged.clone();
        let writer = thread::spawn(move || {
            let first = hw.join("f0000.txt");
            let last = hw.join(&last_name);
            while !stop_bg.load(Ordering::Relaxed) {
                if fs::symlink_metadata(&first).ok().is_some_and(|m| {
                    m.dev() != first_before_id.dev || m.ino() != first_before_id.ino
                }) && fs::read_to_string(&first).ok().as_deref() == Some("new-0\n")
                {
                    // 对首项原地写入并核对交换后新 inode（破坏可撤回性），
                    // 对末项用 RENAME_EXCHANGE 原子替换并核对换出对象是否为交换前旧 inode（触发晚到冲突）。
                    let first_was_old =
                        write_with_inode_evidence(&first, first_before_id, b"host-after-pub-0\n");
                    let last_was_old =
                        exchange_with_inode_evidence(&last, &writer_staged_bg, last_before_id);
                    return if last_was_old {
                        LastWriteEvidence::WrotePreExchangeInode {
                            first_edited_new_inode: !first_was_old,
                        }
                    } else {
                        LastWriteEvidence::WrotePostExchangeInode
                    };
                }
            }
            LastWriteEvidence::DidNotRun
        });

        let inv_id = format!("inv-indet-{attempt}");
        let out = run_act(&mut stream, 11, &inv_id, &digest, "commit()");
        stop.store(true, Ordering::Relaxed);
        let evidence = writer.join().unwrap();
        let _ = fs::remove_file(&writer_staged);

        match out {
            ExecuteOutcome::Succeeded { .. } if can_retry_as_missed_window(evidence) => {
                // 写者独立证据确认未在交换前写入末项旧 inode，作废重试。
                continue;
            }
            ExecuteOutcome::Indeterminate { reason, .. }
                if matches!(
                    evidence,
                    LastWriteEvidence::WrotePreExchangeInode {
                        first_edited_new_inode: true,
                        ..
                    }
                ) =>
            {
                assert!(
                    reason.len() <= MAX_DIAGNOSTIC_BYTES,
                    "Indeterminate 原因不得超过 16 KiB：{}",
                    reason.len()
                );
                assert!(
                    reason.contains("w \"f0000.txt\"：保留宿主修改"),
                    "首项应标注保留宿主修改，实际：\n{reason}"
                );
                // 240 条全部能放进 16 KiB，核对每一条路径都有去向说明。
                for i in 1..file_count {
                    let needle = format!("w \"f{i:04}.txt\"：已撤回");
                    assert!(
                        reason.contains(&needle),
                        "原因缺少 `{needle}`，实际：\n{reason}"
                    );
                }
                assert_eq!(
                    fs::read_to_string(host_w.join("f0000.txt")).unwrap(),
                    "host-after-pub-0\n"
                );
                assert_eq!(
                    fs::read_to_string(host_w.join(format!("f{:04}.txt", file_count - 1))).unwrap(),
                    "host-late-last\n"
                );
                for i in 1..(file_count - 1) {
                    assert_eq!(
                        fs::read_to_string(host_w.join(format!("f{i:04}.txt"))).unwrap(),
                        format!("old-{i}\n"),
                        "中间项 f{i:04}.txt 应撤回为 old"
                    );
                }

                let rec = match query_receipt(&mut stream, 12, &inv_id) {
                    ReceiptLookup::Settled { receipt } => receipt,
                    other => panic!("期望 Settled：{other:?}"),
                };
                let pub_targets: Vec<&str> = rec
                    .effects
                    .iter()
                    .flat_map(|e| {
                        e.changes()
                            .iter()
                            .filter(|c| c.residue == Residue::NotRetained)
                            .map(|c| c.target.as_str())
                    })
                    .collect();
                assert_eq!(
                    pub_targets,
                    vec!["f0000.txt"],
                    "Indeterminate 回执仅应为留在宿主的路径记出 NotRetained 发布 effect"
                );

                // 下一次调用同步后，影子应接纳宿主在 f0000.txt 上的后续修改。
                let next_read = run_act(
                    &mut stream,
                    13,
                    "inv-read-kept",
                    &digest,
                    "return w.read(\"f0000.txt\")",
                );
                match next_read {
                    ExecuteOutcome::Succeeded { content, .. } => {
                        assert_eq!(content["value"], json!("host-after-pub-0\n"));
                    }
                    other => panic!("下次同步应接纳宿主保留的修改：{other:?}"),
                }

                hit_attempt = Some(attempt);
                break;
            }
            other => {
                panic!(
                    "部分撤回并发写者轮次 {attempt}（evidence={evidence:?}）收到意外结局：{other:?}"
                );
            }
        }
    }

    let attempts_used = hit_attempt.expect("在限定重试次数内未能触发部分撤回 Indeterminate");
    eprintln!(
        "rollback_preserves_host_edit_and_lists_every_path: hit on attempt {attempts_used}/{max_attempts}"
    );
}

// =============================================================================
// 8. publish_recovery_before_first_hello（覆盖全部 10 个崩溃窗口 + 宿主修改 + 根身份不匹配）
// =============================================================================

#[test]
fn publish_recovery_before_first_hello() {
    #[derive(Clone, Copy)]
    enum Window {
        NoLogWithStaging,
        UndoBeforeFirstOp,
        UndoExchangeCrashPoint1,
        UndoExchangeCrashPoint2,
        UndoMidBatch,
        UndoAfterLastOpBeforeFinish,
        FinishCommitted,
        FinishRolledBack,
        FinishIndeterminate,
        HalfUpdatedBase,
        FinishAfterCleanupBeforeSettled,
        SettledBeforeReply,
        KilledMidRecovery,
        UndoWithHostEditPartial,
    }

    let windows = [
        ("1-无日志但有暂存目录", Window::NoLogWithStaging),
        ("2-Undo落盘但未改宿主", Window::UndoBeforeFirstOp),
        (
            "3-Undo交换崩溃点1(host_tmp=staged)",
            Window::UndoExchangeCrashPoint1,
        ),
        (
            "4-Undo交换崩溃点2(host_tmp=old)",
            Window::UndoExchangeCrashPoint2,
        ),
        ("5-Undo中途(k项已归档)", Window::UndoMidBatch),
        (
            "6-Undo末项之后Finish之前",
            Window::UndoAfterLastOpBeforeFinish,
        ),
        ("7a-Finish(Committed)", Window::FinishCommitted),
        ("7b-Finish(RolledBack)", Window::FinishRolledBack),
        ("7c-Finish(Indeterminate)", Window::FinishIndeterminate),
        ("8-Finish(Committed)半更新base", Window::HalfUpdatedBase),
        (
            "8b-Finish(Committed)已清理publish未Settled",
            Window::FinishAfterCleanupBeforeSettled,
        ),
        ("9-已Settled未回复+残留目录", Window::SettledBeforeReply),
        ("10-恢复中途再被杀", Window::KilledMidRecovery),
        ("11-Undo撤回遇宿主修改", Window::UndoWithHostEditPartial),
    ];

    for (label, window) in windows {
        let (root, home, config_path, state_dir, process) =
            start_node_with(Duration::from_secs(10), |root| {
                let w = root.join("w");
                fs::create_dir(&w).unwrap();
                fs::write(w.join("a.txt"), "orig-a\n").unwrap();
                fs::write(w.join("b.txt"), "orig-b\n").unwrap();
                fs::write(w.join("del.txt"), "orig-del\n").unwrap();
                format!(
                    "[[dir]]\nname = \"w\"\npath = {:?}\naccess = \"write\"\n",
                    w.to_str().unwrap()
                )
            });
        let host_w = root.path().join("w");
        let canonical_w = fs::canonicalize(&host_w).unwrap();
        let shadow_key = compute_shadow_key(&canonical_w, &[], &[]);
        let shadow_root = state_dir.join("shadows").join(&shadow_key);
        let tree_w = shadow_root.join("tree");
        let base_w = shadow_root.join("base");
        let host_root_id = file_id(&canonical_w);

        let (stream, digest) = connect_ready(&process);
        drop(stream);
        drop(process);

        // 构造真实草稿在 tree/（base/ 仍为原状）：修改 a.txt、修改 b.txt、删除 del.txt、新增 add.txt。
        fs::write(tree_w.join("a.txt"), "pub-a\n").unwrap();
        fs::write(tree_w.join("b.txt"), "pub-b\n").unwrap();
        fs::remove_file(tree_w.join("del.txt")).unwrap();
        fs::write(tree_w.join("add.txt"), "pub-add\n").unwrap();

        let inv_id = format!("inv-rec-{label}");
        let call_hash = call_record_hash(&inv_id);
        let call_dir = state_dir.join("publish").join(&call_hash);
        let stage_dir = call_dir.join("stage");
        let pre_dir = call_dir.join("pre");
        let base_stage_dir = call_dir.join("base");
        fs::create_dir_all(&stage_dir).unwrap();
        fs::create_dir_all(&pre_dir).unwrap();
        fs::create_dir_all(&base_stage_dir).unwrap();

        let roots = vec![JournalRoot {
            handle: "w".to_owned(),
            shadow_key: shadow_key.clone(),
            host_dev: host_root_id.dev,
            host_ino: host_root_id.ino,
        }];

        let st_orig_a = ObjectState::File {
            mode: 0o644,
            size: 7,
            sha256: sha256_bytes(b"orig-a\n"),
        };
        let st_pub_a = ObjectState::File {
            mode: 0o644,
            size: 6,
            sha256: sha256_bytes(b"pub-a\n"),
        };
        let st_orig_b = ObjectState::File {
            mode: 0o644,
            size: 7,
            sha256: sha256_bytes(b"orig-b\n"),
        };
        let st_pub_b = ObjectState::File {
            mode: 0o644,
            size: 6,
            sha256: sha256_bytes(b"pub-b\n"),
        };
        let st_orig_del = ObjectState::File {
            mode: 0o644,
            size: 9,
            sha256: sha256_bytes(b"orig-del\n"),
        };
        let st_pub_add = ObjectState::File {
            mode: 0o644,
            size: 8,
            sha256: sha256_bytes(b"pub-add\n"),
        };

        // 预准备 publish/<hash>/base/<slot>（与产品 prepare_publish 保持一致，A + D4）。
        fs::write(base_stage_dir.join("1"), b"pub-a\n").unwrap();
        fs::set_permissions(base_stage_dir.join("1"), fs::Permissions::from_mode(0o644)).unwrap();
        let sb_a = StagedBase {
            slot: 1,
            id: file_id(&base_stage_dir.join("1")),
        };

        fs::write(base_stage_dir.join("2"), b"pub-add\n").unwrap();
        fs::set_permissions(base_stage_dir.join("2"), fs::Permissions::from_mode(0o644)).unwrap();
        let sb_add = StagedBase {
            slot: 2,
            id: file_id(&base_stage_dir.join("2")),
        };

        fs::write(base_stage_dir.join("3"), b"pub-b\n").unwrap();
        fs::set_permissions(base_stage_dir.join("3"), fs::Permissions::from_mode(0o644)).unwrap();
        let sb_b = StagedBase {
            slot: 3,
            id: file_id(&base_stage_dir.join("3")),
        };

        let paths = vec![
            BatchPath {
                root_idx: 0,
                rel: "a.txt".to_owned(),
                before: Some(st_orig_a.clone()),
                after: Some(st_pub_a.clone()),
                staged_base: Some(sb_a),
            },
            BatchPath {
                root_idx: 0,
                rel: "add.txt".to_owned(),
                before: None,
                after: Some(st_pub_add.clone()),
                staged_base: Some(sb_add),
            },
            BatchPath {
                root_idx: 0,
                rel: "b.txt".to_owned(),
                before: Some(st_orig_b.clone()),
                after: Some(st_pub_b.clone()),
                staged_base: Some(sb_b),
            },
            BatchPath {
                root_idx: 0,
                rel: "del.txt".to_owned(),
                before: Some(st_orig_del.clone()),
                after: None,
                staged_base: None,
            },
        ];

        // 准备 4 个标准操作：op1=Delete(del.txt), op2=Exchange(a.txt), op3=Create(add.txt), op4=Exchange(b.txt)。
        fs::write(stage_dir.join("2"), b"pub-a\n").unwrap();
        fs::set_permissions(stage_dir.join("2"), fs::Permissions::from_mode(0o644)).unwrap();
        let id_stage_2 = file_id(&stage_dir.join("2"));

        fs::write(stage_dir.join("3"), b"pub-add\n").unwrap();
        fs::set_permissions(stage_dir.join("3"), fs::Permissions::from_mode(0o644)).unwrap();
        let id_stage_3 = file_id(&stage_dir.join("3"));

        fs::write(stage_dir.join("4"), b"pub-b\n").unwrap();
        fs::set_permissions(stage_dir.join("4"), fs::Permissions::from_mode(0o644)).unwrap();
        let id_stage_4 = file_id(&stage_dir.join("4"));

        let tmp2 = format!(".lancet-pub-{}-2", &call_hash[..16]);
        let tmp4 = format!(".lancet-pub-{}-4", &call_hash[..16]);

        let ops = vec![
            JournalOp::Delete {
                op_id: 1,
                root_idx: 0,
                rel: "del.txt".to_owned(),
                before: st_orig_del.clone(),
            },
            JournalOp::Exchange {
                op_id: 2,
                root_idx: 0,
                rel: "a.txt".to_owned(),
                staged_id: id_stage_2,
                host_tmp_name: tmp2.clone(),
                before: st_orig_a.clone(),
                after: st_pub_a.clone(),
            },
            JournalOp::Create {
                op_id: 3,
                root_idx: 0,
                rel: "add.txt".to_owned(),
                staged_id: id_stage_3,
                after: st_pub_add.clone(),
            },
            JournalOp::Exchange {
                op_id: 4,
                root_idx: 0,
                rel: "b.txt".to_owned(),
                staged_id: id_stage_4,
                host_tmp_name: tmp4.clone(),
                before: st_orig_b.clone(),
                after: st_pub_b.clone(),
            },
        ];

        // 依据窗口摆好磁盘状态与 CallRecord。
        let record = match window {
            Window::NoLogWithStaging => CallRecord::Pending {
                invocation_id: inv_id.clone(),
                tool: "act".to_owned(),
                catalog_digest: digest.clone(),
                labels: Labels::new(),
                writes_possible: true,
                publish: None,
            },
            Window::UndoBeforeFirstOp => CallRecord::Pending {
                invocation_id: inv_id.clone(),
                tool: "act".to_owned(),
                catalog_digest: digest.clone(),
                labels: Labels::new(),
                writes_possible: true,
                publish: Some(PublishJournal::Undo {
                    call_hash: call_hash.clone(),
                    roots: roots.clone(),
                    ops,
                    paths,
                    rollback_reason: None,
                }),
            },
            Window::UndoExchangeCrashPoint1 => {
                // op1 已删，op2 刚把 stage/2 移到 host_tmp（未交换）。
                fs::rename(host_w.join("del.txt"), pre_dir.join("1")).unwrap();
                fs::rename(stage_dir.join("2"), host_w.join(&tmp2)).unwrap();
                CallRecord::Pending {
                    invocation_id: inv_id.clone(),
                    tool: "act".to_owned(),
                    catalog_digest: digest.clone(),
                    labels: Labels::new(),
                    writes_possible: true,
                    publish: Some(PublishJournal::Undo {
                        call_hash: call_hash.clone(),
                        roots: roots.clone(),
                        ops,
                        paths,
                        rollback_reason: None,
                    }),
                }
            }
            Window::UndoExchangeCrashPoint2 => {
                // op1 已删，op2 已 RENAME_EXCHANGE（host/a.txt=staged, host_tmp=orig-a），未移入 pre/2。
                fs::rename(host_w.join("del.txt"), pre_dir.join("1")).unwrap();
                fs::rename(host_w.join("a.txt"), host_w.join(&tmp2)).unwrap();
                fs::rename(stage_dir.join("2"), host_w.join("a.txt")).unwrap();
                CallRecord::Pending {
                    invocation_id: inv_id.clone(),
                    tool: "act".to_owned(),
                    catalog_digest: digest.clone(),
                    labels: Labels::new(),
                    writes_possible: true,
                    publish: Some(PublishJournal::Undo {
                        call_hash: call_hash.clone(),
                        roots: roots.clone(),
                        ops,
                        paths,
                        rollback_reason: None,
                    }),
                }
            }
            Window::UndoMidBatch => {
                // op1、op2、op3 已落地；op4 尚在 stage/4。
                fs::rename(host_w.join("del.txt"), pre_dir.join("1")).unwrap();
                fs::rename(host_w.join("a.txt"), pre_dir.join("2")).unwrap();
                fs::rename(stage_dir.join("2"), host_w.join("a.txt")).unwrap();
                fs::rename(stage_dir.join("3"), host_w.join("add.txt")).unwrap();
                CallRecord::Pending {
                    invocation_id: inv_id.clone(),
                    tool: "act".to_owned(),
                    catalog_digest: digest.clone(),
                    labels: Labels::new(),
                    writes_possible: true,
                    publish: Some(PublishJournal::Undo {
                        call_hash: call_hash.clone(),
                        roots: roots.clone(),
                        ops,
                        paths,
                        rollback_reason: None,
                    }),
                }
            }
            Window::UndoAfterLastOpBeforeFinish => {
                // 全部 4 项已落地到宿主，但尚未写 Finish。
                fs::rename(host_w.join("del.txt"), pre_dir.join("1")).unwrap();
                fs::rename(host_w.join("a.txt"), pre_dir.join("2")).unwrap();
                fs::rename(stage_dir.join("2"), host_w.join("a.txt")).unwrap();
                fs::rename(stage_dir.join("3"), host_w.join("add.txt")).unwrap();
                fs::rename(host_w.join("b.txt"), pre_dir.join("4")).unwrap();
                fs::rename(stage_dir.join("4"), host_w.join("b.txt")).unwrap();
                CallRecord::Pending {
                    invocation_id: inv_id.clone(),
                    tool: "act".to_owned(),
                    catalog_digest: digest.clone(),
                    labels: Labels::new(),
                    writes_possible: true,
                    publish: Some(PublishJournal::Undo {
                        call_hash: call_hash.clone(),
                        roots: roots.clone(),
                        ops,
                        paths,
                        rollback_reason: None,
                    }),
                }
            }
            Window::FinishCommitted => {
                // 还原真实顺序：宿主全部落地、原宿主对象仍在 pre/ 中等待基准推进后统一清理。
                fs::rename(host_w.join("del.txt"), pre_dir.join("1")).unwrap();
                fs::rename(host_w.join("a.txt"), pre_dir.join("2")).unwrap();
                fs::rename(stage_dir.join("2"), host_w.join("a.txt")).unwrap();
                fs::rename(stage_dir.join("3"), host_w.join("add.txt")).unwrap();
                fs::rename(host_w.join("b.txt"), pre_dir.join("4")).unwrap();
                fs::rename(stage_dir.join("4"), host_w.join("b.txt")).unwrap();
                CallRecord::Pending {
                    invocation_id: inv_id.clone(),
                    tool: "act".to_owned(),
                    catalog_digest: digest.clone(),
                    labels: Labels::new(),
                    writes_possible: true,
                    publish: Some(PublishJournal::Finish {
                        call_hash: call_hash.clone(),
                        roots: roots.clone(),
                        paths,
                        outcome: PublishFinish::Committed,
                    }),
                }
            }
            Window::FinishRolledBack => CallRecord::Pending {
                invocation_id: inv_id.clone(),
                tool: "act".to_owned(),
                catalog_digest: digest.clone(),
                labels: Labels::new(),
                writes_possible: true,
                publish: Some(PublishJournal::Finish {
                    call_hash: call_hash.clone(),
                    roots: roots.clone(),
                    paths,
                    outcome: PublishFinish::RolledBack,
                }),
            },
            Window::FinishIndeterminate => {
                // a.txt 保留了宿主修改，其余均已撤回。
                fs::write(host_w.join("a.txt"), "host-kept-a\n").unwrap();
                CallRecord::Pending {
                    invocation_id: inv_id.clone(),
                    tool: "act".to_owned(),
                    catalog_digest: digest.clone(),
                    labels: Labels::new(),
                    writes_possible: true,
                    publish: Some(PublishJournal::Finish {
                        call_hash: call_hash.clone(),
                        roots: roots.clone(),
                        paths,
                        outcome: PublishFinish::Indeterminate {
                            dispositions: vec![
                                PathDispositionEntry {
                                    root_idx: 0,
                                    rel: "a.txt".to_owned(),
                                    disposition: PathDisposition::KeptHostEdit,
                                },
                                PathDispositionEntry {
                                    root_idx: 0,
                                    rel: "add.txt".to_owned(),
                                    disposition: PathDisposition::RolledBack,
                                },
                                PathDispositionEntry {
                                    root_idx: 0,
                                    rel: "b.txt".to_owned(),
                                    disposition: PathDisposition::RolledBack,
                                },
                                PathDispositionEntry {
                                    root_idx: 0,
                                    rel: "del.txt".to_owned(),
                                    disposition: PathDisposition::RolledBack,
                                },
                            ],
                        },
                    }),
                }
            }
            Window::HalfUpdatedBase => {
                // D4：与真实产品推进顺序完全一致——
                // pre/ 仍保留归档，Pass 1 已删 base/del.txt，Pass 2 已把 publish/<hash>/base/1 改名至 base/a.txt，
                // 而 publish/<hash>/base/{2,3} 尚未改名进 base/ 时崩溃。
                fs::rename(host_w.join("del.txt"), pre_dir.join("1")).unwrap();
                fs::rename(host_w.join("a.txt"), pre_dir.join("2")).unwrap();
                fs::rename(stage_dir.join("2"), host_w.join("a.txt")).unwrap();
                fs::rename(stage_dir.join("3"), host_w.join("add.txt")).unwrap();
                fs::rename(host_w.join("b.txt"), pre_dir.join("4")).unwrap();
                fs::rename(stage_dir.join("4"), host_w.join("b.txt")).unwrap();
                fs::remove_file(base_w.join("del.txt")).unwrap();
                fs::rename(base_stage_dir.join("1"), base_w.join("a.txt")).unwrap();
                CallRecord::Pending {
                    invocation_id: inv_id.clone(),
                    tool: "act".to_owned(),
                    catalog_digest: digest.clone(),
                    labels: Labels::new(),
                    writes_possible: true,
                    publish: Some(PublishJournal::Finish {
                        call_hash: call_hash.clone(),
                        roots: roots.clone(),
                        paths,
                        outcome: PublishFinish::Committed,
                    }),
                }
            }
            Window::FinishAfterCleanupBeforeSettled => {
                // 全部基准已推进、publish/<hash> 已清理，在写入 Settled 前瞬间崩溃。
                fs::remove_file(host_w.join("del.txt")).unwrap();
                fs::rename(stage_dir.join("2"), host_w.join("a.txt")).unwrap();
                fs::rename(stage_dir.join("3"), host_w.join("add.txt")).unwrap();
                fs::rename(stage_dir.join("4"), host_w.join("b.txt")).unwrap();
                fs::remove_file(base_w.join("del.txt")).unwrap();
                fs::rename(base_stage_dir.join("1"), base_w.join("a.txt")).unwrap();
                fs::rename(base_stage_dir.join("2"), base_w.join("add.txt")).unwrap();
                fs::rename(base_stage_dir.join("3"), base_w.join("b.txt")).unwrap();
                fs::remove_dir_all(&call_dir).unwrap();
                CallRecord::Pending {
                    invocation_id: inv_id.clone(),
                    tool: "act".to_owned(),
                    catalog_digest: digest.clone(),
                    labels: Labels::new(),
                    writes_possible: true,
                    publish: Some(PublishJournal::Finish {
                        call_hash: call_hash.clone(),
                        roots: roots.clone(),
                        paths,
                        outcome: PublishFinish::Committed,
                    }),
                }
            }
            Window::SettledBeforeReply => {
                let settled_receipt = Receipt {
                    invocation_id: inv_id.clone(),
                    tool: "act".to_owned(),
                    catalog_digest: digest.clone(),
                    labels: Labels::new(),
                    outcome: ReceiptOutcome::Succeeded {
                        content: json!({"value": 7, "printed": ""}),
                    },
                    steps: vec![StepSummary {
                        tool: "commit".to_owned(),
                        outcome: OutcomeKind::Succeeded,
                    }],
                    effects: Vec::new(),
                    truncated: false,
                };
                CallRecord::Settled {
                    settled_at_ms: 9_999_999_999_999,
                    receipt: settled_receipt,
                }
            }
            Window::KilledMidRecovery => {
                // 模拟恢复撤回中途再被杀：op4 已放回 stage/4，op3 已撤回至 pre/3，
                // op2 刚把 pre/2 换回 host/a.txt 且 staged 留在 host_tmp（未回 stage/2），op1 仍在 pre/1。
                fs::rename(host_w.join("del.txt"), pre_dir.join("1")).unwrap();
                fs::rename(stage_dir.join("2"), host_w.join(&tmp2)).unwrap();
                fs::rename(stage_dir.join("3"), pre_dir.join("3")).unwrap();
                CallRecord::Pending {
                    invocation_id: inv_id.clone(),
                    tool: "act".to_owned(),
                    catalog_digest: digest.clone(),
                    labels: Labels::new(),
                    writes_possible: true,
                    publish: Some(PublishJournal::Undo {
                        call_hash: call_hash.clone(),
                        roots: roots.clone(),
                        ops,
                        paths,
                        rollback_reason: Some("模拟恢复重入".to_owned()),
                    }),
                }
            }
            Window::UndoWithHostEditPartial => {
                // 全部 4 项已落地，但宿主随后修改了 a.txt -> 恢复撤回其余 3 项，保留 a.txt。
                fs::rename(host_w.join("del.txt"), pre_dir.join("1")).unwrap();
                fs::rename(host_w.join("a.txt"), pre_dir.join("2")).unwrap();
                fs::rename(stage_dir.join("2"), host_w.join("a.txt")).unwrap();
                fs::rename(stage_dir.join("3"), host_w.join("add.txt")).unwrap();
                fs::rename(host_w.join("b.txt"), pre_dir.join("4")).unwrap();
                fs::rename(stage_dir.join("4"), host_w.join("b.txt")).unwrap();
                fs::write(host_w.join("a.txt"), "host-edited-after-pub\n").unwrap();
                CallRecord::Pending {
                    invocation_id: inv_id.clone(),
                    tool: "act".to_owned(),
                    catalog_digest: digest.clone(),
                    labels: Labels::new(),
                    writes_possible: true,
                    publish: Some(PublishJournal::Undo {
                        call_hash: call_hash.clone(),
                        roots: roots.clone(),
                        ops,
                        paths,
                        rollback_reason: None,
                    }),
                }
            }
        };

        write_call_record(&state_dir, &inv_id, &record);

        // 重启节点；在首个 hello 之前检查磁盘记录已结算且发布事务目录已清理。
        let toml_content = fs::read_to_string(&config_path).unwrap();
        let restarted = NodeProcess::start(&config_path, &toml_content, &state_dir, &home);

        let disk_after = read_call_record(&state_dir, &inv_id)
            .unwrap_or_else(|| panic!("窗口 `{label}` 重启后记录不应缺失"));
        assert!(
            matches!(disk_after, CallRecord::Settled { .. }),
            "窗口 `{label}` 首个 hello 之前必须已结算，实际：{disk_after:?}"
        );
        assert!(
            !call_dir.exists(),
            "窗口 `{label}` 恢复后应已清理发布目录 {}",
            call_dir.display()
        );

        let (mut s2, digest2) = connect_ready(&restarted);
        assert_eq!(digest2, digest);

        // 校验零重放。
        let replay = send_request(
            &mut s2,
            10,
            exec_req(&inv_id, &digest2, "return 1", 5_000, 65_536),
        );
        assert!(
            matches!(
                replay.message,
                Response::Rejected {
                    code: RejectCode::DuplicateInvocation,
                    ..
                }
            ),
            "窗口 `{label}` 原 id 必须拒绝重放：{replay:?}"
        );

        let receipt = match query_receipt(&mut s2, 11, &inv_id) {
            ReceiptLookup::Settled { receipt } => receipt,
            other => panic!("窗口 `{label}` 期望 Settled，实际：{other:?}"),
        };

        match window {
            Window::NoLogWithStaging => {
                match &receipt.outcome {
                    ReceiptOutcome::Indeterminate { reason } => {
                        assert!(reason.contains("发布未开始"), "{label}: {reason}");
                    }
                    other => panic!("{label}: 期望 Indeterminate，实际：{other:?}"),
                }
                assert!(receipt.effects.is_empty());
                assert_eq!(
                    fs::read_to_string(host_w.join("a.txt")).unwrap(),
                    "orig-a\n"
                );
                assert_eq!(
                    fs::read_to_string(host_w.join("b.txt")).unwrap(),
                    "orig-b\n"
                );
                assert_eq!(
                    fs::read_to_string(host_w.join("del.txt")).unwrap(),
                    "orig-del\n"
                );
                assert!(!host_w.join("add.txt").exists());
            }
            Window::UndoBeforeFirstOp
            | Window::UndoExchangeCrashPoint1
            | Window::UndoExchangeCrashPoint2
            | Window::UndoMidBatch
            | Window::UndoAfterLastOpBeforeFinish
            | Window::FinishRolledBack
            | Window::KilledMidRecovery => {
                match &receipt.outcome {
                    ReceiptOutcome::Indeterminate { reason } => {
                        assert!(reason.contains("发布已撤回"), "{label}: {reason}");
                    }
                    other => panic!("{label}: 期望 Indeterminate，实际：{other:?}"),
                }
                assert!(receipt.effects.is_empty(), "{label}: 撤回不应有发布 effect");
                assert_eq!(
                    fs::read_to_string(host_w.join("a.txt")).unwrap(),
                    "orig-a\n"
                );
                assert_eq!(
                    fs::read_to_string(host_w.join("b.txt")).unwrap(),
                    "orig-b\n"
                );
                assert_eq!(
                    fs::read_to_string(host_w.join("del.txt")).unwrap(),
                    "orig-del\n"
                );
                assert!(!host_w.join("add.txt").exists());
                assert!(!host_w.join(&tmp2).exists());
                assert!(!host_w.join(&tmp4).exists());
                // 草稿仍保留在影子中。
                let st_check = run_act(&mut s2, 12, "inv-st", &digest2, "return len(w.status())");
                match st_check {
                    ExecuteOutcome::Succeeded { content, .. } => {
                        assert_eq!(content["value"], json!(4), "{label}: 草稿应保留");
                    }
                    other => panic!("{label}: 查询 status 失败：{other:?}"),
                }
            }
            Window::FinishCommitted
            | Window::HalfUpdatedBase
            | Window::FinishAfterCleanupBeforeSettled => {
                match &receipt.outcome {
                    ReceiptOutcome::Indeterminate { reason } => {
                        assert!(reason.contains("发布已完成"), "{label}: {reason}");
                    }
                    other => panic!("{label}: 期望 Indeterminate，实际：{other:?}"),
                }
                assert_eq!(receipt.effects.len(), 1);
                let changes = receipt.effects[0].changes();
                assert_eq!(changes.len(), 4);
                assert!(changes.iter().all(|c| {
                    c.action == "publish" && c.diff.is_none() && c.residue == Residue::NotRetained
                }));
                assert_eq!(fs::read_to_string(host_w.join("a.txt")).unwrap(), "pub-a\n");
                assert_eq!(fs::read_to_string(host_w.join("b.txt")).unwrap(), "pub-b\n");
                assert_eq!(
                    fs::read_to_string(host_w.join("add.txt")).unwrap(),
                    "pub-add\n"
                );
                assert!(!host_w.join("del.txt").exists());
                assert_eq!(file_id(&base_w.join("a.txt")), sb_a.id);
                assert_eq!(file_id(&base_w.join("add.txt")), sb_add.id);
                assert_eq!(file_id(&base_w.join("b.txt")), sb_b.id);
                assert!(!base_w.join("del.txt").exists());
                let st_check = run_act(&mut s2, 12, "inv-st", &digest2, "return w.status()");
                match st_check {
                    ExecuteOutcome::Succeeded { content, .. } => {
                        assert_eq!(
                            content["value"],
                            json!([]),
                            "{label}: base 推进后 status 应为空"
                        );
                    }
                    other => panic!("{label}: 查询 status 失败：{other:?}"),
                }
            }
            Window::FinishIndeterminate | Window::UndoWithHostEditPartial => {
                match &receipt.outcome {
                    ReceiptOutcome::Indeterminate { reason } => {
                        assert!(
                            reason.contains("部分保留宿主修改")
                                && reason.contains("w \"a.txt\"：保留宿主修改"),
                            "{label}: {reason}"
                        );
                    }
                    other => panic!("{label}: 期望 Indeterminate，实际：{other:?}"),
                }
                assert_eq!(receipt.effects.len(), 1);
                let changes = receipt.effects[0].changes();
                assert_eq!(changes.len(), 1);
                assert_eq!(changes[0].target, "a.txt");
                assert_eq!(changes[0].action, "publish");
                assert!(changes[0].diff.is_none());
                assert_eq!(changes[0].residue, Residue::NotRetained);
                assert_eq!(
                    fs::read_to_string(host_w.join("b.txt")).unwrap(),
                    "orig-b\n"
                );
                assert_eq!(
                    fs::read_to_string(host_w.join("del.txt")).unwrap(),
                    "orig-del\n"
                );
                assert!(!host_w.join("add.txt").exists());
            }
            Window::SettledBeforeReply => {
                assert_eq!(
                    receipt.outcome,
                    ReceiptOutcome::Succeeded {
                        content: json!({"value": 7, "printed": ""}),
                    },
                    "{label}: 已结算记录必须保持原样"
                );
            }
        }
    }

    // 额外校验：日志记录的宿主根身份 (shadow_key / dev / ino) 与当前配置不匹配时，启动必须失败并保护现场。
    {
        let (root, home, config_path, state_dir, process) =
            start_node_with(Duration::from_secs(10), |root| {
                let w = root.join("w");
                fs::create_dir(&w).unwrap();
                fs::write(w.join("a.txt"), "keep-me\n").unwrap();
                format!(
                    "[[dir]]\nname = \"w\"\npath = {:?}\naccess = \"write\"\n",
                    w.to_str().unwrap()
                )
            });
        let host_w = root.path().join("w");
        let (stream, digest) = connect_ready(&process);
        drop(stream);
        drop(process);

        let inv_id = "inv-mismatch-root";
        let call_hash = call_record_hash(inv_id);
        let mismatch_record = CallRecord::Pending {
            invocation_id: inv_id.to_owned(),
            tool: "act".to_owned(),
            catalog_digest: digest,
            labels: Labels::new(),
            writes_possible: true,
            publish: Some(PublishJournal::Undo {
                call_hash,
                roots: vec![JournalRoot {
                    handle: "w".to_owned(),
                    shadow_key: "deadbeef".to_owned(),
                    host_dev: 999_999,
                    host_ino: 888_888,
                }],
                ops: Vec::new(),
                paths: Vec::new(),
                rollback_reason: None,
            }),
        };
        let rec_path = write_call_record(&state_dir, inv_id, &mismatch_record);
        let toml_content = fs::read_to_string(&config_path).unwrap();
        let stderr = run_expect_failure(&config_path, &toml_content, &home);
        assert!(
            stderr.contains(inv_id) && stderr.contains(rec_path.to_str().unwrap()),
            "宿主根身份不匹配时报错应指明 invocation_id 与日志路径，实际：\n{stderr}"
        );
        assert_eq!(
            fs::read_to_string(host_w.join("a.txt")).unwrap(),
            "keep-me\n"
        );
    }

    // 额外校验：只读目录（0555）经阶段 1 放宽、阶段 4 替换子文件、阶段 5 恢复 0555 后崩溃在 Finish 前，
    // 重启恢复必须先放宽、撤回子文件、再精确恢复 0555 权限。
    {
        let (root, home, config_path, state_dir, process) =
            start_node_with(Duration::from_secs(10), |root| {
                let w = root.join("w");
                fs::create_dir_all(w.join("ro")).unwrap();
                fs::write(w.join("ro/x.txt"), "ro-orig\n").unwrap();
                fs::set_permissions(w.join("ro"), fs::Permissions::from_mode(0o555)).unwrap();
                format!(
                    "[[dir]]\nname = \"w\"\npath = {:?}\naccess = \"write\"\n",
                    w.to_str().unwrap()
                )
            });
        let host_w = root.path().join("w");
        let canonical_w = fs::canonicalize(&host_w).unwrap();
        let shadow_key = compute_shadow_key(&canonical_w, &[], &[]);
        let host_root_id = file_id(&canonical_w);
        let ro_id = file_id(&host_w.join("ro"));

        let (stream, digest) = connect_ready(&process);
        drop(stream);
        drop(process);

        let inv_id = "inv-rec-ro-dir";
        let call_hash = call_record_hash(inv_id);
        let call_dir = state_dir.join("publish").join(&call_hash);
        let stage_dir = call_dir.join("stage");
        let pre_dir = call_dir.join("pre");
        let base_stage_dir = call_dir.join("base");
        fs::create_dir_all(&stage_dir).unwrap();
        fs::create_dir_all(&pre_dir).unwrap();
        fs::create_dir_all(&base_stage_dir).unwrap();

        fs::write(base_stage_dir.join("1"), b"ro-pub\n").unwrap();
        fs::set_permissions(base_stage_dir.join("1"), fs::Permissions::from_mode(0o644)).unwrap();
        let sb_ro = StagedBase {
            slot: 1,
            id: file_id(&base_stage_dir.join("1")),
        };

        fs::set_permissions(host_w.join("ro"), fs::Permissions::from_mode(0o755)).unwrap();
        fs::write(stage_dir.join("2"), b"ro-pub\n").unwrap();
        fs::set_permissions(stage_dir.join("2"), fs::Permissions::from_mode(0o644)).unwrap();
        let staged_id = file_id(&stage_dir.join("2"));
        fs::rename(host_w.join("ro/x.txt"), pre_dir.join("2")).unwrap();
        fs::rename(stage_dir.join("2"), host_w.join("ro/x.txt")).unwrap();
        fs::set_permissions(host_w.join("ro"), fs::Permissions::from_mode(0o555)).unwrap();

        let rec = CallRecord::Pending {
            invocation_id: inv_id.to_owned(),
            tool: "act".to_owned(),
            catalog_digest: digest,
            labels: Labels::new(),
            writes_possible: true,
            publish: Some(PublishJournal::Undo {
                call_hash: call_hash.clone(),
                roots: vec![JournalRoot {
                    handle: "w".to_owned(),
                    shadow_key,
                    host_dev: host_root_id.dev,
                    host_ino: host_root_id.ino,
                }],
                ops: vec![
                    JournalOp::ChmodDir {
                        op_id: 1,
                        root_idx: 0,
                        rel: "ro".to_owned(),
                        before_mode: 0o555,
                        after_mode: 0o755,
                        original_mode: None,
                        target_id: ro_id,
                    },
                    JournalOp::Exchange {
                        op_id: 2,
                        root_idx: 0,
                        rel: "ro/x.txt".to_owned(),
                        staged_id,
                        host_tmp_name: format!(".lancet-pub-{}-2", &call_hash[..16]),
                        before: ObjectState::File {
                            mode: 0o644,
                            size: 8,
                            sha256: sha256_bytes(b"ro-orig\n"),
                        },
                        after: ObjectState::File {
                            mode: 0o644,
                            size: 7,
                            sha256: sha256_bytes(b"ro-pub\n"),
                        },
                    },
                    JournalOp::ChmodDir {
                        op_id: 3,
                        root_idx: 0,
                        rel: "ro".to_owned(),
                        before_mode: 0o755,
                        after_mode: 0o555,
                        original_mode: Some(0o555),
                        target_id: ro_id,
                    },
                ],
                paths: vec![BatchPath {
                    root_idx: 0,
                    rel: "ro/x.txt".to_owned(),
                    before: Some(ObjectState::File {
                        mode: 0o644,
                        size: 8,
                        sha256: sha256_bytes(b"ro-orig\n"),
                    }),
                    after: Some(ObjectState::File {
                        mode: 0o644,
                        size: 7,
                        sha256: sha256_bytes(b"ro-pub\n"),
                    }),
                    staged_base: Some(sb_ro),
                }],
                rollback_reason: None,
            }),
        };
        write_call_record(&state_dir, inv_id, &rec);

        let toml_content = fs::read_to_string(&config_path).unwrap();
        let restarted = NodeProcess::start(&config_path, &toml_content, &state_dir, &home);
        assert!(matches!(
            read_call_record(&state_dir, inv_id),
            Some(CallRecord::Settled { .. })
        ));
        assert_eq!(
            fs::read_to_string(host_w.join("ro/x.txt")).unwrap(),
            "ro-orig\n",
            "只读目录内已替换的子文件必须撤回为原内容"
        );
        assert_eq!(
            fs::symlink_metadata(host_w.join("ro")).unwrap().mode() & 0o7777,
            0o555,
            "只读目录撤回后必须精确恢复 0555 权限"
        );
        drop(restarted);
        let _ = fs::set_permissions(host_w.join("ro"), fs::Permissions::from_mode(0o700));
    }
}

// =============================================================================
// 9. publish_survives_random_kill（L.2：真实 SIGKILL 抽样）
// =============================================================================

#[test]
fn publish_survives_random_kill() {
    let file_count = 450usize;
    let samples = 16usize;

    let (root, home, config_path, state_dir, process) =
        start_node_with(Duration::from_secs(15), |root| {
            let w = root.join("w");
            fs::create_dir(&w).unwrap();
            let body = "A".repeat(1024);
            for i in 0..file_count {
                fs::write(w.join(format!("f{i:04}.txt")), &body).unwrap();
            }
            format!(
                "[[dir]]\nname = \"w\"\npath = {:?}\naccess = \"write\"\n",
                w.to_str().unwrap()
            )
        });
    let host_w = root.path().join("w");
    let canonical_w = fs::canonicalize(&host_w).unwrap();
    let shadow_key = compute_shadow_key(&canonical_w, &[], &[]);
    let tree_w = state_dir.join("shadows").join(&shadow_key).join("tree");
    let toml_content = fs::read_to_string(&config_path).unwrap();

    // 标定正常发布耗时。
    let (mut stream, digest) = connect_ready(&process);
    let mut cal_durations = Vec::new();
    for c in 0..2usize {
        let payload = format!("cal-{c}-").repeat(128);
        for i in 0..file_count {
            fs::write(tree_w.join(format!("f{i:04}.txt")), &payload).unwrap();
        }
        let _ = run_act(
            &mut stream,
            10 + c as u64 * 2,
            &format!("cal-sync-{c}"),
            &digest,
            "return len(w.status())",
        );
        let t0 = Instant::now();
        let r = run_act(
            &mut stream,
            11 + c as u64 * 2,
            &format!("cal-pub-{c}"),
            &digest,
            "commit()",
        );
        cal_durations.push(t0.elapsed());
        assert!(matches!(r, ExecuteOutcome::Succeeded { .. }));
    }
    let avg_ms = (cal_durations.iter().map(Duration::as_micros).sum::<u128>()
        / cal_durations.len() as u128)
        .max(2_000);
    drop(stream);

    let mut current_proc = process;
    let mut stage_hits: BTreeMap<&'static str, usize> = BTreeMap::new();
    let mut all_before_hits = 0usize;
    let mut all_after_hits = 0usize;

    // 确定性混合时延分布，覆盖 [0, 1.1 * avg] 区间。
    let mut rng = 0x51a7_9b23_u64;
    for s in 0..samples {
        let (mut stream, digest) = connect_ready(&current_proc);
        let host_before = scan_dir_files(&host_w);

        let payload = format!("sample-{s:02}-").repeat(100);
        for i in 0..file_count {
            fs::write(tree_w.join(format!("f{i:04}.txt")), &payload).unwrap();
        }
        let expected_after = scan_dir_files(&tree_w);
        assert_ne!(host_before, expected_after);

        let sync_res = run_act(
            &mut stream,
            100,
            &format!("sync-sample-{s}"),
            &digest,
            "return len(w.status())",
        );
        match sync_res {
            ExecuteOutcome::Succeeded { content, .. } => {
                assert_eq!(content["value"], json!(file_count));
            }
            other => panic!("样本 {s} 预核对失败：{other:?}"),
        }

        let inv_id = format!("kill-sample-{s}");
        let call_hash = call_record_hash(&inv_id);
        write_frame(
            &mut stream,
            &Frame {
                request_id: 101,
                message: exec_req(&inv_id, &digest, "commit()", 30_000, 65_536),
            },
        )
        .unwrap();

        rng = rng.wrapping_mul(6364136223846793005).wrapping_add(1);
        let frac = ((s * 7 + (rng as usize % 5)) % (samples + 2)) as u128;
        let delay_us = (avg_ms * 11 * frac) / (10 * (samples as u128 + 1));
        if delay_us > 0 {
            thread::sleep(Duration::from_micros(delay_us as u64));
        }
        // SIGKILL 杀掉节点子进程并回收，保持客户端连接不先关闭。
        drop(current_proc);
        drop(stream);

        let stage = match read_call_record(&state_dir, &inv_id) {
            None => "BeforeAdmit",
            Some(CallRecord::Pending { publish: None, .. }) => {
                if state_dir.join("publish").join(&call_hash).exists() {
                    "Pending(staging)"
                } else {
                    "Pending(no-publish)"
                }
            }
            Some(CallRecord::Pending {
                publish: Some(PublishJournal::Undo { .. }),
                ..
            }) => "Undo",
            Some(CallRecord::Pending {
                publish: Some(PublishJournal::Finish { .. }),
                ..
            }) => "Finish",
            Some(CallRecord::Settled { .. }) => "Settled",
            Some(_) => "Other",
        };
        *stage_hits.entry(stage).or_insert(0) += 1;

        current_proc = NodeProcess::start(&config_path, &toml_content, &state_dir, &home);

        // 首个 hello 之前：若已受理，记录必须已 Settled，且发布事务目录已清理。
        if stage != "BeforeAdmit" {
            let after_rec = read_call_record(&state_dir, &inv_id).expect("已受理调用应留有记录");
            assert!(
                matches!(after_rec, CallRecord::Settled { .. }),
                "样本 {s}（stage={stage}）重启后首个 hello 之前应已结算，实际：{after_rec:?}"
            );
        }
        assert!(
            !state_dir.join("publish").join(&call_hash).exists(),
            "样本 {s} 恢复后不得残留发布目录"
        );

        let host_after = scan_dir_files(&host_w);
        if host_after == host_before {
            all_before_hits += 1;
        } else if host_after == expected_after {
            all_after_hits += 1;
        } else {
            panic!("样本 {s}（stage={stage}）重启后宿主处于部分发布状态！");
        }

        let (mut s_check, d_check) = connect_ready(&current_proc);
        if stage != "BeforeAdmit" {
            let replay = send_request(
                &mut s_check,
                200,
                exec_req(&inv_id, &d_check, "return 1", 5_000, 65_536),
            );
            assert!(matches!(
                replay.message,
                Response::Rejected {
                    code: RejectCode::DuplicateInvocation,
                    ..
                }
            ));
        }
    }

    eprintln!(
        "publish_survives_random_kill: samples={samples}, cal_avg_us={avg_ms}, all_before={all_before_hits}, all_after={all_after_hits}, stages={stage_hits:?}"
    );

    // 确定性 Undo 窗口样本（D1）：观测到首项已交换、末项尚未交换时立即 SIGKILL，
    // 确认确实命中 Undo 阶段且重启后整批撤回至前态。
    let mut deterministic_undo_hit = None;
    for attempt in 1..=10usize {
        let (mut stream, digest) = connect_ready(&current_proc);
        let host_before = scan_dir_files(&host_w);
        let payload = format!("det-undo-{attempt:02}-").repeat(100);
        for i in 0..file_count {
            fs::write(tree_w.join(format!("f{i:04}.txt")), &payload).unwrap();
        }
        let sync_res = run_act(
            &mut stream,
            300,
            &format!("sync-det-undo-{attempt}"),
            &digest,
            "return len(w.status())",
        );
        match sync_res {
            ExecuteOutcome::Succeeded { content, .. } => {
                assert_eq!(content["value"], json!(file_count));
            }
            other => panic!("确定性 Undo 轮次 {attempt} 预核对失败：{other:?}"),
        }

        let first_path = host_w.join("f0000.txt");
        let last_path = host_w.join(format!("f{:04}.txt", file_count - 1));
        let last_before = fs::read_to_string(&last_path).unwrap();
        let inv_id = format!("kill-det-undo-{attempt}");
        let call_hash = call_record_hash(&inv_id);
        write_frame(
            &mut stream,
            &Frame {
                request_id: 301,
                message: exec_req(&inv_id, &digest, "commit()", 30_000, 65_536),
            },
        )
        .unwrap();

        let wait_dl = Instant::now() + Duration::from_secs(15);
        let mut saw_mid = false;
        while Instant::now() < wait_dl {
            if fs::read_to_string(&first_path).ok().as_deref() == Some(payload.as_str()) {
                saw_mid =
                    fs::read_to_string(&last_path).ok().as_deref() == Some(last_before.as_str());
                // 观测到首项已交换后立即 SIGKILL 节点进程。
                drop(current_proc);
                drop(stream);
                break;
            }
            thread::yield_now();
        }
        if !saw_mid {
            // 若在观测时末项已同步完成，重启并进入下一轮重试。
            current_proc = NodeProcess::start(&config_path, &toml_content, &state_dir, &home);
            continue;
        }

        let rec_before_restart = read_call_record(&state_dir, &inv_id);
        let is_undo = matches!(
            rec_before_restart,
            Some(CallRecord::Pending {
                publish: Some(PublishJournal::Undo { .. }),
                ..
            })
        );
        current_proc = NodeProcess::start(&config_path, &toml_content, &state_dir, &home);
        assert!(
            !state_dir.join("publish").join(&call_hash).exists(),
            "确定性 Undo 恢复后不得残留发布目录"
        );
        let after_rec = read_call_record(&state_dir, &inv_id).expect("已受理调用应留有记录");
        assert!(
            matches!(after_rec, CallRecord::Settled { .. }),
            "确定性 Undo 重启后首个 hello 之前应已结算：{after_rec:?}"
        );

        if is_undo {
            assert_eq!(
                scan_dir_files(&host_w),
                host_before,
                "命中 Undo 窗口重启后宿主必须完整撤回至前态"
            );
            let (mut s_check, d_check) = connect_ready(&current_proc);
            let replay = send_request(
                &mut s_check,
                302,
                exec_req(&inv_id, &d_check, "return 1", 5_000, 65_536),
            );
            assert!(matches!(
                replay.message,
                Response::Rejected {
                    code: RejectCode::DuplicateInvocation,
                    ..
                }
            ));
            deterministic_undo_hit = Some(attempt);
            break;
        }
    }
    let det_attempt = deterministic_undo_hit.expect("在限定次数内未能命中确定性 Undo 杀进程窗口");
    eprintln!("publish_survives_random_kill: deterministic_undo_hit on attempt {det_attempt}/10");
}

// =============================================================================
// 10. cancel_does_not_interrupt_started_publish
// =============================================================================

#[test]
fn cancel_does_not_interrupt_started_publish() {
    let file_count = 800usize;
    let (root, _home, _cfg, state_dir, process) =
        start_node_with(Duration::from_secs(20), |root| {
            let w = root.join("w");
            fs::create_dir(&w).unwrap();
            for i in 0..file_count {
                fs::write(w.join(format!("f{i:04}.txt")), format!("v0-{i}\n")).unwrap();
            }
            format!(
                "[[dir]]\nname = \"w\"\npath = {:?}\naccess = \"write\"\n",
                w.to_str().unwrap()
            )
        });
    let host_w = root.path().join("w");
    let canonical_w = fs::canonicalize(&host_w).unwrap();
    let tree_w = state_dir
        .join("shadows")
        .join(compute_shadow_key(&canonical_w, &[], &[]))
        .join("tree");

    // 1. 显式 Cancel：等待观测到首项已发布且末项未完成时发 Cancel ->
    //    已观测到发布的样本严格断言为 Succeeded 且全后态一致。
    {
        let (mut s_exec, digest) = connect_ready(&process);
        let (mut s_ctl, _) = connect_ready(&process);
        let mut observed_mid = false;
        for attempt in 1..=6usize {
            let ver = format!("v1-{attempt}-");
            for i in 0..file_count {
                fs::write(tree_w.join(format!("f{i:04}.txt")), format!("{ver}{i}\n")).unwrap();
            }
            let expected_after = scan_dir_files(&tree_w);
            let _ = run_act(
                &mut s_exec,
                10 + attempt as u64 * 10,
                &format!("inv-sync-cancel-{attempt}"),
                &digest,
                "return len(w.status())",
            );

            let first_path = host_w.join("f0000.txt");
            let last_path = host_w.join(format!("f{:04}.txt", file_count - 1));
            let expected_first = format!("{ver}0\n");
            let expected_last = format!("{ver}{}\n", file_count - 1);
            let inv_id = format!("inv-cancel-pub-{attempt}");
            write_frame(
                &mut s_exec,
                &Frame {
                    request_id: 11 + attempt as u64 * 10,
                    message: exec_req(&inv_id, &digest, "commit()", 30_000, 65_536),
                },
            )
            .unwrap();

            let wait_dl = Instant::now() + Duration::from_secs(15);
            let mut saw_first = false;
            let mut saw_last_still_old = false;
            while Instant::now() < wait_dl {
                if fs::read_to_string(&first_path).ok().as_deref() == Some(expected_first.as_str())
                {
                    saw_first = true;
                    saw_last_still_old = fs::read_to_string(&last_path).ok().as_deref()
                        != Some(expected_last.as_str());
                    break;
                }
                thread::yield_now();
            }
            assert!(saw_first, "等待首项发布超时");

            let c_resp = send_request(
                &mut s_ctl,
                20 + attempt as u64 * 10,
                Request::Cancel {
                    invocation_id: inv_id.clone(),
                },
            );
            assert!(matches!(c_resp.message, Response::CancelOk));

            let resp: Frame<Response> = read_frame(&mut s_exec).unwrap().unwrap();
            // 已经观测到首项进入宿主：无论如何必须 Succeeded 并全批发布，绝不接受 Cancelled！
            assert!(
                matches!(
                    resp.message,
                    Response::ExecuteResult {
                        outcome: ExecuteOutcome::Succeeded { .. }
                    }
                ),
                "已观测到发布开始，Cancel 不得中断发布，实际：{:?}",
                resp.message
            );
            assert_eq!(
                scan_dir_files(&host_w),
                expected_after,
                "已观测到发布开始后必须全批落到宿主"
            );
            if saw_last_still_old {
                observed_mid = true;
                break;
            }
        }
        assert!(
            observed_mid,
            "未能观测到发布中途（首项已发、末项未发）发送 Cancel"
        );
    }

    // 2. 连接断开：观测到首项已发布且末项未完成时断开连接 ->
    //    用第二条连接查 receipt 对账，严格断言为 Succeeded 且全后态一致。
    {
        let (mut s_ctl, digest) = connect_ready(&process);
        let mut observed_mid = false;
        let mut next_id = 200u64;
        for attempt in 1..=6usize {
            let (mut s_drop, _) = connect_ready(&process);
            let ver = format!("v2-{attempt}-");
            for i in 0..file_count {
                fs::write(tree_w.join(format!("f{i:04}.txt")), format!("{ver}{i}\n")).unwrap();
            }
            let expected_after = scan_dir_files(&tree_w);
            let _ = run_act(
                &mut s_ctl,
                next_id,
                &format!("inv-sync-drop-{attempt}"),
                &digest,
                "return len(w.status())",
            );
            next_id += 1;

            let first_path = host_w.join("f0000.txt");
            let last_path = host_w.join(format!("f{:04}.txt", file_count - 1));
            let expected_first = format!("{ver}0\n");
            let expected_last = format!("{ver}{}\n", file_count - 1);
            let inv_id = format!("inv-drop-pub-{attempt}");
            write_frame(
                &mut s_drop,
                &Frame {
                    request_id: next_id,
                    message: exec_req(&inv_id, &digest, "commit()", 30_000, 65_536),
                },
            )
            .unwrap();
            next_id += 1;

            let wait_dl = Instant::now() + Duration::from_secs(15);
            let mut saw_first = false;
            let mut saw_last_still_old = false;
            while Instant::now() < wait_dl {
                if fs::read_to_string(&first_path).ok().as_deref() == Some(expected_first.as_str())
                {
                    saw_first = true;
                    saw_last_still_old = fs::read_to_string(&last_path).ok().as_deref()
                        != Some(expected_last.as_str());
                    break;
                }
                thread::yield_now();
            }
            assert!(saw_first, "等待首项发布超时");

            let _ = s_drop.shutdown(Shutdown::Both);
            drop(s_drop);

            let rec = wait_settled(
                &mut s_ctl,
                &mut next_id,
                &inv_id,
                Instant::now() + Duration::from_secs(15),
            );
            assert!(
                matches!(rec.outcome, ReceiptOutcome::Succeeded { .. }),
                "已观测到发布开始，断连不得中断发布，实际：{:?}",
                rec.outcome
            );
            assert_eq!(
                scan_dir_files(&host_w),
                expected_after,
                "临界区内断连必须继续完成全部发布"
            );
            if saw_last_still_old {
                observed_mid = true;
                break;
            }
        }
        assert!(
            observed_mid,
            "未能观测到发布中途（首项已发、末项未发）断开连接"
        );
    }
}

// =============================================================================
// 11. publication_receipt_uses_protocol_diff_budget
// =============================================================================

#[test]
fn publication_receipt_uses_protocol_diff_budget() {
    let (root, _home, _cfg, state_dir, process) =
        start_node_with(Duration::from_secs(10), |root| {
            let w = root.join("w");
            fs::create_dir(&w).unwrap();
            fs::write(w.join("small.txt"), "line1\n").unwrap();
            fs::write(w.join("big1.txt"), "old1\n").unwrap();
            fs::write(w.join("big2.txt"), "old2\n").unwrap();
            fs::write(w.join("bin.dat"), b"\x00\x01old").unwrap();
            fs::write(w.join("mode_only.txt"), "same\n").unwrap();
            format!(
                "[[dir]]\nname = \"w\"\npath = {:?}\naccess = \"write\"\n",
                w.to_str().unwrap()
            )
        });
    let host_w = root.path().join("w");
    let canonical_w = fs::canonicalize(&host_w).unwrap();
    let tree_w = state_dir
        .join("shadows")
        .join(compute_shadow_key(&canonical_w, &[], &[]))
        .join("tree");
    let (mut stream, digest) = connect_ready(&process);

    // 1. 小补丁在回执摘要中无损保留。
    let r_small = run_act(
        &mut stream,
        10,
        "inv-diff-small",
        &digest,
        "w.write(\"small.txt\", \"line2\\n\")\ncommit()",
    );
    match r_small {
        ExecuteOutcome::Succeeded {
            receipt_summary, ..
        } => {
            assert!(!receipt_summary.is_truncated());
            let pub_entry = receipt_summary
                .effects()
                .iter()
                .find(|e| {
                    e.changes()
                        .iter()
                        .any(|c| c.residue == Residue::NotRetained)
                })
                .expect("应有发布 effect");
            let c = &pub_entry.changes()[0];
            assert_eq!(c.target, "small.txt");
            assert!(!c.diff_truncated);
            let patch = c.diff.as_deref().expect("小补丁应完整保留");
            assert!(patch.contains("-line1") && patch.contains("+line2"));
        }
        other => panic!("小补丁发布失败：{other:?}"),
    }

    // 2. 大补丁（含多字节 UTF-8 与 JSON 转义字符）+ 非文本/仅改权限项：
    //    summary 裁剪进 16 KiB 且保留全部路径，非文本项 diff_truncated == false，被裁剪/去掉文本项 diff_truncated == true，
    //    receipt 查询取回完整补丁。
    let mut big1 = String::new();
    let mut big2 = String::new();
    for i in 0..400 {
        big1.push_str(&format!("中文\"引号\"\\反斜杠行-{i:03}\n"));
        big2.push_str(&format!("第二文件\"转义\"\\内容-{i:03}\n"));
    }
    fs::write(tree_w.join("big1.txt"), &big1).unwrap();
    fs::write(tree_w.join("big2.txt"), &big2).unwrap();
    fs::write(tree_w.join("bin.dat"), b"\x00\x02new").unwrap();
    fs::set_permissions(
        tree_w.join("mode_only.txt"),
        fs::Permissions::from_mode(0o755),
    )
    .unwrap();

    let r_big = run_act(&mut stream, 11, "inv-diff-big", &digest, "commit()");
    let summary = match r_big {
        ExecuteOutcome::Succeeded {
            receipt_summary, ..
        } => receipt_summary,
        other => panic!("大补丁发布失败：{other:?}"),
    };
    assert!(summary.is_truncated(), "超大补丁摘要必须标记 truncated");
    let summary_json_len = serde_json::to_vec(&summary).unwrap().len();
    assert!(
        summary_json_len <= MAX_RECEIPT_SUMMARY_BYTES,
        "摘要紧凑 JSON {summary_json_len} 不得超过 16 KiB"
    );

    let pub_summary = summary
        .effects()
        .iter()
        .find(|e| {
            e.changes()
                .iter()
                .any(|c| c.residue == Residue::NotRetained)
        })
        .expect("摘要应保留发布 effect 条目");
    let by_target: BTreeMap<&str, &lancet_protocol::Change> = pub_summary
        .changes()
        .iter()
        .map(|c| (c.target.as_str(), c))
        .collect();
    assert_eq!(by_target.len(), 4, "摘要应保留全部 4 条路径");
    assert!(
        by_target["big1.txt"].diff_truncated || by_target["big2.txt"].diff_truncated,
        "大文本补丁在摘要中应被裁剪或去掉并置 diff_truncated = true"
    );
    assert!(
        by_target["bin.dat"].diff.is_none() && !by_target["bin.dat"].diff_truncated,
        "二进制无补丁项 diff_truncated 必须为 false"
    );
    assert!(
        by_target["mode_only.txt"].diff.is_none() && !by_target["mode_only.txt"].diff_truncated,
        "仅改权限无补丁项 diff_truncated 必须为 false"
    );

    let full_rec = match query_receipt(&mut stream, 12, "inv-diff-big") {
        ReceiptLookup::Settled { receipt } => receipt,
        other => panic!("期望 Settled：{other:?}"),
    };
    let pub_full = full_rec
        .effects
        .iter()
        .find(|e| {
            e.changes()
                .iter()
                .any(|c| c.residue == Residue::NotRetained)
        })
        .unwrap();
    let full_by_target: BTreeMap<&str, &lancet_protocol::Change> = pub_full
        .changes()
        .iter()
        .map(|c| (c.target.as_str(), c))
        .collect();
    let summary_diff_total: usize = pub_summary
        .changes()
        .iter()
        .filter_map(|c| c.diff.as_ref().map(String::len))
        .sum();
    let full_diff_total: usize = pub_full
        .changes()
        .iter()
        .filter_map(|c| c.diff.as_ref().map(String::len))
        .sum();
    assert!(
        full_diff_total > summary_diff_total,
        "receipt 查询应返回更多补丁内容（full={full_diff_total}, summary={summary_diff_total}）"
    );
    assert!(
        full_by_target["big1.txt"]
            .diff
            .as_deref()
            .unwrap()
            .contains("中文\"引号\"\\反斜杠行-399")
    );
    assert!(
        full_by_target["big2.txt"]
            .diff
            .as_deref()
            .unwrap()
            .contains("第二文件\"转义\"\\内容-399")
    );
    assert!(!full_by_target["bin.dat"].diff_truncated);
    assert!(!full_by_target["mode_only.txt"].diff_truncated);
}

// =============================================================================
// 12. 第 2 轮审查缺陷复现与回归测试（B1–B2、C1–C7）
// =============================================================================

/// B1：普通文件 `x` 换成含 `child` 的目录；`base/x/child` 已安装、`Settled` 未写入时重启，
/// `Finish(Committed)` 重放不得对已存在的非空目录 `base/x` 再执行 `rmdir`，节点须正常响应 `hello` 并补齐其余子项。
#[test]
fn finish_replay_reuses_existing_nonempty_base_dir() {
    let (root, home, config_path, state_dir, process) =
        start_node_with(Duration::from_secs(10), |root| {
            let w = root.join("w");
            fs::create_dir(&w).unwrap();
            fs::write(w.join("x"), "orig-file-x\n").unwrap();
            format!(
                "[[dir]]\nname = \"w\"\npath = {:?}\naccess = \"write\"\n",
                w.to_str().unwrap()
            )
        });
    let host_w = root.path().join("w");
    let canonical_w = fs::canonicalize(&host_w).unwrap();
    let shadow_key = compute_shadow_key(&canonical_w, &[], &[]);
    let shadow_root = state_dir.join("shadows").join(&shadow_key);
    let tree_w = shadow_root.join("tree");
    let base_w = shadow_root.join("base");
    let host_root_id = file_id(&canonical_w);

    let (stream, digest) = connect_ready(&process);
    drop(stream);
    drop(process);

    let inv_id = "inv-b1-file-to-dir-replay";
    let call_hash = call_record_hash(inv_id);
    let call_dir = state_dir.join("publish").join(&call_hash);
    let pre_dir = call_dir.join("pre");
    let base_stage_dir = call_dir.join("base");
    fs::create_dir_all(call_dir.join("stage")).unwrap();
    fs::create_dir_all(&pre_dir).unwrap();
    fs::create_dir_all(&base_stage_dir).unwrap();

    // 第 3 轮 C1：产品在清理前必定把宿主原文件 x 归档在 pre/<op_id> 槽中并保留至恢复清理。
    fs::rename(host_w.join("x"), pre_dir.join("1")).unwrap();
    assert_eq!(
        fs::read_to_string(pre_dir.join("1")).unwrap(),
        "orig-file-x\n"
    );
    fs::remove_file(tree_w.join("x")).unwrap();

    // 宿主与影子均已将普通文件 x 替换为目录 x/{child,child2}。
    for dir in [&host_w, &tree_w] {
        fs::create_dir(dir.join("x")).unwrap();
        fs::set_permissions(dir.join("x"), fs::Permissions::from_mode(0o755)).unwrap();
        fs::write(dir.join("x/child"), "child-1\n").unwrap();
        fs::write(dir.join("x/child2"), "child-2\n").unwrap();
    }

    fs::write(base_stage_dir.join("1"), b"child-1\n").unwrap();
    fs::set_permissions(base_stage_dir.join("1"), fs::Permissions::from_mode(0o644)).unwrap();
    let sb_c1 = StagedBase {
        slot: 1,
        id: file_id(&base_stage_dir.join("1")),
    };
    fs::write(base_stage_dir.join("2"), b"child-2\n").unwrap();
    fs::set_permissions(base_stage_dir.join("2"), fs::Permissions::from_mode(0o644)).unwrap();
    let sb_c2 = StagedBase {
        slot: 2,
        id: file_id(&base_stage_dir.join("2")),
    };

    // 模拟崩溃点：base/x 已变成目录且 base_stage/1 已改名进 base/x/child，base_stage/2 尚未改名，Settled 未写。
    fs::remove_file(base_w.join("x")).unwrap();
    fs::create_dir(base_w.join("x")).unwrap();
    fs::rename(base_stage_dir.join("1"), base_w.join("x/child")).unwrap();

    let st_orig_x = ObjectState::File {
        mode: 0o644,
        size: 12,
        sha256: sha256_bytes(b"orig-file-x\n"),
    };
    let st_dir_x = ObjectState::Dir { mode: 0o755 };
    let st_child1 = ObjectState::File {
        mode: 0o644,
        size: 8,
        sha256: sha256_bytes(b"child-1\n"),
    };
    let st_child2 = ObjectState::File {
        mode: 0o644,
        size: 8,
        sha256: sha256_bytes(b"child-2\n"),
    };

    let record = CallRecord::Pending {
        invocation_id: inv_id.to_owned(),
        tool: "act".to_owned(),
        catalog_digest: digest.clone(),
        labels: Labels::new(),
        writes_possible: true,
        publish: Some(PublishJournal::Finish {
            call_hash: call_hash.clone(),
            roots: vec![JournalRoot {
                handle: "w".to_owned(),
                shadow_key,
                host_dev: host_root_id.dev,
                host_ino: host_root_id.ino,
            }],
            paths: vec![
                BatchPath {
                    root_idx: 0,
                    rel: "x".to_owned(),
                    before: Some(st_orig_x),
                    after: Some(st_dir_x),
                    staged_base: None,
                },
                BatchPath {
                    root_idx: 0,
                    rel: "x/child".to_owned(),
                    before: None,
                    after: Some(st_child1),
                    staged_base: Some(sb_c1),
                },
                BatchPath {
                    root_idx: 0,
                    rel: "x/child2".to_owned(),
                    before: None,
                    after: Some(st_child2),
                    staged_base: Some(sb_c2),
                },
            ],
            outcome: PublishFinish::Committed,
        }),
    };
    write_call_record(&state_dir, inv_id, &record);

    let toml_content = fs::read_to_string(&config_path).unwrap();
    let restarted = NodeProcess::start(&config_path, &toml_content, &state_dir, &home);
    assert!(matches!(
        read_call_record(&state_dir, inv_id),
        Some(CallRecord::Settled { .. })
    ));
    assert!(!call_dir.exists());
    assert_eq!(
        fs::read_to_string(base_w.join("x/child")).unwrap(),
        "child-1\n"
    );
    assert_eq!(
        fs::read_to_string(base_w.join("x/child2")).unwrap(),
        "child-2\n"
    );

    let (mut s2, d2) = connect_ready(&restarted);
    let st = run_act(&mut s2, 10, "inv-b1-status", &d2, "return w.status()");
    match st {
        ExecuteOutcome::Succeeded { content, .. } => {
            assert_eq!(content["value"], json!([]), "恢复补齐后 status 应为空");
        }
        other => panic!("查询 status 失败：{other:?}"),
    }
}

/// 第 3 轮 A1：原有目录 `d/old`，影子把 `d` 换成普通文件并完成实际发布、清理暂存后，
/// 保留 `Finish(Committed)` 记录重启节点（且再次重启），不得报 ``刷盘基准目录 `d` 失败``，
/// 节点须在首个 `hello` 之前正常结算并启动。
#[test]
fn finish_replay_after_dir_replaced_with_file_and_cleanup() {
    let (root, home, config_path, state_dir, process) =
        start_node_with(Duration::from_secs(10), |root| {
            let w = root.join("w");
            fs::create_dir_all(w.join("d")).unwrap();
            fs::write(w.join("d/old"), "old-in-dir\n").unwrap();
            format!(
                "[[dir]]\nname = \"w\"\npath = {:?}\naccess = \"write\"\n",
                w.to_str().unwrap()
            )
        });
    let host_w = root.path().join("w");
    let canonical_w = fs::canonicalize(&host_w).unwrap();
    let shadow_key = compute_shadow_key(&canonical_w, &[], &[]);
    let shadow_root = state_dir.join("shadows").join(&shadow_key);
    let tree_w = shadow_root.join("tree");
    let base_w = shadow_root.join("base");
    let host_root_id = file_id(&canonical_w);

    let (mut stream, digest) = connect_ready(&process);

    // 用真实发布把目录 d/old 换成普通文件 d。
    fs::remove_dir_all(tree_w.join("d")).unwrap();
    fs::write(tree_w.join("d"), "now-a-file\n").unwrap();
    let pub_out = run_act(&mut stream, 10, "inv-a1-real-pub", &digest, "commit()");
    assert!(
        matches!(pub_out, ExecuteOutcome::Succeeded { .. }),
        "真实发布应成功：{pub_out:?}"
    );
    assert!(host_w.join("d").is_file());
    assert!(base_w.join("d").is_file());
    let installed_base_d_id = file_id(&base_w.join("d"));
    drop(stream);
    drop(process);

    // 模拟已发布且暂存已清理、但账本仍停在 Pending/Finish(Committed) 的崩溃现场。
    let inv_id = "inv-a1-replay-dir-to-file";
    let call_hash = call_record_hash(inv_id);
    assert!(!state_dir.join("publish").join(&call_hash).exists());

    let st_old = ObjectState::File {
        mode: 0o644,
        size: 11,
        sha256: sha256_bytes(b"old-in-dir\n"),
    };
    let st_dir = ObjectState::Dir { mode: 0o755 };
    let st_file_d = ObjectState::File {
        mode: 0o644,
        size: 11,
        sha256: sha256_bytes(b"now-a-file\n"),
    };
    let record = CallRecord::Pending {
        invocation_id: inv_id.to_owned(),
        tool: "act".to_owned(),
        catalog_digest: digest,
        labels: Labels::new(),
        writes_possible: true,
        publish: Some(PublishJournal::Finish {
            call_hash,
            roots: vec![JournalRoot {
                handle: "w".to_owned(),
                shadow_key,
                host_dev: host_root_id.dev,
                host_ino: host_root_id.ino,
            }],
            paths: vec![
                BatchPath {
                    root_idx: 0,
                    rel: "d".to_owned(),
                    before: Some(st_dir),
                    after: Some(st_file_d),
                    staged_base: Some(StagedBase {
                        slot: 1,
                        id: installed_base_d_id,
                    }),
                },
                BatchPath {
                    root_idx: 0,
                    rel: "d/old".to_owned(),
                    before: Some(st_old),
                    after: None,
                    staged_base: None,
                },
            ],
            outcome: PublishFinish::Committed,
        }),
    };
    write_call_record(&state_dir, inv_id, &record);

    let toml_content = fs::read_to_string(&config_path).unwrap();
    // 第一次重启必须顺利恢复并结算为 Settled。
    let restarted1 = NodeProcess::start(&config_path, &toml_content, &state_dir, &home);
    assert!(matches!(
        read_call_record(&state_dir, inv_id),
        Some(CallRecord::Settled { .. })
    ));
    assert_eq!(file_id(&base_w.join("d")), installed_base_d_id);
    let (mut s1, d1) = connect_ready(&restarted1);
    let st1 = run_act(&mut s1, 11, "inv-a1-st1", &d1, "return w.status()");
    assert!(matches!(
        st1,
        ExecuteOutcome::Succeeded { content, .. } if content["value"] == json!([])
    ));
    drop(s1);
    drop(restarted1);

    // 第二次重启同样正常启动。
    let restarted2 = NodeProcess::start(&config_path, &toml_content, &state_dir, &home);
    let (mut s2, _) = connect_ready(&restarted2);
    assert!(matches!(
        query_receipt(&mut s2, 12, inv_id),
        ReceiptLookup::Settled { .. }
    ));
}

/// B2：宿主普通文件 `x` 换成影子中的空目录（权限 `0555`），阶段 3 将暂存目录移到宿主临时名后、
/// 执行 `EXCHANGE` 前崩溃；恢复时阶段 5 `ChmodDir` 不得把原文件误判为“保留宿主修改”，
/// 须完整撤回、无发布条目、保留宿主原文件与影子中的目录草稿。
#[test]
fn recovery_does_not_mark_unexchanged_readonly_dir_as_published() {
    let (root, home, config_path, state_dir, process) =
        start_node_with(Duration::from_secs(10), |root| {
            let w = root.join("w");
            fs::create_dir(&w).unwrap();
            fs::write(w.join("x"), "orig-file-x\n").unwrap();
            format!(
                "[[dir]]\nname = \"w\"\npath = {:?}\naccess = \"write\"\n",
                w.to_str().unwrap()
            )
        });
    let host_w = root.path().join("w");
    let canonical_w = fs::canonicalize(&host_w).unwrap();
    let shadow_key = compute_shadow_key(&canonical_w, &[], &[]);
    let tree_w = state_dir.join("shadows").join(&shadow_key).join("tree");
    let host_root_id = file_id(&canonical_w);

    let (stream, digest) = connect_ready(&process);
    drop(stream);
    drop(process);

    // 影子中把普通文件 x 换成空目录（权限 0555）。
    fs::remove_file(tree_w.join("x")).unwrap();
    fs::create_dir(tree_w.join("x")).unwrap();
    fs::set_permissions(tree_w.join("x"), fs::Permissions::from_mode(0o555)).unwrap();

    let inv_id = "inv-b2-unexchanged-ro-dir";
    let call_hash = call_record_hash(inv_id);
    let call_dir = state_dir.join("publish").join(&call_hash);
    let stage_dir = call_dir.join("stage");
    let pre_dir = call_dir.join("pre");
    fs::create_dir_all(&stage_dir).unwrap();
    fs::create_dir_all(&pre_dir).unwrap();

    // 阶段 3 暂存目录（0755）已移至宿主临时名，但尚未执行 RENAME_EXCHANGE，host/x 仍是原普通文件。
    fs::create_dir(stage_dir.join("1")).unwrap();
    fs::set_permissions(stage_dir.join("1"), fs::Permissions::from_mode(0o755)).unwrap();
    let staged_dir_id = file_id(&stage_dir.join("1"));
    let host_tmp = format!(".lancet-pub-{}-1", &call_hash[..16]);
    fs::rename(stage_dir.join("1"), host_w.join(&host_tmp)).unwrap();

    let st_orig_x = ObjectState::File {
        mode: 0o644,
        size: 12,
        sha256: sha256_bytes(b"orig-file-x\n"),
    };
    let st_staged_dir = ObjectState::Dir { mode: 0o755 };
    let st_final_dir = ObjectState::Dir { mode: 0o555 };

    let record = CallRecord::Pending {
        invocation_id: inv_id.to_owned(),
        tool: "act".to_owned(),
        catalog_digest: digest,
        labels: Labels::new(),
        writes_possible: true,
        publish: Some(PublishJournal::Undo {
            call_hash: call_hash.clone(),
            roots: vec![JournalRoot {
                handle: "w".to_owned(),
                shadow_key,
                host_dev: host_root_id.dev,
                host_ino: host_root_id.ino,
            }],
            ops: vec![
                JournalOp::Exchange {
                    op_id: 1,
                    root_idx: 0,
                    rel: "x".to_owned(),
                    staged_id: staged_dir_id,
                    host_tmp_name: host_tmp.clone(),
                    before: st_orig_x.clone(),
                    after: st_staged_dir,
                },
                JournalOp::ChmodDir {
                    op_id: 2,
                    root_idx: 0,
                    rel: "x".to_owned(),
                    before_mode: 0o755,
                    after_mode: 0o555,
                    original_mode: None,
                    target_id: staged_dir_id,
                },
            ],
            paths: vec![BatchPath {
                root_idx: 0,
                rel: "x".to_owned(),
                before: Some(st_orig_x),
                after: Some(st_final_dir),
                staged_base: None,
            }],
            rollback_reason: None,
        }),
    };
    write_call_record(&state_dir, inv_id, &record);

    let toml_content = fs::read_to_string(&config_path).unwrap();
    let restarted = NodeProcess::start(&config_path, &toml_content, &state_dir, &home);
    assert!(!host_w.join(&host_tmp).exists(), "宿主临时名应已清理");
    assert_eq!(
        fs::read_to_string(host_w.join("x")).unwrap(),
        "orig-file-x\n",
        "宿主原文件必须完好"
    );

    let (mut s2, d2) = connect_ready(&restarted);
    let rec = match query_receipt(&mut s2, 10, inv_id) {
        ReceiptLookup::Settled { receipt } => receipt,
        other => panic!("期望 Settled：{other:?}"),
    };
    match &rec.outcome {
        ReceiptOutcome::Indeterminate { reason } => {
            assert!(
                reason.contains("发布已撤回"),
                "未交换的只读目录应完整撤回，实际原因：{reason}"
            );
        }
        other => panic!("期望 Indeterminate，实际：{other:?}"),
    }
    assert!(
        rec.effects.is_empty(),
        "未交换的只读目录不得记出发布条目，实际：{:?}",
        rec.effects
    );

    let st = run_act(&mut s2, 11, "inv-b2-status", &d2, "return w.status()");
    match st {
        ExecuteOutcome::Succeeded { content, .. } => {
            let arr = content["value"].as_array().unwrap();
            assert_eq!(arr.len(), 1, "影子中的目录草稿必须保留，实际：{arr:?}");
            assert_eq!(arr[0]["path"], json!("x"));
        }
        other => panic!("查询 status 失败：{other:?}"),
    }
    assert!(tree_w.join("x").is_dir(), "影子中 x 仍应是目录草稿");
    drop(restarted);
    let _ = fs::set_permissions(tree_w.join("x"), fs::Permissions::from_mode(0o700));
}

/// C1：发布把含 `d/old` 的目录 `d` 换成普通文件，宿主随后又把这个文件换成自己的目录 `d`。
/// 撤回不得把归档的 `old` 放进宿主的新目录 `d`，`d` 与 `d/old` 均记为保留宿主修改，
/// 整棵子树的基准保持原样，本次恢复与再次重启均不得报 `EISDIR`。
#[test]
fn partial_rollback_does_not_restore_children_into_host_dir_and_recovers() {
    let (root, home, config_path, state_dir, process) =
        start_node_with(Duration::from_secs(10), |root| {
            let w = root.join("w");
            fs::create_dir_all(w.join("d")).unwrap();
            fs::write(w.join("d/old"), "old-content\n").unwrap();
            format!(
                "[[dir]]\nname = \"w\"\npath = {:?}\naccess = \"write\"\n",
                w.to_str().unwrap()
            )
        });
    let host_w = root.path().join("w");
    let canonical_w = fs::canonicalize(&host_w).unwrap();
    let shadow_key = compute_shadow_key(&canonical_w, &[], &[]);
    let shadow_root = state_dir.join("shadows").join(&shadow_key);
    let tree_w = shadow_root.join("tree");
    let host_root_id = file_id(&canonical_w);

    let (stream, digest) = connect_ready(&process);
    drop(stream);
    drop(process);

    // 影子中把目录 d（含 d/old）换成普通文件 d。
    fs::remove_dir_all(tree_w.join("d")).unwrap();
    fs::write(tree_w.join("d"), "draft-file-d\n").unwrap();

    let inv_id = "inv-c1-dir-to-file-host-rebuilt-dir";
    let call_hash = call_record_hash(inv_id);
    let call_dir = state_dir.join("publish").join(&call_hash);
    let stage_dir = call_dir.join("stage");
    let pre_dir = call_dir.join("pre");
    let base_stage_dir = call_dir.join("base");
    fs::create_dir_all(&stage_dir).unwrap();
    fs::create_dir_all(&pre_dir).unwrap();
    fs::create_dir_all(&base_stage_dir).unwrap();

    fs::write(base_stage_dir.join("1"), b"draft-file-d\n").unwrap();
    fs::set_permissions(base_stage_dir.join("1"), fs::Permissions::from_mode(0o644)).unwrap();
    let sb_d = StagedBase {
        slot: 1,
        id: file_id(&base_stage_dir.join("1")),
    };

    // 模拟已执行完 op1(Delete d/old -> pre/1) 与 op2(Exchange d: Dir -> File, old dir -> pre/2)。
    fs::write(stage_dir.join("2"), b"draft-file-d\n").unwrap();
    fs::set_permissions(stage_dir.join("2"), fs::Permissions::from_mode(0o644)).unwrap();
    let staged_file_id = file_id(&stage_dir.join("2"));

    fs::rename(host_w.join("d/old"), pre_dir.join("1")).unwrap();
    fs::rename(host_w.join("d"), pre_dir.join("2")).unwrap();
    fs::rename(stage_dir.join("2"), host_w.join("d")).unwrap();

    // 宿主随后删掉发布的普通文件 d，换成宿主自己的目录 d（含 host_keep.txt）。
    fs::remove_file(host_w.join("d")).unwrap();
    fs::create_dir(host_w.join("d")).unwrap();
    fs::write(host_w.join("d/host_keep.txt"), "from-host\n").unwrap();

    let st_old = ObjectState::File {
        mode: 0o644,
        size: 12,
        sha256: sha256_bytes(b"old-content\n"),
    };
    let st_dir = ObjectState::Dir { mode: 0o755 };
    let st_draft_file = ObjectState::File {
        mode: 0o644,
        size: 13,
        sha256: sha256_bytes(b"draft-file-d\n"),
    };

    let record = CallRecord::Pending {
        invocation_id: inv_id.to_owned(),
        tool: "act".to_owned(),
        catalog_digest: digest,
        labels: Labels::new(),
        writes_possible: true,
        publish: Some(PublishJournal::Undo {
            call_hash: call_hash.clone(),
            roots: vec![JournalRoot {
                handle: "w".to_owned(),
                shadow_key,
                host_dev: host_root_id.dev,
                host_ino: host_root_id.ino,
            }],
            ops: vec![
                JournalOp::Delete {
                    op_id: 1,
                    root_idx: 0,
                    rel: "d/old".to_owned(),
                    before: st_old.clone(),
                },
                JournalOp::Exchange {
                    op_id: 2,
                    root_idx: 0,
                    rel: "d".to_owned(),
                    staged_id: staged_file_id,
                    host_tmp_name: format!(".lancet-pub-{}-2", &call_hash[..16]),
                    before: st_dir.clone(),
                    after: st_draft_file.clone(),
                },
            ],
            paths: vec![
                BatchPath {
                    root_idx: 0,
                    rel: "d".to_owned(),
                    before: Some(st_dir),
                    after: Some(st_draft_file),
                    staged_base: Some(sb_d),
                },
                BatchPath {
                    root_idx: 0,
                    rel: "d/old".to_owned(),
                    before: Some(st_old),
                    after: None,
                    staged_base: None,
                },
            ],
            rollback_reason: None,
        }),
    };
    write_call_record(&state_dir, inv_id, &record);

    let toml_content = fs::read_to_string(&config_path).unwrap();
    // 第一次重启：不得报 EISDIR，须成功响应 hello。
    let restarted1 = NodeProcess::start(&config_path, &toml_content, &state_dir, &home);
    assert!(
        !host_w.join("d/old").exists(),
        "不得把归档的 d/old 放进宿主新建的目录 d"
    );
    assert_eq!(
        fs::read_to_string(host_w.join("d/host_keep.txt")).unwrap(),
        "from-host\n"
    );
    assert!(
        shadow_root.join("base/d").is_dir(),
        "保留宿主对象的目录子树基准应保持原样（目录 d 未被换成文件）"
    );

    let (mut s2, _d2) = connect_ready(&restarted1);
    let rec = match query_receipt(&mut s2, 10, inv_id) {
        ReceiptLookup::Settled { receipt } => receipt,
        other => panic!("期望 Settled：{other:?}"),
    };
    match &rec.outcome {
        ReceiptOutcome::Indeterminate { reason } => {
            assert!(
                reason.contains("w \"d\"：保留宿主修改")
                    && reason.contains("w \"d/old\"：保留宿主修改"),
                "d 与 d/old 均应记为保留宿主修改，实际：\n{reason}"
            );
        }
        other => panic!("期望 Indeterminate，实际：{other:?}"),
    }
    drop(s2);
    drop(restarted1);

    // 再次重启同样须顺畅启动。
    let restarted2 = NodeProcess::start(&config_path, &toml_content, &state_dir, &home);
    let (mut s3, _d3) = connect_ready(&restarted2);
    assert!(matches!(
        query_receipt(&mut s3, 11, inv_id),
        ReceiptLookup::Settled { .. }
    ));
}

/// C2：写初始 `Undo` 日志失败时，节点必须按账本写入失败终止进程，
/// 既不在进程内删除暂存目录也不撤回，恢复权限重启后按磁盘日志干净恢复。
#[test]
fn journal_write_failure_terminates_without_deleting_staging() {
    let (root, home, config_path, state_dir, process) =
        start_node_with(Duration::from_secs(10), |root| {
            let w = root.join("w");
            let ctl = root.join("ctl");
            fs::create_dir(&w).unwrap();
            fs::create_dir(&ctl).unwrap();
            fs::write(w.join("a.txt"), "base\n").unwrap();
            fs::write(ctl.join("go.txt"), "0").unwrap();
            format!(
                r#"
[[dir]]
name = "w"
path = {w:?}
access = "write"

[[dir]]
name = "ctl"
path = {ctl:?}
access = "read"
"#,
                w = w.to_str().unwrap(),
                ctl = ctl.to_str().unwrap(),
            )
        });
    let host_w = root.path().join("w");
    let ctl_dir = root.path().join("ctl");
    let canonical_w = fs::canonicalize(&host_w).unwrap();
    let tree_w = state_dir
        .join("shadows")
        .join(compute_shadow_key(&canonical_w, &[], &[]))
        .join("tree");
    let (mut stream, digest) = connect_ready(&process);

    let inv_id = "inv-c2-journal-write-fail";
    let call_hash = call_record_hash(inv_id);
    let script = r#"
w.write("a.txt", "new-c2\n")
w.write("ready.txt", "1")
commit()
for _ in range(100000):
    if ctl.read("go.txt") == "1":
        break
"#;
    write_frame(
        &mut stream,
        &Frame {
            request_id: 10,
            message: exec_req(inv_id, &digest, script, 15_000, 65_536),
        },
    )
    .unwrap();

    // 等脚本完成写入并已通过 admit_execute 落盘初始 Pending 后，锁住 calls/ 为 0555 触发写 Undo 失败。
    let dl = Instant::now() + Duration::from_secs(5);
    while !tree_w.join("ready.txt").exists() {
        assert!(Instant::now() < dl, "等待脚本写出 ready.txt 超时");
        thread::sleep(Duration::from_millis(5));
    }
    let calls_dir = state_dir.join("calls");
    fs::set_permissions(&calls_dir, fs::Permissions::from_mode(0o555)).unwrap();
    fs::write(ctl_dir.join("go.txt"), "1").unwrap();

    // 节点遇到写日志失败必须终止进程（连接随之断开），绝不可返回 Failed(InternalError) 并删暂存。
    let read_res = read_frame::<_, Response>(&mut stream);
    let _ = fs::set_permissions(&calls_dir, fs::Permissions::from_mode(0o700));
    match read_res {
        Ok(None) | Err(_) => {}
        Ok(Some(frame)) => {
            panic!("写日志失败后节点应立即终止进程，不应继续回包：{frame:?}");
        }
    }

    let pub_dir = state_dir.join("publish").join(&call_hash);
    assert!(
        pub_dir.is_dir(),
        "写日志失败终止进程时不得提前删除暂存目录 {}",
        pub_dir.display()
    );
    assert_eq!(fs::read_to_string(host_w.join("a.txt")).unwrap(), "base\n");

    drop(stream);
    drop(process);

    // 恢复权限后重启，节点在首个 hello 之前清理暂存并结算为 Indeterminate。
    let toml_content = fs::read_to_string(&config_path).unwrap();
    let restarted = NodeProcess::start(&config_path, &toml_content, &state_dir, &home);
    assert!(!pub_dir.exists(), "重启恢复后暂存目录应被清理");
    let (mut s2, _) = connect_ready(&restarted);
    let rec = match query_receipt(&mut s2, 20, inv_id) {
        ReceiptLookup::Settled { receipt } => receipt,
        other => panic!("期望 Settled：{other:?}"),
    };
    assert!(matches!(rec.outcome, ReceiptOutcome::Indeterminate { .. }));
}

/// C3：推进 `base/` 期间改权/落盘失败不得静默吞错或写入 `Settled`，
/// 必须保留 `Finish` 日志并终止/拒绝启动，修复权限后重启再补齐结算；
/// 同时验证只读无执行位祖先（`0444`）下的子目录（`0555`）在 Pass 3 中能由深到浅精确设权并刷盘而不吞 `EACCES`。
#[test]
fn base_advance_failure_preserves_finish_without_settling() {
    // 1. 推进失败必须保留 Finish(Committed) 且拒绝启动，不得写入 Settled。
    {
        let (root, home, config_path, state_dir, process) =
            start_node_with(Duration::from_secs(10), |root| {
                let w = root.join("w");
                fs::create_dir_all(w.join("sub")).unwrap();
                fs::write(w.join("sub/f.txt"), "v0\n").unwrap();
                format!(
                    "[[dir]]\nname = \"w\"\npath = {:?}\naccess = \"write\"\n",
                    w.to_str().unwrap()
                )
            });
        let host_w = root.path().join("w");
        let canonical_w = fs::canonicalize(&host_w).unwrap();
        let shadow_key = compute_shadow_key(&canonical_w, &[], &[]);
        let shadow_root = state_dir.join("shadows").join(&shadow_key);
        let base_w = shadow_root.join("base");
        let tree_w = shadow_root.join("tree");
        let host_root_id = file_id(&canonical_w);

        let (stream, digest) = connect_ready(&process);
        drop(stream);
        drop(process);

        fs::set_permissions(host_w.join("sub"), fs::Permissions::from_mode(0o555)).unwrap();
        fs::set_permissions(tree_w.join("sub"), fs::Permissions::from_mode(0o555)).unwrap();
        fs::set_permissions(&base_w, fs::Permissions::from_mode(0o500)).unwrap();
        fs::set_permissions(&shadow_root, fs::Permissions::from_mode(0o500)).unwrap();

        let inv_id = "inv-c3-base-sync-fail";
        let call_hash = call_record_hash(inv_id);
        let call_dir = state_dir.join("publish").join(&call_hash);
        fs::create_dir_all(call_dir.join("stage")).unwrap();
        fs::create_dir_all(call_dir.join("pre")).unwrap();

        let record = CallRecord::Pending {
            invocation_id: inv_id.to_owned(),
            tool: "act".to_owned(),
            catalog_digest: digest,
            labels: Labels::new(),
            writes_possible: true,
            publish: Some(PublishJournal::Finish {
                call_hash: call_hash.clone(),
                roots: vec![JournalRoot {
                    handle: "w".to_owned(),
                    shadow_key,
                    host_dev: host_root_id.dev,
                    host_ino: host_root_id.ino,
                }],
                paths: vec![BatchPath {
                    root_idx: 0,
                    rel: "sub".to_owned(),
                    before: Some(ObjectState::Dir { mode: 0o755 }),
                    after: Some(ObjectState::Dir { mode: 0o555 }),
                    staged_base: None,
                }],
                outcome: PublishFinish::Committed,
            }),
        };
        write_call_record(&state_dir, inv_id, &record);

        let toml_content = fs::read_to_string(&config_path).unwrap();
        let _stderr = run_expect_failure(&config_path, &toml_content, &home);
        assert!(
            matches!(
                read_call_record(&state_dir, inv_id),
                Some(CallRecord::Pending {
                    publish: Some(PublishJournal::Finish {
                        outcome: PublishFinish::Committed,
                        ..
                    }),
                    ..
                })
            ),
            "推进基准失败时必须保留 Finish(Committed)，绝不可写入 Settled"
        );

        fs::set_permissions(&shadow_root, fs::Permissions::from_mode(0o700)).unwrap();
        fs::set_permissions(&base_w, fs::Permissions::from_mode(0o700)).unwrap();
        let restarted = NodeProcess::start(&config_path, &toml_content, &state_dir, &home);
        assert!(matches!(
            read_call_record(&state_dir, inv_id),
            Some(CallRecord::Settled { .. })
        ));
        assert_eq!(
            fs::symlink_metadata(base_w.join("sub")).unwrap().mode() & 0o7777,
            0o555
        );
        drop(restarted);
        let _ = fs::set_permissions(host_w.join("sub"), fs::Permissions::from_mode(0o700));
        let _ = fs::set_permissions(tree_w.join("sub"), fs::Permissions::from_mode(0o700));
        let _ = fs::set_permissions(base_w.join("sub"), fs::Permissions::from_mode(0o700));
    }

    // 2. 基准嵌套只读目录：父目录 p 为 0555，子目录 p/sub 目标权限 0555，且在其中安装预暂存文件 p/sub/f.txt；
    //    Pass 3 必须由深到浅精确恢复权限并刷盘，重启后 ShadowDir::open 正常启动且状态为零。
    //    （无执行位 0444 祖先目录的 EACCES 回归由 shadow::publish 单元测试直接覆盖。）
    {
        let (root, home, config_path, state_dir, process) =
            start_node_with(Duration::from_secs(10), |root| {
                let w = root.join("w");
                fs::create_dir_all(w.join("p/sub")).unwrap();
                fs::set_permissions(w.join("p"), fs::Permissions::from_mode(0o555)).unwrap();
                format!(
                    "[[dir]]\nname = \"w\"\npath = {:?}\naccess = \"write\"\n",
                    w.to_str().unwrap()
                )
            });
        let host_w = root.path().join("w");
        let canonical_w = fs::canonicalize(&host_w).unwrap();
        let shadow_key = compute_shadow_key(&canonical_w, &[], &[]);
        let shadow_root = state_dir.join("shadows").join(&shadow_key);
        let base_w = shadow_root.join("base");
        let tree_w = shadow_root.join("tree");
        let host_root_id = file_id(&canonical_w);

        let (stream, digest) = connect_ready(&process);
        drop(stream);
        drop(process);

        for dir in [&host_w, &tree_w] {
            fs::set_permissions(dir.join("p"), fs::Permissions::from_mode(0o755)).unwrap();
            fs::write(dir.join("p/sub/f.txt"), "nested-ro\n").unwrap();
            fs::set_permissions(dir.join("p/sub"), fs::Permissions::from_mode(0o555)).unwrap();
            fs::set_permissions(dir.join("p"), fs::Permissions::from_mode(0o555)).unwrap();
        }

        let inv_id = "inv-c3-nested-0555-0555";
        let call_hash = call_record_hash(inv_id);
        let call_dir = state_dir.join("publish").join(&call_hash);
        let base_stage_dir = call_dir.join("base");
        fs::create_dir_all(call_dir.join("stage")).unwrap();
        fs::create_dir_all(call_dir.join("pre")).unwrap();
        fs::create_dir_all(&base_stage_dir).unwrap();

        fs::write(base_stage_dir.join("1"), b"nested-ro\n").unwrap();
        fs::set_permissions(base_stage_dir.join("1"), fs::Permissions::from_mode(0o644)).unwrap();
        let sb_f = StagedBase {
            slot: 1,
            id: file_id(&base_stage_dir.join("1")),
        };

        let record = CallRecord::Pending {
            invocation_id: inv_id.to_owned(),
            tool: "act".to_owned(),
            catalog_digest: digest,
            labels: Labels::new(),
            writes_possible: true,
            publish: Some(PublishJournal::Finish {
                call_hash: call_hash.clone(),
                roots: vec![JournalRoot {
                    handle: "w".to_owned(),
                    shadow_key,
                    host_dev: host_root_id.dev,
                    host_ino: host_root_id.ino,
                }],
                paths: vec![
                    BatchPath {
                        root_idx: 0,
                        rel: "p/sub".to_owned(),
                        before: Some(ObjectState::Dir { mode: 0o755 }),
                        after: Some(ObjectState::Dir { mode: 0o555 }),
                        staged_base: None,
                    },
                    BatchPath {
                        root_idx: 0,
                        rel: "p/sub/f.txt".to_owned(),
                        before: None,
                        after: Some(ObjectState::File {
                            mode: 0o644,
                            size: 10,
                            sha256: sha256_bytes(b"nested-ro\n"),
                        }),
                        staged_base: Some(sb_f),
                    },
                ],
                outcome: PublishFinish::Committed,
            }),
        };
        write_call_record(&state_dir, inv_id, &record);

        let toml_content = fs::read_to_string(&config_path).unwrap();
        let restarted = NodeProcess::start(&config_path, &toml_content, &state_dir, &home);
        assert!(matches!(
            read_call_record(&state_dir, inv_id),
            Some(CallRecord::Settled { .. })
        ));
        assert_eq!(
            fs::symlink_metadata(base_w.join("p")).unwrap().mode() & 0o7777,
            0o555
        );
        assert_eq!(
            fs::symlink_metadata(base_w.join("p/sub")).unwrap().mode() & 0o7777,
            0o555,
            "base/p/sub 必须已精确设为 0555"
        );
        assert_eq!(
            fs::read_to_string(base_w.join("p/sub/f.txt")).unwrap(),
            "nested-ro\n"
        );
        let (mut s2, d2) = connect_ready(&restarted);
        let st = run_act(
            &mut s2,
            10,
            "inv-c3-nested-status",
            &d2,
            "return w.status()",
        );
        assert!(matches!(
            st,
            ExecuteOutcome::Succeeded { content, .. } if content["value"] == json!([])
        ));
        drop(s2);
        drop(restarted);
        for dir in [&host_w, &tree_w, &base_w] {
            let _ = fs::set_permissions(dir.join("p"), fs::Permissions::from_mode(0o700));
            let _ = fs::set_permissions(dir.join("p/sub"), fs::Permissions::from_mode(0o700));
        }
    }
}

/// C4：`Finish(RolledBack)` 同样必须核验宿主根身份；将原宿主目录改名后在原路径新建目录，
/// 恢复必须拒绝启动、写明调用与日志位置，且不得静默清理事务目录。
#[test]
fn finish_rolled_back_verifies_root_identity() {
    let (root, home, config_path, state_dir, process) =
        start_node_with(Duration::from_secs(10), |root| {
            let w = root.join("w");
            fs::create_dir(&w).unwrap();
            fs::write(w.join("a.txt"), "orig\n").unwrap();
            format!(
                "[[dir]]\nname = \"w\"\npath = {:?}\naccess = \"write\"\n",
                w.to_str().unwrap()
            )
        });
    let host_w = root.path().join("w");
    let canonical_w = fs::canonicalize(&host_w).unwrap();
    let shadow_key = compute_shadow_key(&canonical_w, &[], &[]);
    let orig_root_id = file_id(&canonical_w);

    let (stream, digest) = connect_ready(&process);
    drop(stream);
    drop(process);

    // 将原宿主根改名并在原路径建一个新目录（同路径、同 shadow_key、不同 ino）。
    fs::rename(&host_w, root.path().join("w_moved")).unwrap();
    fs::create_dir(&host_w).unwrap();
    assert_ne!(file_id(&host_w).ino, orig_root_id.ino);

    let inv_id = "inv-c4-rolledback-root-mismatch";
    let call_hash = call_record_hash(inv_id);
    let call_dir = state_dir.join("publish").join(&call_hash);
    fs::create_dir_all(call_dir.join("stage")).unwrap();
    fs::create_dir_all(call_dir.join("pre")).unwrap();

    let rec = CallRecord::Pending {
        invocation_id: inv_id.to_owned(),
        tool: "act".to_owned(),
        catalog_digest: digest,
        labels: Labels::new(),
        writes_possible: true,
        publish: Some(PublishJournal::Finish {
            call_hash: call_hash.clone(),
            roots: vec![JournalRoot {
                handle: "w".to_owned(),
                shadow_key,
                host_dev: orig_root_id.dev,
                host_ino: orig_root_id.ino,
            }],
            paths: Vec::new(),
            outcome: PublishFinish::RolledBack,
        }),
    };
    let rec_path = write_call_record(&state_dir, inv_id, &rec);

    let toml_content = fs::read_to_string(&config_path).unwrap();
    let stderr = run_expect_failure(&config_path, &toml_content, &home);
    assert!(
        stderr.contains(inv_id) && stderr.contains(rec_path.to_str().unwrap()),
        "Finish(RolledBack) 根身份不匹配时报错须写明 invocation_id 与日志路径，实际：\n{stderr}"
    );
    assert!(call_dir.is_dir(), "拒绝启动时不得静默清理发布事务目录");
}

/// C5：草稿只改了 `d/x`，脚本运行期间宿主把不在本批中的上级目录 `d` 从 `0755` 改为 `0700`（或 `0555`），
/// 发布不得将 `d` 误报为“宿主已改”，须正常发布 `d/x` 并保留宿主对 `d` 的权限修改。
#[test]
fn ancestor_dir_mode_change_not_reported_as_conflict() {
    let (root, _home, _cfg, state_dir, process) =
        start_node_with(Duration::from_secs(10), |root| {
            let w = root.join("w");
            let ctl = root.join("ctl");
            fs::create_dir_all(w.join("d")).unwrap();
            fs::create_dir(&ctl).unwrap();
            fs::set_permissions(w.join("d"), fs::Permissions::from_mode(0o755)).unwrap();
            fs::write(w.join("d/x.txt"), "v0\n").unwrap();
            format!(
                r#"
[[dir]]
name = "w"
path = {w:?}
access = "write"

[[dir]]
name = "ctl"
path = {ctl:?}
access = "read"
"#,
                w = w.to_str().unwrap(),
                ctl = ctl.to_str().unwrap(),
            )
        });
    let host_w = root.path().join("w");
    let ctl_dir = root.path().join("ctl");
    let canonical_w = fs::canonicalize(&host_w).unwrap();
    let tree_w = state_dir
        .join("shadows")
        .join(compute_shadow_key(&canonical_w, &[], &[]))
        .join("tree");
    let (mut stream, digest) = connect_ready(&process);

    for (idx, &(new_mode, ver)) in [(0o700u32, "v1\n"), (0o555u32, "v2\n")].iter().enumerate() {
        let ready = format!("ready_{idx}.txt");
        let go = format!("go_{idx}.txt");
        fs::write(ctl_dir.join(&go), "0").unwrap();
        let script = format!(
            r#"
w.write("d/x.txt", {ver:?})
w.write({ready:?}, "1")
commit()
for _ in range(100000):
    if ctl.read({go:?}) == "1":
        break
"#
        );
        write_frame(
            &mut stream,
            &Frame {
                request_id: 10 + idx as u64,
                message: exec_req(&format!("inv-c5-{idx}"), &digest, &script, 15_000, 65_536),
            },
        )
        .unwrap();

        let dl = Instant::now() + Duration::from_secs(5);
        while !tree_w.join(&ready).exists() {
            assert!(Instant::now() < dl, "等待脚本写出 {ready} 超时");
            thread::sleep(Duration::from_millis(5));
        }
        // 在脚本运行期间（sync_writable_dirs 之后、prepare_publish 之前）修改宿主上级目录 d 的权限。
        fs::set_permissions(host_w.join("d"), fs::Permissions::from_mode(new_mode)).unwrap();
        fs::write(ctl_dir.join(&go), "1").unwrap();

        let resp: Frame<Response> = read_frame(&mut stream).unwrap().unwrap();
        assert!(
            matches!(
                resp.message,
                Response::ExecuteResult {
                    outcome: ExecuteOutcome::Succeeded { .. }
                }
            ),
            "脚本运行期间宿主把不在本批中的上级目录 d 改为 {new_mode:o} 不得报冲突，实际：{:?}",
            resp.message
        );
        assert_eq!(fs::read_to_string(host_w.join("d/x.txt")).unwrap(), ver);
        assert_eq!(
            fs::symlink_metadata(host_w.join("d")).unwrap().mode() & 0o7777,
            new_mode,
            "宿主对 d 的 {new_mode:o} 权限在发布后应保留"
        );
        // 清理为下一轮恢复可写权限并同步。
        fs::set_permissions(host_w.join("d"), fs::Permissions::from_mode(0o755)).unwrap();
    }
}

/// C6：新建项 `x` 在撤回前置核对后被宿主改写，节点移入 `pre/1` 后在后置核对前崩溃；
/// 重启恢复必须对 `pre/1` 重做后置核对，发现内容不等于发布版本后无覆盖放回宿主并记为保留宿主修改。
#[test]
fn recovery_rechecks_created_entry_in_pre_slot() {
    let (root, home, config_path, state_dir, process) =
        start_node_with(Duration::from_secs(10), |root| {
            let w = root.join("w");
            fs::create_dir(&w).unwrap();
            format!(
                "[[dir]]\nname = \"w\"\npath = {:?}\naccess = \"write\"\n",
                w.to_str().unwrap()
            )
        });
    let host_w = root.path().join("w");
    let canonical_w = fs::canonicalize(&host_w).unwrap();
    let shadow_key = compute_shadow_key(&canonical_w, &[], &[]);
    let tree_w = state_dir.join("shadows").join(&shadow_key).join("tree");
    let host_root_id = file_id(&canonical_w);

    let (stream, digest) = connect_ready(&process);
    drop(stream);
    drop(process);

    fs::write(tree_w.join("x"), "pub-x\n").unwrap();

    let inv_id = "inv-c6-create-pre-edited";
    let call_hash = call_record_hash(inv_id);
    let call_dir = state_dir.join("publish").join(&call_hash);
    let stage_dir = call_dir.join("stage");
    let pre_dir = call_dir.join("pre");
    let base_stage_dir = call_dir.join("base");
    fs::create_dir_all(&stage_dir).unwrap();
    fs::create_dir_all(&pre_dir).unwrap();
    fs::create_dir_all(&base_stage_dir).unwrap();

    fs::write(base_stage_dir.join("1"), b"pub-x\n").unwrap();
    fs::set_permissions(base_stage_dir.join("1"), fs::Permissions::from_mode(0o644)).unwrap();
    let sb_x = StagedBase {
        slot: 1,
        id: file_id(&base_stage_dir.join("1")),
    };

    // 模拟：Create x 发布后被宿主原地改写内容，撤回将其移入 pre/1 后尚未复核即崩溃。
    fs::write(pre_dir.join("1"), b"host-edited-in-window\n").unwrap();
    fs::set_permissions(pre_dir.join("1"), fs::Permissions::from_mode(0o644)).unwrap();
    let staged_id = file_id(&pre_dir.join("1"));

    let st_pub_x = ObjectState::File {
        mode: 0o644,
        size: 6,
        sha256: sha256_bytes(b"pub-x\n"),
    };

    let record = CallRecord::Pending {
        invocation_id: inv_id.to_owned(),
        tool: "act".to_owned(),
        catalog_digest: digest,
        labels: Labels::new(),
        writes_possible: true,
        publish: Some(PublishJournal::Undo {
            call_hash: call_hash.clone(),
            roots: vec![JournalRoot {
                handle: "w".to_owned(),
                shadow_key,
                host_dev: host_root_id.dev,
                host_ino: host_root_id.ino,
            }],
            ops: vec![JournalOp::Create {
                op_id: 1,
                root_idx: 0,
                rel: "x".to_owned(),
                staged_id,
                after: st_pub_x.clone(),
            }],
            paths: vec![BatchPath {
                root_idx: 0,
                rel: "x".to_owned(),
                before: None,
                after: Some(st_pub_x),
                staged_base: Some(sb_x),
            }],
            rollback_reason: None,
        }),
    };
    write_call_record(&state_dir, inv_id, &record);

    let toml_content = fs::read_to_string(&config_path).unwrap();
    let restarted = NodeProcess::start(&config_path, &toml_content, &state_dir, &home);
    assert_eq!(
        fs::read_to_string(host_w.join("x")).unwrap(),
        "host-edited-in-window\n",
        "pre/1 中被宿主修改过的新建项必须无覆盖放回宿主，不得丢失宿主写入"
    );

    let (mut s2, _) = connect_ready(&restarted);
    let rec = match query_receipt(&mut s2, 10, inv_id) {
        ReceiptLookup::Settled { receipt } => receipt,
        other => panic!("期望 Settled：{other:?}"),
    };
    match &rec.outcome {
        ReceiptOutcome::Indeterminate { reason } => {
            assert!(
                reason.contains("w \"x\"：保留宿主修改"),
                "应记为保留宿主修改，实际：\n{reason}"
            );
        }
        other => panic!("期望 Indeterminate，实际：{other:?}"),
    }
    assert_eq!(rec.effects.len(), 1, "回执必须包含 x 的发布条目");
    assert_eq!(rec.effects[0].changes()[0].target, "x");
}

/// C7：新建目录 `d` 与子文件 `d/x`，宿主只改写了 `d/x` 的内容；目录 `d` 身份与权限仍是发布后的状态，
/// 撤回原因中 `d` 必须标为“已发布”（而非“保留宿主修改”），且计入回执发布条目。
#[test]
fn rollback_marks_dir_kept_by_edited_child_as_published() {
    for &(label, dir_mode) in &[("0755", 0o755u32), ("0555", 0o555u32)] {
        let (root, home, config_path, state_dir, process) =
            start_node_with(Duration::from_secs(10), |root| {
                let w = root.join("w");
                fs::create_dir(&w).unwrap();
                format!(
                    "[[dir]]\nname = \"w\"\npath = {:?}\naccess = \"write\"\n",
                    w.to_str().unwrap()
                )
            });
        let host_w = root.path().join("w");
        let canonical_w = fs::canonicalize(&host_w).unwrap();
        let shadow_key = compute_shadow_key(&canonical_w, &[], &[]);
        let tree_w = state_dir.join("shadows").join(&shadow_key).join("tree");
        let host_root_id = file_id(&canonical_w);

        let (stream, digest) = connect_ready(&process);
        drop(stream);
        drop(process);

        fs::create_dir(tree_w.join("d")).unwrap();
        fs::write(tree_w.join("d/x"), "pub-x\n").unwrap();
        fs::set_permissions(tree_w.join("d"), fs::Permissions::from_mode(dir_mode)).unwrap();

        let inv_id = format!("inv-c7-dir-published-{label}");
        let call_hash = call_record_hash(&inv_id);
        let call_dir = state_dir.join("publish").join(&call_hash);
        let stage_dir = call_dir.join("stage");
        let pre_dir = call_dir.join("pre");
        let base_stage_dir = call_dir.join("base");
        fs::create_dir_all(&stage_dir).unwrap();
        fs::create_dir_all(&pre_dir).unwrap();
        fs::create_dir_all(&base_stage_dir).unwrap();

        fs::write(base_stage_dir.join("1"), b"pub-x\n").unwrap();
        fs::set_permissions(base_stage_dir.join("1"), fs::Permissions::from_mode(0o644)).unwrap();
        let sb_x = StagedBase {
            slot: 1,
            id: file_id(&base_stage_dir.join("1")),
        };

        // 模拟：d 与 d/x 已发布到宿主，宿主只修改了 d/x 的内容（d 的身份与权限未变）。
        fs::create_dir(host_w.join("d")).unwrap();
        fs::write(host_w.join("d/x"), "host-edited-x\n").unwrap();
        fs::set_permissions(host_w.join("d/x"), fs::Permissions::from_mode(0o644)).unwrap();
        fs::set_permissions(host_w.join("d"), fs::Permissions::from_mode(dir_mode)).unwrap();
        let dir_id = file_id(&host_w.join("d"));
        let file_x_id = file_id(&host_w.join("d/x"));

        let st_dir_writable = ObjectState::Dir {
            mode: dir_mode | 0o700,
        };
        let st_dir_final = ObjectState::Dir { mode: dir_mode };
        let st_x = ObjectState::File {
            mode: 0o644,
            size: 6,
            sha256: sha256_bytes(b"pub-x\n"),
        };

        let mut ops = vec![
            JournalOp::Create {
                op_id: 1,
                root_idx: 0,
                rel: "d".to_owned(),
                staged_id: dir_id,
                after: st_dir_writable,
            },
            JournalOp::Create {
                op_id: 2,
                root_idx: 0,
                rel: "d/x".to_owned(),
                staged_id: file_x_id,
                after: st_x.clone(),
            },
        ];
        if dir_mode != (dir_mode | 0o700) {
            ops.push(JournalOp::ChmodDir {
                op_id: 3,
                root_idx: 0,
                rel: "d".to_owned(),
                before_mode: dir_mode | 0o700,
                after_mode: dir_mode,
                original_mode: None,
                target_id: dir_id,
            });
        }

        let record = CallRecord::Pending {
            invocation_id: inv_id.clone(),
            tool: "act".to_owned(),
            catalog_digest: digest,
            labels: Labels::new(),
            writes_possible: true,
            publish: Some(PublishJournal::Undo {
                call_hash: call_hash.clone(),
                roots: vec![JournalRoot {
                    handle: "w".to_owned(),
                    shadow_key,
                    host_dev: host_root_id.dev,
                    host_ino: host_root_id.ino,
                }],
                ops,
                paths: vec![
                    BatchPath {
                        root_idx: 0,
                        rel: "d".to_owned(),
                        before: None,
                        after: Some(st_dir_final),
                        staged_base: None,
                    },
                    BatchPath {
                        root_idx: 0,
                        rel: "d/x".to_owned(),
                        before: None,
                        after: Some(st_x),
                        staged_base: Some(sb_x),
                    },
                ],
                rollback_reason: None,
            }),
        };
        write_call_record(&state_dir, &inv_id, &record);

        let toml_content = fs::read_to_string(&config_path).unwrap();
        let restarted = NodeProcess::start(&config_path, &toml_content, &state_dir, &home);
        assert_eq!(
            fs::symlink_metadata(host_w.join("d")).unwrap().mode() & 0o7777,
            dir_mode,
            "{label}: 保留在宿主的目录 d 应维持发布后的权限 {dir_mode:o}"
        );
        assert_eq!(
            fs::read_to_string(host_w.join("d/x")).unwrap(),
            "host-edited-x\n"
        );

        let (mut s2, _) = connect_ready(&restarted);
        let rec = match query_receipt(&mut s2, 10, &inv_id) {
            ReceiptLookup::Settled { receipt } => receipt,
            other => panic!("期望 Settled：{other:?}"),
        };
        match &rec.outcome {
            ReceiptOutcome::Indeterminate { reason } => {
                assert!(
                    reason.contains("w \"d\"：已发布"),
                    "{label}: 目录 d 身份与权限未改，应标为已发布，实际：\n{reason}"
                );
                assert!(
                    reason.contains("w \"d/x\"：保留宿主修改"),
                    "{label}: 子文件 d/x 应标为保留宿主修改，实际：\n{reason}"
                );
            }
            other => panic!("期望 Indeterminate，实际：{other:?}"),
        }
        let pub_targets: Vec<&str> = rec
            .effects
            .iter()
            .flat_map(|e| e.changes().iter().map(|c| c.target.as_str()))
            .collect();
        assert_eq!(
            pub_targets,
            vec!["d", "d/x"],
            "{label}: 回执发布条目必须包含目录 d 与子文件 d/x"
        );

        drop(s2);
        drop(restarted);
        let _ = fs::set_permissions(host_w.join("d"), fs::Permissions::from_mode(0o700));
        let _ = fs::set_permissions(tree_w.join("d"), fs::Permissions::from_mode(0o700));
        let _ = fs::set_permissions(
            state_dir
                .join("shadows")
                .join(compute_shadow_key(&canonical_w, &[], &[]))
                .join("base/d"),
            fs::Permissions::from_mode(0o700),
        );
    }
}

// =============================================================================
// 13. measure_publish_latency（Section M + A：4,000 文件发布与晚到冲突撤回基准）
// =============================================================================

#[test]
#[ignore = "4,000 文件 40 目录发布与晚到冲突撤回时延基准；需在 release 下手动运行"]
fn measure_publish_latency() {
    let dir_count = 40usize;
    let files_per_dir = 100usize;
    let total_files = dir_count * files_per_dir;

    let (root, _home, _cfg, state_dir, process) =
        start_node_with(Duration::from_secs(60), |root| {
            let w = root.join("w4k");
            fs::create_dir(&w).unwrap();
            for d in 0..dir_count {
                let sub = w.join(format!("d{d:02}"));
                fs::create_dir(&sub).unwrap();
                for f in 0..files_per_dir {
                    fs::write(
                        sub.join(format!("f{f:02}.txt")),
                        format!("init-d{d:02}-f{f:02}\n"),
                    )
                    .unwrap();
                }
            }
            format!(
                "[[dir]]\nname = \"w\"\npath = {:?}\naccess = \"write\"\n",
                w.to_str().unwrap()
            )
        });

    let host_w = root.path().join("w4k");
    let canonical_w = fs::canonicalize(&host_w).unwrap();
    let tree_w = state_dir
        .join("shadows")
        .join(compute_shadow_key(&canonical_w, &[], &[]))
        .join("tree");
    let (mut stream, digest) = connect_ready(&process);

    let all_rels: Vec<String> = (0..dir_count)
        .flat_map(|d| (0..files_per_dir).map(move |f| format!("d{d:02}/f{f:02}.txt")))
        .collect();
    assert_eq!(all_rels.len(), total_files);

    let mut req_id = 10u64;
    let print_stats = |name: &str, samples: &[Duration]| {
        let mut sorted = samples.to_vec();
        sorted.sort_unstable();
        let median = sorted[sorted.len() / 2];
        let max = *sorted.last().unwrap();
        let formatted: Vec<String> = samples
            .iter()
            .map(|d| format!("{:.3}", d.as_secs_f64() * 1_000.0))
            .collect();
        println!(
            "{name}: samples_ms=[{}], median_ms={:.3}, max_ms={:.3}",
            formatted.join(", "),
            median.as_secs_f64() * 1_000.0,
            max.as_secs_f64() * 1_000.0,
        );
    };

    println!(
        "measure_publish_latency layout: {total_files} files ({dir_count} dirs x {files_per_dir} files), init_file_bytes={}",
        "init-d00-f00\n".len()
    );

    // 场景 1/2/3：修改 1 / 100 / 4,000 个文件并发布。
    for &(batch_size, label) in &[
        (1usize, "publish_1"),
        (100usize, "publish_100"),
        (total_files, "publish_4000"),
    ] {
        let mut samples = Vec::new();
        for s in 0..5usize {
            let content = format!("{label}-s{s}\n");
            for rel in &all_rels[..batch_size] {
                fs::write(tree_w.join(rel), &content).unwrap();
            }
            // 草稿准备与影子核对在计时外完成。
            let prep = run_act(
                &mut stream,
                req_id,
                &format!("prep-{label}-{s}"),
                &digest,
                "return len(w.status())",
            );
            req_id += 1;
            match prep {
                ExecuteOutcome::Succeeded { content: c, .. } => {
                    assert_eq!(c["value"], json!(batch_size));
                }
                other => panic!("{label} 准备失败：{other:?}"),
            }

            let inv_id = format!("bench-{label}-{s}");
            let req = exec_req(&inv_id, &digest, "commit()", 120_000, 65_536);
            let t0 = Instant::now();
            let out = run_exec(&mut stream, req_id, req);
            let dt = t0.elapsed();
            req_id += 1;
            assert!(
                matches!(out, ExecuteOutcome::Succeeded { .. }),
                "{label} 第 {s} 次发布失败：{out:?}"
            );
            samples.push(dt);

            // 计时外验证宿主内容与回执。
            assert_eq!(
                fs::read_to_string(host_w.join(&all_rels[0])).unwrap(),
                content
            );
            assert_eq!(
                fs::read_to_string(host_w.join(&all_rels[batch_size - 1])).unwrap(),
                content
            );
            let rec = match query_receipt(&mut stream, req_id, &inv_id) {
                ReceiptLookup::Settled { receipt } => receipt,
                other => panic!("期望 Settled：{other:?}"),
            };
            req_id += 1;
            let pub_cnt: usize = rec
                .effects
                .iter()
                .flat_map(|e| e.changes().iter())
                .filter(|c| c.residue == Residue::NotRetained)
                .count();
            assert_eq!(pub_cnt, batch_size);
        }
        print_stats(label, &samples);
    }

    // 场景 4：修改 200 个文件并在最后一项触发晚到冲突逆序撤回。
    {
        let batch_size = 200usize;
        let mut samples = Vec::new();
        let mut attempt = 0usize;
        while samples.len() < 5 && attempt < 25 {
            attempt += 1;
            // 先丢弃上一轮残留草稿并做一次空同步，使上一轮并发写者留在宿主上的版本被接纳为干净基准。
            let r_clean = run_act(
                &mut stream,
                req_id,
                &format!("rb-preclean-{attempt}"),
                &digest,
                "discard()",
            );
            req_id += 1;
            assert!(
                matches!(r_clean, ExecuteOutcome::Succeeded { .. }),
                "轮次 {attempt} 预清理失败：{r_clean:?}"
            );
            let r_adopt = run_act(
                &mut stream,
                req_id,
                &format!("rb-adopt-{attempt}"),
                &digest,
                "return len(w.status())",
            );
            req_id += 1;
            assert!(
                matches!(r_adopt, ExecuteOutcome::Succeeded { .. }),
                "轮次 {attempt} 接纳宿主版本失败：{r_adopt:?}"
            );

            let base_ver = format!("rb-base-{attempt}\n");
            for rel in &all_rels[..batch_size] {
                fs::write(tree_w.join(rel), &base_ver).unwrap();
            }
            let _ = run_act(
                &mut stream,
                req_id,
                &format!("rb-base-sync-{attempt}"),
                &digest,
                "return len(w.status())",
            );
            req_id += 1;
            let r_base = run_exec(
                &mut stream,
                req_id,
                exec_req(
                    &format!("rb-base-pub-{attempt}"),
                    &digest,
                    "commit()",
                    60_000,
                    65_536,
                ),
            );
            req_id += 1;
            assert!(
                matches!(r_base, ExecuteOutcome::Succeeded { .. }),
                "轮次 {attempt} 基准发布失败：{r_base:?}"
            );

            let draft_ver = format!("rb-draft-{attempt}\n");
            for rel in &all_rels[..batch_size] {
                fs::write(tree_w.join(rel), &draft_ver).unwrap();
            }
            let _ = run_act(
                &mut stream,
                req_id,
                &format!("rb-draft-sync-{attempt}"),
                &digest,
                "return len(w.status())",
            );
            req_id += 1;

            let stop = Arc::new(AtomicBool::new(false));
            let stop_bg = Arc::clone(&stop);
            let first = host_w.join(&all_rels[0]);
            let last = host_w.join(&all_rels[batch_size - 1]);
            let first_before_id = file_id(&first);
            let last_before_id = file_id(&last);
            let writer_staged = root.path().join(format!("writer-bench-rb-{attempt}.tmp"));
            fs::write(&writer_staged, b"rb-host-late\n").unwrap();
            let writer_staged_bg = writer_staged.clone();
            let expected_first = draft_ver.clone();
            let writer = thread::spawn(move || {
                while !stop_bg.load(Ordering::Relaxed) {
                    if fs::symlink_metadata(&first).ok().is_some_and(|m| {
                        m.dev() != first_before_id.dev || m.ino() != first_before_id.ino
                    }) && fs::read_to_string(&first).ok().as_deref()
                        == Some(expected_first.as_str())
                    {
                        let wrote_old =
                            exchange_with_inode_evidence(&last, &writer_staged_bg, last_before_id);
                        return if wrote_old {
                            LastWriteEvidence::WrotePreExchangeInode {
                                first_edited_new_inode: false,
                            }
                        } else {
                            LastWriteEvidence::WrotePostExchangeInode
                        };
                    }
                }
                LastWriteEvidence::DidNotRun
            });

            let inv_id = format!("bench-rb-{attempt}");
            let req = exec_req(&inv_id, &digest, "commit()", 60_000, 65_536);
            let t0 = Instant::now();
            let out = run_exec(&mut stream, req_id, req);
            let dt = t0.elapsed();
            req_id += 1;
            stop.store(true, Ordering::Relaxed);
            let evidence = writer.join().unwrap();
            let _ = fs::remove_file(&writer_staged);

            match out {
                ExecuteOutcome::Succeeded { .. } if can_retry_as_missed_window(evidence) => {
                    // 写者独立证据确认未在交换前写入末项旧 inode，作废重试。
                    continue;
                }
                ExecuteOutcome::Failed { error, .. }
                    if matches!(evidence, LastWriteEvidence::WrotePreExchangeInode { .. })
                        && error.code == "Conflict" =>
                {
                    assert_eq!(
                        fs::read_to_string(host_w.join(&all_rels[0])).unwrap(),
                        base_ver
                    );
                    assert_eq!(
                        fs::read_to_string(host_w.join(&all_rels[batch_size - 1])).unwrap(),
                        "rb-host-late\n"
                    );
                    let rec = match query_receipt(&mut stream, req_id, &inv_id) {
                        ReceiptLookup::Settled { receipt } => receipt,
                        other => panic!("期望 Settled：{other:?}"),
                    };
                    req_id += 1;
                    assert!(matches!(
                        rec.outcome,
                        ReceiptOutcome::Failed { ref error } if error.code == "Conflict"
                    ));
                    assert!(
                        rec.effects
                            .iter()
                            .flat_map(|e| e.changes().iter())
                            .all(|c| c.residue != Residue::NotRetained)
                    );
                    samples.push(dt);
                }
                other => {
                    panic!(
                        "基准晚到冲突轮次 {attempt}（evidence={evidence:?}）收到意外结局：{other:?}"
                    );
                }
            }
        }
        assert_eq!(samples.len(), 5, "晚到冲突逆序撤回基准未采满 5 个样本");
        println!(
            "late_conflict_rollback_200 attempts={attempt}, file_bytes={}",
            "rb-draft-1\n".len()
        );
        print_stats("late_conflict_rollback_200", &samples);
    }

    // 场景 5（A 节）：同构 4,000 文件目录库层分段计时——单独报告临界区外 prepare_publish
    // 与临界区内（写 Undo 日志 -> execute 五阶段发布 + Finish(Committed) + 基准推进 + 清理 -> 写 Settled 日志）时延。
    {
        let (split_root, _split_home) = temp_env();
        let split_host = split_root.path().join("w4k_split");
        let split_state = split_root.path().join("state_split");
        let split_calls = split_state.join("calls");
        fs::create_dir_all(&split_host).unwrap();
        fs::create_dir_all(&split_calls).unwrap();
        for d in 0..dir_count {
            let sub = split_host.join(format!("d{d:02}"));
            fs::create_dir(&sub).unwrap();
            for f in 0..files_per_dir {
                fs::write(
                    sub.join(format!("f{f:02}.txt")),
                    format!("split-init-d{d:02}-f{f:02}\n"),
                )
                .unwrap();
            }
        }
        let shadow = ShadowDir::open(&split_state, "w", &split_host, &[], &[]).unwrap();
        let split_tree = shadow.tree_path().to_path_buf();

        let persist_record_fsync = |inv_id: &str, rec: &CallRecord| -> anyhow::Result<()> {
            let file_name = call_record_file_name(inv_id);
            let final_path = split_calls.join(&file_name);
            let tmp_path = split_calls.join(format!(".tmp-{file_name}"));
            let bytes = serde_json::to_vec(rec)?;
            let mut f = fs::File::create(&tmp_path)?;
            f.write_all(&bytes)?;
            f.sync_all()?;
            drop(f);
            fs::rename(&tmp_path, &final_path)?;
            let dir_f = fs::File::open(&split_calls)?;
            dir_f.sync_all()?;
            Ok(())
        };

        for &(batch_size, label) in &[(1usize, "1"), (100usize, "100"), (total_files, "4000")] {
            let mut prep_samples = Vec::new();
            let mut crit_samples = Vec::new();
            let mut total_samples = Vec::new();

            for s in 0..5usize {
                let content = format!("split-{label}-s{s}\n");
                for rel in &all_rels[..batch_size] {
                    fs::write(split_tree.join(rel), &content).unwrap();
                }
                assert_eq!(shadow.status().unwrap().len(), batch_size);

                let inv_id = format!("bench-split-{label}-{s}");
                let call_hash = call_record_hash(&inv_id);

                let t_prep = Instant::now();
                let prep =
                    prepare_publish(&split_state, &call_hash, &[&shadow], None, None).unwrap();
                let dt_prep = t_prep.elapsed();
                let PublishPrep::Ready(prepared) = prep else {
                    panic!("分段基准 {label} 第 {s} 次未返回 Ready");
                };

                // 临界区计时起点：持久写 Undo 日志 -> 执行发布与基准推进 -> 持久写 Settled 日志。
                let t_crit = Instant::now();
                let undo_rec = CallRecord::Pending {
                    invocation_id: inv_id.clone(),
                    tool: "act".to_owned(),
                    catalog_digest: digest.clone(),
                    labels: Labels::new(),
                    writes_possible: true,
                    publish: Some(prepared.undo_journal().clone()),
                };
                persist_record_fsync(&inv_id, &undo_rec).unwrap();

                let out = prepared
                    .execute(&[&shadow], |journal| {
                        let rec = CallRecord::Pending {
                            invocation_id: inv_id.clone(),
                            tool: "act".to_owned(),
                            catalog_digest: digest.clone(),
                            labels: Labels::new(),
                            writes_possible: true,
                            publish: Some(journal.clone()),
                        };
                        persist_record_fsync(&inv_id, &rec)
                    })
                    .unwrap();
                let effects = match out {
                    PublishOutcome::Committed { effects } => effects,
                    PublishOutcome::RolledBackConflict { conflicts } => {
                        panic!("分段基准 {label} 第 {s} 次发生晚到冲突：{conflicts:?}")
                    }
                    PublishOutcome::RolledBackIoError { message } => {
                        panic!("分段基准 {label} 第 {s} 次发生 I/O 错误：{message}")
                    }
                    PublishOutcome::Indeterminate { reason, .. } => {
                        panic!("分段基准 {label} 第 {s} 次落入 Indeterminate：{reason}")
                    }
                };
                let settled_rec = CallRecord::Settled {
                    settled_at_ms: 9_999_999_999_999,
                    receipt: Receipt {
                        invocation_id: inv_id.clone(),
                        tool: "act".to_owned(),
                        catalog_digest: digest.clone(),
                        labels: Labels::new(),
                        outcome: ReceiptOutcome::Succeeded {
                            content: json!({"value": null, "printed": ""}),
                        },
                        steps: vec![StepSummary {
                            tool: "commit".to_owned(),
                            outcome: OutcomeKind::Succeeded,
                        }],
                        effects,
                        truncated: false,
                    },
                };
                persist_record_fsync(&inv_id, &settled_rec).unwrap();
                let dt_crit = t_crit.elapsed();

                assert_eq!(
                    fs::read_to_string(split_host.join(&all_rels[batch_size - 1])).unwrap(),
                    content
                );
                assert!(shadow.status().unwrap().is_empty());

                prep_samples.push(dt_prep);
                crit_samples.push(dt_crit);
                total_samples.push(dt_prep + dt_crit);
            }
            print_stats(&format!("split_prepare_{label}"), &prep_samples);
            print_stats(&format!("split_critical_{label}"), &crit_samples);
            print_stats(&format!("split_total_{label}"), &total_samples);
        }
    }
}
