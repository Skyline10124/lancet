// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 本地 CI 的复刻。
//!
//! 编译并发由 `CARGO_BUILD_JOBS` 限制，默认 4。在资源受限或常驻大型宿主进程的开发机上，
//! 限制并发可避免多个 `rustc` 与 codegen 瞬时分配触发全局 OOM；资源充足的 CI 可调高
//! `CARGO_BUILD_JOBS`。
//!
//! 测试步骤按依赖顺序，对每个 workspace 成员单独运行 `cargo test -p <crate>`，每次都有
//! 超时上限（可经 `LANCET_TEST_TIMEOUT_SECS` 调整）。

use std::env;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::Context as _;
use cargo_metadata::{Metadata, MetadataCommand, PackageId};
use clap::Args;

use crate::{docs_check, repo_root};

#[derive(Args, Debug)]
pub struct CiArgs {}

/// 本地 CI 默认的 cargo 编译/测试并发数，可经 `CARGO_BUILD_JOBS` 覆盖。
const DEFAULT_BUILD_JOBS: &str = "4";

/// 单个 crate 的测试超时秒数，可经 `LANCET_TEST_TIMEOUT_SECS` 覆盖。
const DEFAULT_TEST_TIMEOUT_SECS: u64 = 300;

pub fn run(_args: CiArgs) -> anyhow::Result<()> {
    let root = repo_root::repo_root()?;
    let build_jobs = env::var("CARGO_BUILD_JOBS").unwrap_or_else(|_| DEFAULT_BUILD_JOBS.to_owned());

    step("toolchain precheck", check_toolchain)?;

    step("docs-check", docs_check::run)?;

    step("fmt", || {
        run_cargo(&root, &["fmt", "--all", "--check"], &build_jobs)
    })?;

    step("clippy", || {
        run_cargo(
            &root,
            &[
                "clippy",
                "--workspace",
                "--all-targets",
                "--",
                "-D",
                "warnings",
            ],
            &build_jobs,
        )
    })?;

    step("test", || run_tests(&root, &build_jobs))?;

    println!();
    println!("\u{1b}[1;32mAll checks passed.\u{1b}[0m");
    Ok(())
}

fn step(name: &str, run: impl FnOnce() -> anyhow::Result<()>) -> anyhow::Result<()> {
    println!();
    println!("\u{1b}[1m── {name} ──\u{1b}[0m");
    run()?;
    println!("\u{1b}[0;32m✓\u{1b}[0m {name}");
    Ok(())
}

/// 核验 `rust-toolchain.toml` 固定的 `rustc` 版本，让工具链版本不符的主机在门禁开头快速失败。
fn check_toolchain() -> anyhow::Result<()> {
    let rustc_out = Command::new("rustc")
        .arg("--version")
        .stderr(Stdio::inherit())
        .output()
        .context("无法运行 `rustc --version`")?;
    if !rustc_out.status.success() {
        anyhow::bail!("`rustc --version` 失败");
    }
    let version = String::from_utf8_lossy(&rustc_out.stdout);
    if !version.contains("1.96.0") {
        anyhow::bail!("需要固定的 rustc 1.96.0；当前版本：{version}");
    }

    Ok(())
}

fn run_cargo(root: &Path, args: &[&str], build_jobs: &str) -> anyhow::Result<()> {
    let status = Command::new("cargo")
        .current_dir(root)
        .env("CARGO_BUILD_JOBS", build_jobs)
        .args(args)
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()?;
    if !status.success() {
        anyhow::bail!("cargo {} 失败", args.join(" "));
    }
    Ok(())
}

