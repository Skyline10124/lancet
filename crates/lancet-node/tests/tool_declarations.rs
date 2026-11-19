// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

#[path = "../build/declaration.rs"]
mod declaration;
#[path = "../src/act/engine.rs"]
mod engine;

use lancet_node::act::ActRuntime;
use lancet_node::tools::render_act_stub;
use lancet_tool_decl::{
    CoEffectItem, EffectItem, EffectKind, HandleKind, Mode, ParamDecl, ReturnType, ToolDecl,
    WitType,
};
use wasm_encoder::{ComponentSection as _, CustomSection, Encode as _};
use wit_component::{ComponentEncoder, StringEncoding, dummy_module, embed_component_metadata};
use wit_parser::{ManglingAndAbi, Resolve};

const HOST_WIT: &str = include_str!("../wit/lancet-host/host.wit");

fn parse_with_host(tool_wit: &str) -> (Resolve, wit_parser::PackageId, wit_parser::WorldId) {
    let mut resolve = Resolve::default();
    resolve
        .push_str("host.wit", HOST_WIT)
        .expect("解析宿主 WIT 失败");
    let pkg = resolve
        .push_str("tool.wit", tool_wit)
        .expect("解析测试工具 WIT 失败");
    let world = resolve
        .select_world(&[pkg], None)
        .expect("选择测试工具 world 失败");
    (resolve, pkg, world)
}

fn build_dummy_component(resolve: &Resolve, world: wit_parser::WorldId) -> Vec<u8> {
    let mut module = dummy_module(resolve, world, ManglingAndAbi::Standard32);
    embed_component_metadata(&mut module, resolve, world, StringEncoding::UTF8)
        .expect("嵌入组件元数据失败");
    ComponentEncoder::default()
        .module(&module)
        .expect("加载核心模块失败")
        .validate(true)
        .encode()
        .expect("编码虚拟组件失败")
}

fn pack_from_resolve(
    resolve: &Resolve,
    pkg: wit_parser::PackageId,
    world: wit_parser::WorldId,
) -> Vec<u8> {
    let mut comp = build_dummy_component(resolve, world);
    let wit_pkg = wit_component::encode(resolve, pkg).expect("编码 WIT 包失败");
    CustomSection {
        name: declaration::WIT_SECTION.into(),
        data: wit_pkg.into(),
    }
    .append_to_component(&mut comp);
    comp
}

fn build_packed_dummy(comp_wit: &str, doc_wit: &str) -> Vec<u8> {
    let (comp_resolve, _, comp_world) = parse_with_host(comp_wit);
    let mut component = build_dummy_component(&comp_resolve, comp_world);

    let (doc_resolve, doc_pkg, _) = parse_with_host(doc_wit);
    let wit_pkg_bytes = wit_component::encode(&doc_resolve, doc_pkg).expect("编码 WIT 包失败");
    let section = CustomSection {
        name: declaration::WIT_SECTION.into(),
        data: wit_pkg_bytes.into(),
    };
    section.append_to_component(&mut component);
    component
}

