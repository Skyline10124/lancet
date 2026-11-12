## 环境

| 项目 | 记录 |
| --- | --- |
| 实验 | ADR 0002 接受前实验 1 · v2 |
| 日期（UTC） | 2026-11-12 |
| 规模 | 3 组 × 2 模式 × 2 模型 × 24 任务 × 3 次 = 864；试跑 24 次单列 |
| 仓库基线 | 547aabe77b4ef5380ca4a16d00f01779d700b45e |
| 系统 | Linux-6.12.0-211.68.1+2.1.el10_2_ciq.x86_64-x86_64-with-glibc2.39 |
| Rust | rustc 1.96.0 (ac68faa20 2026-05-25) |
| Python | 3.12.15 (main, Oct  1 2026, 00:00:00) [GCC 14.3.1 20251022 (Red Hat 14.3.1-4)] |
| 解释器 | starlark = 0.14.2；Dialect::Standard + enable_top_level_stmt=true；enable_f_strings=false；Globals standard + Json |
| 二进制 SHA256 | 52cc86de3b84f283ae33abc3c41dd5a2e849420a6d55c14ce420fe8fae4df56c |
| 冻结清单 | results/v2/evidence/freeze.json；输入 153 文件；测试台 11 文件 |
| 冻结 UTC | 2026-11-12 15:15:49 |
| 请求设置 | 默认温度，未指定采样参数；MiniMax 并发 1，deepseek 并发 4；请求超时 180 秒，act 子进程超时 60 秒 |
| 重试 | MiniMax 429/5xx/URLError/超时/响应格式错误最多 10 次，等待 1/2/4/8/16/32/64/120/120 秒；deepseek 最多 5 次，等待 1/2/4/8 秒 |
| 预算 | 最多 8 次 act；耗尽后再取一条消息供最终汇报，若仍请求工具则停止；无工具调用的回复即结束 |
| 运行顺序 | Random(20261112) 对完整列表 shuffle；每模型独立线程池按其子序列调度 |
| 返回上限 | 序列化 JSON 的 UTF-8 长度 ≤ 65536 bytes |
| 恢复执行 | Codex 用量上限中断后恢复；原驱动继续运行，已核验试跑/冻结；删除不完整记录 0，因中断重跑 0；见 results/v2/evidence/resume-audit.json |
| 原始记录 | results/v2/formal 与 results/v2/pilot；脚本、API 响应、usage、调用事件、判定、最终文件哈希 |

| 文件 | Git blob SHA1 |
| --- | --- |
| prompts/act_tool_A.md | f70ea7d493ed248d7e90cbe88d5cc9a579a48099 |
| prompts/act_tool_A_composed.md | 53c795152f03e3341e90fda269ee11e2d5a4eb8f |
| prompts/act_tool_B.md | bdb0907ac9c9e7c65843514e141b6fc1d422e3c0 |
| prompts/act_tool_B_composed.md | 2db7eeee295eb581328d7cb06af6a54fe27d4c7c |
| prompts/system.md | a55ba2e25773b797a184210139cf4fa72b415a29 |

| 组 | 提示变化 |
| --- | --- |
| base | 修正后的共同基线 |
| overview | 只在 user 消息开头追加 Repository files: 与排序后的相对文件路径；超过 200 个时显示前 200 个并注明总数 |
| composed | 仅替换工具说明末尾示例；11 行内读→edit→测试→discard/返回摘要；A/B 仅最后返回写法不同 |

| 请求模型 | base URL | 响应 model | 运行数 | api_error | harness_error |
| --- | --- | --- | --- | --- | --- |
| deepseek-flash | https://api.deepseek.com | deepseek-flash | 432 | 0/432 (0.00%) | 1 |
| MiniMax-M3.1-Flash | https://api.minimaxi.com/v1 | MiniMax-M3.1-Flash | 432 | 0/432 (0.00%) | 4 |

| 组 | 模型 | 模式 | 运行数 | complete 分母 | api_error | harness_error |
| --- | --- | --- | --- | --- | --- | --- |
| base | deepseek-flash | A | 72 | 72 | 0 | 0 |
| base | deepseek-flash | B | 72 | 72 | 0 | 0 |
| base | MiniMax-M3.1-Flash | A | 72 | 72 | 0 | 0 |
| base | MiniMax-M3.1-Flash | B | 72 | 72 | 0 | 0 |
| overview | deepseek-flash | A | 72 | 72 | 0 | 0 |
| overview | deepseek-flash | B | 72 | 72 | 0 | 0 |
| overview | MiniMax-M3.1-Flash | A | 72 | 71 | 0 | 1 |
| overview | MiniMax-M3.1-Flash | B | 72 | 71 | 0 | 1 |
| composed | deepseek-flash | A | 72 | 71 | 0 | 1 |
| composed | deepseek-flash | B | 72 | 72 | 0 | 0 |
| composed | MiniMax-M3.1-Flash | A | 72 | 70 | 0 | 2 |
| composed | MiniMax-M3.1-Flash | B | 72 | 72 | 0 | 0 |

| 项目 | 原始数字 |
| --- | --- |
| 正式记录起始 UTC | 2026-11-12 15:15:49 |
| 正式记录结束 UTC | 2026-11-12 18:53:55 |
| api_retry 记录 | 1 |
| 有 JSON 回执的正式 act | 3886 |
| api_error 运行中的 act | 0 |

| 组 | 模型 | 模式 | 首次响应样本数 | 总输入 tokens 最小 | 总输入 tokens 最大 |
| --- | --- | --- | --- | --- | --- |
| base | deepseek-flash | A | 72 | 1727 | 1761 |
| base | deepseek-flash | B | 72 | 1752 | 1786 |
| base | MiniMax-M3.1-Flash | A | 72 | 1794 | 1829 |
| base | MiniMax-M3.1-Flash | B | 72 | 1819 | 1854 |
| overview | deepseek-flash | A | 72 | 1733 | 1776 |
| overview | deepseek-flash | B | 72 | 1758 | 1801 |
| overview | MiniMax-M3.1-Flash | A | 72 | 1801 | 1842 |
| overview | MiniMax-M3.1-Flash | B | 72 | 1826 | 1867 |
| composed | deepseek-flash | A | 72 | 1734 | 1768 |
| composed | deepseek-flash | B | 72 | 1779 | 1813 |
| composed | MiniMax-M3.1-Flash | A | 72 | 1797 | 1831 |
| composed | MiniMax-M3.1-Flash | B | 72 | 1842 | 1877 |

| 统计项 | 口径 |
| --- | --- |
| 分母 | 只计 status=complete；api_error 与 harness_error 的整次运行及其 act 排除并单列；重复一次任务为一次运行 |
| 表 1 | 解析通过 / act；运行通过 / act；discard 为运行通过，fail 为运行失败 |
| 表 2 | 沿用 v1 冻结判定；首次完成按第一次 act 后判定（仅一次 act 且最终回复补齐 fail 汇报也计入） |
| edit 类时序 | 沿用 v1：必须在较早一次 act 读取目标后再修改；同一次 act 读写即使文件正确也未满足此判定 |
| 表 4 | 下一次 act 运行通过 / 存在下一次 act 的失败调用；不要求任务完成；无下一次调用另列 |
| 表 5–6 | 每列按任务运行计数，列可重叠；discard/all=True 按宿主事件，fail 按原生错误行定位 |
| 接口计数 | metrics.interfaces/category_counts 为宿主事件进入次数；read/list/search/status/diff=读，write/edit/reset=写，run/pipe=执行，discard/fail 各列；参数绑定前报错没有宿主事件 |
| 脚本调用点 | source_interfaces/source_category_counts 为排除字符串和注释的词法调用点；未执行分支也保留；与实际执行次数分列 |
| 只读 act | 至少一次读接口事件，且无写、执行、discard、fail 事件；env.run(cat/wc 等)归执行；分母为全部有效 act |
| 单接口 act | 实际事件只涉及一种接口，可多次调用；另记 single_call 表示仅一次接口调用；纯计算/解析失败无事件时两者均否 |
| 首次写 act | 第一次进入 write/edit/reset 所在 act；只对发生写接口事件的任务求均值，无写为 null、不记 0 |
| 用尽预算 | 实际执行 8 次 act 即记 true，包括第 8 次后正常完成 |
| 异常分类限制 | 沿用 v1 外层异常处理：ConnectionResetError 未进入请求重试，会记为 harness_error；原始错误单列且排除该次分母 |
| token | 每 act 关联产生其脚本的 API 响应完整 usage；同响应多工具可重复关联；任务总 token 按 API 响应去重，含最终回复/预算终止响应，不估算失败请求 token |
| print | print 不在全局环境；按脚本词法调用点计数（包括导致解析后名称解析失败的调用点），不是动态循环次数；字符串/注释不计 |

