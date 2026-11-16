//! 全部核对，结果写入 `suite.json` 与两份 `.pyi`。

use std::collections::BTreeSet;
use std::path::Path;
use std::time::Instant;

use anyhow::{Context, Result};
use serde::Serialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use wasmtime::component::{Component, Val};

use crate::decl::{self, HOST_INTERFACE, Interface, WitView};
use crate::manifest::{self, EFFECTS_SECTION, RawManifest, Templates, WIT_SECTION};
use crate::pyi;
use crate::runtime::{self, DOCS_REP, Host, NET_REP, WORK_REP};
use crate::star;

const TOOL: &str = "acme:notes/tool@0.1.0";

#[derive(Serialize)]
struct Check {
    id: String,
    title: String,
    /// `None` 为观察项，不设预期。
    pass: Option<bool>,
    detail: Value,
}

#[derive(Default)]
struct Suite {
    checks: Vec<Check>,
}

impl Suite {
    fn record(&mut self, id: &str, title: &str, pass: Option<bool>, detail: Value) {
        let tag = match pass {
            Some(true) => "PASS",
            Some(false) => "FAIL",
            None => "OBS ",
        };
        eprintln!("[{tag}] {id} {title}");
        self.checks.push(Check {
            id: id.into(),
            title: title.into(),
            pass,
            detail,
        });
    }

    fn check(&mut self, id: &str, title: &str, pass: bool, detail: Value) {
        self.record(id, title, Some(pass), detail);
    }

