<!-- Copyright 2026 Ast Rin -->
<!-- SPDX-License-Identifier: Apache-2.0 -->

# `lancet-node` 工作指引

本 crate 是 LANCET 外部上下文节点协议在 Linux 软件工作区上的节点服务进程。

## 职责与边界

- **承担**：节点配置加载与主机路径解析、三项总是适用的启动校验、节点状态目录管理与 `node_id` 持久化、持久调用记录账本与重启恢复、Unix 域套接字上的五个协议操作（`hello`、`describe`、`execute`、`cancel`、`receipt`）与连接内多路复用、`lancet:host` 宿主 WIT 定义、API 工具组件声明核对与模板推导、`.pyi` 类型存根合成，以及 `lancet-act` 组件构建预编译、`builtin-stub` 读取与 `ActRuntime` 隔离执行（含 `ActContext` 传入与 `dispatch` 宿主回调）。
- **尚未实现**：节点侧真实工具分发（κ 核对、工具实例化、句柄收窄、ε 记录与核对、返回窗口）、产品工具列表（当前内嵌工具声明为空列表，`execute` 分发恒返回错误），以及影子工作区、目录资源实现、执行环境与网络出口。

## 阅读顺序

开始修改本 crate 前，先读：

1. [根 Agent 工作指引](../../AGENTS.md)
2. [节点配置与工作区合同](../../docs/architecture/lancet-node/config.md)
3. [组件运行时合同](../../docs/architecture/lancet-node/tools.md)
4. [`act` 解释器合同](../../docs/architecture/lancet-node/act.md)
5. [外部上下文节点协议合同](../../docs/architecture/lancet-protocol.md)
6. [工程规范](../../docs/development/engineering.md)

## 模块结构

| 模块 | 职责 |
| --- | --- |
| `build.rs` | 触发 `lancet-act` 内层 `wasm32-wasip2` 发布构建、`wasm-tools compose` WASI 适配器组合、`act.cwasm` 预编译，以及测试用 `lancet-fs-read` 工具组件打包 |
| `build/tool.rs` | 工具组件内层 `wasm32-unknown-unknown` 构建、`wasm-tools component new` 封装、`lancet:wit` 段附加与 `.decl.json`/`.cwasm` 生成 |
| `build/declaration.rs` | 核对工具组件仅导入 `lancet:host`、校验顶层 `lancet:wit` 与导出签名一致性、读取声明并推导 co-effect/effect 模板 |
| `wit/lancet-host/host.wit` | 宿主接口 `lancet:host@0.1.0` 定义（`read-dir`、`write-dir`、`exec-env`、`egress` 资源类型） |
| `src/lib.rs` | 库入口（公开 `pub mod act` 与 `pub mod tools`） |
| `src/tools.rs` | 读取内嵌工具声明并把内置存根与 API 工具声明合成为 `.pyi` 类型存根 |
| `src/act.rs` | 预编译组件反序列化加载、读取 `builtin-stub`、链接 `lancet:act/dispatch` 回调、每调用独立 `Store`、128 MiB 线性内存上限、epoch 截止与取消控制 |
| `src/act/engine.rs` | 预编译与运行时共用的 `wasmtime::Config` 定义 |
| `src/main.rs` | 解析命令行参数并按启动顺序调用各模块（含账本恢复与 `render_act_stub`）；出错时向标准错误输出中文错误链并退出 |
| `src/config.rs` | 配置类型定义、TOML 解析、主机路径展开/规范化与引用校验 |
| `src/config/startup.rs` | 三项启动校验（工作目录互不重叠、配置与状态目录位置校验、`FICLONE` 探针） |
| `src/config/catalog.rs` | 由配置与 `act` 类型存根推出环境简述、`seam` 边集、`tools`（`[act]`）与 `catalog_digest` 的唯一构造入口 |
| `src/state.rs` | 节点状态目录创建、`flock` 独占锁、目录刷盘与 `node_id` 持久读写 |
| `src/calls.rs` | 调用记录账本：查重与持久登记、`in_flight` 取消标志、重启恢复与回执到期 |
| `src/execute.rs` | 受理请求校验（目录摘要、`act` 参数解析与 `output_limit`）、注入空上下文及过渡期 `dispatch` 错误回调、`content` 组合、结局映射与回执构造 |
| `src/server.rs` | Unix 域套接字绑定、连接内多路复用分派、`hello`/`describe`/`execute`/`cancel`/`receipt` 协议处理与断连取消 |

## 验证命令

```sh
cargo test -p lancet-node
cargo test -p lancet-node --test tool_declarations
cargo test -p lancet-node --test act
cargo test -p lancet-node --test node
cargo test --release -p lancet-node --test act measure_component_runtime -- --ignored --nocapture --test-threads=1
cargo xtask ci
```
