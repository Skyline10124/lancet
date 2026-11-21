// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 节点配置类型、TOML 解析与主机路径解析。

pub mod catalog;
pub mod startup;

use std::collections::{BTreeMap, HashSet};
use std::io;
use std::path::{Path, PathBuf};

use anyhow::Context as _;
use serde::{Deserialize, Serialize};

/// 默认回执明细保留期：30 天（毫秒）。
pub const DEFAULT_RECEIPT_RETENTION_MS: u64 = 2_592_000_000;

fn default_receipt_retention_ms() -> u64 {
    DEFAULT_RECEIPT_RETENTION_MS
}

/// 解析且路径已就绪的节点配置与配置文件真实路径。
#[derive(Debug)]
pub struct LoadedConfig {
    pub config_path: PathBuf,
    pub config: Config,
}

/// 节点配置根表。
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub node: NodeConfig,
    #[serde(default)]
    pub dir: Vec<DirConfig>,
    #[serde(default)]
    pub env: Vec<EnvConfig>,
    #[serde(default)]
    pub egress: Vec<EgressConfig>,
}

/// `[node]` 配置表。
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NodeConfig {
    pub display_name: String,
    pub state_dir: PathBuf,
    #[serde(default = "default_receipt_retention_ms")]
    pub receipt_retention_ms: u64,
}

/// 工作目录读写级别。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DirAccess {
    Read,
    Write,
}

/// `[[dir]]` 配置条目。
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DirConfig {
    pub name: String,
    pub path: PathBuf,
    pub access: DirAccess,
}

/// `[[env]]` 配置条目。
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnvConfig {
    pub name: String,
    #[serde(default)]
    pub mounts: Vec<String>,
    #[serde(default)]
    pub tools: Vec<PathBuf>,
    #[serde(default)]
    pub caches: Vec<PathBuf>,
    #[serde(default)]
    pub egress: Option<String>,
    #[serde(default)]
    pub path: Vec<PathBuf>,
    #[serde(default)]
    pub vars: BTreeMap<String, String>,
}

/// `[[egress]]` 配置条目。
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EgressConfig {
    pub name: String,
    pub hosts: Vec<String>,
}

/// 读取并解析配置文件，原地展开并解析主机路径。
pub fn load(raw_config_path: &Path) -> anyhow::Result<LoadedConfig> {
    let config_path = std::fs::canonicalize(raw_config_path)
        .with_context(|| format!("无法解析配置文件路径：{}", raw_config_path.display()))?;
    let text = std::fs::read_to_string(&config_path)
        .with_context(|| format!("无法读取配置文件：{}", config_path.display()))?;
    let mut config: Config = toml::from_str(&text)
        .with_context(|| format!("配置文件 TOML 解析失败：{}", config_path.display()))?;

    config.node.state_dir = resolve_state_dir(&config.node.state_dir)?;

    for dir in &mut config.dir {
        let expanded = expand_host_path(&dir.path)
            .with_context(|| format!("工作目录 `{}` 的路径无效", dir.name))?;
        let canonical = std::fs::canonicalize(&expanded).with_context(|| {
            format!(
                "工作目录 `{}` 的路径不存在或不可访问：{}",
                dir.name,
                expanded.display()
            )
        })?;
        if !canonical.is_dir() {
            anyhow::bail!(
                "工作目录 `{}` 的路径不是目录：{}",
                dir.name,
                canonical.display()
            );
        }
        dir.path = canonical;
    }

    for env in &mut config.env {
        for path in env
            .tools
            .iter_mut()
            .chain(env.caches.iter_mut())
            .chain(env.path.iter_mut())
        {
            *path = expand_host_path(path)
                .with_context(|| format!("执行环境 `{}` 的主机路径无效", env.name))?;
        }
    }

    validate_references(&config)?;

    Ok(LoadedConfig {
        config_path,
        config,
    })
}

fn validate_references(config: &Config) -> anyhow::Result<()> {
    let dir_names: HashSet<&str> = config.dir.iter().map(|d| d.name.as_str()).collect();
    let egress_names: HashSet<&str> = config.egress.iter().map(|e| e.name.as_str()).collect();

    for env in &config.env {
        for mount in &env.mounts {
            if !dir_names.contains(mount.as_str()) {
                anyhow::bail!(
                    "配置错误：执行环境 `{}` 的 mounts 引用了不存在的工作目录 `{}`",
                    env.name,
                    mount
                );
            }
        }
        if let Some(egress) = &env.egress
            && !egress_names.contains(egress.as_str())
        {
            anyhow::bail!(
                "配置错误：执行环境 `{}` 的 egress 引用了不存在的网络出口 `{}`",
                env.name,
                egress
            );
        }
    }

    Ok(())
}

fn expand_host_path(path: &Path) -> anyhow::Result<PathBuf> {
    let raw = path
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("配置路径不是有效 UTF-8：{}", path.display()))?;

    if let Some(rest) = raw.strip_prefix("~/") {
        let home = std::env::var("HOME").with_context(|| {
            format!("配置路径 `{raw}` 使用了 `~/`，但环境变量 HOME 未设置或不是有效 UTF-8")
        })?;
        // 去掉多余的前导 `/`：`join` 遇到绝对路径会丢弃 HOME。
        Ok(PathBuf::from(home).join(rest.trim_start_matches('/')))
    } else if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        anyhow::bail!("配置路径 `{raw}` 必须是绝对路径或以 `~/` 开头")
    }
}

fn resolve_state_dir(raw_path: &Path) -> anyhow::Result<PathBuf> {
    let expanded = expand_host_path(raw_path).context("节点 `state_dir` 路径无效")?;
    match std::fs::canonicalize(&expanded) {
        Ok(canonical) => Ok(canonical),
        Err(err) if err.kind() == io::ErrorKind::NotFound => {
            // 尚不存在：以上级目录的真实路径加末段作为候选，先校验位置，再创建。
            let (Some(parent), Some(name)) = (expanded.parent(), expanded.file_name()) else {
                anyhow::bail!("节点 `state_dir` 无效：{}", expanded.display());
            };
            let canonical_parent = std::fs::canonicalize(parent).with_context(|| {
                format!(
                    "节点 `state_dir` 的上级目录不存在或不可访问：{}",
                    parent.display()
                )
            })?;
            Ok(canonical_parent.join(name))
        }
        Err(err) => Err(err)
            .with_context(|| format!("无法解析节点 `state_dir` 路径：{}", expanded.display())),
    }
}
