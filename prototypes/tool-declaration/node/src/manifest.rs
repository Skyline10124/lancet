//! effect 清单：在组件自定义段中查找、解析，并对照 WIT 接口核对。

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use anyhow::{Result, anyhow, bail};
use serde::{Deserialize, Serialize};

use crate::decl::{HandleKind, Interface, Ty, WitView};

/// effect 清单所在的自定义段。
pub const EFFECTS_SECTION: &str = "lancet:effects";
/// 对照方案：带文档的二进制 WIT 包所在的自定义段。
pub const WIT_SECTION: &str = "lancet:wit";

#[derive(Clone, Debug, Serialize)]
pub struct Section {
    /// 1 为组件顶层，2 及以上为嵌套的模块或组件。
    pub depth: usize,
    pub size: usize,
    #[serde(skip)]
    pub data: Vec<u8>,
}

/// 用 `wasmparser` 遍历全部层级，找出指定名称的自定义段。
pub fn find_sections(bytes: &[u8], name: &str) -> Result<Vec<Section>> {
    let mut depth = 0usize;
    let mut found = Vec::new();
    for payload in wasmparser::Parser::new(0).parse_all(bytes) {
        match payload? {
            wasmparser::Payload::Version { .. } => depth += 1,
            wasmparser::Payload::End(_) => depth = depth.saturating_sub(1),
            wasmparser::Payload::CustomSection(reader) if reader.name() == name => {
                found.push(Section {
                    depth,
                    size: reader.data().len(),
                    data: reader.data().to_vec(),
                })
            }
            _ => {}
        }
    }
    Ok(found)
}

/// 在组件末尾追加一个顶层自定义段。
pub fn append_section(component: &[u8], name: &str, data: &[u8]) -> Result<Vec<u8>> {
    if !wasmparser::Parser::is_component(component) {
        bail!("输入不是组件");
    }
    let mut body = Vec::new();
    leb128(&mut body, name.len() as u64);
    body.extend_from_slice(name.as_bytes());
    body.extend_from_slice(data);
    let mut out = component.to_vec();
    out.push(0);
    leb128(&mut out, body.len() as u64);
    out.extend_from_slice(&body);
    Ok(out)
}

fn leb128(out: &mut Vec<u8>, mut value: u64) {
    loop {
        let byte = (value & 0x7f) as u8;
        value >>= 7;
        if value == 0 {
            out.push(byte);
            return;
        }
        out.push(byte | 0x80);
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub enum Mode {
    Read,
    ReadWrite,
}

/// ADR 0002 第 2 节的模板写法：`Write<d>`、`Exec<e>`、`Emit<n>`、`Uses<d, Read>`、`Uses<e>`。
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Template {
    Write(String),
    Exec(String),
    Emit(String),
    Uses(String, Option<Mode>),
}

impl Template {
    pub fn param(&self) -> &str {
        match self {
            Template::Write(p) | Template::Exec(p) | Template::Emit(p) | Template::Uses(p, _) => p,
        }
    }

    pub fn is_effect(&self) -> bool {
        !matches!(self, Template::Uses(..))
    }
}

impl fmt::Display for Template {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Template::Write(p) => write!(f, "Write<{p}>"),
            Template::Exec(p) => write!(f, "Exec<{p}>"),
            Template::Emit(p) => write!(f, "Emit<{p}>"),
            Template::Uses(p, None) => write!(f, "Uses<{p}>"),
            Template::Uses(p, Some(Mode::Read)) => write!(f, "Uses<{p}, Read>"),
            Template::Uses(p, Some(Mode::ReadWrite)) => write!(f, "Uses<{p}, ReadWrite>"),
        }
    }
}

impl Serialize for Template {
    fn serialize<S: serde::Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_string())
    }
}

