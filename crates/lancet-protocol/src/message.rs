// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 五种协议请求、回复、工具描述、结局与错误码定义。

use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};

use crate::digest::CatalogDigest;
use crate::environment::{EffectKind, EnvironmentSummary, Mode};
use crate::frame::MAX_FRAME_BYTES;
use crate::receipt::{MAX_RECEIPT_SUMMARY_BYTES, Receipt, ReceiptSummary};

/// `execute.output_limit` 允许的最大字节数：8 MiB 单帧上限减去 16 KiB 摘要上限，再减去 `request_id = u64::MAX` 时成功回复帧的 132 字节固定 JSON 包装。
pub const MAX_OUTPUT_LIMIT: u64 = MAX_FRAME_BYTES as u64 - MAX_RECEIPT_SUMMARY_BYTES as u64 - 132;

/// 记账标签映射，节点对值保持不透明并原样写入回执。
pub type Labels = BTreeMap<String, serde_json::Value>;

/// 协议版本号（u32 整数标识；列表可携带未知版本用于 `hello` 协商）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ProtocolVersion(pub u32);

impl ProtocolVersion {
    /// 当前协议版本 1。
    pub const V1: Self = Self(1);

    /// 用整数构造协议版本号。
    pub const fn new(version: u32) -> Self {
        Self(version)
    }

    /// 返回底层整数版本号。
    pub const fn as_u32(self) -> u32 {
        self.0
    }
}

impl fmt::Display for ProtocolVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl From<u32> for ProtocolVersion {
    fn from(version: u32) -> Self {
        Self(version)
    }
}

impl From<ProtocolVersion> for u32 {
    fn from(version: ProtocolVersion) -> Self {
        version.0
    }
}

/// 节点在 `hello_ok` 中声明的协议能力参数。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Capabilities {
    /// 预算耗尽、收到 `cancel` 或断连后节点结算调用记录的宽限毫秒数。
    pub settle_grace_ms: u64,
    /// 已结算调用的完整回执明细保留毫秒数。
    pub receipt_retention_ms: u64,
}

/// 操作声明中的 effect 项。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Effect {
    pub handle: String,
    pub kind: EffectKind,
}

/// 操作声明中的 co-effect 项（表达 `Uses<h>` 或 `Uses<h, mode>`）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CoEffect {
    pub handle: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<Mode>,
}

/// 节点通过 `describe` 暴露的单个操作定义。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    pub input_schema: serde_json::Value,
    pub effects: Vec<Effect>,
    pub coeffects: Vec<CoEffect>,
}

/// 开放节点错误码与错误说明。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolError {
    pub code: String,
    pub message: String,
}

impl ToolError {
    /// `seam` 闸口拒绝跨越时的标准错误码。
    pub const CODE_SEAM_BLOCKED: &'static str = "SeamBlocked";

    /// 创建工具错误。
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }

    /// 构造 `SeamBlocked` 错误。
    pub fn seam_blocked(message: impl Into<String>) -> Self {
        Self::new(Self::CODE_SEAM_BLOCKED, message)
    }
}

/// 节点在 `execute` 受理检查阶段的拒绝码。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum RejectCode {
    DuplicateInvocation,
    CatalogMismatch,
    InvalidRequest,
}

impl RejectCode {
    /// 返回与线缆字段一致的错误码字符串，便于调用方把非重复拒绝映射为 [`ToolError`]。
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::DuplicateInvocation => "DuplicateInvocation",
            Self::CatalogMismatch => "CatalogMismatch",
            Self::InvalidRequest => "InvalidRequest",
        }
    }
}

/// 五种标准执行结局种类。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum OutcomeKind {
    Succeeded,
    Failed,
    Cancelled,
    Deadline,
    Indeterminate,
}

/// `execute_result` 携带的执行结局与回执摘要。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum ExecuteOutcome {
    Succeeded {
        content: serde_json::Value,
        receipt_summary: ReceiptSummary,
    },
    Failed {
        error: ToolError,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        receipt_summary: Option<ReceiptSummary>,
    },
    Cancelled {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        receipt_summary: Option<ReceiptSummary>,
    },
    Deadline {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        receipt_summary: Option<ReceiptSummary>,
    },
    Indeterminate {
        reason: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        receipt_summary: Option<ReceiptSummary>,
    },
}

