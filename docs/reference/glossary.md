---
status: active
---

# LANCET

LANCET 是独立工具执行运行时 `lancet-node`，对外提供 `act` 工具面。
本文件是项目唯一的领域术语表，提供简短定义、关键区别与合同入口；完整规则由对应合同维护。
术语表不批准新语义。

## 术语

**lancet-node**:
独立的工具执行运行时进程，对外提供 `act` 工具面，承接调用请求、执行边界与回执交付；它是外部上下文节点在 Linux 软件工作区上的实现。
见 [README.md](../../README.md#定位与状态)、[`lancet-node` 合同](../architecture/lancet-node/README.md)。
_Avoid_: in-process LancetRuntime, CEP host, embedded tool runner

**act**:
`lancet-node` 在 `describe` 中暴露的唯一工具，输入是一段 Starlark 脚本。脚本持有授权句柄、调用 API 工具并交回结果，自身不产生 effect；`act` 的说明是由 API 工具声明渲染的 `.pyi` 类型存根。
见 [`act` 与 Starlark 编排](../architecture/lancet-node/act.md)。
_Avoid_: multi-tool CEP catalog, raw shell tool, untyped command string

### 外部上下文节点协议

**外部上下文节点**:
独立运行的外部上下文与动作服务进程，经 `lancet-protocol` 对外暴露单一被挂载目标的读取、变更与回执查询能力，挂载到协调层。节点不感知 Agent 配置、模型、系统提示词与会话树；软件工作区、只读检索源与硬件设备都可以是节点。
见[外部上下文节点协议](../architecture/lancet-protocol.md#组件与职责)。
_Avoid_: MCP server, tool server, 插件

**`node_id`**:
节点实例跨重启稳定的持久身份（UUID v4），节点首次初始化状态目录时生成。调用方以 `node_id` 绑定挂载目标，并在 `hello` 握手时核对；人类可读的名称另用 `display_name`。
见[外部上下文节点协议](../architecture/lancet-protocol.md#连接与握手)。
_Avoid_: display_name, 节点路径, 进程 id

**`catalog_digest`**:
`describe` 返回的操作定义、环境简述与当前授权的规范摘要。调用方按轮冻结目录，每次 `execute` 带上冻结时的摘要；与节点当前的摘要不一致时，节点以 `CatalogMismatch` 拒绝。
见[外部上下文节点协议](../architecture/lancet-protocol.md#describe-与环境简述)、[`execute` 的受理与结局](../architecture/lancet-protocol.md#execute-的受理与结局)。
_Avoid_: 工具列表版本号, schema hash

**回执（`Receipt`）**:
节点为每次受理的 `execute` 持久生成的记录：调用身份、标签、最终结局、子步骤摘要与 effect 条目（每个变更项带 `residue`）。回包只附紧凑的 `receipt_summary`，完整回执经 `receipt` 查询；交回模型的 `content` 不是回执。
见[外部上下文节点协议](../architecture/lancet-protocol.md#effectco-effect-与回执)。
_Avoid_: content, 执行日志, trace

**调用记录**:
节点为每个 `invocation_id` 维护的持久记录，状态为 `Pending`、`Settled`、`Expired` 或 `Fenced`；零重放、`receipt` 查询与断连后的对账都以它为准。账本条目永久保留，回执明细按保留期清理。
见[外部上下文节点协议](../architecture/lancet-protocol.md#调用记录)。
_Avoid_: 会话记录, 调用日志

**围栏**:
`receipt` 查询节点从未受理的 `invocation_id` 时登记的持久记录（`Fenced`）。此后同 id 的 `execute` 一律以 `DuplicateInvocation` 拒绝，`NeverExecuted` 因此是确定事实。
见[外部上下文节点协议](../architecture/lancet-protocol.md#调用记录)。
_Avoid_: 锁, tombstone

**effect**:
一次调用对世界做出的、调用结束后仍可被后续调用或节点外部观察到的改变，记为 ε，种类为 `Write<h>`、`Exec<h>`、`Emit<h>`。交回调用方的 `content` 是返回值，读取是 co-effect，二者都不是 effect。
见[外部上下文节点协议](../architecture/lancet-protocol.md#effectco-effect-与回执)。
_Avoid_: 副作用（泛称）, Read effect

**co-effect**:
一次调用对上下文的依赖，记为 κ：需要持有哪些句柄、以什么模式使用，写作 `Uses<h>` 或 `Uses<h, mode>`（`mode` 为 `Read` 或 `ReadWrite`）。只读调用的 ε 为空、κ 不为空；节点受理时核对 κ 中的句柄与模式都已授予。
见[外部上下文节点协议](../architecture/lancet-protocol.md#effectco-effect-与回执)。
_Avoid_: 权限, capability

**句柄**:
节点授予的一个具名资源实例（`name`、`kind`），是 effect 与 co-effect 的参数；种类与属性由节点实现的 ADR 定义。`lancet-node` 的句柄种类是三类宿主资源：目录、执行环境、网络出口，在宿主接口 `lancet:host` 中各对应资源类型（目录按模式分为 `read-dir` 与 `write-dir`）。
见[外部上下文节点协议](../architecture/lancet-protocol.md#effectco-effect-与回执)、[宿主资源](../architecture/lancet-node/host-resources.md#概念)。
_Avoid_: 文件描述符, 路径, 凭据

**`seam`**:
跨句柄连接 effect 与 co-effect 的边，分两个方向：完整性指句柄 A 上的 `Write` 成为句柄 B 上 `Exec` 依赖的上下文；机密性指经 `Uses<A>` 读到的内容可经句柄 B 上的 `Emit` 外发。边集由节点按授权静态推出并写入环境简述，配置中不逐条声明。
见[外部上下文节点协议](../architecture/lancet-protocol.md#effectco-effect-与回执)、[环境简述与 `seam` 边集](../architecture/lancet-node/config.md#环境简述与-seam-边集)。
_Avoid_: 污点追踪, 数据流

**`residue`**:
effect 中没有逆、或逆所需材料未被保留的部分，逐变更项记录，取值为 `None`、`NotRetained`、`Regenerable`、`InternalState`、`External`。调用方与用户据此判断哪些改变还能撤回；协议不提供求逆操作。
见[外部上下文节点协议](../architecture/lancet-protocol.md#effectco-effect-与回执)、[effect 与 `residue`](../architecture/lancet-node/tools.md#effect-与-residue)。
_Avoid_: undo 记录, 回滚点

**环境简述**:
`describe` 返回的 `EnvironmentSummary`：已授予的句柄（名称、种类、effect、模式与属性）与 `seam` 边集。它由节点配置与操作声明经单一构造入口推出，只随配置、授权与节点版本变化，序列化后不超过 4 KiB，并纳入 `catalog_digest`。
见[外部上下文节点协议](../architecture/lancet-protocol.md#describe-与环境简述)、[环境简述与 `seam` 边集](../architecture/lancet-node/config.md#环境简述与-seam-边集)。
_Avoid_: workspace_overview, 文件树, 系统提示词

### `lancet-node`

**工作区**:
一个 `lancet-node` 服务的软件工作区，由一组互不重叠的工作目录组成。一个节点只对应一个工作区，多仓库协作由同一工作区内的多个工作目录表达。
见[节点配置与工作区](../architecture/lancet-node/config.md#概念)。
_Avoid_: 仓库, project, 影子工作区

**工作目录**:
模型操作的对象：工作区中的一个目录，对应一个目录句柄，有自己的名称、读写级别与路径规则（`regenerable`、`hidden`）。可写工作目录的读写落在影子工作区上。
见[节点配置与工作区](../architecture/lancet-node/config.md#概念)。
_Avoid_: cwd, 挂载点, 环境目录

**环境目录**:
执行环境挂载、但不属于工作目录的目录：只读挂载的系统目录与工具链，以及节点私有的缓存副本。只有 `proc.*` 能看到，`fs.*` 拿不到它们的句柄。
见[节点配置与工作区](../architecture/lancet-node/config.md#概念)、[挂载表](../architecture/lancet-node/host-resources.md#挂载表)。
_Avoid_: 工作目录, 宿主缓存

**API 工具**:
`lancet-node` 内部唯一的工具定义单元，是一个 WASM 组件；声明就是它导出的 WIT 接口，effect 与 co-effect 模板由句柄参数的类型推出。API 工具不暴露为协议工具，模型只经 `act` 的脚本调用它。
见 [API 工具与工具基座](../architecture/lancet-node/tools.md)。
_Avoid_: 协议工具, MCP tool, 内置函数

**宿主资源**:
原生能力进入 `lancet-node` 的唯一形式，即句柄的种类：目录、执行环境、网络出口。它是由节点实现的封闭集合，API 工具只经句柄使用。
见[宿主资源](../architecture/lancet-node/host-resources.md)。
_Avoid_: 宿主函数, 沙箱

**工具基座**:
`lancet-node` 中由 API 工具声明、分发层与配置加载组成的部分。effect、co-effect、`residue` 与 `seam` 只在这里读取并据以决定；脚本不标注、不感知它们。
见 [API 工具与工具基座](../architecture/lancet-node/tools.md#组件与职责)。
_Avoid_: 脚本层

**影子工作区**:
节点状态目录中每个可写工作目录的一份持久 reflink 副本，在执行环境中挂在真实路径上。`fs.*` 与子进程的写入都落在影子上，宿主工作目录只在发布时被写入。
见[影子工作区、待定改动与发布](../architecture/lancet-node/shadow-workspace.md)。
_Avoid_: overlay, 草稿层, sandbox

**待定改动**:
影子工作区相对基准（每个路径最近一次从宿主同步或发布后的状态）的差异。它跨 `act` 保留，直到某次 `act` 调用 `commit()` 发布或 `discard()` 丢弃；`reset(path)` 丢弃单个路径。发布前可以撤回，对应 `residue` 为 `None`。
见[待定改动](../architecture/lancet-node/shadow-workspace.md#待定改动)、[`act` 的待定改动](../architecture/lancet-node/act.md#待定改动)。
_Avoid_: 草稿, 暂存区, staged changes

**发布**:
脚本调用过 `commit()` 并正常 `return`、交回载荷不超过 `output_limit` 时，节点把全部待定改动作为一批写进宿主工作目录：先核对，再逐文件克隆到宿主目录并交换，后面的文件冲突时撤回已发布的项。发布后不保留前态，对应 `residue` 为 `NotRetained`。
见[发布](../architecture/lancet-node/shadow-workspace.md#发布)。
_Avoid_: 合并, git commit, 同步
