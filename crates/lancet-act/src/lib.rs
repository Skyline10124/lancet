// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! LANCET `act` 工具使用的沙箱化 Starlark 解释器 WASM 组件。

mod globals;
mod handles;
mod script;
mod text;

wit_bindgen::generate!({
    world: "act",
    path: "wit",
    generate_all,
});

use lancet_tool_decl::ActContext;

use exports::lancet::act::interpreter::{Diagnostic, Guest, Payload};

struct Interpreter;

impl Guest for Interpreter {
    fn run(script: String, context_json: String) -> Result<Payload, Diagnostic> {
        let context: ActContext =
            serde_json::from_str(&context_json).expect("解析调用上下文 context-json 失败");
        let globals = globals::build_globals(&context);
        script::run_script(&script, &globals)
    }

    fn builtin_stub() -> String {
        globals::builtin_stub()
    }
}

export!(Interpreter);
