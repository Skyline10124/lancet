// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! LANCET 外部上下文节点进程入口。

mod config;
mod server;
mod state;

use std::path::PathBuf;
use std::process::ExitCode;

use lancet_protocol::Capabilities;

fn main() -> ExitCode {
    if let Err(err) = run() {
        eprintln!("错误：{err:#}");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}

fn run() -> anyhow::Result<()> {
    let config_arg = parse_args()?;
    let loaded = config::load(&config_arg)?;
    let catalog = config::catalog::build(&loaded.config)?;
    config::startup::check_paths(&loaded)?;
    let state_dir = state::open(&loaded.config.node.state_dir)?;
    config::startup::check_ficlone(&loaded.config)?;
    let node_id = state::load_or_create_node_id(&state_dir)?;

    let view = server::NodeView {
        node_id,
        display_name: loaded.config.node.display_name,
        capabilities: Capabilities {
            settle_grace_ms: server::SETTLE_GRACE_MS,
            receipt_retention_ms: loaded.config.node.receipt_retention_ms,
        },
        catalog_digest: catalog.digest,
        environment_summary: catalog.summary,
        tools: catalog.tools,
    };

    server::serve(state_dir.path(), view)
}

fn parse_args() -> anyhow::Result<PathBuf> {
    let mut args = std::env::args_os();
    let _program = args.next();
    let Some(config_path) = args.next() else {
        anyhow::bail!("用法：lancet-node <配置文件路径>");
    };
    if args.next().is_some() {
        anyhow::bail!("用法：lancet-node <配置文件路径>");
    }
    Ok(PathBuf::from(config_path))
}