#[test]
fn packaged_read_keeps_docs_and_signature() {
    let decl_json = include_str!(concat!(env!("OUT_DIR"), "/fs-read.decl.json"));
    let decls: Vec<ToolDecl> =
        serde_json::from_str(decl_json).expect("反序列化 fs-read.decl.json 失败");
    assert_eq!(decls.len(), 1, "fs-read 应包含 1 条工具声明");

    let decl = &decls[0];
    assert_eq!(decl.script_name, "fs.read");
    assert_eq!(decl.wit_interface, "lancet:fs-read/fs@0.1.0");
    assert_eq!(decl.wit_function, "read");
    let docs = decl.docs.as_deref().expect("fs.read 声明应保留 WIT 注释");
    assert!(docs.contains("读取目录 dir 中 path 处的文本文件"), "{docs}");
    assert!(docs.contains("missing-ok 为 true 时"), "{docs}");

    assert_eq!(
        decl.params,
        vec![
            ParamDecl {
                name: "dir".to_owned(),
                ty: WitType::Handle {
                    kind: HandleKind::ReadDir,
                    borrow: true,
                },
            },
            ParamDecl {
                name: "path".to_owned(),
                ty: WitType::String,
            },
            ParamDecl {
                name: "missing-ok".to_owned(),
                ty: WitType::Option(Box::new(WitType::Bool)),
            },
        ]
    );
    assert_eq!(
        decl.result,
        ReturnType::Result {
            ok: Some(WitType::Option(Box::new(WitType::String))),
        }
    );
    assert!(decl.records.is_empty());
    assert_eq!(
        decl.templates.coeffects,
        vec![CoEffectItem::Uses(0, Some(Mode::Read))]
    );
    assert!(decl.templates.effects.is_empty());

    let rt = ActRuntime::load().expect("加载 act 运行时失败");
    let builtin = rt.builtin_stub().expect("读取 builtin-stub 失败");
    let rendered = render_act_stub(&builtin, &decls);
    assert!(rendered.contains(&builtin), "渲染存根应保留原内置部分");
    assert!(
        rendered.contains("读取目录 dir 中 path 处的文本文件"),
        "渲染存根应包含 fs.read 注释：\n{rendered}"
    );
    assert!(
        rendered
            .contains("def read(self, path: str, missing_ok: bool | None = None) -> str | None:"),
        "渲染存根应包含 read 方法签名：\n{rendered}"
    );
    let write_dir_section = rendered
        .split("class WriteDir")
        .nth(1)
        .expect("渲染存根应包含 WriteDir 类");
    assert!(
        write_dir_section
            .contains("def read(self, path: str, missing_ok: bool | None = None) -> str | None:"),
        "WriteDir 应列出 ReadDir 的只读方法：\n{rendered}"
    );

    let cwasm_bytes = include_bytes!(concat!(env!("OUT_DIR"), "/fs-read.cwasm"));
    let engine = wasmtime::Engine::new(&engine::engine_config()).expect("创建共用引擎失败");
    // SAFETY: `fs-read.cwasm` 由同一构建以相同 `engine_config()` 预编译。
    unsafe { wasmtime::component::Component::deserialize(&engine, cwasm_bytes) }
        .expect("共用引擎应能反序列化 fs-read.cwasm");
}

#[test]
fn rejects_foreign_and_incompatible_host_imports() {
    let foreign_wasi = {
        let mut resolve = Resolve::default();
        resolve
            .push_str(
                "wasi.wit",
                r#"
                package wasi:cli@0.2.0;
                interface environment {
                    get-environment: func() -> list<tuple<string, string>>;
                }
                "#,
            )
            .unwrap();
        let pkg = resolve
            .push_str(
                "tool.wit",
                r#"
                package test:bad-wasi@0.1.0;
                interface api {
                    ping: func() -> string;
                }
                world tool {
                    import wasi:cli/environment@0.2.0;
                    export api;
                }
                "#,
            )
            .unwrap();
        let world = resolve.select_world(&[pkg], None).unwrap();
        pack_from_resolve(&resolve, pkg, world)
    };

    let foreign_other = {
        let mut resolve = Resolve::default();
        resolve
            .push_str(
                "other.wit",
                r#"
                package acme:extra@0.1.0;
                interface helper {
                    ping: func() -> string;
                }
                "#,
            )
            .unwrap();
        let pkg = resolve
            .push_str(
                "tool.wit",
                r#"
                package test:bad-other@0.1.0;
                interface api {
                    ping: func() -> string;
                }
                world tool {
                    import acme:extra/helper@0.1.0;
                    export api;
                }
                "#,
            )
            .unwrap();
        let world = resolve.select_world(&[pkg], None).unwrap();
        pack_from_resolve(&resolve, pkg, world)
    };

    let incompatible_host = {
        let mut resolve = Resolve::default();
        resolve
            .push_str(
                "stale-host.wit",
                r#"
                package lancet:host@0.1.0;
                interface handles {
                    resource read-dir {
                        read: func(path: string) -> result<list<u8>, string>;
                    }
                }
                "#,
            )
            .unwrap();
        let pkg = resolve
            .push_str(
                "tool.wit",
                r#"
                package test:stale@0.1.0;
                interface api {
                    use lancet:host/handles@0.1.0.{read-dir};
                    read-one: func(dir: borrow<read-dir>, path: string) -> result<string, string>;
                }
                world tool {
                    import lancet:host/handles@0.1.0;
                    export api;
                }
                "#,
            )
            .unwrap();
        let world = resolve.select_world(&[pkg], None).unwrap();
        pack_from_resolve(&resolve, pkg, world)
    };

    for (name, bytes, expected_needle) in [
        ("导入 WASI 接口", foreign_wasi, "wasi:cli/environment@0.2.0"),
        ("导入其他外部接口", foreign_other, "acme:extra/helper@0.1.0"),
        ("宿主方法签名不兼容", incompatible_host, "read-dir"),
    ] {
        let err = declaration::load_tool_declarations(name, &bytes)
            .expect_err(&format!("用例 `{name}` 应被拒绝"));
        let msg = format!("{err:#}");
        assert!(
            msg.contains(expected_needle),
            "用例 `{name}` 错误信息应写明 `{expected_needle}`，实际：{msg}"
        );
    }

    // 仅导入宿主接口子集（只含 read-dir 及其 read 方法）时应通过。
    let subset_wit = r#"
        package test:subset@0.1.0;
        interface api {
            use lancet:host/handles@0.1.0.{read-dir};
            /// 只读取单个文件。
            read-one: func(dir: borrow<read-dir>, path: string) -> result<option<string>, string>;
        }
        world tool {
            import lancet:host/handles@0.1.0;
            export api;
        }
    "#;
    let subset_bytes = {
        let mut resolve = Resolve::default();
        resolve
            .push_str(
                "subset-host.wit",
                r#"
                package lancet:host@0.1.0;
                interface handles {
                    resource read-dir {
                        read: func(path: string) -> result<option<string>, string>;
                    }
                }
                "#,
            )
            .unwrap();
        let pkg = resolve.push_str("tool.wit", subset_wit).unwrap();
        let world = resolve.select_world(&[pkg], None).unwrap();
        pack_from_resolve(&resolve, pkg, world)
    };
    let subset_decls = declaration::load_tool_declarations("subset", &subset_bytes)
        .expect("仅导入宿主接口子集应允许通过");
    assert_eq!(subset_decls.len(), 1);
    assert_eq!(subset_decls[0].script_name, "api.read_one");
}

