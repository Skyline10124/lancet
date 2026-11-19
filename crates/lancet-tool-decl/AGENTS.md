<!-- Copyright 2026 Ast Rin -->
<!-- SPDX-License-Identifier: Apache-2.0 -->

# `lancet-tool-decl` 工作指引

本 crate 定义 API 工具声明、效果模板与脚本名称映射的可序列化共享类型，供 `lancet-node` 与 `lancet-act` 共用。

## 职责与边界

- **承担**：定义工具声明（`ToolDecl`、`ParamDecl`、`FieldDecl`、`RecordDecl`）、WIT 简化数据类型（`HandleKind`、`WitType`、`ReturnType`）、符号模板（`Templates`、`CoEffectItem`、`EffectItem`、`Mode`、`EffectKind`）、解释器调用上下文（`ActContext`、`HandleGrant`），以及纯名称/位置映射（`wit_to_ident`、`wit_to_type_name`、`script_tool_name`、`first_optional_param`）。
- **不承担**：WIT 解析、组件解码、签名核对、模板推导、`.pyi` 渲染、Starlark 类型构造或工具分发。
- **依赖约束**：只依赖 `serde`，不引入 WIT 解析、JSON 运行时或 `wasmtime` 依赖。

## 阅读顺序

开始修改本 crate 前，先读：

1. [根 Agent 工作指引](../../AGENTS.md)
2. [API 工具与工具基座合同](../../docs/architecture/lancet-node/tools.md)
3. [`act` 与 Starlark 编排合同](../../docs/architecture/lancet-node/act.md)
4. [工程规范](../../docs/development/engineering.md)

## 模块结构

| 模块 | 职责 |
| --- | --- |
| `src/lib.rs` | 声明数据类型、效果模板定义与名称/接收者/可选参数映射辅助函数 |

## 验证命令

```sh
cargo clippy -p lancet-tool-decl --all-targets -- -D warnings
cargo test -p lancet-tool-decl
cargo xtask ci
```
