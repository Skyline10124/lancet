// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 构建并预编译 `lancet-act` 解释器组件、产品 API 工具组件与测试用探针组件。

#[path = "build/declaration.rs"]
mod declaration;
#[path = "src/act/engine.rs"]
mod engine;
#[path = "build/tool.rs"]
mod tool;

use std::env;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

struct ToolArtifactSpec {
    package_name: &'static str,
    lib_stem: &'static str,
    rel_dir: &'static str,
    output_stem: &'static str,
}

/// 产品 API 工具清单：统一驱动 `rerun-if-changed`、打包预编译与生成 Rust 引入表。
const PRODUCT_TOOLS: &[ToolArtifactSpec] = &[
    ToolArtifactSpec {
        package_name: "lancet-fs-read",
        lib_stem: "lancet_fs_read",
        rel_dir: "tools/fs-read",
        output_stem: "fs-read",
    },
    ToolArtifactSpec {
        package_name: "lancet-fs-list",
        lib_stem: "lancet_fs_list",
        rel_dir: "tools/fs-list",
        output_stem: "fs-list",
    },
    ToolArtifactSpec {
        package_name: "lancet-fs-search",
        lib_stem: "lancet_fs_search",
        rel_dir: "tools/fs-search",
        output_stem: "fs-search",
    },
    ToolArtifactSpec {
        package_name: "lancet-fs-write",
        lib_stem: "lancet_fs_write",
        rel_dir: "tools/fs-write",
        output_stem: "fs-write",
    },
    ToolArtifactSpec {
        package_name: "lancet-fs-edit",
        lib_stem: "lancet_fs_edit",
        rel_dir: "tools/fs-edit",
        output_stem: "fs-edit",
    },
    ToolArtifactSpec {
        package_name: "lancet-proc-run",
        lib_stem: "lancet_proc_run",
        rel_dir: "tools/proc-run",
        output_stem: "proc-run",
    },
];

const TEST_PROBE: ToolArtifactSpec = ToolArtifactSpec {
    package_name: "lancet-tool-probe",
    lib_stem: "lancet_tool_probe",
    rel_dir: "crates/lancet-node/tests/fixtures/tool-probe",
    output_stem: "tool-probe",
};

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

    let mut all_decls = Vec::new();
    for spec in PRODUCT_TOOLS {
        let decls = tool::build_and_precompile(
            workspace_root,
            &out_dir,
            spec.package_name,
            spec.lib_stem,
            &workspace_root.join(spec.rel_dir),
            spec.output_stem,
        );
        all_decls.extend(decls);
    }

    declaration::check_tool_conflicts(&all_decls)
        .unwrap_or_else(|e| panic!("产品 API 工具声明冲突：{e:#}"));

    write_product_tools_table(&out_dir, PRODUCT_TOOLS);

    // 测试用探针组件不进入产品清单，仅输出 decl.json 与 cwasm 供测试读取。
    tool::build_and_precompile(
        workspace_root,
        &out_dir,
        TEST_PROBE.package_name,
        TEST_PROBE.lib_stem,
        &workspace_root.join(TEST_PROBE.rel_dir),
        TEST_PROBE.output_stem,
    );
}

fn write_product_tools_table(out_dir: &Path, specs: &[ToolArtifactSpec]) {
    let mut code = String::from("[\n");
    for spec in specs {
        let stem = spec.output_stem;
        let _ = writeln!(
            code,
            "    EmbeddedComponent {{\n        name: {stem:?},\n        decls_json: include_str!(concat!(env!(\"OUT_DIR\"), \"/{stem}.decl.json\")),\n        cwasm: include_bytes!(concat!(env!(\"OUT_DIR\"), \"/{stem}.cwasm\")),\n    }},"
        );
    }
    code.push_str("]\n");
    let table_path = out_dir.join("product_tools.rs");
    fs::write(&table_path, code)
        .unwrap_or_else(|e| panic!("写入产品工具表 {} 失败：{e}", table_path.display()));
}

fn emit_rerun_if_changed(workspace_root: &Path, manifest_dir: &Path, act_dir: &Path) {
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
    ] {
        println!("cargo::rerun-if-changed={}", path.display());
    }
    for spec in PRODUCT_TOOLS.iter().chain(std::iter::once(&TEST_PROBE)) {
        let dir = workspace_root.join(spec.rel_dir);
        for sub in ["Cargo.toml", "wit", "src"] {
            println!("cargo::rerun-if-changed={}", dir.join(sub).display());
        }
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
