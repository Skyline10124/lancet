// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::io::Read as _;
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use lancet_protocol::{
    CatalogDigest, ExecuteOutcome, Frame, FrameError, Labels, MAX_FRAME_BYTES, MAX_OUTPUT_LIMIT,
    OutcomeKind, Receipt, ReceiptLookup, ReceiptOutcome, RejectCode, Request, Response, read_frame,
    write_frame,
};
use serde_json::json;
use tempfile::TempDir;

use super::{HelloInfo, NodeProcess, describe, hello_v1, temp_env};

const WAIT_TIMEOUT: Duration = Duration::from_secs(10);

const OOM_SCRIPT: &str = r#"
chunk = "x" * 1048576
chunks = []
for i in range(200):
    chunks.append(chunk + str(i))
return len(chunks)
"#;

const BUSY_SCRIPT: &str = r#"
acc = 0
for i in range(100000):
    for j in range(100000):
        acc = (acc + i + j) % 97
return acc
"#;

struct TestNode {
    _root: TempDir,
    home: PathBuf,
    config_path: PathBuf,
    state_dir: PathBuf,
    process: Option<NodeProcess>,
}

impl TestNode {
    fn start(retention_ms: Option<u64>) -> Self {
        let (root, home) = temp_env();
        let app_dir = root.path().join("app");
        let state_dir = root.path().join("state");
        std::fs::create_dir(&app_dir).expect("创建 app 目录失败");

        let retention_line = match retention_ms {
            Some(ms) => format!("receipt_retention_ms = {ms}\n"),
            None => String::new(),
        };
        let config_path = root.path().join("node.toml");
        let toml_content = format!(
            "[node]\ndisplay_name = \"调用测试节点\"\nstate_dir = {state_dir:?}\n{retention_line}\n[[dir]]\nname = \"app\"\npath = {app_dir:?}\naccess = \"read\"\n",
            state_dir = state_dir.to_str().unwrap(),
            app_dir = app_dir.to_str().unwrap(),
        );
        let process = NodeProcess::start(&config_path, &toml_content, &state_dir, &home);
        Self {
            _root: root,
            home,
            config_path,
            state_dir,
            process: Some(process),
        }
    }

    fn start_custom(dir_defs: impl FnOnce(&Path) -> String) -> (TempDir, PathBuf, NodeProcess) {
        let (root, home) = temp_env();
        let state_dir = root.path().join("state");
        let config_path = root.path().join("node.toml");
        let dirs_toml = dir_defs(root.path());
        let toml_content = format!(
            "[node]\ndisplay_name = \"只读工具测试节点\"\nstate_dir = {state_dir:?}\n\n{dirs_toml}",
            state_dir = state_dir.to_str().unwrap(),
        );
        let process = NodeProcess::start(&config_path, &toml_content, &state_dir, &home);
        (root, state_dir, process)
    }

    fn connect_hello(&self) -> (UnixStream, HelloInfo, CatalogDigest) {
        let mut stream = self.process.as_ref().expect("节点应正在运行").connect();
        stream
            .set_read_timeout(Some(WAIT_TIMEOUT))
            .expect("设置读取超时失败");
        stream
            .set_write_timeout(Some(WAIT_TIMEOUT))
            .expect("设置写入超时失败");
        let hello = hello_v1(&mut stream, 1);
        let (digest, _, _) = describe(&mut stream, 2);
        (stream, hello, digest)
    }

