// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! LANCET `fs.list` API 工具组件。

wit_bindgen::generate!({
    world: "fs-list",
    path: "wit",
    generate_all,
});

use exports::lancet::fs_list::fs::{Entry, Guest};
use lancet::host::handles::ReadDir;

struct FsList;

impl Guest for FsList {
    fn list(
        dir: &ReadDir,
        path: String,
        missing_ok: Option<bool>,
    ) -> Result<Option<Vec<Entry>>, String> {
        match dir.entries(&path)? {
            Some(entries) => Ok(Some(entries)),
            None if missing_ok == Some(true) => Ok(None),
            None => Err(format!("目录不存在：{path}")),
        }
    }
}

export!(FsList);
