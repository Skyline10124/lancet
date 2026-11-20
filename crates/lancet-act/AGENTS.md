<!-- Copyright 2026 Ast Rin -->
<!-- SPDX-License-Identifier: Apache-2.0 -->

# `lancet-act` 工作指引

本 crate 实现 LANCET `act` 工具使用的沙箱化 Starlark 解释器 WASM 组件（`wasm32-wasip2`）。

## 职责与边界

- **承担**：实现 [act.wit](wit/act.wit) 导出的 `lancet:act/interpreter.run`、`builtin-stub` 与 `check-handle-name` 接口、由 `context-json` 构造名义句柄/记录类型与绑定方法（静态检查与运行期校验）并经 `lancet:act/dispatch.call` 发出子调用、内置 `discard()` 经 `lancet:act/dispatch.discard` 通知宿主丢弃影子草稿、顶层 `return` 自动包裹与行号映射、解析修复提示、`AstModule::typecheck` 预检查、每次调用隔离的 `print` 缓冲、纯计算全局环境（移除 `dir`/`getattr`/`hasattr`/`type`，提供 `sum`/`regex`/`text`/`discard` 与 `TextView`）及其紧凑 `.pyi` 类型存根生成，以及本地 WASI 适配器 [wasi-local.wat](wasi-local.wat)。
- **不承担**：宿主侧资源上限、epoch 超时、取消控制、影子恢复或真实工具实例化（由 [lancet-node](../lancet-node/AGENTS.md) 的 `ActRuntime` 与分发层负责）；文件 I/O、环境变量读取、真实时钟或直接系统调用。

## 阅读顺序

开始修改本 crate 前，先读：

1. [根 Agent 工作指引](../../AGENTS.md)
2. [`act` 解释器合同](../../docs/architecture/lancet-node/act.md)
3. [组件运行时合同](../../docs/architecture/lancet-node/tools.md)
4. [工程规范](../../docs/development/engineering.md)

## 模块结构

| 文件 | 职责 |
| --- | --- |
| `wit/act.wit` | `lancet:act` 接口定义（导入 `dispatch` 的 `call` 与 `discard`，导出 `interpreter` 的 `run(script, context-json)`、`builtin-stub` 与 `check-handle-name(name)`） |
| `wasi-local.wat` | 与 `wasm-tools compose` 组合使用的纯确定性/trap 本地 WASI 适配器（保留 `lancet:act/dispatch` 导入由宿主链接） |
| `src/lib.rs` | 导出组件接口 `Guest` 实现并解析 `context-json`、串联全局环境构建、类型存根生成、句柄名校验与脚本执行 |
| `src/handles.rs` | 由 `ActContext` 构造名义句柄类型与记录类型、`HandleValue` / `BoundMethod` 值实现及 `dispatch.call` 参数/返回值编解码 |
| `src/script.rs` | 脚本包裹与行/列映射、解析修复提示、`AstModule::typecheck` 检查、逐次 `print` 缓冲与求值 |
| `src/globals.rs` | 构建并裁剪 Starlark 全局环境（注入句柄方法与内置函数）、句柄名合法性词法检查、注册 `sum`/`text`/`regex`/`discard` 并从文档生成内置 `.pyi` 存根 |
| `src/text.rs` | `TextView` 只读文本视图及其 `.grep` / `.head` / `.tail` / `.splitlines` 方法与 JSON 序列化 |

## 验证命令

```sh
cargo clippy -p lancet-act --target wasm32-wasip2 --lib -- -D warnings
cargo test -p lancet-node --test act
```
