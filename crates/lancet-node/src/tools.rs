// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! `act` 工具的 `.pyi` 类型存根合成与内嵌声明读取。

use std::collections::BTreeSet;
use std::fmt::Write as _;

use lancet_tool_decl::{
    HandleKind, ParamDecl, RecordDecl, ToolDecl, WitType, first_optional_param, wit_to_ident,
    wit_to_type_name,
};

/// 构建时打包的产品工具组件条目。
pub struct EmbeddedComponent {
    pub name: &'static str,
    pub decls_json: &'static str,
    pub cwasm: &'static [u8],
}

/// 构建时生成的产品 API 工具组件清单（`fs-read`、`fs-list`、`fs-search`、`fs-write`、`fs-edit`、`proc-run`、`proc-pipe`）。
pub const EMBEDDED_COMPONENTS: &[EmbeddedComponent] =
    &include!(concat!(env!("OUT_DIR"), "/product_tools.rs"));

fn parse_component_decls(component: &EmbeddedComponent) -> Vec<ToolDecl> {
    serde_json::from_str(component.decls_json)
        .unwrap_or_else(|e| panic!("内嵌工具 `{}` 声明 JSON 应合法：{e}", component.name))
}

/// 返回本版本内嵌的产品 API 工具声明列表。
pub fn embedded_tools() -> Vec<ToolDecl> {
    EMBEDDED_COMPONENTS
        .iter()
        .flat_map(parse_component_decls)
        .collect()
}

/// 返回本版本内嵌的产品工具组件声明与预编译产物。
pub fn embedded_artifacts() -> Vec<crate::dispatch::ComponentArtifact<'static>> {
    EMBEDDED_COMPONENTS
        .iter()
        .map(|component| crate::dispatch::ComponentArtifact {
            name: component.name,
            decls: parse_component_decls(component),
            cwasm: component.cwasm,
        })
        .collect()
}

/// 由解释器内置存根与 API 工具声明合成 `act` 的 `.pyi` 说明。
pub fn render_act_stub(builtin_stub: &str, tools: &[ToolDecl]) -> String {
    if tools.is_empty() {
        return builtin_stub.to_owned();
    }

    let mut sections = Vec::new();
    if !builtin_stub.trim().is_empty() {
        sections.push(builtin_stub.trim_end().to_owned());
    }

    let mut seen_records = BTreeSet::new();
    for tool in tools {
        for record in &tool.records {
            if seen_records.insert(record.name.clone()) {
                sections.push(render_record(record));
            }
        }
    }

    for kind in HandleKind::ALL {
        sections.push(render_handle_class(kind, tools));
    }

    sections.join("\n\n")
}

fn render_record(record: &RecordDecl) -> String {
    let mut out = format!("class {}:", wit_to_type_name(&record.name));
    if let Some(docs) = &record.docs {
        out.push('\n');
        push_docstring(&mut out, docs, "    ");
    }
    if record.fields.is_empty() {
        out.push_str("\n    ...");
        return out;
    }
    for field in &record.fields {
        if let Some(docs) = &field.docs {
            for line in docs.lines() {
                let _ = write!(out, "\n    # {line}");
            }
        }
        let _ = write!(
            out,
            "\n    {}: {}",
            wit_to_ident(&field.name),
            format_wit_type(&field.ty, false)
        );
    }
    out
}

fn render_handle_class(kind: HandleKind, tools: &[ToolDecl]) -> String {
    let methods: Vec<&ToolDecl> = tools
        .iter()
        .filter(|tool| {
            tool.receiver()
                .is_some_and(|(_, recv_kind)| recv_kind.accepts(kind))
        })
        .collect();

    let mut out = format!("class {}:", kind.stub_name());
    if methods.is_empty() {
        out.push_str("\n    ...");
        return out;
    }

    for tool in methods {
        let block = render_function_block(&tool.method_name(), tool);
        for line in block.lines() {
            let _ = write!(out, "\n    {line}");
        }
    }
    out
}

fn render_function_block(name: &str, tool: &ToolDecl) -> String {
    let method_params: Vec<&ParamDecl> = tool.method_params().into_iter().map(|(_, p)| p).collect();
    let opt_start = first_optional_param(&method_params);

    let mut params_str = vec!["self".to_owned()];
    for (idx, param) in method_params.iter().enumerate() {
        let ty_str = format_wit_type(&param.ty, true);
        let default_suffix = if idx >= opt_start { " = None" } else { "" };
        params_str.push(format!(
            "{}: {ty_str}{default_suffix}",
            wit_to_ident(&param.name)
        ));
    }

    let ret_str = tool
        .result
        .ok_type()
        .map_or_else(|| "None".to_owned(), |ty| format_wit_type(ty, false));

    let mut out = format!("def {name}({}) -> {ret_str}:", params_str.join(", "));
    if let Some(docs) = &tool.docs {
        out.push('\n');
        push_docstring(&mut out, docs, "    ");
    } else {
        out.push_str(" ...");
    }
    out
}

fn format_wit_type(ty: &WitType, is_param: bool) -> String {
    match ty {
        WitType::Bool => "bool".to_owned(),
        WitType::String => "str".to_owned(),
        WitType::Int => "int".to_owned(),
        WitType::Float => "float".to_owned(),
        WitType::Option(inner) => format!("{} | None", format_wit_type(inner, is_param)),
        WitType::List(inner) => format!("list[{}]", format_wit_type(inner, is_param)),
        WitType::Record(name) => wit_to_type_name(name),
        WitType::Handle { kind, .. } if is_param => HandleKind::ALL
            .into_iter()
            .filter(|actual| kind.accepts(*actual))
            .map(HandleKind::stub_name)
            .collect::<Vec<_>>()
            .join(" | "),
        WitType::Handle { kind, .. } => kind.stub_name().to_owned(),
    }
}

fn push_docstring(out: &mut String, docs: &str, indent: &str) {
    let escaped = docs.replace('\\', "\\\\").replace('"', "\\\"");
    let lines: Vec<&str> = escaped.lines().collect();
    if lines.len() <= 1 {
        let _ = write!(
            out,
            "{indent}\"\"\"{}\"\"\"",
            lines.first().copied().unwrap_or("")
        );
    } else {
        let _ = write!(out, "{indent}\"\"\"");
        for line in lines {
            if line.is_empty() {
                out.push('\n');
            } else {
                let _ = write!(out, "\n{indent}{line}");
            }
        }
        let _ = write!(out, "\n{indent}\"\"\"");
    }
}
