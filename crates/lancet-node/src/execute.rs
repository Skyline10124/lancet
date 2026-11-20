// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! `act` 执行、交回载荷组装、字节上限核对、`commit()` 发布事务、结局映射与回执构造。

use std::os::unix::net::UnixStream;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use lancet_node::act::{
    ACT_MEMORY_LIMIT_BYTES, ActError, ActHost, ActRuntime, Diagnostic, DispatchArgument, Payload,
    Phase,
};
use lancet_node::dispatch::{Dispatcher, SyncError, WaitInterrupt};
use lancet_node::shadow::ShadowInterrupt;
use lancet_node::shadow::publish::{
    PublishOutcome, PublishPrep, PublishPrepareError, call_record_hash, format_conflicts,
    prepare_publish,
};
use lancet_protocol::{
    CatalogDigest, EffectEntry, ExecuteOutcome, Frame, MAX_OUTPUT_LIMIT, OutcomeKind, Receipt,
    ReceiptOutcome, ReceiptSummary, RejectCode, Response, StepSummary, ToolError, write_frame,
};

use crate::calls::{AdmittedCall, CriticalSectionGate, Ledger};

/// 带可写目录句柄的 `act` 为待定改动提示固定预留的字节数。
pub const PENDING_HINT_RESERVE_BYTES: u64 = 84;

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
    has_writable: bool,
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

    if has_writable && output_limit < PENDING_HINT_RESERVE_BYTES {
        return Err(RejectedCall {
            code: RejectCode::InvalidRequest,
            message: format!(
                "带可写句柄的调用要求 output_limit 至少为 {PENDING_HINT_RESERVE_BYTES} 字节，实际为 {output_limit}"
            ),
        });
    }

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

struct NodeActHost {
    dispatcher: Dispatcher,
    deadline: Instant,
    cancelled: Arc<AtomicBool>,
    steps: Arc<Mutex<Vec<StepSummary>>>,
    effects: Arc<Mutex<Vec<EffectEntry>>>,
}

impl ActHost for NodeActHost {
    fn call(
        &mut self,
        tool_id: &str,
        args: &[DispatchArgument],
    ) -> wasmtime::Result<Result<String, String>> {
        self.dispatcher.call(
            tool_id,
            args,
            self.deadline,
            Arc::clone(&self.cancelled),
            &self.steps,
            &self.effects,
        )
    }

    fn discard(&mut self) -> wasmtime::Result<Result<(), String>> {
        self.dispatcher.discard(
            self.deadline,
            Arc::clone(&self.cancelled),
            &self.steps,
            &self.effects,
        )
    }

    fn commit(&mut self) -> wasmtime::Result<Result<(), String>> {
        self.dispatcher.commit(&self.steps)
    }
}

