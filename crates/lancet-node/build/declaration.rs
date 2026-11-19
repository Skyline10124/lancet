// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 工具组件声明加载、宿主导入核对、WIT 签名核对与模板推导。

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context as _, Result, anyhow, bail};
use lancet_tool_decl::{
    CoEffectItem, EffectItem, EffectKind, FieldDecl, HandleKind, Mode, ParamDecl, RecordDecl,
    ReturnType, Templates, ToolDecl, WitType, script_tool_name,
};
use wasmparser::{Encoding, Parser, Payload};
use wit_component::{DecodedWasm, Output as _, WitPrinter};
use wit_parser::{
    Function, Handle, InterfaceId, PackageId, Resolve, Type, TypeDefKind, TypeId, WorldItem,
};

/// 自定义段名称，存放带注释的二进制 WIT 包。
pub const WIT_SECTION: &str = "lancet:wit";

const HOST_WIT: &str = include_str!("../wit/lancet-host/host.wit");
const HOST_HANDLES_INTERFACE: &str = "lancet:host/handles@0.1.0";

/// 从工具组件字节读出、核对声明并推出 effect / co-effect 模板。
pub fn load_tool_declarations(
    component_name: &str,
    component_bytes: &[u8],
) -> Result<Vec<ToolDecl>> {
    let wit_section = inspect_top_level_component(component_name, component_bytes)?;

    let (comp_resolve, comp_world) = match wit_component::decode(component_bytes)
        .with_context(|| format!("解码工具组件 `{component_name}` 元数据失败"))?
    {
        DecodedWasm::Component(resolve, world) => (resolve, world),
        DecodedWasm::WitPackage(..) => {
            bail!("工具组件 `{component_name}` 必须是 WASM 组件，不能是纯 WIT 包")
        }
    };

    verify_host_imports(component_name, component_bytes)?;

    let (doc_resolve, doc_pkg) = match wit_component::decode(wit_section)
        .with_context(|| format!("解码工具组件 `{component_name}` 的 `{WIT_SECTION}` 段失败"))?
    {
        DecodedWasm::WitPackage(resolve, pkg) => (resolve, pkg),
        DecodedWasm::Component(..) => {
            bail!("工具组件 `{component_name}` 的 `{WIT_SECTION}` 段必须是 WIT 包而非组件")
        }
    };

    let exported_ifaces = verify_documented_exports(
        component_name,
        component_bytes,
        &comp_resolve,
        comp_world,
        &doc_resolve,
        doc_pkg,
    )?;

    let mut decls = Vec::new();
    for (iface_fqn, doc_iface_id) in exported_ifaces {
        extract_interface_decls(&doc_resolve, doc_iface_id, &iface_fqn, &mut decls)?;
    }
    Ok(decls)
}

/// 校验工具集合跨组件不发生绑定方法重名或同名记录定义不一致。
pub fn check_tool_conflicts(tools: &[ToolDecl]) -> Result<()> {
    // 1. 方法不重名：对每种 HandleKind，接收者种类 accepts(kind) 的工具按 method_name 不重复。
    for kind in HandleKind::ALL {
        let mut seen_methods: BTreeMap<String, &ToolDecl> = BTreeMap::new();
        for tool in tools {
            let Some((_, recv_kind)) = tool.receiver() else {
                continue;
            };
            if !recv_kind.accepts(kind) {
                continue;
            }
            let method = tool.method_name();
            if let Some(prev) = seen_methods.get(&method) {
                bail!(
                    "句柄类型 `{}` 上的方法 `{}` 重名：工具 `{}` 与 `{}` 冲突",
                    kind.stub_name(),
                    method,
                    prev.wit_export(),
                    tool.wit_export()
                );
            }
            seen_methods.insert(method, tool);
        }
    }

    // 2. 同名记录一致：名称相同的记录在全部工具中定义一致。
    let mut seen_records: BTreeMap<&str, (&RecordDecl, &ToolDecl)> = BTreeMap::new();
    for tool in tools {
        for record in &tool.records {
            if let Some((prev_rec, prev_tool)) = seen_records.get(record.name.as_str()) {
                if *prev_rec != record {
                    bail!(
                        "记录 `{}` 在工具 `{}` 与 `{}` 中的定义不一致",
                        record.name,
                        prev_tool.wit_export(),
                        tool.wit_export()
                    );
                }
            } else {
                seen_records.insert(record.name.as_str(), (record, tool));
            }
        }
    }

    Ok(())
}

