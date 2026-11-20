// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use std::net::Shutdown;
use std::os::unix::fs::PermissionsExt as _;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use lancet_node::shadow::compute_shadow_key;
use lancet_protocol::{
    CatalogDigest, EffectKind, ExecuteOutcome, Frame, Labels, OutcomeKind, Receipt, ReceiptLookup,
    ReceiptOutcome, RejectCode, Request, Residue, Response, StepSummary, read_frame, write_frame,
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

#[test]
fn conflict_reset_and_discard_use_distinct_versions() {
    let (root, _home, _cfg, _state, process) = start_node_with(Duration::from_secs(10), |root| {
        let w = root.join("w");
        std::fs::create_dir(&w).unwrap();
        std::fs::write(w.join("doc.txt"), "A\n").unwrap();
        std::fs::write(w.join("other.txt"), "o0\n").unwrap();
        std::fs::write(w.join("gone.txt"), "g0\n").unwrap();
        format!(
            "[[dir]]\nname = \"w\"\npath = {:?}\naccess = \"write\"\n",
            w.to_str().unwrap()
        )
    });
    let host = root.path().join("w");
    let (mut stream, digest) = connect_ready(&process);

    // 基准 A -> 影子写 C（同时对 gone.txt 留草稿）。
    let r1 = run_act(
        &mut stream,
        10,
        "inv-1",
        &digest,
        "w.write(\"doc.txt\", \"C\\n\")\nw.write(\"gone.txt\", \"g_draft\\n\")\n",
    );
    assert!(matches!(r1, ExecuteOutcome::Succeeded { .. }));

    // 宿主改 doc.txt 为 B、改 other.txt 为 o1、删除 gone.txt。
    std::fs::write(host.join("doc.txt"), "B\n").unwrap();
    std::fs::write(host.join("other.txt"), "o1\n").unwrap();
    std::fs::remove_file(host.join("gone.txt")).unwrap();

    // 同步后：status 标出 doc.txt 与 gone.txt 冲突，diff 给出 A -> C，other.txt 正常同步为 o1。
    let r2 = run_act(
        &mut stream,
        11,
        "inv-2",
        &digest,
        "return {\"status\": w.status(), \"diff\": w.diff(), \"other\": w.read(\"other.txt\")}",
    );
    let val2 = match r2 {
        ExecuteOutcome::Succeeded { content, .. } => content["value"].clone(),
        other => panic!("期望成功，实际：{other:?}"),
    };
    assert_eq!(val2["other"], json!("o1\n"));
    assert_eq!(
        val2["status"],
        json!([
            {"path": "doc.txt", "action": "modified", "conflict": true},
            {"path": "gone.txt", "action": "modified", "conflict": true},
        ])
    );
    let diffs = val2["diff"].as_array().unwrap();
    let doc_diff = diffs.iter().find(|d| d["path"] == "doc.txt").unwrap();
    let patch = doc_diff["patch"].as_str().expect("doc.txt 应有 diff");
    assert!(
        patch.contains("-A") && patch.contains("+C") && !patch.contains("-B"),
        "diff 应为基准 A -> 影子 C，实际：\n{patch}"
    );

    // 宿主恢复 doc.txt 为 A：同步后冲突清除，C 仍为待定。
    std::fs::write(host.join("doc.txt"), "A\n").unwrap();
    let r3 = run_act(
        &mut stream,
        12,
        "inv-3",
        &digest,
        "return {\"status\": w.status(), \"doc\": w.read(\"doc.txt\")}",
    );
    let val3 = match r3 {
        ExecuteOutcome::Succeeded { content, .. } => content["value"].clone(),
        other => panic!("期望成功，实际：{other:?}"),
    };
    assert_eq!(val3["doc"], json!("C\n"));
    assert_eq!(
        val3["status"],
        json!([
            {"path": "doc.txt", "action": "modified", "conflict": false},
            {"path": "gone.txt", "action": "modified", "conflict": true},
        ])
    );

    // 宿主再改 doc.txt 为 B2，reset 接受最新宿主版本（含宿主删除的 gone.txt）。
    std::fs::write(host.join("doc.txt"), "B2\n").unwrap();
    let r4 = run_act(
        &mut stream,
        13,
        "inv-4",
        &digest,
        r#"
before = w.status()
w.reset("doc.txt")
w.reset("gone.txt")
w.reset("other.txt")
return {
    "before": before,
    "doc": w.read("doc.txt"),
    "gone": w.read("gone.txt", missing_ok = True),
    "after": w.status(),
}
"#,
    );
    let val4 = match r4 {
        ExecuteOutcome::Succeeded { content, .. } => content["value"].clone(),
        other => panic!("期望成功，实际：{other:?}"),
    };
    assert_eq!(
        val4["before"],
        json!([
            {"path": "doc.txt", "action": "modified", "conflict": true},
            {"path": "gone.txt", "action": "modified", "conflict": true},
        ])
    );
    assert_eq!(val4["doc"], json!("B2\n"));
    assert_eq!(val4["gone"], json!(null));
    assert_eq!(val4["after"], json!([]));

    // 核对回执：无待定改动的 other.txt reset 不产生 effect。
    let rec4 = match query_receipt(&mut stream, 14, "inv-4") {
        ReceiptLookup::Settled { receipt } => receipt,
        other => panic!("期望 Settled：{other:?}"),
    };
    assert_eq!(
        rec4.effects.len(),
        2,
        "应只记录 doc.txt 与 gone.txt 两次 reset"
    );

    // 另一轮：基准已是 B2，影子写 C2，宿主写 B3；discard 先恢复 B2，下次同步再接纳 B3。
    let r5 = run_act(
        &mut stream,
        15,
        "inv-5",
        &digest,
        "w.write(\"doc.txt\", \"C2\\n\")",
    );
    assert!(matches!(r5, ExecuteOutcome::Succeeded { .. }));

    std::fs::write(host.join("doc.txt"), "B3\n").unwrap();
    let r6 = run_act(
        &mut stream,
        16,
        "inv-6",
        &digest,
        r#"
st_before = w.status()
discard()
after_discard = w.read("doc.txt")
st_after = w.status()
return {"before": st_before, "after_discard": after_discard, "st_after": st_after}
"#,
    );
    let val6 = match r6 {
        ExecuteOutcome::Succeeded { content, .. } => content["value"].clone(),
        other => panic!("期望成功，实际：{other:?}"),
    };
    assert_eq!(
        val6["before"],
        json!([{"path": "doc.txt", "action": "modified", "conflict": true}])
    );
    assert_eq!(
        val6["after_discard"],
        json!("B2\n"),
        "discard 应先恢复为基准 B2，尚未接纳宿主 B3"
    );
    assert_eq!(val6["st_after"], json!([]));

    // 下一次调用同步时自动接纳被挡住的宿主版本 B3。
    let r7 = run_act(
        &mut stream,
        17,
        "inv-7",
        &digest,
        "return {\"doc\": w.read(\"doc.txt\"), \"status\": w.status()}",
    );
    let val7 = match r7 {
        ExecuteOutcome::Succeeded { content, .. } => content["value"].clone(),
        other => panic!("期望成功，实际：{other:?}"),
    };
    assert_eq!(val7["doc"], json!("B3\n"));
    assert_eq!(val7["status"], json!([]));

    // 核对非冲突 reset 恢复基准、连带清除保护（目录含待定子项时拒绝 reset），以及原本不存在的 .git 也受保留。
    let r8 = run_act(
        &mut stream,
        18,
        "inv-8",
        &digest,
        r#"
w.write("doc.txt", "temp-draft\n")
w.reset("doc.txt")
doc_after = w.read("doc.txt")
w.write("sub/child.txt", "c\n")
return {"doc_after": doc_after, "status": w.status()}
"#,
    );
    let val8 = match r8 {
        ExecuteOutcome::Succeeded { content, .. } => content["value"].clone(),
        other => panic!("期望成功，实际：{other:?}"),
    };
    assert_eq!(val8["doc_after"], json!("B3\n"));
    assert_eq!(
        val8["status"],
        json!([
            {"path": "sub", "action": "added", "conflict": false},
            {"path": "sub/child.txt", "action": "added", "conflict": false},
        ])
    );

    let r9 = run_act(
        &mut stream,
        19,
        "inv-9-collateral",
        &digest,
        "w.reset(\"sub\")",
    );
    match r9 {
        ExecuteOutcome::Failed { error, .. } => {
            assert_eq!(error.code, "ScriptOrToolError");
            assert!(
                error.message.contains("连带") || error.message.contains("非空"),
                "目录 reset 会连带清除待定子项时应报错，实际：{}",
                error.message
            );
        }
        other => panic!("连带清除待定子项的 reset 应失败，实际：{other:?}"),
    }
    let rec9 = match query_receipt(&mut stream, 20, "inv-9-collateral") {
        ReceiptLookup::Settled { receipt } => receipt,
        other => panic!("期望 Settled：{other:?}"),
    };
    assert!(rec9.effects.is_empty(), "失败的 reset 不应产生 ε");

    // 原本不存在的 .git 入口同样受保留。
    for (idx, git_script) in [
        "w.read(\".git/HEAD\")",
        "w.write(\".git/HEAD\", \"x\")",
        "w.reset(\".git/HEAD\")",
    ]
    .into_iter()
    .enumerate()
    {
        let out = run_act(
            &mut stream,
            21 + idx as u64,
            &format!("inv-absent-git-{idx}"),
            &digest,
            git_script,
        );
        match out {
            ExecuteOutcome::Failed { error, .. } => {
                assert!(
                    error.message.contains("私有 Git 尚不可用"),
                    "`{git_script}` 应报“私有 Git 尚不可用”，实际：{}",
                    error.message
                );
            }
            other => panic!("`{git_script}` 应失败，实际：{other:?}"),
        }
    }

    // 宿主目录全程未被节点改写。
    assert_eq!(
        std::fs::read_to_string(host.join("doc.txt")).unwrap(),
        "B3\n"
    );
    assert_eq!(
        std::fs::read_to_string(host.join("other.txt")).unwrap(),
        "o1\n"
    );
    assert!(!host.join("gone.txt").exists());
}

#[test]
fn write_tools_obey_paths_and_edit_cardinality() {
    let (root, _home, _cfg, state_dir, process) =
        start_node_with(Duration::from_secs(10), |root| {
            let w = root.join("w");
            let ro = root.join("ro");
            std::fs::create_dir_all(w.join("secrets")).unwrap();
            std::fs::create_dir_all(w.join("build")).unwrap();
            std::fs::create_dir_all(w.join(".git")).unwrap();
            std::fs::create_dir_all(ro.join(".git")).unwrap();

            std::fs::write(root.join("outside.txt"), "outside-orig").unwrap();
            std::fs::write(w.join("keep.txt"), "alpha foo beta foo\n").unwrap();
            std::fs::set_permissions(w.join("keep.txt"), std::fs::Permissions::from_mode(0o640))
                .unwrap();
            std::fs::write(w.join("target.txt"), "target0\n").unwrap();
            std::fs::write(w.join("secrets/k.txt"), "topsecret").unwrap();
            std::fs::write(w.join("build/cache.txt"), "seed0").unwrap();
            std::fs::write(w.join("build/secret.txt"), "seed-secret").unwrap();
            std::fs::write(w.join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
            std::fs::write(ro.join(".git/HEAD"), "ref: refs/heads/ro\n").unwrap();

            std::os::unix::fs::symlink("target.txt", w.join("rel_link")).unwrap();
            std::os::unix::fs::symlink("secrets/k.txt", w.join("link_to_secret")).unwrap();
            std::os::unix::fs::symlink(".git/HEAD", w.join("link_to_git")).unwrap();
            std::os::unix::fs::symlink("../outside.txt", w.join("escape_link")).unwrap();

            format!(
                r#"
[[dir]]
name = "w"
path = {w:?}
access = "write"
hidden = [".env", "secrets/", "build/secret.txt"]
regenerable = ["build/", "secrets/"]

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
    let canonical_w = std::fs::canonicalize(&host_w).unwrap();
    let shadow_key = compute_shadow_key(
        &canonical_w,
        &[
            ".env".to_owned(),
            "secrets/".to_owned(),
            "build/secret.txt".to_owned(),
        ],
        &["build/".to_owned(), "secrets/".to_owned()],
    );
    let shadow_tree = state_dir.join("shadows").join(&shadow_key).join("tree");
    let (mut stream, digest) = connect_ready(&process);

    // 1. write 新建（自动建上级目录）、替换（保留权限 0o640）、相对链接、同内容空操作。
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
return {
    "new": w.read("nested/dir/new.txt"),
    "target": w.read("target.txt"),
    "ro_git": ro.read(".git/HEAD"),
    "ro_status": ro.status(),
    "ro_diff": ro.diff(),
}
"#,
    );
    let val1 = match r1 {
        ExecuteOutcome::Succeeded { content, .. } => content["value"].clone(),
        other => panic!("期望成功，实际：{other:?}"),
    };
    assert_eq!(val1["new"], json!("hello\n"));
    assert_eq!(val1["target"], json!("via link\n"));
    assert_eq!(val1["ro_git"], json!("ref: refs/heads/ro\n"));
    assert_eq!(val1["ro_status"], json!([]));
    assert_eq!(val1["ro_diff"], json!([]));

    let mode = std::fs::symlink_metadata(shadow_tree.join("keep.txt"))
        .unwrap()
        .permissions()
        .mode()
        & 0o7777;
    assert_eq!(mode, 0o640, "替换已有普通文件时应保持原权限 0640");

    let rec1 = match query_receipt(&mut stream, 11, "inv-write-ok") {
        ReceiptLookup::Settled { receipt } => receipt,
        other => panic!("期望 Settled：{other:?}"),
    };
    let targets: Vec<&str> = rec1
        .effects
        .iter()
        .flat_map(|e| e.changes().iter().map(|c| c.target.as_str()))
        .collect();
    assert_eq!(
        targets,
        vec!["nested/dir/new.txt", "keep.txt", "target.txt"],
        "同内容第二次 write 不应记入 ε，经链接写入应记录真实目标 target.txt"
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
            std::fs::read_to_string(shadow_tree.join("keep.txt")).unwrap(),
            "one foo two foo\n",
            "`{bad_edit}` 失败前后文件字节不得变化"
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
    match r_edit_ok {
        ExecuteOutcome::Succeeded { content, .. } => {
            assert_eq!(content["value"]["keep"], json!("ONE bar TWO bar\n"));
            assert_eq!(content["value"]["empty"], json!("hello"));
        }
        other => panic!("合法 edit 序列（含空文件空 old）应成功，实际：{other:?}"),
    }

    // 2b. 叶名过长导致 write 失败时，应回滚本次新建的空目录且不记录 ε。
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
        !shadow_tree.join("created").exists(),
        "write 失败后不得残留本次新建的空目录 `created`"
    );
    let rec_long = match query_receipt(&mut stream, 402, "inv-write-nametoolong") {
        ReceiptLookup::Settled { receipt } => receipt,
        other => panic!("期望 Settled：{other:?}"),
    };
    assert!(
        rec_long.effects.is_empty(),
        "回滚后的失败 write 不得产生 ε：{:?}",
        rec_long.effects
    );

    // 3. 种子目录中的写入记 Residue::Regenerable 且 target 仅记命中的规则，不进 status。
    let r_seed = run_act(
        &mut stream,
        41,
        "inv-seed-write",
        &digest,
        "w.write(\"build/cache.txt\", \"seed1\")\nreturn w.read(\"build/cache.txt\")\n",
    );
    assert!(matches!(r_seed, ExecuteOutcome::Succeeded { .. }));
    let rec_seed = match query_receipt(&mut stream, 42, "inv-seed-write") {
        ReceiptLookup::Settled { receipt } => receipt,
        other => panic!("期望 Settled：{other:?}"),
    };
    assert_eq!(rec_seed.effects.len(), 1);
    assert_eq!(rec_seed.effects[0].kind(), EffectKind::Write);
    assert_eq!(rec_seed.effects[0].changes()[0].target, "build/");
    assert_eq!(
        rec_seed.effects[0].changes()[0].residue,
        Residue::Regenerable
    );

    // 4. 越界、hidden（含经链接与同时命中 regenerable 的 secrets/ 以及种子内 build/secret.txt）、保留 .git。
    assert!(
        !shadow_tree.join("build/secret.txt").exists(),
        "种子目录中的 hidden 后代不应复制到影子"
    );
    assert!(
        !shadow_tree.join("secrets").exists(),
        "同时命中 hidden 与 regenerable 的目录应按 hidden 处理，不复制到影子"
    );
    assert!(
        !shadow_tree.join(".git").exists(),
        "可写目录根下的 .git 不应复制到影子"
    );

    let r_list = run_act(
        &mut stream,
        43,
        "inv-list-search",
        &digest,
        "return {\"root\": w.list(\".\"), \"build\": w.list(\"build\"), \"git_hits\": w.search(\"refs\")}",
    );
    let val_ls = match r_list {
        ExecuteOutcome::Succeeded { content, .. } => content["value"].clone(),
        other => panic!("期望成功，实际：{other:?}"),
    };
    let root_names: Vec<&str> = val_ls["root"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["name"].as_str().unwrap())
        .collect();
    assert!(
        !root_names.contains(&".git"),
        "list 应略去 .git：{root_names:?}"
    );
    assert!(
        !root_names.contains(&"secrets"),
        "list 应略去 secrets：{root_names:?}"
    );
    let build_names: Vec<&str> = val_ls["build"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["name"].as_str().unwrap())
        .collect();
    assert_eq!(build_names, vec!["cache.txt"]);
    assert_eq!(val_ls["git_hits"], json!([]));

    let blocked_cases: &[(&str, &str)] = &[
        ("w.write(\"../outside.txt\", \"x\")", ""),
        ("w.write(\"/tmp/abs.txt\", \"x\")", ""),
        ("w.write(\"escape_link\", \"x\")", ""),
        ("w.write(\".env\", \"x\")", "hidden"),
        ("w.write(\"secrets/new.txt\", \"x\")", "hidden"),
        ("w.write(\"link_to_secret\", \"x\")", "hidden"),
        ("w.write(\"build/secret.txt\", \"x\")", "hidden"),
        ("w.read(\".git/HEAD\")", "私有 Git 尚不可用"),
        ("w.read(\"link_to_git\")", "私有 Git 尚不可用"),
        ("w.write(\".git/HEAD\", \"x\")", "私有 Git 尚不可用"),
        ("w.write(\".git/new\", \"x\")", "私有 Git 尚不可用"),
        ("w.reset(\".git/HEAD\")", "私有 Git 尚不可用"),
    ];
    for (idx, (script, needle)) in blocked_cases.iter().enumerate() {
        let inv = format!("inv-blocked-{idx}");
        let res = run_act(&mut stream, 50 + idx as u64, &inv, &digest, script);
        match res {
            ExecuteOutcome::Failed { error, .. } => {
                assert_eq!(error.code, "ScriptOrToolError");
                if !needle.is_empty() {
                    assert!(
                        error.message.contains(needle),
                        "`{script}` 报错应包含 `{needle}`，实际：{}",
                        error.message
                    );
                }
            }
            other => panic!("`{script}` 应被拒绝，实际：{other:?}"),
        }
        let rec = match query_receipt(&mut stream, 100 + idx as u64, &inv) {
            ReceiptLookup::Settled { receipt } => receipt,
            other => panic!("期望 Settled：{other:?}"),
        };
        assert!(rec.effects.is_empty(), "`{script}` 不应产生 ε");
    }

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
        std::fs::read(shadow_tree.join("bad.bin")).unwrap(),
        vec![0xffu8, 0xfe]
    );

    // 宿主文件与树外文件完全未被修改。
    assert_eq!(
        std::fs::read_to_string(root.path().join("outside.txt")).unwrap(),
        "outside-orig"
    );
    assert_eq!(
        std::fs::read_to_string(host_w.join("keep.txt")).unwrap(),
        "alpha foo beta foo\n"
    );
    assert_eq!(
        std::fs::read_to_string(host_w.join("target.txt")).unwrap(),
        "target0\n"
    );
    assert!(!host_w.join("nested").exists());
    assert_eq!(
        std::fs::read_to_string(host_w.join("build/cache.txt")).unwrap(),
        "seed0"
    );
}

#[test]
fn discard_is_immediate() {
    let (_root, _home, _cfg, _state, process) = start_node_with(Duration::from_secs(10), |root| {
        let w1 = root.join("w1");
        let w2 = root.join("w2");
        std::fs::create_dir_all(w1.join("cache")).unwrap();
        std::fs::create_dir_all(&w2).unwrap();
        std::fs::write(w1.join("a.txt"), "base1").unwrap();
        std::fs::write(w1.join("cache/x.txt"), "seed0").unwrap();
        std::fs::write(w2.join("b.txt"), "base2").unwrap();
        format!(
            r#"
[[dir]]
name = "w1"
path = {w1:?}
access = "write"
regenerable = ["cache/"]

[[dir]]
name = "w2"
path = {w2:?}
access = "write"
"#,
            w1 = w1.to_str().unwrap(),
            w2 = w2.to_str().unwrap(),
        )
    });
    let (mut stream, digest) = connect_ready(&process);

    // 跨两次调用在两个可写目录中累积草稿（含种子写入）。
    let r1 = run_act(
        &mut stream,
        10,
        "inv-d1",
        &digest,
        "w1.write(\"a.txt\", \"d1\")\nw1.write(\"cache/x.txt\", \"seed1\")\nw2.write(\"b.txt\", \"d2\")\n",
    );
    match r1 {
        ExecuteOutcome::Succeeded { content, .. } => {
            assert_eq!(
                content["printed"],
                json!("待定文件 2 个。用 commit() 发布或 discard() 丢弃。\n")
            );
        }
        other => panic!("期望成功：{other:?}"),
    }

    let r2 = run_act(
        &mut stream,
        11,
        "inv-d2",
        &digest,
        "w1.write(\"n1.txt\", \"x\")\nw2.write(\"n2.txt\", \"y\")\n",
    );
    match r2 {
        ExecuteOutcome::Succeeded { content, .. } => {
            assert_eq!(
                content["printed"],
                json!("待定文件 4 个。用 commit() 发布或 discard() 丢弃。\n")
            );
        }
        other => panic!("期望成功：{other:?}"),
    }

    // 同一脚本内 discard() 后立即读回基准，种子保留，再写入立即形成新草稿。
    let r3 = run_act(
        &mut stream,
        12,
        "inv-d3",
        &digest,
        r#"
discard()
a_base = w1.read("a.txt")
b_base = w2.read("b.txt")
n1 = w1.read("n1.txt", missing_ok = True)
n2 = w2.read("n2.txt", missing_ok = True)
seed = w1.read("cache/x.txt")
st_empty = [w1.status(), w2.status()]
w1.write("a.txt", "fresh")
return {
    "a_base": a_base,
    "b_base": b_base,
    "n1": n1,
    "n2": n2,
    "seed": seed,
    "st_empty": st_empty,
    "a_fresh": w1.read("a.txt"),
    "st_final": w1.status(),
}
"#,
    );
    match r3 {
        ExecuteOutcome::Succeeded { content, .. } => {
            let val = &content["value"];
            assert_eq!(val["a_base"], json!("base1"));
            assert_eq!(val["b_base"], json!("base2"));
            assert_eq!(val["n1"], json!(null));
            assert_eq!(val["n2"], json!(null));
            assert_eq!(val["seed"], json!("seed1"), "discard 必须保留种子修改");
            assert_eq!(val["st_empty"], json!([[], []]));
            assert_eq!(val["a_fresh"], json!("fresh"));
            assert_eq!(
                val["st_final"],
                json!([{"path": "a.txt", "action": "modified", "conflict": false}])
            );
            assert_eq!(
                content["printed"],
                json!("待定文件 1 个。用 commit() 发布或 discard() 丢弃。\n")
            );
        }
        other => panic!("期望成功：{other:?}"),
    }

    let rec3 = match query_receipt(&mut stream, 13, "inv-d3") {
        ReceiptLookup::Settled { receipt } => receipt,
        other => panic!("期望 Settled：{other:?}"),
    };
    assert_eq!(
        rec3.steps[0],
        StepSummary {
            tool: "discard".to_owned(),
            outcome: OutcomeKind::Succeeded,
        }
    );
    assert!(
        rec3.effects.iter().any(|e| e.handle() == "w1"
            && e.kind() == EffectKind::Write
            && e.changes().iter().any(|c| c.action == "discard")),
        "回执应包含 w1 的 discard 变更：{:?}",
        rec3.effects
    );
    assert!(
        rec3.effects.iter().any(|e| e.handle() == "w2"
            && e.kind() == EffectKind::Write
            && e.changes().iter().any(|c| c.action == "discard")),
        "回执应包含 w2 的 discard 变更：{:?}",
        rec3.effects
    );
}

#[test]
fn waiting_writer_uses_original_deadline() {
    let (root, _home, _cfg, state_dir, process) =
        start_node_with(Duration::from_secs(10), |root| {
            let w = root.join("w");
            std::fs::create_dir(&w).unwrap();
            format!(
                "[[dir]]\nname = \"w\"\npath = {:?}\naccess = \"write\"\n",
                w.to_str().unwrap()
            )
        });
    let canonical_w = std::fs::canonicalize(root.path().join("w")).unwrap();
    let shadow_key = compute_shadow_key(&canonical_w, &[], &[]);
    let hold_marker = state_dir
        .join("shadows")
        .join(&shadow_key)
        .join("tree/hold.txt");
    let (mut s_hold, digest) = connect_ready(&process);
    let (mut s_wait, _) = connect_ready(&process);

    // 1. 持锁调用：先执行写入确认已拿锁，再长时间忙循环占住锁。
    let hold_script = format!("w.write(\"hold.txt\", \"1\")\n{BUSY_SCRIPT}");
    write_frame(
        &mut s_hold,
        &Frame {
            request_id: 10,
            message: exec_req("inv-hold", &digest, &hold_script, 20_000, 65_536),
        },
    )
    .unwrap();
    // 等脚本实际写出 `hold.txt` 标记后，再查询 Pending（既确认已受理也确认已持锁）。
    let hold_deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if std::fs::read_to_string(&hold_marker).ok().as_deref() == Some("1") {
            break;
        }
        assert!(Instant::now() < hold_deadline, "等待持锁标记 hold.txt 超时");
        thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(
        query_receipt(&mut s_wait, 1, "inv-hold"),
        ReceiptLookup::Pending
    );

    // 2. 等锁时用尽自身原始预算（remaining_ms = 120）-> Deadline，无步骤、无 effect。
    let t0 = Instant::now();
    let deadline_out = run_exec(
        &mut s_wait,
        20,
        exec_req(
            "inv-wait-deadline",
            &digest,
            "w.write(\"w1.txt\", \"1\")",
            120,
            65_536,
        ),
    );
    let elapsed = t0.elapsed();
    assert!(
        matches!(deadline_out, ExecuteOutcome::Deadline { .. }),
        "等锁超时应返回 Deadline，实际：{deadline_out:?}"
    );
    assert!(
        elapsed >= Duration::from_millis(100) && elapsed < Duration::from_secs(3),
        "等锁超时耗时应贴近原始预算 120ms，实际：{elapsed:?}"
    );
    let rec_dl = match query_receipt(&mut s_wait, 21, "inv-wait-deadline") {
        ReceiptLookup::Settled { receipt } => receipt,
        other => panic!("期望 Settled：{other:?}"),
    };
    assert!(rec_dl.steps.is_empty());
    assert!(rec_dl.effects.is_empty());

    // 3. 等锁时被显式 Cancel -> Cancelled，无步骤、无 effect。
    write_frame(
        &mut s_wait,
        &Frame {
            request_id: 30,
            message: exec_req(
                "inv-wait-cancel",
                &digest,
                "w.write(\"w2.txt\", \"2\")",
                10_000,
                65_536,
            ),
        },
    )
    .unwrap();
    thread::sleep(Duration::from_millis(30));
    write_frame(
        &mut s_wait,
        &Frame {
            request_id: 31,
            message: Request::Cancel {
                invocation_id: "inv-wait-cancel".to_owned(),
            },
        },
    )
    .unwrap();
    let mut saw_cancel_ok = false;
    let mut cancel_outcome = None;
    for _ in 0..2 {
        let frame: Frame<Response> = read_frame(&mut s_wait).unwrap().unwrap();
        match (frame.request_id, frame.message) {
            (31, Response::CancelOk) => saw_cancel_ok = true,
            (30, Response::ExecuteResult { outcome }) => cancel_outcome = Some(outcome),
            other => panic!("意外帧：{other:?}"),
        }
    }
    assert!(saw_cancel_ok);
    assert!(matches!(
        cancel_outcome,
        Some(ExecuteOutcome::Cancelled { .. })
    ));
    let rec_cancel = match query_receipt(&mut s_wait, 32, "inv-wait-cancel") {
        ReceiptLookup::Settled { receipt } => receipt,
        other => panic!("期望 Settled：{other:?}"),
    };
    assert!(rec_cancel.steps.is_empty());
    assert!(rec_cancel.effects.is_empty());

    // 4. 等锁时连接断开 -> Cancelled，无步骤、无 effect。
    let (mut s_drop, _) = connect_ready(&process);
    write_frame(
        &mut s_drop,
        &Frame {
            request_id: 40,
            message: exec_req(
                "inv-wait-drop",
                &digest,
                "w.write(\"w3.txt\", \"3\")",
                10_000,
                65_536,
            ),
        },
    )
    .unwrap();
    thread::sleep(Duration::from_millis(30));
    let _ = s_drop.shutdown(Shutdown::Both);
    drop(s_drop);

    let mut next_id = 41;
    let rec_drop = wait_settled(
        &mut s_wait,
        &mut next_id,
        "inv-wait-drop",
        Instant::now() + Duration::from_secs(3),
    );
    assert_eq!(rec_drop.outcome, ReceiptOutcome::Cancelled);
    assert!(rec_drop.steps.is_empty());
    assert!(rec_drop.effects.is_empty());

    // 5. 取消持锁调用后，后续调用正常取锁执行。
    let cancel_hold = send_request(
        &mut s_wait,
        next_id,
        Request::Cancel {
            invocation_id: "inv-hold".to_owned(),
        },
    );
    assert!(matches!(cancel_hold.message, Response::CancelOk));
    let hold_resp: Frame<Response> = read_frame(&mut s_hold).unwrap().unwrap();
    assert!(matches!(
        hold_resp.message,
        Response::ExecuteResult {
            outcome: ExecuteOutcome::Cancelled { .. }
        }
    ));

    let after = run_act(
        &mut s_wait,
        next_id + 1,
        "inv-after-unlock",
        &digest,
        r#"
w.write("after.txt", "ok")
return [
    w.read("after.txt"),
    w.read("w1.txt", missing_ok = True),
    w.read("w2.txt", missing_ok = True),
    w.read("w3.txt", missing_ok = True),
]
"#,
    );
    match after {
        ExecuteOutcome::Succeeded { content, .. } => {
            assert_eq!(content["value"], json!(["ok", null, null, null]));
        }
        other => panic!("解锁后应能正常取锁执行，实际：{other:?}"),
    }
}

#[test]
fn pending_hint_respects_compact_json_budget() {
    let (_root, _home, _cfg, _state, process) = start_node_with(Duration::from_secs(10), |root| {
        let w = root.join("w");
        std::fs::create_dir(&w).unwrap();
        format!(
            "[[dir]]\nname = \"w\"\npath = {:?}\naccess = \"write\"\n",
            w.to_str().unwrap()
        )
    });
    let (mut stream, digest) = connect_ready(&process);

    // 1. 低于 84 字节下限以 InvalidRequest 拒绝，且不占用 invocation_id。
    let low_resp = send_request(
        &mut stream,
        10,
        exec_req("inv-low-id", &digest, "return 1", 5_000, 83),
    );
    match low_resp.message {
        Response::Rejected { code, .. } => assert_eq!(code, RejectCode::InvalidRequest),
        other => panic!("output_limit=83 应以 InvalidRequest 拒绝，实际：{other:?}"),
    }

    // 原有优先级：摘要错误优先于 output_limit < 84。
    let wrong_digest = CatalogDigest::from_sha256_bytes([0x44; 32]);
    let bad_dig_resp = send_request(
        &mut stream,
        11,
        exec_req("inv-low-id", &wrong_digest, "return 1", 5_000, 10),
    );
    match bad_dig_resp.message {
        Response::Rejected { code, .. } => assert_eq!(code, RejectCode::CatalogMismatch),
        other => panic!("摘要不匹配应优先于 output_limit < 84，实际：{other:?}"),
    }

    // 同一 inv-low-id 改用合法上限后成功执行（证明此前拒绝未登记 id）。
    let reuse_ok = run_exec(
        &mut stream,
        12,
        exec_req("inv-low-id", &digest, "return 1", 5_000, 256),
    );
    assert!(matches!(reuse_ok, ExecuteOutcome::Succeeded { .. }));

    // 已登记的 id 即使 output_limit < 84 仍优先返回 DuplicateInvocation。
    let dup_resp = send_request(
        &mut stream,
        13,
        exec_req("inv-low-id", &digest, "return 1", 5_000, 10),
    );
    match dup_resp.message {
        Response::Rejected { code, .. } => assert_eq!(code, RejectCode::DuplicateInvocation),
        other => panic!("重复 id 应优先于 output_limit < 84，实际：{other:?}"),
    }

    // 2. 多字节文本 + 换行转义：先在没有待定改动时按 output_limit - 84 核对。
    let mb_script = "print(\"中\\n文\")\nreturn \"汉\\n字\"";
    let base_content = json!({
        "value": "汉\n字",
        "printed": "中\n文\n",
    });
    let payload_bytes = serde_json::to_vec(&base_content).unwrap().len() as u64;

    let exact_no_pending = run_exec(
        &mut stream,
        20,
        exec_req("inv-exact-0", &digest, mb_script, 5_000, payload_bytes + 84),
    );
    match exact_no_pending {
        ExecuteOutcome::Succeeded { content, .. } => {
            assert_eq!(content["value"], json!("汉\n字"));
            assert_eq!(content["printed"], json!("中\n文\n"));
        }
        other => panic!("无待定且恰好等于 output_limit - 84 应成功，实际：{other:?}"),
    }

    let over_no_pending = run_exec(
        &mut stream,
        21,
        exec_req("inv-over-0", &digest, mb_script, 5_000, payload_bytes + 83),
    );
    match over_no_pending {
        ExecuteOutcome::Failed { error, .. } => {
            assert_eq!(error.code, "OutputLimitExceeded");
            assert!(
                !error.message.contains("待定文件"),
                "无待定改动时失败消息不应附带提示：{}",
                error.message
            );
        }
        other => panic!("多 1 字节时即使无待定改动也应报 OutputLimitExceeded，实际：{other:?}"),
    }

    // 3. 制造 1 个待定文件后再测边界与 Failed 末尾提示。
    let mk = run_act(
        &mut stream,
        22,
        "inv-mk-pending",
        &digest,
        "w.write(\"p.txt\", \"draft\")",
    );
    assert!(matches!(mk, ExecuteOutcome::Succeeded { .. }));

    let hint = "待定文件 1 个。用 commit() 发布或 discard() 丢弃。\n";
    let exact_with_pending = run_exec(
        &mut stream,
        23,
        exec_req("inv-exact-1", &digest, mb_script, 5_000, payload_bytes + 84),
    );
    match exact_with_pending {
        ExecuteOutcome::Succeeded { content, .. } => {
            assert_eq!(content["printed"], json!(format!("中\n文\n{hint}")));
            let final_bytes = serde_json::to_vec(&content).unwrap().len() as u64;
            assert!(
                final_bytes <= payload_bytes + 84,
                "追加提示后的最终紧凑 JSON 字节数 {final_bytes} 不应超过 output_limit {}",
                payload_bytes + 84
            );
        }
        other => panic!("有待定且原载荷恰好等于 output_limit - 84 应成功，实际：{other:?}"),
    }

    let over_with_pending = run_exec(
        &mut stream,
        24,
        exec_req("inv-over-1", &digest, mb_script, 5_000, payload_bytes + 83),
    );
    match over_with_pending {
        ExecuteOutcome::Failed { error, .. } => {
            assert_eq!(error.code, "OutputLimitExceeded");
            assert!(
                error.message.ends_with(&format!("\n{hint}")),
                "OutputLimitExceeded 末尾应另起一行附提示，实际：{:?}",
                error.message
            );
        }
        other => panic!("多 1 字节应报 OutputLimitExceeded，实际：{other:?}"),
    }

    // 4. ScriptOrToolError（含普通 fail 与 16 KiB 过长诊断截断）均保留完整待定提示。
    let fail_short = run_act(&mut stream, 25, "inv-fail-short", &digest, "fail(\"boom\")");
    match fail_short {
        ExecuteOutcome::Failed { error, .. } => {
            assert_eq!(error.code, "ScriptOrToolError");
            assert!(
                error.message.ends_with(&format!("\n{hint}")),
                "fail 诊断末尾应附待定提示：{:?}",
                error.message
            );
        }
        other => panic!("期望 Failed：{other:?}"),
    }

    let fail_long = run_act(
        &mut stream,
        26,
        "inv-fail-long",
        &digest,
        "fail(\"x\" * 100000)",
    );
    match fail_long {
        ExecuteOutcome::Failed { error, .. } => {
            assert_eq!(error.code, "ScriptOrToolError");
            let marker = "\n（诊断过长，已截断）";
            assert!(
                error.message.contains(marker),
                "过长诊断应被截断：len={}",
                error.message.len()
            );
            assert!(
                error.message.ends_with(&format!("\n{hint}")),
                "过长诊断截断后必须完整保留待定提示，末尾：{:?}",
                &error.message[error.message.len().saturating_sub(96)..]
            );
            assert!(
                error.message.len() <= 16_384 + marker.len(),
                "16 KiB 截断应为待定提示留出位置，实际长度：{}",
                error.message.len()
            );
        }
        other => panic!("期望 Failed：{other:?}"),
    }

    // 5. commit() 发布后待定清零，不再追加待定提示。
    let committed = run_act(
        &mut stream,
        27,
        "inv-commit-clears-hint",
        &digest,
        "commit()\nprint(\"done\")\nreturn 1",
    );
    match committed {
        ExecuteOutcome::Succeeded { content, .. } => {
            assert_eq!(
                content["printed"],
                json!("done\n"),
                "commit() 发布清空待定后不应再附带待定提示"
            );
        }
        other => panic!("commit() 发布应成功，实际：{other:?}"),
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
    let host_w = root.path().join("w");
    let canonical_w = std::fs::canonicalize(&host_w).unwrap();
    let shadow_key = compute_shadow_key(&canonical_w, &[], &[]);
    let shadow_a = state_dir
        .join("shadows")
        .join(&shadow_key)
        .join("tree/a.txt");

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

    // 等写入落到影子且调用仍在 Pending 时杀掉节点进程。
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if std::fs::read_to_string(&shadow_a).ok().as_deref() == Some("before-kill") {
            break;
        }
        assert!(Instant::now() < deadline, "等待写入落盘超时");
        thread::sleep(Duration::from_millis(10));
    }
    // 保持连接打开，先杀节点进程并等待退出，避免先关连接触发 Cancelled。
    drop(process);
    drop(s1);

    // 重启节点；在首个 hello 之前检查账本文件已结算为 Indeterminate。
    let toml_content = std::fs::read_to_string(&config_path).unwrap();
    let restarted = NodeProcess::start(&config_path, &toml_content, &state_dir, &home);

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
                    reason.contains("节点在调用结算前重启") && reason.contains("status()"),
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

    // 影子中的草稿仍可通过 status() 与 read() 查到，宿主仍为 base。
    let check = run_act(
        &mut s2,
        13,
        "inv-check-after-restart",
        &digest2,
        "return {\"status\": w.status(), \"a\": w.read(\"a.txt\")}",
    );
    match check {
        ExecuteOutcome::Succeeded { content, .. } => {
            assert_eq!(
                content["value"]["status"],
                json!([{"path": "a.txt", "action": "modified", "conflict": false}])
            );
            assert_eq!(content["value"]["a"], json!("before-kill"));
        }
        other => panic!("重启后应可查影子草稿，实际：{other:?}"),
    }
    assert_eq!(
        std::fs::read_to_string(host_w.join("a.txt")).unwrap(),
        "base"
    );
}

#[test]
#[ignore = "100,000 文件 1,000 目录可写工作区空 act 端到端时延基准；需在 release 下手动运行"]
fn measure_empty_act_100k() {
    let (root, home) = temp_env();
    let host_w = root.path().join("w100k");
    let state_dir = root.path().join("state");
    std::fs::create_dir(&host_w).expect("创建 100k 宿主目录失败");

    // 1,000 个目录 × 100 个文件 = 100,000 个常规文件，不含 .git。
    let t_prep = Instant::now();
    thread::scope(|scope| {
        let workers = 8usize;
        for worker in 0..workers {
            let host_w = &host_w;
            scope.spawn(move || {
                for d in (worker..1_000).step_by(workers) {
                    let dir = host_w.join(format!("d{d:03}"));
                    std::fs::create_dir(&dir).expect("创建子目录失败");
                    for f in 0..100 {
                        let p = dir.join(format!("f{f:02}.txt"));
                        std::fs::write(&p, b"line\n").expect("写入基准文件失败");
                    }
                }
            });
        }
    });
    let prep_elapsed = t_prep.elapsed();

    let config_path = root.path().join("node.toml");
    let toml_content = format!(
        "[node]\ndisplay_name = \"100k 基准节点\"\nstate_dir = {state_dir:?}\n\n[[dir]]\nname = \"w\"\npath = {w:?}\naccess = \"write\"\n",
        state_dir = state_dir.to_str().unwrap(),
        w = host_w.to_str().unwrap(),
    );

    let t_start = Instant::now();
    let process = NodeProcess::start_with_timeout(
        &config_path,
        &toml_content,
        &state_dir,
        &home,
        Duration::from_secs(180),
    );
    let init_elapsed = t_start.elapsed();

    let (mut stream, digest) = connect_ready(&process);

    // 预热（不计时）。
    for i in 0..3 {
        let out = run_act(
            &mut stream,
            10 + i,
            &format!("warmup-{i}"),
            &digest,
            "return 1",
        );
        assert!(matches!(out, ExecuteOutcome::Succeeded { .. }));
    }

    // 同一连接、不同 invocation_id 连续实测 11 次。
    let mut samples = Vec::with_capacity(11);
    for i in 0..11u64 {
        let req = exec_req(&format!("bench-{i}"), &digest, "return 1", 10_000, 65_536);
        let t0 = Instant::now();
        let out = run_exec(&mut stream, 100 + i, req);
        let dt = t0.elapsed();
        match out {
            ExecuteOutcome::Succeeded { content, .. } => {
                assert_eq!(content["value"], json!(1));
            }
            other => panic!("第 {i} 次基准执行失败：{other:?}"),
        }
        samples.push(dt);
    }

    let mut sorted = samples.clone();
    sorted.sort_unstable();
    let median = sorted[sorted.len() / 2];

    let sample_ms: Vec<String> = samples
        .iter()
        .map(|d| format!("{:.3}", d.as_secs_f64() * 1_000.0))
        .collect();
    println!(
        "measure_empty_act_100k: prep={:.2}s, node_init={:.2}s",
        prep_elapsed.as_secs_f64(),
        init_elapsed.as_secs_f64(),
    );
    println!("samples_ms: [{}]", sample_ms.join(", "));
    println!("median_ms: {:.3}", median.as_secs_f64() * 1_000.0);
}
