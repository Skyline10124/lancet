// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 节点启动校验：工作目录互不重叠、配置与状态目录位置隔离，以及执行环境的路径与挂载目的地校验。

use std::collections::HashSet;
use std::os::unix::ffi::OsStrExt as _;
use std::path::{Component, Path, PathBuf};

use anyhow::Context as _;

use super::{Config, DirAccess, LoadedConfig};

/// 已校验的单个工具或缓存路径（保留展开后的挂载目的地与规范化来源）。
#[derive(Debug, Clone)]
pub struct ValidatedEnvPath {
    pub dest: PathBuf,
    pub canonical: PathBuf,
}

/// 单个 `[[env]]` 经启动校验后的路径结果。
#[derive(Debug, Clone)]
pub struct ValidatedEnv {
    pub tools: Vec<ValidatedEnvPath>,
    pub caches: Vec<ValidatedEnvPath>,
}

/// 全部 `[[env]]` 经启动校验后的结果与规范化宿主家目录路径。
#[derive(Debug, Clone)]
pub struct ValidatedEnvs {
    pub host_home: PathBuf,
    pub envs: Vec<ValidatedEnv>,
}

/// 执行基础启动路径校验（纯路径比较，在创建或打开状态目录之前完成）。
pub fn check_paths(loaded: &LoadedConfig) -> anyhow::Result<()> {
    check_no_overlapping_dirs(&loaded.config)?;
    check_config_and_state_outside_dirs(loaded)?;
    Ok(())
}

/// 当配置授予了 `[[env]]` 时，执行环境路径、类型、隔离与保留挂载点校验。
///
/// 未授予任何 `[[env]]` 时返回 `Ok(None)`，不施加任何附加限制。
pub fn check_env_paths(loaded: &LoadedConfig) -> anyhow::Result<Option<ValidatedEnvs>> {
    if loaded.config.env.is_empty() {
        return Ok(None);
    }

    let raw_home = std::env::var("HOME")
        .context("启动校验失败：配置授予了执行环境，但环境变量 HOME 未设置或不是有效 UTF-8")?;
    if raw_home.is_empty() || raw_home.as_bytes().contains(&0) {
        anyhow::bail!("启动校验失败：环境变量 HOME 无效：{raw_home:?}");
    }
    let home_path = Path::new(&raw_home);
    if !home_path.is_absolute() {
        anyhow::bail!("启动校验失败：环境变量 HOME 必须是绝对路径：{raw_home}");
    }
    let host_home = normalize_lexical(home_path);
    check_not_reserved_system_path("HOME", "家目录校验", &host_home)?;
    let canonical_home = std::fs::canonicalize(home_path).ok();

    let state_dir = &loaded.config.node.state_dir;
    let mut validated_envs = Vec::with_capacity(loaded.config.env.len());

    for env in &loaded.config.env {
        validate_env_vars(&env.name, &env.vars)?;
        for p in &env.path {
            if p.as_os_str().as_bytes().contains(&0) {
                anyhow::bail!(
                    "启动校验失败：执行环境 `{}` 的 path 条目包含非法空字节：{}",
                    env.name,
                    p.display()
                );
            }
        }

        let mut seen_dests: HashSet<PathBuf> = HashSet::new();

        for mount_name in &env.mounts {
            let dir = loaded
                .config
                .dir
                .iter()
                .find(|d| &d.name == mount_name)
                .expect("配置加载已核对 env.mounts 引用均存在");
            check_mount_dest(
                &env.name,
                "mounts 挂载目的地校验",
                &dir.path,
                &host_home,
                canonical_home.as_deref(),
            )?;
            if !seen_dests.insert(dir.path.clone()) {
                anyhow::bail!(
                    "启动校验失败：执行环境 `{}` 的挂载目的地重复：{}",
                    env.name,
                    dir.path.display()
                );
            }
        }

        let mut tools = Vec::with_capacity(env.tools.len());
        for raw_tool in &env.tools {
            let vp = validate_env_path(
                &env.name,
                "tools",
                raw_tool,
                false,
                &host_home,
                canonical_home.as_deref(),
                &loaded.config.dir,
                state_dir,
                &mut seen_dests,
            )?;
            tools.push(vp);
        }

        let mut caches = Vec::with_capacity(env.caches.len());
        for raw_cache in &env.caches {
            let vp = validate_env_path(
                &env.name,
                "caches",
                raw_cache,
                true,
                &host_home,
                canonical_home.as_deref(),
                &loaded.config.dir,
                state_dir,
                &mut seen_dests,
            )?;
            caches.push(vp);
        }

        validated_envs.push(ValidatedEnv { tools, caches });
    }

    Ok(Some(ValidatedEnvs {
        host_home,
        envs: validated_envs,
    }))
}