/// 在执行线程运行已受理的 `act` 调用：取可写锁、同步宿主、执行脚本、按需发布、持久结算、放锁，最后写回套接字。
#[allow(clippy::too_many_arguments)]
pub fn run_admitted(
    admitted: AdmittedCall,
    output_limit: u64,
    deadline: Instant,
    runtime: &ActRuntime,
    dispatcher: &Dispatcher,
    ledger: &Ledger,
    writer: &Mutex<UnixStream>,
    request_id: u64,
) {
    let has_writable = dispatcher.has_writable_dirs();

    // 1. 按槽位顺序取得全部可写目录的互斥锁（无等待且只读配置直接返回空锁）。
    let locks = match dispatcher.acquire_writable_locks(deadline, &admitted.cancelled) {
        Ok(locks) => locks,
        Err(intr) => {
            let receipt = build_interrupted_receipt(admitted, intr);
            ledger.settle(&receipt.invocation_id, &receipt);
            write_execute_result(writer, request_id, &receipt);
            return;
        }
    };

    // 2. 持锁后把宿主改动同步进影子与基准并重算冲突。
    if let Err(err) = dispatcher.sync_writable_dirs(deadline, &admitted.cancelled) {
        let receipt = match err {
            SyncError::Deadline => build_interrupted_receipt(admitted, WaitInterrupt::Deadline),
            SyncError::Cancelled => build_interrupted_receipt(admitted, WaitInterrupt::Cancelled),
            SyncError::Failed(msg) => {
                let pending_count = if has_writable {
                    dispatcher.visible_pending_count()
                } else {
                    0
                };
                let hint = format_pending_hint(pending_count);
                Receipt {
                    invocation_id: admitted.invocation_id,
                    tool: admitted.tool,
                    catalog_digest: admitted.catalog_digest,
                    labels: admitted.labels,
                    outcome: ReceiptOutcome::Failed {
                        error: ToolError::new(
                            "InternalError",
                            append_hint_line(format!("同步宿主改动失败：{msg}"), hint.as_deref()),
                        ),
                    },
                    steps: Vec::new(),
                    effects: Vec::new(),
                    truncated: false,
                }
            }
        };
        ledger.settle(&receipt.invocation_id, &receipt);
        drop(locks);
        write_execute_result(writer, request_id, &receipt);
        return;
    }

    let steps = Arc::new(Mutex::new(Vec::new()));
    let effects = Arc::new(Mutex::new(Vec::new()));
    let host = NodeActHost {
        dispatcher: dispatcher.clone(),
        deadline,
        cancelled: Arc::clone(&admitted.cancelled),
        steps: Arc::clone(&steps),
        effects: Arc::clone(&effects),
    };

    let run_res = runtime.run(
        &admitted.script,
        dispatcher.context(),
        host,
        deadline,
        Arc::clone(&admitted.cancelled),
    );

    let recorded_steps = std::mem::take(&mut *steps.lock().expect("步骤互斥锁不应中毒"));
    let mut recorded_effects = std::mem::take(&mut *effects.lock().expect("effect 互斥锁不应中毒"));
    let commit_marked = recorded_steps
        .iter()
        .any(|s| s.tool == "commit" && s.outcome == OutcomeKind::Succeeded);

    let outcome = match run_res {
        Err(err) => {
            let pending_count = if has_writable {
                dispatcher.visible_pending_count()
            } else {
                0
            };
            map_run_error(err, pending_count)
        }
        Ok(payload) => settle_script_payload(
            payload,
            output_limit,
            has_writable,
            commit_marked,
            &admitted.invocation_id,
            deadline,
            &admitted.cancelled,
            dispatcher,
            ledger,
            &mut recorded_effects,
        ),
    };

    let receipt = Receipt {
        invocation_id: admitted.invocation_id,
        tool: admitted.tool,
        catalog_digest: admitted.catalog_digest,
        labels: admitted.labels,
        outcome,
        steps: recorded_steps,
        effects: recorded_effects,
        truncated: false,
    };

    ledger.settle(&receipt.invocation_id, &receipt);
    // 结算完成后先放锁，再写回套接字。
    drop(locks);
    write_execute_result(writer, request_id, &receipt);
}