## 表 1：每次 act 调用

| 组 | 模型 | 模式 | 调用数 | 解析通过率 | 运行通过率 |
| --- | --- | --- | --- | --- | --- |
| base | deepseek-flash | A | 281 | 272/281 (96.80%) | 239/281 (85.05%) |
| base | deepseek-flash | B | 264 | 259/264 (98.11%) | 235/264 (89.02%) |
| base | MiniMax-M3.1-Flash | A | 437 | 411/437 (94.05%) | 330/437 (75.51%) |
| base | MiniMax-M3.1-Flash | B | 414 | 399/414 (96.38%) | 346/414 (83.57%) |
| overview | deepseek-flash | A | 244 | 243/244 (99.59%) | 206/244 (84.43%) |
| overview | deepseek-flash | B | 221 | 221/221 (100.00%) | 191/221 (86.43%) |
| overview | MiniMax-M3.1-Flash | A | 351 | 337/351 (96.01%) | 270/351 (76.92%) |
| overview | MiniMax-M3.1-Flash | B | 379 | 363/379 (95.78%) | 295/379 (77.84%) |
| composed | deepseek-flash | A | 235 | 230/235 (97.87%) | 202/235 (85.96%) |
| composed | deepseek-flash | B | 242 | 237/242 (97.93%) | 212/242 (87.60%) |
| composed | MiniMax-M3.1-Flash | A | 377 | 366/377 (97.08%) | 302/377 (80.11%) |
| composed | MiniMax-M3.1-Flash | B | 414 | 406/414 (98.07%) | 341/414 (82.37%) |

## 表 2：每个任务

| 组 | 模型 | 模式 | 任务完成率 | 第一次 act 即完成的比例 | 平均 act 次数 |
| --- | --- | --- | --- | --- | --- |
| base | deepseek-flash | A | 68/72 (94.44%) | 9/72 (12.50%) | 281/72 = 3.9028 |
| base | deepseek-flash | B | 68/72 (94.44%) | 14/72 (19.44%) | 264/72 = 3.6667 |
| base | MiniMax-M3.1-Flash | A | 61/72 (84.72%) | 18/72 (25.00%) | 437/72 = 6.0694 |
| base | MiniMax-M3.1-Flash | B | 59/72 (81.94%) | 13/72 (18.06%) | 414/72 = 5.7500 |
| overview | deepseek-flash | A | 70/72 (97.22%) | 14/72 (19.44%) | 244/72 = 3.3889 |
| overview | deepseek-flash | B | 66/72 (91.67%) | 16/72 (22.22%) | 221/72 = 3.0694 |
| overview | MiniMax-M3.1-Flash | A | 63/71 (88.73%) | 11/71 (15.49%) | 351/71 = 4.9437 |
| overview | MiniMax-M3.1-Flash | B | 63/71 (88.73%) | 9/71 (12.68%) | 379/71 = 5.3380 |
| composed | deepseek-flash | A | 64/71 (90.14%) | 15/71 (21.13%) | 235/71 = 3.3099 |
| composed | deepseek-flash | B | 68/72 (94.44%) | 21/72 (29.17%) | 242/72 = 3.3611 |
| composed | MiniMax-M3.1-Flash | A | 60/70 (85.71%) | 18/70 (25.71%) | 377/70 = 5.3857 |
| composed | MiniMax-M3.1-Flash | B | 59/72 (81.94%) | 16/72 (22.22%) | 414/72 = 5.7500 |

## 表 3：错误分类

| 组 | 模型 | 模式 | Python 习惯 | 接口误用 | 唯一匹配失败 | 命令未预置 | 超出上限 | 其他 |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| base | deepseek-flash | A | 1 | 25 | 0 | 7 | 0 | 9 |
| base | deepseek-flash | B | 2 | 14 | 0 | 7 | 0 | 6 |
| base | MiniMax-M3.1-Flash | A | 21 | 16 | 0 | 50 | 0 | 20 |
| base | MiniMax-M3.1-Flash | B | 10 | 14 | 0 | 32 | 0 | 12 |
| overview | deepseek-flash | A | 0 | 19 | 0 | 15 | 0 | 4 |
| overview | deepseek-flash | B | 0 | 13 | 1 | 12 | 0 | 4 |
| overview | MiniMax-M3.1-Flash | A | 11 | 8 | 0 | 51 | 0 | 11 |
| overview | MiniMax-M3.1-Flash | B | 8 | 9 | 1 | 53 | 0 | 13 |
| composed | deepseek-flash | A | 1 | 17 | 0 | 7 | 0 | 8 |
| composed | deepseek-flash | B | 1 | 17 | 0 | 6 | 0 | 6 |
| composed | MiniMax-M3.1-Flash | A | 17 | 9 | 0 | 36 | 1 | 12 |
| composed | MiniMax-M3.1-Flash | B | 5 | 10 | 0 | 50 | 0 | 8 |

## 表 4：失败后下一次调用改对的比例

