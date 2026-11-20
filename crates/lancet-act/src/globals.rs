// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! `act` 解释器全局环境构建与扩展函数。

use std::collections::BTreeMap;

use regex::Regex;
use starlark::docs::{DocFunction, DocItem, DocMember, DocModule, DocParam, DocReturn, DocType};
use starlark::environment::{Globals, GlobalsBuilder, LibraryExtension};
use starlark::starlark_module;
use starlark::typing::Ty;
use starlark::values::Heap;
use starlark::values::ValueOfUnchecked;
use starlark::values::float::UnpackFloat;
use starlark::values::none::NoneType;
use starlark::values::typing::StarlarkIter;

use crate::text::TextView;
use lancet_tool_decl::ActContext;

/// 移除反思与元编程全局函数后构造 `act` 使用的全局环境，并按上下文注入句柄与绑定方法。
pub fn build_globals(context: &ActContext) -> Globals {
    let standard = Globals::standard();

    let mut builder = GlobalsBuilder::new();
    builder.frozen_heap().add_reference(standard.heap());
    for (name, value) in standard.iter() {
        if !matches!(name, "dir" | "getattr" | "hasattr" | "type") {
            builder.set(name, value);
        }
    }
    register_act_globals(&mut builder);
    crate::handles::register_context(&mut builder, context);
    builder.build()
}

/// 校验句柄名是否为合法 Starlark 标识符且不与内置全局或包装入口函数重名。
pub fn check_handle_name(name: &str) -> Result<(), String> {
    if starlark_syntax::lexer::lex_exactly_one_identifier(name).as_deref() != Some(name) {
        return Err(format!("`{name}` 不是合法的 Starlark 标识符"));
    }
    let globals = build_globals(&ActContext::default());
    if name == crate::script::ENTRY_FN || globals.names().any(|g| g.as_str() == name) {
        return Err(format!("`{name}` 与内置全局重名"));
    }
    Ok(())
}

/// 从 `act` 扩展、新增全局与 `TextView` 的文档生成紧凑、按名称排序的 `.pyi` 类型存根（不含标准内置）。
pub fn builtin_stub() -> String {
    let mut builder = GlobalsBuilder::new();
    register_act_globals(&mut builder);
    let docs = builder.build().documentation();
    let mut entries: BTreeMap<&str, String> = BTreeMap::new();

    for (name, item) in &docs.members {
        if let Some(rendered) = render_doc_item(name, item) {
            entries.insert(name.as_str(), rendered);
        }
    }

    let text_view = DocType::from_starlark_value::<TextView>();
    if let Some(rendered) = render_doc_type("TextView", &text_view) {
        entries.insert("TextView", rendered);
    }

    entries.into_values().collect::<Vec<_>>().join("\n\n")
}

fn register_act_globals(builder: &mut GlobalsBuilder) {
    for ext in [
        LibraryExtension::Json,
        LibraryExtension::SetType,
        LibraryExtension::StructType,
        LibraryExtension::Map,
        LibraryExtension::Filter,
        LibraryExtension::Print,
    ] {
        ext.add(builder);
    }
    register_core_helpers(builder);
    register_regex(builder);
}

fn render_doc_item(name: &str, item: &DocItem) -> Option<String> {
    match item {
        DocItem::Member(member) => Some(render_doc_member(name, member)),
        DocItem::Module(module) => render_doc_module(name, module),
        DocItem::Type(ty) => render_doc_type(name, ty),
    }
}

fn render_doc_member(name: &str, member: &DocMember) -> String {
    match member {
        DocMember::Function(func) => render_function(name, func),
        DocMember::Property(prop) => format!("{name}: {}", prop.typ),
    }
}

fn render_function(name: &str, func: &DocFunction) -> String {
    render_doc_function(name, func, false, None)
}

