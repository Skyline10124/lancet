<!-- Copyright 2026 Ast Rin -->
<!-- SPDX-License-Identifier: Apache-2.0 -->

# `lancet-node` 工作指引

本 crate 是 LANCET 外部上下文节点协议在 Linux 软件工作区上的节点服务进程。

## 职责与边界

- **承担**：节点配置加载与主机路径解析、三项总是适用的启动校验与句柄名校验（`check-handle-name`）、节点状态目录管理与 `node_id` 持久化、持久调用记录账本与重启恢复（含发布事务崩溃恢复与 `writes_possible` 结算为 `Indeterminate`）、Unix 域套接字上的五个协议操作（`hello`、`describe`、`execute`、`cancel`、`receipt`）与连接内多路复用、`lancet:host` 宿主 WIT 定义、只读目录宿主资源与共享逐分量路径解析（`openat2(RESOLVE_BENEATH | RESOLVE_NO_SYMLINKS)`、`hidden`/`regenerable` 与可写目录根下 `.git` 保留）、持久 reflink 影子工作区管理（`state_dir/shadows/<key>/{tree,base}/`、宿主 inotify + 1 秒后台 stat 扫描同步、影子 inotify 增量待定集合、冲突重算、`status`/`diff`/`write`/`reset`/`discard` 与 `commit()` 五阶段发布事务、晚到冲突/I/O 错误逆序撤回、保留宿主修改与启动期崩溃恢复）、API 工具组件声明/冲突核对与模板推导、`.pyi` 类型存根合成、节点侧工具分发（κ 核对、隔离 `Store` 实例化、只读/可写目录槽位注入与 `read-dir` 收窄、WIT 数值转换、128 MiB 内存与 epoch 中断控制、8 MiB 紧凑 JSON 返回窗口、可写目录互斥锁、内置 `discard()` 与 `commit()`、ε 记录与越出模板检查、84 字节待定提示预留、产品工具 `fs.read`/`fs.list`/`fs.search`/`fs.write`/`fs.edit`/`fs.reset`/`fs.status`/`fs.diff` 与 `StepSummary` 记录），以及 `lancet-act` 组件构建预编译、`builtin-stub` 读取与 `ActRuntime` 隔离执行。
- **尚未实现**：私有 Git（排在 #8 的执行环境之后），以及 `exec-env`/`egress` 句柄注入、执行环境与网络出口。

## 阅读顺序

开始修改本 crate 前，先读：

1. [根 Agent 工作指引](../../AGENTS.md)
2. [节点配置与工作区合同](../../docs/architecture/lancet-node/config.md)
3. [影子工作区合同](../../docs/architecture/lancet-node/shadow-workspace.md)
4. [组件运行时合同](../../docs/architecture/lancet-node/tools.md)
5. [`act` 解释器合同](../../docs/architecture/lancet-node/act.md)
6. [外部上下文节点协议合同](../../docs/architecture/lancet-protocol.md)
7. [工程规范](../../docs/development/engineering.md)

## 模块结构