#[allow(clippy::too_many_arguments)]
fn validate_env_path(
    env_name: &str,
    field: &str,
    raw: &Path,
    require_dir: bool,
    host_home: &Path,
    canonical_home: Option<&Path>,
    dirs: &[super::DirConfig],
    state_dir: &Path,
    seen_dests: &mut HashSet<PathBuf>,
) -> anyhow::Result<ValidatedEnvPath> {
    if raw.as_os_str().as_bytes().contains(&0) {
        anyhow::bail!(
            "启动校验失败：执行环境 `{env_name}` 的 {field} 路径包含非法空字节：{}",
            raw.display()
        );
    }

    // 先对展开后的原始路径做 canonicalize，避免先词法折叠 `..` 绕过前置符号链接。
    let canonical = std::fs::canonicalize(raw).with_context(|| {
        format!(
            "启动校验失败：执行环境 `{env_name}` 的 {field} 路径不存在或无法解析符号链接：{}",
            raw.display()
        )
    })?;

    let dest = normalize_lexical(raw);
    let op = format!("{field} 校验");
    check_mount_dest(env_name, &op, &dest, host_home, canonical_home)?;

    if !seen_dests.insert(dest.clone()) {
        anyhow::bail!(
            "启动校验失败：执行环境 `{env_name}` 的 {field} 挂载目的地重复：{}",
            dest.display()
        );
    }

    let meta = std::fs::metadata(&canonical).with_context(|| {
        format!(
            "启动校验失败：执行环境 `{env_name}` 无法读取 {field} 路径 {}（来源 {}）元数据",
            dest.display(),
            canonical.display()
        )
    })?;
    if require_dir {
        if !meta.is_dir() {
            anyhow::bail!(
                "启动校验失败：执行环境 `{env_name}` 的 caches 必须是目录：{}（来源 {}）",
                dest.display(),
                canonical.display()
            );
        }
    } else if !meta.is_dir() && !meta.is_file() {
        anyhow::bail!(
            "启动校验失败：执行环境 `{env_name}` 的 tools 必须是已存在的目录或普通文件：{}（来源 {}）",
            dest.display(),
            canonical.display()
        );
    }

    for (label, candidate) in [
        ("展开后的目的路径", dest.as_path()),
        ("解析链接后的来源路径", canonical.as_path()),
    ] {
        check_not_home_or_ancestor(env_name, field, label, candidate, host_home, canonical_home)?;
        for dir in dirs {
            if candidate.starts_with(&dir.path) || dir.path.starts_with(candidate) {
                anyhow::bail!(
                    "启动校验失败：执行环境 `{env_name}` 的 {field} {label} ({}) 与工作目录 `{}` ({}) 相同或互为祖先",
                    candidate.display(),
                    dir.name,
                    dir.path.display()
                );
            }
        }
        if candidate.starts_with(state_dir) || state_dir.starts_with(candidate) {
            anyhow::bail!(
                "启动校验失败：执行环境 `{env_name}` 的 {field} {label} ({}) 与节点状态目录 ({}) 相同或互为祖先",
                candidate.display(),
                state_dir.display()
            );
        }
    }

    Ok(ValidatedEnvPath { dest, canonical })
}

fn check_not_home_or_ancestor(
    env_name: &str,
    field: &str,
    label: &str,
    candidate: &Path,
    host_home: &Path,
    canonical_home: Option<&Path>,
) -> anyhow::Result<()> {
    if host_home.starts_with(candidate)
        || canonical_home.is_some_and(|ch| ch.starts_with(candidate))
    {
        anyhow::bail!(
            "启动校验失败：执行环境 `{env_name}` 的 {field} {label} ({}) 不能等于家目录 ({}) 或为其上级",
            candidate.display(),
            host_home.display()
        );
    }
    Ok(())
}

fn check_mount_dest(
    env_name: &str,
    op: &str,
    dest: &Path,
    host_home: &Path,
    canonical_home: Option<&Path>,
) -> anyhow::Result<()> {
    check_not_reserved_system_path(env_name, op, dest)?;
    if host_home.starts_with(dest) || canonical_home.is_some_and(|ch| ch.starts_with(dest)) {
        anyhow::bail!(
            "启动校验失败：执行环境 `{env_name}` {op}：挂载目的地 ({}) 不能覆盖私有家目录的根 ({})",
            dest.display(),
            host_home.display()
        );
    }
    Ok(())
}

