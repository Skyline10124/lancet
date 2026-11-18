// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 节点状态目录：创建与 `node_id` 持久读写。

use std::fs::{DirBuilder, File};
use std::io::{self, Write as _};
use std::os::unix::fs::DirBuilderExt as _;
use std::path::{Path, PathBuf};

use anyhow::Context as _;
use rustix::fs::{FlockOperation, Mode, OFlags};
use uuid::Uuid;

/// 已打开的节点状态目录句柄。
pub struct StateDir {
    path: PathBuf,
    dir_file: File,
}

impl StateDir {
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// 刷盘节点状态目录元数据。
    pub fn sync(&self) -> anyhow::Result<()> {
        self.dir_file
            .sync_all()
            .with_context(|| format!("无法刷盘节点状态目录：{}", self.path.display()))
    }
}

/// 打开节点状态目录，不存在时以权限 `0o700` 创建。
pub fn open(state_dir: &Path) -> anyhow::Result<StateDir> {
    match DirBuilder::new().mode(0o700).create(state_dir) {
        Ok(()) => {}
        Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {}
        Err(err) => {
            return Err(err)
                .with_context(|| format!("无法创建节点状态目录：{}", state_dir.display()));
        }
    }

    let dir_file = File::from(
        rustix::fs::open(
            state_dir,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .with_context(|| format!("无法打开节点状态目录：{}", state_dir.display()))?,
    );

    match rustix::fs::flock(&dir_file, FlockOperation::NonBlockingLockExclusive) {
        Ok(()) => {}
        Err(err) if io::Error::from(err).kind() == io::ErrorKind::WouldBlock => {
            anyhow::bail!(
                "节点状态目录正被另一个节点进程使用：{}",
                state_dir.display()
            );
        }
        Err(err) => {
            return Err(io::Error::from(err))
                .with_context(|| format!("无法锁定节点状态目录：{}", state_dir.display()));
        }
    }

    Ok(StateDir {
        path: state_dir.to_path_buf(),
        dir_file,
    })
}

/// 读取 `<state_dir>/node_id`，不存在时原子生成 UUID v4。
pub fn load_or_create_node_id(state_dir: &StateDir) -> anyhow::Result<String> {
    let id_path = state_dir.path.join("node_id");
    match std::fs::read_to_string(&id_path) {
        Ok(content) => {
            let trimmed = content.trim_end_matches(['\r', '\n']);
            let parsed = Uuid::parse_str(trimmed)
                .with_context(|| format!("节点身份文件解析失败：{}", id_path.display()))?;
            Ok(parsed.hyphenated().to_string())
        }
        Err(err) if err.kind() == io::ErrorKind::NotFound => {
            let id = Uuid::new_v4().hyphenated().to_string();
            let tmp_path = state_dir.path.join("node_id.tmp");
            {
                let mut tmp_file = File::create(&tmp_path)
                    .with_context(|| format!("无法创建临时身份文件：{}", tmp_path.display()))?;
                writeln!(tmp_file, "{id}")
                    .with_context(|| format!("无法写入临时身份文件：{}", tmp_path.display()))?;
                tmp_file
                    .sync_all()
                    .with_context(|| format!("无法刷盘临时身份文件：{}", tmp_path.display()))?;
            }
            std::fs::rename(&tmp_path, &id_path).with_context(|| {
                format!(
                    "无法将 {} 重命名为 {}",
                    tmp_path.display(),
                    id_path.display()
                )
            })?;
            state_dir
                .dir_file
                .sync_all()
                .with_context(|| format!("无法刷盘节点状态目录：{}", state_dir.path.display()))?;
            Ok(id)
        }
        Err(err) => {
            Err(err).with_context(|| format!("无法读取节点身份文件：{}", id_path.display()))
        }
    }
}
