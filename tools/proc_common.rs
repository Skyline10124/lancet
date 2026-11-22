// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! `proc.run` 与 `proc.pipe` 共用的 `check` 判定与非零退出尾部诊断。

/// 非零退出诊断中每路输出保留的最大尾部字节数（4 KiB）。
const MAX_DIAGNOSTIC_TAIL_BYTES: usize = 4096;

/// 按 `check` 规则核对退出码；`check` 省略或为 `true` 且非零退出时返回带两路有界尾部的错误。
/// `program` 为 `Some` 表示单进程调用，`None` 表示多阶段管道调用。
pub(crate) fn check_exit(
    check: Option<bool>,
    code: i32,
    stdout: &str,
    stderr: &str,
    program: Option<&str>,
) -> Result<(), String> {
    if check != Some(false) && code != 0 {
        let mut msg = match program {
            Some(prog) => format!("进程 `{prog}` 以非零退出码 {code} 结束"),
            None => format!("管道以非零退出码 {code} 结束"),
        };
        append_tail(&mut msg, "stdout", stdout);
        append_tail(&mut msg, "stderr", stderr);
        return Err(msg);
    }
    Ok(())
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
