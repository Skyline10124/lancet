---
status: active
---

# API 工具与工具基座

API 工具是 `lancet-node` 内部唯一的工具定义单元：每个工具是一个 WASM 组件，声明就是它导出的 WIT 接口。API 工具不暴露为协议层的工具，模型只经 `act` 的脚本调用它们。工具基座从声明推出 effect 与 co-effect 模板、`act` 的类型存根与环境简述的句柄条目；每次调用时，它只把声明允许的句柄交给工具，并核对实际发生的 effect 不越出模板。

## 概念

| 概念 | 含义 | 状态持有者 | 生命周期 |
| --- | --- | --- | --- |
| API 工具 | 一个导出 WIT 接口、只导入宿主接口 `lancet:host` 的 WASM 组件 | 工具组件二进制 | 随节点版本 |
| 工具声明 | 组件导出接口的函数签名，以及组件顶层自定义段 `lancet:wit` 中带注释的 WIT 包 | 工具组件二进制 | 随组件 |
| 宿主接口 `lancet:host` | 句柄的资源类型 `read-dir`、`write-dir`、`exec-env`、`egress` 及其方法 | `lancet-node` 源码中的 WIT 包 | 随节点版本 |
| effect 与 co-effect 模板 | 由句柄参数的类型推出的 ε 与 κ | 声明加载的结果 | 随声明 |
| 预编译产物 | 组件经 `wasmtime` 预编译的结果，与读出的声明一同保存 | 节点 | 随组件与节点版本 |
| 工具实例 | 一次 API 工具调用实例化的组件 | 分发层 | 一次调用 |

## 组件与职责

