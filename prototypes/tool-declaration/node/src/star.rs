//! 在运行时由 WIT 接口构造 Starlark 静态类型，对脚本做执行前检查。

use std::collections::HashMap;
use std::fmt;

use allocative::Allocative;
use starlark::StarlarkPagablePanic;
use starlark::environment::{Globals, GlobalsBuilder, LibraryExtension};
use starlark::syntax::{AstModule, Dialect};
use starlark::typing::{
    AstModuleTypecheck, ParamIsRequired, ParamSpec, Ty as STy, TyStarlarkValue, TyUser,
    TyUserFields, TyUserParams,
};
use starlark::util::ArcStr;
use starlark::values::typing::TypeInstanceId;
use starlark::values::{NoSerialize, ProvidesStaticType, StarlarkValue};
use starlark_derive::starlark_value;
use starlark_map::sorted_map::SortedMap;

use crate::decl::{FuncDecl, HandleKind, Interface, Ty};
use crate::pyi::{first_optional, pascal, receiver, snake};

/// 注入脚本的句柄对象。静态检查只需要它报告的类型。
#[derive(Debug, ProvidesStaticType, NoSerialize, StarlarkPagablePanic, Allocative)]
pub struct HandleValue {
    name: String,
    ty: STy,
}
starlark::starlark_simple_value!(HandleValue);

impl fmt::Display for HandleValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "<handle {}>", self.name)
    }
}

#[starlark_value(type = "lancet_handle")]
impl<'v> StarlarkValue<'v> for HandleValue {
    fn typechecker_ty(&self) -> Option<STy> {
        Some(self.ty.clone())
    }
}

/// 工具返回的记录。只作为记录类型的底层值类型。
#[derive(Debug, ProvidesStaticType, NoSerialize, StarlarkPagablePanic, Allocative)]
pub struct RecordValue {
    name: String,
}
starlark::starlark_simple_value!(RecordValue);

impl fmt::Display for RecordValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "<record {}>", self.name)
    }
}

#[starlark_value(type = "lancet_record")]
impl<'v> StarlarkValue<'v> for RecordValue {}

struct TypeEnv {
    records: HashMap<String, STy>,
    /// 参数位置使用的句柄类型（与完整类型同 id，不带方法）。
    handle_refs: HashMap<HandleKind, STy>,
}

fn user_type(
    name: &str,
    base: TyStarlarkValue,
    id: TypeInstanceId,
    fields: Vec<(String, STy)>,
) -> anyhow::Result<STy> {
    let known: SortedMap<String, STy> = fields.into_iter().collect();
    let user = TyUser::new(
        name.to_string(),
        base,
        id,
        TyUserParams {
            fields: TyUserFields {
                known,
                unknown: false,
            },
            ..TyUserParams::default()
        },
    )
    .map_err(|e| anyhow::anyhow!("{e}"))?;
    Ok(STy::custom(user))
}

fn sty(ty: &Ty, env: &TypeEnv) -> STy {
    match ty {
        Ty::Prim(p) => match p.as_str() {
            "bool" => STy::bool(),
            "f32" | "f64" => STy::float(),
            "char" | "string" => STy::string(),
            _ => STy::int(),
        },
        Ty::List(e) => STy::list(sty(e, env)),
        Ty::Option(e) => STy::union2(sty(e, env), STy::none()),
        Ty::Tuple(ts) => STy::tuple(ts.iter().map(|t| sty(t, env)).collect()),
        Ty::Record(name) => env.records.get(name).cloned().unwrap_or_else(STy::any),
        Ty::Handle { kind, .. } => env.handle_refs[kind].clone(),
        Ty::Result { ok, .. } => ok.as_ref().map_or_else(STy::none, |t| sty(t, env)),
        Ty::Foreign(_) => STy::any(),
    }
}

