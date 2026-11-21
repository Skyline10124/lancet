<!-- Copyright 2026 Ast Rin -->
<!-- SPDX-License-Identifier: Apache-2.0 -->

# `lancet-node` 工作指引

本 crate 是 LANCET 外部上下文节点协议在 Linux 软件工作区上的节点服务进程。

## 职责与边界

- **承担**：节点配置加载与主机路径解析、路径与状态目录隔离启动校验与句柄名校验（`check-handle-name`）、节点状态目录管理与 `node_id` 持久化、持久调用记录账本与重启恢复（按 `writes_possible` 将遗留 `Pending` 结算为 `Indeterminate` 或 `Failed(InternalError)`）、Unix 域套接字上的五个协议操作（`hello`、`describe`、`execute`、`cancel`、`receipt`）与连接内多路复用、`lancet:host` 宿主 WIT 定义、宿主目录资源与逐分量路径解析（`openat2(RESOLVE_BENEATH | RESOLVE_NO_SYMLINKS)`、目录内相对符号链接、只读普通文件、UTF-8 与 128 MiB 读取边界、可写目录直接原子写宿主）、API 工具组件声明/冲突核对与模板推导、`.pyi` 类型存根合成、节点侧工具分发（κ 核对、隔离 `Store` 实例化、只读/可写目录槽位注入与 `read-dir` 收窄、WIT 数值转换、128 MiB 内存与 epoch 中断控制、8 MiB 紧凑 JSON 返回窗口、ε 记录 `Write`/`NotRetained` 与越出模板检查、产品工具 `fs.read`/`fs.list`/`fs.search`/`fs.write`/`fs.edit` 与 `StepSummary` 记录），以及 `lancet-act` 组件构建预编译、`builtin-stub` 读取与 `ActRuntime` 隔离执行。
- **尚未实现**：`exec-env`/`egress` 句柄注入、执行环境（#8）与网络出口（#9）。

## 阅读顺序

开始修改本 crate 前，先读：

1. [根 Agent 工作指引](../../AGENTS.md)
2. [节点配置与工作区合同](../../docs/architecture/lancet-node/config.md)
3. [API 工具与工具基座合同](../../docs/architecture/lancet-node/tools.md)
4. [`act` 解释器合同](../../docs/architecture/lancet-node/act.md)
5. [宿主资源合同](../../docs/architecture/lancet-node/host-resources.md)
6. [外部上下文节点协议合同](../../docs/architecture/lancet-protocol.md)
7. [工程规范](../../docs/development/engineering.md)

## 模块结构

