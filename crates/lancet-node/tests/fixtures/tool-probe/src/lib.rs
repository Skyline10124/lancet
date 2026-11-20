// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! LANCET 分发层测试专用探针工具组件。

wit_bindgen::generate!({
    world: "tool-probe",
    path: "wit",
    generate_all,
});

use exports::lancet::tool_probe::probe::Guest;
use lancet::host::handles::{ReadDir, WriteDir};

struct ToolProbe;

impl Guest for ToolProbe {
    fn echo(_dir: &ReadDir, a: u8, b: i32, c: u64, d: f32, e: Vec<u8>) -> String {
        format!("a={a},b={b},c={c},d={d:?},e={e:?}")
    }

    fn misuse(dir: &ReadDir) {
        // SAFETY: 故意以错误资源类型构造并释放句柄，用于测试组件陷入隔离。
        drop(unsafe { WriteDir::from_handle(dir.handle()) });
    }

    fn grow(_dir: &ReadDir) {
        let buf = vec![1u8; 136 * 1024 * 1024];
        std::hint::black_box(&buf);
    }

    fn spin(_dir: &ReadDir) {
        let mut i = 0u64;
        loop {
            i = i.wrapping_add(1);
            std::hint::black_box(i);
        }
    }

    fn write_then_fail(dir: &WriteDir, path: String, content: String) -> Result<(), String> {
        dir.write(&path, &content)?;
        Err(format!("写入 `{path}` 后故意报错"))
    }

    fn write_first(
        dst: &WriteDir,
        _other: &WriteDir,
        path: String,
        content: String,
    ) -> Result<(), String> {
        dst.write(&path, &content)
    }
}

export!(ToolProbe);