| 组 | 模型 | 模式 | 错误分类 | 下次运行通过 | 存在下一次调用 | 比例 | 无下一次调用 |
| --- | --- | --- | --- | --- | --- | --- | --- |
| base | deepseek-flash | A | Python 习惯 | 1 | 1 | 1/1 (100.00%) | 0 |
| base | deepseek-flash | A | 接口误用 | 24 | 25 | 24/25 (96.00%) | 0 |
| base | deepseek-flash | A | 唯一匹配失败 | 0 | 0 | 0/0 (—) | 0 |
| base | deepseek-flash | A | 命令未预置 | 4 | 5 | 4/5 (80.00%) | 2 |
| base | deepseek-flash | A | 超出上限 | 0 | 0 | 0/0 (—) | 0 |
| base | deepseek-flash | A | 其他 | 7 | 9 | 7/9 (77.78%) | 0 |
| base | deepseek-flash | B | Python 习惯 | 2 | 2 | 2/2 (100.00%) | 0 |
| base | deepseek-flash | B | 接口误用 | 14 | 14 | 14/14 (100.00%) | 0 |
| base | deepseek-flash | B | 唯一匹配失败 | 0 | 0 | 0/0 (—) | 0 |
| base | deepseek-flash | B | 命令未预置 | 6 | 7 | 6/7 (85.71%) | 0 |
| base | deepseek-flash | B | 超出上限 | 0 | 0 | 0/0 (—) | 0 |
| base | deepseek-flash | B | 其他 | 5 | 6 | 5/6 (83.33%) | 0 |
| base | MiniMax-M3.1-Flash | A | Python 习惯 | 7 | 20 | 7/20 (35.00%) | 1 |
| base | MiniMax-M3.1-Flash | A | 接口误用 | 12 | 16 | 12/16 (75.00%) | 0 |
| base | MiniMax-M3.1-Flash | A | 唯一匹配失败 | 0 | 0 | 0/0 (—) | 0 |
| base | MiniMax-M3.1-Flash | A | 命令未预置 | 28 | 41 | 28/41 (68.29%) | 9 |
| base | MiniMax-M3.1-Flash | A | 超出上限 | 0 | 0 | 0/0 (—) | 0 |
| base | MiniMax-M3.1-Flash | A | 其他 | 11 | 20 | 11/20 (55.00%) | 0 |
| base | MiniMax-M3.1-Flash | B | Python 习惯 | 6 | 10 | 6/10 (60.00%) | 0 |
| base | MiniMax-M3.1-Flash | B | 接口误用 | 13 | 14 | 13/14 (92.86%) | 0 |
| base | MiniMax-M3.1-Flash | B | 唯一匹配失败 | 0 | 0 | 0/0 (—) | 0 |
| base | MiniMax-M3.1-Flash | B | 命令未预置 | 15 | 22 | 15/22 (68.18%) | 10 |
| base | MiniMax-M3.1-Flash | B | 超出上限 | 0 | 0 | 0/0 (—) | 0 |
| base | MiniMax-M3.1-Flash | B | 其他 | 7 | 12 | 7/12 (58.33%) | 0 |
| overview | deepseek-flash | A | Python 习惯 | 0 | 0 | 0/0 (—) | 0 |
| overview | deepseek-flash | A | 接口误用 | 19 | 19 | 19/19 (100.00%) | 0 |
| overview | deepseek-flash | A | 唯一匹配失败 | 0 | 0 | 0/0 (—) | 0 |
| overview | deepseek-flash | A | 命令未预置 | 6 | 11 | 6/11 (54.55%) | 4 |
| overview | deepseek-flash | A | 超出上限 | 0 | 0 | 0/0 (—) | 0 |
| overview | deepseek-flash | A | 其他 | 2 | 3 | 2/3 (66.67%) | 1 |
| overview | deepseek-flash | B | Python 习惯 | 0 | 0 | 0/0 (—) | 0 |
| overview | deepseek-flash | B | 接口误用 | 12 | 13 | 12/13 (92.31%) | 0 |
| overview | deepseek-flash | B | 唯一匹配失败 | 1 | 1 | 1/1 (100.00%) | 0 |
| overview | deepseek-flash | B | 命令未预置 | 9 | 11 | 9/11 (81.82%) | 1 |
| overview | deepseek-flash | B | 超出上限 | 0 | 0 | 0/0 (—) | 0 |
| overview | deepseek-flash | B | 其他 | 4 | 4 | 4/4 (100.00%) | 0 |
| overview | MiniMax-M3.1-Flash | A | Python 习惯 | 6 | 11 | 6/11 (54.55%) | 0 |
| overview | MiniMax-M3.1-Flash | A | 接口误用 | 7 | 8 | 7/8 (87.50%) | 0 |
| overview | MiniMax-M3.1-Flash | A | 唯一匹配失败 | 0 | 0 | 0/0 (—) | 0 |
| overview | MiniMax-M3.1-Flash | A | 命令未预置 | 25 | 43 | 25/43 (58.14%) | 8 |
| overview | MiniMax-M3.1-Flash | A | 超出上限 | 0 | 0 | 0/0 (—) | 0 |
| overview | MiniMax-M3.1-Flash | A | 其他 | 4 | 11 | 4/11 (36.36%) | 0 |
| overview | MiniMax-M3.1-Flash | B | Python 习惯 | 4 | 7 | 4/7 (57.14%) | 1 |
| overview | MiniMax-M3.1-Flash | B | 接口误用 | 6 | 9 | 6/9 (66.67%) | 0 |
| overview | MiniMax-M3.1-Flash | B | 唯一匹配失败 | 1 | 1 | 1/1 (100.00%) | 0 |
| overview | MiniMax-M3.1-Flash | B | 命令未预置 | 32 | 45 | 32/45 (71.11%) | 8 |
| overview | MiniMax-M3.1-Flash | B | 超出上限 | 0 | 0 | 0/0 (—) | 0 |
| overview | MiniMax-M3.1-Flash | B | 其他 | 10 | 13 | 10/13 (76.92%) | 0 |
| composed | deepseek-flash | A | Python 习惯 | 0 | 1 | 0/1 (0.00%) | 0 |
| composed | deepseek-flash | A | 接口误用 | 16 | 17 | 16/17 (94.12%) | 0 |
| composed | deepseek-flash | A | 唯一匹配失败 | 0 | 0 | 0/0 (—) | 0 |
| composed | deepseek-flash | A | 命令未预置 | 2 | 4 | 2/4 (50.00%) | 3 |
| composed | deepseek-flash | A | 超出上限 | 0 | 0 | 0/0 (—) | 0 |
| composed | deepseek-flash | A | 其他 | 7 | 8 | 7/8 (87.50%) | 0 |
| composed | deepseek-flash | B | Python 习惯 | 1 | 1 | 1/1 (100.00%) | 0 |
| composed | deepseek-flash | B | 接口误用 | 16 | 17 | 16/17 (94.12%) | 0 |
| composed | deepseek-flash | B | 唯一匹配失败 | 0 | 0 | 0/0 (—) | 0 |
| composed | deepseek-flash | B | 命令未预置 | 2 | 3 | 2/3 (66.67%) | 3 |
| composed | deepseek-flash | B | 超出上限 | 0 | 0 | 0/0 (—) | 0 |
| composed | deepseek-flash | B | 其他 | 5 | 5 | 5/5 (100.00%) | 1 |
| composed | MiniMax-M3.1-Flash | A | Python 习惯 | 15 | 17 | 15/17 (88.24%) | 0 |
| composed | MiniMax-M3.1-Flash | A | 接口误用 | 7 | 9 | 7/9 (77.78%) | 0 |
| composed | MiniMax-M3.1-Flash | A | 唯一匹配失败 | 0 | 0 | 0/0 (—) | 0 |
| composed | MiniMax-M3.1-Flash | A | 命令未预置 | 20 | 28 | 20/28 (71.43%) | 8 |
| composed | MiniMax-M3.1-Flash | A | 超出上限 | 0 | 0 | 0/0 (—) | 1 |
| composed | MiniMax-M3.1-Flash | A | 其他 | 10 | 12 | 10/12 (83.33%) | 0 |
| composed | MiniMax-M3.1-Flash | B | Python 习惯 | 3 | 4 | 3/4 (75.00%) | 1 |
| composed | MiniMax-M3.1-Flash | B | 接口误用 | 8 | 10 | 8/10 (80.00%) | 0 |
| composed | MiniMax-M3.1-Flash | B | 唯一匹配失败 | 0 | 0 | 0/0 (—) | 0 |
| composed | MiniMax-M3.1-Flash | B | 命令未预置 | 28 | 41 | 28/41 (68.29%) | 9 |
| composed | MiniMax-M3.1-Flash | B | 超出上限 | 0 | 0 | 0/0 (—) | 0 |
| composed | MiniMax-M3.1-Flash | B | 其他 | 8 | 8 | 8/8 (100.00%) | 0 |

## 表 5：「失败就放弃」任务

| 组 | 模型 | 模式 | 运行数 | 用 discard | 用 fail | 正常返回（改动被错误提交） | 先提交后恢复 | 未运行测试 | 最终完成 |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| base | deepseek-flash | A | 12 | 4 | 0 | 1 | 6 | 0 | 11 |
| base | deepseek-flash | B | 12 | 4 | 0 | 1 | 5 | 0 | 11 |
| base | MiniMax-M3.1-Flash | A | 12 | 0 | 0 | 7 | 5 | 0 | 5 |
| base | MiniMax-M3.1-Flash | B | 12 | 0 | 0 | 6 | 3 | 0 | 3 |
| overview | deepseek-flash | A | 12 | 1 | 0 | 0 | 7 | 0 | 12 |
| overview | deepseek-flash | B | 12 | 1 | 0 | 0 | 9 | 0 | 12 |
| overview | MiniMax-M3.1-Flash | A | 11 | 0 | 0 | 4 | 7 | 0 | 7 |
| overview | MiniMax-M3.1-Flash | B | 11 | 0 | 0 | 3 | 6 | 0 | 6 |
| composed | deepseek-flash | A | 12 | 8 | 0 | 1 | 3 | 0 | 11 |
| composed | deepseek-flash | B | 12 | 3 | 0 | 0 | 9 | 0 | 12 |
| composed | MiniMax-M3.1-Flash | A | 10 | 0 | 0 | 5 | 4 | 0 | 4 |
| composed | MiniMax-M3.1-Flash | B | 12 | 0 | 0 | 7 | 5 | 1 | 5 |

## 表 6：批量替换任务

| 组 | 模型 | 模式 | 运行数 | 用 all=True | 撞上唯一匹配报错 | 最终完成 |
| --- | --- | --- | --- | --- | --- | --- |
| base | deepseek-flash | A | 12 | 11 | 0 | 12 |
| base | deepseek-flash | B | 12 | 10 | 0 | 12 |
| base | MiniMax-M3.1-Flash | A | 12 | 9 | 0 | 11 |
| base | MiniMax-M3.1-Flash | B | 12 | 11 | 0 | 12 |
| overview | deepseek-flash | A | 12 | 8 | 0 | 12 |
| overview | deepseek-flash | B | 12 | 9 | 1 | 12 |
| overview | MiniMax-M3.1-Flash | A | 12 | 12 | 0 | 12 |
| overview | MiniMax-M3.1-Flash | B | 12 | 10 | 0 | 12 |
| composed | deepseek-flash | A | 12 | 7 | 0 | 12 |
| composed | deepseek-flash | B | 12 | 7 | 0 | 12 |
| composed | MiniMax-M3.1-Flash | A | 12 | 9 | 0 | 12 |
| composed | MiniMax-M3.1-Flash | B | 12 | 11 | 0 | 12 |

## 表 7：探索行为

