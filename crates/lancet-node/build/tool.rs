// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 打包并预编译工具组件，写出声明 JSON 与 `.cwasm`。

use std::borrow::Cow;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use wasm_encoder::{ComponentSection as _, CustomSection};

use crate::declaration;
use crate::engine;

/// 使用隔离的 `$OUT_DIR/wasm-target` 构建指定包并输出到给定 wasm 目标。
pub fn build_wasm_artifact(
    workspace_root: &Path,
    wasm_target_dir: &Path,
    package_name: &str,
    target_triple: &str,
    artifact_name: &str,
) -> PathBuf {
    let cargo = env::var_os("CARGO").unwrap_or_else(|| "cargo".into());

    let mut cmd = Command::new(&cargo);
    cmd.current_dir(workspace_root)
        .args([
            "build",
            "--locked",
            "--release",
            "-p",
            package_name,
            "--target",
            target_triple,
        ])
        .arg("--target-dir")
        .arg(wasm_target_dir)
        // 若外层设置了 build-dir，`--target-dir` 只隔离最终产物，需同时隔离中间构建目录避免锁争用。
        .env("CARGO_BUILD_BUILD_DIR", wasm_target_dir)
        // 外层 `cargo clippy` 会设置 clippy 包装器，带入内层会破坏 wasm 组件编译。
        .env_remove("RUSTC_WORKSPACE_WRAPPER")
        // 外层目标（如 x86_64）的编译标志不能传给 wasm 内层构建。
        .env_remove("CARGO_ENCODED_RUSTFLAGS")
        .env_remove("RUSTFLAGS")
        // 避免子进程引用已关闭的 jobserver 管道 fd 产生警告。
        .env_remove("CARGO_MAKEFLAGS");

    let status = cmd
        .status()
        .unwrap_or_else(|e| panic!("无法启动内层 cargo 构建 `{package_name}`：{e}"));
    if !status.success() {
        panic!("内层 cargo 构建 `{package_name}`（{target_triple}）失败：{status}");
    }

    wasm_target_dir.join(format!("{target_triple}/release/{artifact_name}"))
}

/// 预编译组件字节并写入 `<output_stem>.cwasm`。
pub fn precompile_to_cwasm(out_dir: &Path, output_stem: &str, component_bytes: &[u8]) {
    let engine =
        wasmtime::Engine::new(&engine::engine_config()).expect("创建 wasmtime 预编译引擎失败");
    let precompiled = engine
        .precompile_component(component_bytes)
        .unwrap_or_else(|e| panic!("预编译组件 `{output_stem}` 失败：{e}"));
    let cwasm_path = out_dir.join(format!("{output_stem}.cwasm"));
    fs::write(&cwasm_path, precompiled)
        .unwrap_or_else(|e| panic!("写入预编译产物 {} 失败：{e}", cwasm_path.display()));
}

/// 内层构建、打包、校验声明并预编译工具组件，写出 `<stem>.decl.json` 与 `<stem>.cwasm`。
pub fn build_and_precompile(
    workspace_root: &Path,
    out_dir: &Path,
    package_name: &str,
    lib_stem: &str,
    tool_dir: &Path,
    output_stem: &str,
) {
    let wasm_target_dir = out_dir.join("wasm-target");
    let core_wasm = build_wasm_artifact(
        workspace_root,
        &wasm_target_dir,
        package_name,
        "wasm32-unknown-unknown",
        &format!("{lib_stem}.wasm"),
    );
    let component_wasm = componentize(&core_wasm, out_dir, output_stem);
    let wit_pkg_wasm = encode_wit_package(&tool_dir.join("wit"), out_dir, output_stem);
    let packed_bytes = append_wit_section(&component_wasm, &wit_pkg_wasm);

    let decls = declaration::load_tool_declarations(package_name, &packed_bytes)
        .unwrap_or_else(|e| panic!("加载工具 `{package_name}` 声明失败：{e:#}"));
    let decl_json = serde_json::to_string_pretty(&decls)
        .unwrap_or_else(|e| panic!("序列化 `{package_name}` 声明 JSON 失败：{e}"));
    fs::write(out_dir.join(format!("{output_stem}.decl.json")), decl_json)
        .unwrap_or_else(|e| panic!("写入 {output_stem}.decl.json 失败：{e}"));

    precompile_to_cwasm(out_dir, output_stem, &packed_bytes);
}

fn componentize(core_wasm: &Path, out_dir: &Path, output_stem: &str) -> PathBuf {
    let component_wasm = out_dir.join(format!("{output_stem}.component.wasm"));
    let status = Command::new("wasm-tools")
        .args(["component", "new"])
        .arg(core_wasm)
        .arg("-o")
        .arg(&component_wasm)
        .status()
        .unwrap_or_else(|e| panic!("无法运行 `wasm-tools component new`：{e}"));
    if !status.success() {
        panic!("`wasm-tools component new` 失败：{status}");
    }
    component_wasm
}

fn encode_wit_package(wit_dir: &Path, out_dir: &Path, output_stem: &str) -> PathBuf {
    let wit_pkg_wasm = out_dir.join(format!("{output_stem}.wit-pkg.wasm"));
    let status = Command::new("wasm-tools")
        .args(["component", "wit"])
        .arg(wit_dir)
        .args(["--wasm", "-o"])
        .arg(&wit_pkg_wasm)
        .status()
        .unwrap_or_else(|e| panic!("无法运行 `wasm-tools component wit`：{e}"));
    if !status.success() {
        panic!("`wasm-tools component wit` 失败：{status}");
    }
    wit_pkg_wasm
}

fn append_wit_section(component_wasm: &Path, wit_pkg_wasm: &Path) -> Vec<u8> {
    let mut component_bytes = fs::read(component_wasm)
        .unwrap_or_else(|e| panic!("读取组件 {} 失败：{e}", component_wasm.display()));
    let wit_bytes = fs::read(wit_pkg_wasm)
        .unwrap_or_else(|e| panic!("读取 WIT 包 {} 失败：{e}", wit_pkg_wasm.display()));
    let section = CustomSection {
        name: Cow::Borrowed(declaration::WIT_SECTION),
        data: Cow::Borrowed(&wit_bytes),
    };
    section.append_to_component(&mut component_bytes);
    component_bytes
}
