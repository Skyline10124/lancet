// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! LANCET 外部上下文节点进程入口。

mod calls;
mod config;
mod execute;
mod server;
mod state;

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::Context as _;
use lancet_node::act::ActRuntime;
use lancet_node::dispatch::{DirSlot, Dispatcher};
use lancet_node::read_dir::ReadDir;
use lancet_node::shadow::ShadowDir;
use lancet_node::shadow::publish::RecoveryRoot;
use lancet_protocol::Capabilities;

fn main() -> ExitCode {
    if let Err(err) = run() {
        eprintln!("错误：{err:#}");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}

fn validate_handle_names(runtime: &ActRuntime, cfg: &config::Config) -> anyhow::Result<()> {
    let entries = cfg
        .dir
        .iter()
        .map(|d| ("dir", d.name.as_str()))
        .chain(cfg.env.iter().map(|e| ("env", e.name.as_str())))
        .chain(cfg.egress.iter().map(|e| ("egress", e.name.as_str())));
    for (table, name) in entries {
        runtime
            .check_handle_name(name)
            .with_context(|| format!("配置 [[{table}]] 中的句柄名 `{name}` 无效"))?;
    }
    Ok(())
}

fn run() -> anyhow::Result<()> {
    let config_arg = parse_args()?;
    let loaded = config::load(&config_arg)?;
    let runtime = ActRuntime::load()?;
    validate_handle_names(&runtime, &loaded.config)?;
    let builtin_stub = runtime.builtin_stub()?;
    let embedded_tools = lancet_node::tools::embedded_tools();
    let act_stub = lancet_node::tools::render_act_stub(&builtin_stub, &embedded_tools);
    let catalog = config::catalog::build(&loaded.config, act_stub, &embedded_tools)?;

    config::startup::check_paths(&loaded)?;
    let state_dir = state::open(&loaded.config.node.state_dir)?;
    config::startup::check_ficlone(&loaded.config)?;
    let node_id = state::load_or_create_node_id(&state_dir)?;

    // 启动恢复顺序：先打开可写目录恢复根句柄并处理 4a tmp/widened，再恢复带发布事务日志的遗留调用，
    // 最后才打开 ShadowDir 重建索引与监听，确保在响应首个 hello 前一切已恢复并结算。
    let mut recovery_roots = Vec::new();
    for dir in &loaded.config.dir {
        if dir.access == config::DirAccess::Write {
            recovery_roots.push(RecoveryRoot::open(
                state_dir.path(),
                &dir.name,
                &dir.path,
                &dir.hidden,
                &dir.regenerable,
            )?);
        }
    }
    calls::recover_published_calls(&state_dir, &recovery_roots)?;
    drop(recovery_roots);

    let mut dirs = Vec::with_capacity(loaded.config.dir.len());
    for dir in &loaded.config.dir {
        match dir.access {
            config::DirAccess::Read => {
                dirs.push(DirSlot::ReadOnly(ReadDir::open(
                    &dir.name,
                    &dir.path,
                    &dir.hidden,
                )?));
            }
            config::DirAccess::Write => {
                dirs.push(DirSlot::Shadow(ShadowDir::open(
                    state_dir.path(),
                    &dir.name,
                    &dir.path,
                    &dir.hidden,
                    &dir.regenerable,
                )?));
            }
        }
    }
    let artifacts = lancet_node::tools::embedded_artifacts();
    // SAFETY: `embedded_artifacts()` 中的 `.cwasm` 均由同一构建、相同的 `engine_config()` 预编译。
    let dispatcher = unsafe { Dispatcher::new(runtime.engine(), &artifacts, dirs) }?;
    let ledger = calls::Ledger::open(&state_dir, loaded.config.node.receipt_retention_ms)?;

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
        runtime,
        dispatcher,
        ledger,
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
