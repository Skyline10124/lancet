// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 节点启动校验：工作目录互不重叠、配置与状态目录位置校验、FICLONE 探针。

use std::fs::File;
use std::io::Write as _;
use std::path::Path;

use anyhow::Context as _;
use rustix::fs::{Mode, OFlags};

use super::{Config, DirAccess, LoadedConfig};

/// 执行启动校验 1 与 2（纯路径比较，在创建或打开状态目录之前完成）。
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

/// 启动校验 2：节点配置文件与节点状态目录位于所有工作目录之外。
fn check_config_and_state_outside_dirs(loaded: &LoadedConfig) -> anyhow::Result<()> {
    for dir in &loaded.config.dir {
        if loaded.config_path.starts_with(&dir.path) {
            anyhow::bail!(
                "启动校验失败：节点配置文件与节点状态目录在所有工作目录之外：节点配置文件 ({}) 位于工作目录 `{}` ({}) 内",
                loaded.config_path.display(),
                dir.name,
                dir.path.display(),
            );
        }
        if loaded.config.node.state_dir.starts_with(&dir.path) {
            anyhow::bail!(
                "启动校验失败：节点配置文件与节点状态目录在所有工作目录之外：节点状态目录 ({}) 位于工作目录 `{}` ({}) 内",
                loaded.config.node.state_dir.display(),
                dir.name,
                dir.path.display(),
            );
        }
    }
    Ok(())
}

/// 启动校验 3：节点状态目录及所有可写工作目录同处一个支持 `FICLONE`（reflink）的文件系统。
pub fn check_ficlone(config: &Config) -> anyhow::Result<()> {
    let state_dir = &config.node.state_dir;
    probe_ficlone(state_dir, state_dir).with_context(|| {
        format!(
            "启动校验失败：可写工作目录、影子、私有缓存与节点状态目录同处一个支持 FICLONE（reflink）的文件系统：节点状态目录 ({}) 探测失败",
            state_dir.display()
        )
    })?;

    for dir in &config.dir {
        if dir.access == DirAccess::Write {
            probe_ficlone(&dir.path, state_dir).with_context(|| {
                format!(
                    "启动校验失败：可写工作目录、影子、私有缓存与节点状态目录同处一个支持 FICLONE（reflink）的文件系统：可写工作目录 `{}` ({}) 与节点状态目录 ({}) 探测失败",
                    dir.name,
                    dir.path.display(),
                    state_dir.display(),
                )
            })?;
        }
    }

    Ok(())
}

fn probe_ficlone(src_dir: &Path, dst_dir: &Path) -> anyhow::Result<()> {
    let mut src_file = File::from(
        rustix::fs::openat(
            rustix::fs::CWD,
            src_dir,
            OFlags::TMPFILE | OFlags::RDWR,
            Mode::RUSR | Mode::WUSR,
        )
        .with_context(|| format!("无法在 {} 创建 O_TMPFILE 源临时文件", src_dir.display()))?,
    );
    src_file
        .write_all(&[0xa5u8; 4096])
        .with_context(|| format!("无法向 {} 的临时文件写入探针数据", src_dir.display()))?;

    let dst_file = File::from(
        rustix::fs::openat(
            rustix::fs::CWD,
            dst_dir,
            OFlags::TMPFILE | OFlags::RDWR,
            Mode::RUSR | Mode::WUSR,
        )
        .with_context(|| format!("无法在 {} 创建 O_TMPFILE 目标临时文件", dst_dir.display()))?,
    );

    rustix::fs::ioctl_ficlone(&dst_file, &src_file).with_context(|| {
        format!(
            "从 {} 到 {} 的 FICLONE 调用失败",
            src_dir.display(),
            dst_dir.display()
        )
    })?;

    Ok(())
}