fn test_timeout_secs() -> u64 {
    env::var("LANCET_TEST_TIMEOUT_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(DEFAULT_TEST_TIMEOUT_SECS)
}

fn run_tests(root: &Path, build_jobs: &str) -> anyhow::Result<()> {
    let metadata = MetadataCommand::new()
        .current_dir(root)
        .exec()
        .map_err(|e| anyhow::anyhow!("读取 workspace metadata 失败：{e}"))?;
    let crates = ordered_test_crates(&metadata);
    let timeout_secs = test_timeout_secs();

    println!();
    println!(
        "\u{1b}[1m── test ({} crates, {build_jobs} build jobs, {timeout_secs}s/crate) ──\u{1b}[0m",
        crates.len()
    );

    let mut failures: Vec<String> = Vec::new();
    for crate_name in &crates {
        match run_cargo_test(root, crate_name, build_jobs, timeout_secs) {
            Ok(()) => println!("\u{1b}[0;32m✓\u{1b}[0m test {crate_name}"),
            Err(e) => {
                println!("\u{1b}[0;31m✗\u{1b}[0m test {crate_name}: {e}");
                failures.push(crate_name.clone());
            }
        }
    }

    if !failures.is_empty() {
        anyhow::bail!(
            "cargo test 在 {} 个 crate 上失败：{}",
            failures.len(),
            failures.join(", ")
        );
    }
    Ok(())
}

fn run_cargo_test(
    root: &Path,
    crate_name: &str,
    build_jobs: &str,
    timeout_secs: u64,
) -> anyhow::Result<()> {
    let cmd_desc = format!("cargo test -p {crate_name}");
    let mut cmd = Command::new("cargo");
    cmd.current_dir(root)
        .env("CARGO_BUILD_JOBS", build_jobs)
        .arg("test")
        .arg("-p")
        .arg(crate_name)
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        // 将 cargo 放入独立进程组，超时时可一并终止整个子进程树。
        .process_group(0);

    let start = Instant::now();
    let deadline = Duration::from_secs(timeout_secs);
    let mut child = cmd
        .spawn()
        .with_context(|| format!("启动 `{cmd_desc}` 失败"))?;
    let pgid = child.id();

    loop {
        match child.try_wait()? {
            Some(status) => {
                if status.success() {
                    return Ok(());
                }
                anyhow::bail!("`{cmd_desc}` 失败：{status}");
            }
            None => {
                if start.elapsed() >= deadline {
                    let _ = Command::new("kill")
                        .arg("-KILL")
                        .arg(format!("-{pgid}"))
                        .stdout(Stdio::null())
                        .stderr(Stdio::null())
                        .status();
                    let _ = child.kill();
                    let _ = child.wait();
                    anyhow::bail!(
                        "`{cmd_desc}` 在 {timeout_secs}s 后超时（可设置 LANCET_TEST_TIMEOUT_SECS 提高上限）"
                    );
                }
                std::thread::sleep(Duration::from_millis(200));
            }
        }
    }
}

/// 按依赖顺序（被依赖方在前，同名按字典序）返回所有 workspace 成员名称。
fn ordered_test_crates(metadata: &Metadata) -> Vec<String> {
    use std::collections::{BTreeMap, HashSet};

    let id_to_name: BTreeMap<&PackageId, &str> = metadata
        .packages
        .iter()
        .map(|p| (&p.id, p.name.as_str()))
        .collect();
    let member_ids: HashSet<&PackageId> = metadata.workspace_members.iter().collect();
    let nodes: std::collections::HashMap<&PackageId, &cargo_metadata::Node> = metadata
        .resolve
        .as_ref()
        .map(|r| r.nodes.iter().map(|n| (&n.id, n)).collect())
        .unwrap_or_default();

    let mut order: Vec<String> = Vec::new();
    let mut visited: HashSet<&PackageId> = HashSet::new();

    let mut roots: Vec<&PackageId> = member_ids.iter().copied().collect();
    roots.sort_by_key(|id| id_to_name.get(id).copied().unwrap_or(""));

    fn visit<'a>(
        id: &'a PackageId,
        member_ids: &HashSet<&'a PackageId>,
        nodes: &std::collections::HashMap<&'a PackageId, &'a cargo_metadata::Node>,
        id_to_name: &BTreeMap<&'a PackageId, &'a str>,
        visited: &mut HashSet<&'a PackageId>,
        order: &mut Vec<String>,
    ) {
        if !member_ids.contains(id) || !visited.insert(id) {
            return;
        }
        if let Some(node) = nodes.get(id) {
            let mut deps: Vec<&PackageId> = node
                .dependencies
                .iter()
                .filter(|d| member_ids.contains(d))
                .collect();
            deps.sort_by_key(|d| id_to_name.get(d).copied().unwrap_or(""));
            for dep in deps {
                visit(dep, member_ids, nodes, id_to_name, visited, order);
            }
        }
        let name = id_to_name.get(id).copied().unwrap_or("unknown");
        order.push(name.to_owned());
    }

    for root in roots {
        visit(
            root,
            &member_ids,
            &nodes,
            &id_to_name,
            &mut visited,
            &mut order,
        );
    }

    order
}