pub fn parse_template(text: &str) -> Result<Template> {
    let text = text.trim();
    let open = text
        .find('<')
        .ok_or_else(|| anyhow!("模板 `{text}` 缺少 `<`"))?;
    if !text.ends_with('>') {
        bail!("模板 `{text}` 缺少 `>`");
    }
    let kind = &text[..open];
    let args: Vec<&str> = text[open + 1..text.len() - 1]
        .split(',')
        .map(str::trim)
        .collect();
    let param = |p: &str| p.to_string();
    match (kind, args.as_slice()) {
        ("Write", [p]) => Ok(Template::Write(param(p))),
        ("Exec", [p]) => Ok(Template::Exec(param(p))),
        ("Emit", [p]) => Ok(Template::Emit(param(p))),
        ("Uses", [p]) => Ok(Template::Uses(param(p), None)),
        ("Uses", [p, "Read"]) => Ok(Template::Uses(param(p), Some(Mode::Read))),
        ("Uses", [p, "ReadWrite"]) => Ok(Template::Uses(param(p), Some(Mode::ReadWrite))),
        _ => bail!("无法识别的模板 `{text}`"),
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct FnTemplates {
    pub effects: BTreeSet<Template>,
    pub coeffects: BTreeSet<Template>,
}

pub type Templates = BTreeMap<String, FnTemplates>;

/// 只由 WIT 参数类型推出的模板：每个句柄参数按其资源类型给出能力。
pub fn derive_templates(iface: &Interface) -> Templates {
    let mut out = Templates::new();
    for f in &iface.functions {
        let mut t = FnTemplates::default();
        for p in &f.params {
            if let Ty::Handle { kind, .. } = p.ty {
                let name = p.name.clone();
                match kind {
                    HandleKind::ReadDir => {
                        t.coeffects.insert(Template::Uses(name, Some(Mode::Read)));
                    }
                    HandleKind::WriteDir => {
                        t.coeffects
                            .insert(Template::Uses(name.clone(), Some(Mode::ReadWrite)));
                        t.effects.insert(Template::Write(name));
                    }
                    HandleKind::ExecEnv => {
                        t.coeffects.insert(Template::Uses(name.clone(), None));
                        t.effects.insert(Template::Exec(name));
                    }
                    HandleKind::Egress => {
                        t.coeffects.insert(Template::Uses(name.clone(), None));
                        t.effects.insert(Template::Emit(name));
                    }
                }
            }
        }
        out.insert(f.name.clone(), t);
    }
    out
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RawManifest {
    pub format: u32,
    pub export: String,
    pub functions: BTreeMap<String, RawEntry>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RawEntry {
    pub effects: Vec<String>,
    pub coeffects: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct Validated {
    pub export: String,
    pub templates: Templates,
}

/// 从组件顶层读出唯一的清单段并核对。返回全部违规项。
pub fn validate_component(
    component: &[u8],
    view: &WitView,
) -> std::result::Result<Validated, Vec<String>> {
    let sections = find_sections(component, EFFECTS_SECTION)
        .map_err(|e| vec![format!("R1 组件解析失败：{e}")])?;
    let top: Vec<&Section> = sections.iter().filter(|s| s.depth == 1).collect();
    match top.len() {
        1 => validate_manifest(&top[0].data, view),
        0 if sections.is_empty() => Err(vec![format!("R1 组件顶层没有 {EFFECTS_SECTION} 段")]),
        0 => Err(vec![format!(
            "R1 组件顶层没有 {EFFECTS_SECTION} 段；嵌套模块中有 {} 个（深度 {:?}），不采信",
            sections.len(),
            sections.iter().map(|s| s.depth).collect::<Vec<_>>()
        )]),
        n => Err(vec![format!("R1 组件顶层有 {n} 个 {EFFECTS_SECTION} 段")]),
    }
}

/// R2–R10。
pub fn validate_manifest(
    data: &[u8],
    view: &WitView,
) -> std::result::Result<Validated, Vec<String>> {
    let raw: RawManifest =
        serde_json::from_slice(data).map_err(|e| vec![format!("R2 清单解析失败：{e}")])?;
    if raw.format != 1 {
        return Err(vec![format!("R2 不支持的清单格式 {}", raw.format)]);
    }
    let Some(iface) = view.exports.iter().find(|i| i.id == raw.export) else {
        return Err(vec![format!("R3 组件没有导出接口 {}", raw.export)]);
    };
    let mut errors = Vec::new();
    for other in view.exports.iter().filter(|i| i.id != raw.export) {
        errors.push(format!("R3 导出接口 {} 没有清单", other.id));
    }
    for f in &iface.functions {
        if !raw.functions.contains_key(&f.name) {
            errors.push(format!("R4 缺少导出函数 {} 的条目", f.name));
        }
    }
    let mut templates = Templates::new();
    for (name, entry) in &raw.functions {
        let Some(func) = iface.function(name) else {
            errors.push(format!("R4 条目 {name} 不对应任何导出函数"));
            continue;
        };
        let mut fnt = FnTemplates::default();
        for (list, effect_list) in [(&entry.effects, true), (&entry.coeffects, false)] {
            for text in list {
                match parse_template(text) {
                    Err(e) => errors.push(format!("R5 {name}: {e}")),
                    Ok(t) if t.is_effect() != effect_list => {
                        errors.push(format!("R5 {name}: {t} 放错了列表"))
                    }
                    Ok(t) => {
                        let set = if effect_list {
                            &mut fnt.effects
                        } else {
                            &mut fnt.coeffects
                        };
                        if !set.insert(t.clone()) {
                            errors.push(format!("R5 {name}: {t} 重复"));
                        }
                    }
                }
            }
        }
        for t in fnt.effects.iter().chain(&fnt.coeffects) {
            let Some(param) = func.params.iter().find(|p| p.name == t.param()) else {
                errors.push(format!("R6 {name}: {t} 引用的参数 {} 不存在", t.param()));
                continue;
            };
            let Ty::Handle { kind, borrow } = param.ty else {
                errors.push(format!(
                    "R7 {name}: {t} 引用的参数 {} 不是句柄（{}）",
                    param.name, param.ty
                ));
                continue;
            };
            if !borrow {
                errors.push(format!("R7 {name}: 句柄参数 {} 必须是 borrow", param.name));
            }
            let fits = matches!(
                (t, kind),
                (Template::Write(_), HandleKind::WriteDir)
                    | (Template::Exec(_), HandleKind::ExecEnv)
                    | (Template::Emit(_), HandleKind::Egress)
                    | (Template::Uses(_, Some(Mode::Read)), HandleKind::ReadDir)
                    | (
                        Template::Uses(_, Some(Mode::ReadWrite)),
                        HandleKind::WriteDir
                    )
                    | (
                        Template::Uses(_, None),
                        HandleKind::ExecEnv | HandleKind::Egress
                    )
            );
            if !fits {
                errors.push(format!(
                    "R8 {name}: {t} 与参数 {} 的类型 {} 不符",
                    param.name, param.ty
                ));
            }
        }
        for p in &func.params {
            let Ty::Handle { kind, .. } = p.ty else {
                continue;
            };
            let uses = fnt.coeffects.iter().filter(|t| t.param() == p.name).count();
            if uses != 1 {
                errors.push(format!(
                    "R9 {name}: 句柄参数 {} 应有 1 个 Uses 模板，实有 {uses} 个",
                    p.name
                ));
            }
            let granted = match kind {
                HandleKind::ReadDir => None,
                HandleKind::WriteDir => Some(Template::Write(p.name.clone())),
                HandleKind::ExecEnv => Some(Template::Exec(p.name.clone())),
                HandleKind::Egress => Some(Template::Emit(p.name.clone())),
            };
            if let Some(granted) = granted {
                if !fnt.effects.contains(&granted) {
                    errors.push(format!(
                        "R10 {name}: 参数 {} 的类型 {} 给出了 {granted} 的能力，effect 模板没有覆盖",
                        p.name, p.ty
                    ));
                }
            }
        }
        templates.insert(name.clone(), fnt);
    }
    if errors.is_empty() {
        Ok(Validated {
            export: raw.export,
            templates,
        })
    } else {
        Err(errors)
    }
}