fn render_doc_function(
    name: &str,
    func: &DocFunction,
    with_self: bool,
    ret_override: Option<Ty>,
) -> String {
    let mut params = func.params.clone();
    if with_self {
        let self_param = DocParam {
            name: "self".to_owned(),
            docs: None,
            typ: Ty::any(),
            default_value: None,
        };
        if params.pos_only.is_empty() {
            params.pos_or_named.insert(0, self_param);
        } else {
            params.pos_only.insert(0, self_param);
        }
    }

    let rendered = DocFunction {
        docs: None,
        params,
        ret: DocReturn {
            docs: None,
            typ: ret_override.unwrap_or_else(|| func.ret.typ.clone()),
        },
    }
    .render_as_code(name);
    if func.ret.typ.to_string().contains("struct(..)") {
        rendered.replace("struct(..)", "struct")
    } else {
        rendered
    }
}

fn render_doc_module(name: &str, module: &DocModule) -> Option<String> {
    let members: BTreeMap<&str, &DocItem> = module
        .members
        .iter()
        .map(|(k, v)| (k.as_str(), v))
        .collect();
    if members.is_empty() {
        return None;
    }

    let mut out = format!("class {name}:");
    for (member_name, item) in members {
        if let Some(rendered) = render_doc_item(member_name, item) {
            out.push('\n');
            out.push_str(&indent_block(&rendered));
        }
    }
    Some(out)
}

fn render_doc_type(name: &str, ty: &DocType) -> Option<String> {
    let members: BTreeMap<&str, &DocMember> =
        ty.members.iter().map(|(k, v)| (k.as_str(), v)).collect();
    if ty.constructor.is_none() && members.is_empty() {
        return None;
    }

    let mut class_block = format!("class {name}:");
    if let Some(ctor) = &ty.constructor {
        let rendered = render_doc_function("__init__", ctor, true, Some(Ty::none()));
        class_block.push('\n');
        class_block.push_str(&indent_block(&rendered));
    }
    for (member_name, member) in members {
        let rendered = match member {
            DocMember::Function(func) => render_doc_function(member_name, func, true, None),
            DocMember::Property(prop) => format!("{member_name}: {}", prop.typ),
        };
        class_block.push('\n');
        class_block.push_str(&indent_block(&rendered));
    }
    Some(class_block)
}

fn indent_block(block: &str) -> String {
    block
        .lines()
        .map(|line| format!("    {line}"))
        .collect::<Vec<_>>()
        .join("\n")
}

#[starlark_module]
fn register_core_helpers(builder: &mut GlobalsBuilder) {
    fn sum<'v>(
        xs: ValueOfUnchecked<'v, StarlarkIter<UnpackFloat>>,
        heap: Heap<'v>,
    ) -> starlark::Result<ValueOfUnchecked<'v, UnpackFloat>> {
        let mut acc = heap.alloc(0i32);
        for item in xs.get().iterate(heap)? {
            acc = acc.add(item, heap)?;
        }
        Ok(ValueOfUnchecked::new(acc))
    }

    fn text(s: &str) -> anyhow::Result<TextView> {
        Ok(TextView::new(s))
    }

    fn discard() -> anyhow::Result<NoneType> {
        crate::lancet::act::dispatch::discard().map_err(anyhow::Error::msg)?;
        Ok(NoneType)
    }
}

fn register_regex(builder: &mut GlobalsBuilder) {
    #[starlark_module]
    fn regex_members(builder: &mut GlobalsBuilder) {
        fn r#match(pattern: &str, s: &str) -> anyhow::Result<bool> {
            let re = Regex::new(pattern)?;
            Ok(re.find(s).is_some_and(|m| m.start() == 0))
        }

        fn findall(pattern: &str, s: &str) -> anyhow::Result<Vec<String>> {
            let re = Regex::new(pattern)?;
            Ok(re.find_iter(s).map(|m| m.as_str().to_owned()).collect())
        }

        fn replace(pattern: &str, replacement: &str, s: &str) -> anyhow::Result<String> {
            let re = Regex::new(pattern)?;
            Ok(re.replace_all(s, replacement).into_owned())
        }
    }

    builder.namespace("regex", regex_members);
}
