// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 构建并预编译 `lancet-act` 解释器组件与测试用 API 工具组件。

#[path = "build/declaration.rs"]
mod declaration;
#[path = "src/act/engine.rs"]
mod engine;
#[path = "build/tool.rs"]
mod tool;

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    let manifest_dir =
        PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("缺少 CARGO_MANIFEST_DIR"));
    let workspace_root = manifest_dir
        .parent()
        .and_then(Path::parent)
        .expect("无法定位工作区根目录");
    let act_dir = workspace_root.join("crates/lancet-act");
    let fs_read_dir = workspace_root.join("tools/fs-read");
    let out_dir = PathBuf::from(env::var_os("OUT_DIR").expect("缺少 OUT_DIR"));

    emit_rerun_if_changed(workspace_root, &manifest_dir, &act_dir, &fs_read_dir);

    let wasm_target_dir = out_dir.join("wasm-target");
    let raw_component = tool::build_wasm_artifact(
        workspace_root,
        &wasm_target_dir,
        "lancet-act",
        "wasm32-wasip2",
        "lancet_act.wasm",
    );
    let composed_component = compose_wasi_adapter(&act_dir, &out_dir, &raw_component);
    let composed_bytes = fs::read(&composed_component)
        .unwrap_or_else(|e| panic!("读取组合组件 {} 失败：{e}", composed_component.display()));
    tool::precompile_to_cwasm(&out_dir, "act", &composed_bytes);

    // 节点发布的 API 工具声明集合为空，仅打包 fs-read 供测试读取。
    fs::write(out_dir.join("tools.decl.json"), "[]").expect("写入内嵌工具声明空列表失败");
    tool::build_and_precompile(
        workspace_root,
        &out_dir,
        "lancet-fs-read",
        "lancet_fs_read",
        &fs_read_dir,
        "fs-read",
    );
}

fn emit_rerun_if_changed(
    workspace_root: &Path,
    manifest_dir: &Path,
    act_dir: &Path,
    fs_read_dir: &Path,
) {
    for path in [
        workspace_root.join("Cargo.toml"),
        workspace_root.join("Cargo.lock"),
        manifest_dir.join("src/act/engine.rs"),
        manifest_dir.join("build/tool.rs"),
        manifest_dir.join("build/declaration.rs"),
        manifest_dir.join("wit/lancet-host"),
        act_dir.join("Cargo.toml"),
        act_dir.join("wasi-local.wat"),
        act_dir.join("wit"),
        act_dir.join("src"),
        fs_read_dir.join("Cargo.toml"),
        fs_read_dir.join("wit"),
        fs_read_dir.join("src"),
    ] {
        println!("cargo::rerun-if-changed={}", path.display());
    }
}

fn compose_wasi_adapter(act_dir: &Path, out_dir: &Path, raw_component: &Path) -> PathBuf {
    let wat_path = act_dir.join("wasi-local.wat");
    let adapter_wasm = out_dir.join("wasi-local.wasm");
    let composed_wasm = out_dir.join("act-composed.wasm");

    let parse_status = Command::new("wasm-tools")
        .arg("parse")
        .arg(&wat_path)
        .arg("-o")
        .arg(&adapter_wasm)
        .status()
        .unwrap_or_else(|e| panic!("无法运行 `wasm-tools parse`（请确认已安装 wasm-tools）：{e}"));
    if !parse_status.success() {
        panic!(
            "`wasm-tools parse` 编译 {} 失败：{parse_status}",
            wat_path.display()
        );
    }

    let compose_status = Command::new("wasm-tools")
        .args(["compose", "-d"])
        .arg(&adapter_wasm)
        .arg(raw_component)
        .arg("-o")
        .arg(&composed_wasm)
        .status()
        .unwrap_or_else(|e| {
            panic!("无法运行 `wasm-tools compose`（请确认已安装 wasm-tools）：{e}")
        });
    if !compose_status.success() {
        panic!("`wasm-tools compose` 组合失败：{compose_status}");
    }

    composed_wasm
}
