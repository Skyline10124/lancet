// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Starlark 脚本包裹、解析、静态检查、求值、诊断映射与修复提示。

use std::cell::RefCell;
use std::collections::HashMap;
use std::fmt::Write as _;

use starlark::PrintHandler;
use starlark::environment::{Globals, Module};
use starlark::eval::Evaluator;
use starlark::syntax::{AstModule, Dialect};
use starlark::typing::AstModuleTypecheck as _;
use starlark_syntax::call_stack::CallStack;
use starlark_syntax::codemap::{CodeMap, FileSpan, Pos, ResolvedPos, Span};
use starlark_syntax::frame::Frame;
use starlark_syntax::lexer::{Lexer, Token};
use starlark_syntax::span_display::span_display;

use crate::exports::lancet::act::interpreter::{Diagnostic, Payload, Phase};

/// 包裹用户脚本的保留入口函数名（不以下划线开头，以便 `Module::get` 按公开符号读取）。
pub(crate) const ENTRY_FN: &str = "lancet_act_entry__";

/// 诊断信息中的虚拟脚本文件名。
const SCRIPT_NAME: &str = "act.star";

/// 构造 `act` 使用的 Starlark 方言配置。
pub fn dialect() -> Dialect {
    Dialect {
        enable_top_level_stmt: true,
        enable_load: false,
        enable_f_strings: false,
        ..Dialect::Standard
    }
}

/// 解析、静态检查并在隔离的求值器中运行一段 Starlark 脚本。
pub fn run_script(script: &str, globals: &Globals) -> Result<Payload, Diagnostic> {
    let dialect = dialect();
    let wrapped = WrappedScript::wrap(script, &dialect);

    let ast = AstModule::parse(SCRIPT_NAME, wrapped.source.clone(), &dialect).map_err(|err| {
        let mut diag = wrapped.to_diagnostic(Phase::Parse, &err);
        if let Some(hint) = repair_hint(script, &dialect, diag.line) {
            if !diag.message.ends_with('\n') {
                diag.message.push('\n');
            }
            diag.message.push_str(hint);
        }
        diag
    })?;

    let (type_errors, ..) = ast.clone().typecheck(globals, &HashMap::new());
    if let Some(first) = type_errors.into_iter().next() {
        return Err(wrapped.to_diagnostic(Phase::Check, &first));
    }

    let printer = PrintBuffer(RefCell::new(String::new()));
    let value_json = Module::with_temp_heap(|module| {
        let mut eval = Evaluator::new(&module);
        eval.set_print_handler(&printer);
        eval.eval_module(ast, globals)
            .map_err(|err| wrapped.to_diagnostic(Phase::Eval, &err))?;
        let entry = module.get(ENTRY_FN).expect("入口函数应已定义");
        let value = eval
            .eval_function(entry, &[], &[])
            .map_err(|err| wrapped.to_diagnostic(Phase::Eval, &err))?;
        value.to_json().map_err(|err| Diagnostic {
            phase: Phase::Eval,
            line: None,
            message: err.to_string(),
        })
    })?;

    Ok(Payload {
        value_json,
        printed: printer.0.into_inner(),
    })
}

struct PrintBuffer(RefCell<String>);

impl PrintHandler for PrintBuffer {
    fn println(&self, text: &str) -> starlark::Result<()> {
        let mut buf = self.0.borrow_mut();
        buf.push_str(text);
        buf.push('\n');
        Ok(())
    }
}

struct WrappedScript {
    orig_codemap: CodeMap,
    orig_line_count: usize,
    indented: Vec<bool>,
    source: String,
}

impl WrappedScript {
    fn wrap(script: &str, dialect: &Dialect) -> Self {
        let orig_codemap = CodeMap::new(SCRIPT_NAME.to_owned(), script.to_owned());
        let lines: Vec<&str> = if script.is_empty() {
            Vec::new()
        } else {
            script.split_inclusive('\n').collect()
        };
        let orig_line_count = lines.len();
        let mut inside_string_line = vec![false; orig_line_count];
        let mut has_code = false;

        for item in Lexer::new(script, dialect, orig_codemap.clone()) {
            match item {
                Ok((start, token, end)) => match token {
                    Token::Comment(_) | Token::Newline | Token::Indent | Token::Dedent => {}
                    Token::String(_) | Token::Bytes(_) => {
                        has_code = true;
                        mark_multiline_lines(&orig_codemap, start, end, &mut inside_string_line);
                    }
                    _ => {
                        has_code = true;
                    }
                },
                Err(err) => {
                    has_code = true;
                    let e = err.into_error();
                    if let Some(fs) = e.span() {
                        let r = fs.resolve_span();
                        for flag in inside_string_line
                            .iter_mut()
                            .take((r.end.line + 1).min(orig_line_count))
                            .skip(r.begin.line + 1)
                        {
                            *flag = true;
                        }
                    }
                    // 遇到首个词法错误即结束预扫描，避免 Lexer 在不前进位置重复报错。
                    break;
                }
            }
        }

        let mut source = String::with_capacity(script.len() + lines.len() * 4 + 40);
        source.push_str("def ");
        source.push_str(ENTRY_FN);
        source.push_str("():\n");

        let mut indented = Vec::with_capacity(orig_line_count);
        if !has_code {
            source.push_str("    pass\n");
            indented.resize(orig_line_count, true);
        } else {
            for (line, &in_string) in lines.iter().zip(&inside_string_line) {
                let add_indent = !in_string;
                indented.push(add_indent);
                if add_indent {
                    source.push_str("    ");
                }
                source.push_str(line);
            }
            if !source.ends_with('\n') {
                source.push('\n');
            }
        }

        Self {
            orig_codemap,
            orig_line_count,
            indented,
            source,
        }
    }