| 组件 | 持有的状态 | 执行的规则 | 对外接口 |
| --- | --- | --- | --- |
| 声明加载 | 读出的声明、推出的模板与预编译产物 | [声明加载](#声明加载)：核对声明、推出模板、预编译；渲染 `act` 的类型存根与 Starlark 类型 | 节点内部 |
| 分发层 | 本次 `act` 已记录的 ε | [分发](#分发)：核对 κ、实例化工具并注入句柄、记录并核对 ε | 解释器组件导入的子工具分发接口 |
| 组件运行时 | 复用的 `wasmtime` 引擎 | [组件运行时](#组件运行时)：只加载预编译产物，为每次组件调用设置内存上限与截止中断 | 节点内部 |
| 配置加载 | 环境简述与 `seam` 边集 | 由模板推出句柄条目与边集，见[节点配置](config.md#环境简述与-seam-边集) | 节点内部 |

## 工具声明

- 参数与返回类型签名是工具组件导出的 WIT 接口。工具组件只导入宿主接口 `lancet:host`；句柄的种类与模式各是其中的一种资源类型：`read-dir`、`write-dir`、`exec-env`、`egress`。资源类型的方法由 `lancet-node` 源码中的 `lancet:host` WIT 包定义，见[宿主接口](#宿主接口)。
- effect 与 co-effect 模板由句柄参数的类型推出（见下表），工具不另行声明。授权的单位是节点配置的句柄；更窄的范围靠配置更窄的句柄表达。
- 工具说明取自组件顶层自定义段 `lancet:wit` 中带注释的 WIT 包。构建出的组件只含类型、不含 WIT 注释，说明只经这个段带入。WIT 不带参数注释，参数的说明写在函数注释中。
- 第三方工具可以用任何能编译为 WASM 组件的语言编写。节点以同一方式对待全部工具：不信任工具的实现，只按声明分发句柄。
- 本仓库的 API 工具以 `wasm32-unknown-unknown` 构建，经 `wasm-tools component new` 转为组件，再追加 `lancet:wit` 段。
- 每个工具是导出接口中的一个函数。脚本中的工具名为接口名加函数名，例如 `lancet:fs-read/fs@0.1.0` 的 `read` 为 `fs.read`；WIT 名称中的连字符在脚本中写作下划线，例如 `missing-ok` 写作 `missing_ok`。
- 工具的首个 `borrow` 句柄参数是接收者，工具绑定为接收者类型的方法（[`act` 的说明与全局环境](act.md#act-的说明与全局环境)）。每个工具至少有一个句柄参数；句柄只作为顶层参数出现，且只能是 `borrow`。
- 参数可用的数据类型为 `bool`、`string`、整数、浮点数、`option` 与 `list`；记录字段与返回值另可用具名 `record`。`option` 不直接嵌套 `option`；类型别名按它指向的类型处理。返回类型为 `result` 时，错误类型必须是 `string`。

| 句柄参数的类型 | co-effect 模板 | effect 模板 |
| --- | --- | --- |
| `borrow<read-dir>` | `Uses<d, Read>` | 无 |
| `borrow<write-dir>` | `Uses<d, ReadWrite>` | `Write<d>` |
| `borrow<exec-env>` | `Uses<e>` 与 `mounts(e)` | `Exec<e>`，以及派生的 `Write<d>`（`d` 是挂进 `e` 的可写目录）与 `Emit<n>`（`e` 持有出口） |
| `borrow<egress>` | `Uses<n>` | `Emit<n>` |

`d`、`e`、`n` 为该参数；`mounts(e)` 为 `e` 挂载的目录及其读写级别。

### 宿主接口

宿主接口的包为 `lancet:host@0.1.0`，四种资源类型都在接口 `handles` 中。

| 资源类型 | 方法 |
| --- | --- |
| `read-dir` | `read(path) -> result<option<string>, string>` 读取 UTF-8 文本文件；`entries(path) -> result<option<list<entry>>, string>` 列出目录中可见的直接子项，按名称的 UTF-8 字节序排列，`entry` 为 `{ name: string, is-dir: bool, is-link: bool }`：`is-link` 表示子项本身是符号链接，`is-dir` 取链接目标的类型。目标不存在（`ENOENT`）且没有命中 `hidden` 时，两者都返回 `none`。路径、符号链接与 `hidden` 的处理见[目录](host-resources.md#目录) |
| `write-dir` | `read(path)` 同 `read-dir`；`write(path, content) -> result<_, string>` 整文件写入，缺少的上级目录一并创建；`reset(path) -> result<_, string>` 丢弃该路径的待定改动。读写都作用于影子，见[影子工作区](shadow-workspace.md) |
| `exec-env`、`egress` | 只有类型，用于推出模板；方法见[尚未承接](#尚未承接) |

`read-dir` 另有 `status() -> result<list<pending-change>, string>` 与 `diff() -> result<list<path-diff>, string>`，列出影子的待定改动；只读工作目录没有影子，二者返回空列表。`pending-change` 为 `{ path: string, action: string, conflict: bool }`，`path-diff` 另带 `patch: option<string>`。

## 声明加载

节点在预编译之前，从原组件读出并核对声明：

```text
加载工具组件:
    if 组件导入了 lancet:host 以外的接口:          拒绝加载
    elif 组件顶层缺少自定义段 lancet:wit:          拒绝加载   # 只在嵌套模块中的同名段不算
    elif 导入与节点的 lancet:host 不兼容:          拒绝加载
    elif lancet:wit 中的签名与组件导出接口不同:    拒绝加载   # 注释不参与比较
    elif 有工具不符合工具声明的形状:                拒绝加载   # 没有句柄参数，或用了不支持的类型
    else:
        读出声明，由句柄参数的类型推出 effect 与 co-effect 模板
        预编译组件，把声明与预编译产物成对保存         # 预编译产物不含自定义段
```

- 本仓库的工具在节点构建时加载：构建脚本打包组件、核对声明，以与运行时相同的引擎配置预编译，把每个工具的声明与预编译产物成对写入构建输出。拒绝加载即构建失败，错误写明组件名与原因。
- 构建脚本按产品清单组装节点发布的工具集合，并核对两项：同一句柄类型上可绑定的方法不重名，含 `ReadDir` 的方法复制到 `WriteDir` 之后；同名记录在各工具中定义一致。不满足即构建失败，错误写明冲突的双方。
- 工具可以只导入它用到的资源与方法；导入是否兼容，由 `wit-component` 按节点的 `lancet:host` 核对。
- 签名核对分两步：先把两边的导出接口打印成不含注释的 WIT 文本后比较；再由 `wit-component` 按 `lancet:wit` 中的 world 核对组件，覆盖经 `use` 引入的宿主类型。`lancet:wit` 中的 WIT 包恰好含一个 world。
- 工具声明的形状见[工具声明](#工具声明)。
- 节点用 `wit-component` 与 `wasmparser` 读取声明。
- `act` 的 `.pyi` 类型存根、Starlark 静态检查所用的类型、环境简述与 `catalog_digest` 都由节点从声明推出，不另行手写；类型存根与静态检查见[`act` 的说明与全局环境](act.md#act-的说明与全局环境)。

## 分发

解释器组件只导入子工具分发接口。脚本每调用一次 API 工具，分发层执行：

```text
分发一次 API 工具调用:
    用实参中的句柄实例化模板中的 κ
    if κ 中有未授予的句柄或模式:
        该次调用报错
    实例化工具组件，只注入本次调用的句柄，句柄按参数类型收窄
    在内存上限与截止中断下运行该实例
    把实际发生的 ε 记入本次 act 的回执
    if ε 越出模板:
        节点缺陷，本次 act 以 Failed 结束
    把结果交回脚本，受单次返回窗口上限约束
```

- 解释器经分发接口提交调用：工具的 WIT 导出名，以及按形参顺序排列的实参。句柄实参只传槽位编号，其余实参以 JSON 传递。分发层交回成功值的 JSON，或交回错误消息，使脚本在调用处中止。
- 工具实例只得到本次调用注入的句柄。参数为 `borrow<read-dir>` 的工具只拿到 `d` 的只读视图；没有交给它的资源，它无从引用。
- `wasmtime` 强制声明中的类型：实例化时按节点的链接器核对组件的导入；每次调用时核对实参的类型、个数与资源种类；组件内部把一种句柄当作另一种使用时陷入，该次调用报错。
- 数值实参按 WIT 位宽核对：整数超出目标类型的取值范围，或 `f32` 实参不是有限值、绝对值超过 `f32::MAX` 时，该次调用报错。
- 工具实例陷入或超出内存上限时，该次调用报错，脚本在调用处中止。工具运行期间 `remaining_ms` 耗尽或收到 `cancel` 时，节点中断该实例，本次 `act` 结算为 `Deadline` 或 `Cancelled`（[`act` 的结局](act.md#结局)）。
- **单次返回窗口：** 一次 API 工具交回脚本的成功值，按紧凑 UTF-8 JSON 的字节数计，上限为 8 MiB（8,388,608 字节），等于上限时成功。超出时该次调用报错，错误提示改用 `proc.pipe` 过滤后再返回。窗口容纳未过滤的测试日志或响应体，供脚本就地筛选。
- 每次到达分发层的调用在回执的 `steps` 中记一条 `{ tool, outcome }`：`tool` 为脚本中的工具名（如 `fs.read`），`outcome` 为该次调用的结局；κ 不满足、实参无效或超出返回窗口的调用记为 `Failed`。解析、静态检查与求值中没有发出分发的错误不记步骤。
- 形参为 `borrow<read-dir>`、实参是可写工作目录时，分发层只注入影子的只读视图；只有 `borrow<write-dir>` 形参拿到可写资源。
- 宿主方法报告实际发生的变更，分发层把它们记为本次 `act` 回执中的 effect 条目：影子中常规路径的变更为 `Write`、`residue` 为 `None`；`regenerable` 路径中的写入为 `Write`、`residue` 为 `Regenerable`，变更项只记命中的规则。工具报错、陷入或被中断时，已发生的变更照样记入。每次调用结束时，分发层核对这些变更都在以实参实例化的 effect 模板之内；越出时本次 `act` 以 `Failed(InternalError)` 结束，已发生的变更保留在回执中。
- 内置函数 `discard()` 经同一分发接口交给节点（[`act` 的待定改动](act.md#待定改动)），在回执的 `steps` 中记为 `discard`。

## 组件运行时

API 工具与解释器都是 WASM 组件，在 `wasmtime` 中运行；隔离与资源上限由组件边界承担。

- 节点只加载预编译产物；每次调用现编译的开销在秒级。
- 节点为每次组件调用设置内存上限，并在截止时间到达时中断该次调用。超限或超时只终止这一个实例，节点继续服务。
- 解释器组件在构建时预编译，产物内嵌在 `lancet-node` 中；节点启动时由 `ActRuntime::load()` 反序列化，之后的调用复用同一个 `Engine`。每次调用新建 `Store` 与实例。
- 解释器调用的 `Store` 中，全部线性内存合计上限为 128 MiB（134,217,728 字节），含初始分配；超出时该次调用立即失败。
- 每次 API 工具调用新建 `Store` 与工具实例，与解释器共用同一个 `Engine` 与计时线程。工具实例的 `Store` 中，全部线性内存合计上限同为 128 MiB（134,217,728 字节），含初始分配；截止时间与取消标志沿用所属 `act` 的。
- 计时线程按 10 ms 周期推进 epoch。实例运行到随后的 epoch 检查点时，读取本次调用的截止时间与取消标志，决定是否中断。
- 实测（2026-11-18，AMD EPYC 9B14，Linux 6.12，`wasmtime` 49.0.5，release 构建）：每次解释器调用（新建 `Store`、实例化并运行 `return 1`，100 次）p50 3.77 ms、p95 3.86 ms；取消从置位到返回（30 次）p50 10.09 ms、最大 10.15 ms；截止超出预算（30 次）最大 7.31 ms。同一运行时上，持有 62 个约 1 MiB 字符串的脚本正常返回，63 个时得到 `MemoryLimit`，随后的调用正常返回。
- 实测（2026-11-19，AMD EPYC 9B14，Linux 6.12，`wasmtime` 49.0.5，release 构建）：复用 `Engine` 与预链接结果时，在空目录上完整执行一次 API 工具调用（核对 κ、新建 `Store`、实例化、调用并释放，各 1,000 次）：`fs.read` p50 42.80 µs、p95 54.03 µs；`fs.list` p50 42.46 µs、p95 53.08 µs；`fs.search` p50 70.04 µs、p95 88.66 µs。
- 解释器组件的中断时机与它自带的资源限制见[解释器](act.md#解释器)。

## 首批 API 工具

参数与返回类型以各工具组件导出的 WIT 接口为准。`d` 为目录句柄，`e` 为执行环境句柄，`n` 为网络出口句柄；模板按[推导表](#工具声明)由参数类型推出。

| API 工具 | effect 模板 | co-effect 模板 | 行为 |
| --- | --- | --- | --- |
| `fs.read`、`fs.list` | 无 | `Uses<d, Read>` | 读取文件、列举目录 |
| `fs.search` | 无 | `Uses<d, Read>` | 按正则与路径模式搜索 |
| `fs.status`、`fs.diff` | 无 | `Uses<d, Read>` | [待定改动](shadow-workspace.md#待定改动)的增删改列表（含冲突路径）与 unified diff |
| `fs.write` | `Write<d>` | `Uses<d, ReadWrite>` | 整文件写入 |
| `fs.edit` | `Write<d>` | `Uses<d, ReadWrite>` | 精确匹配替换 |
| `fs.reset` | `Write<d>` | `Uses<d, ReadWrite>` | 丢弃指定路径的待定改动 |
| `proc.run` | `Exec<e>`，以及派生的 `Write<d>` 与 `Emit<n>` | `Uses<e>` 与 `mounts(e)` | 以参数数组运行单个进程，不经过 `/bin/sh` |
| `proc.pipe` | 同上 | 同上 | 用内核 `pipe()` 串联多阶段参数数组，不经过 `/bin/sh` |
| `http.fetch` | `Emit<n>` | `Uses<n>` | 经授权的网络出口发起 HTTP 请求 |

工具的报错规则：任何错误默认让脚本中止（[快速失败](act.md#快速失败)），只在预期的探测点提供显式的窄参数。

| API 工具 | 默认 | 显式参数 | 始终报错 |
| --- | --- | --- | --- |
| `fs.read`、`fs.list` | 目标不存在时报错 | `missing_ok = True` 时，目标不存在（`ENOENT`）返回 `None` | 命中 `hidden` 规则、越界路径、编码错误 |
| `fs.search` | — | — | 无效的正则或 `glob`；读取候选文件失败，含文件不是 UTF-8 |
| `fs.edit(d, path, old, new, all = False)` | `old` 在文件中恰好出现 1 次，否则报错 | `all = True` 时替换全部出现位置，至少出现 1 次，否则报错 | — |
| `proc.run`、`proc.pipe` | `check = True`：子进程非零退出时报错，附尾部输出摘要 | `check = False` 时把 `{ code, stdout, stderr }` 作为普通数据交回 | — |
| `http.fetch` | HTTP 4xx/5xx 状态码作为响应对象的字段交回 | — | DNS 失败、连接超时、不在出口清单内的请求 |

首批只读工具的脚本签名与行为，`d` 为 `ReadDir`：

| 方法 | 返回 | 行为 |
| --- | --- | --- |
| `d.read(path, missing_ok = None)` | `str` 或 `None` | 读取 UTF-8 文本文件 |
| `d.list(path, missing_ok = None)` | `Entry` 的列表，或 `None` | 宿主 `entries` 的结果：目录中可见的直接子项；空目录为空列表 |
| `d.search(pattern, glob = None)` | `SearchHit` 的列表 | 见下 |

`d.search`：

- 自句柄根递归遍历可见的文件，不进入符号链接指向的目录；指向文件的链接照常参与。
- `glob` 匹配文件的根相对路径：区分大小写，`*` 不跨 `/`，`**` 可跨目录。省略时检查全部文件。
- 按正则逐行匹配，每个匹配行记一条 `{ path, line_no, text }`：行号从 1 开始，`text` 为去掉行结束符的整行。
- 结果按 `path` 的字节序、再按 `line_no` 升序排列；没有命中时为空列表。

写入与待定改动工具的脚本签名与行为，`d` 为 `WriteDir`；`status()`、`diff()` 也是 `ReadDir` 的方法：

| 方法 | 返回 | 行为 |
| --- | --- | --- |
| `d.write(path, content)` | `None` | 整文件写入 UTF-8 文本；缺少的上级目录一并创建，替换已有普通文件时保持其权限。内容与原文件相同时不发生写入 |
| `d.edit(path, old, new, all = None)` | `None` | 按字面、自左向右、不重叠地匹配 `old`，次数规则见上方的报错表；核对通过后才写入 |
| `d.reset(path)` | `None` | 丢弃该路径的待定改动；冲突路径改为接受当前的宿主版本。没有待定改动时不做任何事；会连带丢弃其他路径的待定改动时报错 |
| `d.status()` | `PendingChange` 的列表 | 可见的待定路径，按路径的字节序排列；`action` 为 `added`、`modified` 或 `deleted`（类型与权限的变化算 `modified`），`conflict` 标出冲突 |
| `d.diff()` | `PathDiff` 的列表 | 与 `status()` 同一集合，另带基准到影子的 unified diff，上下文 3 行；目录、链接、纯权限变化、非文本文件，以及任一侧超过 128 MiB 的文件，`patch` 为 `None` |

- API 工具保持单一职责，交回原始的领域结构，不在各个工具上定义文本裁剪参数；筛选由脚本的文本视图完成（[全局环境](act.md#act-的说明与全局环境)）。
- 所有宿主返回类型与视图类型都可以直接序列化为 JSON。
- 超出单次返回窗口的流式进程输出，用 `proc.pipe` 在内核管道中过滤。

## effect 与 `residue`

声明中的每一项都有读取方，并据以作决定；没有读取方的项不进声明。

| 概念 | 读取方 | 据此作的决定 |
| --- | --- | --- |
| effect 与 co-effect 模板 | 分发层 | 用本次调用实际传入的句柄实例化 κ，核对其中句柄与模式均已授予；核对回执记录的 ε 不越出模板 |
| 同上 | 配置加载 | 推出 `seam` 边集与环境简述的句柄条目 |
| `residue` | 协调层与用户 | 哪些改动还能撤回，如实报告不可撤回的项 |
| `seam` 边集 | 调用方与用户，经环境简述 | 见[环境简述与 `seam` 边集](config.md#环境简述与-seam-边集) |

| effect 项 | `residue` |
| --- | --- |
| 影子中的待定改动（常规路径，以及 `.git/` 的引用与 index） | `None`：发布前可经 `discard()` 或 `reset(path)` 撤回 |
| 经 `commit()` 发布到宿主工作目录的改动 | `NotRetained`：发布后不保留前态 |
| 匹配 `regenerable` 的写入（只在影子里） | `Regenerable` |
| 私有家目录与包缓存中的写入 | `InternalState` |
| `http.fetch` 或子进程经节点代理发出的网络请求 | `External`，记录目标主机 |

- 只读 API 工具没有 effect。
- 待定改动在写入它的 `act` 的回执中记为 `Write` 条目（`None`）；发布它的 `act` 的回执再记一次发布条目（`NotRetained`）。
- `discard()` 与 `reset(path)` 也是对影子的写入，记入所在 `act` 的回执。

## 保证

| 保证 | 适用范围 | 成立条件 | 依据或验证 |
| --- | --- | --- | --- |
| 工具只能触及本次调用注入的句柄，并且只能按参数类型使用它们 | 每次 API 工具调用 | 链接器只定义 `lancet:host`；`wasmtime` 在实例化与调用时核对类型 | [ADR 0002 第 2 节](../../adr/0002-lancet-node-api-tools-and-act.md#2-唯一的定义单元是-api-工具对外唯一工具是-act)、[第 4 节](../../adr/0002-lancet-node-api-tools-and-act.md#4-边界由宿主资源承担)；实验 4（[已核实的事实](../../adr/0002-lancet-node-api-tools-and-act.md#已核实的事实)）；`crates/lancet-node/src/dispatch.rs::tests::check_coeffects_enforces_grants_and_modes`；`crates/lancet-node/tests/dispatch.rs::tool_trap_fails_only_that_call`、`dispatch_checks_wit_numeric_ranges` |
| 模板、类型存根、环境简述与 `catalog_digest` 都与工具声明一致 | 每个加载的工具 | 都由声明推出；签名对不上的组件拒绝加载 | ADR 0002 第 2 节；实验 4；`crates/lancet-node/tests/tool_declarations.rs::packaged_read_keeps_docs_and_signature`、`derives_templates_for_all_handle_parameters`、`product_read_tools_match_templates`；`crates/lancet-node/tests/node.rs::hello_describe_matches_config`（环境简述的 `modes` 与类型存根；`catalog_digest` 由二者与授权计算） |
| 导入了 `lancet:host` 以外的接口、与节点的 `lancet:host` 不兼容、缺少顶层 `lancet:wit` 段、签名对不上或不符合声明形状的组件不被加载 | 每个工具组件 | 声明加载在预编译之前核对；拒绝即构建失败 | `crates/lancet-node/tests/tool_declarations.rs::rejects_foreign_and_incompatible_host_imports`、`rejects_missing_wit_section`、`rejects_mismatched_documented_exports`、`rejects_tool_without_handle_receiver`、`maps_type_aliases_and_rejects_unrepresentable_types` |
| 回执记录的 ε 不越出模板；越出时本次 `act` 以 `Failed` 结束 | 每次 API 工具调用 | 分发层逐次核对 | [ADR 0002 第 7 节](../../adr/0002-lancet-node-api-tools-and-act.md#7-effectco-effectresidue-与-seam-在工具基座中的读取方)；`crates/lancet-node/tests/dispatch.rs::effect_outside_template_fails_act`、`write_receipt_survives_tool_failure` |
| 交回脚本的单次工具结果不超过 8 MiB；超出时该次调用报错 | 每次 API 工具调用 | 分发层按紧凑 JSON 的字节数核对 | `crates/lancet-node/tests/node/calls.rs::tool_result_window_uses_compact_json_bytes` |
| 节点发布的工具集合中，同一句柄类型上的方法不重名，同名记录定义一致 | 节点发布的工具集合 | 构建时核对，不满足即构建失败 | `crates/lancet-node/tests/tool_declarations.rs::rejects_method_and_record_conflicts_across_components` |
| 组件超出内存上限或截止时间时只终止该实例，节点继续服务 | 每次组件调用 | `wasmtime` 的 `ResourceLimiter` 与 epoch 中断 | ADR 0002 第 4 节；`crates/lancet-node/tests/act.rs::memory_limit_terminates_only_its_store`、`deadline_interrupts_busy_store`、`cancel_interrupts_only_selected_store`（解释器组件）；`crates/lancet-node/tests/dispatch.rs::tool_limits_interrupt_only_that_call`、`tool_trap_fails_only_that_call`（API 工具组件） |

## 范围之外

| 职责 | 现状 |
| --- | --- |
| 第三方工具的安装、分发与信任配置 | 推后；第一个版本只发布首批工具 |
| 审批 | 推后，见 [`lancet-node` 尚未承接](README.md#尚未承接) |

## 尚未承接

| 事项 | 现状 |
| --- | --- |
| `exec-env`、`egress` 的方法 | 由 [#8](https://github.com/Skyline10124/lancet/issues/8)、[#9](https://github.com/Skyline10124/lancet/issues/9) 定义 |

## 验证

- `cargo xtask docs-check`：本合同的链接与锚点有效。
- `cargo test -p lancet-node --test act`：解释器组件的内存上限、截止与取消。
- `cargo test --release -p lancet-node --test act measure_component_runtime -- --ignored --nocapture --test-threads=1`：实测解释器每次调用的开销与中断时延，结果见[组件运行时](#组件运行时)。
- `cargo test -p lancet-node --test tool_declarations`：声明加载的拒绝情形、签名核对、模板推导、类型存根渲染，以及产品工具集合的模板与冲突检查。拒绝情形包括[实验 4](https://github.com/Skyline10124/lancet/blob/6dd0416d15ff4b49e95ec00c787643c8c590e376/prototypes/tool-declaration/REPORT.md) 中仍适用的几种：导入 WASI 接口、缺少 `lancet:wit` 段、段只在嵌套核心模块中。
- `cargo test -p lancet-node --lib dispatch`：κ 的核对。
- `cargo test -p lancet-node --test dispatch`：数值实参的范围、API 工具组件的陷入、内存上限、截止与取消；可写目录的只读视图收窄、工具报错后保留的写入，以及 effect 越出模板时的结局。
- `cargo test --release -p lancet-node --test dispatch measure_tool_call_overhead -- --ignored --nocapture --test-threads=1`：实测 API 工具单次调用的固定开销，结果见[组件运行时](#组件运行时)。
- `cargo test -p lancet-node --test node`：首批 API 工具经 `execute` 的行为、回执的 `steps` 与 `Write` 条目、返回窗口与超帧回执的截断。