    fn restart(&mut self) {
        drop(self.process.take());
        let toml_content = std::fs::read_to_string(&self.config_path).expect("读取配置文件失败");
        self.process = Some(NodeProcess::start(
            &self.config_path,
            &toml_content,
            &self.state_dir,
            &self.home,
        ));
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

fn send_expect_id(stream: &mut UnixStream, request_id: u64, message: Request) -> Response {
    let resp = super::send_request(stream, request_id, message);
    assert_eq!(resp.request_id, request_id, "回复 request_id 应与请求一致");
    resp.message
}

fn execute_req(
    invocation_id: &str,
    catalog_digest: &CatalogDigest,
    tool: &str,
    arguments: serde_json::Value,
    remaining_ms: u64,
    output_limit: u64,
    labels: Labels,
) -> Request {
    Request::Execute {
        invocation_id: invocation_id.to_owned(),
        catalog_digest: catalog_digest.clone(),
        tool: tool.to_owned(),
        arguments,
        remaining_ms,
        output_limit,
        labels,
    }
}

fn run_execute(stream: &mut UnixStream, request_id: u64, req: Request) -> ExecuteOutcome {
    match send_expect_id(stream, request_id, req) {
        Response::ExecuteResult { outcome } => outcome,
        other => panic!("期望 ExecuteResult，实际为：{other:?}"),
    }
}

fn run_act(
    stream: &mut UnixStream,
    request_id: u64,
    invocation_id: &str,
    digest: &CatalogDigest,
    script: &str,
) -> ExecuteOutcome {
    let req = execute_req(
        invocation_id,
        digest,
        "act",
        json!({ "script": script }),
        5_000,
        65_536,
        Labels::new(),
    );
    run_execute(stream, request_id, req)
}

fn send_rejected(stream: &mut UnixStream, request_id: u64, req: Request) -> (RejectCode, String) {
    match send_expect_id(stream, request_id, req) {
        Response::Rejected { code, message } => (code, message),
        other => panic!("期望 Rejected，实际为：{other:?}"),
    }
}

fn query_receipt(stream: &mut UnixStream, request_id: u64, invocation_id: &str) -> ReceiptLookup {
    match send_expect_id(
        stream,
        request_id,
        Request::Receipt {
            invocation_id: invocation_id.to_owned(),
        },
    ) {
        Response::ReceiptResult { lookup } => lookup,
        other => panic!("期望 ReceiptResult，实际为：{other:?}"),
    }
}

fn send_cancel(stream: &mut UnixStream, request_id: u64, invocation_id: &str) {
    match send_expect_id(
        stream,
        request_id,
        Request::Cancel {
            invocation_id: invocation_id.to_owned(),
        },
    ) {
        Response::CancelOk => {}
        other => panic!("期望 CancelOk，实际为：{other:?}"),
    }
}

fn wait_for_settled(
    stream: &mut UnixStream,
    next_req_id: &mut u64,
    invocation_id: &str,
    deadline: Instant,
) -> Receipt {
    loop {
        let req_id = *next_req_id;
        *next_req_id += 1;
        match query_receipt(stream, req_id, invocation_id) {
            ReceiptLookup::Settled { receipt } => {
                assert!(
                    Instant::now() < deadline,
                    "调用 `{invocation_id}` 在宽限时限内未结算"
                );
                return receipt;
            }
            ReceiptLookup::Pending => {
                assert!(
                    Instant::now() < deadline,
                    "调用 `{invocation_id}` 在宽限时限内未结算"
                );
                thread::sleep(Duration::from_millis(5));
            }
            other => panic!("等待 `{invocation_id}` 结算时收到意外状态：{other:?}"),
        }
    }
}

#[test]
fn admission_preserves_invocation_records() {
    let node = TestNode::start(None);
    let (mut stream, _, digest) = node.connect_hello();
    let wrong_digest = CatalogDigest::from_sha256_bytes([0x99; 32]);

    // 先成功结算一个调用，保存原回执。
    let outcome = run_act(&mut stream, 10, "inv-settled", &digest, "return 42\n");
    assert!(matches!(outcome, ExecuteOutcome::Succeeded { .. }));
    let orig_receipt = match query_receipt(&mut stream, 11, "inv-settled") {
        ReceiptLookup::Settled { receipt } => receipt,
        other => panic!("期望 Settled 回执，实际：{other:?}"),
    };

    // 同 id 再次投递：DuplicateInvocation，且优先于 CatalogMismatch 与 InvalidRequest。
    let (code, _) = send_rejected(
        &mut stream,
        12,
        execute_req(
            "inv-settled",
            &digest,
            "act",
            json!({"script": "return 1"}),
            5_000,
            65_536,
            Labels::new(),
        ),
    );
    assert_eq!(code, RejectCode::DuplicateInvocation);

    let (code, _) = send_rejected(
        &mut stream,
        13,
        execute_req(
            "inv-settled",
            &wrong_digest,
            "other",
            json!([]),
            5_000,
            MAX_OUTPUT_LIMIT + 1,
            Labels::new(),
        ),
    );
    assert_eq!(
        code,
        RejectCode::DuplicateInvocation,
        "重复 id 与错误摘要同时出现时应返回 DuplicateInvocation"
    );

    assert_eq!(
        query_receipt(&mut stream, 14, "inv-settled"),
        ReceiptLookup::Settled {
            receipt: orig_receipt
        },
        "被拒绝的重放投递不得改变已结算回执"
    );

    // 错误摘要优先于非法参数。
    let (code, _) = send_rejected(
        &mut stream,
        15,
        execute_req(
            "inv-fresh",
            &wrong_digest,
            "act",
            json!({"script": "return 1"}),
            5_000,
            65_536,
            Labels::new(),
        ),
    );
    assert_eq!(code, RejectCode::CatalogMismatch);

    let (code, _) = send_rejected(
        &mut stream,
        16,
        execute_req(
            "inv-fresh",
            &wrong_digest,
            "other",
            json!([]),
            5_000,
            MAX_OUTPUT_LIMIT + 1,
            Labels::new(),
        ),
    );
    assert_eq!(
        code,
        RejectCode::CatalogMismatch,
        "错误摘要与非法参数同时出现时应返回 CatalogMismatch"
    );

    // 各种 InvalidRequest 情形。
    let invalid_cases = [
        ("未知工具", "other", json!({"script": "return 1"}), 65_536),
        ("缺 script", "act", json!({}), 65_536),
        ("arguments 为数组", "act", json!(["return 1"]), 65_536),
        ("script 非字符串", "act", json!({"script": 123}), 65_536),
        (
            "多余键",
            "act",
            json!({"script": "return 1", "extra": 1}),
            65_536,
        ),
        (
            "output_limit 超限",
            "act",
            json!({"script": "return 1"}),
            MAX_OUTPUT_LIMIT + 1,
        ),
    ];

    for (idx, (case_name, tool, args, limit)) in invalid_cases.into_iter().enumerate() {
        let (code, _) = send_rejected(
            &mut stream,
            20 + idx as u64,
            execute_req(
                "inv-fresh",
                &digest,
                tool,
                args,
                5_000,
                limit,
                Labels::new(),
            ),
        );
        assert_eq!(
            code,
            RejectCode::InvalidRequest,
            "用例 `{case_name}` 应返回 InvalidRequest"
        );
    }

    // 上述拒绝均未新建记录，用同一个 id 正确投递仍可受理并成功。
    let ok = run_execute(
        &mut stream,
        30,
        execute_req(
            "inv-fresh",
            &digest,
            "act",
            json!({ "script": "return 99\n" }),
            5_000,
            MAX_OUTPUT_LIMIT,
            Labels::new(),
        ),
    );
    match ok {
        ExecuteOutcome::Succeeded { content, .. } => {
            assert_eq!(content["value"], json!(99));
        }
        other => panic!("被拒后同一 id 正确投递应成功，实际：{other:?}"),
    }
}

#[test]
fn fence_survives_restart() {
    let mut node = TestNode::start(None);
    let (mut stream, _, digest) = node.connect_hello();

    assert_eq!(
        query_receipt(&mut stream, 10, "inv-fence"),
        ReceiptLookup::NeverExecuted
    );
    let (code, _) = send_rejected(
        &mut stream,
        11,
        execute_req(
            "inv-fence",
            &digest,
            "act",
            json!({"script": "return 1"}),
            5_000,
            65_536,
            Labels::new(),
        ),
    );
    assert_eq!(code, RejectCode::DuplicateInvocation);

    drop(stream);
    node.restart();
    let (mut stream, _, digest) = node.connect_hello();

    let (code, _) = send_rejected(
        &mut stream,
        10,
        execute_req(
            "inv-fence",
            &digest,
            "act",
            json!({"script": "return 1"}),
            5_000,
            65_536,
            Labels::new(),
        ),
    );
    assert_eq!(
        code,
        RejectCode::DuplicateInvocation,
        "重启后先对已围栏 id 发 execute 应立即返回 DuplicateInvocation"
    );
    assert_eq!(
        query_receipt(&mut stream, 11, "inv-fence"),
        ReceiptLookup::NeverExecuted,
        "重启后围栏记录仍应返回 NeverExecuted"
    );
}

#[test]
fn execute_returns_content_at_byte_limit() {
    let node = TestNode::start(None);
    let (mut stream, _, digest) = node.connect_hello();

    let script = r#"
print("第一行 \"引号\" \\ 文本")
print("第二行中文")
return {"nested": {"nums": [1, 2], "msg": "多字节\"测试\"\n换行"}}
"#;
    let labels = BTreeMap::from([
        ("session".to_owned(), json!("s-9")),
        ("meta".to_owned(), json!({"turn": 4, "tags": ["a", null]})),
    ]);

    let act_with = |id: &str, limit: u64| {
        execute_req(
            id,
            &digest,
            "act",
            json!({ "script": script }),
            5_000,
            limit,
            labels.clone(),
        )
    };

    let base_outcome = run_execute(&mut stream, 10, act_with("inv-limit-base", 65_536));
    let content = match &base_outcome {
        ExecuteOutcome::Succeeded { content, .. } => content.clone(),
        other => panic!("期望 Succeeded，实际：{other:?}"),
    };
    let obj = content.as_object().expect("content 应为 JSON 对象");
    assert_eq!(obj.len(), 2, "content 应恰好包含 value 与 printed 两个键");
    assert_eq!(
        obj["value"],
        json!({"nested": {"nums": [1, 2], "msg": "多字节\"测试\"\n换行"}})
    );
    assert_eq!(
        obj["printed"],
        json!("第一行 \"引号\" \\ 文本\n第二行中文\n")
    );

    let exact_bytes = serde_json::to_vec(&content)
        .expect("序列化 content 失败")
        .len() as u64;

    let exact_outcome = run_execute(&mut stream, 11, act_with("inv-limit-exact", exact_bytes));
    assert_eq!(exact_outcome, base_outcome, "恰好等于上限时应成功");
    let exact_receipt = match query_receipt(&mut stream, 12, "inv-limit-exact") {
        ReceiptLookup::Settled { receipt } => receipt,
        other => panic!("期望 Settled，实际：{other:?}"),
    };
    assert_eq!(exact_receipt.labels, labels, "回执应原样保留 labels");
    assert_eq!(exact_receipt.outcome, ReceiptOutcome::Succeeded { content });

    let over_outcome = run_execute(&mut stream, 13, act_with("inv-limit-over", exact_bytes - 1));
    let (exec_err, exec_summary) = match over_outcome {
        ExecuteOutcome::Failed {
            error,
            receipt_summary,
        } => (error, receipt_summary),
        other => panic!("少 1 字节应返回 Failed(OutputLimitExceeded)，实际：{other:?}"),
    };
    assert_eq!(exec_err.code, "OutputLimitExceeded");
    assert!(
        exec_err.message.contains(&exact_bytes.to_string())
            && exec_err.message.contains(&(exact_bytes - 1).to_string()),
        "错误消息应写明实际字节数与上限，实际：{}",
        exec_err.message
    );
    assert!(
        exec_summary.is_some(),
        "Failed 应附带 Some(receipt_summary)"
    );

    let over_receipt = match query_receipt(&mut stream, 14, "inv-limit-over") {
        ReceiptLookup::Settled { receipt } => receipt,
        other => panic!("期望 Settled，实际：{other:?}"),
    };
    assert_eq!(over_receipt.labels, labels);
    assert_eq!(
        over_receipt.outcome,
        ReceiptOutcome::Failed { error: exec_err },
        "执行回复与 receipt 查询的结局应一致"
    );
}

#[test]
fn execute_reports_script_diagnostics_before_evaluation() {
    let node = TestNode::start(None);
    let (mut stream, _, digest) = node.connect_hello();

    let static_cases = [
        ("未定义名称", "fail(\"执行哨兵\")\nx = missing_ident\n"),
        (
            "静态类型错误",
            "fail(\"执行哨兵\")\nx = sorted([2, 1], revers = True)\n",
        ),
    ];
    for (idx, (name, script)) in static_cases.into_iter().enumerate() {
        let outcome = run_act(
            &mut stream,
            10 + idx as u64,
            &format!("inv-static-{idx}"),
            &digest,
            script,
        );
        match outcome {
            ExecuteOutcome::Failed {
                error,
                receipt_summary,
            } => {
                assert!(receipt_summary.is_some());
                assert_eq!(error.code, "ScriptOrToolError");
                assert!(
                    error.message.contains("check") && error.message.contains("第 2 行"),
                    "用例 `{name}` 应报告 check 阶段与第 2 行，实际：{}",
                    error.message
                );
                assert!(
                    !error.message.contains("执行哨兵"),
                    "用例 `{name}` 不得执行首行哨兵，实际：{}",
                    error.message
                );
            }
            other => panic!("用例 `{name}` 期望 Failed，实际：{other:?}"),
        }
    }

    let sentinel = run_act(
        &mut stream,
        20,
        "inv-sentinel",
        &digest,
        "fail(\"执行哨兵\")\n",
    );
    match sentinel {
        ExecuteOutcome::Failed { error, .. } => {
            assert_eq!(error.code, "ScriptOrToolError");
            assert!(
                error.message.contains("eval") && error.message.contains("执行哨兵"),
                "去掉静态错误后应触发执行哨兵，实际：{}",
                error.message
            );
        }
        other => panic!("期望 Failed，实际：{other:?}"),
    }

    let parse_err = run_act(
        &mut stream,
        21,
        "inv-parse",
        &digest,
        "x = None\nif x is None:\n    return 1\n",
    );
    match parse_err {
        ExecuteOutcome::Failed { error, .. } => {
            assert_eq!(error.code, "ScriptOrToolError");
            assert!(
                error.message.contains("parse") && error.message.contains("== None"),
                "解析错误消息应包含阶段与修复提示，实际：{}",
                error.message
            );
        }
        other => panic!("期望 Failed，实际：{other:?}"),
    }

    let long_diag = run_act(
        &mut stream,
        22,
        "inv-long-diag",
        &digest,
        "fail(\"x\" * 100000)\n",
    );
    match long_diag {
        ExecuteOutcome::Failed {
            error,
            receipt_summary,
        } => {
            assert!(receipt_summary.is_some());
            assert_eq!(error.code, "ScriptOrToolError");
            let marker = "\n（诊断过长，已截断）";
            assert!(
                error.message.len() <= 16_384 + marker.len(),
                "过长诊断截断后长度 {} 不应超过 {}",
                error.message.len(),
                16_384 + marker.len()
            );
            assert!(
                error.message.ends_with(marker),
                "过长诊断应以截断标记结尾，实际末尾：{:?}",
                &error.message[error.message.len().saturating_sub(64)..]
            );
            let stored = match query_receipt(&mut stream, 23, "inv-long-diag") {
                ReceiptLookup::Settled { receipt } => receipt,
                other => panic!("期望 Settled 回执，实际：{other:?}"),
            };
            assert_eq!(
                stored.outcome,
                ReceiptOutcome::Failed { error },
                "回执与 execute_result 应共用已截断的错误消息"
            );
        }
        other => panic!("过长 fail 诊断应返回 Failed(ScriptOrToolError)，实际：{other:?}"),
    }
}

#[test]
fn memory_limit_leaves_node_serving() {
    let node = TestNode::start(None);
    let (mut stream, _, digest) = node.connect_hello();

    let oom = run_execute(
        &mut stream,
        10,
        execute_req(
            "inv-oom",
            &digest,
            "act",
            json!({ "script": OOM_SCRIPT }),
            10_000,
            65_536,
            Labels::new(),
        ),
    );
    match oom {
        ExecuteOutcome::Failed {
            error,
            receipt_summary,
        } => {
            assert!(receipt_summary.is_some());
            assert_eq!(error.code, "ScriptOrToolError");
            assert!(
                error.message.contains("134217728"),
                "超内存错误应包含内存上限字节数 134217728，实际：{}",
                error.message
            );
        }
        other => panic!("超内存应返回 Failed(ScriptOrToolError)，实际：{other:?}"),
    }

    let healthy = run_act(
        &mut stream,
        11,
        "inv-after-oom",
        &digest,
        "return sum(range(10))\n",
    );
    match healthy {
        ExecuteOutcome::Succeeded { content, .. } => {
            assert_eq!(content["value"], json!(45));
        }
        other => panic!("超内存后同一节点应继续正常服务，实际：{other:?}"),
    }
}

#[test]
fn deadline_settles_within_advertised_grace() {
    let node = TestNode::start(None);
    let (mut stream, hello, digest) = node.connect_hello();

    let budget_ms = 200;
    let started = Instant::now();
    let outcome = run_execute(
        &mut stream,
        10,
        execute_req(
            "inv-deadline",
            &digest,
            "act",
            json!({ "script": BUSY_SCRIPT }),
            budget_ms,
            65_536,
            Labels::new(),
        ),
    );
    assert!(
        matches!(
            outcome,
            ExecuteOutcome::Deadline {
                receipt_summary: Some(_)
            }
        ),
        "超时应返回 Deadline，实际：{outcome:?}"
    );

    let max_wait = Duration::from_millis(budget_ms + hello.capabilities.settle_grace_ms);
    let mut req_id = 11;
    let receipt = wait_for_settled(&mut stream, &mut req_id, "inv-deadline", started + max_wait);
    let elapsed = started.elapsed();
    assert_eq!(receipt.outcome, ReceiptOutcome::Deadline);
    assert!(
        elapsed < max_wait,
        "超时结算耗时 {elapsed:?} 应小于预算加 settle_grace_ms ({max_wait:?})"
    );
    eprintln!("deadline_settle_elapsed_ms={}", elapsed.as_millis());

    for idx in 0..20 {
        let zero_outcome = run_execute(
            &mut stream,
            100 + idx,
            execute_req(
                &format!("inv-zero-budget-{idx}"),
                &digest,
                "act",
                json!({ "script": "return 1\n" }),
                0,
                65_536,
                Labels::new(),
            ),
        );
        assert!(
            matches!(
                zero_outcome,
                ExecuteOutcome::Deadline {
                    receipt_summary: Some(_)
                }
            ),
            "remaining_ms = 0 第 {idx} 次应返回 Deadline，实际：{zero_outcome:?}"
        );
    }
}

#[test]
fn cancel_multiplexes_with_receipt() {
    let node = TestNode::start(None);
    let (mut stream, hello, digest) = node.connect_hello();

    // 1. 发忙循环 execute
    send_only(
        &mut stream,
        10,
        execute_req(
            "inv-cancel",
            &digest,
            "act",
            json!({"script": BUSY_SCRIPT}),
            30_000,
            65_536,
            Labels::new(),
        ),
    );
    // 2. 发 receipt，得 Pending
    assert_eq!(
        query_receipt(&mut stream, 11, "inv-cancel"),
        ReceiptLookup::Pending
    );
    // 3. 同 id 再次 execute，得 DuplicateInvocation
    let (code, _) = send_rejected(
        &mut stream,
        12,
        execute_req(
            "inv-cancel",
            &digest,
            "act",
            json!({"script": BUSY_SCRIPT}),
            30_000,
            65_536,
            Labels::new(),
        ),
    );
    assert_eq!(code, RejectCode::DuplicateInvocation);

    // 4. 发 cancel，得 cancel_ok 与 Cancelled 执行回复
    let cancel_started = Instant::now();
    send_only(
        &mut stream,
        13,
        Request::Cancel {
            invocation_id: "inv-cancel".to_owned(),
        },
    );
    let mut got_cancel_ok = false;
    let mut got_exec_cancelled = false;
    for _ in 0..2 {
        let frame = read_reply(&mut stream);
        match (frame.request_id, frame.message) {
            (13, Response::CancelOk) => got_cancel_ok = true,
            (
                10,
                Response::ExecuteResult {
                    outcome:
                        ExecuteOutcome::Cancelled {
                            receipt_summary: Some(_),
                        },
                },
            ) => got_exec_cancelled = true,
            other => panic!("收到意外回复帧：{other:?}"),
        }
    }
    assert!(got_cancel_ok && got_exec_cancelled);

    let grace = Duration::from_millis(hello.capabilities.settle_grace_ms);
    let mut req_id = 14;
    let receipt = wait_for_settled(
        &mut stream,
        &mut req_id,
        "inv-cancel",
        cancel_started + grace,
    );
    let elapsed = cancel_started.elapsed();
    assert_eq!(receipt.outcome, ReceiptOutcome::Cancelled);
    assert!(
        elapsed < grace,
        "取消结算耗时 {elapsed:?} 应小于 settle_grace_ms ({grace:?})"
    );
    eprintln!("cancel_settle_elapsed_ms={}", elapsed.as_millis());

    // 对已结束 id 与未知 id 发 cancel 均返回 cancel_ok，且未知 id 之后仍可执行。
    send_cancel(&mut stream, 20, "inv-cancel");
    send_cancel(&mut stream, 21, "inv-unknown-after-cancel");
    let after = run_act(
        &mut stream,
        22,
        "inv-unknown-after-cancel",
        &digest,
        "return 7\n",
    );
    assert!(matches!(after, ExecuteOutcome::Succeeded { .. }));
}

#[test]
fn disconnect_cancels_only_its_connection() {
    let node = TestNode::start(None);
    let (mut conn_a, hello, digest) = node.connect_hello();
    let (mut conn_b, _, _) = node.connect_hello();

    // 连接 A 上有两个在途调用，先在同一连接上确认都为 Pending。
    send_only(
        &mut conn_a,
        10,
        execute_req(
            "inv-a1",
            &digest,
            "act",
            json!({"script": BUSY_SCRIPT}),
            30_000,
            65_536,
            Labels::new(),
        ),
    );
    send_only(
        &mut conn_a,
        11,
        execute_req(
            "inv-a2",
            &digest,
            "act",
            json!({"script": BUSY_SCRIPT}),
            30_000,
            65_536,
            Labels::new(),
        ),
    );
    assert_eq!(
        query_receipt(&mut conn_a, 12, "inv-a1"),
        ReceiptLookup::Pending
    );
    assert_eq!(
        query_receipt(&mut conn_a, 13, "inv-a2"),
        ReceiptLookup::Pending
    );

    // 连接 B 上有一个在途调用，先在 B 上确认为 Pending。
    send_only(
        &mut conn_b,
        10,
        execute_req(
            "inv-b1",
            &digest,
            "act",
            json!({"script": BUSY_SCRIPT}),
            30_000,
            65_536,
            Labels::new(),
        ),
    );
    assert_eq!(
        query_receipt(&mut conn_b, 11, "inv-b1"),
        ReceiptLookup::Pending
    );

    // 关闭 A 后，A 的两个调用在 settle_grace_ms 内结算为 Cancelled。
    let grace = Duration::from_millis(hello.capabilities.settle_grace_ms);
    let disconnect_started = Instant::now();
    drop(conn_a);

    let (mut conn_c, _, _) = node.connect_hello();
    let mut req_c = 10;
    let r_a1 = wait_for_settled(
        &mut conn_c,
        &mut req_c,
        "inv-a1",
        disconnect_started + grace,
    );
    let r_a2 = wait_for_settled(
        &mut conn_c,
        &mut req_c,
        "inv-a2",
        disconnect_started + grace,
    );
    let disconnect_elapsed = disconnect_started.elapsed();
    assert_eq!(r_a1.outcome, ReceiptOutcome::Cancelled);
    assert_eq!(r_a2.outcome, ReceiptOutcome::Cancelled);
    assert!(
        disconnect_elapsed < grace,
        "断连取消结算耗时 {disconnect_elapsed:?} 应小于 settle_grace_ms ({grace:?})"
    );
    eprintln!(
        "disconnect_settle_elapsed_ms={}",
        disconnect_elapsed.as_millis()
    );

    // B 的调用仍为 Pending。
    req_c += 1;
    assert_eq!(
        query_receipt(&mut conn_c, req_c, "inv-b1"),
        ReceiptLookup::Pending,
        "关闭 A 不得影响连接 B 上的在途调用"
    );

    // 从另一条连接 C 对 B 的 id 发 cancel，B 结算为 Cancelled。
    req_c += 1;
    let cancel_b_started = Instant::now();
    send_cancel(&mut conn_c, req_c, "inv-b1");
    req_c += 1;
    let r_b1 = wait_for_settled(&mut conn_c, &mut req_c, "inv-b1", cancel_b_started + grace);
    assert_eq!(r_b1.outcome, ReceiptOutcome::Cancelled);

    let b_reply = read_reply(&mut conn_b);
    assert_eq!(b_reply.request_id, 10);
    assert!(matches!(
        b_reply.message,
        Response::ExecuteResult {
            outcome: ExecuteOutcome::Cancelled {
                receipt_summary: Some(_)
            }
        }
    ));
}

#[test]
fn restart_settles_pending_before_first_hello() {
    let mut node = TestNode::start(None);
    let (mut stream, _, digest) = node.connect_hello();

    let labels_1 = BTreeMap::from([("slot".to_owned(), json!(1))]);
    let labels_2 = BTreeMap::from([("slot".to_owned(), json!("two"))]);

    send_only(
        &mut stream,
        10,
        execute_req(
            "inv-p1",
            &digest,
            "act",
            json!({"script": BUSY_SCRIPT}),
            30_000,
            65_536,
            labels_1.clone(),
        ),
    );
    send_only(
        &mut stream,
        11,
        execute_req(
            "inv-p2",
            &digest,
            "act",
            json!({"script": BUSY_SCRIPT}),
            30_000,
            65_536,
            labels_2.clone(),
        ),
    );
    assert_eq!(
        query_receipt(&mut stream, 12, "inv-p1"),
        ReceiptLookup::Pending
    );
    assert_eq!(
        query_receipt(&mut stream, 13, "inv-p2"),
        ReceiptLookup::Pending
    );

    // 在保持原连接未断开的状态下强杀并重启节点，确保两条调用以 Pending 状态进入恢复流程。
    node.restart();
    drop(stream);
    let (mut stream, _, digest) = node.connect_hello();

    for (req_id, inv_id, expected_labels) in [(10, "inv-p1", labels_1), (11, "inv-p2", labels_2)] {
        let receipt = match query_receipt(&mut stream, req_id, inv_id) {
            ReceiptLookup::Settled { receipt } => receipt,
            other => panic!("重启首个 hello 后 `{inv_id}` 应已结算，实际：{other:?}"),
        };
        assert_eq!(receipt.labels, expected_labels);
        match &receipt.outcome {
            ReceiptOutcome::Failed { error } => {
                assert_eq!(error.code, "InternalError");
                assert_eq!(error.message, "节点在调用结算前重启，未保存执行结果");
            }
            other => panic!("重启恢复应结算为 InternalError，实际：{other:?}"),
        }
    }

    let (code, _) = send_rejected(
        &mut stream,
        12,
        execute_req(
            "inv-p1",
            &digest,
            "act",
            json!({"script": "return 1"}),
            5_000,
            65_536,
            Labels::new(),
        ),
    );
    assert_eq!(code, RejectCode::DuplicateInvocation);
}

#[test]
fn expired_receipt_keeps_outcome_and_replay_fence() {
    // 保留期留出足够余量，使负载下结算到首次查询的延迟不会让回执提前过期。
    const RETENTION_MS: u64 = 2_000;
    let node = TestNode::start(Some(RETENTION_MS));
    let (mut stream, hello, digest) = node.connect_hello();
    assert_eq!(hello.capabilities.receipt_retention_ms, RETENTION_MS);

    let outcome = run_act(&mut stream, 10, "inv-expire", &digest, "return 123\n");
    assert!(matches!(outcome, ExecuteOutcome::Succeeded { .. }));

    assert!(
        matches!(
            query_receipt(&mut stream, 11, "inv-expire"),
            ReceiptLookup::Settled { .. }
        ),
        "保留期内查询应返回完整 Settled 回执"
    );

    thread::sleep(Duration::from_millis(RETENTION_MS + 100));
    assert_eq!(
        query_receipt(&mut stream, 12, "inv-expire"),
        ReceiptLookup::Expired {
            outcome: OutcomeKind::Succeeded
        },
        "超过保留期后查询应返回 Expired 并保留结局种类"
    );

    let (code, _) = send_rejected(
        &mut stream,
        13,
        execute_req(
            "inv-expire",
            &digest,
            "act",
            json!({"script": "return 1"}),
            5_000,
            65_536,
            Labels::new(),
        ),
    );
    assert_eq!(
        code,
        RejectCode::DuplicateInvocation,
        "回执过期后零重放围栏仍须生效"
    );
}

fn connect_process(process: &NodeProcess) -> (UnixStream, CatalogDigest) {
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

#[test]
fn execute_runs_read_list_and_search() {
    let (_root, _state_dir, process) = TestNode::start_custom(|root_path| {
        let ro_a = root_path.join("ro_a");
        let ro_b = root_path.join("ro_b");
        let rw = root_path.join("rw");
        for d in [&ro_a, &ro_b, &rw] {
            std::fs::create_dir(d).unwrap();
        }
        std::fs::write(ro_a.join("same.txt"), "from a").unwrap();
        std::fs::write(ro_b.join("same.txt"), "from b").unwrap();
        std::fs::write(rw.join("same.txt"), "from rw").unwrap();

        std::fs::create_dir(ro_a.join("empty")).unwrap();
        std::fs::create_dir_all(ro_a.join("tree/sub")).unwrap();
        std::fs::write(ro_a.join("tree/a.txt"), "match once\nother\n").unwrap();
        std::fs::write(
            ro_a.join("tree/b.txt"),
            "first\r\nmatch line match again\r\nsecond\r\n",
        )
        .unwrap();
        std::fs::write(ro_a.join("tree/sub/deep.txt"), "match deep\n").unwrap();
        std::os::unix::fs::symlink("a.txt", ro_a.join("tree/c_link")).unwrap();

        format!(
            r#"
[[dir]]
name = "ro_a"
path = {ro_a:?}
access = "read"

[[dir]]
name = "rw"
path = {rw:?}
access = "write"

[[dir]]
name = "ro_b"
path = {ro_b:?}
access = "read"
"#,
            ro_a = ro_a.to_str().unwrap(),
            rw = rw.to_str().unwrap(),
            ro_b = ro_b.to_str().unwrap(),
        )
    });
    let (mut stream, digest) = connect_process(&process);

    // 1. 槽位与基本 read / list / search。
    let ok = run_act(
        &mut stream,
        10,
        "inv-read-ok",
        &digest,
        r#"
entries = ro_a.list("tree")
hits_all = ro_a.search("match")
hits_shallow = ro_a.search("match", glob = "tree/*.txt")
hits_globstar = ro_a.search("match", glob = "tree/**/*.txt")
return {
    "slots": [ro_a.read("same.txt"), ro_b.read("same.txt")],
    "empty": ro_a.list("empty"),
    "missing_list": ro_a.list("absent", missing_ok = True),
    "entries": entries,
    "hits_all": hits_all,
    "hits_shallow": hits_shallow,
    "hits_globstar": hits_globstar,
}
"#,
    );
    let content = match ok {
        ExecuteOutcome::Succeeded { content, .. } => content,
        other => panic!("期望成功，实际：{other:?}"),
    };
    let val = &content["value"];
    assert_eq!(val["slots"], json!(["from a", "from b"]));
    assert_eq!(val["empty"], json!([]));
    assert_eq!(val["missing_list"], json!(null));
    assert_eq!(
        val["entries"],
        json!([
            {"name": "a.txt", "is_dir": false, "is_link": false},
            {"name": "b.txt", "is_dir": false, "is_link": false},
            {"name": "c_link", "is_dir": false, "is_link": true},
            {"name": "sub", "is_dir": true, "is_link": false},
        ])
    );
    assert_eq!(
        val["hits_all"],
        json!([
            {"path": "tree/a.txt", "line_no": 1, "text": "match once"},
            {"path": "tree/b.txt", "line_no": 2, "text": "match line match again"},
            {"path": "tree/c_link", "line_no": 1, "text": "match once"},
            {"path": "tree/sub/deep.txt", "line_no": 1, "text": "match deep"},
        ])
    );
    assert_eq!(
        val["hits_shallow"],
        json!([
            {"path": "tree/a.txt", "line_no": 1, "text": "match once"},
            {"path": "tree/b.txt", "line_no": 2, "text": "match line match again"},
        ])
    );
    assert_eq!(
        val["hits_globstar"],
        json!([
            {"path": "tree/a.txt", "line_no": 1, "text": "match once"},
            {"path": "tree/b.txt", "line_no": 2, "text": "match line match again"},
            {"path": "tree/sub/deep.txt", "line_no": 1, "text": "match deep"},
        ])
    );

    // 2. 可写目录按槽位顺序注入，收窄为只读视图即可通过 read() 读出内容，effects 为空。
    let rw_res = run_act(
        &mut stream,
        11,
        "inv-rw-read",
        &digest,
        "return rw.read(\"same.txt\")",
    );
    match rw_res {
        ExecuteOutcome::Succeeded { content, .. } => {
            assert_eq!(content["value"], json!("from rw"));
        }
        other => panic!("可写目录读调用应成功，实际：{other:?}"),
    }
    let rw_receipt = match query_receipt(&mut stream, 12, "inv-rw-read") {
        ReceiptLookup::Settled { receipt } => receipt,
        other => panic!("期望 Settled 回执，实际：{other:?}"),
    };
    assert_eq!(
        rw_receipt.steps,
        vec![lancet_protocol::StepSummary {
            tool: "fs.read".to_owned(),
            outcome: OutcomeKind::Succeeded,
        }]
    );
    assert!(rw_receipt.effects.is_empty());

    // 3. list 缺失目录默认报错；无效正则报错。
    for (idx, bad_script) in ["return ro_a.list(\"absent\")", "return ro_a.search(\"[\")"]
        .into_iter()
        .enumerate()
    {
        let res = run_act(
            &mut stream,
            20 + idx as u64,
            &format!("inv-bad-call-{idx}"),
            &digest,
            bad_script,
        );
        match res {
            ExecuteOutcome::Failed { error, .. } => assert_eq!(error.code, "ScriptOrToolError"),
            other => panic!("预期脚本失败，实际：{other:?}"),
        }
    }

    // 4. 先成功、后失败的两次调用留下两条步骤，effects 为空。
    let two_steps = run_act(
        &mut stream,
        30,
        "inv-two-steps",
        &digest,
        "a = ro_a.read(\"same.txt\")\nb = ro_a.read(\"missing.txt\")\nreturn [a, b]\n",
    );
    assert!(matches!(two_steps, ExecuteOutcome::Failed { .. }));
    let receipt = match query_receipt(&mut stream, 31, "inv-two-steps") {
        ReceiptLookup::Settled { receipt } => receipt,
        other => panic!("期望 Settled 回执，实际：{other:?}"),
    };
    assert_eq!(
        receipt.steps,
        vec![
            lancet_protocol::StepSummary {
                tool: "fs.read".to_owned(),
                outcome: OutcomeKind::Succeeded,
            },
            lancet_protocol::StepSummary {
                tool: "fs.read".to_owned(),
                outcome: OutcomeKind::Failed,
            },
        ]
    );
    assert!(receipt.effects.is_empty());
}

#[test]
fn hidden_paths_are_invisible_to_all_read_tools() {
    let (_root, _state_dir, process) = TestNode::start_custom(|root_path| {
        let ro = root_path.join("ro");
        std::fs::create_dir_all(ro.join("secrets/deep")).unwrap();
        std::fs::create_dir_all(ro.join("nested")).unwrap();
        std::fs::create_dir_all(ro.join("docs")).unwrap();
        std::fs::write(ro.join(".gitignore"), "from_gitignore.txt\n").unwrap();
        std::fs::write(ro.join("from_gitignore.txt"), "visible gitignored").unwrap();
        std::fs::write(ro.join("secrets/key.txt"), "secret key").unwrap();
        std::fs::write(ro.join("secrets/unignored.txt"), "still hidden").unwrap();
        std::fs::write(ro.join("secrets/present.txt"), "present secret").unwrap();
        std::fs::write(ro.join("nested/secret.txt"), "nested secret").unwrap();
        std::fs::write(ro.join("docs/public.txt"), "public doc").unwrap();
        std::os::unix::fs::symlink("public.txt", ro.join("docs/secretlink")).unwrap();
        std::os::unix::fs::symlink("loop", ro.join("docs/loop")).unwrap();
        std::fs::write(ro.join("name"), "target content").unwrap();
        std::fs::write(ro.join("notes"), "not a dir").unwrap();
        std::fs::write(ro.join("app.log"), "hidden log").unwrap();
        std::fs::write(ro.join("keep.log"), "kept log").unwrap();
        std::fs::write(ro.join("visible.txt"), "ok").unwrap();

        std::os::unix::fs::symlink("docs", ro.join("alias")).unwrap();
        std::os::unix::fs::symlink("omitted/../name", ro.join("via_omitted")).unwrap();
        std::os::unix::fs::symlink("secrets/present.txt", ro.join("peek_present")).unwrap();
        std::os::unix::fs::symlink("secrets/maybe.txt", ro.join("peek_missing")).unwrap();

        format!(
            r#"
[[dir]]
name = "ro"
path = {ro:?}
access = "read"
hidden = ["secrets/", "!secrets/unignored.txt", "**/secret.txt", "*.log", "!keep.log", "notes/", "docs/secretlink", "omitted/", "alias/loop", "vault/"]
"#,
            ro = ro.to_str().unwrap(),
        )
    });
    let (mut stream, digest) = connect_process(&process);

    let ok = run_act(
        &mut stream,
        10,
        "inv-hidden-visible",
        &digest,
        r#"
return {
    "gitignored": ro.read("from_gitignore.txt"),
    "kept": ro.read("keep.log"),
    "notes": ro.read("notes"),
    "docs_public": ro.read("docs/public.txt"),
    "root_list": [e.name for e in ro.list(".")],
    "nested_list": [e.name for e in ro.list("nested")],
    "docs_list": [e.name for e in ro.list("docs")],
    "alias_list": [e.name for e in ro.list("alias")],
    "search": ro.search("visible|kept|secret|hidden"),
}
"#,
    );
    let val = match ok {
        ExecuteOutcome::Succeeded { content, .. } => content["value"].clone(),
        other => panic!("可见内容访问应成功，实际：{other:?}"),
    };
    assert_eq!(val["gitignored"], "visible gitignored");
    assert_eq!(val["kept"], "kept log");
    assert_eq!(val["notes"], "not a dir");
    assert_eq!(val["docs_public"], "public doc");
    assert_eq!(
        val["root_list"],
        json!([
            ".gitignore",
            "alias",
            "docs",
            "from_gitignore.txt",
            "keep.log",
            "name",
            "nested",
            "notes",
            "visible.txt"
        ])
    );
    assert_eq!(val["nested_list"], json!([]));
    assert_eq!(val["docs_list"], json!(["public.txt"]));
    assert_eq!(val["alias_list"], json!(["public.txt"]));
    assert_eq!(
        val["search"],
        json!([
            {"path": "from_gitignore.txt", "line_no": 1, "text": "visible gitignored"},
            {"path": "keep.log", "line_no": 1, "text": "kept log"},
        ])
    );

    let non_dir_parent = run_act(
        &mut stream,
        15,
        "inv-hidden-non-dir-parent",
        &digest,
        r#"return ro.read("notes/chapter")"#,
    );
    match non_dir_parent {
        ExecuteOutcome::Failed { error, .. } => {
            assert_eq!(error.code, "ScriptOrToolError");
            assert!(
                error.message.contains("上级不是目录"),
                "经普通文件 notes 继续向下访问应报上级不是目录，实际：{}",
                error.message
            );
        }
        other => panic!("经普通文件 notes 继续向下访问应报错，实际：{other:?}"),
    }

    let hidden_probes = [
        r#"return ro.read("secrets/key.txt", missing_ok = True)"#,
        r#"return ro.read("secrets/unignored.txt", missing_ok = True)"#,
        r#"return ro.read("secrets/nonexistent.txt", missing_ok = True)"#,
        r#"return ro.read("nested/secret.txt", missing_ok = True)"#,
        r#"return ro.read("app.log", missing_ok = True)"#,
        r#"return ro.read("docs/secretlink", missing_ok = True)"#,
        r#"return ro.read("alias/secretlink", missing_ok = True)"#,
        r#"return ro.read("via_omitted", missing_ok = True)"#,
        r#"return ro.read("alias/loop", missing_ok = True)"#,
        r#"return ro.read("peek_present", missing_ok = True)"#,
        r#"return ro.read("peek_missing", missing_ok = True)"#,
        r#"return ro.list("vault", missing_ok = True)"#,
        r#"return ro.read("vault", missing_ok = True)"#,
        r#"return ro.list("secrets", missing_ok = True)"#,
        r#"return ro.read("secrets", missing_ok = True)"#,
        r#"return ro.list("secrets/missing", missing_ok = True)"#,
    ];
    for (idx, script) in hidden_probes.into_iter().enumerate() {
        let res = run_act(
            &mut stream,
            20 + idx as u64,
            &format!("inv-hidden-probe-{idx}"),
            &digest,
            script,
        );
        match res {
            ExecuteOutcome::Failed { error, .. } => {
                assert_eq!(error.code, "ScriptOrToolError");
                assert!(
                    error.message.contains("命中 hidden 规则"),
                    "脚本 `{script}` 应报命中 hidden 规则错误，实际：{}",
                    error.message
                );
                assert!(
                    !error.message.contains("层数过多"),
                    "脚本 `{script}` 不应报符号链接层数过多，实际：{}",
                    error.message
                );
            }
            other => panic!("隐藏探测 `{script}` 必须报错，实际：{other:?}"),
        }
    }

    // omitted 作为真实目录存在后，via_omitted 同样应报 hidden（与 omitted 不存在时一致）。
    std::fs::create_dir_all(_root.path().join("ro/omitted")).unwrap();
    let res = run_act(
        &mut stream,
        40,
        "inv-hidden-omitted-exists",
        &digest,
        r#"return ro.read("via_omitted", missing_ok = True)"#,
    );
    match res {
        ExecuteOutcome::Failed { error, .. } => {
            assert_eq!(error.code, "ScriptOrToolError");
            assert!(
                error.message.contains("命中 hidden 规则"),
                "omitted 存在后 via_omitted 仍应报命中 hidden 规则错误，实际：{}",
                error.message
            );
        }
        other => panic!("omitted 存在后 via_omitted 必须报错，实际：{other:?}"),
    }
}

#[test]
fn symlinks_follow_only_within_dir() {
    let (_root, _state_dir, process) = TestNode::start_custom(|root_path| {
        let ro = root_path.join("ro");
        let outside = root_path.join("outside");
        std::fs::create_dir_all(ro.join("subdir")).unwrap();
        std::fs::create_dir_all(ro.join("secrets")).unwrap();
        std::fs::create_dir_all(&outside).unwrap();

        std::fs::write(ro.join("real.txt"), "inside target\n").unwrap();
        std::fs::write(ro.join("subdir/inside.txt"), "inside sub\n").unwrap();
        std::fs::write(ro.join(".env"), "TOP_SECRET=1\n").unwrap();
        std::fs::write(ro.join("secrets/s.txt"), "hidden dir\n").unwrap();
        std::fs::write(outside.join("escape.txt"), "outside\n").unwrap();

        std::os::unix::fs::symlink("real.txt", ro.join("link_file")).unwrap();
        std::os::unix::fs::symlink("subdir", ro.join("link_dir")).unwrap();
        std::os::unix::fs::symlink("../outside/escape.txt", ro.join("link_escape_rel")).unwrap();
        std::os::unix::fs::symlink(outside.join("escape.txt"), ro.join("link_escape_abs")).unwrap();
        std::os::unix::fs::symlink(".env", ro.join("link_hidden_file")).unwrap();
        std::os::unix::fs::symlink("secrets", ro.join("link_hidden_dir")).unwrap();
        drop(UnixListener::bind(ro.join("sock")).unwrap());
        std::os::unix::fs::symlink("sock", ro.join("link_sock")).unwrap();

        // 构造恰好需要 40 跳符号链接才能到达的目录 `chain/hop40`。
        std::fs::create_dir_all(ro.join("chain/target")).unwrap();
        std::fs::write(ro.join("chain/target/leaf.txt"), "hop leaf\n").unwrap();
        std::os::unix::fs::symlink("leaf.txt", ro.join("chain/target/leaf_link")).unwrap();
        std::os::unix::fs::symlink("target", ro.join("chain/hop01")).unwrap();
        for i in 2..=40 {
            let prev = format!("hop{:02}", i - 1);
            let cur = format!("hop{i:02}");
            std::os::unix::fs::symlink(prev, ro.join("chain").join(cur)).unwrap();
        }

        format!(
            r#"
[[dir]]
name = "ro"
path = {ro:?}
access = "read"
hidden = ["secrets/", ".env"]
"#,
            ro = ro.to_str().unwrap(),
        )
    });
    let (mut stream, digest) = connect_process(&process);

    let ok = run_act(
        &mut stream,
        10,
        "inv-sym-ok",
        &digest,
        r#"
return {
    "link_file": ro.read("link_file"),
    "link_dir_entries": ro.list("link_dir"),
    "root_entries": ro.list("."),
    "hop39_entries": ro.list("chain/hop39"),
    "hop39_link": ro.read("chain/hop39/leaf_link"),
    "hop40_entries": ro.list("chain/hop40"),
    "hop40_leaf": ro.read("chain/hop40/leaf.txt"),
    "search_file": ro.search("inside target"),
    "search_sub": ro.search("inside sub"),
}
"#,
    );
    let val = match ok {
        ExecuteOutcome::Succeeded { content, .. } => content["value"].clone(),
        other => panic!("目录内合法符号链接操作应成功，实际：{other:?}"),
    };
    assert_eq!(val["link_file"], "inside target\n");
    assert_eq!(
        val["link_dir_entries"],
        json!([{"name": "inside.txt", "is_dir": false, "is_link": false}])
    );
    assert_eq!(
        val["root_entries"],
        json!([
            {"name": "chain", "is_dir": true, "is_link": false},
            {"name": "link_dir", "is_dir": true, "is_link": true},
            {"name": "link_file", "is_dir": false, "is_link": true},
            {"name": "real.txt", "is_dir": false, "is_link": false},
            {"name": "subdir", "is_dir": true, "is_link": false},
        ])
    );
    assert_eq!(
        val["hop39_entries"],
        json!([
            {"name": "leaf.txt", "is_dir": false, "is_link": false},
            {"name": "leaf_link", "is_dir": false, "is_link": true},
        ])
    );
    assert_eq!(val["hop39_link"], "hop leaf\n");
    // 经 40 跳到达的目录中，`leaf_link` 会触发第 41 跳，因此 list 略去它且 read 也会失败。
    assert_eq!(
        val["hop40_entries"],
        json!([{"name": "leaf.txt", "is_dir": false, "is_link": false}])
    );
    assert_eq!(val["hop40_leaf"], "hop leaf\n");
    assert_eq!(
        val["search_file"],
        json!([
            {"path": "link_file", "line_no": 1, "text": "inside target"},
            {"path": "real.txt", "line_no": 1, "text": "inside target"},
        ])
    );
    assert_eq!(
        val["search_sub"],
        json!([{"path": "subdir/inside.txt", "line_no": 1, "text": "inside sub"}])
    );

    let err_cases = [
        (
            "相对越界链接",
            "return ro.read(\"link_escape_rel\")",
            "越出",
        ),
        (
            "绝对越界链接",
            "return ro.read(\"link_escape_abs\")",
            "越出",
        ),
        (
            "第 41 跳子链接",
            "return ro.read(\"chain/hop40/leaf_link\")",
            "层数过多",
        ),
        (
            "指向隐藏文件的链接",
            "return ro.read(\"link_hidden_file\")",
            "命中 hidden 规则",
        ),
        (
            "经隐藏目录链接读不存在路径",
            "return ro.read(\"link_hidden_dir/absent\", missing_ok = True)",
            "命中 hidden 规则",
        ),
        (
            "经隐藏目录链接列不存在路径",
            "return ro.list(\"link_hidden_dir/absent\", missing_ok = True)",
            "命中 hidden 规则",
        ),
        ("Unix socket", "return ro.read(\"sock\")", "不是普通文件"),
        (
            "指向 Unix socket 的链接",
            "return ro.read(\"link_sock\")",
            "不是普通文件",
        ),
        (
            "路径含 ..",
            "return ro.read(\"../outside/escape.txt\")",
            "越出",
        ),
        ("绝对路径", "return ro.list(\"/tmp\")", "越出"),
    ];
    for (idx, (case_name, script, needle)) in err_cases.into_iter().enumerate() {
        let res = run_act(
            &mut stream,
            20 + idx as u64,
            &format!("inv-sym-err-{idx}"),
            &digest,
            script,
        );
        match res {
            ExecuteOutcome::Failed { error, .. } => {
                assert_eq!(error.code, "ScriptOrToolError");
                assert!(
                    error.message.contains(needle),
                    "`{case_name}` 报错应包含 `{needle}`，实际：{}",
                    error.message
                );
            }
            other => panic!("`{case_name}` 应失败，实际：{other:?}"),
        }
    }
}

#[test]
fn read_tools_report_encoding_errors() {
    let (_root, _state_dir, process) = TestNode::start_custom(|root_path| {
        let ro = root_path.join("ro");
        let bad_dir = root_path.join("bad_names");
        std::fs::create_dir_all(&ro).unwrap();
        std::fs::create_dir_all(&bad_dir).unwrap();
        std::fs::write(ro.join("good.txt"), "hello world\n").unwrap();
        std::fs::write(ro.join("bad.bin"), [0xffu8, 0xfe, 0xfd]).unwrap();
        std::fs::write(bad_dir.join(OsStr::from_bytes(b"bad-\xff.txt")), "ok").unwrap();

        format!(
            r#"
[[dir]]
name = "ro"
path = {ro:?}
access = "read"

[[dir]]
name = "bad_names"
path = {bad_dir:?}
access = "read"
"#,
            ro = ro.to_str().unwrap(),
            bad_dir = bad_dir.to_str().unwrap(),
        )
    });
    let (mut stream, digest) = connect_process(&process);

    // 1. 非 UTF-8 文件使 read 报错。
    match run_act(
        &mut stream,
        10,
        "inv-enc-read",
        &digest,
        "return ro.read(\"bad.bin\")",
    ) {
        ExecuteOutcome::Failed { error, .. } => {
            assert_eq!(error.code, "ScriptOrToolError");
            assert!(error.message.contains("UTF-8"), "{}", error.message);
        }
        other => panic!("非 UTF-8 read 应失败，实际：{other:?}"),
    }

    // 2. 省略 glob 的 search 因 bad.bin 失败并写明路径。
    match run_act(
        &mut stream,
        11,
        "inv-enc-search-all",
        &digest,
        "return ro.search(\"hello\")",
    ) {
        ExecuteOutcome::Failed { error, .. } => {
            assert_eq!(error.code, "ScriptOrToolError");
            assert!(error.message.contains("bad.bin"), "{}", error.message);
        }
        other => panic!("未排除 bad.bin 的 search 应失败，实际：{other:?}"),
    }

    // 3. glob 排除 bad.bin 时 search 成功。
    match run_act(
        &mut stream,
        12,
        "inv-enc-search-glob",
        &digest,
        "return ro.search(\"hello\", glob = \"*.txt\")",
    ) {
        ExecuteOutcome::Succeeded { content, .. } => {
            assert_eq!(
                content["value"],
                json!([{"path": "good.txt", "line_no": 1, "text": "hello world"}])
            );
        }
        other => panic!("glob 排除非 UTF-8 文件后 search 应成功，实际：{other:?}"),
    }

    // 4. 名称不是 UTF-8 的条目使 list 报错。
    match run_act(
        &mut stream,
        13,
        "inv-enc-list",
        &digest,
        "return bad_names.list(\".\")",
    ) {
        ExecuteOutcome::Failed { error, .. } => {
            assert_eq!(error.code, "ScriptOrToolError");
            assert!(error.message.contains("UTF-8"), "{}", error.message);
        }
        other => panic!("非 UTF-8 文件名 list 应失败，实际：{other:?}"),
    }
}

#[test]
fn tool_result_window_uses_compact_json_bytes() {
    // 包含需要 JSON 转义的字符（'"' 1->2、'\\' 1->2、'\n' 1->2）与多字节 UTF-8 字符（'é'，2 字节计 1 字符）。
    let prefix = "\"\\\né";
    let prefix_json_len = serde_json::to_string(prefix).unwrap().len(); // 包含两侧引号的字节数
    let pad_exact = 8 * 1024 * 1024 - prefix_json_len;
    let exact = format!("{prefix}{}", "a".repeat(pad_exact));
    assert_eq!(
        serde_json::to_string(&exact).unwrap().len(),
        8 * 1024 * 1024
    );
    let expected_chars = exact.chars().count();
    let over = format!("{exact}b");
    assert_eq!(
        serde_json::to_string(&over).unwrap().len(),
        8 * 1024 * 1024 + 1
    );

    let (_root, _state_dir, process) = TestNode::start_custom(move |root_path| {
        let ro = root_path.join("ro");
        std::fs::create_dir(&ro).unwrap();

        std::fs::write(ro.join("exact.txt"), &exact).unwrap();
        std::fs::write(ro.join("over.txt"), &over).unwrap();

        format!(
            r#"
[[dir]]
name = "ro"
path = {ro:?}
access = "read"
"#,
            ro = ro.to_str().unwrap(),
        )
    });
    let (mut stream, digest) = connect_process(&process);

    let ok = run_execute(
        &mut stream,
        10,
        execute_req(
            "inv-win-exact",
            &digest,
            "act",
            json!({"script": "return len(ro.read(\"exact.txt\"))"}),
            10_000,
            1024,
            Labels::new(),
        ),
    );
    match ok {
        ExecuteOutcome::Succeeded { content, .. } => {
            assert_eq!(content["value"], json!(expected_chars));
        }
        other => panic!("恰好 8 MiB 返回窗口应成功，实际：{other:?}"),
    }

    let over = run_execute(
        &mut stream,
        11,
        execute_req(
            "inv-win-over",
            &digest,
            "act",
            json!({"script": "return len(ro.read(\"over.txt\"))"}),
            10_000,
            1024,
            Labels::new(),
        ),
    );
    match over {
        ExecuteOutcome::Failed { error, .. } => {
            assert_eq!(error.code, "ScriptOrToolError");
            assert!(
                error.message.contains("8388608") && error.message.contains("proc.pipe"),
                "超窗报错应含上限与 proc.pipe 提示，实际：{}",
                error.message
            );
        }
        other => panic!("多 1 字节应超出返回窗口并报错，实际：{other:?}"),
    }
}

#[test]
fn receipt_truncates_real_read_steps_to_one_frame() {
    let (_root, state_dir, process) = TestNode::start_custom(|root_path| {
        let ro = root_path.join("ro");
        std::fs::create_dir(&ro).unwrap();
        format!(
            r#"
[[dir]]
name = "ro"
path = {ro:?}
access = "read"
"#,
            ro = ro.to_str().unwrap(),
        )
    });
    let (mut stream, digest) = connect_process(&process);

    let envelope_len = serde_json::to_vec(&json!({"value": "", "printed": ""}))
        .unwrap()
        .len();
    let payload_str_len = MAX_OUTPUT_LIMIT as usize - envelope_len;
    let script = format!(
        "ro.list(\".\")\nfor _ in range(511):\n    ro.read(\"absent\", missing_ok = True)\nreturn \"a\" * {payload_str_len}\n"
    );

    let labels = BTreeMap::from([
        ("session".to_owned(), json!("s-trunc")),
        ("turn".to_owned(), json!(7)),
    ]);

    let outcome = run_execute(
        &mut stream,
        10,
        execute_req(
            "inv-trunc-512",
            &digest,
            "act",
            json!({ "script": script }),
            30_000,
            MAX_OUTPUT_LIMIT,
            labels.clone(),
        ),
    );
    assert!(
        matches!(outcome, ExecuteOutcome::Succeeded { .. }),
        "512 步加最大载荷应成功，实际：{outcome:?}"
    );

    // 读取节点调用记录中的完整回执。
    let call_files: Vec<_> = std::fs::read_dir(state_dir.join("calls"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    assert_eq!(call_files.len(), 1);
    let record_json: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&call_files[0]).unwrap()).unwrap();
    let stored: Receipt = serde_json::from_value(record_json["receipt"].clone()).unwrap();
    assert_eq!(stored.invocation_id, "inv-trunc-512");
    assert_eq!(stored.steps.len(), 512);
    assert!(stored.effects.is_empty());
    assert!(!stored.truncated);

    // 去掉 steps 时放得进一帧；带上 steps 时超出单帧上限。
    let mut no_steps = stored.clone();
    no_steps.steps.clear();
    let mut wire_check = Vec::new();
    write_frame(
        &mut wire_check,
        &Frame {
            request_id: 99,
            message: Response::ReceiptResult {
                lookup: ReceiptLookup::Settled { receipt: no_steps },
            },
        },
    )
    .expect("去掉 steps 时回执应放得进一帧");

    wire_check.clear();
    let err_with_steps = write_frame(
        &mut wire_check,
        &Frame {
            request_id: 99,
            message: Response::ReceiptResult {
                lookup: ReceiptLookup::Settled {
                    receipt: stored.clone(),
                },
            },
        },
    )
    .expect_err("带上 512 条 steps 时应超出单帧上限");
    assert!(matches!(err_with_steps, FrameError::FrameTooLarge { .. }));

    // 直接读线上的 4 字节长度前缀核对 receipt 回复帧负载不超过 8 MiB。
    send_only(
        &mut stream,
        99,
        Request::Receipt {
            invocation_id: "inv-trunc-512".to_owned(),
        },
    );
    let mut prefix = [0u8; 4];
    stream.read_exact(&mut prefix).unwrap();
    let wire_len = u32::from_be_bytes(prefix) as usize;
    assert!(
        wire_len <= MAX_FRAME_BYTES,
        "receipt 帧负载 {wire_len} 不应超过 {MAX_FRAME_BYTES}"
    );
    let mut payload_buf = vec![0u8; wire_len];
    stream.read_exact(&mut payload_buf).unwrap();
    let frame: Frame<Response> = serde_json::from_slice(&payload_buf).unwrap();
    assert_eq!(frame.request_id, 99);
    let truncated_receipt = match frame.message {
        Response::ReceiptResult {
            lookup: ReceiptLookup::Settled { receipt },
        } => receipt,
        other => panic!("期望 Settled 回执，实际：{other:?}"),
    };

    assert!(truncated_receipt.truncated);
    assert!(!truncated_receipt.steps.is_empty());
    assert!(truncated_receipt.steps.len() < stored.steps.len());
    assert_eq!(stored.steps[0].tool, "fs.list");
    assert_eq!(truncated_receipt.steps[0].tool, "fs.list");
    assert_eq!(
        truncated_receipt.steps,
        stored.steps[..truncated_receipt.steps.len()]
    );
    assert_eq!(truncated_receipt.invocation_id, stored.invocation_id);
    assert_eq!(truncated_receipt.tool, stored.tool);
    assert_eq!(truncated_receipt.catalog_digest, stored.catalog_digest);
    assert_eq!(truncated_receipt.labels, stored.labels);
    assert_eq!(truncated_receipt.outcome, stored.outcome);
    assert!(truncated_receipt.effects.is_empty());
}