| 组 | 模型 | 模式 | 只读 act 占比 | 单接口 act 占比 | 首次写操作 act 平均序号 | 用尽预算任务占比 | 平均 act 次数 | 平均总 token |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| base | deepseek-flash | A | 154/281 (54.80%) | 105/281 (37.37%) | 107/36 = 2.9722 | 10/72 (13.89%) | 281/72 = 3.9028 | 885547/72 = 12299.2639 |
| base | deepseek-flash | B | 158/264 (59.85%) | 111/264 (42.05%) | 110/36 = 3.0556 | 4/72 (5.56%) | 264/72 = 3.6667 | 834553/72 = 11591.0139 |
| base | MiniMax-M3.1-Flash | A | 200/437 (45.77%) | 203/437 (46.45%) | 147/35 = 4.2000 | 33/72 (45.83%) | 437/72 = 6.0694 | 1424047/72 = 19778.4306 |
| base | MiniMax-M3.1-Flash | B | 217/414 (52.42%) | 241/414 (58.21%) | 132/33 = 4.0000 | 29/72 (40.28%) | 414/72 = 5.7500 | 1248078/72 = 17334.4167 |
| overview | deepseek-flash | A | 125/244 (51.23%) | 117/244 (47.95%) | 89/36 = 2.4722 | 6/72 (8.33%) | 244/72 = 3.3889 | 807721/72 = 11218.3472 |
| overview | deepseek-flash | B | 115/221 (52.04%) | 105/221 (47.51%) | 84/36 = 2.3333 | 3/72 (4.17%) | 221/72 = 3.0694 | 757869/72 = 10525.9583 |
| overview | MiniMax-M3.1-Flash | A | 149/351 (42.45%) | 188/351 (53.56%) | 96/35 = 2.7429 | 22/71 (30.99%) | 351/71 = 4.9437 | 1229866/71 = 17322.0563 |
| overview | MiniMax-M3.1-Flash | B | 176/379 (46.44%) | 222/379 (58.58%) | 101/33 = 3.0606 | 26/71 (36.62%) | 379/71 = 5.3380 | 1323241/71 = 18637.1972 |
| composed | deepseek-flash | A | 139/235 (59.15%) | 108/235 (45.96%) | 101/36 = 2.8056 | 3/71 (4.23%) | 235/71 = 3.3099 | 749193/71 = 10552.0141 |
| composed | deepseek-flash | B | 139/242 (57.44%) | 109/242 (45.04%) | 102/36 = 2.8333 | 6/72 (8.33%) | 242/72 = 3.3611 | 790370/72 = 10977.3611 |
| composed | MiniMax-M3.1-Flash | A | 178/377 (47.21%) | 179/377 (47.48%) | 128/33 = 3.8788 | 20/70 (28.57%) | 377/70 = 5.3857 | 1272739/70 = 18181.9857 |
| composed | MiniMax-M3.1-Flash | B | 213/414 (51.45%) | 220/414 (53.14%) | 130/36 = 3.6111 | 28/72 (38.89%) | 414/72 = 5.7500 | 1368969/72 = 19013.4583 |

## 表 8：print

| 组 | 模型 | 模式 | 调用过 print 的 act 数 | 调用过 print 的任务数 | print 调用点总数 |
| --- | --- | --- | --- | --- | --- |
| base | deepseek-flash | A | 8 | 8 | 18 |
| base | deepseek-flash | B | 1 | 1 | 1 |
| base | MiniMax-M3.1-Flash | A | 1 | 1 | 9 |
| base | MiniMax-M3.1-Flash | B | 2 | 2 | 10 |
| overview | deepseek-flash | A | 4 | 4 | 7 |
| overview | deepseek-flash | B | 3 | 3 | 9 |
| overview | MiniMax-M3.1-Flash | A | 0 | 0 | 0 |
| overview | MiniMax-M3.1-Flash | B | 0 | 0 | 0 |
| composed | deepseek-flash | A | 0 | 0 | 0 |
| composed | deepseek-flash | B | 0 | 0 | 0 |
| composed | MiniMax-M3.1-Flash | A | 0 | 0 | 0 |
| composed | MiniMax-M3.1-Flash | B | 0 | 0 | 0 |

## 原始数字：分类任务与试跑

| 组 | 模型 | 模式 | 类别 | 正式运行 | 完成 | 首次 act 完成 | act 数 |
| --- | --- | --- | --- | --- | --- | --- | --- |
| base | deepseek-flash | A | read | 12 | 12 | 8 | 26 |
| base | deepseek-flash | A | edit | 12 | 12 | 0 | 29 |
| base | deepseek-flash | A | batch | 12 | 12 | 0 | 53 |
| base | deepseek-flash | A | test | 12 | 12 | 0 | 56 |
| base | deepseek-flash | A | rollback | 12 | 11 | 0 | 79 |
| base | deepseek-flash | A | probe | 12 | 9 | 1 | 38 |
| base | deepseek-flash | B | read | 12 | 12 | 7 | 34 |
| base | deepseek-flash | B | edit | 12 | 12 | 0 | 28 |
| base | deepseek-flash | B | batch | 12 | 12 | 0 | 43 |
| base | deepseek-flash | B | test | 12 | 12 | 4 | 53 |
| base | deepseek-flash | B | rollback | 12 | 11 | 0 | 70 |
| base | deepseek-flash | B | probe | 12 | 9 | 3 | 36 |
| base | MiniMax-M3.1-Flash | A | read | 12 | 12 | 8 | 42 |
| base | MiniMax-M3.1-Flash | A | edit | 12 | 12 | 0 | 62 |
| base | MiniMax-M3.1-Flash | A | batch | 12 | 11 | 0 | 82 |
| base | MiniMax-M3.1-Flash | A | test | 12 | 12 | 9 | 89 |
| base | MiniMax-M3.1-Flash | A | rollback | 12 | 5 | 0 | 96 |
| base | MiniMax-M3.1-Flash | A | probe | 12 | 9 | 1 | 66 |
| base | MiniMax-M3.1-Flash | B | read | 12 | 12 | 6 | 45 |
| base | MiniMax-M3.1-Flash | B | edit | 12 | 12 | 0 | 47 |
| base | MiniMax-M3.1-Flash | B | batch | 12 | 12 | 0 | 85 |
| base | MiniMax-M3.1-Flash | B | test | 12 | 12 | 7 | 82 |
| base | MiniMax-M3.1-Flash | B | rollback | 12 | 3 | 0 | 96 |
| base | MiniMax-M3.1-Flash | B | probe | 12 | 8 | 0 | 59 |
| overview | deepseek-flash | A | read | 12 | 12 | 10 | 20 |
| overview | deepseek-flash | A | edit | 12 | 12 | 0 | 25 |
| overview | deepseek-flash | A | batch | 12 | 12 | 0 | 38 |
| overview | deepseek-flash | A | test | 12 | 12 | 3 | 47 |
| overview | deepseek-flash | A | rollback | 12 | 12 | 0 | 79 |
| overview | deepseek-flash | A | probe | 12 | 10 | 1 | 35 |
| overview | deepseek-flash | B | read | 12 | 11 | 11 | 20 |
| overview | deepseek-flash | B | edit | 12 | 12 | 0 | 24 |
| overview | deepseek-flash | B | batch | 12 | 12 | 0 | 30 |
| overview | deepseek-flash | B | test | 12 | 12 | 5 | 46 |
| overview | deepseek-flash | B | rollback | 12 | 12 | 0 | 74 |
| overview | deepseek-flash | B | probe | 12 | 7 | 0 | 27 |
| overview | MiniMax-M3.1-Flash | A | read | 12 | 12 | 10 | 28 |
| overview | MiniMax-M3.1-Flash | A | edit | 12 | 12 | 0 | 34 |
| overview | MiniMax-M3.1-Flash | A | batch | 12 | 12 | 0 | 61 |
| overview | MiniMax-M3.1-Flash | A | test | 12 | 12 | 1 | 88 |
| overview | MiniMax-M3.1-Flash | A | rollback | 11 | 7 | 0 | 88 |
| overview | MiniMax-M3.1-Flash | A | probe | 12 | 8 | 0 | 52 |
| overview | MiniMax-M3.1-Flash | B | read | 12 | 12 | 8 | 35 |
| overview | MiniMax-M3.1-Flash | B | edit | 12 | 12 | 0 | 44 |
| overview | MiniMax-M3.1-Flash | B | batch | 12 | 12 | 0 | 64 |
| overview | MiniMax-M3.1-Flash | B | test | 12 | 12 | 1 | 94 |
| overview | MiniMax-M3.1-Flash | B | rollback | 11 | 6 | 0 | 88 |
| overview | MiniMax-M3.1-Flash | B | probe | 12 | 9 | 0 | 54 |
| composed | deepseek-flash | A | read | 11 | 10 | 6 | 27 |
| composed | deepseek-flash | A | edit | 12 | 10 | 0 | 28 |
| composed | deepseek-flash | A | batch | 12 | 12 | 0 | 41 |
| composed | deepseek-flash | A | test | 12 | 12 | 7 | 39 |
| composed | deepseek-flash | A | rollback | 12 | 11 | 0 | 70 |
| composed | deepseek-flash | A | probe | 12 | 9 | 2 | 30 |
| composed | deepseek-flash | B | read | 12 | 11 | 11 | 21 |
| composed | deepseek-flash | B | edit | 12 | 12 | 0 | 29 |
| composed | deepseek-flash | B | batch | 12 | 12 | 0 | 47 |
| composed | deepseek-flash | B | test | 12 | 12 | 9 | 32 |
| composed | deepseek-flash | B | rollback | 12 | 12 | 0 | 84 |
| composed | deepseek-flash | B | probe | 12 | 9 | 1 | 29 |
| composed | MiniMax-M3.1-Flash | A | read | 12 | 12 | 6 | 53 |
| composed | MiniMax-M3.1-Flash | A | edit | 12 | 12 | 0 | 43 |
| composed | MiniMax-M3.1-Flash | A | batch | 12 | 12 | 0 | 78 |
| composed | MiniMax-M3.1-Flash | A | test | 12 | 12 | 12 | 78 |
| composed | MiniMax-M3.1-Flash | A | rollback | 10 | 4 | 0 | 80 |
| composed | MiniMax-M3.1-Flash | A | probe | 12 | 8 | 0 | 45 |
| composed | MiniMax-M3.1-Flash | B | read | 12 | 10 | 6 | 55 |
| composed | MiniMax-M3.1-Flash | B | edit | 12 | 12 | 0 | 53 |
| composed | MiniMax-M3.1-Flash | B | batch | 12 | 12 | 0 | 70 |
| composed | MiniMax-M3.1-Flash | B | test | 12 | 12 | 10 | 92 |
| composed | MiniMax-M3.1-Flash | B | rollback | 12 | 5 | 0 | 95 |
| composed | MiniMax-M3.1-Flash | B | probe | 12 | 8 | 0 | 49 |

