## 环境

| 项目 | 记录 |
| --- | --- |
| 实验 | ADR 0002 接受前实验 1 |
| 日期（UTC） | 2026-11-12 |
| 运行范围 | 24 任务 × 2 模式 × 2 模型 × 3 次 = 288；试跑 8 次单列 |
| 仓库基线 | 67674d308db81a015cf1cff60e2b9d40f0e7ff74 |
| 系统 | Linux-6.12.0-211.68.1+2.1.el10_2_ciq.x86_64-x86_64-with-glibc2.39 |
| Rust | rustc 1.96.0 (ac68faa20 2026-05-25) |
| Python | 3.12.15 (main, Oct  1 2026, 00:00:00) [GCC 14.3.1 20251022 (Red Hat 14.3.1-4)] |
| 解释器 | starlark = 0.14.2；Dialect::Standard + enable_top_level_stmt=true；enable_f_strings=false；Globals standard + Json |
| 二进制 SHA256 | 000cc7d05b05ecf20616e86a5148184fa61b2d22504930d2bd627bab963ef4d8 |
| 冻结清单 | evidence/freeze.json（151 个文件；SHA256） |
| 请求设置 | 默认温度；未指定采样参数；并发 4；每任务最多 4 次 act；429/5xx 最多 5 次尝试，等待 1/2/4/8 秒 |
| 运行顺序 | Random(20261112) 对完整运行列表 shuffle |
| 返回上限 | 序列化 JSON 的 UTF-8 长度 ≤ 65536 bytes |

| 文件 | Git blob SHA1 |
| --- | --- |
| prompts/system.md | a5a1cc470cd7814f9ed1865311f48721cebb3a75 |
| prompts/act_tool_A.md | f70ea7d493ed248d7e90cbe88d5cc9a579a48099 |
| prompts/act_tool_B.md | bdb0907ac9c9e7c65843514e141b6fc1d422e3c0 |

| 模型 | 模式 | 首次响应样本数 | 总输入 tokens 最小 | 总输入 tokens 最大 |
| --- | --- | --- | --- | --- |
| deepseek-flash | A | 72 | 1741 | 1775 |
| deepseek-flash | B | 72 | 1766 | 1800 |
| MiniMax-M3.1-Flash | A | 62 | 1808 | 1909 |
| MiniMax-M3.1-Flash | B | 66 | 1833 | 1934 |

| 项目 | 原始数字 |
| --- | --- |
| 正式记录起始 UTC | 2026-11-12 09:30:18 |
| 正式记录结束 UTC | 2026-11-12 09:50:17 |
| api_retry 记录数 | 668 |
| 全部正式 act 调用数 | 892 |
| api_error 运行中的 act 调用数 | 62 |

| 请求模型 | base URL | 响应 model 字段 | 正式运行数 | api_error | harness_error |
| --- | --- | --- | --- | --- | --- |
| deepseek-flash | https://api.deepseek.com | deepseek-flash | 144 | 0 | 0 |
| MiniMax-M3.1-Flash | https://api.minimaxi.com/v1 | MiniMax-M3.1-Flash | 144 | 44 | 0 |

| 统计项 | 口径 |
| --- | --- |
| 正式通过率分母 | 只包含 status=complete 的正式运行；api_error 整次运行及其 act 从通过率、均值和错误统计排除 |
| 表 1 | 解析通过 / act 调用数；运行通过 / act 调用数。discard 为运行通过；fail 为运行失败 |
| 表 2 | 每个任务重复一次为一次运行；最终文件与指定事实均符合冻结 check.py 才完成；首次完成按第一次 act 后的判定（仅一次 act 且最终回复补齐 fail 汇报也计入） |
| 表 4 | 下一次 act 的运行通过 / 存在下一次 act 的失败调用；最后一次失败无下一次调用单列；不要求任务完成 |
| 表 5 | 按任务运行计数；各列可重叠；discard 按执行事件，fail 按失败脚本与 fail 调用行的错误定位；正常提交错误按最终文件发生变化 |
| 表 6 | 按任务运行计数；all=True 按执行事件（包括尝试）；唯一匹配报错按原始错误；最终完成按 check.py |
| 原始记录 | results/formal 与 results/pilot；每次 API 响应、act 脚本/结局/判定及最终摘要；不含密钥 |

## 表 1：每次 act 调用（按 模型 × 模式）

| 模型 | 模式 | 调用数 | 解析通过率 | 运行通过率 |
| --- | --- | --- | --- | --- |
| deepseek-flash | A | 231 | 228/231 (98.70%) | 203/231 (87.88%) |
| deepseek-flash | B | 232 | 226/232 (97.41%) | 206/232 (88.79%) |
| MiniMax-M3.1-Flash | A | 175 | 171/175 (97.71%) | 153/175 (87.43%) |
| MiniMax-M3.1-Flash | B | 192 | 186/192 (96.88%) | 164/192 (85.42%) |

## 表 2：每个任务（按 模型 × 模式）

| 模型 | 模式 | 任务完成率 | 第一次 act 即完成的比例 | 平均 act 次数 |
| --- | --- | --- | --- | --- |
| deepseek-flash | A | 57/72 (79.17%) | 7/72 (9.72%) | 231/72 = 3.2083 |
| deepseek-flash | B | 60/72 (83.33%) | 12/72 (16.67%) | 232/72 = 3.2222 |
| MiniMax-M3.1-Flash | A | 27/48 (56.25%) | 8/48 (16.67%) | 175/48 = 3.6458 |
| MiniMax-M3.1-Flash | B | 25/52 (48.08%) | 8/52 (15.38%) | 192/52 = 3.6923 |

