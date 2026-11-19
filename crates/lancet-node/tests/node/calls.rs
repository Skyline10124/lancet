// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use std::collections::BTreeMap;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::thread;
use std::time::{Duration, Instant};

use lancet_protocol::{
    CatalogDigest, ExecuteOutcome, Frame, Labels, MAX_OUTPUT_LIMIT, OutcomeKind, Receipt,
    ReceiptLookup, ReceiptOutcome, RejectCode, Request, Response, read_frame, write_frame,
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
            "[node]\ndisplay_name = \"调用测试节点\"\nstate_dir = {state_dir:?}\n{retention_line}\n[[dir]]\nname = \"app\"\npath = {app_dir:?}\naccess = \"write\"\n",
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
