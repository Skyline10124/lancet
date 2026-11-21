// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! LANCET `proc.run` API 工具组件。

wit_bindgen::generate!({
    world: "proc-run",
    path: "wit",
    generate_all,
});

use exports::lancet::proc_run::proc::Guest;
use lancet::host::handles::{ExecEnv, ProcOutput};

/// 非零退出诊断中每路输出保留的最大尾部字节数（4 KiB）。
const MAX_DIAGNOSTIC_TAIL_BYTES: usize = 4096;

struct ProcRun;

impl Guest for ProcRun {
    fn run(
        env: &ExecEnv,
        argv: Vec<String>,
        cwd: Option<String>,
        check: Option<bool>,
    ) -> Result<ProcOutput, String> {
        let output = env.run(&argv, cwd.as_deref())?;
        if check != Some(false) && output.code != 0 {
            return Err(format_nonzero_exit(&argv, &output));
        }
        Ok(output)
    }
}

fn format_nonzero_exit(argv: &[String], output: &ProcOutput) -> String {
    let program = argv.first().map(String::as_str).unwrap_or("");
    let mut msg = format!("进程 `{program}` 以非零退出码 {} 结束", output.code);
    append_tail(&mut msg, "stdout", &output.stdout);
    append_tail(&mut msg, "stderr", &output.stderr);
    msg
}

fn append_tail(out: &mut String, label: &str, text: &str) {
    if text.is_empty() {
        return;
    }
    let (tail, truncated) = tail_utf8(text, MAX_DIAGNOSTIC_TAIL_BYTES);
    if truncated {
        out.push_str(&format!("\n{label} 尾部（已截断，仅保留末段）：\n{tail}"));
    } else {
        out.push_str(&format!("\n{label}：\n{tail}"));
    }
}

fn tail_utf8(text: &str, max_bytes: usize) -> (&str, bool) {
    if text.len() <= max_bytes {
        return (text, false);
    }
    let mut start = text.len() - max_bytes;
    while start < text.len() && !text.is_char_boundary(start) {
        start += 1;
    }
    (&text[start..], true)
}

export!(ProcRun);
