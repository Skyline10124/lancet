//! 工具声明原型：打包自定义段、解出接口、核对清单、在 wasmtime 与 Starlark 中使用声明。

mod decl;
mod manifest;
mod pyi;
mod runtime;
mod star;
mod suite;

use std::path::PathBuf;

use anyhow::{Context, Result, bail};

fn usage() -> ! {
    eprintln!(
        "用法：\n  lancet-decl pack <组件> -o <输出> [--effects <清单.json>] [--wit <WIT 包.wasm>]\n  lancet-decl suite --artifacts <目录> --manifest <清单.json> --out <目录>"
    );
    std::process::exit(2)
}

fn flag(args: &[String], name: &str) -> Option<PathBuf> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .map(PathBuf::from)
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("pack") => {
            let input = args.get(1).map(PathBuf::from).unwrap_or_else(|| usage());
            let output = flag(&args, "-o").unwrap_or_else(|| usage());
            let mut bytes =
                std::fs::read(&input).with_context(|| format!("读取 {}", input.display()))?;
            if !manifest::find_sections(&bytes, manifest::EFFECTS_SECTION)?.is_empty() {
                bail!("输入已含 {} 段", manifest::EFFECTS_SECTION);
            }
            if let Some(effects) = flag(&args, "--effects") {
                bytes = manifest::append_section(
                    &bytes,
                    manifest::EFFECTS_SECTION,
                    &std::fs::read(effects)?,
                )?;
            }
            if let Some(wit) = flag(&args, "--wit") {
                bytes =
                    manifest::append_section(&bytes, manifest::WIT_SECTION, &std::fs::read(wit)?)?;
            }
            wasmparser::Validator::new()
                .validate_all(&bytes)
                .context("打包后的组件未通过校验")?;
            std::fs::write(&output, bytes)?;
            Ok(())
        }
        Some("suite") => {
            let artifacts = flag(&args, "--artifacts").unwrap_or_else(|| usage());
            let manifest = flag(&args, "--manifest").unwrap_or_else(|| usage());
            let out = flag(&args, "--out").unwrap_or_else(|| usage());
            std::fs::create_dir_all(&out)?;
            suite::run(&artifacts, &manifest, &out)
        }
        _ => usage(),
    }
}
