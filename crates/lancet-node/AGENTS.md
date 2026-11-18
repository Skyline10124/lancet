<!-- Copyright 2026 Ast Rin -->
<!-- SPDX-License-Identifier: Apache-2.0 -->

# `lancet-node` 工作指引

本 crate 是 LANCET 外部上下文节点协议在 Linux 软件工作区上的节点服务进程。

## 职责与边界

- **承担**：节点配置加载与主机路径解析、三项总是适用的启动校验、节点状态目录管理与 `node_id` 持久化，以及 Unix 域套接字上的 `hello` 与 `describe` 协议服务。
- **尚未实现**：`execute`、`cancel`、`receipt` 与调用记录账本、Starlark 解释器组件、API 工具、影子工作区、执行环境与网络出口。

## 阅读顺序

开始修改本 crate 前，先读：

1. [根 Agent 工作指引](../../AGENTS.md)
2. [节点配置与工作区合同](../../docs/architecture/lancet-node/config.md)
3. [外部上下文节点协议合同](../../docs/architecture/lancet-protocol.md)
4. [工程规范](../../docs/development/engineering.md)

## 模块结构

| 模块 | 职责 |
| --- | --- |
| `src/main.rs` | 解析命令行参数并按启动顺序调用各模块；出错时向标准错误输出中文错误链并退出 |
| `src/config.rs` | 配置类型定义、TOML 解析、主机路径展开/规范化与引用校验 |
| `src/config/startup.rs` | 三项启动校验（工作目录互不重叠、配置与状态目录位置校验、`FICLONE` 探针） |
| `src/config/catalog.rs` | 由配置推出环境简述、`seam` 边集、`tools` 与 `catalog_digest` 的唯一构造入口 |
| `src/state.rs` | 节点状态目录创建、`flock` 独占锁与 `node_id` 持久读写 |
| `src/server.rs` | Unix 域套接字绑定、连接线程分派与 `hello`/`describe` 协议处理 |

## 验证命令

```sh
cargo test -p lancet-node
cargo xtask ci
```