| 模块 | 职责 |
| --- | --- |
| `build.rs` | 触发 `lancet-act` 内层 `wasm32-wasip2` 发布构建、`wasm-tools compose` WASI 适配器组合、`act.cwasm` 预编译、八个产品工具组件（`fs-read`、`fs-list`、`fs-search`、`fs-write`、`fs-edit`、`fs-reset`、`fs-status`、`fs-diff`）跨组件冲突检查与打包（`product_tools.rs`），以及仅测试/基准使用的 `lancet-tool-probe` 打包 |
| `build/tool.rs` | 工具组件内层 `wasm32-unknown-unknown` 构建、`wasm-tools component new` 封装、`lancet:wit` 段附加与 `.decl.json`/`.cwasm` 生成 |
| `build/declaration.rs` | 核对工具组件仅导入 `lancet:host`、校验顶层 `lancet:wit` 与导出签名一致性、跨组件方法与记录定义冲突校验、读取声明并推导 co-effect/effect 模板 |
| `wit/lancet-host/host.wit` | 宿主接口 `lancet:host@0.1.0` 定义（`read-dir`、`write-dir`、`exec-env`、`egress` 资源类型与 `entry`、`pending-change`、`path-diff` 记录） |
| `src/lib.rs` | 库入口（公开 `pub mod act`、`pub mod dispatch`、`pub mod read_dir`、`pub mod shadow` 与 `pub mod tools`） |
| `src/tools.rs` | 读取构建时内嵌产品工具声明与预编译产物并把内置存根与 API 工具声明合成为 `.pyi` 类型存根 |
| `src/read_dir.rs` | 共享逐分量路径解析与规则集（`PathRules`、`DirRoot`）及只读目录宿主资源 `ReadDir`：`openat2(RESOLVE_BENEATH \| RESOLVE_NO_SYMLINKS)`、符号链接边界与目标校验、`hidden`/`regenerable` 与可写目录根 `.git` 保留、原子写入、UTF-8 与 128 MiB 读取上限 |
| `src/shadow.rs` | 持久影子工作区 `ShadowDir` 与只读视图 `ShadowReadView`：首次 8 线程并行 `FICLONE` 建树与重启复用、宿主同步（先影子后基准）、影子 inotify 增量待定集合、冲突重算、`status`/`diff`/`write`/`reset`/`discard` 与可写目录互斥锁 |
| `src/shadow/model.rs` | 影子条目快照、按存在性/类型/权限/内容/链接目标的逐路径独立比较（见[影子工作区合同](../../docs/architecture/lancet-node/shadow-workspace.md#待定改动)）、原子 `FICLONE`/符号链接/目录安装、`tmp/` 与 `widened` 崩溃恢复及 `similar` unified diff 生成 |
| `src/shadow/publish.rs` | `CallRecord`/`PublishJournal` 持久结构唯一真源、发布前冲突检查（`hidden`/多链接/宿主已改/非空目录）与 16 KiB 截断排版、临界区外 `publish/<hash>/{stage,pre,base}` 宿主与基准后态预暂存、五阶段发布执行、晚到冲突/I/O 错误逆序撤回（保留宿主修改）、基准改名推进与启动期幂等崩溃恢复 |
| `src/shadow/watch.rs` | 非阻塞 Linux `inotify` 目录递归监听、移动配对、队列溢出与 `ENOSPC` 处理与重建 |
| `src/shadow/scanner.rs` | 宿主侧 1 秒间隔后台 stat 扫描线程（stat 变化或 racy 窗口才经 `FICLONE` 到 `O_TMPFILE` 求摘要） |
| `src/dispatch.rs` | 工具分发层 `Dispatcher`：持有 `DirSlot`（`ReadOnly` / `Shadow`）与 `ActContext`、κ 核对、预链接组件实例化、`ResourceTable` 句柄注入与 `ReadDirResource` 收窄、JSON↔WIT 编解码、128 MiB 内存与 epoch 中断控制、8 MiB 紧凑 JSON 返回窗口、可写锁获取与宿主同步、内置 `discard()`/`commit()`、`StepSummary` 记录与统一 ε 归集/越出模板检查 |
| `src/act.rs` | 预编译组件反序列化加载、读取 `builtin-stub`、`check-handle-name` 校验、`ActHost` 回调接口链接 `lancet:act/dispatch`（`call`、`discard` 与 `commit` 均 `trappable`）、每调用独立 `Store`、128 MiB 线性内存上限、epoch 截止与取消控制 |
| `src/act/engine.rs` | 预编译与运行时共用的 `wasmtime::Config` 定义 |
| `src/main.rs` | 解析命令行参数并按启动顺序调用各模块（配置加载与句柄名校验、路径与 `FICLONE` 校验、状态目录打开、可写恢复根与发布事务恢复、`ReadDir`/`ShadowDir` 打开与 `Dispatcher` 装配、账本恢复与 `serve`） |
| `src/config.rs` | 配置类型定义、TOML 解析、主机路径展开/规范化与引用校验 |
| `src/config/startup.rs` | 三项启动校验（工作目录互不重叠、配置与状态目录位置校验、`FICLONE` 探针） |
| `src/config/catalog.rs` | 由配置、`act` 类型存根与工具声明推出环境简述（含 `ReadDir` 与 `WriteDir` 句柄的 `modes`/`effects`）、`seam` 边集、`tools`（`[act]`）与 `catalog_digest` 的唯一构造入口 |
| `src/state.rs` | 节点状态目录创建、`flock` 独占锁、目录刷盘与 `node_id` 持久读写 |
| `src/calls.rs` | 调用记录账本：查重与持久登记（含 `writes_possible` 与 `PublishJournal`）、`in_flight` 取消标志与发布临界区准入闸门、启动期发布事务恢复与未开始调用结算及回执到期 |
| `src/execute.rs` | 受理请求校验（目录摘要、`act` 参数解析与 `output_limit` 含可写 84 字节下限）、取锁与宿主同步、`Dispatcher` 执行与 `discard()`/`commit()`、84 字节预留核对与待定提示附加、发布准备与不可中断临界区执行、`steps`/`effects` 归集、结局映射、结算后放锁再写回套接字 |
| `src/server.rs` | Unix 域套接字绑定、连接内多路复用分派、`hello`/`describe`/`execute`/`cancel`/`receipt` 协议处理与断连取消 |
| `tests/shadow.rs` | 影子工作区库层集成测试（重启复用不重播种子、实验 3 全部 63 项继承用例、5 种子×两侧×20 批含偶批丢弃/奇批发布的随机对照、45 项未通知宿主同步、真实 `IN_Q_OVERFLOW` 队列溢出恢复、全路径类型发布与基准推进测试） |
| `tests/dispatch.rs` | 工具分发层集成测试（WIT 数值范围、陷入隔离、工具超限中断、可写槽位收窄、写后失败保留回执、effect 越出模板与耗时基准 `measure_tool_call_overhead`） |
| `tests/node.rs` | 节点进程黑盒测试入口（`tests/node/calls.rs`、`tests/node/writable.rs` 与 `tests/node/publish.rs`，含 100k 文件空 `act` 基准 `measure_empty_act_100k` 与发布时延基准 `measure_publish_latency`） |
| `tests/fixtures/tool-probe/` | 仅用于 `tests/dispatch.rs` 的探针工具组件（测试多参数 WIT 数值边界、资源误用陷入、写后报错、双句柄越界写、`Spin`/高内存场景） |

## 验证命令

```sh
cargo test -p lancet-node
cargo test -p lancet-node --test tool_declarations
cargo test -p lancet-node --test act
cargo test -p lancet-node --test shadow
cargo test -p lancet-node --test dispatch
cargo test -p lancet-node --test node
cargo test --release -p lancet-node --test act measure_component_runtime -- --ignored --nocapture --test-threads=1
cargo test --release -p lancet-node --test dispatch measure_tool_call_overhead -- --ignored --nocapture --test-threads=1
cargo test --release -p lancet-node --test node measure_empty_act_100k -- --ignored --nocapture --test-threads=1
cargo test --release -p lancet-node --test node measure_publish_latency -- --ignored --nocapture --test-threads=1
cargo xtask ci
```