    fn observe(&mut self, id: &str, title: &str, detail: Value) {
        self.record(id, title, None, detail);
    }
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

fn sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// 声明摘要：接口签名与模板的规范 JSON。
fn decl_digest(iface: &Interface, templates: &Templates) -> String {
    let canonical =
        json!({ "interface": iface.id, "signature": iface.signature(), "templates": templates });
    sha256(&serde_json::to_vec(&canonical).expect("序列化"))
}

fn top_level(bytes: &[u8], name: &str) -> Result<Vec<manifest::Section>> {
    Ok(manifest::find_sections(bytes, name)?
        .into_iter()
        .filter(|s| s.depth == 1)
        .collect())
}

/// 从组件的 `lancet:wit` 段取带文档的同名接口。
fn docs_from_section(bytes: &[u8]) -> Result<Option<Interface>> {
    let sections = top_level(bytes, WIT_SECTION)?;
    if sections.len() != 1 {
        return Ok(None);
    }
    Ok(decl::decode_wit_package(&sections[0].data)?
        .into_iter()
        .find(|i| i.id == TOOL))
}

fn rejection(result: &std::result::Result<manifest::Validated, Vec<String>>) -> Vec<String> {
    match result {
        Ok(_) => Vec::new(),
        Err(errors) => errors.clone(),
    }
}

fn observed_effects(host: &Host, binding: &[(&str, u32)]) -> BTreeSet<String> {
    host.observed
        .iter()
        .filter(|o| o.action != "Read")
        .map(|o| {
            let param = binding
                .iter()
                .find(|(_, rep)| *rep == o.rep)
                .map_or("?", |(name, _)| *name);
            format!("{}<{param}>", o.action)
        })
        .collect()
}

fn template_effects(templates: &Templates, function: &str) -> BTreeSet<String> {
    templates.get(function).map_or_else(BTreeSet::new, |t| {
        t.effects.iter().map(ToString::to_string).collect()
    })
}

fn error_text(result: &Result<impl Sized>) -> Value {
    match result {
        Ok(_) => Value::Null,
        Err(e) => json!(format!("{e:#}")),
    }
}

/// 简单的确定性伪随机数，避免引入依赖。
struct XorShift(u64);

impl XorShift {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
}

pub fn run(artifacts: &Path, manifest_path: &Path, out: &Path) -> Result<()> {
    let read =
        |name: &str| std::fs::read(artifacts.join(name)).with_context(|| format!("读取 {name}"));
    let raw = read("notes.component.wasm")?;
    let packed = read("notes.packed.wasm")?;
    let manifest_bytes = std::fs::read(manifest_path)?;
    let base: Value = serde_json::from_slice(&manifest_bytes)?;
    let _: RawManifest = serde_json::from_value(base.clone())?;
    let mut s = Suite::default();

    // ---- D：解出接口 ----
    let view = decl::decode_component(&packed)?;
    let iface = view
        .exports
        .iter()
        .find(|i| i.id == TOOL)
        .context("缺少工具接口")?
        .clone();
    let expected = [
        "record hit { path: string, line: u32, text: string }",
        "grep(root: borrow<read-dir>, needle: string, max-hits: option<u32>) -> result<list<hit>, string>",
        "append-note(root: borrow<write-dir>, path: string, text: string) -> result<u32, string>",
        "fetch-into(net: borrow<egress>, root: borrow<write-dir>, url: string, path: string) -> result<u32, string>",
        "forge(root: borrow<read-dir>) -> result<_, string>",
    ];
    s.check(
        "D1",
        "wit-component 从组件解出导出接口：函数、参数名、参数与返回类型、记录",
        iface.signature() == expected && view.imports == [HOST_INTERFACE] && view.exports.len() == 1,
        json!({ "imports": view.imports, "exports": view.exports.iter().map(|i| &i.id).collect::<Vec<_>>(), "signature": iface.signature() }),
    );
    s.observe(
        "D2",
        "组件解码结果里 WIT 文档注释的条目数（源文件中有 9 条）",
        json!({ "doc_entries": iface.doc_count() }),
    );
    let docs_iface = docs_from_section(&packed)?;
    s.check(
        "D3",
        "从 lancet:wit 段解出带文档的 WIT 包；签名与组件导出接口一致，文档 9 条",
        docs_iface
            .as_ref()
            .is_some_and(|d| d.signature() == iface.signature() && d.doc_count() == 9),
        json!({
            "section_bytes": top_level(&packed, WIT_SECTION)?.first().map(|s| s.size),
            "doc_entries": docs_iface.as_ref().map(Interface::doc_count),
            "signature_equal": docs_iface.as_ref().map(|d| d.signature() == iface.signature()),
        }),
    );
    let wasip2 = read("notes.wasip2.wasm")?;
    let wasip2_view = decl::decode_component(&wasip2)?;
    let foreign: Vec<&String> = wasip2_view
        .imports
        .iter()
        .filter(|i| *i != HOST_INTERFACE)
        .collect();
    s.check(
        "D4",
        "导入名单核对拒绝 wasm32-wasip2 构建：同一源码多出 WASI 导入",
        !foreign.is_empty()
            && wasip2_view
                .exports
                .iter()
                .find(|i| i.id == TOOL)
                .map(Interface::signature)
                == Some(iface.signature()),
        json!({ "imports": wasip2_view.imports, "foreign_imports": foreign.len() }),
    );

    // ---- M：清单 ----
    let derived = manifest::derive_templates(&iface);
    let validated = manifest::validate_component(&packed, &view);
    s.check(
        "M1",
        "从组件顶层读出清单并通过 R1–R10；清单模板与由参数类型推出的模板相同",
        validated.as_ref().is_ok_and(|v| v.templates == derived),
        json!({ "errors": rejection(&validated), "templates": validated.as_ref().ok().map(|v| &v.templates), "derived": derived }),
    );
    let validated = validated.map_err(|e| anyhow::anyhow!("清单未通过：{e:?}"))?;

    let expect_reject = |s: &mut Suite,
                         id: &str,
                         title: &str,
                         bytes: &[u8],
                         view: &WitView,
                         rule: &str,
                         extra: Value| {
        let result = manifest::validate_component(bytes, view);
        let errors = rejection(&result);
        let pass = result.is_err() && errors.iter().any(|e| e.starts_with(rule));
        s.check(
            id,
            title,
            pass,
            json!({ "expected_rule": rule, "errors": errors, "extra": extra }),
        );
    };
    expect_reject(
        &mut s,
        "M2",
        "未打包的组件：顶层没有清单段",
        &raw,
        &view,
        "R1",
        Value::Null,
    );
    let linksection = read("notes.linksection.component.wasm")?;
    let ls_view = decl::decode_component(&linksection)?;
    let ls_depths: Vec<usize> = manifest::find_sections(&linksection, EFFECTS_SECTION)?
        .iter()
        .map(|x| x.depth)
        .collect();
    expect_reject(
        &mut s,
        "M3",
        "经 #[link_section] 写入的清单落在嵌套核心模块中，顶层读不到",
        &linksection,
        &ls_view,
        "R1",
        json!({ "section_depths": ls_depths }),
    );
    let strip_default = read("notes.packed.strip-default.wasm")?;
    let strip_all = read("notes.packed.strip-all.wasm")?;
    s.observe(
        "M4",
        "wasm-tools strip 之后：默认参数与 --all",
        json!({
            "default": { "effects_sections": top_level(&strip_default, EFFECTS_SECTION)?.len(), "wit_sections": top_level(&strip_default, WIT_SECTION)?.len(), "errors": rejection(&manifest::validate_component(&strip_default, &view)) },
            "all": { "effects_sections": top_level(&strip_all, EFFECTS_SECTION)?.len(), "wit_sections": top_level(&strip_all, WIT_SECTION)?.len(), "errors": rejection(&manifest::validate_component(&strip_all, &view)) },
        }),
    );
    let duplicated = manifest::append_section(
        &manifest::append_section(&raw, EFFECTS_SECTION, &manifest_bytes)?,
        EFFECTS_SECTION,
        &manifest_bytes,
    )?;
    expect_reject(
        &mut s,
        "M5",
        "顶层有两个清单段",
        &duplicated,
        &view,
        "R1",
        Value::Null,
    );

    type Mutation = Box<dyn Fn(&mut Value)>;
    let variants: Vec<(&str, &str, &str, Mutation)> = vec![
        (
            "N2",
            "R2",
            "不支持的格式版本",
            Box::new(|m| m["format"] = json!(2)),
        ),
        (
            "N3",
            "R2",
            "未知字段",
            Box::new(|m| m["functions"]["grep"]["pure"] = json!(true)),
        ),
        (
            "N4",
            "R3",
            "清单指向组件没有导出的接口",
            Box::new(|m| m["export"] = json!("acme:notes/other@0.1.0")),
        ),
        (
            "N5",
            "R4",
            "缺少导出函数 forge 的条目",
            Box::new(|m| {
                m["functions"].as_object_mut().unwrap().remove("forge");
            }),
        ),
        (
            "N6",
            "R4",
            "多出不存在的函数 delete-all",
            Box::new(|m| m["functions"]["delete-all"] = json!({ "effects": [], "coeffects": [] })),
        ),
        (
            "N7",
            "R5",
            "模板语法错误 Uses(root, Read)",
            Box::new(|m| m["functions"]["grep"]["coeffects"] = json!(["Uses(root, Read)"])),
        ),
        (
            "N8",
            "R5",
            "Uses 模板写进 effect 列表",
            Box::new(|m| m["functions"]["grep"]["effects"] = json!(["Uses<root, Read>"])),
        ),
        (
            "N9",
            "R6",
            "引用不存在的参数 dir",
            Box::new(|m| m["functions"]["grep"]["coeffects"] = json!(["Uses<dir, Read>"])),
        ),
        (
            "N10",
            "R7",
            "引用非句柄参数 needle",
            Box::new(|m| {
                m["functions"]["grep"]["coeffects"] =
                    json!(["Uses<root, Read>", "Uses<needle, Read>"])
            }),
        ),
        (
            "N11",
            "R8",
            "只读目录参数声明 ReadWrite",
            Box::new(|m| m["functions"]["grep"]["coeffects"] = json!(["Uses<root, ReadWrite>"])),
        ),
        (
            "N12",
            "R8",
            "只读目录参数声明 Write",
            Box::new(|m| m["functions"]["grep"]["effects"] = json!(["Write<root>"])),
        ),
        (
            "N13",
            "R8",
            "Emit 指向目录参数",
            Box::new(|m| {
                m["functions"]["fetch-into"]["effects"] = json!(["Emit<root>", "Write<root>"])
            }),
        ),
        (
            "N14",
            "R9",
            "出口参数没有 Uses 模板",
            Box::new(|m| {
                m["functions"]["fetch-into"]["coeffects"] = json!(["Uses<root, ReadWrite>"])
            }),
        ),
        (
            "N15",
            "R10",
            "可写目录参数不声明 Write",
            Box::new(|m| m["functions"]["append-note"]["effects"] = json!([])),
        ),
    ];
    let mut negatives = Vec::new();
    let mut all_rejected = true;
    {
        let bytes = manifest::append_section(&raw, EFFECTS_SECTION, b"{\"format\": 1,")?;
        let errors = rejection(&manifest::validate_component(&bytes, &view));
        all_rejected &= errors.iter().any(|e| e.starts_with("R2"));
        negatives.push(
            json!({ "id": "N1", "expected_rule": "R2", "title": "JSON 截断", "errors": errors }),
        );
    }
    for (id, rule, title, mutate) in &variants {
        let mut m = base.clone();
        mutate(&mut m);
        let bytes = manifest::append_section(&raw, EFFECTS_SECTION, &serde_json::to_vec(&m)?)?;
        let errors = rejection(&manifest::validate_component(&bytes, &view));
        let ok = errors.iter().any(|e| e.starts_with(rule));
        all_rejected &= ok;
        negatives.push(json!({ "id": id, "expected_rule": rule, "title": title, "rejected_as_expected": ok, "errors": errors }));
    }
    s.check(
        "M6",
        "15 种错误清单都被拒绝，且命中预期的规则",
        all_rejected,
        json!(negatives),
    );

    // M7：在每个函数的模板全集上变异，统计“通过”与“等于推出的模板”是否一致。
    let mut table = [[0u32; 2]; 2]; // [通过][相等]
    let mut rng = XorShift(0x9E37_79B9_7F4A_7C15);
    let mut single = 0u32;
    for f in &iface.functions {
        let mut universe: Vec<(bool, String)> = Vec::new();
        for p in &f.params {
            for kind in ["Write", "Exec", "Emit"] {
                universe.push((true, format!("{kind}<{}>", p.name)));
            }
            for mode in ["", ", Read", ", ReadWrite"] {
                universe.push((false, format!("Uses<{}{mode}>", p.name)));
            }
        }
        let entry = &base["functions"][&f.name];
        let base_set: BTreeSet<(bool, String)> = entry["effects"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| (true, t.as_str().unwrap().to_string()))
            .chain(
                entry["coeffects"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|t| (false, t.as_str().unwrap().to_string())),
            )
            .collect();
        let mut candidates: Vec<BTreeSet<(bool, String)>> = Vec::new();
        for item in &universe {
            let mut c = base_set.clone();
            if !c.remove(item) {
                c.insert(item.clone());
            }
            candidates.push(c);
            single += 1;
        }
        for _ in 0..500 {
            candidates.push(
                universe
                    .iter()
                    .filter(|_| rng.next() % 100 < 15)
                    .cloned()
                    .collect(),
            );
        }
        candidates.push(base_set.clone());
        for c in candidates {
            let mut m = base.clone();
            m["functions"][&f.name] = json!({
                "effects": c.iter().filter(|(e, _)| *e).map(|(_, t)| t).collect::<Vec<_>>(),
                "coeffects": c.iter().filter(|(e, _)| !*e).map(|(_, t)| t).collect::<Vec<_>>(),
            });
            let accepted = manifest::validate_manifest(&serde_json::to_vec(&m)?, &view).is_ok();
            let parsed: Option<manifest::FnTemplates> = (|| {
                let mut t = manifest::FnTemplates::default();
                for (effect, text) in &c {
                    let tpl = manifest::parse_template(text).ok()?;
                    if *effect {
                        t.effects.insert(tpl);
                    } else {
                        t.coeffects.insert(tpl);
                    }
                }
                Some(t)
            })();
            let equal = parsed.as_ref() == derived.get(&f.name);
            table[usize::from(accepted)][usize::from(equal)] += 1;
        }
    }
    s.check(
        "M7",
        "变异全集：清单通过当且仅当它等于由参数类型推出的模板",
        table[1][0] == 0 && table[0][1] == 0,
        json!({
            "single_toggles": single,
            "accepted_and_equal": table[1][1],
            "accepted_but_different": table[1][0],
            "rejected_but_equal": table[0][1],
            "rejected_and_different": table[0][0],
        }),
    );

    let rebuild_raw = read("notes.rebuild.component.wasm")?;
    let rebuild_packed = manifest::append_section(&rebuild_raw, EFFECTS_SECTION, &manifest_bytes)?;
    let rebuild_view = decl::decode_component(&rebuild_packed)?;
    let rebuild_iface = rebuild_view
        .exports
        .iter()
        .find(|i| i.id == TOOL)
        .context("重建缺少接口")?;
    let rebuild_validated = manifest::validate_component(&rebuild_packed, &rebuild_view)
        .map_err(|e| anyhow::anyhow!("{e:?}"))?;
    let compact = serde_json::to_vec(&base)?;
    let compact_packed = manifest::append_section(&raw, EFFECTS_SECTION, &compact)?;
    let compact_validated = manifest::validate_component(&compact_packed, &view)
        .map_err(|e| anyhow::anyhow!("{e:?}"))?;
    let digest = decl_digest(&iface, &validated.templates);
    s.check(
        "M8",
        "声明摘要只随签名与模板变化：另一目录重建、清单改格式后摘要不变，且等于由类型推出时的摘要",
        digest == decl_digest(rebuild_iface, &rebuild_validated.templates)
            && digest == decl_digest(&iface, &compact_validated.templates)
            && digest == decl_digest(&iface, &derived),
        json!({
            "digest": digest,
            "component_bytes_identical_across_builds": raw == rebuild_raw,
            "manifest_bytes": [manifest_bytes.len(), compact.len()],
        }),
    );

    // ---- W：wasmtime ----
    let engine = runtime::engine()?;
    let linker = runtime::linker(&engine)?;
    let t0 = Instant::now();
    let component = Component::new(&engine, &packed)?;
    let compile_ms = t0.elapsed().as_secs_f64() * 1e3;
    let w1 = runtime::instantiate(&engine, &linker, &component);
    s.check(
        "W1",
        "链接器只提供 lancet:host/handles，打包后的组件实例化成功（自定义段不影响加载）",
        w1.is_ok(),
        json!({ "error": error_text(&w1), "compile_ms": compile_ms }),
    );
    let w2 = runtime::instantiate(&engine, &linker, &Component::new(&engine, &wasip2)?);
    s.check(
        "W2",
        "wasm32-wasip2 构建在同一链接器下实例化失败",
        w2.is_err(),
        json!({ "error": error_text(&w2) }),
    );
    let stale = read("stale.component.wasm")?;
    let w3 = runtime::instantiate(&engine, &linker, &Component::new(&engine, &stale)?);
    s.check(
        "W3",
        "按旧宿主接口签名编译的组件在实例化时被拒绝",
        w3.is_err(),
        json!({ "error": error_text(&w3) }),
    );

    let mut sess = runtime::instantiate(&engine, &linker, &component)?;
    let root = sess.read_dir(DOCS_REP)?;
    let r = sess.call(
        TOOL,
        "grep",
        &[root, Val::String("TODO".into()), Val::Option(None)],
    );
    let eps = observed_effects(sess.store.data(), &[("root", DOCS_REP)]);
    let kappa = template_effects(&validated.templates, "grep");
    let hits = match &r {
        Ok(Val::Result(Ok(Some(v)))) => match v.as_ref() {
            Val::List(items) => items.len(),
            _ => 0,
        },
        _ => 0,
    };
    s.check(
        "W4",
        "动态调用 grep：返回 2 条匹配；实际 effect ⊆ 模板",
        hits == 2 && eps.is_subset(&kappa),
        json!({ "result": r.as_ref().map(runtime::val_json).ok(), "error": error_text(&r), "epsilon": eps, "kappa": kappa }),
    );

    let mut sess = runtime::instantiate(&engine, &linker, &component)?;
    let root = sess.write_dir(WORK_REP)?;
    let r = sess.call(
        TOOL,
        "append-note",
        &[
            root,
            Val::String("notes.md".into()),
            Val::String("second".into()),
        ],
    );
    let eps = observed_effects(sess.store.data(), &[("root", WORK_REP)]);
    let kappa = template_effects(&validated.templates, "append-note");
    s.check(
        "W5",
        "动态调用 append-note：写入发生，实际 effect ⊆ 模板",
        matches!(&r, Ok(Val::Result(Ok(_)))) && !eps.is_empty() && eps.is_subset(&kappa),
        json!({ "result": r.as_ref().map(runtime::val_json).ok(), "error": error_text(&r), "epsilon": eps, "kappa": kappa }),
    );

    let mut sess = runtime::instantiate(&engine, &linker, &component)?;
    let net = sess.egress(NET_REP)?;
    let root = sess.write_dir(WORK_REP)?;
    let r = sess.call(
        TOOL,
        "fetch-into",
        &[
            net,
            root,
            Val::String("https://example.com/".into()),
            Val::String("page.html".into()),
        ],
    );
    let eps = observed_effects(sess.store.data(), &[("net", NET_REP), ("root", WORK_REP)]);
    let kappa = template_effects(&validated.templates, "fetch-into");
    s.check(
        "W6",
        "动态调用 fetch-into：Emit 与 Write 都发生，实际 effect ⊆ 模板",
        matches!(&r, Ok(Val::Result(Ok(_)))) && eps.len() == 2 && eps.is_subset(&kappa),
        json!({ "result": r.as_ref().map(runtime::val_json).ok(), "error": error_text(&r), "epsilon": eps, "kappa": kappa }),
    );

    let mut sess = runtime::instantiate(&engine, &linker, &component)?;
    let root = sess.read_dir(DOCS_REP)?;
    let r = sess.call(TOOL, "grep", &[root, Val::U32(5), Val::Option(None)]);
    s.check(
        "W7",
        "参数类型不符（needle 传 u32）：调用前报错，组件未执行",
        r.is_err() && sess.store.data().observed.is_empty(),
        json!({ "error": error_text(&r), "host_calls": sess.store.data().observed.len() }),
    );

    let mut sess = runtime::instantiate(&engine, &linker, &component)?;
    let root = sess.read_dir(DOCS_REP)?;
    let r = sess.call(TOOL, "grep", &[root, Val::String("TODO".into())]);
    s.check(
        "W8",
        "参数个数不符：调用前报错",
        r.is_err() && sess.store.data().observed.is_empty(),
        json!({ "error": error_text(&r) }),
    );

    let mut sess = runtime::instantiate(&engine, &linker, &component)?;
    let root = sess.write_dir(WORK_REP)?;
    let r = sess.call(
        TOOL,
        "grep",
        &[root, Val::String("TODO".into()), Val::Option(None)],
    );
    s.check(
        "W9",
        "把可写目录资源传给 borrow<read-dir> 参数：资源类型不符，调用前报错",
        r.is_err() && sess.store.data().observed.is_empty(),
        json!({ "error": error_text(&r) }),
    );

    let mut sess = runtime::instantiate(&engine, &linker, &component)?;
    let root = sess.read_dir(DOCS_REP)?;
    let r = sess.call(TOOL, "forge", &[root]);
    s.check(
        "W10",
        "组件把只读句柄的编号当作可写句柄调用 write：陷入，宿主写函数未被调用",
        r.is_err() && sess.store.data().write_calls == 0,
        json!({ "error": error_text(&r), "write_calls": sess.store.data().write_calls }),
    );

    let t0 = Instant::now();
    let cwasm = engine.precompile_component(&packed)?;
    let precompile_ms = t0.elapsed().as_secs_f64() * 1e3;
    s.observe(
        "W11",
        "预编译产物中是否保留清单与 WIT 包",
        json!({
            "cwasm_bytes": cwasm.len(),
            "precompile_ms": precompile_ms,
            "has_manifest_text": contains(&cwasm, b"Uses<root, ReadWrite>"),
            "has_section_name": contains(&cwasm, EFFECTS_SECTION.as_bytes()),
            "has_wit_docs": contains(&cwasm, "一处匹配".as_bytes()),
            "has_param_names": contains(&cwasm, b"max-hits"),
        }),
    );
    let deserialized = unsafe { Component::deserialize(&engine, &cwasm)? };
    let from_wasm = runtime::component_signature(&engine, &component);
    let from_cwasm = runtime::component_signature(&engine, &deserialized);
    let mut sess = runtime::instantiate(&engine, &linker, &deserialized)?;
    let root = sess.read_dir(DOCS_REP)?;
    let r = sess.call(
        TOOL,
        "grep",
        &[
            root,
            Val::String("TODO".into()),
            Val::Option(Some(Box::new(Val::U32(1)))),
        ],
    );
    s.check(
        "W12",
        "预编译产物的组件类型给出同样的导入、导出函数与参数名，且可实例化调用",
        from_wasm == from_cwasm && r.is_ok(),
        json!({ "imports": from_cwasm.0, "exports": from_cwasm.1, "call": r.as_ref().map(runtime::val_json).ok() }),
    );

    // ---- S：Starlark 静态检查 ----
    let globals = star::globals(&iface)?;
    let scripts: [(&str, &str, bool, &str); 10] = [
        (
            "S1",
            "合法脚本：搜索、追加、推导、算术",
            true,
            "hits = docs.grep(\"TODO\")\nn = lancet.append_note(\"notes.md\", \"checked\")\npaths = [h.path for h in hits]\nreturn {\"paths\": paths, \"lines\": n + 1}",
        ),
        (
            "S2",
            "合法脚本：可选参数按位置传入；可写目录调用只读目录的方法",
            true,
            "hits = lancet.grep(\"TODO\", 5)\nreturn [h.line for h in hits]",
        ),
        ("S3", "参数类型错误", false, "return docs.grep(42)"),
        (
            "S4",
            "参数名错误",
            false,
            "return docs.grep(pattern = \"TODO\")",
        ),
        (
            "S5",
            "对只读目录调用写方法",
            false,
            "return docs.append_note(\"notes.md\", \"x\")",
        ),
        (
            "S6",
            "记录字段拼写错误",
            false,
            "return [h.lien for h in docs.grep(\"TODO\")]",
        ),
        (
            "S7",
            "返回值类型误用（int + str）",
            false,
            "return lancet.append_note(\"a.md\", \"b\") + \"x\"",
        ),
        (
            "S8",
            "把只读目录传给要求可写目录的参数",
            false,
            "return net.fetch_into(docs, \"https://example.com\", \"a.md\")",
        ),
        ("S9", "不存在的方法", false, "return docs.grepp(\"TODO\")"),
        (
            "S10",
            "缺少必需参数",
            false,
            "return lancet.append_note(\"a.md\")",
        ),
    ];
    let mut star_results = Vec::new();
    let mut star_pass = true;
    for (id, title, expect_ok, script) in scripts {
        let errors = star::typecheck(&globals, script);
        let ok = errors.is_empty() == expect_ok;
        star_pass &= ok;
        star_results.push(json!({ "id": id, "title": title, "expect_ok": expect_ok, "as_expected": ok, "script": script, "errors": errors }));
    }
    s.check(
        "S",
        "由 WIT 在运行时构造 Starlark 类型：2 个合法脚本通过，8 个错误脚本在执行前报错",
        star_pass,
        json!(star_results),
    );

    // ---- P：.pyi ----
    let stub_plain = pyi::render(&iface, None);
    let stub_docs = pyi::render(&iface, docs_iface.as_ref());
    std::fs::write(out.join("act-from-component.pyi"), &stub_plain)?;
    std::fs::write(out.join("act-with-wit-docs.pyi"), &stub_docs)?;
    s.observe(
        "P1",
        "渲染 .pyi：只用组件 / 加上 lancet:wit 段的文档",
        json!({
            "from_component": { "lines": stub_plain.lines().count(), "docstrings": stub_plain.matches("\"\"\"").count() / 2 },
            "with_wit_docs": { "lines": stub_docs.lines().count(), "docstrings": stub_docs.matches("\"\"\"").count() / 2 },
        }),
    );

    // ---- T：加载声明的耗时 ----
    let mut samples = Vec::new();
    for _ in 0..200 {
        let t = Instant::now();
        let v = decl::decode_component(&packed)?;
        let checked = manifest::validate_component(&packed, &v);
        let i = v
            .exports
            .iter()
            .find(|i| i.id == TOOL)
            .context("缺少接口")?;
        let d = docs_from_section(&packed)?;
        let stub = pyi::render(i, d.as_ref());
        let g = star::globals(i)?;
        std::hint::black_box((checked.is_ok(), stub.len(), g.names().count()));
        samples.push(t.elapsed().as_secs_f64() * 1e6);
    }
    samples.sort_by(f64::total_cmp);
    s.observe(
        "T1",
        "加载一个工具声明的耗时（解码接口、读清单并核对、读文档、渲染 .pyi、构造 Starlark 全局类型），200 次",
        json!({ "median_us": samples[100], "p95_us": samples[190], "max_us": samples[199], "component_bytes": packed.len() }),
    );

    let summary = json!({
        "checks": s.checks.iter().filter(|c| c.pass.is_some()).count(),
        "passed": s.checks.iter().filter(|c| c.pass == Some(true)).count(),
        "observations": s.checks.iter().filter(|c| c.pass.is_none()).count(),
    });
    eprintln!("{summary}");
    let report = json!({ "summary": summary, "checks": s.checks });
    std::fs::write(
        out.join("suite.json"),
        serde_json::to_string_pretty(&report)? + "\n",
    )?;
    Ok(())
}
