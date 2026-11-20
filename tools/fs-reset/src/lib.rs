// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! LANCET `fs.reset` API 工具组件。

wit_bindgen::generate!({
    world: "fs-reset",
    path: "wit",
    generate_all,
});

use exports::lancet::fs_reset::fs::Guest;
use lancet::host::handles::WriteDir;

struct FsReset;

impl Guest for FsReset {
    fn reset(dir: &WriteDir, path: String) -> Result<(), String> {
        dir.reset(&path)
    }
}

export!(FsReset);
