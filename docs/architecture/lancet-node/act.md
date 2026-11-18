---
status: active
---

# `act` 与 Starlark 编排

`act` 是 `lancet-node` 对模型可见的唯一工具。模型传入一段 Starlark 脚本，脚本持有授权句柄、组合调用 API 工具、对结果做纯计算筛选，再把需要进入模型上下文的数据交回。脚本默认遇错即止；写入成为跨 `act` 保留的待定改动，由某次 `act` 的脚本调用 `commit()` 发布或调用 `discard()` 丢弃。

## 概念

| 概念 | 含义 | 状态持有者 | 生命周期 |
| --- | --- | --- | --- |
| 脚本 | 模型传给 `act` 的 Starlark 源码，只做编排，自身不产生、也不声明 effect 与 co-effect | `execute` 的参数 | 一次 `act` |
| 解释器组件 | 内嵌 `starlark` 与 `regex` 的 WASM 组件，不持有宿主资源 | 节点 | 一次 `act` |
| 全局环境 | 本次 `act` 注入的句柄对象、内置函数与类型 | 解释器组件 | 一次 `act` |
| 类型存根 | `act` 的说明：由 API 工具声明与解释器内置类型渲染的 `.pyi` | 节点 | 随节点版本 |
| 文本视图 | 解释器内的链式文本筛选对象 `TextView` | 解释器组件 | 一次 `act` |
| `print` 缓冲 | 本次 `act` 的 `print` 输出 | 解释器组件 | 一次 `act` |
| 交回载荷 | `return` 值与 `print` 缓冲，序列化后作为 `content` | `execute_result` 回包 | 单次回包 |

## 解释器

| 设置 | 取值 |
| --- | --- |
| 语言 | Starlark（`starlark` 0.14） |
| `enable_top_level_stmt` | `true` |
| `enable_load` | `false` |
| `enable_f_strings` | `false` |
| 内置扩展 | `Json`、`SetType`、`StructType`、`Map`、`Filter`、`Print` |
| 构建 | `wasm32-wasip2` 组件，内嵌 `starlark` 与 `regex` |

