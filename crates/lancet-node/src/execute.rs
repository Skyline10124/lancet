// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! `act` 执行、交回载荷组装、字节上限核对、结局映射与回执构造。

use std::os::unix::net::UnixStream;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use lancet_node::act::{ACT_MEMORY_LIMIT_BYTES, ActError, ActRuntime, Diagnostic, Payload, Phase};
use lancet_protocol::{
    CatalogDigest, ExecuteOutcome, Frame, MAX_OUTPUT_LIMIT, Receipt, ReceiptOutcome,
    ReceiptSummary, RejectCode, Response, ToolError, write_frame,
};
use lancet_tool_decl::ActContext;

use crate::calls::{AdmittedCall, Ledger};

/// 受理校验未通过时的拒绝信息。
pub struct RejectedCall {
    pub code: RejectCode,
    pub message: String,
}

/// 按协议规则校验请求的目录摘要、工具名、`act` 脚本参数与输出上限；`CatalogMismatch` 优先于 `InvalidRequest`。
pub fn validate_execute(
    expected_digest: &CatalogDigest,
    catalog_digest: &CatalogDigest,
    tool: &str,
    arguments: &serde_json::Value,
    output_limit: u64,
) -> Result<String, RejectedCall> {
    if catalog_digest != expected_digest {
        return Err(RejectedCall {
            code: RejectCode::CatalogMismatch,
            message: format!("目录摘要不匹配：请求为 {catalog_digest}，节点为 {expected_digest}"),
        });
    }

    if tool != "act" {
        return Err(RejectedCall {
            code: RejectCode::InvalidRequest,
            message: format!("未知工具 `{tool}`，节点仅支持 `act`"),
        });
    }

    let script = parse_act_script(arguments).map_err(|reason| RejectedCall {
        code: RejectCode::InvalidRequest,
        message: reason.to_owned(),
    })?;

    if output_limit > MAX_OUTPUT_LIMIT {
        return Err(RejectedCall {
            code: RejectCode::InvalidRequest,
            message: format!("output_limit {output_limit} 超过上限 {MAX_OUTPUT_LIMIT}"),
        });
    }

    Ok(script)
}

fn parse_act_script(arguments: &serde_json::Value) -> Result<String, &'static str> {
    let Some(obj) = arguments.as_object() else {
        return Err("`arguments` 必须是仅含字符串字段 `script` 的 JSON 对象");
    };
    if obj.len() != 1 {
        return Err("`arguments` 必须仅包含 `script` 字段");
    }
    let Some(script_val) = obj.get("script") else {
        return Err("`arguments` 缺少 `script` 字段");
    };
    let Some(script) = script_val.as_str() else {
        return Err("`arguments.script` 必须是字符串");
    };
    Ok(script.to_owned())
}

/// 根据完整读到 `execute` 帧时的单调时钟与 `remaining_ms` 预算计算截止时间。
pub fn compute_deadline(received_at: Instant, remaining_ms: u64) -> Instant {
    received_at + Duration::from_millis(remaining_ms)
}

/// 在执行线程运行已受理的 `act` 调用，先持久结算再向连接写入 `execute_result`。
pub fn run_admitted(
    admitted: AdmittedCall,
    output_limit: u64,
    deadline: Instant,
    runtime: &ActRuntime,
    ledger: &Ledger,
    writer: &Mutex<UnixStream>,
    request_id: u64,
) {
    let empty_context = ActContext::default();
    let run_res = runtime.run(
        &admitted.script,
        &empty_context,
        |_, _| Err("没有可调用的 API 工具".to_owned()),
        deadline,
        admitted.cancelled,
    );
    let outcome = map_run_outcome(run_res, output_limit);
    let receipt = Receipt {
        invocation_id: admitted.invocation_id,
        tool: admitted.tool,
        catalog_digest: admitted.catalog_digest,
        labels: admitted.labels,
        outcome,
        steps: Vec::new(),
        effects: Vec::new(),
        truncated: false,
    };

    ledger.settle(&receipt.invocation_id, &receipt);

    let execute_outcome = to_execute_outcome(&receipt);
    let mut guard = writer.lock().expect("套接字写锁不应中毒");
    let _ = write_frame(
        &mut *guard,
        &Frame {
            request_id,
            message: Response::ExecuteResult {
                outcome: execute_outcome,
            },
        },
    );
}

