// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! LANCET `fs.status` API 工具组件。

wit_bindgen::generate!({
    world: "fs-status",
    path: "wit",
    generate_all,
});

use exports::lancet::fs_status::fs::{Guest, PendingChange};
use lancet::host::handles::ReadDir;

struct FsStatus;

impl Guest for FsStatus {
    fn status(dir: &ReadDir) -> Result<Vec<PendingChange>, String> {
        dir.status()
    }
}

export!(FsStatus);