## 表 3：错误分类（按 模型 × 模式）

| 模型 | 模式 | Python 习惯 | 接口误用 | 唯一匹配失败 | 命令未预置 | 超出上限 | 其他 |
| --- | --- | --- | --- | --- | --- | --- | --- |
| deepseek-flash | A | 0 | 16 | 0 | 7 | 0 | 5 |
| deepseek-flash | B | 3 | 13 | 0 | 6 | 0 | 4 |
| MiniMax-M3.1-Flash | A | 3 | 4 | 0 | 9 | 0 | 6 |
| MiniMax-M3.1-Flash | B | 5 | 5 | 0 | 12 | 0 | 6 |

## 表 4：失败后下一次调用改对的比例（按错误分类）

| 错误分类 | 下次运行通过 | 存在下一次调用 | 比例 | 无下一次调用 |
| --- | --- | --- | --- | --- |
| Python 习惯 | 5 | 8 | 5/8 (62.50%) | 3 |
| 接口误用 | 33 | 34 | 33/34 (97.06%) | 4 |
| 唯一匹配失败 | 0 | 0 | 0/0 (—) | 0 |
| 命令未预置 | 16 | 18 | 16/18 (88.89%) | 16 |
| 超出上限 | 0 | 0 | 0/0 (—) | 0 |
| 其他 | 15 | 18 | 15/18 (83.33%) | 3 |

## 表 5：「失败就放弃」任务

| 模型 | 模式 | 运行数 | 用 discard | 用 fail | 正常返回（改动被错误提交） | 先提交后恢复 | 未运行测试 | 最终完成 |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| deepseek-flash | A | 12 | 2 | 0 | 1 | 1 | 4 | 7 |
| deepseek-flash | B | 12 | 2 | 0 | 5 | 2 | 1 | 5 |
| MiniMax-M3.1-Flash | A | 6 | 0 | 0 | 2 | 0 | 4 | 0 |
| MiniMax-M3.1-Flash | B | 8 | 0 | 0 | 2 | 0 | 6 | 0 |

## 表 6：批量替换任务

| 模型 | 模式 | 运行数 | 用 all=True | 撞上唯一匹配报错 | 最终完成 |
| --- | --- | --- | --- | --- | --- |
| deepseek-flash | A | 12 | 8 | 0 | 11 |
| deepseek-flash | B | 12 | 9 | 0 | 12 |
| MiniMax-M3.1-Flash | A | 8 | 3 | 0 | 3 |
| MiniMax-M3.1-Flash | B | 8 | 2 | 0 | 2 |

## 原始数字：冻结判定器限制

| 冻结检查 | 已知限制 | 处理 |
| --- | --- | --- |
| read-03 文件名与行数配对 | 只匹配文件名后紧接行数；wc -l 的反向排列及嵌套计数对象会造成假阴性 | 保持 check.py 和冻结统计；受影响原始记录单列 |

| run_id | 冻结判定 | 正确 wc 输出的 act | 原始输出 |
| --- | --- | --- | --- |
| deepseek-A-read-03-r1 | 0 | 3 |  3 data/a.txt<br> 4 data/b.txt<br> 7 total<br> |
| deepseek-A-read-03-r3 | 0 | 2 | 3 data/a.txt<br> 4 data/b.txt<br> 7 total |
| deepseek-B-read-03-r1 | 0 | 3 |  3 data/a.txt<br> 4 data/b.txt<br> 7 total<br> |
| deepseek-B-read-03-r3 | 0 | 3 |  3 data/a.txt<br> 4 data/b.txt<br> 7 total<br> |
| minimax-A-read-03-r1 | 0 | 3 |  3 data/a.txt<br> 4 data/b.txt<br> 7 total<br> |
| minimax-B-read-03-r3 | 0 | 4 |  3 data/a.txt<br> 4 data/b.txt<br> 7 total<br> |

## 原始数字：分类任务与试跑

| 模型 | 模式 | 类别 | 正式运行 | 完成 | 首次 act 完成 | act 数 |
| --- | --- | --- | --- | --- | --- | --- |
| deepseek-flash | A | read | 12 | 10 | 6 | 30 |
| deepseek-flash | A | edit | 12 | 12 | 0 | 30 |
| deepseek-flash | A | batch | 12 | 11 | 0 | 45 |
| deepseek-flash | A | test | 12 | 10 | 1 | 44 |
| deepseek-flash | A | rollback | 12 | 7 | 0 | 47 |
| deepseek-flash | A | probe | 12 | 7 | 0 | 35 |
| deepseek-flash | B | read | 12 | 10 | 7 | 29 |
| deepseek-flash | B | edit | 12 | 12 | 0 | 29 |
| deepseek-flash | B | batch | 12 | 12 | 0 | 42 |
| deepseek-flash | B | test | 12 | 12 | 4 | 47 |
| deepseek-flash | B | rollback | 12 | 5 | 0 | 48 |
| deepseek-flash | B | probe | 12 | 9 | 1 | 37 |
| MiniMax-M3.1-Flash | A | read | 11 | 8 | 6 | 34 |
| MiniMax-M3.1-Flash | A | edit | 9 | 7 | 0 | 32 |
| MiniMax-M3.1-Flash | A | batch | 8 | 3 | 0 | 31 |
| MiniMax-M3.1-Flash | A | test | 7 | 6 | 2 | 28 |
| MiniMax-M3.1-Flash | A | rollback | 6 | 0 | 0 | 24 |
| MiniMax-M3.1-Flash | A | probe | 7 | 3 | 0 | 26 |
| MiniMax-M3.1-Flash | B | read | 9 | 7 | 5 | 32 |
| MiniMax-M3.1-Flash | B | edit | 7 | 7 | 0 | 25 |
| MiniMax-M3.1-Flash | B | batch | 8 | 2 | 0 | 32 |
| MiniMax-M3.1-Flash | B | test | 10 | 8 | 3 | 39 |
| MiniMax-M3.1-Flash | B | rollback | 8 | 0 | 0 | 32 |
| MiniMax-M3.1-Flash | B | probe | 10 | 1 | 0 | 32 |

