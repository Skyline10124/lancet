---
status: active
---

# `act` 与 Starlark 编排

`act` 是 `lancet-node` 对模型可见的唯一工具。模型传入一段 Starlark 脚本，脚本持有授权句柄、组合调用 API 工具、对结果做纯计算筛选，再把需要进入模型上下文的数据交回。脚本默认遇错即止；可写目录的写入直接落在宿主工作目录上并立即生效。

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

- `describe` 中 `act` 的 `description` 是节点由 API 工具声明与解释器内置类型渲染的紧凑 `.pyi` 类型存根，只随节点版本变化。存根列出下表中的内置扩展、`sum`、`regex`、`text` 与 `TextView` 的签名，由解释器从注册的函数与类型生成；标准 Starlark 内置不列入。句柄类型（区分只读目录类型 `ReadDir` 与可写目录类型 `WriteDir`，以及 `ExecEnv`、`Egress`）、它们的方法与各返回记录类型由 API 工具声明渲染；没有 API 工具时，存根只含上述内置部分。
- 存根中由声明渲染的部分：
  - 工具用到的每个记录类型是一个类，字段带 WIT 注释。
  - 每种句柄类型是一个类，方法是以该类型为接收者的工具，签名去掉接收者，WIT 注释作为文档字符串。`WriteDir` 同时列出 `ReadDir` 的方法；需要 `ReadDir` 的参数写作 `ReadDir | WriteDir`。
  - 末尾连续的 `option` 参数写成 `= None`；返回类型 `result<T, string>` 写成 `T`。