| 试跑 run_id | 状态 | 完成 | act 数 |
| --- | --- | --- | --- |
| base-deepseek-A-read-02-r1 | complete | 1 | 1 |
| base-deepseek-A-rollback-01-r1 | complete | 1 | 8 |
| base-deepseek-B-read-02-r1 | complete | 1 | 2 |
| base-deepseek-B-rollback-01-r1 | complete | 1 | 7 |
| base-minimax-A-read-02-r1 | complete | 1 | 2 |
| base-minimax-A-rollback-01-r1 | complete | 0 | 8 |
| base-minimax-B-read-02-r1 | complete | 1 | 2 |
| base-minimax-B-rollback-01-r1 | complete | 1 | 8 |
| composed-deepseek-A-read-02-r1 | complete | 1 | 3 |
| composed-deepseek-A-rollback-01-r1 | complete | 1 | 2 |
| composed-deepseek-B-read-02-r1 | complete | 1 | 1 |
| composed-deepseek-B-rollback-01-r1 | complete | 1 | 4 |
| composed-minimax-A-read-02-r1 | complete | 1 | 2 |
| composed-minimax-A-rollback-01-r1 | complete | 1 | 8 |
| composed-minimax-B-read-02-r1 | complete | 1 | 3 |
| composed-minimax-B-rollback-01-r1 | complete | 1 | 8 |
| overview-deepseek-A-read-02-r1 | complete | 1 | 2 |
| overview-deepseek-A-rollback-01-r1 | complete | 1 | 5 |
| overview-deepseek-B-read-02-r1 | complete | 1 | 2 |
| overview-deepseek-B-rollback-01-r1 | complete | 1 | 8 |
| overview-minimax-A-read-02-r1 | complete | 1 | 2 |
| overview-minimax-A-rollback-01-r1 | complete | 1 | 8 |
| overview-minimax-B-read-02-r1 | complete | 1 | 2 |
| overview-minimax-B-rollback-01-r1 | complete | 0 | 8 |

## 测试台修正

| 编号 | 实际改法 | 证据 |
| --- | --- | --- |
| 1 | system.md 删除引导分步句，未替换为其他建议 | prompts/system.md |
| 2 | 预算 4→8；末尾公开最多 8 次；耗尽后行为保持 v1 | driver.py；tests/test_v2.py |
| 3 | 8 份 canned 输出首行按实际用例计数（test 类 3、rollback 类 2）；宿主和判定器共享命令归一化表 | command-normalization.json；commands.py；act-sim/src/commands.rs |
| 4 | read-03 同时接受文件名→计数、wc 行数→文件名、嵌套计数对象和记录列表；错误配对/13 代替 3 不通过 | 6 条历史假阴性回归 + 嵌套/反例；tests/test_v2.py |
| 5 | MiniMax 独立单线程池，最多 10 次请求；429 指数退避封顶 120 秒；deepseek 独立 4 线程池 | driver.py；重试回归；results/v2/formal |

| 命令归一化项 | 显式规则 |
| --- | --- |
| 命令前缀 | ["cargo", "test"] |
| cargo 层忽略选项 | -q, --quiet, --no-fail-fast |
| 保留并排序的 scope 选项 | --offline, --workspace, --lib |
| cargo 层测试名过滤 | 至多 1 个不以 - 开头的位置参数，忽略过滤值 |
| -- 后忽略选项 | -q, --quiet, --nocapture, --show-output, --exact |
| -- 后调度参数 | --test-threads；正整数，支持 --key N 与 --key=N |
| -- 后测试名过滤 | 至多 1 个不以 - 开头的位置参数，忽略过滤值 |
| 输出 | 仅 cargo test 族归一化：忽略列出的输出/调度开关与测试名过滤；保留并排序 scope flags。--list、--no-run、未知开关及其他命令保持原样，不伪装为已运行测试。过滤变体仍返回该任务完整预置输出。 |
| 表外命令 | 保留原 argv；先精确预置匹配，否则沿用 v1 白名单；不执行真实 cargo |

| v1 run_id | act | 原 argv | 归一化 argv | 命中同任务预置 |
| --- | --- | --- | --- | --- |
| deepseek-A-rollback-03-r1 | 4 | ["cargo", "--version"] | ["cargo", "--version"] | 0 |
| deepseek-A-test-01-r1 | 1 | ["cargo", "test", "--no-fail-fast"] | ["cargo", "test"] | 1 |
| deepseek-A-test-01-r1 | 3 | ["cargo", "--version"] | ["cargo", "--version"] | 0 |
| deepseek-A-test-01-r2 | 4 | ["cargo", "test", "--no-fail-fast"] | ["cargo", "test"] | 1 |
| deepseek-A-test-02-r1 | 4 | ["cargo", "test", "--offline", "--no-fail-fast"] | ["cargo", "test", "--offline"] | 1 |
| deepseek-A-test-04-r2 | 4 | ["cargo", "test", "--lib", "--", "--test-threads=1"] | ["cargo", "test", "--lib"] | 1 |
| deepseek-A-test-04-r3 | 2 | ["cargo", "test", "--lib", "--", "--test-threads=1"] | ["cargo", "test", "--lib"] | 1 |
| deepseek-B-probe-03-r2 | 4 | ["sort"] | ["sort"] | 0 |
| deepseek-B-test-01-r1 | 3 | ["cargo", "test", "--no-fail-fast", "--", "--test-threads=1"] | ["cargo", "test"] | 1 |
| deepseek-B-test-01-r3 | 4 | ["cargo", "test", "--", "--test-threads=1"] | ["cargo", "test"] | 1 |
| deepseek-B-test-03-r1 | 3 | ["cargo", "test", "--workspace", "--no-fail-fast"] | ["cargo", "test", "--workspace"] | 1 |
| deepseek-B-test-03-r3 | 3 | ["cargo", "test", "--workspace", "--no-fail-fast"] | ["cargo", "test", "--workspace"] | 1 |
| deepseek-B-test-04-r3 | 3 | ["cargo", "test", "--lib", "--", "--list"] | ["cargo", "test", "--lib", "--", "--list"] | 0 |
| minimax-A-edit-02-r1 | 3 | ["cargo", "build"] | ["cargo", "build"] | 0 |
| minimax-A-probe-03-r3 | 4 | ["awk", "{print $1}", "logs/large.log"] | ["awk", "{print $1}", "logs/large.log"] | 0 |
| minimax-A-read-03-r2 | 3 | ["sh", "-c", "for f in data/a.txt data/b.txt; do echo \"== $f\"; od -c \"$f\" \| tail -3; done"] | ["sh", "-c", "for f in data/a.txt data/b.txt; do echo \"== $f\"; od -c \"$f\" \| tail -3; done"] | 0 |
| minimax-A-read-03-r3 | 2 | ["bash", "-c", "find data -type f -name '*.txt' \| sort"] | ["bash", "-c", "find data -type f -name '*.txt' \| sort"] | 0 |
| minimax-A-rollback-02-r1 | 4 | ["cargo", "--version"] | ["cargo", "--version"] | 0 |
| minimax-A-rollback-03-r3 | 2 | ["cargo", "--version"] | ["cargo", "--version"] | 0 |
| minimax-A-rollback-04-r2 | 4 | ["cargo", "--version"] | ["cargo", "--version"] | 0 |
| minimax-A-rollback-04-r3 | 4 | ["cargo", "--version"] | ["cargo", "--version"] | 0 |
| minimax-A-test-01-r3 | 3 | ["cargo", "test", "--no-fail-fast"] | ["cargo", "test"] | 1 |
| minimax-A-test-02-r2 | 3 | ["cargo", "test", "--offline", "--", "--nocapture"] | ["cargo", "test", "--offline"] | 1 |
| minimax-A-test-03-r1 | 2 | ["cargo", "test", "--workspace", "--no-fail-fast"] | ["cargo", "test", "--workspace"] | 1 |
| minimax-A-test-04-r3 | 2 | ["cargo", "test", "--lib", "--", "--nocapture"] | ["cargo", "test", "--lib"] | 1 |
| minimax-B-batch-04-r2 | 4 | ["bash", "-lc", "find . -type f \| sort && echo '---' && find src -type f \| sort"] | ["bash", "-lc", "find . -type f \| sort && echo '---' && find src -type f \| sort"] | 0 |
| minimax-B-rollback-01-r3 | 4 | ["cargo", "--version"] | ["cargo", "--version"] | 0 |
| minimax-B-rollback-03-r1 | 4 | ["cargo", "--version"] | ["cargo", "--version"] | 0 |
| minimax-B-rollback-03-r3 | 4 | ["cargo", "test", "--no-run"] | ["cargo", "test", "--no-run"] | 0 |
| minimax-B-rollback-04-r3 | 4 | ["bash", "-lc", "ls -la; find . -path ./target -prune -o -type f -print"] | ["bash", "-lc", "ls -la; find . -path ./target -prune -o -type f -print"] | 0 |
| minimax-B-test-01-r1 | 3 | ["cargo", "test", "--no-fail-fast", "--", "--test-threads=1"] | ["cargo", "test"] | 1 |
| minimax-B-test-01-r1 | 4 | ["cargo", "test", "--no-fail-fast"] | ["cargo", "test"] | 1 |
| minimax-B-test-01-r3 | 2 | ["cargo", "test", "--no-fail-fast"] | ["cargo", "test"] | 1 |
| minimax-B-test-02-r1 | 2 | ["cargo", "--version"] | ["cargo", "--version"] | 0 |
| minimax-B-test-02-r1 | 3 | ["which", "cargo"] | ["which", "cargo"] | 0 |
| minimax-B-test-04-r1 | 2 | ["cargo", "test", "--lib", "--", "--nocapture"] | ["cargo", "test", "--lib"] | 1 |
| minimax-B-test-04-r2 | 2 | ["cargo", "test", "--lib", "--", "--nocapture"] | ["cargo", "test", "--lib"] | 1 |

