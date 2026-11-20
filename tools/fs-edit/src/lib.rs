// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! LANCET `fs.edit` API 工具组件。

wit_bindgen::generate!({
    world: "fs-edit",
    path: "wit",
    generate_all,
});

use exports::lancet::fs_edit::fs::Guest;
use lancet::host::handles::WriteDir;

struct FsEdit;

impl Guest for FsEdit {
    fn edit(
        dir: &WriteDir,
        path: String,
        old: String,
        new: String,
        all: Option<bool>,
    ) -> Result<(), String> {
        let Some(content) = dir.read(&path)? else {
            return Err(format!("文件不存在：{path}"));
        };

        let count = content.matches(&old).count();
        let replace_all = all == Some(true);
        let updated = if replace_all {
            if count == 0 {
                return Err(format!("编辑文件 `{path}` 失败：未找到待替换文本"));
            }
            content.replace(&old, &new)
        } else {
            if count == 0 {
                return Err(format!("编辑文件 `{path}` 失败：未找到待替换文本"));
            }
            if count > 1 {
                return Err(format!(
                    "编辑文件 `{path}` 失败：待替换文本出现 {count} 次，要求恰好出现 1 次（需要全部替换时请传 all = True）"
                ));
            }
            content.replacen(&old, &new, 1)
        };

        dir.write(&path, &updated)
    }
}

export!(FsEdit);
