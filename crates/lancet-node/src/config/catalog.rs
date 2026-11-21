// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 由配置推出环境简述、`seam` 边集、`tools` 与 `catalog_digest` 的唯一构造入口。

use std::collections::{BTreeMap, BTreeSet};

use anyhow::Context as _;
use lancet_protocol::{
    CatalogDigest, EffectKind, EnvironmentSummary, Handle, Mode, Seam, SeamDirection, SeamPolicy,
    ToolSpec, catalog_digest,
};
use lancet_tool_decl::{CoEffectItem, EffectItem, HandleKind, ToolDecl, WitType};
use serde::Serialize;
use serde_json::json;

use super::{Config, DirAccess, DirConfig, EgressConfig, EnvConfig};

#[derive(Serialize)]
struct Authorization<'a> {
    dir: &'a [DirConfig],
    env: &'a [EnvConfig],
    egress: &'a [EgressConfig],
}

/// 节点目录：环境简述、`tools` 声明与规范摘要。
pub struct Catalog {
    pub summary: EnvironmentSummary,
    pub tools: Vec<ToolSpec>,
    pub digest: CatalogDigest,
}

/// 由解析后的配置、`act` 类型存根与工具声明构造节点目录（唯一构造入口）。
pub fn build(
    config: &Config,
    act_description: String,
    tool_decls: &[ToolDecl],
) -> anyhow::Result<Catalog> {
    let (read_dir_modes, read_dir_effects) =
        summarize_handle_usage(HandleKind::ReadDir, tool_decls);
    let (write_dir_modes, write_dir_effects) =
        summarize_handle_usage(HandleKind::WriteDir, tool_decls);
    let mut handles = Vec::with_capacity(config.dir.len() + config.env.len() + config.egress.len());

    for dir in &config.dir {
        let (kind, modes, effects) = match dir.access {
            DirAccess::Read => ("ReadDir", read_dir_modes.clone(), read_dir_effects.clone()),
            DirAccess::Write => (
                "WriteDir",
                write_dir_modes.clone(),
                write_dir_effects.clone(),
            ),
        };
        handles.push(Handle {
            name: dir.name.clone(),
            kind: kind.to_owned(),
            effects,
            modes,
            attributes: BTreeMap::new(),
        });
    }

    for env in &config.env {
        // `tools` 由 UTF-8 的配置字符串与 HOME 展开而来，序列化不会失败。
        handles.push(Handle {
            name: env.name.clone(),
            kind: "ExecEnv".to_owned(),
            effects: Vec::new(),
            modes: Vec::new(),
            attributes: BTreeMap::from([
                ("mounts".to_owned(), json!(env.mounts)),
                ("tools".to_owned(), json!(env.tools)),
            ]),
        });
    }

    for egress in &config.egress {
        handles.push(Handle {
            name: egress.name.clone(),
            kind: "Egress".to_owned(),
            effects: Vec::new(),
            modes: Vec::new(),
            attributes: BTreeMap::from([("hosts".to_owned(), json!(egress.hosts))]),
        });
    }

    let mut seams = Vec::new();
    for env in &config.env {
        for dir in &config.dir {
            if dir.access == DirAccess::Write && env.mounts.contains(&dir.name) {
                seams.push(Seam {
                    from: dir.name.clone(),
                    to: env.name.clone(),
                    direction: SeamDirection::Integrity,
                    policy: SeamPolicy::Allow,
                });
            }
        }
    }
    for egress in &config.egress {
        for dir in &config.dir {
            seams.push(Seam {
                from: dir.name.clone(),
                to: egress.name.clone(),
                direction: SeamDirection::Confidentiality,
                policy: SeamPolicy::Allow,
            });
        }
        for env in &config.env {
            seams.push(Seam {
                from: env.name.clone(),
                to: egress.name.clone(),
                direction: SeamDirection::Confidentiality,
                policy: SeamPolicy::Allow,
            });
        }
    }

    let summary = EnvironmentSummary::new(handles, seams).context("构造环境简述失败")?;
    let tools = vec![ToolSpec {
        name: "act".to_owned(),
        description: act_description,
        input_schema: json!({
            "type": "object",
            "properties": {
                "script": { "type": "string" }
            },
            "required": ["script"],
            "additionalProperties": false
        }),
        effects: Vec::new(),
        coeffects: Vec::new(),
    }];
    let authorization = serde_json::to_value(Authorization {
        dir: &config.dir,
        env: &config.env,
        egress: &config.egress,
    })
    .context("序列化授权配置失败")?;
    let digest = catalog_digest(&tools, &summary, &authorization);

    Ok(Catalog {
        summary,
        tools,
        digest,
    })
}

fn summarize_handle_usage(
    handle_kind: HandleKind,
    tool_decls: &[ToolDecl],
) -> (Vec<Mode>, Vec<EffectKind>) {
    let mut modes = BTreeSet::new();
    let mut effects = BTreeSet::new();

    for decl in tool_decls {
        let param_accepts = |idx: usize| -> bool {
            decl.params.get(idx).is_some_and(|p| match p.ty {
                WitType::Handle { kind, borrow: true } => kind.accepts(handle_kind),
                _ => false,
            })
        };

        for item in &decl.templates.coeffects {
            if let CoEffectItem::Uses(idx, Some(mode)) = *item
                && param_accepts(idx)
            {
                let proto_mode = match mode {
                    lancet_tool_decl::Mode::Read => Mode::Read,
                    lancet_tool_decl::Mode::ReadWrite => Mode::ReadWrite,
                };
                modes.insert(proto_mode);
            }
        }

        for item in &decl.templates.effects {
            if let EffectItem::Effect(idx, kind) = *item
                && param_accepts(idx)
            {
                let proto_kind = match kind {
                    lancet_tool_decl::EffectKind::Write => EffectKind::Write,
                    lancet_tool_decl::EffectKind::Exec => EffectKind::Exec,
                    lancet_tool_decl::EffectKind::Emit => EffectKind::Emit,
                };
                effects.insert(proto_kind);
            }
        }
    }

    (modes.into_iter().collect(), effects.into_iter().collect())
}