## 与 v1 的差异

| 测试台项 | v1 | v2 |
| --- | --- | --- |
| 分步引导 | system 中要求先读、后次 act 再改 | 删除该句 |
| 预算 | 4，未公开 | 8，公开 |
| canned 测试数 | 固定 4 | 与列出的 2/3 用例一致 |
| 命令匹配 | 完整 argv | 显式选项表归一化；scope 保留 |
| read-03 判定 | 只匹配文件名后紧接数字 | 补充 wc 反向排列与嵌套计数 |
| MiniMax 并发/重试 | 共享 4 线程；5 次 | 独立 1 线程；10 次，上限 120 秒 |
| 提示因素 | 单组 | base/overview/composed；A/B |
| 调用观测 | 部分 repo 事件 | 补齐 list/status/diff；pipe 在白名单检查前记录；另记词法调用点、分类、只读/单接口、print、usage |
| 任务观测 | act_count 与通过判定 | 增加首次写 act、预算耗尽、汇总 usage |
| 冻结 | prompts/tasks/check.py | 同样输入冻结，另记录并核验运行驱动、归一化、指标代码、Rust 源码和二进制哈希 |
| 数据位置 | results/formal、results/pilot、REPORT.md | results/v2/formal、results/v2/pilot、REPORT-v2.md |

## 验证

| 命令 | 结果 |
| --- | --- |
| cargo build --manifest-path prototypes/starlark-pass-rate/Cargo.toml -j 4 | Compiling act-sim v0.1.0 (/home/gcp-vegas/.t3/worktrees/lancet/t3-4e357b6c/prototypes/starlark-pass-rate/act-sim)<br>    Finished `dev` profile [unoptimized] target(s) in 1.01s |
| python3 -m unittest discover -s prototypes/starlark-pass-rate/tests | ................<br>----------------------------------------------------------------------<br>Ran 16 tests in 3.753s<br><br>OK |
| python3 prototypes/starlark-pass-rate/language_probe.py | 70 条探针记录；results/v2/evidence/language-probes.jsonl |
| python3 prototypes/starlark-pass-rate/driver.py --phase pilot --workers 4 | 24 条摘要，试跑不计正式分母 |
| python3 prototypes/starlark-pass-rate/driver.py --phase formal --workers 4 | 864 条摘要 |
| python3 prototypes/starlark-pass-rate/validate.py | {"runs": 864, "calls": 3886, "grid_complete": true, "frozen_files": 153, "replay_state_or_check_mismatches": 0, "replay_value_differences": 176, "metrics_and_usage_checked": true, "peak_concurrent_runs": {"deepseek": 4, "minimax": 1}, "v1_and_docs_unchanged": true, "status_counts": {"complete": 859, "harness_error": 5}} |
| cargo xtask docs-check | Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.29s<br>     Running `target/debug/xtask docs-check`<br>文档链接检查通过：50 个 Markdown 文件 |
| 未覆盖 | 真实 cargo、沙箱、overlay；模型内部版本及服务端默认采样参数；根 workspace 全量 CI |

| 返回值差异分类 | 数量 |
| --- | --- |
| ls 元数据 | 161 |
| 含 fixture 外命令的返回差异 | 15 |

| 命令访问范围 | 原始数字与限制 |
| --- | --- |
| 含绝对/上级路径参数的命令尝试 | 84 |
| 执行环境 | 沿用 v1：只设置 fixture 副本为 cwd，未隔离文件系统；部分只读命令枚举 fixture 外目录。原始 argv 与 act 结局见 results/v2/evidence/external-command-audit.json；具体外部文件访问范围未验证 |

## 阻塞项

| 命令 / run_id | 原始报错 |
| --- | --- |
| driver.py --phase formal --workers 4 / composed-deepseek-A-read-02-r2 |  [Errno 104] Connection reset by peer |
| driver.py --phase formal --workers 4 / composed-minimax-A-rollback-01-r1 |  Command '['/home/gcp-vegas/.t3/worktrees/lancet/t3-4e357b6c/prototypes/starlark-pass-rate/target/debug/act-sim', '--fixture', '/tmp/act-experiment-weykta4i/repo', '--script', '/tmp/act-experiment-weykta4i/script.star', '--mode', 'A', '--canned', '/home/gcp-vegas/.t3/worktrees/lancet/t3-4e357b6c/prototypes/starlark-pass-rate/tasks/rollback-01/canned.json']' timed out after 60 seconds |
| driver.py --phase formal --workers 4 / composed-minimax-A-rollback-02-r1 |  Command '['/home/gcp-vegas/.t3/worktrees/lancet/t3-4e357b6c/prototypes/starlark-pass-rate/target/debug/act-sim', '--fixture', '/tmp/act-experiment-9v8e86pc/repo', '--script', '/tmp/act-experiment-9v8e86pc/script.star', '--mode', 'A', '--canned', '/home/gcp-vegas/.t3/worktrees/lancet/t3-4e357b6c/prototypes/starlark-pass-rate/tasks/rollback-02/canned.json']' timed out after 60 seconds |
| driver.py --phase formal --workers 4 / overview-minimax-A-rollback-01-r1 |  Command '['/home/gcp-vegas/.t3/worktrees/lancet/t3-4e357b6c/prototypes/starlark-pass-rate/target/debug/act-sim', '--fixture', '/tmp/act-experiment-lfb9j7ul/repo', '--script', '/tmp/act-experiment-lfb9j7ul/script.star', '--mode', 'A', '--canned', '/home/gcp-vegas/.t3/worktrees/lancet/t3-4e357b6c/prototypes/starlark-pass-rate/tasks/rollback-01/canned.json']' timed out after 60 seconds |
| driver.py --phase formal --workers 4 / overview-minimax-B-rollback-02-r1 |  Command '['/home/gcp-vegas/.t3/worktrees/lancet/t3-4e357b6c/prototypes/starlark-pass-rate/target/debug/act-sim', '--fixture', '/tmp/act-experiment-_yz_sim0/repo', '--script', '/tmp/act-experiment-_yz_sim0/script.star', '--mode', 'B', '--canned', '/home/gcp-vegas/.t3/worktrees/lancet/t3-4e357b6c/prototypes/starlark-pass-rate/tasks/rollback-02/canned.json']' timed out after 60 seconds |

