// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 节点启动校验：工作目录互不重叠、配置与状态目录位置隔离。

use super::{Config, DirAccess, LoadedConfig};

/// 执行启动路径校验（纯路径比较，在创建或打开状态目录之前完成）。
pub fn check_paths(loaded: &LoadedConfig) -> anyhow::Result<()> {
    check_no_overlapping_dirs(&loaded.config)?;
    check_config_and_state_outside_dirs(loaded)?;
    Ok(())
}

/// 启动校验 1：所有工作目录的真实路径互不重叠。
fn check_no_overlapping_dirs(config: &Config) -> anyhow::Result<()> {
    for (i, a) in config.dir.iter().enumerate() {
        for b in &config.dir[i + 1..] {
            if a.path.starts_with(&b.path) || b.path.starts_with(&a.path) {
                anyhow::bail!(
                    "启动校验失败：工作目录互不重叠：工作目录 `{}` ({}) 与 `{}` ({}) 重叠",
                    a.name,
                    a.path.display(),
                    b.name,
                    b.path.display(),
                );
            }
        }
    }
    Ok(())
}

/// 启动校验 2：节点配置文件与节点状态目录位于所有工作目录之外，且可写工作目录不能是状态目录或其子目录。
fn check_config_and_state_outside_dirs(loaded: &LoadedConfig) -> anyhow::Result<()> {
    let state_dir = &loaded.config.node.state_dir;
    for dir in &loaded.config.dir {
        if loaded.config_path.starts_with(&dir.path) {
            anyhow::bail!(
                "启动校验失败：节点配置文件与节点状态目录在所有工作目录之外：节点配置文件 ({}) 位于工作目录 `{}` ({}) 内",
                loaded.config_path.display(),
                dir.name,
                dir.path.display(),
            );
        }
        if dir.access == DirAccess::Write && dir.path.starts_with(state_dir) {
            anyhow::bail!(
                "启动校验失败：可写工作目录不能是节点状态目录或其子目录：可写工作目录 `{}` ({}) 位于节点状态目录 ({}) 内",
                dir.name,
                dir.path.display(),
                state_dir.display(),
            );
        }
        if state_dir.starts_with(&dir.path) {
            anyhow::bail!(
                "启动校验失败：节点配置文件与节点状态目录在所有工作目录之外：节点状态目录 ({}) 位于工作目录 `{}` ({}) 内",
                state_dir.display(),
                dir.name,
                dir.path.display(),
            );
        }
    }
    Ok(())
}