#[test]
fn rejects_missing_wit_section() {
    let wit = r#"
        package test:no-wit@0.1.0;
        interface api {
            use lancet:host/handles@0.1.0.{read-dir};
            read-one: func(dir: borrow<read-dir>, path: string) -> result<string, string>;
        }
        world tool {
            import lancet:host/handles@0.1.0;
            export api;
        }
    "#;
    let (resolve, pkg, world) = parse_with_host(wit);
    let wit_pkg = wit_component::encode(&resolve, pkg).unwrap();

    // 1. 完全没有 lancet:wit 段。
    let without_section = build_dummy_component(&resolve, world);
    let err_missing = declaration::load_tool_declarations("no-section", &without_section)
        .expect_err("缺少 lancet:wit 段的组件应被拒绝");
    let msg_missing = format!("{err_missing:#}");
    assert!(
        msg_missing.contains("缺少顶层") && msg_missing.contains(declaration::WIT_SECTION),
        "错误信息应写明缺少顶层 `lancet:wit` 段：{msg_missing}"
    );

    // 2. 只有嵌套核心模块里存在同名段。
    let nested_only = {
        let mut module = dummy_module(&resolve, world, ManglingAndAbi::Standard32);
        embed_component_metadata(&mut module, &resolve, world, StringEncoding::UTF8).unwrap();
        let sec = CustomSection {
            name: declaration::WIT_SECTION.into(),
            data: wit_pkg.into(),
        };
        module.push(wasm_encoder::Section::id(&sec));
        sec.encode(&mut module);
        ComponentEncoder::default()
            .module(&module)
            .unwrap()
            .validate(true)
            .encode()
            .unwrap()
    };
    let err_nested = declaration::load_tool_declarations("nested-only", &nested_only)
        .expect_err("只有嵌套核心模块存在 lancet:wit 段的组件应被拒绝");
    let msg_nested = format!("{err_nested:#}");
    assert!(
        msg_nested.contains("缺少顶层") && msg_nested.contains(declaration::WIT_SECTION),
        "错误信息应写明缺少顶层 `lancet:wit` 段：{msg_nested}"
    );
}

