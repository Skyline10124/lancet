//! 由导出接口渲染 `.pyi` 类型存根：第一个参数是句柄的函数绑定为该句柄类型的方法。

use std::fmt::Write as _;

use crate::decl::{FuncDecl, HandleKind, Interface, Ty};

pub fn snake(name: &str) -> String {
    name.replace('-', "_")
}

pub fn pascal(name: &str) -> String {
    name.split('-')
        .map(|part| {
            let mut chars = part.chars();
            chars
                .next()
                .map_or(String::new(), |c| c.to_uppercase().chain(chars).collect())
        })
        .collect()
}

pub fn py_type(ty: &Ty) -> String {
    match ty {
        Ty::Prim(p) => match p.as_str() {
            "bool" => "bool".into(),
            "f32" | "f64" => "float".into(),
            "char" | "string" => "str".into(),
            _ => "int".into(),
        },
        Ty::List(e) => format!("list[{}]", py_type(e)),
        Ty::Option(e) => format!("{} | None", py_type(e)),
        Ty::Tuple(ts) => {
            let parts: Vec<String> = ts.iter().map(py_type).collect();
            format!("tuple[{}]", parts.join(", "))
        }
        Ty::Record(name) => pascal(name),
        Ty::Handle { kind, .. } => kind.stub().into(),
        Ty::Result { .. } | Ty::Foreign(_) => "object".into(),
    }
}

/// 返回值：`result<T, E>` 的错误使脚本中止，存根只写 `T`。
pub fn py_return(ty: Option<&Ty>) -> String {
    match ty {
        None => "None".into(),
        Some(Ty::Result { ok: None, .. }) => "None".into(),
        Some(Ty::Result { ok: Some(ok), .. }) => py_type(ok),
        Some(other) => py_type(other),
    }
}

/// 末尾连续的 `option<T>` 参数在存根中可省略，默认 `None`。
pub fn first_optional(params: &[crate::decl::Param]) -> usize {
    let mut index = params.len();
    while index > 0 && matches!(params[index - 1].ty, Ty::Option(_)) {
        index -= 1;
    }
    index
}

/// 函数绑定到哪个句柄类型：第一个参数的句柄种类。
pub fn receiver(f: &FuncDecl) -> Option<HandleKind> {
    match f.params.first().map(|p| &p.ty) {
        Some(Ty::Handle { kind, .. }) => Some(*kind),
        _ => None,
    }
}

fn docstring(out: &mut String, indent: &str, docs: Option<&str>) {
    if let Some(docs) = docs {
        let _ = writeln!(out, "{indent}\"\"\"{}\"\"\"", docs.trim());
    }
}

/// `docs` 为带文档的同名接口（来自 WIT 包）；为 `None` 时存根不含说明。
pub fn render(iface: &Interface, docs: Option<&Interface>) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "# 由 {} 的 WIT 接口生成。\n", iface.id);
    for record in &iface.records {
        let doc_record = docs.and_then(|d| d.records.iter().find(|r| r.name == record.name));
        let _ = writeln!(out, "class {}:", pascal(&record.name));
        docstring(&mut out, "    ", doc_record.and_then(|r| r.docs.as_deref()));
        for field in &record.fields {
            let _ = writeln!(out, "    {}: {}", snake(&field.name), py_type(&field.ty));
            let doc = doc_record
                .and_then(|r| r.fields.iter().find(|f| f.name == field.name))
                .and_then(|f| f.docs.as_deref());
            docstring(&mut out, "    ", doc);
        }
        out.push('\n');
    }
    for kind in HandleKind::ALL {
        let base = if kind == HandleKind::WriteDir {
            "(ReadDir)"
        } else {
            ""
        };
        let _ = writeln!(out, "class {}{base}:", kind.stub());
        let methods: Vec<&FuncDecl> = iface
            .functions
            .iter()
            .filter(|f| receiver(f) == Some(kind))
            .collect();
        if methods.is_empty() {
            out.push_str("    ...\n\n");
            continue;
        }
        for f in methods {
            let rest = &f.params[1..];
            let optional = first_optional(rest);
            let mut params = vec!["self".to_string()];
            for (i, p) in rest.iter().enumerate() {
                let default = if i >= optional { " = None" } else { "" };
                params.push(format!("{}: {}{default}", snake(&p.name), py_type(&p.ty)));
            }
            let _ = writeln!(
                out,
                "    def {}({}) -> {}:",
                snake(&f.name),
                params.join(", "),
                py_return(f.result.as_ref())
            );
            let doc = docs
                .and_then(|d| d.function(&f.name))
                .and_then(|d| d.docs.as_deref());
            if doc.is_some() {
                docstring(&mut out, "        ", doc);
            } else {
                out.push_str("        ...\n");
            }
        }
        out.push('\n');
    }
    out
}
