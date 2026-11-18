// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! `catalog_digest` 规范 JSON 编码与 SHA-256 摘要计算。
//!
//! 规范化规则：输入是由 `tools`、`environment_summary`、`authorization` 三个顶层键组成的
//! JSON 对象；递归按对象键的 UTF-8 字节序（等价于 Unicode 码位序）排序输出紧凑 JSON，
//! 不含空白，数组严格保留原顺序，字符串与数值按 `serde_json` 标准表示。该规范不等同于
//! 完整的 RFC 8785 (JCS)，且显式不依赖 `serde_json` 的 `Map` 底层特征开关。

use std::fmt;
use std::io::{self, Write};
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use sha2::{Digest, Sha256};

use crate::environment::EnvironmentSummary;
use crate::message::ToolSpec;

/// 目录规范摘要（SHA-256 小写 64 位十六进制字符串）。
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CatalogDigest(String);

/// 解析 [`CatalogDigest`] 的错误。
#[derive(Debug, PartialEq, Eq)]
pub enum CatalogDigestError {
    InvalidLength { actual: usize },
    InvalidHexChar { index: usize, ch: char },
}

impl fmt::Display for CatalogDigestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidLength { actual } => {
                write!(
                    f,
                    "catalog_digest 长度应为 64 个十六进制字符，实际为 {actual}"
                )
            }
            Self::InvalidHexChar { index, ch } => {
                write!(
                    f,
                    "catalog_digest 第 {index} 位不是小写十六进制字符：{ch:?}"
                )
            }
        }
    }
}

impl std::error::Error for CatalogDigestError {}

impl CatalogDigest {
    /// 校验并解析 64 位小写十六进制 SHA-256 摘要字符串。
    pub fn parse(hex: impl AsRef<str>) -> Result<Self, CatalogDigestError> {
        let s = hex.as_ref();
        if s.len() != 64 {
            return Err(CatalogDigestError::InvalidLength { actual: s.len() });
        }
        for (index, ch) in s.char_indices() {
            if !matches!(ch, '0'..='9' | 'a'..='f') {
                return Err(CatalogDigestError::InvalidHexChar { index, ch });
            }
        }
        Ok(Self(s.to_owned()))
    }

    /// 由 32 字节 SHA-256 原始摘要编码为小写 64 位十六进制字符串。
    pub fn from_sha256_bytes(bytes: [u8; 32]) -> Self {
        let mut out = String::with_capacity(64);
        const HEX: &[u8; 16] = b"0123456789abcdef";
        for b in bytes {
            out.push(HEX[(b >> 4) as usize] as char);
            out.push(HEX[(b & 0x0f) as usize] as char);
        }
        Self(out)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl AsRef<str> for CatalogDigest {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for CatalogDigest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for CatalogDigest {
    type Err = CatalogDigestError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse(s)
    }
}

impl TryFrom<&str> for CatalogDigest {
    type Error = CatalogDigestError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        Self::parse(value)
    }
}

impl TryFrom<String> for CatalogDigest {
    type Error = CatalogDigestError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::parse(&value)
    }
}

impl Serialize for CatalogDigest {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for CatalogDigest {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = String::deserialize(deserializer)?;
        Self::parse(raw).map_err(serde::de::Error::custom)
    }
}

struct Sha256Writer(Sha256);