#[test]
fn rejects_mismatched_documented_exports() {
    let base_wit = r#"
        package acme:sample@0.1.0;
        interface tool {
            use lancet:host/handles@0.1.0.{read-dir};
            record hit {
                path: string,
                line: u32,
            }
            search: func(dir: borrow<read-dir>, query: string) -> result<list<hit>, string>;
            ping: func(dir: borrow<read-dir>) -> result<_, string>;
        }
        world sample {
            import lancet:host/handles@0.1.0;
            export tool;
        }
    "#;

    let mismatch_cases = [
        (
            "参数类型不同",
            r#"
            package acme:sample@0.1.0;
            interface tool {
                use lancet:host/handles@0.1.0.{read-dir};
                record hit {
                    path: string,
                    line: u32,
                }
                search: func(dir: borrow<read-dir>, query: u32) -> result<list<hit>, string>;
                ping: func(dir: borrow<read-dir>) -> result<_, string>;
            }
            world sample {
                import lancet:host/handles@0.1.0;
                export tool;
            }
            "#,
            "query",
        ),
        (
            "参数名不同",
            r#"
            package acme:sample@0.1.0;
            interface tool {
                use lancet:host/handles@0.1.0.{read-dir};
                record hit {
                    path: string,
                    line: u32,
                }
                search: func(dir: borrow<read-dir>, needle: string) -> result<list<hit>, string>;
                ping: func(dir: borrow<read-dir>) -> result<_, string>;
            }
            world sample {
                import lancet:host/handles@0.1.0;
                export tool;
            }
            "#,
            "needle",
        ),
        (
            "少一个函数",
            r#"
            package acme:sample@0.1.0;
            interface tool {
                use lancet:host/handles@0.1.0.{read-dir};
                record hit {
                    path: string,
                    line: u32,
                }
                search: func(dir: borrow<read-dir>, query: string) -> result<list<hit>, string>;
            }
            world sample {
                import lancet:host/handles@0.1.0;
                export tool;
            }
            "#,
            "ping",
        ),
        (
            "记录字段不同",
            r#"
            package acme:sample@0.1.0;
            interface tool {
                use lancet:host/handles@0.1.0.{read-dir};
                record hit {
                    path: string,
                    row: u32,
                }
                search: func(dir: borrow<read-dir>, query: string) -> result<list<hit>, string>;
                ping: func(dir: borrow<read-dir>) -> result<_, string>;
            }
            world sample {
                import lancet:host/handles@0.1.0;
                export tool;
            }
            "#,
            "row",
        ),
    ];

    for (name, doc_wit, expected_diff) in mismatch_cases {
        let bytes = build_packed_dummy(base_wit, doc_wit);
        let err = declaration::load_tool_declarations(name, &bytes)
            .expect_err(&format!("用例 `{name}` 应被拒绝"));
        let msg = format!("{err:#}");
        assert!(
            msg.contains(expected_diff),
            "用例 `{name}` 报错应写明差异 `{expected_diff}`，实际：{msg}"
        );
    }

    // 经 use 引入的宿主记录不同：组件使用真实 HOST_WIT，lancet:wit 副本里 entry.is-dir 改为 u32。
    let host_use_wit = r#"
        package acme:host-use@0.1.0;
        interface tool {
            use lancet:host/handles@0.1.0.{read-dir, entry};
            list-items: func(dir: borrow<read-dir>, path: string) -> result<list<entry>, string>;
        }
        world sample {
            import lancet:host/handles@0.1.0;
            export tool;
        }
    "#;
    let host_use_bytes = {
        let (comp_resolve, _, comp_world) = parse_with_host(host_use_wit);
        let mut comp = build_dummy_component(&comp_resolve, comp_world);

        let mut doc_resolve = Resolve::default();
        let stale_host_wit = HOST_WIT.replacen("is-dir: bool", "is-dir: u32", 1);
        doc_resolve.push_str("host.wit", &stale_host_wit).unwrap();
        let doc_pkg = doc_resolve.push_str("tool.wit", host_use_wit).unwrap();
        let wit_pkg_bytes = wit_component::encode(&doc_resolve, doc_pkg).unwrap();
        CustomSection {
            name: declaration::WIT_SECTION.into(),
            data: wit_pkg_bytes.into(),
        }
        .append_to_component(&mut comp);
        comp
    };
    let host_use_err = declaration::load_tool_declarations("stale-use-record", &host_use_bytes)
        .expect_err("经 use 引入的过期宿主记录定义应被拒绝");
    let host_use_msg = format!("{host_use_err:#}");
    assert!(
        host_use_msg.contains("is-dir"),
        "过期宿主记录报错应写明差异 `is-dir`，实际：{host_use_msg}"
    );

    // 只改注释：签名相同，应通过并渲染出新说明，且反斜杠与双引号应正确转义。
    let updated_docs_wit = r#"
        package acme:sample@0.1.0;
        interface tool {
            use lancet:host/handles@0.1.0.{read-dir};
            /// 更新后的匹配记录说明。
            record hit {
                /// 文件相对路径。
                path: string,
                /// 命中行号。
                line: u32,
            }
            /// 更新后的搜索函数说明。
            search: func(dir: borrow<read-dir>, query: string) -> result<list<hit>, string>;
            /// 连通性探测，路径 C:\tmp，标记 "ok"
            ping: func(dir: borrow<read-dir>) -> result<_, string>;
        }
        world sample {
            import lancet:host/handles@0.1.0;
            export tool;
        }
    "#;
    let ok_bytes = build_packed_dummy(base_wit, updated_docs_wit);
    let decls = declaration::load_tool_declarations("docs-only", &ok_bytes)
        .expect("仅改动 WIT 注释时应通过签名核对");
    let search_decl = decls
        .iter()
        .find(|d| d.script_name == "tool.search")
        .expect("应包含 tool.search");
    assert_eq!(
        search_decl.docs.as_deref().map(str::trim),
        Some("更新后的搜索函数说明。")
    );

    let rt = ActRuntime::load().expect("加载 act 运行时失败");
    let builtin = rt.builtin_stub().expect("读取 builtin-stub 失败");
    let rendered = render_act_stub(&builtin, &decls);
    for expected in [
        "更新后的匹配记录说明。",
        "文件相对路径。",
        "更新后的搜索函数说明。",
        r#"连通性探测，路径 C:\\tmp，标记 \"ok\""#,
    ] {
        assert!(
            rendered.contains(expected),
            "渲染存根应包含新注释 `{expected}`：\n{rendered}"
        );
    }
}

