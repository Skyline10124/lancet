mod commands;
mod host;
mod text;
use anyhow::{Context, bail};
use serde_json::{Value as Json, json};
use starlark::environment::LibraryExtension;
use starlark::environment::{GlobalsBuilder, Module};
use starlark::eval::Evaluator;
use starlark::syntax::{AstModule, Dialect};
use starlark::values::Value;
use std::cell::{Cell, RefCell};
use std::fs;
use std::path::PathBuf;

fn diagnostic(error: String, mode: &str, lines: usize) -> (String, Option<usize>) {
    let locations = regex::Regex::new(r"script\.star:(\d+)(?::(\d+))?").unwrap();
    let mut line = None;
    let output = locations
        .replace_all(&error, |c: &regex::Captures| {
            let raw: usize = c[1].parse().unwrap();
            let adjusted = if mode == "A" {
                raw.saturating_sub(1).clamp(1, lines.max(1))
            } else {
                raw
            };
            line = Some(adjusted);
            match c.get(2) {
                Some(col) => format!("script.star:{adjusted}:{}", col.as_str()),
                None => format!("script.star:{adjusted}"),
            }
        })
        .into_owned();
    // 诊断的源码行标号也换回模型原脚本，保留原始诊断供审计。
    let gutter = regex::Regex::new(r"(?m)^(\s*)(\d+)( \|)").unwrap();
    let output = if mode == "A" {
        gutter
            .replace_all(&output, |c: &regex::Captures| {
                format!(
                    "{}{}{}",
                    &c[1],
                    c[2].parse::<usize>().unwrap().saturating_sub(1),
                    &c[3]
                )
            })
            .into_owned()
    } else {
        output
    };
    (output, line)
}
fn execute(fixture: PathBuf, script: String, canned: Json, mode: &str) -> anyhow::Result<Json> {
    let temp = tempfile::tempdir()?;
    let baseline = host::snapshot(&fixture)?;
    host::restore(temp.path(), &baseline)?;
    let state = host::State {
        root: temp.path().to_owned(),
        baseline,
        canned,
        discarded: Cell::new(false),
        result_set: Cell::new(false),
        events: RefCell::new(Vec::new()),
    };
    let source = if mode == "A" {
        format!(
            "def __act_main__():\n{}\n__act_result__ = __act_main__()\n",
            if script.trim().is_empty() {
                "    pass".to_owned()
            } else {
                script
                    .lines()
                    .map(|l| format!("    {l}"))
                    .collect::<Vec<_>>()
                    .join("\n")
            }
        )
    } else {
        script.clone()
    };
    let mut dialect = Dialect::Standard;
    dialect.enable_top_level_stmt = true;
    dialect.enable_f_strings = false;
    let ast = match AstModule::parse("script.star", source, &dialect) {
        Ok(ast) => ast,
        Err(e) => {
            let raw = e.to_string();
            let (error, line) = diagnostic(raw.clone(), mode, script.lines().count());
            return Ok(
                json!({"parsed":false,"ran":false,"outcome":"parse_error","error":error,"raw_error":raw,"line":line,"value":null,"events":[],"changes":[]}),
            );
        }
    };
    let mut builder = GlobalsBuilder::extended_by(&[LibraryExtension::Json]);
    builder.namespace("repo", host::repo);
    builder.namespace("env", host::env);
    host::common(&mut builder);
    if mode == "B" {
        host::mode_b(&mut builder);
    }
    let globals = builder.build();
    let (value, raw_error) = Module::with_temp_heap(|module| {
        let mut eval = Evaluator::new(&module);
        eval.extra = Some(&state);
        let result = eval.eval_module(ast, &globals);
        if let Err(e) = result {
            if !state.discarded.get() {
                return (None, Some(e.to_string()));
            }
        }
        let v = if state.discarded.get() || mode == "B" {
            module.extra_value().unwrap_or(Value::new_none())
        } else {
            let readback =
                AstModule::parse("host.star", "__act_result__".to_owned(), &dialect).unwrap();
            match eval.eval_module(readback, &globals) {
                Ok(v) => v,
                Err(e) => return (None, Some(e.to_string())),
            }
        };
        match v.to_json() {
            Ok(value) => (Some(value), None),
            Err(e) => (None, Some(e.to_string())),
        }
    });
    let changes = state.changes()?;
    let events = state.events.into_inner();
    if let Some(raw) = raw_error {
        let (error, line) = diagnostic(raw.clone(), mode, script.lines().count());
        return Ok(
            json!({"parsed":true,"ran":false,"outcome":"script_error","error":error,"raw_error":raw,"line":line,"value":null,"events":events,"changes":changes}),
        );
    }
    let value = value.unwrap();
    if value.len() > 65536 {
        return Ok(
            json!({"parsed":true,"ran":false,"outcome":"OutputLimitExceeded","error":"OutputLimitExceeded","raw_error":"OutputLimitExceeded","line":null,"value":null,"output_bytes":value.len(),"events":events,"changes":changes}),
        );
    }
    if !state.discarded.get() {
        host::restore(&fixture, &host::snapshot(temp.path())?)?;
    }
    Ok(
        json!({"parsed":true,"ran":true,"outcome":if state.discarded.get(){"discarded"}else{"committed"},"error":null,"raw_error":null,"line":null,"value":serde_json::from_str::<Json>(&value)?,"output_bytes":value.len(),"events":events,"changes":changes}),
    )
}
fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let mut fixture = None;
    let mut script = None;
    let mut canned = json!([]);
    let mut mode = "A".to_owned();
    while let Some(arg) = args.next() {
        let value = args.next().context("参数缺少值")?;
        match arg.as_str() {
            "--fixture" => fixture = Some(PathBuf::from(value)),
            "--script" => script = Some(fs::read_to_string(value)?),
            "--canned" => canned = serde_json::from_str(&fs::read_to_string(value)?)?,
            "--mode" => mode = value,
            _ => bail!("未知参数: {arg}"),
        }
    }
    if mode != "A" && mode != "B" {
        bail!("模式必须为 A 或 B");
    }
    println!(
        "{}",
        execute(
            fixture.context("缺少 --fixture")?,
            script.context("缺少 --script")?,
            canned,
            &mode
        )?
    );
    Ok(())
}