| 试跑 run_id | 状态 | 完成 | act 数 |
| --- | --- | --- | --- |
| deepseek-A-read-02-r1 | complete | 1 | 1 |
| deepseek-A-rollback-01-r1 | complete | 0 | 4 |
| deepseek-B-read-02-r1 | complete | 1 | 2 |
| deepseek-B-rollback-01-r1 | complete | 1 | 4 |
| minimax-A-read-02-r1 | complete | 1 | 4 |
| minimax-A-rollback-01-r1 | complete | 0 | 4 |
| minimax-B-read-02-r1 | complete | 0 | 4 |
| minimax-B-rollback-01-r1 | complete | 0 | 4 |

## 提示词事实修正

| 原文 | 修正 | 实测依据 |
| --- | --- | --- |
| \| recursion \| Not allowed. A function may not call itself. \| | \| recursion \| Allowed in this interpreter. \| | evidence/language-probes.jsonl: recursion（A/B） |
| \| `print(x)` \| Allowed, but the output is not shown to you. Return what you need. \| | \| `print(x)` \| Not available in this environment. Return what you need. \| | evidence/language-probes.jsonl: print（A/B） |

| 探针 | A：解析 / 运行 | B：解析 / 运行 | 结果或原始错误首行 |
| --- | --- | --- | --- |
| while | 0 / 0 | 0 / 0 | error: Parse error: cannot use reserved keyword `while` |
| try | 0 / 0 | 0 / 0 | error: Parse error: cannot use reserved keyword `try` |
| raise | 0 / 0 | 0 / 0 | error: Parse error: cannot use reserved keyword `raise` |
| import | 0 / 0 | 0 / 0 | error: Parse error: cannot use reserved keyword `import` |
| class | 0 / 0 | 0 / 0 | error: Parse error: cannot use reserved keyword `class` |
| with | 0 / 0 | 0 / 0 | error: Parse error: cannot use reserved keyword `with` |
| yield | 0 / 0 | 0 / 0 | error: Parse error: cannot use reserved keyword `yield` |
| global | 0 / 0 | 0 / 0 | error: Parse error: cannot use reserved keyword `global` |
| assert | 0 / 0 | 0 / 0 | error: Parse error: cannot use reserved keyword `assert` |
| f-string | 0 / 0 | 0 / 0 | error: Your Starlark dialect must enable f-strings to use them |
| is None | 0 / 0 | 0 / 0 | error: Parse error: cannot use reserved keyword `is` |
| string iteration | 1 / 0 | 1 / 0 | error: Operation `(iter)` not supported on type `string` |
| string elems | 1 / 1 | 1 / 1 | ["a", "b"] |
| chained comparison | 0 / 0 | 0 / 0 | error: Parse error: unexpected symbol '<', expected end of expression (comparisons are not associative in Starlark) |
| recursion | 1 / 1 | 1 / 1 | 0 |
| set | 1 / 0 | 1 / 0 | error: Variable `set` not found, did you mean `int`? |
| print | 1 / 0 | 1 / 0 | error: Variable `print` not found, did you mean `int`? |
| def | 1 / 1 | 1 / 1 | 3 |
| lambda | 1 / 1 | 1 / 1 | 3 |
| list comprehension | 1 / 1 | 1 / 1 | [2, 4] |
| dict comprehension | 1 / 1 | 1 / 1 | {"1": 2, "2": 4} |
| if elif else | 1 / 1 | 1 / 1 | 2 |
| for break continue | 1 / 1 | 1 / 1 | [0, 2] |
| len | 1 / 1 | 1 / 1 | 2 |
| sorted | 1 / 1 | 1 / 1 | [1, 2, 3] |
| enumerate | 1 / 1 | 1 / 1 | [[0, "a"], [1, "b"]] |
| zip | 1 / 1 | 1 / 1 | [[1, 3], [2, 4]] |
| min | 1 / 1 | 1 / 1 | 1 |
| max | 1 / 1 | 1 / 1 | 3 |
| any | 1 / 1 | 1 / 1 | true |
| all | 1 / 1 | 1 / 1 | false |
| format | 1 / 1 | 1 / 1 | "x:2" |
| percent format | 1 / 1 | 1 / 1 | "x: 2" |
| string methods | 1 / 1 | 1 / 1 | ["x", ["a", "b"], ["a", "b"], "cb", true, true, "a-b"] |
| json | 1 / 1 | 1 / 1 | {"x": [1, true, null]} |

## 测试台与方案的偏差