#[test]
fn derives_templates_for_all_handle_parameters() {
    let multi_wit = r#"
        package test:multi@0.1.0;
        interface ops {
            use lancet:host/handles@0.1.0.{read-dir, write-dir, exec-env, egress};
            run-all: func(
                label: string,
                ro: borrow<read-dir>,
                rw: borrow<write-dir>,
                env: borrow<exec-env>,
                net: borrow<egress>,
                dry-run: option<bool>
            ) -> result<string, string>;
        }
        world multi {
            import lancet:host/handles@0.1.0;
            export ops;
        }
    "#;
    let bytes = build_packed_dummy(multi_wit, multi_wit);
    let decls =
        declaration::load_tool_declarations("multi", &bytes).expect("加载多句柄组件声明失败");
    assert_eq!(decls.len(), 1);
    let decl = &decls[0];

    assert_eq!(decl.receiver(), Some((1, HandleKind::ReadDir)));
    assert_eq!(
        decl.templates.coeffects,
        vec![
            CoEffectItem::Uses(1, Some(Mode::Read)),
            CoEffectItem::Uses(2, Some(Mode::ReadWrite)),
            CoEffectItem::Uses(3, None),
            CoEffectItem::MountsOf(3),
            CoEffectItem::Uses(4, None),
        ]
    );
    assert_eq!(
        decl.templates.effects,
        vec![
            EffectItem::Effect(2, EffectKind::Write),
            EffectItem::Effect(3, EffectKind::Exec),
            EffectItem::WriteWritableMountsOf(3),
            EffectItem::EmitViaEgressOf(3),
            EffectItem::Effect(4, EffectKind::Emit),
        ]
    );
}

#[test]
fn rejects_tool_without_handle_receiver() {
    let wit = r#"
        package test:no-recv@0.1.0;
        interface net {
            ping: func(target: string) -> result<string, string>;
        }
        world sample {
            export net;
        }
    "#;
    let bytes = build_packed_dummy(wit, wit);
    let err = declaration::load_tool_declarations("no-recv", &bytes)
        .expect_err("缺少 borrow 句柄接收者的工具应被拒绝");
    let msg = format!("{err:#}");
    assert!(msg.contains("net.ping"), "错误信息应点名工具名：{msg}");
}

