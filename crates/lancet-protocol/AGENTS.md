<!-- Copyright 2026 Ast Rin -->
<!-- SPDX-License-Identifier: Apache-2.0 -->

# `lancet-protocol` 工作指引

本 crate 是 LANCET 外部上下文节点协议的纯协议库，由节点实现与调用方适配器共同依赖。

## 职责与边界

- **承担**：字节流帧编解码（4 字节大端前缀、8 MiB 负载上限）、五种协议操作消息与回复、结局与错误码类型、环境简述、`effect` / `co-effect`、`Receipt` / `ReceiptSummary`（16 KiB 裁剪）、`residue` 合法范围，以及 `catalog_digest` 规范摘要计算。
- **不承担**：节点服务进程、套接字与连接状态机、调用记录账本、持久化存储、WASM 运行时与沙箱、调用方对账与协调层适配器逻辑。
- **依赖约束**：只依赖 `serde`、`serde_json` 与 `sha2`，不引入异步运行时、存储引擎或执行运行时依赖。

## 阅读顺序

开始修改本 crate 前，先读：

1. [根 Agent 工作指引](../../AGENTS.md)
2. [外部上下文节点协议合同](../../docs/architecture/lancet-protocol.md)
3. [工程规范](../../docs/development/engineering.md)

## 模块结构

| 模块 | 职责 |
| --- | --- |
| `src/frame.rs` | `MAX_FRAME_BYTES`（8 MiB）、`Frame<T>`、`read_frame` / `write_frame` 纯流编解码与有界序列化辅助 |
| `src/message.rs` | `ProtocolVersion`、`Capabilities`、`Labels`、`ToolSpec`、`ToolError`、`Request`、`Response`、`ExecuteOutcome`、`ReceiptLookup` |
| `src/environment.rs` | `MAX_ENVIRONMENT_SUMMARY_BYTES`（4 KiB）、`EffectKind`、`Mode`、`SeamDirection`、`SeamPolicy`、`Handle`、`Seam`、`EnvironmentSummary` 唯一校验入口 |
| `src/receipt.rs` | `MAX_RECEIPT_SUMMARY_BYTES`（16 KiB）、`Receipt`、`ReceiptOutcome`、`EffectEntry`、`Change`、`Residue` 约束与 `ReceiptSummary` 裁剪/解码上限 |
| `src/digest.rs` | `CatalogDigest` 与 `catalog_digest` 规范 JSON SHA-256 摘要计算 |

## 验证命令

```sh
cargo test -p lancet-protocol
cargo xtask ci
```
