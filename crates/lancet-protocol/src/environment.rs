// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 环境简述、句柄、effect/co-effect 基础枚举与形状校验。

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use serde::{Deserialize, Deserializer, Serialize};

use crate::frame::json_len_within;

/// 环境简述紧凑 JSON 序列化的最大字节数（4 KiB）。
pub const MAX_ENVIRONMENT_SUMMARY_BYTES: usize = 4096;

/// effect 种类。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum EffectKind {
    /// 可寻址状态变更。
    Write,
    /// 子进程或设备动作执行。
    Exec,
    /// 网络外发或物理信号输出。
    Emit,
}

/// co-effect 句柄使用模式。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum Mode {
    Read,
    ReadWrite,
}

/// `seam` 边的方向。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum SeamDirection {
    Integrity,
    Confidentiality,
}

/// `seam` 闸口的放行策略。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum SeamPolicy {
    Allow,
    Deny,
}

/// 节点授予的具名资源实例描述。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Handle {
    pub name: String,
    pub kind: String,
    pub effects: Vec<EffectKind>,
    pub modes: Vec<Mode>,
    pub attributes: BTreeMap<String, serde_json::Value>,
}

/// 跨句柄连接 effect 与 co-effect 的静态边。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Seam {
    pub from: String,
    pub to: String,
    pub direction: SeamDirection,
    pub policy: SeamPolicy,
}

/// `describe` 返回的已校验环境简述。
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct EnvironmentSummary {
    handles: Vec<Handle>,
    seams: Vec<Seam>,
}

/// 环境简述形状校验错误。
#[derive(Debug)]
pub enum EnvironmentError {
    EmptyHandleName,
    DuplicateHandleName { name: String },
    UnknownSeamFrom { handle: String },
    UnknownSeamTo { handle: String },
    TooLarge { max: usize },
    Serialize(serde_json::Error),
}

impl fmt::Display for EnvironmentError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyHandleName => write!(f, "环境简述中的句柄名称不能为空"),
            Self::DuplicateHandleName { name } => {
                write!(f, "环境简述中的句柄名称重复：{name}")
            }
            Self::UnknownSeamFrom { handle } => {
                write!(f, "环境简述中的 seam 起点句柄不存在：{handle}")
            }
            Self::UnknownSeamTo { handle } => {
                write!(f, "环境简述中的 seam 终点句柄不存在：{handle}")
            }
            Self::TooLarge { max } => {
                write!(f, "环境简述紧凑 JSON 序列化超过上限 {max} 字节")
            }
            Self::Serialize(err) => write!(f, "环境简述序列化失败：{err}"),
        }
    }
}

impl std::error::Error for EnvironmentError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Serialize(err) => Some(err),
            Self::EmptyHandleName
            | Self::DuplicateHandleName { .. }
            | Self::UnknownSeamFrom { .. }
            | Self::UnknownSeamTo { .. }
            | Self::TooLarge { .. } => None,
        }
    }
}

#[derive(Serialize)]
struct EnvironmentSummaryRef<'a> {
    handles: &'a [Handle],
    seams: &'a [Seam],
}

impl EnvironmentSummary {
    /// 构造并校验环境简述的唯一入口（同时供 `Deserialize` 复用）。
    pub fn new(handles: Vec<Handle>, seams: Vec<Seam>) -> Result<Self, EnvironmentError> {
        let mut names = BTreeSet::new();
        for handle in &handles {
            if handle.name.is_empty() {
                return Err(EnvironmentError::EmptyHandleName);
            }
            if !names.insert(handle.name.as_str()) {
                return Err(EnvironmentError::DuplicateHandleName {
                    name: handle.name.clone(),
                });
            }
        }

        for seam in &seams {
            if !names.contains(seam.from.as_str()) {
                return Err(EnvironmentError::UnknownSeamFrom {
                    handle: seam.from.clone(),
                });
            }
            if !names.contains(seam.to.as_str()) {
                return Err(EnvironmentError::UnknownSeamTo {
                    handle: seam.to.clone(),
                });
            }
        }

        let summary_ref = EnvironmentSummaryRef {
            handles: &handles,
            seams: &seams,
        };
        let within = json_len_within(&summary_ref, MAX_ENVIRONMENT_SUMMARY_BYTES)
            .map_err(EnvironmentError::Serialize)?;
        if within.is_none() {
            return Err(EnvironmentError::TooLarge {
                max: MAX_ENVIRONMENT_SUMMARY_BYTES,
            });
        }

        Ok(Self { handles, seams })
    }

    /// 返回已校验的句柄列表。
    pub fn handles(&self) -> &[Handle] {
        &self.handles
    }

    /// 返回已校验的 `seam` 边集。
    pub fn seams(&self) -> &[Seam] {
        &self.seams
    }
}

#[derive(Deserialize)]
struct RawEnvironmentSummary {
    handles: Vec<Handle>,
    seams: Vec<Seam>,
}