fn check_not_reserved_system_path(env_name: &str, op: &str, dest: &Path) -> anyhow::Result<()> {
    let covered = dest == Path::new("/")
        || dest == Path::new("/tmp")
        || dest == Path::new("/run")
        || dest.starts_with("/usr")
        || dest.starts_with("/etc")
        || dest.starts_with("/proc")
        || dest.starts_with("/dev")
        || dest.starts_with("/run/lancet-init");
    if covered {
        anyhow::bail!(
            "启动校验失败：执行环境 `{env_name}` {op}：挂载目的地 ({}) 不能覆盖系统保留路径（/、/usr、/etc、/proc、/dev、/tmp、/run/lancet-init）",
            dest.display()
        );
    }
    Ok(())
}

fn validate_env_vars(
    env_name: &str,
    vars: &std::collections::BTreeMap<String, String>,
) -> anyhow::Result<()> {
    for (k, v) in vars {
        if k.is_empty() || k.contains('=') || k.as_bytes().contains(&0) {
            anyhow::bail!("启动校验失败：执行环境 `{env_name}` 的 vars 变量名无效：{k:?}");
        }
        if k == "HOME" || k == "PATH" {
            anyhow::bail!("启动校验失败：执行环境 `{env_name}` 的 vars 不能设置保留环境变量 `{k}`");
        }
        if v.as_bytes().contains(&0) {
            anyhow::bail!(
                "启动校验失败：执行环境 `{env_name}` 的 vars 变量 `{k}` 值包含非法空字节"
            );
        }
    }
    Ok(())
}

/// 纯词法规范化绝对路径：去掉 `.` 并就地折叠 `..`（不访问文件系统，不跟随符号链接）。
pub fn normalize_lexical(path: &Path) -> PathBuf {
    let mut parts: Vec<&std::ffi::OsStr> = Vec::new();
    for comp in path.components() {
        match comp {
            Component::Prefix(_) | Component::RootDir | Component::CurDir => {}
            Component::ParentDir => {
                parts.pop();
            }
            Component::Normal(p) => parts.push(p),
        }
    }
    let mut out = PathBuf::from("/");
    for part in parts {
        out.push(part);
    }
    out
}

/// 启动校验 1：所有工作目录的真实路径互不重叠。
fn check_no_overlapping_dirs(config: &Config) -> anyhow::Result<()> {
    for (i, a) in config.dir.iter().enumerate() {
        for b in &config.dir[i + 1..] {
            if a.path.starts_with(&b.path) || b.path.starts_with(&a.path) {
                anyhow::bail!(
                    "启动校验失败：工作目录互不重叠：工作目录 `{}` ({}) 与 `{}` ({}) 重叠",
                    a.name,
                    a.path.display(),
                    b.name,
                    b.path.display(),
                );
            }
        }
    }
    Ok(())
}

/// 启动校验 2：节点配置文件与节点状态目录位于所有工作目录之外，且可写工作目录不能是状态目录或其子目录。
fn check_config_and_state_outside_dirs(loaded: &LoadedConfig) -> anyhow::Result<()> {
    let state_dir = &loaded.config.node.state_dir;
    for dir in &loaded.config.dir {
        if loaded.config_path.starts_with(&dir.path) {
            anyhow::bail!(
                "启动校验失败：节点配置文件与节点状态目录在所有工作目录之外：节点配置文件 ({}) 位于工作目录 `{}` ({}) 内",
                loaded.config_path.display(),
                dir.name,
                dir.path.display(),
            );
        }
        if dir.access == DirAccess::Write && dir.path.starts_with(state_dir) {
            anyhow::bail!(
                "启动校验失败：可写工作目录不能是节点状态目录或其子目录：可写工作目录 `{}` ({}) 位于节点状态目录 ({}) 内",
                dir.name,
                dir.path.display(),
                state_dir.display(),
            );
        }
        if state_dir.starts_with(&dir.path) {
            anyhow::bail!(
                "启动校验失败：节点配置文件与节点状态目录在所有工作目录之外：节点状态目录 ({}) 位于工作目录 `{}` ({}) 内",
                state_dir.display(),
                dir.name,
                dir.path.display(),
            );
        }
    }
    Ok(())
}
