<!-- Copyright 2026 Ast Rin -->
<!-- SPDX-License-Identifier: Apache-2.0 -->

# `lancet-node` 工作指引

本 crate 是 LANCET 外部上下文节点协议在 Linux 软件工作区上的节点服务进程。

## 职责与边界

- **承担**：节点配置加载与主机路径解析、三项总是适用的启动校验与句柄名校验（`check-handle-name`）、节点状态目录管理与 `node_id` 持久化、持久调用记录账本与重启恢复、Unix 域套接字上的五个协议操作（`hello`、`describe`、`execute`、`cancel`、`receipt`）与连接内多路复用、`lancet:host` 宿主 WIT 定义、只读目录宿主资源实现（`openat2(RESOLVE_BENEATH | RESOLVE_NO_SYMLINKS)` 逐分量解析与 `hidden` 过滤）、API 工具组件声明/冲突核对与模板推导、`.pyi` 类型存根合成、节点侧工具分发（κ 核对、隔离 `Store` 实例化、句柄注入与类型收窄、WIT 数值转换、128 MiB 内存与 epoch 中断控制、8 MiB 紧凑 JSON 返回窗口、只读产品工具 `fs.read`/`fs.list`/`fs.search` 与 `StepSummary` 记录），以及 `lancet-act` 组件构建预编译、`builtin-stub` 读取与 `ActRuntime` 隔离执行。
- **尚未实现**：`write-dir`/`exec-env`/`egress` 工具注入及对应 `ε` 记录/越界检查，以及影子工作区、执行环境与网络出口。

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
| `build.rs` | 触发 `lancet-act` 内层 `wasm32-wasip2` 发布构建、`wasm-tools compose` WASI 适配器组合、`act.cwasm` 预编译、产品工具组件（`fs-read`、`fs-list`、`fs-search`）跨组件冲突检查与打包（`product_tools.rs`），以及仅测试/基准使用的 `lancet-tool-probe` 打包 |
| `build/tool.rs` | 工具组件内层 `wasm32-unknown-unknown` 构建、`wasm-tools component new` 封装、`lancet:wit` 段附加与 `.decl.json`/`.cwasm` 生成 |
| `build/declaration.rs` | 核对工具组件仅导入 `lancet:host`、校验顶层 `lancet:wit` 与导出签名一致性、跨组件方法与记录定义冲突校验、读取声明并推导 co-effect/effect 模板 |
| `wit/lancet-host/host.wit` | 宿主接口 `lancet:host@0.1.0` 定义（`read-dir`、`write-dir`、`exec-env`、`egress` 资源类型与 `entry` 记录） |
| `src/lib.rs` | 库入口（公开 `pub mod act`、`pub mod dispatch`、`pub mod read_dir` 与 `pub mod tools`） |
| `src/tools.rs` | 读取构建时内嵌产品工具声明与预编译产物并把内置存根与 API 工具声明合成为 `.pyi` 类型存根 |
| `src/read_dir.rs` | 只读目录宿主资源 `ReadDir`：`openat2(RESOLVE_BENEATH \| RESOLVE_NO_SYMLINKS)` 逐分量解析、符号链接边界与目标校验、`hidden` 过滤、UTF-8 与 128 MiB 读取上限检查 |
| `src/dispatch.rs` | 工具分发层 `Dispatcher`：持有授权表与 `ActContext`、κ 核对、预链接组件实例化、`ResourceTable` 句柄注入与收窄、JSON↔WIT 数值与记录编解码、128 MiB 内存与 epoch 中断控制、8 MiB 紧凑 JSON 返回窗口、`StepSummary` 记录 |
| `src/act.rs` | 预编译组件反序列化加载、读取 `builtin-stub`、`check-handle-name` 校验、链接 `lancet:act/dispatch`（`trappable`）回调、每调用独立 `Store`、128 MiB 线性内存上限、epoch 截止与取消控制 |
| `src/act/engine.rs` | 预编译与运行时共用的 `wasmtime::Config` 定义 |
| `src/main.rs` | 解析命令行参数并按启动顺序调用各模块（含 `check_handle_name`、只读目录打开与 `Dispatcher` 装配、账本恢复与 `render_act_stub`）；出错时向标准错误输出中文错误链并退出 |
| `src/config.rs` | 配置类型定义、TOML 解析、主机路径展开/规范化与引用校验 |
| `src/config/startup.rs` | 三项启动校验（工作目录互不重叠、配置与状态目录位置校验、`FICLONE` 探针） |
| `src/config/catalog.rs` | 由配置、`act` 类型存根与工具声明推出环境简述（含已注入句柄的 `modes`/`effects`）、`seam` 边集、`tools`（`[act]`）与 `catalog_digest` 的唯一构造入口 |
| `src/state.rs` | 节点状态目录创建、`flock` 独占锁、目录刷盘与 `node_id` 持久读写 |
| `src/calls.rs` | 调用记录账本：查重与持久登记、`in_flight` 取消标志、重启恢复与回执到期 |
| `src/execute.rs` | 受理请求校验（目录摘要、`act` 参数解析与 `output_limit`）、使用 `Dispatcher::context()` 与 `Dispatcher::call` 执行分发、记录回执步骤（`steps`）、`content` 组合、结局映射与回执构造 |
| `src/server.rs` | Unix 域套接字绑定、连接内多路复用分派、`hello`/`describe`/`execute`/`cancel`/`receipt` 协议处理与断连取消 |
| `tests/dispatch.rs` | 工具分发层集成测试（WIT 数值范围、陷入隔离、工具超限中断与耗时基准 `measure_tool_call_overhead`） |
| `tests/fixtures/tool-probe/` | 仅用于 `tests/dispatch.rs` 的探针工具组件（测试多参数 WIT 数值边界、触发陷入、`Spin`/高内存场景） |

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