impl ExecuteOutcome {
    /// 推导该结局对应的 [`OutcomeKind`]。
    pub const fn kind(&self) -> OutcomeKind {
        match self {
            Self::Succeeded { .. } => OutcomeKind::Succeeded,
            Self::Failed { .. } => OutcomeKind::Failed,
            Self::Cancelled { .. } => OutcomeKind::Cancelled,
            Self::Deadline { .. } => OutcomeKind::Deadline,
            Self::Indeterminate { .. } => OutcomeKind::Indeterminate,
        }
    }
}

/// `receipt` 操作返回的调用记录查询结果。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum ReceiptLookup {
    NeverExecuted,
    Pending,
    Settled { receipt: Receipt },
    Expired { outcome: OutcomeKind },
}

/// 调用方发往节点的五种协议请求。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Request {
    Hello {
        supported_versions: Vec<ProtocolVersion>,
    },
    Describe,
    Execute {
        invocation_id: String,
        catalog_digest: CatalogDigest,
        tool: String,
        arguments: serde_json::Value,
        remaining_ms: u64,
        output_limit: u64,
        labels: Labels,
    },
    Cancel {
        invocation_id: String,
    },
    Receipt {
        invocation_id: String,
    },
}

/// 节点返回调用方的协议回复。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Response {
    HelloOk {
        version: ProtocolVersion,
        node_id: String,
        display_name: String,
        capabilities: Capabilities,
    },
    DescribeOk {
        catalog_digest: CatalogDigest,
        environment_summary: EnvironmentSummary,
        tools: Vec<ToolSpec>,
    },
    ExecuteResult {
        outcome: ExecuteOutcome,
    },
    Rejected {
        code: RejectCode,
        message: String,
    },
    CancelOk,
    ReceiptResult {
        lookup: ReceiptLookup,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::environment::EnvironmentSummary;
    use crate::receipt::{ReceiptOutcome, ReceiptSummary, StepSummary};

    #[test]
    fn wire_tags_opaque_labels_seam_blocked_and_outcome_kind_derivation() {
        let mut labels = Labels::new();
        labels.insert("session".to_owned(), serde_json::json!("s-1"));
        labels.insert(
            "nested".to_owned(),
            serde_json::json!({"turn": 3, "tags": [true, null]}),
        );

        let digest = CatalogDigest::from_sha256_bytes([0x33; 32]);
        let req = Request::Execute {
            invocation_id: "inv-42".to_owned(),
            catalog_digest: digest.clone(),
            tool: "act".to_owned(),
            arguments: serde_json::json!({"script": "return 1"}),
            remaining_ms: 1000,
            output_limit: 65536,
            labels: labels.clone(),
        };
        let req_val = serde_json::to_value(&req).unwrap();
        assert_eq!(req_val["type"], "execute");
        assert_eq!(req_val["labels"]["nested"]["turn"], 3);
        let decoded_req: Request = serde_json::from_value(req_val).unwrap();
        assert_eq!(decoded_req, req, "labels 应原样保留任意不透明 JSON 值");

        // SeamBlocked 与 RejectCode 映射
        let seam_err = ToolError::seam_blocked("跨越未放行");
        assert_eq!(seam_err.code, ToolError::CODE_SEAM_BLOCKED);
        let receipt = Receipt {
            invocation_id: "inv-42".to_owned(),
            tool: "act".to_owned(),
            catalog_digest: digest,
            labels,
            outcome: ReceiptOutcome::Failed { error: seam_err },
            steps: vec![StepSummary {
                tool: "http.fetch".to_owned(),
                outcome: OutcomeKind::Failed,
            }],
            effects: vec![],
            truncated: false,
        };
        assert_eq!(receipt.outcome_kind(), OutcomeKind::Failed);

        let summary = ReceiptSummary::from_receipt(&receipt);
        let exec_outcome = ExecuteOutcome::Succeeded {
            content: serde_json::json!({"ok": true}),
            receipt_summary: summary,
        };
        assert_eq!(exec_outcome.kind(), OutcomeKind::Succeeded);

        let describe = Response::DescribeOk {
            catalog_digest: receipt.catalog_digest.clone(),
            environment_summary: EnvironmentSummary::new(vec![], vec![]).unwrap(),
            tools: vec![ToolSpec {
                name: "act".to_owned(),
                description: "stub".to_owned(),
                input_schema: serde_json::json!({"type": "object"}),
                effects: vec![],
                coeffects: vec![CoEffect {
                    handle: "ws".to_owned(),
                    mode: None,
                }],
            }],
        };
        let describe_val = serde_json::to_value(&describe).unwrap();
        assert_eq!(describe_val["type"], "describe_ok");
        assert!(
            describe_val["tools"][0]["coeffects"][0]
                .get("mode")
                .is_none()
        );

        let lookup_resp = Response::ReceiptResult {
            lookup: ReceiptLookup::Settled { receipt },
        };
        let lookup_val = serde_json::to_value(&lookup_resp).unwrap();
        assert_eq!(lookup_val["type"], "receipt_result");
        assert_eq!(lookup_val["lookup"]["kind"], "Settled");
        assert!(
            lookup_val["lookup"]["receipt"]
                .get("seam_crossings")
                .is_none(),
            "回执中不得记录 seam 跨越"
        );
    }

    #[test]
    fn max_output_limit_fits_largest_success_frame() {
        use crate::frame::{Frame, FrameError, MAX_FRAME_BYTES, write_frame};
        use crate::receipt::{MAX_RECEIPT_SUMMARY_BYTES, ReceiptOutcome, StepSummary};

        let digest = CatalogDigest::from_sha256_bytes([0x11; 32]);
        let probe_receipt = Receipt {
            invocation_id: "inv".to_owned(),
            tool: "act".to_owned(),
            catalog_digest: digest.clone(),
            labels: Labels::new(),
            outcome: ReceiptOutcome::Cancelled,
            steps: vec![StepSummary {
                tool: String::new(),
                outcome: OutcomeKind::Succeeded,
            }],
            effects: vec![],
            truncated: false,
        };
        let probe_summary = ReceiptSummary::from_receipt(&probe_receipt);
        let probe_len = serde_json::to_vec(&probe_summary).unwrap().len();
        let exact_receipt = Receipt {
            steps: vec![StepSummary {
                tool: "t".repeat(MAX_RECEIPT_SUMMARY_BYTES - probe_len),
                outcome: OutcomeKind::Succeeded,
            }],
            ..probe_receipt
        };
        let exact_summary = ReceiptSummary::from_receipt(&exact_receipt);
        assert_eq!(
            serde_json::to_vec(&exact_summary).unwrap().len(),
            MAX_RECEIPT_SUMMARY_BYTES
        );
        assert!(!exact_summary.is_truncated());

        // 构造紧凑 JSON 恰好占 MAX_OUTPUT_LIMIT 字节的字符串内容（含首尾双引号）。
        let exact_content = serde_json::Value::String("a".repeat(MAX_OUTPUT_LIMIT as usize - 2));
        assert_eq!(
            serde_json::to_vec(&exact_content).unwrap().len(),
            MAX_OUTPUT_LIMIT as usize
        );

        let exact_frame = Frame {
            request_id: u64::MAX,
            message: Response::ExecuteResult {
                outcome: ExecuteOutcome::Succeeded {
                    content: exact_content,
                    receipt_summary: exact_summary.clone(),
                },
            },
        };
        let mut wire = Vec::new();
        write_frame(&mut wire, &exact_frame).expect("MAX_OUTPUT_LIMIT 恰好应放进最大成功回复帧");
        assert_eq!(
            wire.len() - 4,
            MAX_FRAME_BYTES,
            "负载应恰好等于 8 MiB 单帧上限"
        );

        let over_content = serde_json::Value::String("a".repeat(MAX_OUTPUT_LIMIT as usize - 1));
        let over_frame = Frame {
            request_id: u64::MAX,
            message: Response::ExecuteResult {
                outcome: ExecuteOutcome::Succeeded {
                    content: over_content,
                    receipt_summary: exact_summary,
                },
            },
        };
        let mut over_wire = Vec::new();
        let err = write_frame(&mut over_wire, &over_frame)
            .expect_err("超出 MAX_OUTPUT_LIMIT 1 字节时成功回复帧应超过 8 MiB");
        assert!(
            matches!(err, FrameError::FrameTooLarge { .. }),
            "应返回 FrameTooLarge，实际为：{err:?}"
        );
    }
}