## 原始数字：未收到 act 回执的调用

| run_id | API 响应序号 | 调用点 | usage | 原始报错 |
| --- | --- | --- | --- | --- |
| composed-minimax-A-rollback-01-r1 | 8 | {"env.run": 1} | {"total_tokens": 5781, "total_characters": 0, "prompt_tokens": 5008, "completion_tokens": 773, "completion_tokens_details": {"reasoning_tokens": 640}, "prompt_tokens_details": {"cached_tokens": 4163}} | Command '['/home/gcp-vegas/.t3/worktrees/lancet/t3-4e357b6c/prototypes/starlark-pass-rate/target/debug/act-sim', '--fixture', '/tmp/act-experiment-weykta4i/repo', '--script', '/tmp/act-experiment-weykta4i/script.star', '--mode', 'A', '--canned', '/home/gcp-vegas/.t3/worktrees/lancet/t3-4e357b6c/prototypes/starlark-pass-rate/tasks/rollback-01/canned.json']' timed out after 60 seconds |
| composed-minimax-A-rollback-02-r1 | 6 | {"env.run": 1} | {"total_tokens": 3473, "total_characters": 0, "prompt_tokens": 3259, "completion_tokens": 214, "completion_tokens_details": {"reasoning_tokens": 147}, "prompt_tokens_details": {"cached_tokens": 2769}} | Command '['/home/gcp-vegas/.t3/worktrees/lancet/t3-4e357b6c/prototypes/starlark-pass-rate/target/debug/act-sim', '--fixture', '/tmp/act-experiment-9v8e86pc/repo', '--script', '/tmp/act-experiment-9v8e86pc/script.star', '--mode', 'A', '--canned', '/home/gcp-vegas/.t3/worktrees/lancet/t3-4e357b6c/prototypes/starlark-pass-rate/tasks/rollback-02/canned.json']' timed out after 60 seconds |
| overview-minimax-A-rollback-01-r1 | 7 | {"env.run": 3} | {"total_tokens": 6264, "total_characters": 0, "prompt_tokens": 5762, "completion_tokens": 502, "completion_tokens_details": {"reasoning_tokens": 330}, "prompt_tokens_details": {"cached_tokens": 3369}} | Command '['/home/gcp-vegas/.t3/worktrees/lancet/t3-4e357b6c/prototypes/starlark-pass-rate/target/debug/act-sim', '--fixture', '/tmp/act-experiment-lfb9j7ul/repo', '--script', '/tmp/act-experiment-lfb9j7ul/script.star', '--mode', 'A', '--canned', '/home/gcp-vegas/.t3/worktrees/lancet/t3-4e357b6c/prototypes/starlark-pass-rate/tasks/rollback-01/canned.json']' timed out after 60 seconds |
| overview-minimax-B-rollback-02-r1 | 8 | {"env.run": 3} | {"total_tokens": 6903, "total_characters": 0, "prompt_tokens": 6213, "completion_tokens": 690, "completion_tokens_details": {"reasoning_tokens": 540}, "prompt_tokens_details": {"cached_tokens": 3183}} | Command '['/home/gcp-vegas/.t3/worktrees/lancet/t3-4e357b6c/prototypes/starlark-pass-rate/target/debug/act-sim', '--fixture', '/tmp/act-experiment-_yz_sim0/repo', '--script', '/tmp/act-experiment-_yz_sim0/script.star', '--mode', 'B', '--canned', '/home/gcp-vegas/.t3/worktrees/lancet/t3-4e357b6c/prototypes/starlark-pass-rate/tasks/rollback-02/canned.json']' timed out after 60 seconds |

| 未覆盖项 | 数量与原因 |
| --- | --- |
| 缺少动态接口事件 | 4 次调用；act-sim 超时没有 JSON 回执，脚本和 token 来自原始 API 响应；整次运行排除统计分母 |

````python
cmds = {}
for argv in [["ls", "-la", "target"], ["find", "target", "-name", "*.rs"], ["grep", "-rl", "expected rejection at", "/"], ["cargo", "test", "--", "--nocapture"]]:
    r = env.run(argv, check=False)
    cmds[" ".join(argv)] = {"code": r.code, "out": r.stdout[-3000:], "err": r.stderr[-2000:]}
return {"cmds": cmds}

````

````python
find = env.run(["grep", "-rl", "memory_budget", "/"], check=False)
return {"out": (find.stdout + find.stderr)[:3000], "code": find.code}
````

````python
res = env.run(["grep", "-rl", "expected rejection at", "/tmp", "/opt", "/root", "/usr/local", "/home", "/srv", "/app", "/workspace"], check=False)
res2 = env.run(["ls", "-la", "/"], check=False)
res3 = env.run(["find", "/", "-maxdepth", "4", "-name", "*.rs", "-path", "*test*"], check=False)
return {"grep": res.stdout + res.stderr, "lsroot": res2.stdout + res2.stderr, "find": res3.stdout + res3.stderr}
````

````python
r1 = env.run(["ls", "-la", "/home/gcp-vegas"], check=False)
r2 = env.run(["cat", "/home/gcp-vegas/.cargo/config.toml"], check=False)
r3 = env.run(["grep", "-rl", "memory_budget", "/home/gcp-vegas", "/usr/local", "/opt", "/var/tmp", "/tmp"], check=False)
result({"home": r1.stdout + r1.stderr, "cargo_cfg": r2.stdout + r2.stderr, "grep": r3.stdout[:3000] + r3.stderr})
````

## 附录：每个错误分类各附 3 个脚本原文

### Python 习惯

| 正式错误调用总数 | 附录脚本数 |
| --- | --- |
| 77 | 3 |

| 组 | run_id | act | 原始错误 |
| --- | --- | --- | --- |
| base | base-deepseek-A-edit-01-r1 | 1 | error: Parse error: cannot use reserved keyword `try`<br> --> script.star:3:9<br>  \|<br>3 \|         try:<br>  \|         ^^^<br>  \|<br> |

````python
paths = []
for root in [".", "src", "config", "configs"]:
    try:
        entries = repo.list(root)
    except Exception:
        continue
    paths.append((root, entries))
return paths
````

| 组 | run_id | act | 原始错误 |
| --- | --- | --- | --- |
| base | base-deepseek-B-batch-01-r1 | 1 | error: Variable `set` not found, did you mean `int`?<br> --> script.star:4:19<br>  \|<br>4 \|   "files": sorted(set([h.path for h in hits])),<br>  \|                   ^^^<br>  \|<br> |

````python
hits = repo.search("OldClient", glob = "src/**")
result({
  "count": len(hits),
  "files": sorted(set([h.path for h in hits])),
})

````

| 组 | run_id | act | 原始错误 |
| --- | --- | --- | --- |
| base | base-deepseek-B-probe-03-r1 | 2 | error: Parse error: cannot use reserved keyword `import`<br> --> script.star:1:1<br>  \|<br>1 \| import re<br>  \| ^^^^^^<br>  \|<br> |

````python
import re
content = repo.read("logs/large.log")
lines = content.split("\n")
errs = []
for i, l in enumerate(lines):
    if "error" in l.lower():
        errs.append("%d: %s" % (i+1, l))
result({"total_lines": len(lines), "error_like": errs[:20], "num_error_like": len(errs)})
````

### 接口误用

| 正式错误调用总数 | 附录脚本数 |
| --- | --- |
| 171 | 3 |

| 组 | run_id | act | 原始错误 |
| --- | --- | --- | --- |
| base | base-deepseek-A-edit-02-r2 | 1 | error: Variable `print` not found, did you mean `int`?<br> --> script.star:1:5<br>  \|<br>1 \|     print(repo.list("."))<br>  \|     ^^^^^<br>  \|<br> |

````python
print(repo.list("."))
print(repo.read("src/options.rs", missing_ok=True))

````

| 组 | run_id | act | 原始错误 |
| --- | --- | --- | --- |
| base | base-deepseek-A-probe-02-r1 | 3 | error: Variable `sum` not found, did you mean `str`?<br> --> script.star:5:13<br>  \|<br>5 \|     total = sum([s["port"] for s in enabled])<br>  \|             ^^^<br>  \|<br> |

