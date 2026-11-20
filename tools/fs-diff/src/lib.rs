// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! LANCET `fs.diff` API 工具组件。

wit_bindgen::generate!({
    world: "fs-diff",
    path: "wit",
    generate_all,
});

use exports::lancet::fs_diff::fs::{Guest, PathDiff};
use lancet::host::handles::ReadDir;

struct FsDiff;

impl Guest for FsDiff {
    fn diff(dir: &ReadDir) -> Result<Vec<PathDiff>, String> {
        dir.diff()
    }
}

export!(FsDiff);