fn inspect_top_level_component<'a>(
    component_name: &str,
    component_bytes: &'a [u8],
) -> Result<&'a [u8]> {
    let mut depth = 0usize;
    let mut saw_root = false;
    let mut wit_section = None;

    for payload in Parser::new(0).parse_all(component_bytes) {
        let payload = payload.with_context(|| format!("解析工具组件 `{component_name}` 失败"))?;
        match payload {
            Payload::Version { encoding, .. } => {
                if !saw_root {
                    saw_root = true;
                    if encoding != Encoding::Component {
                        bail!("工具组件 `{component_name}` 不是 WASM 组件格式");
                    }
                }
            }
            Payload::ModuleSection { .. } | Payload::ComponentSection { .. } => {
                depth += 1;
            }
            Payload::End(_) => {
                depth = depth.saturating_sub(1);
            }
            Payload::ComponentImportSection(reader) if depth == 0 => {
                for item in reader {
                    let import = item
                        .with_context(|| format!("解析工具组件 `{component_name}` 导入段失败"))?;
                    let name = import.name.name;
                    if !name.starts_with("lancet:host/") {
                        bail!(
                            "工具组件 `{component_name}` 导入了 `lancet:host` 以外的接口：`{name}`"
                        );
                    }
                }
            }
            Payload::CustomSection(sec) if depth == 0 && sec.name() == WIT_SECTION => {
                wit_section = Some(sec.data());
            }
            _ => {}
        }
    }

    wit_section
        .ok_or_else(|| anyhow!("工具组件 `{component_name}` 缺少顶层 `{WIT_SECTION}` 自定义段"))
}

fn verify_host_imports(component_name: &str, component_bytes: &[u8]) -> Result<()> {
    let mut host_resolve = Resolve::default();
    let host_pkg = host_resolve
        .push_str("host.wit", HOST_WIT)
        .context("解析宿主 WIT `lancet:host` 失败")?;
    let host_world = host_resolve
        .select_world(&[host_pkg], Some("tool-host"))
        .context("选择宿主 `tool-host` world 失败")?;

    wit_component::targets(&host_resolve, host_world, component_bytes)
        .with_context(|| format!("工具组件 `{component_name}` 导入的宿主接口与当前宿主不兼容"))?;

    Ok(())
}

type ExportedInterfaces = BTreeMap<String, (String, InterfaceId)>;

fn collect_world_exports(
    resolve: &Resolve,
    world_id: wit_parser::WorldId,
    source_desc: &str,
) -> Result<ExportedInterfaces> {
    let mut exports = BTreeMap::new();
    for (key, item) in &resolve.worlds[world_id].exports {
        match item {
            WorldItem::Interface { id, .. } => {
                let fqn = resolve
                    .id_of(*id)
                    .unwrap_or_else(|| resolve.name_world_key(key));
                let local_name = resolve.interfaces[*id]
                    .name
                    .clone()
                    .unwrap_or_else(|| fqn.clone());
                exports.insert(fqn, (local_name, *id));
            }
            WorldItem::Function(func) => {
                bail!(
                    "{source_desc}导出了顶层函数 `{}`，工具声明只允许导出接口",
                    func.name
                );
            }
            WorldItem::Type { id, .. } => {
                let name = resolve.types[*id].name.as_deref().unwrap_or("<anon>");
                bail!("{source_desc}导出了顶层类型 `{name}`，工具声明只允许导出接口");
            }
        }
    }
    Ok(exports)
}