- `act` 的 `arguments` 是只含字符串字段 `script` 的 JSON 对象；`input_schema` 不允许其他字段。
- `act` 的 `ToolSpec` 中 `effects` 与 `coeffects` 为空：脚本自身不产生、也不声明 effect 与 co-effect；每次子调用的 κ 与 ε 见[分发](tools.md#分发)。
- 节点按配置向全局环境注入授权句柄，并在句柄对象上直接绑定对应的 API 工具：`lancet.read(...)` 等价于 `fs.read(lancet, ...)`，`dev.run(...)` 等价于 `proc.run(dev, ...)`。
  - 句柄对象只带槽位编号与类型，不持有宿主资源；脚本不能构造句柄。
  - 方法名为工具的函数名，见[工具声明](tools.md#工具声明)。实参可按位置或关键字传递；末尾连续的 `option` 参数可以省略，省略即 `None`。
  - 方法的结果按声明转为 Starlark 值：记录转为字段可点取、可序列化为 JSON 的 `struct`，字段名中的连字符写作下划线；`none` 为 `None`。工具报错时，脚本在该调用处中止（[快速失败](#快速失败)）。
- 全局环境中下表的内容不带句柄，没有 effect：

| 名称 | 内容 |
| --- | --- |
| 内置扩展 | `starlark` 的 `Json`、`SetType`、`StructType`、`Map`、`Filter` 与 `Print` |
| `sum` | 求和：对可迭代的数值按 Starlark 加法累加，空输入为 `0` |
| `regex` | `match(pattern, s)` 从开头匹配，返回布尔值；`findall(pattern, s)` 返回自左向右、不重叠的完整匹配；`replace(pattern, replacement, s)` 替换全部匹配，替换串可用 `$1`、`${name}`。无效的模式在求值时报错 |
| `text(s)` | 文本视图，基于 Rust `regex`：`text(s).grep(pattern, before = 0, after = 0).head(n).tail(n)`，拆行方法与字符串一致，为 `.splitlines()`；所有字符串结果都可按自左向右的顺序筛选。`grep` 按行匹配并带上前后文，重叠的上下文按原顺序合并、不重复；视图序列化为它的文本，选中的行保留原行结束符 |

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
- 静态检查在执行第一条语句之前拦下未定义的标识符；对类型可静态确定的值，还拦下参数名错误、缺少参数、参数类型错误、字段名拼写错误、对 `ReadDir` 调用写方法，以及把 `ReadDir` 传给需要 `WriteDir` 的参数。类型在静态检查时未知的值（例如经无类型的函数参数传入）上的这类错误，在求值到该处时立即失败，不发出子调用（见[快速失败](#快速失败)）。
- 修复提示覆盖的语法差异：无括号生成器、`import`、`is None`、花括号集合推导。

## 快速失败

- 语言层不提供通用的 `try / except`。没有经显式窄参数容忍的错误立即展开调用栈，中止脚本。
- 可容忍的情形只在预期的探测点，以 API 工具的窄参数表达：`missing_ok`、`check` 与 `http.fetch` 的状态码字段，见[首批 API 工具](tools.md#首批-api-工具)。
- 运行中途中止（脚本失败、工具报错、取消、超时或交回载荷超限）时，已经完成的写入会保留在宿主工作目录上，并在回执中记为 `Write` / `NotRetained`。
- 需要先读文件内容、再推理修改方案时，模型分两次 `act`：第一次读取并筛出上下文，第二次修改并验证。

## 交回载荷与上限

| 上限 | 约束对象 | 超出时 |
| --- | --- | --- |
| 单次返回窗口（见[分发](tools.md#分发)） | 一次 API 工具交回脚本的成功值 | 该次调用报错，`Failed(ScriptOrToolError)` |
| `execute.output_limit`（如 64 KiB） | `return` 值与 `print` 缓冲序列化后的合计 | `Failed(OutputLimitExceeded)` |

`return` 值按 JSON 序列化，`None` 为 `null`；不能序列化的返回值按运行期错误处理。`content` 是对象 `{"value": <return 值>, "printed": <print 缓冲>}`，两个键始终存在；`output_limit` 按最终 `content` 的实际紧凑 UTF-8 JSON 字节数核对，等于上限时成功，超出时在受理之后结算为 `Failed(OutputLimitExceeded)`。

## 结局

```text
act 结束时:
    if 脚本正常 return 且 交回载荷不超过 output_limit:
        Succeeded { content, receipt_summary }
    elif 交回载荷超限:
        Failed(OutputLimitExceeded)
    elif 脚本 fail 或子调用报错:
        Failed(ScriptOrToolError)     # 含解释器或 API 工具超内存、工具陷入、proc.run 沙箱 OOM 与启动失败
    elif 调用方 cancel 或连接断开:
        Cancelled
    elif remaining_ms 耗尽:
        Deadline
```

节点自身的故障结算为 `Failed(InternalError)`，消息写明失败环节，例如解释器组件陷入、组件交回的值不是合法 JSON、沙箱挂载或叶组回收故障。重启前在途的调用不重新执行，按受理时保存的 `writes_possible` 结算：被授予可写工作目录或执行环境句柄的调用结算为 `Indeterminate`（原因为“节点在调用结算前重启，可能已产生 effect”），其余调用没有 effect，结算为 `Failed(InternalError)`，原因为“节点在调用结算前重启，未保存执行结果”。

`ScriptOrToolError` 的诊断消息超过 16,384 字节时，在不超过该长度的 UTF-8 字符边界截断，并注明已截断，使失败回复与回执都能装进一帧。

## 示意

修改文件并读回结果：

```python
cfg = lancet.read("Cargo.toml", missing_ok = True)
if cfg == None:
    fail("missing Cargo.toml")

lancet.edit("src/lib.rs", "OldSymbol", "NewSymbol")
updated = lancet.read("src/lib.rs")
return {
    "matches": text(updated).grep("NewSymbol", before = 1, after = 1),
}
```

## 保证

| 保证 | 适用范围 | 成立条件 | 依据或验证 |
| --- | --- | --- | --- |
| 解析或静态检查不通过的脚本不发生任何子调用与 effect | 每次 `act` | 检查在执行第一条语句之前完成 | [ADR 0002 第 3 节](../../adr/0002-lancet-node-api-tools-and-act.md#3-starlark-编排类型存根正交过滤与快速失败)；`crates/lancet-node/tests/act.rs::check_finishes_before_first_statement`、`parse_errors_map_lines_and_offer_repairs`、`checks_bound_methods_before_first_dispatch`；`crates/lancet-node/tests/node/calls.rs::execute_reports_script_diagnostics_before_evaluation` |
| 脚本只经授权句柄调用 API 工具；解释器交给分发层的句柄只是槽位编号 | 每次 API 工具调用 | 句柄对象由节点注入，脚本不能构造；分发接口是解释器唯一的导入 | `crates/lancet-node/tests/act.rs::bound_method_round_trips_arguments_records_and_errors`；`crates/lancet-node/tests/node/calls.rs::execute_runs_read_list_and_search` |
| 解释器超出内存上限，或因 `remaining_ms` 耗尽、`cancel` 被中断时，只终止该实例，节点继续服务 | 每次 `act` | 组件运行时的内存上限与截止中断 | [ADR 0002 第 4 节](../../adr/0002-lancet-node-api-tools-and-act.md#4-边界由宿主资源承担)；`crates/lancet-node/tests/act.rs::memory_limit_terminates_only_its_store`、`deadline_interrupts_busy_store`、`cancel_interrupts_only_selected_store`；`crates/lancet-node/tests/node/calls.rs::memory_limit_leaves_node_serving`、`deadline_settles_within_advertised_grace`、`cancel_multiplexes_with_receipt`、`disconnect_cancels_only_its_connection` |

## 范围之外

| 职责 | 现状 |
| --- | --- |
| 跨 `act` 的脚本挂起断点、每次 `fs.edit` 落盘前暂停确认 | 不提供；先读后改分两次 `act` |
| shell 命令字符串 | 不提供；`proc.*` 只接受参数数组 |
| 每次调用附带的动态工作区文件树 | 不提供；模型按需调用 `fs.list` 或 `fs.search` |

## 尚未承接

| 事项 | 现状 |
| --- | --- |
| 网络出口的句柄 | 随 [#9](https://github.com/Skyline10124/lancet/issues/9) 注入；当前全局环境注入工作目录与执行环境句柄 |

## 验证

- `cargo xtask docs-check`：本合同的链接与锚点有效。
- `cargo test -p lancet-node --test act`：解释器的组件级测试，覆盖包裹与顶层 `return`、静态检查、解析诊断与修复提示、全局环境、句柄方法的绑定与运行期校验、内存上限、截止与取消。
- `cargo test -p lancet-node --test node`：`act` 的协议级测试，覆盖受理、交回载荷与 `output_limit`、诊断、内存上限、截止、取消与断连，以及五个 `fs.*` API 工具与 `proc.run` 的调用、直接写入宿主、执行环境沙箱与回执。
