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
use std::sync::Arc;

use anyhow::Context as _;
use lancet_node::act::ActRuntime;
use lancet_node::dispatch::{Dispatcher, HandleSlot};
use lancet_node::exec_env::{
    EnvMount, HostExecEnv, HostExecEnvSpec, cache, cgroup, check_bwrap_and_init,
    locate_init_binary, open_or_create_private_home, pin_tool_source, probe_sandbox,
};
use lancet_node::read_dir::HostDir;
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
    let validated_envs = config::startup::check_env_paths(&loaded)?;

    let env_label = loaded
        .config
        .env
        .iter()
        .map(|e| e.name.as_str())
        .collect::<Vec<_>>()
        .join("、");

    let env_runtime_prereqs = if validated_envs.is_some() {
        let init_path = locate_init_binary()?;
        let init_fd = Arc::new(check_bwrap_and_init(&init_path, &env_label)?);
        Some((init_path, init_fd))
    } else {
        None
    };

    let state_dir = state::open(&loaded.config.node.state_dir)?;
    let node_id = state::load_or_create_node_id(&state_dir)?;

    let mut slots = Vec::with_capacity(loaded.config.dir.len() + loaded.config.env.len());
    for dir in &loaded.config.dir {
        let host_dir = HostDir::open(&dir.name, &dir.path)?;
        match dir.access {
            config::DirAccess::Read => slots.push(HandleSlot::ReadDir(host_dir)),
            config::DirAccess::Write => slots.push(HandleSlot::WriteDir(host_dir)),
        }
    }

    if let (Some(validated), Some((init_path, init_fd))) = (validated_envs, env_runtime_prereqs) {
        let cgroup_root = cgroup::CgroupRoot::init(&init_path, &env_label)?;
        probe_sandbox(
            &cgroup_root,
            std::os::fd::AsFd::as_fd(&*init_fd),
            &env_label,
        )?;

        for (env_cfg, val_env) in loaded.config.env.iter().zip(validated.envs) {
            let private_home_fd = open_or_create_private_home(state_dir.path(), &env_cfg.name)?;

            let mut pinned_tools = Vec::with_capacity(val_env.tools.len());
            for tool_path in val_env.tools {
                pinned_tools.push(pin_tool_source(
                    tool_path.dest,
                    &tool_path.canonical,
                    &env_cfg.name,
                )?);
            }

            let env_caches_root = state_dir
                .path()
                .join("envs")
                .join(&env_cfg.name)
                .join("caches");
            let mut pinned_caches = Vec::with_capacity(val_env.caches.len());
            for cache_path in val_env.caches {
                let private_dir_fd = cache::seed_or_open_cache(
                    &cache_path.canonical,
                    &cache_path.dest,
                    &env_caches_root,
                    &env_cfg.name,
                )
                .with_context(|| {
                    format!(
                        "执行环境 `{}` 准备私有缓存失败（来源 {}，目标 {}，挂载目的地 {}）",
                        env_cfg.name,
                        cache_path.canonical.display(),
                        env_caches_root.display(),
                        cache_path.dest.display(),
                    )
                })?;
                pinned_caches.push(lancet_node::exec_env::PinnedCache {
                    dest: cache_path.dest,
                    private_dir_fd,
                });
            }

            let mut mounts = Vec::with_capacity(env_cfg.mounts.len());
            for mount_name in &env_cfg.mounts {
                let (slot_idx, dir_slot) = slots
                    .iter()
                    .enumerate()
                    .find(|(_, s)| {
                        matches!(s, HandleSlot::ReadDir(_) | HandleSlot::WriteDir(_))
                            && s.name() == mount_name
                    })
                    .expect("配置加载已核对 env.mounts 均指向已授予工作目录");
                let (dir, writable) = match dir_slot {
                    HandleSlot::ReadDir(d) => (d.clone(), false),
                    HandleSlot::WriteDir(d) => (d.clone(), true),
                    HandleSlot::ExecEnv(_) => unreachable!(),
                };
                mounts.push(EnvMount {
                    slot: slot_idx as u32,
                    dir,
                    writable,
                });
            }

            let host_env = HostExecEnv::new(HostExecEnvSpec {
                name: env_cfg.name.clone(),
                mounts,
                host_home: validated.host_home.clone(),
                private_home_fd,
                tools: pinned_tools,
                caches: pinned_caches,
                extra_path: env_cfg.path.clone(),
                vars: env_cfg.vars.clone(),
                cgroup_root: cgroup_root.clone(),
                init_fd: Arc::clone(&init_fd),
            });
            slots.push(HandleSlot::ExecEnv(host_env));
        }
    }

    let artifacts = lancet_node::tools::embedded_artifacts();
    // SAFETY: `embedded_artifacts()` 中的 `.cwasm` 均由同一构建、相同的 `engine_config()` 预编译。
    let dispatcher = unsafe { Dispatcher::new(runtime.engine(), &artifacts, slots) }?;
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