fn verify_documented_exports(
    component_name: &str,
    component_bytes: &[u8],
    comp_resolve: &Resolve,
    comp_world_id: wit_parser::WorldId,
    doc_resolve: &Resolve,
    doc_pkg: PackageId,
) -> Result<Vec<(String, InterfaceId)>> {
    let comp_exports = collect_world_exports(
        comp_resolve,
        comp_world_id,
        &format!("工具组件 `{component_name}` "),
    )?;
    let (doc_world, doc_exports) = collect_doc_exports(component_name, doc_resolve, doc_pkg)?;

    for fqn in comp_exports.keys() {
        if !doc_exports.contains_key(fqn) {
            bail!("工具组件 `{component_name}` 导出的接口 `{fqn}` 未在 `{WIT_SECTION}` 段中声明");
        }
    }
    for fqn in doc_exports.keys() {
        if !comp_exports.contains_key(fqn) {
            bail!("工具组件 `{component_name}` 的 `{WIT_SECTION}` 段声明了未导出的接口 `{fqn}`");
        }
    }

    let mut verified = Vec::with_capacity(comp_exports.len());
    for (fqn, (local_name, comp_iface_id)) in &comp_exports {
        let (_, doc_iface_id) = &doc_exports[fqn];
        let comp_wit = print_interface_without_docs(comp_resolve, *comp_iface_id, local_name)
            .with_context(|| format!("打印组件 `{component_name}` 导出接口 `{fqn}` 失败"))?;
        let doc_wit = print_interface_without_docs(doc_resolve, *doc_iface_id, local_name)
            .with_context(|| format!("打印 `{WIT_SECTION}` 接口 `{fqn}` 失败"))?;

        if comp_wit != doc_wit {
            bail!(
                "工具组件 `{component_name}` 导出接口 `{fqn}` 的签名与 `{WIT_SECTION}` 段不一致：\n组件导出：\n{comp_wit}\n文档声明：\n{doc_wit}"
            );
        }
        verified.push((fqn.clone(), *doc_iface_id));
    }

    wit_component::targets(doc_resolve, doc_world, component_bytes).with_context(|| {
        format!("工具组件 `{component_name}` 不符合 `{WIT_SECTION}` 中的 world")
    })?;

    Ok(verified)
}

fn collect_doc_exports(
    component_name: &str,
    doc_resolve: &Resolve,
    doc_pkg: PackageId,
) -> Result<(wit_parser::WorldId, ExportedInterfaces)> {
    let pkg = &doc_resolve.packages[doc_pkg];
    let world_count = pkg.worlds.len();
    if world_count != 1 {
        bail!(
            "工具组件 `{component_name}` 的 `{WIT_SECTION}` 包必须恰好包含 1 个 world，实际有 {world_count} 个"
        );
    }
    let world_id = *pkg.worlds.values().next().expect("已检查 world 数量为 1");
    let exports = collect_world_exports(
        doc_resolve,
        world_id,
        &format!("工具组件 `{component_name}` 的 `{WIT_SECTION}` world "),
    )?;
    Ok((world_id, exports))
}

fn print_interface_without_docs(resolve: &Resolve, id: InterfaceId, name: &str) -> Result<String> {
    let mut printer = WitPrinter::default();
    printer.emit_docs(false);
    printer.print_interface_outer(resolve, id, name)?;
    printer.output.indent_start();
    printer.print_interface(resolve, id)?;
    printer.output.indent_end();
    Ok(printer.output.to_string())
}

fn extract_interface_decls(
    resolve: &Resolve,
    iface_id: InterfaceId,
    iface_fqn: &str,
    out: &mut Vec<ToolDecl>,
) -> Result<()> {
    let iface = &resolve.interfaces[iface_id];
    for (func_name, func) in &iface.functions {
        if func.kind.resource().is_some() {
            bail!("工具接口 `{iface_fqn}` 不允许直接导出资源方法 `{func_name}`");
        }

        let script_name = script_tool_name(iface_fqn, func_name);
        let mut records = Vec::new();
        let mut seen_records = BTreeSet::new();
        let mut params = Vec::with_capacity(func.params.len());
        for param in &func.params {
            let member = format!("工具 `{script_name}` 的参数 `{}`", param.name);
            let mapped = map_wit_type(
                resolve,
                &param.ty,
                &member,
                true,
                false,
                &mut records,
                &mut seen_records,
            )?;
            params.push(ParamDecl {
                name: param.name.clone(),
                ty: mapped,
            });
        }

        let result = map_return_type(resolve, func, &script_name, &mut records, &mut seen_records)?;
        let templates = derive_templates(&params)?;

        let decl = ToolDecl {
            script_name,
            wit_interface: iface_fqn.to_owned(),
            wit_function: func_name.clone(),
            docs: normalize_docs(func.docs.contents.as_deref()),
            params,
            result,
            records,
            templates,
        };
        if decl.receiver().is_none() {
            bail!("工具 `{}` 缺少 `borrow` 句柄接收者参数", decl.script_name);
        }
        out.push(decl);
    }
    Ok(())
}