| 模块 | 职责 |
| --- | --- |
| `build.rs` | 触发 `lancet-act` 内层 `wasm32-wasip2` 发布构建、`wasm-tools compose` WASI 适配器组合、`act.cwasm` 预编译、五个产品工具组件（`fs-read`、`fs-list`、`fs-search`、`fs-write`、`fs-edit`）跨组件冲突检查与打包（`product_tools.rs`），以及仅测试/基准使用的 `lancet-tool-probe` 打包 |
| `build/tool.rs` | 工具组件内层 `wasm32-unknown-unknown` 构建、`wasm-tools component new` 封装、`lancet:wit` 段附加与 `.decl.json`/`.cwasm` 生成 |
| `build/declaration.rs` | 核对工具组件仅导入 `lancet:host`、校验顶层 `lancet:wit` 与导出签名一致性、跨组件方法与记录定义冲突校验、读取声明并推导 co-effect/effect 模板 |
| `wit/lancet-host/host.wit` | 宿主接口 `lancet:host@0.1.0` 定义（`read-dir`、`write-dir`、`exec-env`、`egress` 资源类型与 `entry` 记录） |
| `src/lib.rs` | 库入口（公开 `pub mod act`、`pub mod dispatch`、`pub mod read_dir` 与 `pub mod tools`） |
| `src/tools.rs` | 读取构建时内嵌产品工具声明与预编译产物并把内置存根与 API 工具声明合成为 `.pyi` 类型存根 |
| `src/read_dir.rs` | 宿主目录资源 `HostDir` 与逐分量路径解析：`openat2(RESOLVE_BENEATH \| RESOLVE_NO_SYMLINKS)`、目录内相对符号链接、UTF-8 与 128 MiB 读取上限，以及在已核验父目录内写临时文件并改名就位的原子写入 |
| `src/dispatch.rs` | 工具分发层 `Dispatcher`：持有 `DirSlot`（`ReadOnly` / `Writable`）与 `ActContext`、κ 核对、预链接组件实例化、`ResourceTable` 句柄注入与 `ReadDirResource` 收窄、JSON↔WIT 编解码、128 MiB 内存与 epoch 中断控制、8 MiB 紧凑 JSON 返回窗口、`StepSummary` 记录与统一 `Write`/`NotRetained` ε 归集及越出模板检查 |
| `src/act.rs` | 预编译组件反序列化加载、读取 `builtin-stub`、`check-handle-name` 校验、`ActHost` 回调接口链接 `lancet:act/dispatch.call`（`trappable`）、每调用独立 `Store`、128 MiB 线性内存上限、epoch 截止与取消控制 |
| `src/act/engine.rs` | 预编译与运行时共用的 `wasmtime::Config` 定义 |
| `src/main.rs` | 解析命令行参数并按启动顺序调用各模块（配置加载与句柄名校验、路径隔离校验、状态目录打开、`HostDir` 打开与 `Dispatcher` 装配、账本恢复与 `serve`） |
| `src/config.rs` | 配置类型定义、TOML 解析、主机路径展开/规范化与引用校验 |
| `src/config/startup.rs` | 启动路径校验（工作目录互不重叠、配置文件与状态目录在所有工作目录之外、可写工作目录不能是状态目录或其子目录） |
| `src/config/catalog.rs` | 由配置、`act` 类型存根与工具声明推出环境简述（含 `ReadDir` 与 `WriteDir` 句柄的 `modes`/`effects`）、`seam` 边集、`tools`（`[act]`）与 `catalog_digest` 的唯一构造入口 |
| `src/state.rs` | 节点状态目录创建、`flock` 独占锁、目录刷盘与 `node_id` 持久读写 |
| `src/calls.rs` | 调用记录账本：`CallRecord` 与 SHA-256 文件名、查重与持久登记（含 `writes_possible`）、`in_flight` 取消标志、启动期遗留调用结算及回执到期 |
| `src/execute.rs` | 受理请求校验（目录摘要、`act` 参数解析与 `output_limit`）、`Dispatcher` 执行、交回载荷紧凑 JSON 字节核对、`steps`/`effects` 归集、结局映射、先持久结算再写回套接字 |
| `src/server.rs` | Unix 域套接字绑定、连接内多路复用分派、`hello`/`describe`/`execute`/`cancel`/`receipt` 协议处理与断连取消 |
| `tests/dispatch.rs` | 工具分发层集成测试（WIT 数值范围、陷入隔离、工具超限中断、可写槽位收窄、写后失败保留宿主内容与 `NotRetained` 回执、effect 越出模板与耗时基准 `measure_tool_call_overhead`） |
| `tests/node.rs` | 节点进程黑盒测试入口（`tests/node/calls.rs` 与 `tests/node/writable.rs`） |
| `tests/fixtures/tool-probe/` | 仅用于 `tests/dispatch.rs` 的探针工具组件（测试多参数 WIT 数值边界、资源误用陷入、写后报错、双句柄越界写、`Spin`/高内存场景） |

## 验证命令

```sh
cargo test -p lancet-node
cargo test -p lancet-node --test tool_declarations
cargo test -p lancet-node --test act
cargo test -p lancet-node --test dispatch
cargo test -p lancet-node --test node
cargo test --release -p lancet-node --test act measure_component_runtime -- --ignored --nocapture --test-threads=1
cargo test --release -p lancet-node --test dispatch measure_tool_call_overhead -- --ignored --nocapture --test-threads=1
cargo xtask ci
```
