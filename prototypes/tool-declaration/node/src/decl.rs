//! 用 `wit-component` 从组件（或 WIT 包）解出接口，映射为节点使用的简化类型。

use anyhow::{Context, Result, bail};
use serde::Serialize;
use wit_component::DecodedWasm;
use wit_parser::{Handle, InterfaceId, Resolve, Type, TypeDefKind, TypeId, TypeOwner, WorldItem};

/// 节点提供的宿主接口。
pub const HOST_INTERFACE: &str = "lancet:host/handles@0.1.0";

/// 句柄种类与模式；每种对应宿主接口里的一种资源类型。
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub enum HandleKind {
    ReadDir,
    WriteDir,
    ExecEnv,
    Egress,
}

impl HandleKind {
    pub const ALL: [HandleKind; 4] = [
        HandleKind::ReadDir,
        HandleKind::WriteDir,
        HandleKind::ExecEnv,
        HandleKind::Egress,
    ];

    fn from_resource(name: &str) -> Option<Self> {
        match name {
            "read-dir" => Some(Self::ReadDir),
            "write-dir" => Some(Self::WriteDir),
            "exec-env" => Some(Self::ExecEnv),
            "egress" => Some(Self::Egress),
            _ => None,
        }
    }

    pub fn resource(self) -> &'static str {
        match self {
            Self::ReadDir => "read-dir",
            Self::WriteDir => "write-dir",
            Self::ExecEnv => "exec-env",
            Self::Egress => "egress",
        }
    }

    /// `.pyi` 与 Starlark 中的类型名。
    pub fn stub(self) -> &'static str {
        match self {
            Self::ReadDir => "ReadDir",
            Self::WriteDir => "WriteDir",
            Self::ExecEnv => "ExecEnv",
            Self::Egress => "Egress",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub enum Ty {
    Prim(String),
    List(Box<Ty>),
    Option(Box<Ty>),
    Result {
        ok: Option<Box<Ty>>,
        err: Option<Box<Ty>>,
    },
    Tuple(Vec<Ty>),
    Record(String),
    Handle {
        kind: HandleKind,
        borrow: bool,
    },
    Foreign(String),
}

impl std::fmt::Display for Ty {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Ty::Prim(p) => f.write_str(p),
            Ty::List(e) => write!(f, "list<{e}>"),
            Ty::Option(e) => write!(f, "option<{e}>"),
            Ty::Result { ok, err } => {
                let ok = ok.as_ref().map_or("_".to_string(), |t| t.to_string());
                let err = err.as_ref().map_or("_".to_string(), |t| t.to_string());
                write!(f, "result<{ok}, {err}>")
            }
            Ty::Tuple(ts) => {
                let parts: Vec<String> = ts.iter().map(ToString::to_string).collect();
                write!(f, "tuple<{}>", parts.join(", "))
            }
            Ty::Record(name) => f.write_str(name),
            Ty::Handle { kind, borrow } => {
                let wrap = if *borrow { "borrow" } else { "own" };
                write!(f, "{wrap}<{}>", kind.resource())
            }
            Ty::Foreign(name) => write!(f, "foreign<{name}>"),
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct Param {
    pub name: String,
    pub ty: Ty,
}

#[derive(Clone, Debug, Serialize)]
pub struct FuncDecl {
    pub name: String,
    pub params: Vec<Param>,
    pub result: Option<Ty>,
    pub docs: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct Field {
    pub name: String,
    pub ty: Ty,
    pub docs: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct RecordDecl {
    pub name: String,
    pub fields: Vec<Field>,
    pub docs: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct Interface {
    pub id: String,
    pub docs: Option<String>,
    pub functions: Vec<FuncDecl>,
    pub records: Vec<RecordDecl>,
}

impl Interface {
    pub fn function(&self, name: &str) -> Option<&FuncDecl> {
        self.functions.iter().find(|f| f.name == name)
    }

    /// 只比较名称与类型，不比较文档。
    pub fn signature(&self) -> Vec<String> {
        let mut out = Vec::new();
        for r in &self.records {
            let fields: Vec<String> = r
                .fields
                .iter()
                .map(|f| format!("{}: {}", f.name, f.ty))
                .collect();
            out.push(format!("record {} {{ {} }}", r.name, fields.join(", ")));
        }
        for f in &self.functions {
            let params: Vec<String> = f
                .params
                .iter()
                .map(|p| format!("{}: {}", p.name, p.ty))
                .collect();
            let result = f
                .result
                .as_ref()
                .map_or(String::new(), |t| format!(" -> {t}"));
            out.push(format!("{}({}){result}", f.name, params.join(", ")));
        }
        out
    }

    /// 文档条目数：接口、记录、字段与函数。
    pub fn doc_count(&self) -> usize {
        let mut n = usize::from(self.docs.is_some());
        for r in &self.records {
            n += usize::from(r.docs.is_some());
            n += r.fields.iter().filter(|f| f.docs.is_some()).count();
        }
        n + self.functions.iter().filter(|f| f.docs.is_some()).count()
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct WitView {
    pub imports: Vec<String>,
    pub exports: Vec<Interface>,
}

/// 解出组件的导入与导出接口。
pub fn decode_component(bytes: &[u8]) -> Result<WitView> {
    let (resolve, world) = match wit_component::decode(bytes).context("wit-component 解码失败")?
    {
        DecodedWasm::Component(resolve, world) => (resolve, world),
        DecodedWasm::WitPackage(..) => bail!("输入是 WIT 包，不是组件"),
    };
    let world = &resolve.worlds[world];
    let mut imports = Vec::new();
    for (key, item) in &world.imports {
        let name = match item {
            WorldItem::Interface { id, .. } => resolve
                .id_of(*id)
                .unwrap_or_else(|| resolve.name_world_key(key)),
            _ => resolve.name_world_key(key),
        };
        imports.push(name);
    }
    let mut exports = Vec::new();
    for (key, item) in &world.exports {
        match item {
            WorldItem::Interface { id, .. } => exports.push(interface(&resolve, *id)),
            _ => bail!("导出项 {} 不是接口", resolve.name_world_key(key)),
        }
    }
    Ok(WitView { imports, exports })
}

/// 解出二进制 WIT 包中的全部接口（含 `package-docs` 段带来的文档）。
pub fn decode_wit_package(bytes: &[u8]) -> Result<Vec<Interface>> {
    match wit_component::decode(bytes).context("WIT 包解码失败")? {
        DecodedWasm::WitPackage(resolve, package) => Ok(resolve.packages[package]
            .interfaces
            .values()
            .map(|id| interface(&resolve, *id))
            .collect()),
        DecodedWasm::Component(..) => bail!("输入是组件，不是 WIT 包"),
    }
}

fn interface(resolve: &Resolve, id: InterfaceId) -> Interface {
    let iface = &resolve.interfaces[id];
    let name = resolve
        .id_of(id)
        .or_else(|| iface.name.clone())
        .unwrap_or_else(|| "<anonymous>".to_string());
    let mut records = Vec::new();
    for (type_name, type_id) in &iface.types {
        let def = &resolve.types[*type_id];
        if let TypeDefKind::Record(record) = &def.kind {
            records.push(RecordDecl {
                name: type_name.clone(),
                docs: def.docs.contents.clone(),
                fields: record
                    .fields
                    .iter()
                    .map(|f| Field {
                        name: f.name.clone(),
                        ty: convert(resolve, &f.ty),
                        docs: f.docs.contents.clone(),
                    })
                    .collect(),
            });
        }
    }
    let functions = iface
        .functions
        .values()
        .map(|f| FuncDecl {
            name: f.name.clone(),
            params: f
                .params
                .iter()
                .map(|p| Param {
                    name: p.name.clone(),
                    ty: convert(resolve, &p.ty),
                })
                .collect(),
            result: f.result.as_ref().map(|t| convert(resolve, t)),
            docs: f.docs.contents.clone(),
        })
        .collect();
    Interface {
        id: name,
        docs: iface.docs.contents.clone(),
        functions,
        records,
    }
}

fn convert(resolve: &Resolve, ty: &Type) -> Ty {
    let prim = match ty {
        Type::Id(id) => return convert_id(resolve, *id),
        Type::Bool => "bool",
        Type::U8 => "u8",
        Type::U16 => "u16",
        Type::U32 => "u32",
        Type::U64 => "u64",
        Type::S8 => "s8",
        Type::S16 => "s16",
        Type::S32 => "s32",
        Type::S64 => "s64",
        Type::F32 => "f32",
        Type::F64 => "f64",
        Type::Char => "char",
        Type::String => "string",
        _ => "unsupported",
    };
    Ty::Prim(prim.to_string())
}

fn convert_id(resolve: &Resolve, id: TypeId) -> Ty {
    let def = &resolve.types[id];
    match &def.kind {
        TypeDefKind::Type(inner) => convert(resolve, inner),
        TypeDefKind::Record(_) => Ty::Record(def.name.clone().unwrap_or_default()),
        TypeDefKind::List(e) => Ty::List(Box::new(convert(resolve, e))),
        TypeDefKind::Option(e) => Ty::Option(Box::new(convert(resolve, e))),
        TypeDefKind::Result(r) => Ty::Result {
            ok: r.ok.as_ref().map(|t| Box::new(convert(resolve, t))),
            err: r.err.as_ref().map(|t| Box::new(convert(resolve, t))),
        },
        TypeDefKind::Tuple(t) => Ty::Tuple(t.types.iter().map(|t| convert(resolve, t)).collect()),
        TypeDefKind::Handle(Handle::Own(r)) => handle(resolve, *r, false),
        TypeDefKind::Handle(Handle::Borrow(r)) => handle(resolve, *r, true),
        _ => Ty::Foreign(
            def.name
                .clone()
                .unwrap_or_else(|| "<anonymous>".to_string()),
        ),
    }
}

/// 句柄类型：沿 `use` 产生的别名找到资源的定义处，只有定义在宿主接口里的四种资源算句柄。
fn handle(resolve: &Resolve, mut id: TypeId, borrow: bool) -> Ty {
    while let TypeDefKind::Type(Type::Id(next)) = &resolve.types[id].kind {
        id = *next;
    }
    let def = &resolve.types[id];
    let name = def.name.clone().unwrap_or_default();
    let owner = match def.owner {
        TypeOwner::Interface(owner) => resolve.id_of(owner),
        _ => None,
    };
    if owner.as_deref() == Some(HOST_INTERFACE) {
        if let Some(kind) = HandleKind::from_resource(&name) {
            return Ty::Handle { kind, borrow };
        }
    }
    Ty::Foreign(format!(
        "{}#{name}",
        owner.unwrap_or_else(|| "<world>".to_string())
    ))
}