fn map_return_type(
    resolve: &Resolve,
    func: &Function,
    script_name: &str,
    records: &mut Vec<RecordDecl>,
    seen_records: &mut BTreeSet<String>,
) -> Result<ReturnType> {
    let Some(ret_ty) = &func.result else {
        return Ok(ReturnType::Unit);
    };
    let member = format!("工具 `{script_name}` 的返回值");

    if let Type::Id(id) = resolve_type(resolve, *ret_ty)
        && let TypeDefKind::Result(res) = &resolve.types[id].kind
    {
        if res.err.map(|t| resolve_type(resolve, t)) != Some(Type::String) {
            bail!("工具函数 `{script_name}` 返回的 `result` 错误类型必须是 `string`");
        }
        let ok = res
            .ok
            .as_ref()
            .map(|t| map_wit_type(resolve, t, &member, false, true, records, seen_records))
            .transpose()?;
        return Ok(ReturnType::Result { ok });
    }

    let direct = map_wit_type(resolve, ret_ty, &member, false, true, records, seen_records)?;
    Ok(ReturnType::Direct(direct))
}

fn map_wit_type(
    resolve: &Resolve,
    ty: &Type,
    member_desc: &str,
    allow_handle: bool,
    allow_record: bool,
    records: &mut Vec<RecordDecl>,
    seen_records: &mut BTreeSet<String>,
) -> Result<WitType> {
    match resolve_type(resolve, *ty) {
        Type::Bool => Ok(WitType::Bool),
        Type::String => Ok(WitType::String),
        Type::U8
        | Type::U16
        | Type::U32
        | Type::U64
        | Type::S8
        | Type::S16
        | Type::S32
        | Type::S64 => Ok(WitType::Int),
        Type::F32 | Type::F64 => Ok(WitType::Float),
        Type::Char => bail!("{member_desc} 使用了暂不支持的 WIT `char` 类型"),
        Type::ErrorContext => {
            bail!("{member_desc} 使用了暂不支持的 WIT `error-context` 类型")
        }
        Type::Id(id) => {
            let type_def = &resolve.types[id];
            match &type_def.kind {
                TypeDefKind::Option(inner) => {
                    if matches!(
                        resolve_type(resolve, *inner),
                        Type::Id(inner_id) if matches!(resolve.types[inner_id].kind, TypeDefKind::Option(_))
                    ) {
                        bail!("{member_desc} 不支持 `option` 直接嵌套 `option`");
                    }
                    Ok(WitType::Option(Box::new(map_wit_type(
                        resolve,
                        inner,
                        member_desc,
                        false,
                        allow_record,
                        records,
                        seen_records,
                    )?)))
                }
                TypeDefKind::List(inner) => Ok(WitType::List(Box::new(map_wit_type(
                    resolve,
                    inner,
                    member_desc,
                    false,
                    allow_record,
                    records,
                    seen_records,
                )?))),
                TypeDefKind::Record(rec) => {
                    let rec_name = type_def
                        .name
                        .clone()
                        .ok_or_else(|| anyhow!("{member_desc} 包含匿名 `record`"))?;
                    if !allow_record {
                        bail!("{member_desc} 不支持记录类型 `{rec_name}`");
                    }
                    if !seen_records.contains(&rec_name) {
                        let mut fields = Vec::with_capacity(rec.fields.len());
                        for field in &rec.fields {
                            let field_ty = map_wit_type(
                                resolve,
                                &field.ty,
                                &format!("记录 `{rec_name}` 的字段 `{}`", field.name),
                                false,
                                true,
                                records,
                                seen_records,
                            )?;
                            fields.push(FieldDecl {
                                name: field.name.clone(),
                                ty: field_ty,
                                docs: normalize_docs(field.docs.contents.as_deref()),
                            });
                        }
                        if seen_records.insert(rec_name.clone()) {
                            records.push(RecordDecl {
                                name: rec_name.clone(),
                                fields,
                                docs: normalize_docs(type_def.docs.contents.as_deref()),
                            });
                        }
                    }
                    Ok(WitType::Record(rec_name))
                }
                TypeDefKind::Handle(handle) => {
                    if !allow_handle {
                        bail!("{member_desc} 的句柄只能作为顶层参数出现");
                    }
                    let (res_id, borrow) = match handle {
                        Handle::Own(id) => (*id, false),
                        Handle::Borrow(id) => (*id, true),
                    };
                    let kind = resolve_host_handle_kind(resolve, res_id, member_desc)?;
                    Ok(WitType::Handle { kind, borrow })
                }
                other => bail!("{member_desc} 使用了暂不支持的 WIT 类型 `{other:?}`"),
            }
        }
    }
}

