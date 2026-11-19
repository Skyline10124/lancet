// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! API 工具的可序列化声明类型、效果模板与脚本名称映射。
//!
//! 供节点构建时声明加载、节点 `.pyi` 说明渲染与 `lancet-act` 解释器三方共用；
//! 本 crate 只包含数据结构与纯名称映射，不包含 WIT 解析或组件加载逻辑。

use serde::{Deserialize, Serialize};

/// 宿主句柄的种类与访问模式，对应 `lancet:host/handles` 中的四种资源类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum HandleKind {
    ReadDir,
    WriteDir,
    ExecEnv,
    Egress,
}

impl HandleKind {
    /// 按稳定顺序排列的全部句柄种类。
    pub const ALL: [HandleKind; 4] = [
        HandleKind::ReadDir,
        HandleKind::WriteDir,
        HandleKind::ExecEnv,
        HandleKind::Egress,
    ];

    /// 由 `lancet:host/handles` 的资源名解析句柄种类。
    pub fn from_resource(name: &str) -> Option<Self> {
        match name {
            "read-dir" => Some(Self::ReadDir),
            "write-dir" => Some(Self::WriteDir),
            "exec-env" => Some(Self::ExecEnv),
            "egress" => Some(Self::Egress),
            _ => None,
        }
    }

    /// 对应的 `lancet:host/handles` WIT 资源名。
    pub fn resource(self) -> &'static str {
        match self {
            Self::ReadDir => "read-dir",
            Self::WriteDir => "write-dir",
            Self::ExecEnv => "exec-env",
            Self::Egress => "egress",
        }
    }

    /// `.pyi` 存根与 Starlark 类型检查使用的名义类型名。
    pub fn stub_name(self) -> &'static str {
        match self {
            Self::ReadDir => "ReadDir",
            Self::WriteDir => "WriteDir",
            Self::ExecEnv => "ExecEnv",
            Self::Egress => "Egress",
        }
    }

    /// 返回需要 `self` 种类句柄的参数位置是否可接收 `actual` 种类的句柄。
    pub fn accepts(self, actual: Self) -> bool {
        match self {
            Self::ReadDir => matches!(actual, Self::ReadDir | Self::WriteDir),
            expected => actual == expected,
        }
    }
}

/// co-effect 使用模式。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum Mode {
    Read,
    ReadWrite,
}

/// effect 种类。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum EffectKind {
    Write,
    Exec,
    Emit,
}

/// 可在工具参数、记录字段或返回成功值中出现的 WIT 数据类型。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum WitType {
    Bool,
    String,
    Int,
    Float,
    Option(Box<WitType>),
    List(Box<WitType>),
    Record(String),
    Handle { kind: HandleKind, borrow: bool },
}

/// 工具函数的返回类型。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReturnType {
    Unit,
    Direct(WitType),
    Result { ok: Option<WitType> },
}

impl ReturnType {
    /// 返回交回脚本的值类型；无返回值或 `result<_, string>` 时返回 `None`。
    pub fn ok_type(&self) -> Option<&WitType> {
        match self {
            Self::Unit => None,
            Self::Direct(ty) => Some(ty),
            Self::Result { ok } => ok.as_ref(),
        }
    }
}

/// 工具形参声明。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParamDecl {
    pub name: String,
    pub ty: WitType,
}

/// WIT 记录的字段声明。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FieldDecl {
    pub name: String,
    pub ty: WitType,
    pub docs: Option<String>,
}

/// WIT 记录类型声明。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecordDecl {
    pub name: String,
    pub fields: Vec<FieldDecl>,
    pub docs: Option<String>,
}

/// co-effect 符号模板项，引用形参在 `ToolDecl::params` 中的 0 起位置。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum CoEffectItem {
    Uses(usize, Option<Mode>),
    MountsOf(usize),
}

/// effect 符号模板项，引用形参在 `ToolDecl::params` 中的 0 起位置。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum EffectItem {
    Effect(usize, EffectKind),
    WriteWritableMountsOf(usize),
    EmitViaEgressOf(usize),
}

/// 由句柄参数类型推出的符号模板集合。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Templates {
    pub coeffects: Vec<CoEffectItem>,
    pub effects: Vec<EffectItem>,
}

/// 单个 API 工具函数的完整声明。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolDecl {
    pub script_name: String,
    pub wit_interface: String,
    pub wit_function: String,
    pub docs: Option<String>,
    pub params: Vec<ParamDecl>,
    pub result: ReturnType,
    pub records: Vec<RecordDecl>,
    pub templates: Templates,
}

impl ToolDecl {
    /// 完整的 WIT 导出标识，例如 `lancet:fs-read/fs@0.1.0#read`。
    pub fn wit_export(&self) -> String {
        format!("{}#{}", self.wit_interface, self.wit_function)
    }

    /// 绑定在句柄对象上的 Starlark 方法名（连字符转为下划线）。
    pub fn method_name(&self) -> String {
        wit_to_ident(&self.wit_function)
    }

    /// 返回接收者位置与句柄种类，即首个 `borrow` 句柄形参。
    pub fn receiver(&self) -> Option<(usize, HandleKind)> {
        self.params
            .iter()
            .enumerate()
            .find_map(|(idx, param)| match param.ty {
                WitType::Handle { kind, borrow: true } => Some((idx, kind)),
                _ => None,
            })
    }

    /// 去掉接收者后的其余形参及其在 `params` 中的原始下标。
    pub fn method_params(&self) -> Vec<(usize, &ParamDecl)> {
        let recv_idx = self.receiver().map(|(idx, _)| idx);
        self.params
            .iter()
            .enumerate()
            .filter(|(idx, _)| Some(*idx) != recv_idx)
            .collect()
    }
}

/// 单次 `act` 调用授予脚本的具名句柄。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HandleGrant {
    pub name: String,
    pub kind: HandleKind,
    pub slot: u32,
}

/// 节点经 `context-json` 传给 `lancet-act` 的本次调用上下文。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActContext {
    pub tools: Vec<ToolDecl>,
    pub handles: Vec<HandleGrant>,
}

/// 把 WIT kebab-case 标识符映射为 Starlark / Python 标识符（`-` 转 `_`）。
pub fn wit_to_ident(name: &str) -> String {
    name.replace('-', "_")
}

/// 把 WIT kebab-case 类型名映射为 PascalCase 类型名。
pub fn wit_to_type_name(name: &str) -> String {
    name.split('-')
        .map(|part| {
            let mut chars = part.chars();
            chars
                .next()
                .map_or_else(String::new, |c| c.to_uppercase().chain(chars).collect())
        })
        .collect()
}

/// 由完整 WIT 接口标识与函数名生成脚本中的工具名（如 `fs.read`）。
pub fn script_tool_name(wit_interface: &str, wit_function: &str) -> String {
    let without_ver = wit_interface.split('@').next().unwrap_or(wit_interface);
    let iface_short = without_ver.rsplit('/').next().unwrap_or(without_ver);
    format!(
        "{}.{}",
        wit_to_ident(iface_short),
        wit_to_ident(wit_function)
    )
}

/// 返回方法形参切片中末尾连续 `option<T>` 可省略参数的起始下标。
pub fn first_optional_param(params: &[&ParamDecl]) -> usize {
    let mut index = params.len();
    while index > 0 && matches!(params[index - 1].ty, WitType::Option(_)) {
        index -= 1;
    }
    index
}