impl Write for Sha256Writer {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.update(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn write_canonical_value<W: Write>(writer: &mut W, value: &serde_json::Value) -> io::Result<()> {
    match value {
        serde_json::Value::Null => writer.write_all(b"null"),
        serde_json::Value::Bool(true) => writer.write_all(b"true"),
        serde_json::Value::Bool(false) => writer.write_all(b"false"),
        serde_json::Value::Number(num) => {
            serde_json::to_writer(&mut *writer, num).map_err(io::Error::other)
        }
        serde_json::Value::String(s) => {
            serde_json::to_writer(&mut *writer, s).map_err(io::Error::other)
        }
        serde_json::Value::Array(items) => {
            writer.write_all(b"[")?;
            for (idx, item) in items.iter().enumerate() {
                if idx > 0 {
                    writer.write_all(b",")?;
                }
                write_canonical_value(writer, item)?;
            }
            writer.write_all(b"]")
        }
        serde_json::Value::Object(map) => {
            let mut entries: Vec<(&str, &serde_json::Value)> =
                map.iter().map(|(k, v)| (k.as_str(), v)).collect();
            // 显式递归排序对象键，避免未来启用 preserve_order 时摘要依赖插入顺序。
            entries.sort_unstable_by_key(|(k1, _)| *k1);
            writer.write_all(b"{")?;
            for (idx, (key, val)) in entries.iter().enumerate() {
                if idx > 0 {
                    writer.write_all(b",")?;
                }
                serde_json::to_writer(&mut *writer, key).map_err(io::Error::other)?;
                writer.write_all(b":")?;
                write_canonical_value(writer, val)?;
            }
            writer.write_all(b"}")
        }
    }
}

/// 计算 `tools`、`environment_summary` 与 `authorization` 的规范 SHA-256 摘要。
pub fn catalog_digest(
    tools: &[ToolSpec],
    environment_summary: &EnvironmentSummary,
    authorization: &serde_json::Value,
) -> CatalogDigest {
    let tools_value = serde_json::to_value(tools).expect("ToolSpec 序列化为 JSON 不应失败");
    let env_value = serde_json::to_value(environment_summary)
        .expect("EnvironmentSummary 序列化为 JSON 不应失败");

    let mut top = [
        ("authorization", authorization),
        ("environment_summary", &env_value),
        ("tools", &tools_value),
    ];
    top.sort_unstable_by_key(|(k1, _)| *k1);

    let mut hasher = Sha256Writer(Sha256::new());
    hasher.write_all(b"{").expect("SHA-256 写入不应失败");
    for (idx, (key, val)) in top.iter().enumerate() {
        if idx > 0 {
            hasher.write_all(b",").expect("SHA-256 写入不应失败");
        }
        serde_json::to_writer(&mut hasher, key).expect("键名写入不应失败");
        hasher.write_all(b":").expect("SHA-256 写入不应失败");
        write_canonical_value(&mut hasher, val).expect("规范 JSON 写入不应失败");
    }
    hasher.write_all(b"}").expect("SHA-256 写入不应失败");

    let digest: [u8; 32] = hasher.0.finalize().into();
    CatalogDigest::from_sha256_bytes(digest)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::environment::{EffectKind, Handle, Mode, Seam, SeamDirection, SeamPolicy};
    use crate::message::{CoEffect, Effect};
    use std::collections::BTreeMap;

    fn base_inputs() -> (Vec<ToolSpec>, EnvironmentSummary, serde_json::Value) {
        let tools = vec![ToolSpec {
            name: "act".to_owned(),
            description: "执行编排脚本".to_owned(),
            input_schema: serde_json::from_str(
                r#"{
                    "type": "object",
                    "properties": {
                        "script": { "type": "string", "minLength": 1 }
                    },
                    "required": ["script"]
                }"#,
            )
            .unwrap(),
            effects: vec![Effect {
                handle: "ws".to_owned(),
                kind: EffectKind::Write,
            }],
            coeffects: vec![CoEffect {
                handle: "ws".to_owned(),
                mode: Some(Mode::ReadWrite),
            }],
        }];
        let mut attrs = BTreeMap::new();
        attrs.insert(
            "rules".to_owned(),
            serde_json::from_str(r#"{"b": 2, "a": {"y": true, "x": 1}}"#).unwrap(),
        );
        let env = EnvironmentSummary::new(
            vec![Handle {
                name: "ws".to_owned(),
                kind: "WriteDir".to_owned(),
                effects: vec![EffectKind::Write],
                modes: vec![Mode::ReadWrite],
                attributes: attrs,
            }],
            vec![Seam {
                from: "ws".to_owned(),
                to: "ws".to_owned(),
                direction: SeamDirection::Integrity,
                policy: SeamPolicy::Allow,
            }],
        )
        .unwrap();
        let auth: serde_json::Value = serde_json::from_str(
            r#"{
                "z": [1, 2, 3],
                "a": { "d": "val", "c": [false, null] }
            }"#,
        )
        .unwrap();
        (tools, env, auth)
    }

    #[test]
    fn invariant_to_nested_object_key_order_and_whitespace() {
        let (mut tools_a, mut env_a, auth_a) = base_inputs();
        let d1 = catalog_digest(&tools_a, &env_a, &auth_a);

        // 变更三类输入中嵌套对象的键顺序与 JSON 空白。
        tools_a[0].input_schema = serde_json::from_str(
            r#"{  "required" : ["script"], "properties": {"script": {"minLength":1,"type":"string"}}, "type": "object" }"#,
        )
        .unwrap();
        let mut attrs_b = BTreeMap::new();
        attrs_b.insert(
            "rules".to_owned(),
            serde_json::from_str(r#"{ "a": { "x": 1, "y": true }, "b": 2 }"#).unwrap(),
        );
        let mut handle_b = env_a.handles()[0].clone();
        handle_b.attributes = attrs_b;
        env_a = EnvironmentSummary::new(vec![handle_b], env_a.seams().to_vec()).unwrap();
        let auth_b: serde_json::Value =
            serde_json::from_str(r#"{ "a": { "c": [false, null], "d": "val" }, "z": [1, 2, 3] }"#)
                .unwrap();

        let d2 = catalog_digest(&tools_a, &env_a, &auth_b);
        assert_eq!(d1, d2, "嵌套对象字段顺序与空白变化不得改变 catalog_digest");
    }

    #[test]
    fn sensitive_to_each_input_and_array_order() {
        let (tools, env, auth) = base_inputs();
        let base = catalog_digest(&tools, &env, &auth);

        // 1. 改 tools
        let mut tools_mod = tools.clone();
        tools_mod[0].description = "修改后的说明".to_owned();
        assert_ne!(
            base,
            catalog_digest(&tools_mod, &env, &auth),
            "tools 变化必须改变摘要"
        );

        // 2. 改 environment_summary
        let mut h = env.handles()[0].clone();
        h.kind = "ReadDir".to_owned();
        let env_mod = EnvironmentSummary::new(vec![h], env.seams().to_vec()).unwrap();
        assert_ne!(
            base,
            catalog_digest(&tools, &env_mod, &auth),
            "environment_summary 变化必须改变摘要"
        );

        // 3. 改 authorization（含数组顺序敏感）
        let auth_mod: serde_json::Value = serde_json::from_str(
            r#"{
                "z": [3, 2, 1],
                "a": { "d": "val", "c": [false, null] }
            }"#,
        )
        .unwrap();
        assert_ne!(
            base,
            catalog_digest(&tools, &env, &auth_mod),
            "authorization 及数组顺序变化必须改变摘要"
        );
    }

    #[test]
    fn matches_pinned_canonical_sha256_vector() {
        let env = EnvironmentSummary::new(vec![], vec![]).unwrap();
        let auth = serde_json::json!({"b": 1, "a": [2, 1]});
        let digest = catalog_digest(&[], &env, &auth);

        // 顶层键排序：authorization, environment_summary, tools；
        // authorization 内部键排序：a, b；数组 [2, 1] 保留顺序。
        let expected_canonical = r#"{"authorization":{"a":[2,1],"b":1},"environment_summary":{"handles":[],"seams":[]},"tools":[]}"#;
        // 固定摘要由独立的 Python hashlib.sha256 计算，避免期望值复用被测编码路径。
        let expected = "64efa23ee5b590bc8f3d195f37d33bf12414f332b3cd9836044da4f94a2c97bd";
        assert_eq!(
            digest.as_str(),
            expected,
            "应与规范 JSON {expected_canonical} 的固定 SHA-256 向量一致"
        );

        // 拒绝非 64 位或含大写/非十六进制字符的摘要。
        assert!(CatalogDigest::parse("abcd").is_err(), "过短的摘要应拒绝");
        assert!(
            CatalogDigest::parse(expected.to_uppercase()).is_err(),
            "包含大写字母的摘要应拒绝"
        );
    }
}
