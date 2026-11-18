// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! LANCET `act` 工具的 Starlark 解释器 WASM 组件。

mod globals;
mod script;
mod text;

wit_bindgen::generate!({
    world: "act",
    path: "wit",
    generate_all,
});

use exports::lancet::act::interpreter::{Diagnostic, Guest, Payload};

struct Interpreter;

impl Guest for Interpreter {
    fn run(script: String) -> Result<Payload, Diagnostic> {
        let globals = globals::build_globals();
        script::run_script(&script, &globals)
    }

    fn builtin_stub() -> String {
        globals::builtin_stub()
    }
}

export!(Interpreter);