fn resolve_type(resolve: &Resolve, mut ty: Type) -> Type {
    while let Type::Id(id) = ty {
        let TypeDefKind::Type(next) = resolve.types[id].kind else {
            break;
        };
        ty = next;
    }
    ty
}

fn resolve_host_handle_kind(
    resolve: &Resolve,
    res_id: TypeId,
    context_name: &str,
) -> Result<HandleKind> {
    let Type::Id(target_id) = resolve_type(resolve, Type::Id(res_id)) else {
        bail!("工具 `{context_name}` 的句柄目标不是资源类型");
    };
    let target_def = &resolve.types[target_id];
    if !matches!(target_def.kind, TypeDefKind::Resource) {
        bail!("工具 `{context_name}` 的句柄目标不是资源类型");
    }
    let owner_fqn = match target_def.owner {
        wit_parser::TypeOwner::Interface(iface_id) => resolve.id_of(iface_id),
        _ => None,
    };
    if owner_fqn.as_deref() != Some(HOST_HANDLES_INTERFACE) {
        bail!(
            "工具 `{context_name}` 引用了非 `{HOST_HANDLES_INTERFACE}` 的资源 `{}`",
            target_def.name.as_deref().unwrap_or("<anon>")
        );
    }
    let res_name = target_def.name.as_deref().unwrap_or("");
    HandleKind::from_resource(res_name)
        .ok_or_else(|| anyhow!("工具 `{context_name}` 引用了未知宿主资源 `{res_name}`"))
}

fn derive_templates(params: &[ParamDecl]) -> Result<Templates> {
    let mut templates = Templates::default();
    for (idx, param) in params.iter().enumerate() {
        let WitType::Handle { kind, borrow } = param.ty else {
            continue;
        };
        if !borrow {
            bail!("工具句柄参数 `{}` 必须为 `borrow<...>`", param.name);
        }
        match kind {
            HandleKind::ReadDir => {
                templates
                    .coeffects
                    .push(CoEffectItem::Uses(idx, Some(Mode::Read)));
            }
            HandleKind::WriteDir => {
                templates
                    .coeffects
                    .push(CoEffectItem::Uses(idx, Some(Mode::ReadWrite)));
                templates
                    .effects
                    .push(EffectItem::Effect(idx, EffectKind::Write));
            }
            HandleKind::ExecEnv => {
                templates.coeffects.push(CoEffectItem::Uses(idx, None));
                templates.coeffects.push(CoEffectItem::MountsOf(idx));
                templates
                    .effects
                    .push(EffectItem::Effect(idx, EffectKind::Exec));
                templates
                    .effects
                    .push(EffectItem::WriteWritableMountsOf(idx));
                templates.effects.push(EffectItem::EmitViaEgressOf(idx));
            }
            HandleKind::Egress => {
                templates.coeffects.push(CoEffectItem::Uses(idx, None));
                templates
                    .effects
                    .push(EffectItem::Effect(idx, EffectKind::Emit));
            }
        }
    }
    Ok(templates)
}

fn normalize_docs(raw: Option<&str>) -> Option<String> {
    raw.map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}