    fn to_diagnostic(&self, phase: Phase, err: &starlark::Error) -> Diagnostic {
        let mapped_span = err.span().map(|fs| self.map_file_span(fs));
        let line = mapped_span
            .as_ref()
            .map(|fs| (fs.resolve_span().begin.line + 1) as u32);

        let frames: Vec<Frame> = err
            .call_stack()
            .frames
            .iter()
            .enumerate()
            .filter(|(idx, frame)| !(*idx == 0 && frame.name == ENTRY_FN))
            .map(|(_, frame)| Frame {
                name: frame.name.clone(),
                location: frame.location.as_ref().map(|loc| {
                    if loc.filename() == SCRIPT_NAME {
                        self.map_file_span(loc)
                    } else {
                        loc.clone()
                    }
                }),
            })
            .collect();

        let label = err.without_diagnostic().to_string();
        let message = if mapped_span.is_some() || !frames.is_empty() {
            let mut out = String::new();
            let call_stack = CallStack { frames };
            let _ = write!(out, "{call_stack}");
            let display = span_display(mapped_span.as_ref().map(FileSpan::as_ref), &label, false);
            let _ = writeln!(out, "{display}");
            out
        } else {
            label
        };

        Diagnostic {
            phase,
            line,
            message,
        }
    }

    fn map_file_span(&self, fs: &FileSpan) -> FileSpan {
        if fs.filename() != SCRIPT_NAME {
            return fs.clone();
        }
        let r = fs.resolve_span();
        let begin = self.map_pos(r.begin);
        let end = self.map_pos(r.end).max(begin);
        self.orig_codemap.file_span(Span::new(begin, end))
    }

    fn map_pos(&self, pos: ResolvedPos) -> Pos {
        if self.orig_line_count == 0 || pos.line == 0 {
            return Pos::new(0);
        }
        let (orig_line, orig_col) = if pos.line > self.orig_line_count {
            (self.orig_line_count - 1, usize::MAX)
        } else {
            let line = pos.line - 1;
            let col = if self.indented[line] {
                pos.column.saturating_sub(4)
            } else {
                pos.column
            };
            (line, col)
        };
        let line_span = self.orig_codemap.line_span(orig_line);
        let line_text = self.orig_codemap.source_span(line_span);
        let trimmed_len = line_text.trim_end_matches(['\r', '\n']).len();
        let byte_col = if orig_col == usize::MAX {
            trimmed_len
        } else {
            line_text[..trimmed_len]
                .char_indices()
                .nth(orig_col)
                .map_or(trimmed_len, |(idx, _)| idx)
        };
        Pos::new(line_span.begin().get() + byte_col as u32)
    }
}

fn mark_multiline_lines(codemap: &CodeMap, start: usize, end: usize, inside: &mut [bool]) {
    let span = Span::new(Pos::new(start as u32), Pos::new(end as u32));
    let resolved = codemap.resolve_span(span);
    let limit = (resolved.end.line + 1).min(inside.len());
    for flag in inside.iter_mut().take(limit).skip(resolved.begin.line + 1) {
        *flag = true;
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Bracket {
    Round,
    Square,
    Curly,
}

fn repair_hint(script: &str, dialect: &Dialect, diag_line: Option<u32>) -> Option<&'static str> {
    let err_line = diag_line?.checked_sub(1)? as usize;
    let codemap = CodeMap::new(SCRIPT_NAME.to_owned(), script.to_owned());
    let mut stack: Vec<(Bracket, bool)> = Vec::new();

    for res in Lexer::new(script, dialect, codemap.clone()) {
        match res {
            Ok((start, token, _)) => match token {
                Token::OpeningRound => stack.push((Bracket::Round, false)),
                Token::OpeningSquare => stack.push((Bracket::Square, false)),
                Token::OpeningCurly => stack.push((Bracket::Curly, false)),
                Token::ClosingRound | Token::ClosingSquare | Token::ClosingCurly => {
                    stack.pop();
                }
                Token::Colon => {
                    if let Some((_, has_colon)) = stack.last_mut() {
                        *has_colon = true;
                    }
                }
                Token::For if codemap.find_line(Pos::new(start as u32)) == err_line => {
                    if let Some(&(bracket, has_colon)) = stack.last() {
                        match bracket {
                            Bracket::Round => {
                                return Some(
                                    "提示：Starlark 不支持无括号生成器，请改为 [x for x in xs]",
                                );
                            }
                            Bracket::Curly if !has_colon => {
                                return Some(
                                    "提示：Starlark 不支持花括号集合推导，请改为 set([x for x in xs])",
                                );
                            }
                            _ => {}
                        }
                    }
                }
                _ => {}
            },
            Err(err) => {
                let e = err.into_error();
                if let Some(fs) = e.span()
                    && fs.resolve_span().begin.line == err_line
                {
                    match fs.source_span() {
                        "import" | "from" => {
                            return Some(
                                "提示：Starlark 不支持 import，请直接使用已提供的全局名称",
                            );
                        }
                        "is" => {
                            return Some(
                                "提示：Starlark 不支持 `is`，与 None 比较请改为 `== None` 或 `!= None`",
                            );
                        }
                        _ => {}
                    }
                }
                // 遇到首个词法错误即结束扫描，先处理出错行保留字再退出，避免 Lexer 在原地循环。
                break;
            }
        }
    }

    None
}