| 项 | 实际行为 |
| --- | --- |
| 句柄 | 按覆盖指令使用 repo.*；无 lancet 全局 |
| 第 0 步 | 取消人工停顿；语义自动化测试、48 个任务参考用例与试跑后冻结 |
| 返回值读取 | A 保持指定 def/赋值包装；eval 完成后单独求值私有 __act_result__ 以读取返回值；B 的值保存在 Module.extra_value |
| reset/status/diff 基线 | 按提示词草稿以本次 act 开始的文件状态为基线；方案接口表写“任务初始状态” |
| text | 保留自定义 text(s) 与链式方法；过滤结果以换行连接，不保留末尾换行 |
| 命令输出 | 预置 canned.json 按 argv 精确匹配且不随文件变化；真实管道仅返回最后阶段的 code/stdout/stderr |
| 预置日志计数 | canned.stdout 首行固定 running 4 tests，后续实际列出的用例为 2 或 3；任务判定使用明确的失败名和原因，不使用 running 数字 |
| 提示词示例 | 按草稿，B 示例去掉 A 的“无结果提前 return”分支；除返回段和示例外 A/B 正文相同 |
| 提示词顺序与长度 | 按用户指定草稿保留 Outcome→Returning→Repository→Commands→Text→语言差异→Example；长度以首次 API 响应 usage.prompt_tokens 核验，含 system/任务/工具 schema 的总输入小于 2500；见 evidence/prompt-tokens.json |
| 密钥配置 | 按覆盖指令从两个密钥文件运行时读取，strip 后 rstrip("~")；未使用 SUBJECT_API_KEY 环境变量；文件未修改 |
| 终止条件 | 无工具调用的模型回复即结束；最多 4 次 act 后再取一条模型消息供最终汇报，若仍请求工具则停止；API 请求超时 180 秒，act 子进程超时 60 秒 |
| tool_calls | 同一条 assistant 的多个工具调用顺序执行，计入同一任务的 4 次上限；超出部分仅回传预算耗尽 |
| 任务判定 | check.py 对文件做全文精确比较，对事实做固定词/数字检查；两个 JSON 任务额外核验结构、排序、数字及布尔；edit 类验证先前 act 读取时序；rollback 类核验测试时的候选文件 |
| 试跑前后修复 | 修复 A 私有变量读取及堆栈行号换算；参考判定的 rollback 常量替换改为 = n;，避免把 u32 的 2 一并改掉；未改任务正文或提示词来适配模型 |
| 范围 | 未实现 http.fetch、沙箱、overlay、内存窗口；仅在临时 fixture 副本执行命令 |

## 验证

| 命令 | 结果 |
| --- | --- |
| cargo xtask docs-check | 通过；evidence/docs-check.log（最终暂存后执行） |
| cargo build --manifest-path prototypes/starlark-pass-rate/Cargo.toml -j 4 | 通过；evidence/build-final.log |
| python3 -m unittest discover -s prototypes/starlark-pass-rate/tests | 9 个测试通过；48 个参考用例；evidence/tests.log |
| python3 prototypes/starlark-pass-rate/language_probe.py | 70 条探针记录 |
| python3 prototypes/starlark-pass-rate/driver.py --phase pilot --workers 4 | 8 条运行摘要 |
| python3 prototypes/starlark-pass-rate/driver.py --phase formal --workers 4 | 288 条运行摘要 |
| python3 prototypes/starlark-pass-rate/validate.py | {"runs": 288, "calls": 892, "grid_complete": true, "frozen_files": 151, "replay_state_or_check_mismatches": 0, "replay_value_differences": 7, "status_counts": {"complete": 244, "api_error": 44}} |
| 返回值差异核验 | 7 处均仅为真实 ls 的时间戳及父目录链接计数；evidence/replay-value-differences.json 与 replay-difference-audit.json |
| 未覆盖 | 真实命令沙箱与真实 cargo test；正式模型的内部版本与服务端采样默认值无法锁定；未运行根工作区全量 CI（原型独立 workspace） |

## 阻塞项