````python
data = json.decode(repo.read("services.json"))
svcs = data["services"]
enabled = [s for s in svcs if s["enabled"]]
names = sorted([s["name"] for s in enabled])
total = sum([s["port"] for s in enabled])
return {"enabled_services": names, "total_port": total}
````

| 组 | run_id | act | 原始错误 |
| --- | --- | --- | --- |
| base | base-deepseek-A-probe-02-r2 | 1 | error: Variable `print` not found, did you mean `int`?<br> --> script.star:1:5<br>  \|<br>1 \|     print(repo.list("."))<br>  \|     ^^^^^<br>  \|<br> |

````python
print(repo.list("."))
````

### 唯一匹配失败

| 正式错误调用总数 | 附录脚本数 |
| --- | --- |
| 2 | 2 |

| 组 | run_id | act | 原始错误 |
| --- | --- | --- | --- |
| overview | overview-deepseek-B-batch-01-r3 | 2 | Traceback (most recent call last):<br>  * script.star:2, in <module><br>      repo.edit(f, "OldClient", "NewClient", all = True)<br>error: EditMatchError: src/untouched.txt: 匹配 0 次，要求至少 1 次<br> --> script.star:2:5<br>  \|<br>2 \|     repo.edit(f, "OldClient", "NewClient", all = True)<br>  \|     ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^<br>  \|<br> |

````python
for f in ["src/a.txt", "src/nested/b.txt", "src/untouched.txt"]:
    repo.edit(f, "OldClient", "NewClient", all = True)
result(repo.diff())
````

| 组 | run_id | act | 原始错误 |
| --- | --- | --- | --- |
| overview | overview-minimax-B-edit-04-r3 | 3 | Traceback (most recent call last):<br>  * script.star:1, in <module><br>      repo.edit("src/settings.py", "\nPORT = 8080\n", "\nPORT = 9090\n")<br>error: EditMatchError: src/settings.py: 匹配 0 次，要求恰好 1 次<br> --> script.star:1:1<br>  \|<br>1 \| repo.edit("src/settings.py", "\nPORT = 8080\n", "\nPORT = 9090\n")<br>  \| ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^<br>  \|<br> |

````python
repo.edit("src/settings.py", "\nPORT = 8080\n", "\nPORT = 9090\n")
result({"content": repo.read("src/settings.py"), "diff": repo.diff(), "status": repo.status()})
````

### 命令未预置

| 正式错误调用总数 | 附录脚本数 |
| --- | --- |
| 326 | 3 |

| 组 | run_id | act | 原始错误 |
| --- | --- | --- | --- |
| base | base-deepseek-A-rollback-01-r2 | 5 | Traceback (most recent call last):<br>  * script.star:4, in <module><br>      __act_result__ = __act_main__()<br>  * script.star:3, in __act_main__<br>      noRun = env.run(["cargo", "test", "--no-run"], check = False)<br>error: 模拟环境未提供该命令: ["cargo", "test", "--no-run"]<br> --> script.star:3:13<br>  \|<br>3 \|     noRun = env.run(["cargo", "test", "--no-run"], check = False)<br>  \|             ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^<br>  \|<br> |

````python
ls = env.run(["ls", "-a"], check = False)
find = env.run(["find", ".", "-name", "*test*", "-not", "-path", "./target/*"], check = False)
noRun = env.run(["cargo", "test", "--no-run"], check = False)
return {"ls": ls.stdout, "find": find.stdout, "noRun": noRun.stderr[-2000:], "noRunOut": noRun.stdout[-2000:]}
````

| 组 | run_id | act | 原始错误 |
| --- | --- | --- | --- |
| base | base-deepseek-A-rollback-02-r2 | 6 | Traceback (most recent call last):<br>  * script.star:14, in <module><br>      __act_result__ = __act_main__()<br>  * script.star:4, in __act_main__<br>      t2 = env.run(["cargo", "metadata", "--no-deps", "--format-version", "1"], che...<br>error: 模拟环境未提供该命令: ["cargo", "metadata", "--no-deps", "--format-version", "1"]<br> --> script.star:4:10<br>  \|<br>4 \|     t2 = env.run(["cargo", "metadata", "--no-deps", "--format-version", "1"], check = False)<br>  \|          ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^<br>  \|<br> |

````python
repo.write("src/cache.rs", "pub const CAPACITY: usize = 16;\n")
after = repo.read("src/cache.rs")
t1 = env.run(["cargo", "test"], check = False)
t2 = env.run(["cargo", "metadata", "--no-deps", "--format-version", "1"], check = False)
return {
  "file_now": after,
  "test_code": t1.code,
  "test_out": t1.stdout[-2000:],
  "test_err": t1.stderr[-2000:],
  "meta_code": t2.code,
  "meta_out": t2.stdout[:1500],
  "meta_err": t2.stderr[:1500],
  "status": [{"p": s.path, "k": s.kind} for s in repo.status()],
}
````

| 组 | run_id | act | 原始错误 |
| --- | --- | --- | --- |
| base | base-deepseek-A-rollback-03-r2 | 7 | Traceback (most recent call last):<br>  * script.star:11, in <module><br>      __act_result__ = __act_main__()<br>  * script.star:2, in __act_main__<br>      which = env.run(["which", "cargo"], check = False)<br>error: 模拟环境未提供该命令: ["which", "cargo"]<br> --> script.star:2:13<br>  \|<br>2 \|     which = env.run(["which", "cargo"], check = False)<br>  \|             ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^<br>  \|<br> |

````python
status = repo.status()
which = env.run(["which", "cargo"], check = False)
ver = env.run(["cargo", "--version"], check = False)
listing = env.run(["ls", "-la"], check = False)
return {
    "status": [(s.path, s.kind) for s in status],
    "file": repo.read("src/retry.rs"),
    "which_cargo": which.stdout.strip(),
    "cargo_version": (ver.stdout + ver.stderr).strip(),
    "ls": listing.stdout,
}
````

### 超出上限

| 正式错误调用总数 | 附录脚本数 |
| --- | --- |
| 1 | 1 |

| 组 | run_id | act | 原始错误 |
| --- | --- | --- | --- |
| composed | composed-minimax-A-rollback-04-r1 | 8 | OutputLimitExceeded |

````python
f = env.run(["find", "/", "-maxdepth", "5", "-name", "*.rs"], check=False)
g = env.run(["find", "/", "-maxdepth", "4", "-name", "*test*"], check=False)
return {"rs_files": f.stdout + f.stderr, "test_files": g.stdout + g.stderr}

````

### 其他

| 正式错误调用总数 | 附录脚本数 |
| --- | --- |
| 113 | 3 |

| 组 | run_id | act | 原始错误 |
| --- | --- | --- | --- |
| base | base-deepseek-A-batch-02-r3 | 1 | error: Parse error: unexpected keyword 'for', expected symbol ':'<br> --> script.star:5:28<br>  \|<br>5 \|     files = sorted({h.path for h in hits})<br>  \|                            ^^^<br>  \|<br> |

````python
hits = repo.search("legacy_flag", glob = "**")
res = []
for h in hits:
    res.append("%s:%d: %s" % (h.path, h.line, h.text))
files = sorted({h.path for h in hits})
return {"files": files, "count": len(hits), "sample": res[:40]}
````

| 组 | run_id | act | 原始错误 |
| --- | --- | --- | --- |
| base | base-deepseek-A-batch-03-r1 | 3 | error: Parse error: unexpected keyword 'for', expected symbol ':'<br> --> script.star:3:26<br>  \|<br>3 \|     distinct = sorted({m for h in variants for m in [h.text]})<br>  \|                          ^^^<br>  \|<br> |

````python
import_like = None
variants = repo.search("(?i)slow_path", glob="src/**")
distinct = sorted({m for h in variants for m in [h.text]})

for path in ["src/a.txt", "src/nested/b.txt"]:
    c = repo.read(path)
    if c.count("slow_path") > 0:
        repo.edit(path, "slow_path", "fast_path", all=True)

return {
    "distinct_lines": distinct,
    "diff": repo.diff(),
    "after": {p: repo.read(p) for p in ["src/a.txt", "src/nested/b.txt", "src/untouched.txt"]},
}

````

| 组 | run_id | act | 原始错误 |
| --- | --- | --- | --- |
| base | base-deepseek-A-batch-04-r1 | 1 | error: Parse error: unexpected keyword 'for', expected symbol ':'<br> --> script.star:4:31<br>  \|<br>4 \|       "files": sorted({h.path for h in hits}),<br>  \|                               ^^^<br>  \|<br> |

````python
hits = repo.search("V1_TAG")
return {
  "count": len(hits),
  "files": sorted({h.path for h in hits}),
}

````
