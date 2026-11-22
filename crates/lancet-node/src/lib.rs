// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! LANCET 节点库导出接口。

/// 节点在 `hello_ok` 中声明的结算宽限毫秒数，也用作执行环境收尾的统一结算宽限。
pub const SETTLE_GRACE_MS: u64 = 5_000;

pub mod act;
pub mod dispatch;
pub mod exec_env;
pub mod read_dir;
pub mod tools;
