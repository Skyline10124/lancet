// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! LANCET `fs.read` API 工具组件。

wit_bindgen::generate!({
    world: "fs-read",
    path: "wit",
    generate_all,
});

use exports::lancet::fs_read::fs::Guest;
use lancet::host::handles::ReadDir;

struct FsRead;

impl Guest for FsRead {
    fn read(
        dir: &ReadDir,
        path: String,
        missing_ok: Option<bool>,
    ) -> Result<Option<String>, String> {
        match dir.read(&path)? {
            Some(content) => Ok(Some(content)),
            None if missing_ok == Some(true) => Ok(None),
            None => Err(format!("文件不存在：{path}")),
        }
    }
}

export!(FsRead);