fn map_run_outcome(run_res: Result<Payload, ActError>, output_limit: u64) -> ReceiptOutcome {
    match run_res {
        Ok(payload) => build_success_outcome(payload, output_limit),
        Err(ActError::Script(diag)) => ReceiptOutcome::Failed {
            error: ToolError::new("ScriptOrToolError", format_script_diagnostic(&diag)),
        },
        Err(ActError::MemoryLimit) => ReceiptOutcome::Failed {
            error: ToolError::new(
                "ScriptOrToolError",
                format!("解释器超出线性内存上限 {ACT_MEMORY_LIMIT_BYTES} 字节"),
            ),
        },
        Err(ActError::Deadline) => ReceiptOutcome::Deadline,
        Err(ActError::Cancelled) => ReceiptOutcome::Cancelled,
        Err(ActError::Component(err)) => ReceiptOutcome::Failed {
            error: ToolError::new("InternalError", format!("解释器组件调用失败：{err:#}")),
        },
    }
}

fn build_success_outcome(payload: Payload, output_limit: u64) -> ReceiptOutcome {
    let value: serde_json::Value = match serde_json::from_str(&payload.value_json) {
        Ok(value) => value,
        Err(err) => {
            return ReceiptOutcome::Failed {
                error: ToolError::new(
                    "InternalError",
                    format!("解析解释器返回值 JSON 失败：{err}"),
                ),
            };
        }
    };
    let content = serde_json::json!({
        "value": value,
        "printed": payload.printed,
    });
    let bytes = serde_json::to_vec(&content).expect("交回载荷 JSON Value 序列化不应失败");
    let actual_bytes = bytes.len() as u64;
    if actual_bytes > output_limit {
        return ReceiptOutcome::Failed {
            error: ToolError::new(
                "OutputLimitExceeded",
                format!("交回载荷 {actual_bytes} 字节超过 output_limit 上限 {output_limit} 字节"),
            ),
        };
    }
    ReceiptOutcome::Succeeded { content }
}

/// 脚本诊断错误消息截断前的最大正文字节数，避免 `ScriptOrToolError` 超出单帧上限。
const MAX_SCRIPT_DIAGNOSTIC_BYTES: usize = 16_384;
const TRUNCATED_DIAGNOSTIC_SUFFIX: &str = "\n（诊断过长，已截断）";

fn format_script_diagnostic(diag: &Diagnostic) -> String {
    let phase = match diag.phase {
        Phase::Parse => "parse",
        Phase::Check => "check",
        Phase::Eval => "eval",
    };
    let msg = match diag.line {
        Some(line) => format!("脚本在 {phase} 阶段第 {line} 行失败：{}", diag.message),
        None => format!("脚本在 {phase} 阶段失败：{}", diag.message),
    };
    truncate_diagnostic(msg)
}

fn truncate_diagnostic(mut msg: String) -> String {
    if msg.len() <= MAX_SCRIPT_DIAGNOSTIC_BYTES {
        return msg;
    }
    let end = msg.floor_char_boundary(MAX_SCRIPT_DIAGNOSTIC_BYTES);
    msg.truncate(end);
    msg.push_str(TRUNCATED_DIAGNOSTIC_SUFFIX);
    msg
}

fn to_execute_outcome(receipt: &Receipt) -> ExecuteOutcome {
    let summary = ReceiptSummary::from_receipt(receipt);
    match &receipt.outcome {
        ReceiptOutcome::Succeeded { content } => ExecuteOutcome::Succeeded {
            content: content.clone(),
            receipt_summary: summary,
        },
        ReceiptOutcome::Failed { error } => ExecuteOutcome::Failed {
            error: error.clone(),
            receipt_summary: Some(summary),
        },
        ReceiptOutcome::Cancelled => ExecuteOutcome::Cancelled {
            receipt_summary: Some(summary),
        },
        ReceiptOutcome::Deadline => ExecuteOutcome::Deadline {
            receipt_summary: Some(summary),
        },
        ReceiptOutcome::Indeterminate { reason } => ExecuteOutcome::Indeterminate {
            reason: reason.clone(),
            receipt_summary: Some(summary),
        },
    }
}