- 脚本的求值是确定的、有限步终止的，脚本自身不产生 effect；实际的运行时间由下面的中断约束。
- 解释器组件不持有任何宿主资源，只导入调用节点子工具分发的接口（[分发](tools.md#分发)）。
- Rust 标准库为 `wasm32-wasip2` 引入的 WASI 接口由组件内的本地适配满足：环境变量为空，时钟与非安全随机种子为固定值，终端查询返回无终端，其余操作陷入。组合后的组件不导入 WASI。
- 节点为解释器设置内存上限，并在 `remaining_ms` 耗尽或收到 `cancel` 时中断它（[组件运行时](tools.md#组件运行时)）。`starlark` 自带的堆上限与取消检查只是尽力而为，用于尽早返回可读的错误，不作为边界。

## `act` 的说明与全局环境

- `describe` 中 `act` 的 `description` 是节点由 API 工具声明与解释器内置类型渲染的紧凑 `.pyi` 类型存根，只随节点版本变化。存根区分只读目录类型 `ReadDir` 与可写目录类型 `WriteDir`，并含 `ExecEnv`、`Egress`、`TextView` 与各返回记录类型。
- 节点按配置向全局环境注入授权句柄，并在句柄对象上直接绑定对应的 API 工具：`lancet.read(...)` 等价于 `fs.read(lancet, ...)`，`dev.run(...)` 等价于 `proc.run(dev, ...)`。
- 全局环境中下表的内容不带句柄，没有 effect：

| 名称 | 内容 |
| --- | --- |
| 内置扩展 | `starlark` 的 `Json`、`SetType`、`StructType`、`Map`、`Filter` 与 `Print` |
| `sum` | 求和：对可迭代的数值按 Starlark 加法累加，空输入为 `0` |
| `regex` | `match(pattern, s)` 从开头匹配，返回布尔值；`findall(pattern, s)` 返回自左向右、不重叠的完整匹配；`replace(pattern, replacement, s)` 替换全部匹配，替换串可用 `$1`、`${name}`。无效的模式在求值时报错 |
| `text(s)` | 文本视图，基于 Rust `regex`：`text(s).grep(pattern, before = 0, after = 0).head(n).tail(n)`，拆行方法与字符串一致，为 `.splitlines()`；所有字符串结果都可按自左向右的顺序筛选。`grep` 按行匹配并带上前后文，重叠的上下文按原顺序合并、不重复；视图序列化为它的文本，选中的行保留原行结束符 |

- 内置函数 `commit()` 与 `discard()` 决定待定改动的去留，见[待定改动](#待定改动)；`discard()` 是对影子的写入，记入所在 `act` 的回执（[effect 与 `residue`](tools.md#effect-与-residue)）。
- 全局环境不含反射与宿主系统库：标准全局中的 `dir`、`getattr`、`hasattr` 与 `type` 不提供。
- `print` 经自定义 `PrintHandler` 写入本次 `act` 的内存缓冲，每次追加一行，不写节点的标准错误流。

## 执行流程

```text
act(script):
    把脚本包进单一入口函数
    if 解析失败:
        Failed(ScriptOrToolError)；常见的 Python 语法差异附一行修复提示
    对照本次注入的全局环境，做静态名称解析与类型检查
    if 检查不通过:
        Failed(ScriptOrToolError)              # 尚未发生任何子调用与 effect
    求值：脚本经句柄方法调用 API 工具
    结束时按“结局”一节确定 ExecuteOutcome
```

`Failed(X)` 指 `Failed { error: ToolError { code: X } }`。

- 包裹进单一入口函数后，脚本可以在顶层直接写 `return value` 交回结果；没写 `return` 时交回 `None`。包裹时多行字符串内部的行保持原样。解析、静态检查与运行期报错的行号都映射回原脚本。
- 静态检查在执行第一条语句之前拦下未定义的标识符；对类型可静态确定的值，还拦下参数名错误、字段名拼写错误，以及对 `ReadDir` 调用写方法。类型在静态检查时未知的值（例如经无类型的函数参数传入）上的这类错误，在求值到该处时立即失败（见[快速失败](#快速失败)）。
- 修复提示覆盖的语法差异：无括号生成器、`import`、`is None`、花括号集合推导。

## 快速失败

- 语言层不提供通用的 `try / except`。没有经显式窄参数容忍的错误立即展开调用栈，中止脚本。
- 可容忍的情形只在预期的探测点，以 API 工具的窄参数表达：`missing_ok`、`check` 与 `http.fetch` 的状态码字段，见[首批 API 工具](tools.md#首批-api-工具)。
- 脚本中止时，已做的写入作为待定改动保留。
- 需要先读文件内容、再推理修改方案时，模型分两次 `act`：第一次读取并筛出上下文，第二次修改并验证。

## 待定改动

`fs.*` 与 `proc.*` 的写入都落在影子工作区上，成为[待定改动](shadow-workspace.md#待定改动)。待定改动在 `act` 结束后原样保留，跨 `act` 累积，直到某次 `act` 的脚本调用 `commit()` 或 `discard()`。

| 脚本调用 | 效果 |
| --- | --- |
| `commit()` | 标记发布：脚本正常 `return` 且交回载荷不超过 `output_limit` 时，节点把全部待定改动作为一批[发布](shadow-workspace.md#发布)，在 `receipt_summary` 中填入发布的文件列表与裁剪后的 unified diff。可以在任意一次 `act` 中调用，包括写入发生的那一次（例如测试通过才调用） |
| `discard()` | 立即丢弃全部待定改动，影子恢复到基准；脚本继续执行，此后的写入成为新的待定改动 |
| `d.status()`、`d.diff()` | 查看目录 `d` 的待定改动（`fs.status`、`fs.diff`） |
| `d.reset(path)` | 丢弃单个路径的待定改动（`fs.reset`） |

- 模型看过结果再决定去留：下一次 `act` 可以只写一行 `commit()` 或 `discard()`，也可以继续修改。
- 脚本出错、被取消、超时或交回载荷超限时不发布，待定改动保留。
- `act` 结束时仍有待定改动的，节点在 `content` 末尾附一行提示，写明待定的文件数，提醒用 `commit()` 发布或用 `discard()` 丢弃。节点为这行提示预留固定的字节数，计入 `output_limit`；提示本身不会让结局变为 `OutputLimitExceeded`。

## 交回载荷与上限

| 上限 | 约束对象 | 超出时 |
| --- | --- | --- |
| 单次返回窗口（MiB 级，见[分发](tools.md#分发)） | 一次 API 工具交回脚本的数据 | 见 [API 工具与工具基座的尚未承接](tools.md#尚未承接) |
| `execute.output_limit`（如 64 KiB） | `return` 值与 `print` 缓冲序列化后的合计 | `Failed(OutputLimitExceeded)` |

`return` 值按 JSON 序列化，`None` 为 `null`；不能序列化的返回值按运行期错误处理。

## 结局

```text
act 结束时:
    if 脚本正常 return 且 (return 值 + print 缓冲) 序列化字节数 <= output_limit:
        if 脚本调用过 commit() 且存在待定改动:
            发布全部待定改动
            if 全部发布:       Succeeded { content, receipt_summary }   # 列出已发布的文件与 diff
            elif 一项也没发布: Failed(Conflict)                         # 列出冲突及原因，待定改动保留
            else:              Indeterminate { reason }                 # 撤回时宿主又改了已发布的文件
        else:                  Succeeded { content, receipt_summary }   # 仍有待定改动时 content 末尾附提示
    elif 序列化字节数 > output_limit:  Failed(OutputLimitExceeded)
    elif 脚本 fail 或子调用报错:       Failed(ScriptOrToolError)
    elif 调用方 cancel 或连接断开:     Cancelled
    elif remaining_ms 耗尽:           Deadline
# 发布之外的各分支都不写宿主工作目录，待定改动原样保留
```

## 示意

第一次 `act` 修改并运行测试，改动留在影子里待定：

```python
cfg = lancet.read("Cargo.toml", missing_ok = True)
if cfg == None:
    fail("missing Cargo.toml")

for hit in lancet.search("OldSymbol", glob = "crates/**/*.rs"):
    lancet.edit(hit.path, "OldSymbol", "NewSymbol")

res = dev.run(["cargo", "test", "--offline"], cwd = lancet, check = False)
return {
    "ok": res.code == 0,
    "failures": text(res.stdout + res.stderr).grep("FAILED|error\\[", after = 5).tail(40),
    "pending": lancet.status(),
}
```

模型看过结果后，第二次 `act` 决定发布或丢弃：

```python
commit()  # 或 discard()
```

## 保证

| 保证 | 适用范围 | 成立条件 | 依据或验证 |
| --- | --- | --- | --- |
| 解析或静态检查不通过的脚本不发生任何子调用与 effect | 每次 `act` | 检查在执行第一条语句之前完成 | [ADR 0002 第 3 节](../../adr/0002-lancet-node-api-tools-and-act.md#3-starlark-编排类型存根正交过滤与快速失败)；`crates/lancet-node/tests/act.rs::check_finishes_before_first_statement`、`parse_errors_map_lines_and_offer_repairs`（组件级；经 `execute` 的验证待 #5） |
| 只有调用过 `commit()` 并正常 `return`、载荷不超限的 `act` 写宿主工作目录 | 每次 `act` | 其余分支不发布 | ADR 0002 第 3 节、[第 6 节](../../adr/0002-lancet-node-api-tools-and-act.md#6-影子工作区待定改动与发布) |
| 出错、取消、超时与载荷超限的 `act` 保留全部待定改动 | 每次 `act` | 待定改动只由 `commit()` 后的发布、`discard()` 与 `reset(path)` 消除 | ADR 0002 第 3 节 |
| 待定提示不会让结局变为 `OutputLimitExceeded` | 每次 `act` | 提示的字节数预留并计入 `output_limit` | ADR 0002 第 3 节 |
| 解释器超出内存上限，或因 `remaining_ms` 耗尽、`cancel` 被中断时，只终止该实例，节点继续服务 | 每次 `act` | 组件运行时的内存上限与截止中断 | [ADR 0002 第 4 节](../../adr/0002-lancet-node-api-tools-and-act.md#4-边界由宿主资源承担)；`crates/lancet-node/tests/act.rs::memory_limit_terminates_only_its_store`、`deadline_interrupts_busy_store`、`cancel_interrupts_only_selected_store`（同一运行时上的解释器实例；`remaining_ms`、`cancel` 与断连经协议的接入待 #5 验证） |

## 范围之外

| 职责 | 现状 |
| --- | --- |
| 跨 `act` 的脚本挂起断点、每次 `fs.edit` 落盘前暂停确认 | 不提供；先读后改分两次 `act` |
| shell 命令字符串 | 不提供；`proc.*` 只接受参数数组 |
| 每次调用附带的动态工作区文件树 | 不提供；模型按需调用 `fs.list` 或 `fs.search` |

## 尚未承接

| 事项 | 现状 |
| --- | --- |
| 结局不是 `Succeeded`（没有 `content`）时，待定提示放在哪里；`output_limit` 小于提示的预留字节时怎样处理 | 待定，随 [#5](https://github.com/Skyline10124/lancet/issues/5)、[#7](https://github.com/Skyline10124/lancet/issues/7) 决定 |

## 验证

- `cargo xtask docs-check`：本合同的链接与锚点有效。
- `cargo test -p lancet-node --test act`：解释器的组件级测试，覆盖包裹与顶层 `return`、静态检查、解析诊断与修复提示、全局环境、内存上限、截止与取消。
- `act` 的协议级测试随 [#5](https://github.com/Skyline10124/lancet/issues/5) 建立，待定改动与结局的测试随 [#7](https://github.com/Skyline10124/lancet/issues/7) 建立；建立后写入保证表。
