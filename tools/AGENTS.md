<!-- Copyright 2026 Ast Rin -->
<!-- SPDX-License-Identifier: Apache-2.0 -->

# `tools/` 工作指引

本目录存放 `lancet-node` 内置的 API 工具 WASM 组件（`fs-read`、`fs-list`、`fs-search`、`fs-write`、`fs-edit`、`fs-reset`、`fs-status`、`fs-diff`）。每个工具是一个独立的 workspace crate，编译为 `wasm32-unknown-unknown` 核心模块，再由 `lancet-node` 打包并核对声明。

## 职责与边界

- **承担**：实现各工具导出的 WIT 接口逻辑；只导入宿主接口 `lancet:host/handles@0.1.0`；各工具 `wit/deps/lancet-host` 经相对符号链接指向 [crates/lancet-node/wit/lancet-host](../crates/lancet-node/wit/lancet-host)，保证宿主 WIT 唯一。
- **不承担**：WASI 接口导入、`lancet:wit` 自定义段封装、签名核对与预编译（由 [lancet-node](../crates/lancet-node/AGENTS.md) 构建脚本完成），以及目录权限、影子工作区、`hidden` 过滤或 `ε` 记录等宿主实现。

## 阅读顺序

修改或新增工具 crate 前，先读：

1. [根 Agent 工作指引](../AGENTS.md)
2. [API 工具与工具基座合同](../docs/architecture/lancet-node/tools.md)
3. [工程规范](../docs/development/engineering.md)

## 工具清单

| 目录 / Crate | 导出接口 | 脚本工具名 |
| --- | --- | --- |
| `fs-read/`（`lancet-fs-read`） | `lancet:fs-read/fs@0.1.0` | `fs.read` |
| `fs-list/`（`lancet-fs-list`） | `lancet:fs-list/fs@0.1.0` | `fs.list` |
| `fs-search/`（`lancet-fs-search`） | `lancet:fs-search/fs@0.1.0` | `fs.search` |
| `fs-write/`（`lancet-fs-write`） | `lancet:fs-write/fs@0.1.0` | `fs.write` |
| `fs-edit/`（`lancet-fs-edit`） | `lancet:fs-edit/fs@0.1.0` | `fs.edit` |
| `fs-reset/`（`lancet-fs-reset`） | `lancet:fs-reset/fs@0.1.0` | `fs.reset` |
| `fs-status/`（`lancet-fs-status`） | `lancet:fs-status/fs@0.1.0` | `fs.status` |
| `fs-diff/`（`lancet-fs-diff`） | `lancet:fs-diff/fs@0.1.0` | `fs.diff` |

## 验证命令

```sh
cargo clippy -p lancet-fs-read -p lancet-fs-list -p lancet-fs-search -p lancet-fs-write -p lancet-fs-edit -p lancet-fs-reset -p lancet-fs-status -p lancet-fs-diff --target wasm32-unknown-unknown --lib -- -D warnings
cargo test -p lancet-node --test tool_declarations
cargo xtask ci
```
