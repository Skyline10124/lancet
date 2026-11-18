// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! `wasmtime` 引擎共用配置。
//!
//! 本文件同时被 `build.rs`（预编译）与节点运行时（反序列化加载）引入，保证配置一致。

use wasmtime::{Config, WasmBacktraceDetails};

/// 构造 `act` 解释器组件使用的 `wasmtime::Config`。
pub fn engine_config() -> Config {
    let mut config = Config::new();
    config.wasm_component_model(true);
    config.epoch_interruption(true);
    config.wasm_backtrace_details(WasmBacktraceDetails::Disable);
    config
}