#[allow(clippy::too_many_arguments)]
fn settle_script_payload(
    payload: Payload,
    output_limit: u64,
    has_writable: bool,
    commit_marked: bool,
    invocation_id: &str,
    deadline: Instant,
    cancelled: &AtomicBool,
    dispatcher: &Dispatcher,
    ledger: &Ledger,
    recorded_effects: &mut Vec<EffectEntry>,
) -> ReceiptOutcome {
    // 1. 先检查交回载荷 JSON 合法性与字节上限；不通过时绝不发布，草稿保留。
    let (value, mut printed) = match check_payload_budget(payload, output_limit, has_writable) {
        Ok(pair) => pair,
        Err(err_outcome) => {
            let pending_count = if has_writable {
                dispatcher.visible_pending_count()
            } else {
                0
            };
            let hint = format_pending_hint(pending_count);
            return match err_outcome {
                PayloadCheckError::JsonParse(msg) => ReceiptOutcome::Failed {
                    error: ToolError::new("InternalError", append_hint_line(msg, hint.as_deref())),
                },
                PayloadCheckError::LimitExceeded(msg) => ReceiptOutcome::Failed {
                    error: ToolError::new(
                        "OutputLimitExceeded",
                        append_hint_line(msg, hint.as_deref()),
                    ),
                },
            };
        }
    };

    // 2. 未调用 commit() 或无任何可写目录：沿用 4a 结算逻辑。
    if !commit_marked || !has_writable {
        let pending_count = if has_writable {
            dispatcher.visible_pending_count()
        } else {
            0
        };
        if let Some(hint) = format_pending_hint(pending_count) {
            printed.push_str(&hint);
        }
        return ReceiptOutcome::Succeeded {
            content: serde_json::json!({
                "value": value,
                "printed": printed,
            }),
        };
    }

    // 3. 临界区之外的发布准备（可响应 deadline 与 cancelled）。
    let call_hash = call_record_hash(invocation_id);
    let shadows = dispatcher.writable_shadows();
    let prepared = match prepare_publish(
        ledger.state_dir(),
        &call_hash,
        &shadows,
        Some(deadline),
        Some(cancelled),
    ) {
        Ok(PublishPrep::Empty) => {
            return ReceiptOutcome::Succeeded {
                content: serde_json::json!({
                    "value": value,
                    "printed": printed,
                }),
            };
        }
        Ok(PublishPrep::Conflicts(conflicts)) => {
            let pending_count = dispatcher.visible_pending_count();
            let hint = format_pending_hint(pending_count);
            let msg = format_conflicts(&conflicts, hint.as_deref());
            return ReceiptOutcome::Failed {
                error: ToolError::new("Conflict", append_hint_line(msg, hint.as_deref())),
            };
        }
        Err(PublishPrepareError::Interrupted(ShadowInterrupt::Cancelled)) => {
            return ReceiptOutcome::Cancelled;
        }
        Err(PublishPrepareError::Interrupted(ShadowInterrupt::Deadline)) => {
            return ReceiptOutcome::Deadline;
        }
        Err(PublishPrepareError::Failed(msg)) => {
            let pending_count = dispatcher.visible_pending_count();
            let hint = format_pending_hint(pending_count);
            return ReceiptOutcome::Failed {
                error: ToolError::new(
                    "InternalError",
                    append_hint_line(format!("准备发布失败：{msg}"), hint.as_deref()),
                ),
            };
        }
        Ok(PublishPrep::Ready(prepared)) => prepared,
    };

    // 4. 临界区准入闸门：与 cancel/cancel_connection 串行核对并持久落盘 Undo 日志。
    //    写日志任何失败均已在 enter_publish_critical_section 内按账本失败终止进程，绝不在进程内清理暂存。
    match ledger.enter_publish_critical_section(invocation_id, deadline, prepared.undo_journal()) {
        CriticalSectionGate::Cancelled => {
            prepared.cleanup_staging();
            return ReceiptOutcome::Cancelled;
        }
        CriticalSectionGate::Deadline => {
            prepared.cleanup_staging();
            return ReceiptOutcome::Deadline;
        }
        CriticalSectionGate::Entered => {}
    }

    // 5. 临界区内执行发布事务（不再读取 cancelled）；致命错误交由 fatal_error 终止进程待重启恢复。
    match prepared.execute(&shadows, |journal| {
        ledger.update_publish_journal(invocation_id, journal)
    }) {
        Ok(PublishOutcome::Committed {
            effects: pub_effects,
        }) => {
            let violation = dispatcher.validate_publish_effects(&pub_effects).err();
            recorded_effects.extend(pub_effects);
            if let Some(msg) = violation {
                let pending_count = dispatcher.visible_pending_count();
                let hint = format_pending_hint(pending_count);
                return ReceiptOutcome::Failed {
                    error: ToolError::new("InternalError", append_hint_line(msg, hint.as_deref())),
                };
            }
            ReceiptOutcome::Succeeded {
                content: serde_json::json!({
                    "value": value,
                    "printed": printed,
                }),
            }
        }
        Ok(PublishOutcome::RolledBackConflict { conflicts }) => {
            let pending_count = dispatcher.visible_pending_count();
            let hint = format_pending_hint(pending_count);
            let msg = format_conflicts(&conflicts, hint.as_deref());
            ReceiptOutcome::Failed {
                error: ToolError::new("Conflict", append_hint_line(msg, hint.as_deref())),
            }
        }
        Ok(PublishOutcome::RolledBackIoError { message }) => {
            let pending_count = dispatcher.visible_pending_count();
            let hint = format_pending_hint(pending_count);
            ReceiptOutcome::Failed {
                error: ToolError::new(
                    "InternalError",
                    append_hint_line(format!("发布失败并已撤回：{message}"), hint.as_deref()),
                ),
            }
        }
        Ok(PublishOutcome::Indeterminate {
            reason,
            effects: pub_effects,
        }) => {
            let violation = dispatcher.validate_publish_effects(&pub_effects).err();
            recorded_effects.extend(pub_effects);
            if let Some(msg) = violation {
                let pending_count = dispatcher.visible_pending_count();
                let hint = format_pending_hint(pending_count);
                return ReceiptOutcome::Failed {
                    error: ToolError::new("InternalError", append_hint_line(msg, hint.as_deref())),
                };
            }
            ReceiptOutcome::Indeterminate { reason }
        }
        Err(fatal) => crate::calls::fatal_error(fatal.0),
    }
}

enum PayloadCheckError {
    JsonParse(String),
    LimitExceeded(String),
}

