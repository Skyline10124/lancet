---
status: active
---

# 节点配置与工作区

节点配置由机器所有者维护，规定一个 `lancet-node` 服务的工作区、授予的句柄与路径规则。节点启动时校验配置与主机，再由配置与 API 工具声明推出环境简述、`seam` 边集与 `catalog_digest`。

## 概念

| 概念 | 含义 | 状态持有者 | 生命周期 |
| --- | --- | --- | --- |
| 节点配置 | 工作区组成、授权、路径规则与网络出口清单 | 节点配置文件 | 由机器所有者修改；节点启动时读取 |
| 节点状态目录 | 节点的持久状态，内容见[节点状态目录](#节点状态目录) | 主机文件系统 | 节点首次启动时初始化，跨重启保留 |
| 工作区 | 一组互不重叠的工作目录 | 节点配置 | 随配置 |
| 工作目录 | 模型操作的对象：一个目录，有自己的名称、读写级别与路径规则，对应一个目录句柄 | 节点配置 | 随配置 |
| 环境目录 | 执行环境挂载、但不属于工作目录的目录：只读挂载的系统目录与工具链，以及节点私有的缓存副本 | 节点配置与节点状态目录 | 随配置；私有缓存首次从宿主缓存 reflink 复制 |
| 路径规则 | 工作目录内按模式匹配的 `regenerable` 与 `hidden` 规则 | 节点配置 | 随配置 |

- 一个节点只对应一个工作区。多仓库协作由同一工作区内的多个工作目录表达。
- 只有 `proc.*` 能看到环境目录；`fs.*` 拿不到它们的句柄。

## 配置文件

授权、路径规则与网络出口清单只写在节点配置文件里，由机器所有者维护；协议中没有修改它们的操作。节点配置文件与节点状态目录都放在所有工作目录之外的主机路径上，节点不在被挂载的业务仓库内生成配置文件或状态目录。

| 表 | 键 | 含义 |
| --- | --- | --- |
| `[node]` | `display_name` | 节点的人类可读名称，由 `hello_ok` 返回；必填 |
| `[node]` | `state_dir` | [节点状态目录](#节点状态目录)的宿主路径；必填 |
| `[node]` | `receipt_retention_ms` | 回执明细的保留期，单位毫秒，自结算起计算；可省略，默认 `2592000000`（30 天） |
| `[[dir]]` | `name` | 工作目录名，即目录句柄名 |
| `[[dir]]` | `path` | 工作目录在宿主上的路径 |
| `[[dir]]` | `access` | 读写级别：`"read"` 或 `"write"` |
| `[[dir]]` | `regenerable` | 可再生路径的规则列表 |
| `[[dir]]` | `hidden` | 排除路径的规则列表 |
| `[[env]]` | `name` | 执行环境句柄名 |
| `[[env]]` | `mounts` | 挂进执行环境的工作目录名；挂载方式跟随目录自己的 `access`，见[挂载表](host-resources.md#挂载表) |
| `[[env]]` | `tools` | 只读挂载的宿主路径（目录或单个文件），如工具链；按原路径挂载 |
| `[[env]]` | `caches` | 包缓存路径；执行环境使用节点私有的副本 |
| `[[env]]` | `egress` | 执行环境持有的网络出口名 |
| `[[env]]` | `path` | 追加到执行环境 `PATH` 的目录，可省略；见[环境变量](host-resources.md#环境变量) |
| `[[env]]` | `vars` | 执行环境中的其他环境变量，显式列出，可省略 |
| `[[egress]]` | `name` | 网络出口句柄名 |
| `[[egress]]` | `hosts` | 允许访问的主机域名 |

执行环境的内存上限也由节点配置设置，键名见[尚未承接](#尚未承接)。`hello_ok.capabilities` 中的 `settle_grace_ms` 由节点实现固定为 5000 毫秒，不可配置。

节点以 `lancet-node <配置文件路径>` 启动，按下列规则读取配置：

- `[node]` 必须出现；`[[dir]]`、`[[env]]` 与 `[[egress]]` 可以没有。
- 可省略的键：`receipt_retention_ms`；`dir` 的 `regenerable`、`hidden`；`env` 的 `mounts`、`tools`、`caches`、`path`、`vars` 与 `egress`。省略的列表取空列表，省略的 `vars` 取空映射。其余键必填。
- 宿主路径（`state_dir`、`dir.path`、`env.tools`、`env.caches`、`env.path`）写成绝对路径，或以 `~/` 开头；`~/` 表示节点进程的 `HOME`。
- `dir.path` 必须是已存在的目录。
- `env.mounts` 中的名称必须是某个 `[[dir]]` 的 `name`；`env.egress` 必须是某个 `[[egress]]` 的 `name`。
- `dir.name`、`env.name` 与 `egress.name` 是脚本中的句柄名：必须是合法的 Starlark 标识符（不是关键字，不含空白），且不与 `act` 全局环境中的内置名称重名，也不是解释器包装脚本用的保留名 `lancet_act_entry__`。违反时节点报错退出，错误写明表名与条目名。
- 配置中出现上表以外的键（`vars` 中的变量名除外），或违反上面任一条时，节点报错退出。

配置形态示意：

```toml
[node]
display_name = "dev"
state_dir = "/home/dev/.local/state/lancet-node"

[[dir]]
name = "lancet"
path = "/home/dev/lancet"
access = "write"
regenerable = ["target/"]        # 可再生路径
hidden = [".env"]                # 排除路径

[[dir]]
name = "lattice"
path = "/home/dev/lattice"
access = "read"

[[env]]
name = "dev"
mounts = ["lancet", "lattice"]          # 挂载方式跟随各目录的 access
tools = ["~/.rustup", "~/.cargo/bin"]   # 只读挂载
caches = ["~/.cargo/registry"]          # 节点私有副本
path = ["~/.cargo/bin"]                 # 追加到 PATH
vars = { LANG = "C.UTF-8" }             # 其他环境变量
egress = "crates"

[[egress]]
name = "crates"
hosts = ["index.crates.io", "static.crates.io"]

# seam 边集由上面的授权推出，不在配置中声明。本例推出：
#   integrity        lancet ⇒ dev       lancet 上的写入会在 dev 中被执行
#   confidentiality  lancet ⇒ crates    lancet 的可读内容可经 crates 外发
#   confidentiality  lattice ⇒ crates
#   confidentiality  dev ⇒ crates       dev 挂载的工具链、缓存与私有家目录可经 crates 外发
```

## 路径规则

- `regenerable` 与 `hidden` 借用 `.gitignore` 的模式语法：目录后缀 `/`、`**`、`!` 否定、后写的规则优先。路径相对各工作目录的根计算。
- 规则只来自节点配置；工作区里的 `.gitignore` 不参与。
- 规则默认为空，节点不内置默认的 `regenerable` 或 `hidden` 规则；配置模板给出示例。
- 各工作目录下的 `.git/` 固定按[影子工作区的 Git 规则](shadow-workspace.md#git)处理。

`regenerable` 标出可再生路径（如 `target/`），`hidden` 标出排除路径（如 `.env`）。两类路径在影子、发布与撤回中的处理见[路径分类](shadow-workspace.md#路径分类)；`hidden` 在目录资源与执行环境中的处理见[目录](host-resources.md#目录)与[挂载表](host-resources.md#挂载表)。

## 启动校验

节点启动时校验下列条件。任一项不满足时，节点报错退出，不退回到无沙箱的执行方式。

| 校验 | 适用条件 |
| --- | --- |
| 工作目录互不重叠 | 总是 |
| 节点配置文件与节点状态目录在所有工作目录之外 | 总是 |
| 可写工作目录、影子、私有缓存与节点状态目录同处一个支持 `FICLONE`（reflink）的文件系统 | 总是 |
| 主机有可用的 `bubblewrap`，以及非特权的用户、挂载、PID、网络命名空间与 cgroup 委派 | 配置授予了执行环境 |
| 执行环境的全部可写挂载的来源都在节点状态目录内 | 配置授予了执行环境 |
| `tools` 与 `caches` 的路径不是宿主家目录，也不是它的上级目录 | 配置授予了执行环境 |
| `tools` 与 `caches` 的路径不与任何工作目录重叠；工作目录只经 `mounts` 挂入 | 配置授予了执行环境 |

- 路径按解析符号链接后的真实路径、以路径分量比较：`/x/a` 与 `/x/ab` 互不重叠；经符号链接指向同一目录的两个工作目录相互重叠。
- `FICLONE` 一项以实际克隆判定：节点在节点状态目录内克隆一次，再从每个可写工作目录向节点状态目录各克隆一次，任一次失败即不满足。克隆的对象都是无名临时文件（`O_TMPFILE`），不在工作目录里留下文件名。影子与私有缓存都在节点状态目录内，随它一并满足。
- 报错写明违反的规则，以及涉及的名称与路径。

## 节点状态目录

| 内容 | 规则所在 |
| --- | --- |
| `node_id`，存于文件 `node_id` | [协议合同](../lancet-protocol.md#概念) |
| 协议服务的 Unix 域套接字 `node.sock` | [协议合同](../lancet-protocol.md#连接与握手) |
| 调用记录账本与回执，存于 `calls/` | [协议合同](../lancet-protocol.md#调用记录) |
| 每个可写工作目录的影子，含私有 `.git/` | [影子工作区](shadow-workspace.md) |
| 发布日志与换出的宿主 inode | [发布](shadow-workspace.md#发布) |
| 执行环境的私有家目录、私有包缓存与私有临时目录 | [执行环境](host-resources.md#执行环境) |

- `state_dir` 不存在时，节点以权限 `0700` 创建它；它的上级目录必须已存在。
- 同一节点状态目录同一时间只由一个节点进程使用：节点启动时对它取独占锁，取不到时报错退出。
- 文件 `node_id` 保存一行小写、带连字符的 UUID。文件不存在时，节点生成 UUID v4，写入并落盘后才开始服务；文件无法解析时，节点报错退出。
- 节点绑定 `node.sock` 之前删除同名的旧文件。
- `calls/` 中每个 `invocation_id` 一个记录文件，文件名为 id 的 UTF-8 字节的 SHA-256 小写十六进制加 `.json`。每次状态迁移写临时文件、落盘后改名替换，再对目录落盘。
- 回执保留期自结算时刻起按节点本地时钟计算；节点在响应 `receipt` 查询与启动时，把到期的记录迁移为 `Expired`。
- 节点启动时，在监听 `node.sock` 之前结算全部遗留的 `Pending` 记录，结局见[`act` 的结局](act.md#结局)。
- 调用记录读写失败时，节点报错退出，重启后由上一条收敛。

## 环境简述与 `seam` 边集

配置加载是推出环境简述的单一构造入口：输入是节点配置与 API 工具声明，输出是 `environment_summary`。环境简述、`act` 的说明（`tools`）与当前授权一起计算 `catalog_digest`，规则见[协议合同](../lancet-protocol.md#describe-与环境简述)。

| 句柄条目的字段 | 取值 |
| --- | --- |
| `name` | 配置中的句柄名 |
| `kind` | 对应的类型存根名：`ReadDir`、`WriteDir`、`ExecEnv`、`Egress` |
| `effects`、`modes` | 注入脚本的句柄：由 API 工具的 effect 与 co-effect 模板对该句柄的类型推出，见[推导表](tools.md#工具声明)。没有注入脚本的句柄为空：可写目录、执行环境与网络出口分别随 [#7](https://github.com/Skyline10124/lancet/issues/7)、[#8](https://github.com/Skyline10124/lancet/issues/8)、[#9](https://github.com/Skyline10124/lancet/issues/9) 注入 |
| `attributes` | 目录：`hidden`、`regenerable`，取配置中的模式原文与顺序；执行环境：`mounts`（挂载的工作目录名）与 `tools`（展开 `~/` 后的路径）；网络出口：`hosts` |

句柄依次列出全部 `[[dir]]`、`[[env]]` 与 `[[egress]]`，各组内按配置中的顺序。

`catalog_digest` 的当前授权是解析后的全部授权条目，即 JSON 对象 `{"dir": [...], "env": [...], "egress": [...]}`：

| 条目 | 字段 |
| --- | --- |
| `dir` | `name`、`path`（真实路径）、`access`、`regenerable`、`hidden` |
| `env` | `name`、`mounts`、`tools`、`caches`、`egress`（省略时为 `null`）、`path`、`vars` |
| `egress` | `name`、`hosts` |

`[node]` 中的设置与 `node_id` 不属于授权。

`seam` 边集由授权推出，不在配置中声明。边按下面的循环顺序排列，各循环按配置中的顺序：

```text
for 每个执行环境 e, for 每个挂进 e 的可写目录 d:
    integrity        d ⇒ e     # d 上的 Write 成为 e 中 Exec 的上下文
for 每个网络出口 n:
    for 每个可读的目录 d（目录句柄或执行环境挂载）:
        confidentiality  d ⇒ n     # 经 Uses<d> 读到的内容可经 n 上的 Emit 外发
    for 每个执行环境 e:
        confidentiality  e ⇒ n     # e 自己挂载的工具链、缓存与私有家目录可经 n 外发
```

第一个版本每条边的 `policy` 都是 `Allow`：授予可读目录、执行环境与网络出口，就是确认它们之间的组合。机器所有者不接受某个组合时，在配置中收窄授权：

| 要去掉的边 | 配置中的做法 |
| --- | --- |
| 完整性 `d ⇒ e` | 不把 `d` 列进 `e` 的 `mounts`，或把 `d` 的 `access` 改为 `"read"` |
| 机密性 `d ⇒ n` | 不授予 `d`，或不授予 `n` |
| 机密性 `e ⇒ n` | 不授予 `e`，或不授予 `n`；要缩小 `e` 暴露的内容，只在 `tools` 中列需要的路径 |

- 机密性边不限于同一次 `act`：第一次 `act` 读到的内容交回模型后，模型可以把它写进下一次 `act` 的外发请求。授权组合里同时有可读目录（或执行环境）与网络出口，就成立一条边；执行环境里进程的输出同样可以经模型中转，所以 `e ⇒ n` 不限于 `e` 自己持有的出口。
- 边全是 `Allow`，闸口放行全部跨越，节点不在运行时拦截（闸口的规则见[协议合同](../lancet-protocol.md#effectco-effect-与回执)）。回执照常记录变更的文件与外发的主机，供调用方核对与用户事后审计。

## 保证

| 保证 | 适用范围 | 成立条件 | 依据或验证 |
| --- | --- | --- | --- |
| 环境简述、`seam` 边集与 `catalog_digest` 只随配置、授权与节点版本变化 | 每次启动与 `describe` | 单一构造入口，输入只有配置与声明 | [ADR 0002 第 5 节](../../adr/0002-lancet-node-api-tools-and-act.md#5-工作区与节点配置)、[第 7 节](../../adr/0002-lancet-node-api-tools-and-act.md#7-effectco-effectresidue-与-seam-在工具基座中的读取方)；`crates/lancet-node/tests/node.rs::hello_describe_matches_config`、`restart_preserves_identity_and_catalog`、`digest_tracks_non_displayed_grants`（含由首批 API 工具声明推出的 `modes`） |
| 违反任一启动校验时，节点不提供服务 | 每次启动 | 校验先于监听协议连接 | [ADR 0002 第 4 节](../../adr/0002-lancet-node-api-tools-and-act.md#4-边界由宿主资源承担)、第 5 节、[第 6 节](../../adr/0002-lancet-node-api-tools-and-act.md#6-影子工作区待定改动与发布)；`crates/lancet-node/tests/node.rs::startup_rejects_overlapping_dirs`、`startup_keeps_config_and_state_outside_dirs`、`startup_requires_shared_reflink_filesystem`（覆盖总是适用的三项；授予执行环境时适用的四项待 [#8](https://github.com/Skyline10124/lancet/issues/8)） |
| 授权、路径规则与网络出口清单只能由机器所有者修改 | 节点运行期间 | 协议没有修改它们的操作；配置文件在所有工作目录之外，脚本拿不到它的句柄 | ADR 0002 第 5 节；`crates/lancet-node/tests/node.rs::startup_keeps_config_and_state_outside_dirs`（配置文件位于工作目录内时节点拒绝启动） |

## 范围之外

| 职责 | 承担方 |
| --- | --- |
| 编辑与分发节点配置 | 机器所有者 |
| 审批 | 推后，见 [`lancet-node` 尚未承接](README.md#尚未承接) |

## 尚未承接

| 事项 | 现状 |
| --- | --- |
| 执行环境内存上限的键名与默认值 | 由 [#8](https://github.com/Skyline10124/lancet/issues/8) 确定 |
| 同一路径同时匹配 `hidden` 与 `regenerable` 时怎样处理：启动时拒绝，或按某种优先级 | 待定，随 [#7](https://github.com/Skyline10124/lancet/issues/7) 决定 |

## 验证

- `cargo xtask docs-check`：本合同的链接与锚点有效。
- `cargo test -p lancet-node`：节点进程级集成测试，覆盖配置读取、句柄名校验、总是适用的三项启动校验、`node_id` 的持久化与状态目录独占、环境简述、`seam` 边集与 `catalog_digest`。
