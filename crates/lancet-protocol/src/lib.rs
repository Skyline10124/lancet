// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! LANCET 外部上下文节点协议的纯协议库。
//!
//! 负责线缆帧编解码、五种协议操作消息、环境简述与 effect/co-effect 类型、
//! 回执与摘要裁剪，以及 `catalog_digest` 规范摘要计算；不包含节点服务、
//! 调用记录账本、持久化、运行时或协调层适配器逻辑。

pub mod digest;
pub mod environment;
pub mod frame;
pub mod message;
pub mod receipt;

pub use digest::{CatalogDigest, CatalogDigestError, catalog_digest};
pub use environment::{
    EffectKind, EnvironmentError, EnvironmentSummary, Handle, MAX_ENVIRONMENT_SUMMARY_BYTES, Mode,
    Seam, SeamDirection, SeamPolicy,
};
pub use frame::{Frame, FrameError, MAX_FRAME_BYTES, read_frame, write_frame};
pub use message::{
    Capabilities, CoEffect, Effect, ExecuteOutcome, Labels, OutcomeKind, ProtocolVersion,
    ReceiptLookup, RejectCode, Request, Response, ToolError, ToolSpec,
};
pub use receipt::{
    Change, EffectEntry, EffectEntryError, MAX_RECEIPT_SUMMARY_BYTES, Receipt, ReceiptOutcome,
    ReceiptSummary, ReceiptSummaryError, Residue, StepSummary, is_residue_allowed,
};