/// 句柄方法的类型：去掉第一个（句柄）参数；末尾的 `option<T>` 参数可省略。
fn method_ty(f: &FuncDecl, env: &TypeEnv) -> anyhow::Result<STy> {
    let rest = &f.params[1..];
    let optional = first_optional(rest);
    let params: Vec<(ArcStr, ParamIsRequired, STy)> = rest
        .iter()
        .enumerate()
        .map(|(i, p)| {
            let required = if i >= optional {
                ParamIsRequired::No
            } else {
                ParamIsRequired::Yes
            };
            (
                ArcStr::from(snake(&p.name).as_str()),
                required,
                sty(&p.ty, env),
            )
        })
        .collect();
    let spec =
        ParamSpec::new_parts([], params, None, [], None).map_err(|e| anyhow::anyhow!("{e}"))?;
    let result = match &f.result {
        None => STy::none(),
        Some(t) => sty(t, env),
    };
    Ok(STy::function(spec, result))
}

/// 由接口构造全局环境：`docs` 为只读目录，`lancet` 为可写目录，`net` 为网络出口。
pub fn globals(iface: &Interface) -> anyhow::Result<Globals> {
    let handle_base = TyStarlarkValue::new::<HandleValue>();
    let record_base = TyStarlarkValue::new::<RecordValue>();
    let ids: HashMap<HandleKind, TypeInstanceId> = HandleKind::ALL
        .iter()
        .map(|k| (*k, TypeInstanceId::r#gen()))
        .collect();
    let mut env = TypeEnv {
        records: HashMap::new(),
        handle_refs: HashMap::new(),
    };
    for kind in HandleKind::ALL {
        env.handle_refs.insert(
            kind,
            user_type(kind.stub(), handle_base, ids[&kind], Vec::new())?,
        );
    }
    for record in &iface.records {
        let fields = record
            .fields
            .iter()
            .map(|f| (snake(&f.name), sty(&f.ty, &env)))
            .collect();
        let ty = user_type(
            &pascal(&record.name),
            record_base,
            TypeInstanceId::r#gen(),
            fields,
        )?;
        env.records.insert(record.name.clone(), ty);
    }
    let mut full = HashMap::new();
    for kind in HandleKind::ALL {
        let mut methods = Vec::new();
        for f in &iface.functions {
            let bound = match receiver(f) {
                Some(k) if k == kind => true,
                // 可写目录可以收窄为只读目录，因此也带只读目录的方法。
                Some(HandleKind::ReadDir) => kind == HandleKind::WriteDir,
                _ => false,
            };
            if bound {
                methods.push((snake(&f.name), method_ty(f, &env)?));
            }
        }
        full.insert(
            kind,
            user_type(kind.stub(), handle_base, ids[&kind], methods)?,
        );
    }
    let mut builder = GlobalsBuilder::extended_by(&[
        LibraryExtension::Json,
        LibraryExtension::SetType,
        LibraryExtension::StructType,
        LibraryExtension::Map,
        LibraryExtension::Filter,
        LibraryExtension::Print,
    ]);
    for (name, kind) in [
        ("docs", HandleKind::ReadDir),
        ("lancet", HandleKind::WriteDir),
        ("net", HandleKind::Egress),
    ] {
        builder.set(
            name,
            HandleValue {
                name: name.to_string(),
                ty: full[&kind].clone(),
            },
        );
    }
    Ok(builder.build())
}

/// 把脚本包进单一入口函数后做静态检查，返回错误信息。
pub fn typecheck(globals: &Globals, script: &str) -> Vec<String> {
    let mut wrapped = String::from("def __act__():\n");
    for line in script.lines() {
        wrapped.push_str("    ");
        wrapped.push_str(line);
        wrapped.push('\n');
    }
    wrapped.push_str("__act__()\n");
    let dialect = Dialect {
        enable_top_level_stmt: true,
        ..Dialect::Standard
    };
    match AstModule::parse("act.star", wrapped, &dialect) {
        Err(e) => vec![format!("解析失败：{e}")],
        Ok(ast) => {
            let (errors, ..) = ast.typecheck(globals, &HashMap::new());
            errors.into_iter().map(|e| e.to_string()).collect()
        }
    }
}