fn check_payload_budget(
    payload: Payload,
    output_limit: u64,
    has_writable: bool,
) -> Result<(serde_json::Value, String), PayloadCheckError> {
    let value: serde_json::Value = serde_json::from_str(&payload.value_json).map_err(|err| {
        PayloadCheckError::JsonParse(format!("解析解释器返回值 JSON 失败：{err}"))
    })?;
    let check_content = serde_json::json!({
        "value": &value,
        "printed": &payload.printed,
    });
    let bytes = serde_json::to_vec(&check_content).expect("交回载荷 JSON Value 序列化不应失败");
    let actual_bytes = bytes.len() as u64;
    let effective_limit = if has_writable {
        output_limit.saturating_sub(PENDING_HINT_RESERVE_BYTES)
    } else {
        output_limit
    };
    if actual_bytes > effective_limit {
        let msg = if has_writable {
            format!(
                "交回载荷 {actual_bytes} 字节超过 output_limit 扣除 {PENDING_HINT_RESERVE_BYTES} 字节预留后的上限 {effective_limit} 字节"
            )
        } else {
            format!("交回载荷 {actual_bytes} 字节超过 output_limit 上限 {output_limit} 字节")
        };
        return Err(PayloadCheckError::LimitExceeded(msg));
    }
    Ok((value, payload.printed))
}

fn build_interrupted_receipt(admitted: AdmittedCall, intr: WaitInterrupt) -> Receipt {
    let outcome = match intr {
        WaitInterrupt::Cancelled => ReceiptOutcome::Cancelled,
        WaitInterrupt::Deadline => ReceiptOutcome::Deadline,
    };
    Receipt {
        invocation_id: admitted.invocation_id,
        tool: admitted.tool,
        catalog_digest: admitted.catalog_digest,
        labels: admitted.labels,
        outcome,
        steps: Vec::new(),
        effects: Vec::new(),
        truncated: false,
    }
}

fn write_execute_result(writer: &Mutex<UnixStream>, request_id: u64, receipt: &Receipt) {
    let execute_outcome = to_execute_outcome(receipt);
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

fn format_pending_hint(pending_count: usize) -> Option<String> {
    if pending_count == 0 {
        None
    } else {
        Some(format!(
            "待定文件 {pending_count} 个。用 commit() 发布或 discard() 丢弃。\n"
        ))
    }
}

fn append_hint_line(mut message: String, hint: Option<&str>) -> String {
    if let Some(hint) = hint {
        message.push('\n');
        message.push_str(hint);
    }
    message
}

fn map_run_error(err: ActError, pending_count: usize) -> ReceiptOutcome {
    let hint = format_pending_hint(pending_count);
    match err {
        ActError::Script(diag) => ReceiptOutcome::Failed {
            error: ToolError::new(
                "ScriptOrToolError",
                format_script_diagnostic(&diag, hint.as_deref()),
            ),
        },
        ActError::MemoryLimit => ReceiptOutcome::Failed {
            error: ToolError::new(
                "ScriptOrToolError",
                append_hint_line(
                    format!("解释器超出线性内存上限 {ACT_MEMORY_LIMIT_BYTES} 字节"),
                    hint.as_deref(),
                ),
            ),
        },
        ActError::Deadline => ReceiptOutcome::Deadline,
        ActError::Cancelled => ReceiptOutcome::Cancelled,
        ActError::Component(err) => ReceiptOutcome::Failed {
            error: ToolError::new(
                "InternalError",
                append_hint_line(format!("解释器组件调用失败：{err:#}"), hint.as_deref()),
            ),
        },
    }
}

/// 脚本诊断错误消息截断前的最大正文字节数，避免 `ScriptOrToolError` 超出单帧上限。
const MAX_SCRIPT_DIAGNOSTIC_BYTES: usize = 16_384;
const TRUNCATED_DIAGNOSTIC_SUFFIX: &str = "\n（诊断过长，已截断）";

fn format_script_diagnostic(diag: &Diagnostic, pending_hint: Option<&str>) -> String {
    let phase = match diag.phase {
        Phase::Parse => "parse",
        Phase::Check => "check",
        Phase::Eval => "eval",
    };
    let msg = match diag.line {
        Some(line) => format!("脚本在 {phase} 阶段第 {line} 行失败：{}", diag.message),
        None => format!("脚本在 {phase} 阶段失败：{}", diag.message),
    };
    let reserve = pending_hint.map_or(0, |h| 1 + h.len());
    let truncated = truncate_diagnostic(msg, MAX_SCRIPT_DIAGNOSTIC_BYTES.saturating_sub(reserve));
    append_hint_line(truncated, pending_hint)
}

fn truncate_diagnostic(mut msg: String, limit: usize) -> String {
    if msg.len() <= limit {
        return msg;
    }
    let end = msg.floor_char_boundary(limit);
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