#[test]
fn maps_type_aliases_and_rejects_unrepresentable_types() {
    let aliased_wit = r#"
        package test:aliases@0.1.0;
        interface tool {
            use lancet:host/handles@0.1.0.{read-dir};
            type path = string;
            type rel-path = path;
            type message = string;
            read-aliased: func(dir: borrow<read-dir>, p: rel-path) -> result<path, message>;
        }
        world sample {
            import lancet:host/handles@0.1.0;
            export tool;
        }
    "#;
    let aliased_bytes = build_packed_dummy(aliased_wit, aliased_wit);
    let aliased_decls = declaration::load_tool_declarations("aliases", &aliased_bytes)
        .expect("基本类型别名用作参数、返回值与 result 错误类型时应成功加载");
    assert_eq!(aliased_decls.len(), 1);
    assert_eq!(aliased_decls[0].params[1].ty, WitType::String);
    assert_eq!(
        aliased_decls[0].result,
        ReturnType::Result {
            ok: Some(WitType::String)
        }
    );

    let rejected_cases: &[(&str, &str, &[&str])] = &[
        (
            "顶层记录参数",
            r#"
            package test:rec-param@0.1.0;
            interface tool {
                use lancet:host/handles@0.1.0.{read-dir};
                record item { path: string }
                send: func(dir: borrow<read-dir>, payload: item) -> result<_, string>;
            }
            world sample {
                import lancet:host/handles@0.1.0;
                export tool;
            }
            "#,
            &["tool.send", "payload"],
        ),
        (
            "option 嵌套记录参数",
            r#"
            package test:rec-opt-param@0.1.0;
            interface tool {
                use lancet:host/handles@0.1.0.{read-dir};
                record item { path: string }
                send-opt: func(dir: borrow<read-dir>, maybe: option<item>) -> result<_, string>;
            }
            world sample {
                import lancet:host/handles@0.1.0;
                export tool;
            }
            "#,
            &["tool.send_opt", "maybe"],
        ),
        (
            "list 嵌套记录参数",
            r#"
            package test:rec-list-param@0.1.0;
            interface tool {
                use lancet:host/handles@0.1.0.{read-dir};
                record item { path: string }
                send-list: func(dir: borrow<read-dir>, batch: list<item>) -> result<_, string>;
            }
            world sample {
                import lancet:host/handles@0.1.0;
                export tool;
            }
            "#,
            &["tool.send_list", "batch"],
        ),
        (
            "参数中的直接嵌套 option",
            r#"
            package test:nested-opt-param@0.1.0;
            interface tool {
                use lancet:host/handles@0.1.0.{read-dir};
                bad-opt: func(dir: borrow<read-dir>, flag: option<option<bool>>) -> result<_, string>;
            }
            world sample {
                import lancet:host/handles@0.1.0;
                export tool;
            }
            "#,
            &["flag"],
        ),
        (
            "经别名直接嵌套的 option",
            r#"
            package test:nested-opt-alias@0.1.0;
            interface tool {
                use lancet:host/handles@0.1.0.{read-dir};
                type opt-str = option<string>;
                bad-alias-opt: func(dir: borrow<read-dir>, text-val: option<opt-str>) -> result<_, string>;
            }
            world sample {
                import lancet:host/handles@0.1.0;
                export tool;
            }
            "#,
            &["text-val"],
        ),
        (
            "记录字段中的嵌套 option",
            r#"
            package test:nested-opt-field@0.1.0;
            interface tool {
                use lancet:host/handles@0.1.0.{read-dir};
                record bad-rec { nested: option<option<string>> }
                fetch: func(dir: borrow<read-dir>) -> result<bad-rec, string>;
            }
            world sample {
                import lancet:host/handles@0.1.0;
                export tool;
            }
            "#,
            &["nested"],
        ),
        (
            "返回值中的嵌套 option",
            r#"
            package test:nested-opt-ret@0.1.0;
            interface tool {
                use lancet:host/handles@0.1.0.{read-dir};
                fetch: func(dir: borrow<read-dir>) -> result<option<option<string>>, string>;
            }
            world sample {
                import lancet:host/handles@0.1.0;
                export tool;
            }
            "#,
            &["tool.fetch"],
        ),
    ];

    for (name, wit, expected_needles) in rejected_cases {
        let bytes = build_packed_dummy(wit, wit);
        let err = declaration::load_tool_declarations(name, &bytes)
            .expect_err(&format!("用例 `{name}` 应被拒绝"));
        let msg = format!("{err:#}");
        for needle in *expected_needles {
            assert!(
                msg.contains(needle),
                "用例 `{name}` 错误信息应包含 `{needle}`，实际：{msg}"
            );
        }
    }
}
