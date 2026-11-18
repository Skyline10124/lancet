// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 构建并预编译 `lancet-act` 解释器组件。

#[path = "src/act/engine.rs"]
mod engine;

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
    let out_dir = PathBuf::from(env::var_os("OUT_DIR").expect("缺少 OUT_DIR"));

    emit_rerun_if_changed(workspace_root, &manifest_dir, &act_dir);

    let raw_component = build_act_component(workspace_root, &out_dir);
    let composed_component = compose_wasi_adapter(&act_dir, &out_dir, &raw_component);
    precompile_component(&out_dir, &composed_component);
}

fn emit_rerun_if_changed(workspace_root: &Path, manifest_dir: &Path, act_dir: &Path) {
    println!(
        "cargo::rerun-if-changed={}",
        workspace_root.join("Cargo.toml").display()
    );
    println!(
        "cargo::rerun-if-changed={}",
        workspace_root.join("Cargo.lock").display()
    );
    println!(
        "cargo::rerun-if-changed={}",
        manifest_dir.join("src/act/engine.rs").display()
    );
    println!(
        "cargo::rerun-if-changed={}",
        act_dir.join("Cargo.toml").display()
    );
    println!(
        "cargo::rerun-if-changed={}",
        act_dir.join("wasi-local.wat").display()
    );
    println!("cargo::rerun-if-changed={}", act_dir.join("wit").display());
    println!("cargo::rerun-if-changed={}", act_dir.join("src").display());
}

fn build_act_component(workspace_root: &Path, out_dir: &Path) -> PathBuf {
    let cargo = env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let inner_target = out_dir.join("lancet-act-target");

    let mut cmd = Command::new(&cargo);
    cmd.current_dir(workspace_root)
        .args([
            "build",
            "--locked",
            "--release",
            "-p",
            "lancet-act",
            "--target",
            "wasm32-wasip2",
        ])
        .arg("--target-dir")
        .arg(&inner_target)
        // 若外层设置了 build-dir，`--target-dir` 只隔离最终产物，需同时隔离中间构建目录避免锁争用。
        .env("CARGO_BUILD_BUILD_DIR", &inner_target)
        // 外层 `cargo clippy` 会设置 clippy 包装器，带入内层会破坏 wasm 组件编译。
        .env_remove("RUSTC_WORKSPACE_WRAPPER")
        // 外层目标（如 x86_64）的编译标志不能传给 `wasm32-wasip2` 内层构建。
        .env_remove("CARGO_ENCODED_RUSTFLAGS")
        .env_remove("RUSTFLAGS")
        // 避免子进程引用已关闭的 jobserver 管道 fd 产生警告。
        .env_remove("CARGO_MAKEFLAGS");

    let status = cmd
        .status()
        .unwrap_or_else(|e| panic!("无法启动内层 cargo 构建 lancet-act：{e}"));
    if !status.success() {
        panic!("内层 cargo 构建 lancet-act（wasm32-wasip2）失败，退出状态：{status}");
    }

    inner_target.join("wasm32-wasip2/release/lancet_act.wasm")
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
        .args(["compose", "--no-imports", "-d"])
        .arg(&adapter_wasm)
        .arg(raw_component)
        .arg("-o")
        .arg(&composed_wasm)
        .status()
        .unwrap_or_else(|e| {
            panic!("无法运行 `wasm-tools compose`（请确认已安装 wasm-tools）：{e}")
        });
    if !compose_status.success() {
        panic!("`wasm-tools compose --no-imports` 组合失败：{compose_status}");
    }

    composed_wasm
}

fn precompile_component(out_dir: &Path, composed_component: &Path) {
    let engine =
        wasmtime::Engine::new(&engine::engine_config()).expect("创建 wasmtime 预编译引擎失败");
    let component_bytes = fs::read(composed_component)
        .unwrap_or_else(|e| panic!("读取组合组件 {} 失败：{e}", composed_component.display()));
    let precompiled = engine
        .precompile_component(&component_bytes)
        .expect("预编译 act 解释器组件失败");
    let cwasm_path = out_dir.join("act.cwasm");
    fs::write(&cwasm_path, precompiled)
        .unwrap_or_else(|e| panic!("写入预编译产物 {} 失败：{e}", cwasm_path.display()));
}