impl<'de> Deserialize<'de> for EnvironmentSummary {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = RawEnvironmentSummary::deserialize(deserializer)?;
        Self::new(raw.handles, raw.seams).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::digest::CatalogDigest;
    use crate::message::Response;

    fn sample_handle(name: &str) -> Handle {
        Handle {
            name: name.to_owned(),
            kind: "WriteDir".to_owned(),
            effects: vec![EffectKind::Write],
            modes: vec![Mode::ReadWrite],
            attributes: BTreeMap::new(),
        }
    }

    fn describe_ok_json(env_json: serde_json::Value) -> String {
        serde_json::json!({
            "type": "describe_ok",
            "catalog_digest": CatalogDigest::from_sha256_bytes([0x11; 32]),
            "environment_summary": env_json,
            "tools": [],
        })
        .to_string()
    }

    #[test]
    fn rejects_empty_and_duplicate_handle_names_via_describe_ok() {
        let empty_json = describe_ok_json(serde_json::json!({
            "handles": [{
                "name": "",
                "kind": "ReadDir",
                "effects": [],
                "modes": ["Read"],
                "attributes": {}
            }],
            "seams": []
        }));
        assert!(
            serde_json::from_str::<Response>(&empty_json).is_err(),
            "空句柄名在解码 describe_ok 时必须被拒绝"
        );

        let dup_json = describe_ok_json(serde_json::json!({
            "handles": [sample_handle("ws"), sample_handle("ws")],
            "seams": []
        }));
        assert!(
            serde_json::from_str::<Response>(&dup_json).is_err(),
            "重名句柄在解码 describe_ok 时必须被拒绝"
        );
    }

    #[test]
    fn rejects_unknown_enums_and_dangling_seam_endpoints_via_describe_ok() {
        let bad_enum = describe_ok_json(serde_json::json!({
            "handles": [{
                "name": "ws",
                "kind": "WriteDir",
                "effects": ["Delete"],
                "modes": ["ReadWrite"],
                "attributes": {}
            }],
            "seams": []
        }));
        assert!(
            serde_json::from_str::<Response>(&bad_enum).is_err(),
            "未知枚举值必须被拒绝"
        );

        let dangling_from = describe_ok_json(serde_json::json!({
            "handles": [sample_handle("dev")],
            "seams": [{
                "from": "missing",
                "to": "dev",
                "direction": "Integrity",
                "policy": "Allow"
            }]
        }));
        assert!(
            serde_json::from_str::<Response>(&dangling_from).is_err(),
            "悬空 seam from 端必须被拒绝"
        );

        let dangling_to = describe_ok_json(serde_json::json!({
            "handles": [sample_handle("ws")],
            "seams": [{
                "from": "ws",
                "to": "missing",
                "direction": "Confidentiality",
                "policy": "Deny"
            }]
        }));
        assert!(
            serde_json::from_str::<Response>(&dangling_to).is_err(),
            "悬空 seam to 端必须被拒绝"
        );
    }

    #[test]
    fn enforces_4096_byte_limit_including_multibyte_via_describe_ok() {
        let mut h = sample_handle("ws");
        h.attributes
            .insert("note".to_owned(), serde_json::json!(""));
        let base_len =
            serde_json::to_vec(&EnvironmentSummary::new(vec![h.clone()], vec![]).unwrap())
                .unwrap()
                .len();
        let remaining = MAX_ENVIRONMENT_SUMMARY_BYTES - base_len;

        // 构造恰好 4096 字节（含多字节 UTF-8 中文：每字 3 字节）。
        let zh_count = remaining / 3;
        let pad_ascii = remaining % 3;
        let exact_note = format!("{}{}", "汉".repeat(zh_count), "a".repeat(pad_ascii));
        h.attributes
            .insert("note".to_owned(), serde_json::json!(exact_note));
        let exact_json = describe_ok_json(
            serde_json::to_value(EnvironmentSummaryRef {
                handles: &[h.clone()],
                seams: &[],
            })
            .unwrap(),
        );
        let decoded: Response =
            serde_json::from_str(&exact_json).expect("恰好 4096 字节的环境简述应允许通过");
        assert!(matches!(decoded, Response::DescribeOk { .. }));

        // 增加 1 个 ASCII 字节到达 4097 字节，应拒绝。
        let over_note = format!("{}{}", "汉".repeat(zh_count), "a".repeat(pad_ascii + 1));
        h.attributes
            .insert("note".to_owned(), serde_json::json!(over_note));
        let over_json = describe_ok_json(
            serde_json::to_value(EnvironmentSummaryRef {
                handles: &[h],
                seams: &[],
            })
            .unwrap(),
        );
        assert!(
            serde_json::from_str::<Response>(&over_json).is_err(),
            "4097 字节的多字节环境简述必须被拒绝"
        );
    }
}