| 命令 / run_id | 原始报错 |
| --- | --- |
| python3 -m pip --version; python3 -m pip install --user tiktoken | /usr/bin/python3: No module named pip |
| driver.py --phase formal --workers 4 / minimax-A-batch-01-r2 | {"type":"error","error":{"type":"rate_limit_error","message":"rate limit exceeded(RPM) (1002)","http_code":"429"},"request_id":"0744bcb58d95d60ce65bb2b20373b779"}<br> API 请求最多 5 次尝试后仍失败 |
| driver.py --phase formal --workers 4 / minimax-A-batch-02-r1 | {"type":"error","error":{"type":"rate_limit_error","message":"rate limit exceeded(RPM) (1002)","http_code":"429"},"request_id":"0744ba92a76a7d2dcea81c1762d66608"}<br> API 请求最多 5 次尝试后仍失败 |
| driver.py --phase formal --workers 4 / minimax-A-batch-02-r3 | {"type":"error","error":{"type":"rate_limit_error","message":"rate limit exceeded(RPM) (1002)","http_code":"429"},"request_id":"0744bc6056e1b477b8f42555a0903699"}<br> API 请求最多 5 次尝试后仍失败 |
| driver.py --phase formal --workers 4 / minimax-A-batch-04-r3 | {"type":"error","error":{"type":"rate_limit_error","message":"rate limit exceeded(RPM) (1002)","http_code":"429"},"request_id":"0744be8e6001a2e6140c59ee0f321261"}<br> API 请求最多 5 次尝试后仍失败 |
| driver.py --phase formal --workers 4 / minimax-A-edit-02-r1 | {"type":"error","error":{"type":"rate_limit_error","message":"rate limit exceeded(RPM) (1002)","http_code":"429"},"request_id":"0744bb763ba9c0d21981823d1a2ffdd3"}<br> API 请求最多 5 次尝试后仍失败 |
| driver.py --phase formal --workers 4 / minimax-A-edit-02-r3 | {"type":"error","error":{"type":"rate_limit_error","message":"rate limit exceeded(RPM) (1002)","http_code":"429"},"request_id":"0744be4cb19d64b96151119416315e38"}<br> API 请求最多 5 次尝试后仍失败 |
| driver.py --phase formal --workers 4 / minimax-A-edit-03-r1 | {"type":"error","error":{"type":"rate_limit_error","message":"rate limit exceeded(RPM) (1002)","http_code":"429"},"request_id":"0744bdf8081f6cc6ddb742c9772ce42d"}<br> API 请求最多 5 次尝试后仍失败 |
| driver.py --phase formal --workers 4 / minimax-A-probe-01-r1 | {"type":"error","error":{"type":"rate_limit_error","message":"rate limit exceeded(RPM) (1002)","http_code":"429"},"request_id":"0744be4719b4ac444891958ac7becf92"}<br> API 请求最多 5 次尝试后仍失败 |
| driver.py --phase formal --workers 4 / minimax-A-probe-01-r3 | {"type":"error","error":{"type":"rate_limit_error","message":"rate limit exceeded(RPM) (1002)","http_code":"429"},"request_id":"0744bc09a93d0dc4b4ce1ecdd4b9c2c2"}<br> API 请求最多 5 次尝试后仍失败 |
| driver.py --phase formal --workers 4 / minimax-A-probe-02-r3 | {"type":"error","error":{"type":"rate_limit_error","message":"rate limit exceeded(RPM) (1002)","http_code":"429"},"request_id":"0744bd5c1d1b4588ddd79b57587670a0"}<br> API 请求最多 5 次尝试后仍失败 |
| driver.py --phase formal --workers 4 / minimax-A-probe-03-r1 | {"type":"error","error":{"type":"rate_limit_error","message":"rate limit exceeded(RPM) (1002)","http_code":"429"},"request_id":"0744be8eb73412cc413aa339d6620172"}<br> API 请求最多 5 次尝试后仍失败 |
| driver.py --phase formal --workers 4 / minimax-A-probe-04-r2 | {"type":"error","error":{"type":"rate_limit_error","message":"rate limit exceeded(RPM) (1002)","http_code":"429"},"request_id":"0744bdd9c90799791bc4228a7b45bd96"}<br> API 请求最多 5 次尝试后仍失败 |
| driver.py --phase formal --workers 4 / minimax-A-read-02-r1 | {"type":"error","error":{"type":"rate_limit_error","message":"rate limit exceeded(RPM) (1002)","http_code":"429"},"request_id":"0744be0b0a322da91d01b01eacc82250"}<br> API 请求最多 5 次尝试后仍失败 |
| driver.py --phase formal --workers 4 / minimax-A-rollback-01-r1 | {"type":"error","error":{"type":"rate_limit_error","message":"rate limit exceeded(RPM) (1002)","http_code":"429"},"request_id":"0744be64418cd1f87be9a92303e4671f"}<br> API 请求最多 5 次尝试后仍失败 |
| driver.py --phase formal --workers 4 / minimax-A-rollback-01-r3 | {"type":"error","error":{"type":"rate_limit_error","message":"rate limit exceeded(RPM) (1002)","http_code":"429"},"request_id":"0744bd084237bf5a741f8ed787e5bd92"}<br> API 请求最多 5 次尝试后仍失败 |
| driver.py --phase formal --workers 4 / minimax-A-rollback-02-r3 | {"type":"error","error":{"type":"rate_limit_error","message":"rate limit exceeded(RPM) (1002)","http_code":"429"},"request_id":"0744bbf5876f37c45494a5c2bf84e306"}<br> API 请求最多 5 次尝试后仍失败 |
| driver.py --phase formal --workers 4 / minimax-A-rollback-03-r1 | {"type":"error","error":{"type":"rate_limit_error","message":"rate limit exceeded(RPM) (1002)","http_code":"429"},"request_id":"0744bc737f19ff634d403e52a5e04b8d"}<br> API 请求最多 5 次尝试后仍失败 |
| driver.py --phase formal --workers 4 / minimax-A-rollback-03-r3 | {"type":"error","error":{"type":"rate_limit_error","message":"rate limit exceeded(RPM) (1002)","http_code":"429"},"request_id":"0744be22d98ddcc8e3d72d87a9f48dd3"}<br> API 请求最多 5 次尝试后仍失败 |
| driver.py --phase formal --workers 4 / minimax-A-rollback-04-r1 | {"type":"error","error":{"type":"rate_limit_error","message":"rate limit exceeded(RPM) (1002)","http_code":"429"},"request_id":"0744bb92eb24d121c4726b8fd41cfd16"}<br> API 请求最多 5 次尝试后仍失败 |
| driver.py --phase formal --workers 4 / minimax-A-test-01-r1 | {"type":"error","error":{"type":"rate_limit_error","message":"rate limit exceeded(RPM) (1002)","http_code":"429"},"request_id":"0744bd691e4aa5ea40f726eae50c4460"}<br> API 请求最多 5 次尝试后仍失败 |
| driver.py --phase formal --workers 4 / minimax-A-test-02-r1 | {"type":"error","error":{"type":"rate_limit_error","message":"rate limit exceeded(RPM) (1002)","http_code":"429"},"request_id":"0744bc05a92248e661db742f3327a5a6"}<br> API 请求最多 5 次尝试后仍失败 |
| driver.py --phase formal --workers 4 / minimax-A-test-03-r1 | {"type":"error","error":{"type":"rate_limit_error","message":"rate limit exceeded(RPM) (1002)","http_code":"429"},"request_id":"0744bc3b5bab808c6cfe230890f5b27d"}<br> API 请求最多 5 次尝试后仍失败 |
| driver.py --phase formal --workers 4 / minimax-A-test-03-r2 | {"type":"error","error":{"type":"rate_limit_error","message":"rate limit exceeded(RPM) (1002)","http_code":"429"},"request_id":"0744be6eae21379b19ae0c9e3fb3c9d3"}<br> API 请求最多 5 次尝试后仍失败 |
| driver.py --phase formal --workers 4 / minimax-A-test-03-r3 | {"type":"error","error":{"type":"rate_limit_error","message":"rate limit exceeded(RPM) (1002)","http_code":"429"},"request_id":"0744bd45fa235d0cc56c78b17e831655"}<br> API 请求最多 5 次尝试后仍失败 |
| driver.py --phase formal --workers 4 / minimax-B-batch-02-r1 | {"type":"error","error":{"type":"rate_limit_error","message":"rate limit exceeded(RPM) (1002)","http_code":"429"},"request_id":"0744bc5171f435e84343481cff64e0a6"}<br> API 请求最多 5 次尝试后仍失败 |
| driver.py --phase formal --workers 4 / minimax-B-batch-02-r2 | {"type":"error","error":{"type":"rate_limit_error","message":"rate limit exceeded(RPM) (1002)","http_code":"429"},"request_id":"0744bea4d10e5b3a9449d0a0db0fe9e9"}<br> API 请求最多 5 次尝试后仍失败 |
| driver.py --phase formal --workers 4 / minimax-B-batch-03-r3 | {"type":"error","error":{"type":"rate_limit_error","message":"rate limit exceeded(RPM) (1002)","http_code":"429"},"request_id":"0744bceb4c3c95d317944c3b355b92e3"}<br> API 请求最多 5 次尝试后仍失败 |
| driver.py --phase formal --workers 4 / minimax-B-batch-04-r3 | {"type":"error","error":{"type":"rate_limit_error","message":"rate limit exceeded(RPM) (1002)","http_code":"429"},"request_id":"0744bae4b1eca6aa8f9e6870f148952c"}<br> API 请求最多 5 次尝试后仍失败 |
| driver.py --phase formal --workers 4 / minimax-B-edit-01-r3 | {"type":"error","error":{"type":"rate_limit_error","message":"rate limit exceeded(RPM) (1002)","http_code":"429"},"request_id":"0744bda13bb0c3a8832fe3671dc7cbe9"}<br> API 请求最多 5 次尝试后仍失败 |
| driver.py --phase formal --workers 4 / minimax-B-edit-02-r2 | {"type":"error","error":{"type":"rate_limit_error","message":"rate limit exceeded(RPM) (1002)","http_code":"429"},"request_id":"0744bbbc7b348135ac08d5d0d678c476"}<br> API 请求最多 5 次尝试后仍失败 |
| driver.py --phase formal --workers 4 / minimax-B-edit-03-r1 | {"type":"error","error":{"type":"rate_limit_error","message":"rate limit exceeded(RPM) (1002)","http_code":"429"},"request_id":"0744bba74c1d0bbcdb2ba03d3a93ab13"}<br> API 请求最多 5 次尝试后仍失败 |
| driver.py --phase formal --workers 4 / minimax-B-edit-04-r2 | {"type":"error","error":{"type":"rate_limit_error","message":"rate limit exceeded(RPM) (1002)","http_code":"429"},"request_id":"0744bd2f2e81e6fa2c7158074da93fbd"}<br> API 请求最多 5 次尝试后仍失败 |
| driver.py --phase formal --workers 4 / minimax-B-edit-04-r3 | {"type":"error","error":{"type":"rate_limit_error","message":"rate limit exceeded(RPM) (1002)","http_code":"429"},"request_id":"0744bd405e8c7371505ac8cb2b208578"}<br> API 请求最多 5 次尝试后仍失败 |
| driver.py --phase formal --workers 4 / minimax-B-probe-02-r3 | {"type":"error","error":{"type":"rate_limit_error","message":"rate limit exceeded(RPM) (1002)","http_code":"429"},"request_id":"0744be4a29a2a804795f015c45c2a747"}<br> API 请求最多 5 次尝试后仍失败 |
| driver.py --phase formal --workers 4 / minimax-B-probe-03-r1 | {"type":"error","error":{"type":"rate_limit_error","message":"rate limit exceeded(RPM) (1002)","http_code":"429"},"request_id":"0744bb583b1c189e5dbd2d39250000e0"}<br> API 请求最多 5 次尝试后仍失败 |
| driver.py --phase formal --workers 4 / minimax-B-read-02-r1 | {"type":"error","error":{"type":"rate_limit_error","message":"rate limit exceeded(RPM) (1002)","http_code":"429"},"request_id":"0744bb7119de2da87091179b35684a09"}<br> API 请求最多 5 次尝试后仍失败 |
| driver.py --phase formal --workers 4 / minimax-B-read-03-r2 | {"type":"error","error":{"type":"rate_limit_error","message":"rate limit exceeded(RPM) (1002)","http_code":"429"},"request_id":"0744bbb2b2c2adc7f138af5c87848cd1"}<br> API 请求最多 5 次尝试后仍失败 |
| driver.py --phase formal --workers 4 / minimax-B-read-04-r3 | {"type":"error","error":{"type":"rate_limit_error","message":"rate limit exceeded(RPM) (1002)","http_code":"429"},"request_id":"0744baeabea0e12e85f2a5a485fa2b26"}<br> API 请求最多 5 次尝试后仍失败 |
| driver.py --phase formal --workers 4 / minimax-B-rollback-01-r1 | {"type":"error","error":{"type":"rate_limit_error","message":"rate limit exceeded(RPM) (1002)","http_code":"429"},"request_id":"0744bd07842e93f3b75d4af0936ea4df"}<br> API 请求最多 5 次尝试后仍失败 |
| driver.py --phase formal --workers 4 / minimax-B-rollback-01-r2 | {"type":"error","error":{"type":"rate_limit_error","message":"rate limit exceeded(RPM) (1002)","http_code":"429"},"request_id":"0744bc271a263b765eef8488eb672ac4"}<br> API 请求最多 5 次尝试后仍失败 |
| driver.py --phase formal --workers 4 / minimax-B-rollback-02-r2 | {"type":"error","error":{"type":"rate_limit_error","message":"rate limit exceeded(RPM) (1002)","http_code":"429"},"request_id":"0744ba6b9ea96c0aa251ae618220a550"}<br> API 请求最多 5 次尝试后仍失败 |
| driver.py --phase formal --workers 4 / minimax-B-rollback-04-r2 | {"type":"error","error":{"type":"rate_limit_error","message":"rate limit exceeded(RPM) (1002)","http_code":"429"},"request_id":"0744bb0eed74a85d44e4e11ce65ae98d"}<br> API 请求最多 5 次尝试后仍失败 |
| driver.py --phase formal --workers 4 / minimax-B-test-03-r2 | {"type":"error","error":{"type":"rate_limit_error","message":"rate limit exceeded(RPM) (1002)","http_code":"429"},"request_id":"0744be8957b83e4d5e2b28211f1278d2"}<br> API 请求最多 5 次尝试后仍失败 |
| driver.py --phase formal --workers 4 / minimax-B-test-03-r3 | {"type":"error","error":{"type":"rate_limit_error","message":"rate limit exceeded(RPM) (1002)","http_code":"429"},"request_id":"0744bb921199863143bfa157d7f1f656"}<br> API 请求最多 5 次尝试后仍失败 |

