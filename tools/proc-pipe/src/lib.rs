// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! LANCET `proc.pipe` API 工具组件。

wit_bindgen::generate!({
    world: "proc-pipe",
    path: "wit",
    generate_all,
});

#[path = "../../proc_common.rs"]
mod proc_common;

use exports::lancet::proc_pipe::proc::Guest;
use lancet::host::handles::{ExecEnv, ProcOutput};

struct ProcPipe;

impl Guest for ProcPipe {
    fn pipe(
        env: &ExecEnv,
        stages: Vec<Vec<String>>,
        cwd: Option<String>,
        check: Option<bool>,
    ) -> Result<ProcOutput, String> {
        let output = env.pipe(&stages, cwd.as_deref())?;
        proc_common::check_exit(check, output.code, &output.stdout, &output.stderr, None)?;
        Ok(output)
    }
}

export!(ProcPipe);
