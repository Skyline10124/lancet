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

此外，节点配置还设置节点的 `display_name`、执行环境的内存上限，以及回执明细的保留期（默认 30 天）。

配置形态示意：

```toml
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

## 节点状态目录

| 内容 | 规则所在 |
| --- | --- |
| `node_id` | [协议合同](../lancet-protocol.md#概念) |
| 调用记录账本与回执 | [协议合同](../lancet-protocol.md#调用记录) |
| 每个可写工作目录的影子，含私有 `.git/` | [影子工作区](shadow-workspace.md) |
| 发布日志与换出的宿主 inode | [发布](shadow-workspace.md#发布) |
| 执行环境的私有家目录、私有包缓存与私有临时目录 | [执行环境](host-resources.md#执行环境) |

## 环境简述与 `seam` 边集

配置加载是推出环境简述的单一构造入口：输入是节点配置与 API 工具声明，输出是 `environment_summary`。环境简述、`act` 的说明（`tools`）与当前授权一起计算 `catalog_digest`，规则见[协议合同](../lancet-protocol.md#describe-与环境简述)。

| 句柄条目的字段 | 取值 |
| --- | --- |
| `name` | 配置中的句柄名 |
| `kind` | 对应的类型存根名：`ReadDir`、`WriteDir`、`ExecEnv`、`Egress` |
| `effects`、`modes` | 由 API 工具的 effect 与 co-effect 模板对该句柄的类型推出，见[推导表](tools.md#工具声明) |
| `attributes` | 目录：`hidden`、`regenerable`；执行环境：挂载的工作目录与 `tools` 列出的路径；网络出口：允许的主机 |

`seam` 边集由授权推出，不在配置中声明：

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
| 环境简述、`seam` 边集与 `catalog_digest` 只随配置、授权与节点版本变化 | 每次启动与 `describe` | 单一构造入口，输入只有配置与声明 | [ADR 0002 第 5 节](../../adr/0002-lancet-node-api-tools-and-act.md#5-工作区与节点配置)、[第 7 节](../../adr/0002-lancet-node-api-tools-and-act.md#7-effectco-effectresidue-与-seam-在工具基座中的读取方) |
| 违反任一启动校验时，节点不提供服务 | 每次启动 | 校验先于监听协议连接 | [ADR 0002 第 4 节](../../adr/0002-lancet-node-api-tools-and-act.md#4-边界由宿主资源承担)、第 5 节、[第 6 节](../../adr/0002-lancet-node-api-tools-and-act.md#6-影子工作区待定改动与发布) |
| 授权、路径规则与网络出口清单只能由机器所有者修改 | 节点运行期间 | 协议没有修改它们的操作；配置文件在所有工作目录之外，脚本拿不到它的句柄 | ADR 0002 第 5 节 |

## 范围之外

| 职责 | 承担方 |
| --- | --- |
| 编辑与分发节点配置 | 机器所有者 |
| 审批 | 推后，见 [`lancet-node` 尚未承接](README.md#尚未承接) |

## 尚未承接

| 事项 | 现状 |
| --- | --- |
| 节点级设置与执行环境内存上限的键名与默认值，节点状态目录与 Unix 域套接字的位置 | 由 [#5](https://github.com/Skyline10124/lancet/issues/5)、[#8](https://github.com/Skyline10124/lancet/issues/8) 确定 |
| 同一路径同时匹配 `hidden` 与 `regenerable` 时怎样处理：启动时拒绝，或按某种优先级 | 待定，随 [#7](https://github.com/Skyline10124/lancet/issues/7) 决定 |

## 验证

- `cargo xtask docs-check`：本合同的链接与锚点有效。
- 启动校验、环境简述与 `catalog_digest` 的测试随 [#5](https://github.com/Skyline10124/lancet/issues/5) 建立，建立后写入保证表。