## 附录：每个错误分类各附 3 个脚本原文

### Python 习惯

| 正式错误调用总数 | 附录脚本数 |
| --- | --- |
| 11 | 3 |

| run_id | act | 原始错误 |
| --- | --- | --- |
| deepseek-B-probe-03-r3 | 4 | error: Parse error: cannot use reserved keyword `from`<br> --> script.star:4:1<br>  \|<br>4 \| from collections import Counter<br>  \| ^^^^<br>  \|<br> |

````python
out = env.run(["sed", "-n", "2401p;3602p", "logs/large.log"], check=False)
files = repo.list("logs")
levels = env.run(["awk", "-F", " ", "{print $1}", "logs/large.log"], check=False)
from collections import Counter
result({"lines": out.stdout, "logs_dir": files, "levels_sample": text(levels.stdout).lines()[:5]})
````

| run_id | act | 原始错误 |
| --- | --- | --- |
| deepseek-B-test-01-r2 | 1 | error: `return` cannot be used outside of a `def` function<br> --> script.star:1:1<br>  \|<br>1 \| return repo.list(".")<br>  \| ^^^^^^^^^^^^^^^^^^^^^<br>  \|<br> |

````python
return repo.list(".")
````

| run_id | act | 原始错误 |
| --- | --- | --- |
| deepseek-B-test-01-r2 | 2 | error: `return` cannot be used outside of a `def` function<br> --> script.star:2:1<br>  \|<br>2 \| return {"code": r.code, "stdout_tail": r.stdout[-4000:], "stderr_tail": r.stderr[-4000:]}<br>  \| ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^<br>  \|<br> |

