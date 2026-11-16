---
status: draft
---

# 工具声明：WIT 接口与自定义段

## 结论

本实验核对 [ADR 0002](https://github.com/Skyline10124/lancet/blob/2850986cd85544c17106960766fcbd3c757359e0/docs/adr/0002-lancet-node-api-tools-and-act.md) 第 2 节（`docs/adr-0002-lancet-node-act` 分支 `2850986`）的规则：工具声明由组件导出的 WIT 接口与嵌在自定义段中的 effect 清单组成，`.pyi`、Starlark 静态检查的类型与摘要由节点从两者推出。22 项核对全部通过，另记 5 项观察。

| 规则中的说法 | 实测 | 依据 |
| --- | --- | --- |
| 节点能从组件读出 WIT 接口与自定义段中的清单 | 成立。`wit-component` 解出导出接口的函数、参数名、参数与返回类型、记录；`wasmparser` 读出组件顶层的 `lancet:effects` 段；按 R1–R10 核对，15 种错误清单全部被拒绝 | D1、M1、M6 |
| `wasmtime` 加载组件时按 WIT 接口核对类型 | 需要改写。实例化时按节点的链接器核对组件的导入（缺 WASI 实现、宿主签名不一致都失败）；每次调用时核对实参的类型、个数与资源种类；组件内部把只读句柄当可写句柄用时陷入 | W2、W3、W7–W10 |
| WIT 表达不了 effect 与 co-effect | 句柄的种类与模式各为一种资源类型时不成立：模板由句柄参数的类型唯一确定。2,070 份变异清单中，通过核对的 54 份恰好都等于由类型推出的模板 | M7 |
| `.pyi` 与 Starlark 类型由声明推出 | 类型能推出，说明推不出：组件里没有 WIT 文档注释，`.pyi` 只有签名。把带文档的 WIT 包放进自定义段 `lancet:wit` 后，9 条文档全部取回，签名与组件导出接口一致 | D2、D3、P1、S |

自定义段的使用条件：

- 段要在组件顶层。经 Rust `#[link_section]` 写入的段落在嵌套的核心模块里（深度 2），组件顶层读不到。
- `wasm-tools strip` 用默认参数就会删除这两个段（默认只保留 `name`、`component-type`、`dylink.0`）；缺段的组件按 R1 被拒绝加载。
- `wasmtime` 的预编译产物保留参数名与类型，不含清单与文档；节点要在预编译前从原组件读出声明，与预编译产物一同保存。

工具的构建方式：同一源码用 Rust 默认的 `wasm32-wasip2` 构建时，标准库带入 13 个 WASI 导入，在只提供 `lancet:host/handles` 的链接器下实例化失败；用 `wasm32-unknown-unknown` 构建、再经 `wasm-tools component new` 转为组件，导入只有宿主接口。

## 环境

2026-11-16 在开发主机上运行了三次完整的 `run.sh`（第三次在 `cargo fmt` 之后，[results/](results/) 为这一次的输出）：各项结论相同，除耗时外的细节相同，全部 `.wasm` 产物逐字节相同；耗时写三次的范围。主机为 Rocky Linux 10.2，内核 6.12，AMD EPYC 9B14（8 核），`rustc 1.96.0`。

| 依赖 | 版本 | 用途 |
| --- | --- | --- |
| `wasmtime` | 49.0.5 | 节点侧编译、链接、实例化、动态调用、预编译 |
| `wit-component`、`wit-parser`、`wasmparser` | 0.258.3 | 节点侧解出接口、读取自定义段 |
| `starlark` | 0.14.2 | 节点侧构造类型、静态检查 |
| `wit-bindgen` | 0.60.0 | 工具侧绑定（内部使用 `wit-component` 0.254.2） |
| `wasm-tools` | 1.254.0 | 核心模块转组件、编码 WIT 包、`strip`、`validate` |

## 原型

### 宿主接口与示例工具

宿主接口 `lancet:host/handles@0.1.0`（[host.wit](wit/lancet-host/host.wit)）把句柄的种类与模式写成四种资源类型 `read-dir`、`write-dir`、`exec-env`、`egress`，与 ADR 第 3 节存根中的 `ReadDir`、`WriteDir`、`ExecEnv`、`Egress` 一一对应。

示例工具 `acme:notes/tool@0.1.0`（[world.wit](tools/notes/wit/world.wit)、[effects.json](tools/notes/effects.json)、[lib.rs](tools/notes/src/lib.rs)）：

| 函数 | 句柄参数 | 清单中的 effect | 清单中的 co-effect |
| --- | --- | --- | --- |
| `grep` | `root: borrow<read-dir>` | 无 | `Uses<root, Read>` |
| `append-note` | `root: borrow<write-dir>` | `Write<root>` | `Uses<root, ReadWrite>` |
| `fetch-into` | `net: borrow<egress>`、`root: borrow<write-dir>` | `Emit<net>`、`Write<root>` | `Uses<net>`、`Uses<root, ReadWrite>` |
| `forge` | `root: borrow<read-dir>` | 无 | `Uses<root, Read>` |

`forge` 的声明正确、实现恶意：它用 `WriteDir::from_handle(root.handle())` 把只读句柄的编号当作可写句柄，调用 `write`。

### 产物

| 产物 | 构建方式 |
| --- | --- |
| `notes.component.wasm` | `wasm32-unknown-unknown` 核心模块，经 `wasm-tools component new` 转为组件 |
| `notes.packed.wasm` | 在上者顶层追加 `lancet:effects`（清单 JSON，480 字节）与 `lancet:wit`（带文档的 WIT 包，1,906 字节） |
| `notes.linksection.component.wasm` | 清单经 `#[link_section = "lancet:effects"]` 写入核心模块 |
| `notes.rebuild.component.wasm` | 在另一个目标目录从头重建 |
| `notes.wasip2.wasm` | 同一源码，`wasm32-wasip2` |
| `stale.component.wasm` | 按旧宿主接口编译：`read-dir.read` 返回 `list<u8>` 而不是 `string`，版本号相同 |
| `notes.packed.strip-default.wasm`、`notes.packed.strip-all.wasm` | `wasm-tools strip`，分别用默认参数与 `--all` |

### 核对规则

节点（[manifest.rs](node/src/manifest.rs)）按下列规则核对清单。R8–R10 由 ADR 第 4 节“句柄按声明的模式收窄”与第 7 节“实际 effect 越出模板即为节点缺陷”推出。

| 规则 | 内容 |
| --- | --- |
| R1 | 组件顶层恰有一个 `lancet:effects` 段 |
| R2 | 清单是格式版本 1 的 JSON，不含未知字段 |
| R3 | 清单指向的接口由组件导出，组件没有清单未覆盖的导出接口 |
| R4 | 清单条目与导出函数一一对应 |
| R5 | 模板语法正确，effect 与 co-effect 各在自己的列表中，不重复 |
| R6 | 模板引用的参数存在 |
| R7 | 模板引用的参数是 `borrow` 句柄 |
| R8 | 模板的种类与模式与参数的资源类型相符：`Write` 只对 `write-dir`，`Exec` 只对 `exec-env`，`Emit` 只对 `egress`；`Uses<p, Read>` 只对 `read-dir`，`Uses<p, ReadWrite>` 只对 `write-dir`，`Uses<p>` 只对 `exec-env` 与 `egress` |
| R9 | 每个句柄参数恰有一个 `Uses` 模板 |
| R10 | 句柄类型给出的写、执行、发出能力都在 effect 模板中；缺一项时工具就能做出模板之外的 effect，第 7 节的不变式不成立 |

## 结果

原始结果见 [suite.json](results/suite.json)，组件的 WIT 与段布局见 [notes.packed.wit](results/notes.packed.wit) 与 [sections.txt](results/sections.txt)。

### 解出接口（D）

| 编号 | 核对 | 结果 |
| --- | --- | --- |
| D1 | 解出导出接口 | 通过。导入只有 `lancet:host/handles@0.1.0`；4 个函数的参数名、参数与返回类型，以及记录 `hit` 的 3 个字段都与源 WIT 一致。组件只导入用到的资源与方法，未用到的 `exec-env` 与 `write-dir.entries` 不在导入中 |
| D2 | 组件里的文档 | 观察：0 条。`wit-bindgen` 写入核心模块的 `component-type` 段只编码世界的类型；WIT 的 `package-docs` 段只在编码 WIT 包时生成，`wit-parser` 解码组件时也只把它注入组件自己的根包，到不了导出的 `acme:notes/tool` |
| D3 | 从 `lancet:wit` 段取文档 | 通过。解出 9 条文档（接口 1、记录 1、字段 3、函数 4），签名与组件导出接口逐项相同 |
| D4 | `wasm32-wasip2` 构建 | 通过。导出签名相同，导入多出 13 个 WASI 接口（`wasi:io` 3 个、`wasi:cli` 10 个），按导入名单拒绝 |

### 清单（M）

| 编号 | 核对 | 结果 |
| --- | --- | --- |
| M1 | 读出并核对打包后的清单 | 通过 R1–R10；清单模板与由参数类型推出的模板相同 |
| M2 | 未打包的组件 | 按 R1 拒绝 |
| M3 | 经 `#[link_section]` 写入的清单 | 段在深度 2（嵌套核心模块），顶层没有；按 R1 拒绝 |
| M4 | `wasm-tools strip` | 观察：默认参数与 `--all` 都删除了 `lancet:effects` 与 `lancet:wit`；按 R1 拒绝 |
| M5 | 顶层有两个清单段 | 按 R1 拒绝 |
| M6 | 15 种错误清单 | 全部被拒绝且命中预期规则：JSON 截断、格式版本、未知字段（R2）；指向未导出的接口（R3）；缺条目、多条目（R4）；语法错误、放错列表（R5）；不存在的参数（R6）；非句柄参数（R7）；只读目录声明 `ReadWrite` 或 `Write`、`Emit` 指向目录（R8）；出口参数缺 `Uses`（R9）；可写目录不声明 `Write`（R10） |
| M7 | 模板全集上的变异 | 通过。每个参数对应 6 种模板（`Write`、`Exec`、`Emit`、`Uses`、`Uses Read`、`Uses ReadWrite`），每个函数的全集是其全部参数的这 6 种模板；变异包括单项增删 66 份、每个函数 500 份随机子集（每项以 15% 的概率选入）、原清单 4 份，共 2,070 份。通过且等于推出模板 54 份，被拒绝且不等 2,016 份；“通过但不等”与“被拒绝但相等”均为 0 |
| M8 | 声明摘要 | 通过。另一目标目录重建的组件与原组件逐字节相同；清单改为紧凑格式（480 → 353 字节）后摘要不变；由类型推出的模板得到同一摘要 |

### `wasmtime`（W）

节点的链接器只定义 `lancet:host/handles`；每次调用新建 `Store` 与实例，经 `Func::call` 动态调用导出函数。

| 编号 | 核对 | 结果 |
| --- | --- | --- |
| W1 | 实例化打包后的组件 | 成功，自定义段不影响加载；`Component::new` 编译 11.6–12.0 ms |
| W2 | 实例化 `wasip2` 构建 | 失败：`component imports instance wasi:io/poll@0.2.6, but a matching implementation was not found in the linker` |
| W3 | 实例化按旧宿主接口编译的组件 | 失败：`instance export [method]read-dir.read has the wrong type: type mismatch with results: expected string found list` |
| W4–W6 | 合法调用 `grep`、`append-note`、`fetch-into` | 结果正确；宿主资源记下的实际 effect 依次为无、`Write<root>`、`Emit<net>` 与 `Write<root>`，都在模板内 |
| W7 | `needle` 传 `u32` | 调用前报错 `type mismatch: expected string, found u32`，宿主函数未被调用 |
| W8 | 少传一个参数 | 调用前报错 `expected 3 argument(s), got 2` |
| W9 | 把 `write-dir` 资源传给 `borrow<read-dir>` | 调用前报错 `mismatched resource types` |
| W10 | `forge` | 陷入：`handle index 1 used with the wrong type`；宿主写函数调用 0 次 |
| W11 | 预编译产物 | 观察：132,968 字节，预编译 8.7–9.2 ms；不含清单文本、段名与文档，含参数名 |
| W12 | 反序列化预编译产物 | `component_type()` 给出的导入、导出函数与参数名和原组件相同；实例化与调用成功 |

### Starlark 静态检查（S）与 `.pyi`（P）

节点在运行时由解出的接口构造类型（[star.rs](node/src/star.rs)）：

- 每种句柄是一个 `TyUser`，方法为第一个参数是该句柄的工具函数；可写目录同时带只读目录的方法。
- 记录是带已知字段的 `TyUser`。
- 全局注入 `docs`（只读目录）、`lancet`（可写目录）、`net`（出口）。

脚本包进单一入口函数后，用 `AstModuleTypecheck::typecheck` 检查：

| 编号 | 脚本 | 结果 |
| --- | --- | --- |
| S1 | 搜索、追加、推导、算术 | 无错误 |
| S2 | `lancet.grep("TODO", 5)` | 无错误 |
| S3 | `docs.grep(42)` | `Expected type str but got int` |
| S4 | `docs.grep(pattern = "TODO")` | `Unexpected parameter named pattern` |
| S5 | `docs.append_note(...)` | `The attribute append_note is not available on the type ReadDir` |
| S6 | `h.lien` | `The attribute lien is not available on the type Hit` |
| S7 | `lancet.append_note(...) + "x"` | `Binary operator + is not available on the types int and str` |
| S8 | `net.fetch_into(docs, ...)` | `Expected type WriteDir but got ReadDir` |
| S9 | `docs.grepp(...)` | `The attribute grepp is not available on the type ReadDir` |
| S10 | `lancet.append_note("a.md")` | `Missing required parameter text` |

`.pyi` 渲染了两份：[只用组件](results/act-from-component.pyi)（24 行，没有说明）与[加上 `lancet:wit` 段的文档](results/act-with-wit-docs.pyi)（28 行，8 条说明）。

### 耗时（T）

加载一个工具声明（解出接口、读清单并核对、读文档、渲染 `.pyi`、构造 Starlark 全局类型）200 次：中位数 0.39 ms，p95 0.42–0.43 ms，最大 0.58–0.60 ms；组件 49,982 字节。

## 对 ADR 0002 第 2 节的建议

以下是建议，由维护者决定。

1. **effect 与 co-effect 模板由句柄参数的资源类型推出，不再手写清单。** 宿主接口把句柄的种类与模式定为四种资源类型后，R8–R10 使清单只能等于由类型推出的模板（M7），手写清单只多出一份需要保持一致的副本；类型本身由 `wasmtime` 强制（W9、W10）。推出规则：

   | 参数类型 | co-effect | effect |
   | --- | --- | --- |
   | `borrow<read-dir>` | `Uses<p, Read>` | 无 |
   | `borrow<write-dir>` | `Uses<p, ReadWrite>` | `Write<p>` |
   | `borrow<exec-env>` | `Uses<p>` 与 `mounts(p)` | `Exec<p>`，以及按挂载与出口派生的 `Write`、`Emit` |
   | `borrow<egress>` | `Uses<p>` | `Emit<p>` |

   这一结论限于第 2 节现有的模板粒度（按句柄）。将来模板要细到参数值（例如只写 `path` 指定的文件）时，WIT 类型表达不了，需要另行声明。
2. **说明来自自定义段 `lancet:wit` 中带文档的 WIT 包。** 节点核对其签名与组件导出接口相同（D3），从中取文档渲染 `.pyi`。工具作者只写一份带注释的 WIT。
3. **改写“`wasmtime` 加载组件时按它核对类型”**：实例化时按节点的链接器核对导入；调用时核对实参的类型、个数与资源种类；组件内部用错句柄种类时陷入。
4. **写明自定义段的读取条件**：只读组件顶层的段；节点在预编译前读出声明，与预编译产物一同保存；缺段的组件拒绝加载。
5. **工具组件只导入 `lancet:host`。** 第三方工具用 `wasm32-unknown-unknown` 构建再转为组件；是否向工具提供部分 WASI 接口另行决定。

## 未覆盖

- Starlark 静态检查在节点进程中原生运行，没有放进解释器组件；“脚本调用 → 节点分发到工具组件 → 返回值转回 Starlark 值”的整条路径没有打通。
- 没有示例工具以 `exec-env` 为参数，`mounts(e)` 派生的 effect 没有测。
- `environment_summary` 与 `catalog_digest` 依赖节点配置，这里只测了声明本身的摘要。
- 第三方工具的安装、分发与信任配置（ADR 第 1 节推后的部分）。
- ADR“尚未核实”中的另外两项：`ResourceLimiter` 与 epoch 中断；复用 `Engine` 时的实例化耗时。

## 复现

```bash
./run.sh
```
