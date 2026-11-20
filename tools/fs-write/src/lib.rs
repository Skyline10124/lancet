// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! LANCET `fs.write` API 工具组件。

wit_bindgen::generate!({
    world: "fs-write",
    path: "wit",
    generate_all,
});

use exports::lancet::fs_write::fs::Guest;
use lancet::host::handles::WriteDir;

struct FsWrite;

impl Guest for FsWrite {
    fn write(dir: &WriteDir, path: String, content: String) -> Result<(), String> {
        dir.write(&path, &content)
    }
}

export!(FsWrite);