````python
r = env.run(["cargo", "test"], check = False)
return {"code": r.code, "stdout_tail": r.stdout[-4000:], "stderr_tail": r.stderr[-4000:]}
````

### 接口误用

| 正式错误调用总数 | 附录脚本数 |
| --- | --- |
| 38 | 3 |

| run_id | act | 原始错误 |
| --- | --- | --- |
| deepseek-A-batch-03-r1 | 1 | error: Variable `print` not found, did you mean `int`?<br> --> script.star:3:9<br>  \|<br>3 \|         print("%s:%d: %s" % (h.path, h.line, h.text))<br>  \|         ^^^^^<br>  \|<br> |

````python
hits = repo.search("slow_path", glob = "src/**")
for h in hits:
    print("%s:%d: %s" % (h.path, h.line, h.text))
return len(hits)
````

| run_id | act | 原始错误 |
| --- | --- | --- |
| deepseek-A-edit-03-r1 | 1 | Operation `serde::serialize` not supported on type `text_view` |

````python
c = repo.read("README.md")
return text(c).grep("endpoint|/v1/|/v2/|/query|curl|http", before = 2, after = 2)

````

| run_id | act | 原始错误 |
| --- | --- | --- |
| deepseek-A-probe-02-r2 | 2 | error: Variable `sum` not found, did you mean `str`?<br> --> script.star:4:13<br>  \|<br>4 \|     total = sum([s["port"] for s in enabled])<br>  \|             ^^^<br>  \|<br> |

