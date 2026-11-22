// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! LANCET `proc.run` API 工具组件。

wit_bindgen::generate!({
    world: "proc-run",
    path: "wit",
    generate_all,
});

#[path = "../../proc_common.rs"]
mod proc_common;

use exports::lancet::proc_run::proc::Guest;
use lancet::host::handles::{ExecEnv, ProcOutput};

struct ProcRun;

impl Guest for ProcRun {
    fn run(
        env: &ExecEnv,
        argv: Vec<String>,
        cwd: Option<String>,
        check: Option<bool>,
    ) -> Result<ProcOutput, String> {
        let output = env.run(&argv, cwd.as_deref())?;
        let program = argv.first().map(String::as_str).unwrap_or("");
        proc_common::check_exit(
            check,
            output.code,
            &output.stdout,
            &output.stderr,
            Some(program),
        )?;
        Ok(output)
    }
}

export!(ProcRun);
