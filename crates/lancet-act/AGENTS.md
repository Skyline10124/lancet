<!-- Copyright 2026 Ast Rin -->
<!-- SPDX-License-Identifier: Apache-2.0 -->

# `lancet-act` 工作指引

本 crate 实现 LANCET `act` 工具使用的沙箱化 Starlark 解释器 WASM 组件（`wasm32-wasip2`）。

## 职责与边界

- **承担**：实现 [act.wit](wit/act.wit) 导出的 `lancet:act/interpreter.run` 接口、顶层 `return` 自动包裹与行号映射、解析修复提示、`AstModule::typecheck` 预检查、每次调用隔离的 `print` 缓冲、纯计算全局环境（移除 `dir`/`getattr`/`hasattr`/`type`，提供 `sum`、`regex`、`text` 与 `TextView`），以及无导入的本地 WASI 适配器 [wasi-local.wat](wasi-local.wat)。
- **不承担**：宿主侧资源上限、epoch 超时或取消控制（由 [lancet-node](../lancet-node/AGENTS.md) 的 `ActRuntime` 负责）；文件 I/O、环境变量读取、真实时钟或外部工具调用。

## 阅读顺序

开始修改本 crate 前，先读：

1. [根 Agent 工作指引](../../AGENTS.md)
2. [`act` 解释器合同](../../docs/architecture/lancet-node/act.md)
3. [组件运行时合同](../../docs/architecture/lancet-node/tools.md)
4. [工程规范](../../docs/development/engineering.md)

## 模块结构

| 文件 | 职责 |
| --- | --- |
| `wit/act.wit` | `lancet:act/interpreter` 组件接口定义（`phase`、`payload`、`diagnostic`、`run`） |
| `wasi-local.wat` | 与 `wasm-tools compose --no-imports` 组合使用的纯确定性/trap 本地 WASI 适配器 |
| `src/lib.rs` | 导出组件接口 `Guest` 实现并串联全局环境构建与脚本执行 |
| `src/script.rs` | 脚本包裹与行/列映射、解析修复提示、`AstModule::typecheck` 检查、逐次 `print` 缓冲与求值 |
| `src/globals.rs` | 构建并裁剪 Starlark 纯计算全局环境，注册 `sum`、`text` 与 `regex` |
| `src/text.rs` | `TextView` 只读文本视图及其 `.grep` / `.head` / `.tail` / `.splitlines` 方法与 JSON 序列化 |

## 验证命令

```sh
cargo clippy -p lancet-act --target wasm32-wasip2 --lib -- -D warnings
cargo test -p lancet-node --test act
```