````python
data = json.decode(repo.read("services.json"))
enabled = [s for s in data["services"] if s.get("enabled") == True]
names = sorted([s["name"] for s in enabled])
total = sum([s["port"] for s in enabled])
return {"enabled_services": names, "total_port_sum": total, "excluded": [s["name"] for s in data["services"] if s.get("enabled") != True]}
````

### 唯一匹配失败

| 正式错误调用总数 | 附录脚本数 |
| --- | --- |
| 0 | 0 |

### 命令未预置

| 正式错误调用总数 | 附录脚本数 |
| --- | --- |
| 34 | 3 |

| run_id | act | 原始错误 |
| --- | --- | --- |
| deepseek-A-rollback-03-r1 | 4 | Traceback (most recent call last):<br>  * script.star:4, in <module><br>      __act_result__ = __act_main__()<br>  * script.star:1, in __act_main__<br>      cargo = env.run(["cargo", "--version"], check=False)<br>error: 模拟环境未提供该命令: ["cargo", "--version"]<br> --> script.star:1:13<br>  \|<br>1 \|     cargo = env.run(["cargo", "--version"], check=False)<br>  \|             ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^<br>  \|<br> |

````python
cargo = env.run(["cargo", "--version"], check=False)
rustc = env.run(["rustc", "--version"], check=False)
return {"cargo": cargo.code, "cargo_out": cargo.stdout.strip(), "cargo_err": cargo.stderr.strip()[:300],
        "rustc": rustc.code, "rustc_out": rustc.stdout.strip(), "rustc_err": rustc.stderr.strip()[:300]}
````

| run_id | act | 原始错误 |
| --- | --- | --- |
| deepseek-A-test-01-r1 | 1 | Traceback (most recent call last):<br>  * script.star:2, in <module><br>      __act_result__ = __act_main__()<br>  * script.star:1, in __act_main__<br>      r = env.run(["cargo", "test", "--no-fail-fast"], check = False)<br>error: 模拟环境未提供该命令: ["cargo", "test", "--no-fail-fast"]<br> --> script.star:1:9<br>  \|<br>1 \|     r = env.run(["cargo", "test", "--no-fail-fast"], check = False)<br>  \|         ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^<br>  \|<br> |

````python
r = env.run(["cargo", "test", "--no-fail-fast"], check = False)
return {"code": r.code, "stdout_tail": text(r.stdout).tail(150), "stderr_tail": text(r.stderr).tail(60)}
````

| run_id | act | 原始错误 |
| --- | --- | --- |
| deepseek-A-test-01-r1 | 3 | Traceback (most recent call last):<br>  * script.star:4, in <module><br>      __act_result__ = __act_main__()<br>  * script.star:2, in __act_main__<br>      r = env.run(c, check = False)<br>error: 模拟环境未提供该命令: ["cargo", "--version"]<br> --> script.star:2:13<br>  \|<br>2 \|         r = env.run(c, check = False)<br>  \|             ^^^^^^^^^^^^^^^^^^^^^^^^^<br>  \|<br> |

````python
for c in [["cargo", "--version"], ["ls"], ["sh", "-c", "echo hi"]]:
    r = env.run(c, check = False)
    results = results + [] if False else None
return "placeholder"
````

### 超出上限

| 正式错误调用总数 | 附录脚本数 |
| --- | --- |
| 0 | 0 |

### 其他

| 正式错误调用总数 | 附录脚本数 |
| --- | --- |
| 21 | 3 |

| run_id | act | 原始错误 |
| --- | --- | --- |
| deepseek-A-batch-02-r1 | 1 | error: Parse error: unexpected keyword 'for', expected symbol ':'<br> --> script.star:4:31<br>  \|<br>4 \|       "files": sorted({h.path for h in hits})[:50],<br>  \|                               ^^^<br>  \|<br> |

````python
hits = repo.search("legacy_flag", glob = "**")
return {
  "count": len(hits),
  "files": sorted({h.path for h in hits})[:50],
  "by_file": {p: len([h for h in hits if h.path == p]) for p in {h.path for h in hits}},
}

````

| run_id | act | 原始错误 |
| --- | --- | --- |
| deepseek-A-probe-01-r1 | 2 | error: Parse error: unexpected keyword 'for', expected symbol ':'<br> --> script.star:3:42<br>  \|<br>3 \|         "optional_paths": sorted({h.path for h in hits}),<br>  \|                                          ^^^<br>  \|<br> |

````python
hits = repo.search("optional", glob = "**")
return {
    "optional_paths": sorted({h.path for h in hits}),
    "files_named_optional": [h.path for h in hits if h.path.endswith("optional.json")],
    "status": [{"path": s.path, "kind": s.kind} for s in repo.status()],
}
````

| run_id | act | 原始错误 |
| --- | --- | --- |
| deepseek-A-read-01-r2 | 2 | error: Parse error: unexpected keyword 'for', expected symbol ':'<br> --> script.star:3:28<br>  \|<br>3 \|     files = sorted({h.path for h in all_hits})<br>  \|                            ^^^<br>  \|<br> |

````python
all_hits = repo.search("(?i)todo", glob = "**")
nested = [{"path": h.path, "line": h.line, "text": h.text.strip()} for h in all_hits if h.path.startswith("src/")]
files = sorted({h.path for h in all_hits})
return {"in_src": nested, "all_files_with_todo": files}
````
