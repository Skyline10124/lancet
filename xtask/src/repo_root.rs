// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use std::path::PathBuf;

/// Workspace 根目录（`xtask` crate 目录的父目录）。
pub fn repo_root() -> anyhow::Result<PathBuf> {
    let manifest_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    Ok(manifest_dir
        .parent()
        .ok_or_else(|| anyhow::anyhow!("xtask manifest 没有父目录"))?
        .to_path_buf())
}
