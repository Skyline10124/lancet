---
status: active
---

# 节点配置与工作区

节点配置由机器所有者维护，规定一个 `lancet-node` 服务的工作区与授予的句柄。节点启动时校验配置与路径隔离，再由配置与 API 工具声明推出环境简述、`seam` 边集与 `catalog_digest`。

## 概念

| 概念 | 含义 | 状态持有者 | 生命周期 |
| --- | --- | --- | --- |
| 节点配置 | 工作区组成、授权与网络出口清单 | 节点配置文件 | 由机器所有者修改；节点启动时读取 |
| 节点状态目录 | 节点的持久状态，内容见[节点状态目录](#节点状态目录) | 主机文件系统 | 节点首次启动时初始化，跨重启保留 |
| 工作区 | 一组互不重叠的工作目录 | 节点配置 | 随配置 |
| 工作目录 | 模型操作的对象：宿主上的一个目录，有自己的名称与读写级别，对应一个目录句柄；目录内所有路径一视同仁 | 节点配置 | 随配置 |
| 环境目录 | 执行环境挂载、但不属于工作目录的目录（如系统目录、工具链与包缓存，具体挂载细节随 [#8](https://github.com/Skyline10124/lancet/issues/8) 定） | 节点配置 | 随配置 |

- 一个节点只对应一个工作区。多仓库协作由同一工作区内的多个工作目录表达。
- 只有 `proc.*` 能看到环境目录；`fs.*` 拿不到它们的句柄。

## 配置文件

授权与网络出口清单只写在节点配置文件里，由机器所有者维护；协议中没有修改它们的操作。节点配置文件与节点状态目录都放在所有工作目录之外的主机路径上，节点不在工作目录内生成配置文件或状态目录。

| 表 | 键 | 含义 |
| --- | --- | --- |
| `[node]` | `display_name` | 节点的人类可读名称，由 `hello_ok` 返回；必填 |
| `[node]` | `state_dir` | [节点状态目录](#节点状态目录)的宿主路径；必填 |
| `[node]` | `receipt_retention_ms` | 回执明细的保留期，单位毫秒，自结算起计算；可省略，默认 `2592000000`（30 天） |
| `[[dir]]` | `name` | 工作目录名，即目录句柄名 |
| `[[dir]]` | `path` | 工作目录在宿主上的路径 |
| `[[dir]]` | `access` | 读写级别：`"read"` 或 `"write"` |
| `[[env]]` | `name` | 执行环境句柄名 |
| `[[env]]` | `mounts` | 挂进执行环境的工作目录名；细节见[执行环境](host-resources.md#执行环境) |
| `[[env]]` | `tools` | 挂载的宿主路径（目录或单个文件），如工具链 |
| `[[env]]` | `caches` | 包缓存路径 |
| `[[env]]` | `egress` | 执行环境持有的网络出口名 |
| `[[env]]` | `path` | 追加到执行环境 `PATH` 的目录，可省略 |
| `[[env]]` | `vars` | 执行环境中的其他环境变量，显式列出，可省略 |
| `[[egress]]` | `name` | 网络出口句柄名 |
| `[[egress]]` | `hosts` | 允许访问的主机域名 |

执行环境的内存上限等细节由 [#8](https://github.com/Skyline10124/lancet/issues/8) 确定。`hello_ok.capabilities` 中的 `settle_grace_ms` 由节点实现固定为 5000 毫秒，不可配置。

节点以 `lancet-node <配置文件路径>` 启动，按下列规则读取配置：

- `[node]` 必须出现；`[[dir]]`、`[[env]]` 与 `[[egress]]` 可以没有。
- 可省略的键：`receipt_retention_ms`；`env` 的 `mounts`、`tools`、`caches`、`path`、`vars` 与 `egress`。省略的列表取空列表，省略的 `vars` 取空映射。其余键必填。
- 宿主路径（`state_dir`、`dir.path`、`env.tools`、`env.caches`、`env.path`）写成绝对路径，或以 `~/` 开头；`~/` 表示节点进程的 `HOME`。
- `dir.path` 必须是已存在的目录。
- `env.mounts` 中的名称必须是某个 `[[dir]]` 的 `name`；`env.egress` 必须是某个 `[[egress]]` 的 `name`。
- `dir.name`、`env.name` 与 `egress.name` 是脚本中的句柄名：必须是合法的 Starlark 标识符（不是关键字，不含空白），且不与 `act` 全局环境中的内置名称重名，也不是解释器包装脚本用的保留名 `lancet_act_entry__`。违反时节点报错退出，错误写明表名与条目名。
- 配置中出现上表以外的键（`vars` 中的变量名除外，包括已取消的 `hidden` 与 `regenerable`），或违反上面任一条时，节点报错退出。

配置形态示意：

```toml
[node]
display_name = "dev"
state_dir = "/home/dev/.local/state/lancet-node"

[[dir]]
name = "lancet"
path = "/home/dev/lancet"
access = "write"

[[dir]]
name = "lattice"
path = "/home/dev/lattice"
access = "read"

[[env]]
name = "dev"
mounts = ["lancet", "lattice"]
tools = ["~/.rustup", "~/.cargo/bin"]
caches = ["~/.cargo/registry"]
path = ["~/.cargo/bin"]
vars = { LANG = "C.UTF-8" }
egress = "crates"

[[egress]]
name = "crates"
hosts = ["index.crates.io", "static.crates.io"]

# seam 边集由上面的授权推出，不在配置中声明。本例推出：
#   integrity        lancet ⇒ dev       lancet 上的写入会在 dev 中被执行
#   confidentiality  lancet ⇒ crates    lancet 的可读内容可经 crates 外发
#   confidentiality  lattice ⇒ crates
#   confidentiality  dev ⇒ crates       dev 挂载的工具链与缓存可经 crates 外发
```

## 启动校验

节点启动时校验下列条件。任一项不满足时，节点报错退出。

| 校验 | 适用条件 |
| --- | --- |
| 工作目录互不重叠 | 总是 |
| 节点配置文件与节点状态目录在所有工作目录之外 | 总是 |
| 可写工作目录不能是节点状态目录或其子目录 | 总是 |

- 路径按解析符号链接后的真实路径、以路径分量比较：`/x/a` 与 `/x/ab` 互不重叠；经符号链接指向同一目录的两个工作目录相互重叠。
- 可写工作目录与状态目录不要求位于同一文件系统，也不探测 `FICLONE`。
- 报错写明违反的规则，以及涉及的名称与路径。
- 授予执行环境时的附加启动校验随 [#8](https://github.com/Skyline10124/lancet/issues/8) 确定。

## 节点状态目录

节点在磁盘上保存的持久状态为 `node_id` 与 `calls/`；运行时在状态目录中绑定套接字 `node.sock`。

| 内容 | 规则所在 |
| --- | --- |
| `node_id`，存于文件 `node_id` | [协议合同](../lancet-protocol.md#概念) |
| 协议服务的 Unix 域套接字 `node.sock` | [协议合同](../lancet-protocol.md#连接与握手) |
| 调用记录账本与回执，存于 `calls/` | [协议合同](../lancet-protocol.md#调用记录) |

- `state_dir` 不存在时，节点以权限 `0700` 创建它；它的上级目录必须已存在。
- 同一节点状态目录同一时间只由一个节点进程使用：节点启动时对它取独占锁，取不到时报错退出。
- 文件 `node_id` 保存一行小写、带连字符的 UUID。文件不存在时，节点生成 UUID v4，写入并落盘后才开始服务；文件无法解析时，节点报错退出。
- 节点绑定 `node.sock` 之前删除同名的旧文件。
- `calls/` 中每个 `invocation_id` 一个记录文件，文件名为 id 的 UTF-8 字节的 SHA-256 小写十六进制加 `.json`。每次状态迁移写临时文件、落盘后改名替换，再对目录落盘。
- 回执保留期自结算时刻起按节点本地时钟计算；节点在响应 `receipt` 查询与启动时，把到期的记录迁移为 `Expired`。
- 节点启动时直接打开授权的目录，在监听 `node.sock` 之前恢复并结算全部遗留的 `Pending` 记录，结局见[`act` 的结局](act.md#结局)。
- 调用记录读写失败时，节点报错退出，重启后由上一条收敛。

## 环境简述与 `seam` 边集

配置加载是推出环境简述的单一构造入口：输入是节点配置与 API 工具声明，输出是 `environment_summary`。环境简述、`act` 的说明（`tools`）与当前授权一起计算 `catalog_digest`，规则见[协议合同](../lancet-protocol.md#describe-与环境简述)。

| 句柄条目的字段 | 取值 |
| --- | --- |
| `name` | 配置中的句柄名 |
| `kind` | 对应的类型存根名：`ReadDir`、`WriteDir`、`ExecEnv`、`Egress` |
| `effects`、`modes` | 注入脚本的句柄：由 API 工具的 effect 与 co-effect 模板对该句柄的类型推出，见[推导表](tools.md#工具声明)。没有注入脚本的句柄为空：执行环境与网络出口分别随 [#8](https://github.com/Skyline10124/lancet/issues/8)、[#9](https://github.com/Skyline10124/lancet/issues/9) 注入 |
| `attributes` | 目录：空映射 `{}`；执行环境：`mounts`（挂载的工作目录名）与 `tools`（展开 `~/` 后的路径）；网络出口：`hosts` |

句柄依次列出全部 `[[dir]]`、`[[env]]` 与 `[[egress]]`，各组内按配置中的顺序。

`catalog_digest` 的当前授权是解析后的全部授权条目，即 JSON 对象 `{"dir": [...], "env": [...], "egress": [...]}`：

| 条目 | 字段 |
| --- | --- |
| `dir` | `name`、`path`（真实路径）、`access` |
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
        confidentiality  e ⇒ n     # e 自己挂载的工具链与缓存可经 n 外发
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
| 环境简述、`seam` 边集与 `catalog_digest` 只随配置、授权与节点版本变化 | 每次启动与 `describe` | 单一构造入口，输入只有配置与声明 | [ADR 0002 第 5 节](../../adr/0002-lancet-node-api-tools-and-act.md#5-工作区与节点配置)、[第 7 节](../../adr/0002-lancet-node-api-tools-and-act.md#7-effectco-effectresidue-与-seam-在工具基座中的读取方)、[ADR 0003](../../adr/0003-direct-workspace-and-per-call-sandbox.md)；`crates/lancet-node/tests/node.rs::hello_describe_matches_config`、`restart_preserves_identity_and_catalog`、`digest_tracks_non_displayed_grants` |
| 违反任一启动校验时，节点不提供服务 | 每次启动 | 校验先于监听协议连接 | [ADR 0002 第 4 节](../../adr/0002-lancet-node-api-tools-and-act.md#4-边界由宿主资源承担)、第 5 节、[ADR 0003](../../adr/0003-direct-workspace-and-per-call-sandbox.md)；`crates/lancet-node/tests/node.rs::startup_rejects_overlapping_dirs`、`startup_keeps_config_and_state_outside_dirs`、`startup_rejects_removed_dir_rule_keys` |
| 授权与网络出口清单只能由机器所有者修改 | 节点运行期间 | 协议没有修改它们的操作；配置文件在所有工作目录之外，脚本拿不到它的句柄 | ADR 0002 第 5 节；`crates/lancet-node/tests/node.rs::startup_keeps_config_and_state_outside_dirs` |

## 范围之外

| 职责 | 承担方 |
| --- | --- |
| 编辑与分发节点配置 | 机器所有者 |
| 审批 | 推后，见 [`lancet-node` 尚未承接](README.md#尚未承接) |

## 尚未承接

| 事项 | 现状 |
| --- | --- |
| 执行环境配置细节与附加启动校验 | 由 [#8](https://github.com/Skyline10124/lancet/issues/8) 确定 |

## 验证

- `cargo xtask docs-check`：本合同的链接与锚点有效。
- `cargo test -p lancet-node --test node`：节点进程级集成测试，覆盖配置读取、旧键拒绝、句柄名校验、路径与状态目录隔离启动校验、跨文件系统可写目录启动、`node_id` 持久化与状态目录独占、环境简述、`seam` 边集与 `catalog_digest`。
