// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

mod ci;
mod docs_check;
mod repo_root;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "xtask", about = "LANCET workspace automation tasks")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// 本地 CI 复刻（工具链检查、文档链接检查、格式、clippy、测试）。
    Ci(ci::CiArgs),
    /// 检查 Markdown 相对链接的目标文件与锚点。
    DocsCheck,
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Commands::Ci(args) => ci::run(args),
        Commands::DocsCheck => docs_check::run(),
    }
}
