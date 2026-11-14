# 实验 1 v3：显式 `commit()` 与撤销提示

## 结论

正式运行 144/144 次，状态 complete 143/144；沿用 v2 判定器的任务完成率为 135/143 (94.41%)。

在 edit/batch/test 三类中，文件已正确却未提交的 act 为 17/65 (26.15%)（分母为正常返回且候选文件正确的改动 act）；受影响运行 17/72 (23.61%)。真正要求写入的 edit/batch 运行遗漏率为 17/48 (35.42%)。

回滚任务在要求的候选修改上测试失败后仍带提交标记的比例为 0/24 (0.00%)，正确不提交 24/24 (100.00%)；回滚任务完成率 22/23 (95.65%)。

正确改动被撤销后，下一次 act 重做并提交比例为 15/17 (88.24%)；遗漏后最终完成 17/17 (100.00%)。

这批小型任务支持继续采用显式提交的默认撤销语义；没有无提示对照组，不能证明提示本身提高了完成率。重复仅 3 次、写入任务只有 8 个，模型版本和提供方也与 v2 不同，不能将完成率差异解释为语义修改的因果效果。

## 范围、输入与运行方式

| 项目 | 记录 |
| --- | --- |
| 日期（UTC） | 2026-11-14 |
| worktree | /home/gcp-vegas/.t3/worktrees/lancet/t3-6c35aeba |
| worktree HEAD | 4e16f24d4eca68d8282e2fe7c39892d3aafbec89 |
| 起点 | 按任务要求执行 git checkout origin/exp/act-v2 -- prototypes/starlark-pass-rate |
| 正式网格 | composed/A × 2 模型 × 24 任务 × 3 次 = 144；每任务最多 8 次 act |
| 试跑 | 每模型 read-02、probe-02、edit-01、batch-01、test-01、rollback-01 各 1 次，共 12；不并入正式分母 |
| 模式选择 | A 支持顶层 return，与 ADR 示例一致；B 使用 result()，本轮未运行 |
| 调度 | Random(20261114) 打乱任务；DeepSeek 4 并发、MiniMax 1 并发 |
| 请求参数 | 未指定温度/采样参数；max_tokens=8192（v2 未设该上限）；请求超时 180 秒 |
| 重试 | 沿用 v2：DeepSeek 最多 5 次、MiniMax 最多 10 次，指数退避封顶 120 秒；额外将 ConnectionResetError 纳入网络重试 |
| 冻结 | results/v3/evidence/freeze.json；输入 153 文件、测台 16 文件，含二进制 SHA256 与试跑哈希 |
| 解释器 | Starlark 0.14.2；保留 v2 纯计算环境、命令归一化表、24 个任务与判定器 |
| 返回上限 | 65536 UTF-8 字节；统一为提示预留 256 字节，脚本值上限 65280 字节；提示不额外触发失败 |
| 临时目录 | results/v3/tmp；模拟器的副本也放入夹具父目录；超时终止进程组 |
| 隔离范围 | 保留 v2 模拟器，非生产沙箱：真实只读命令可能访问绝对路径；正式前设置 GIT_CEILING_DIRECTORIES，防止夹具内 git 向上发现研究仓库。所有实际脚本的外部路径/写命令需结合证据审查 |

## 端点与实际返回标识

| 模型 | base URL | 请求模型 | 实际 model（响应数） | 实际提供方（响应数） |
| --- | --- | --- | --- | --- |
| DeepSeek（OpenRouter） | https://openrouter.ai/api/v1 | deepseek/deepseek-v4.1-flash | {"deepseek/deepseek-v4.1-flash": 287} | {"Makora": 4, "Wafer": 77, "DigitalOcean": 26, "Together": 5, "Cohere": 44, "DeepInfra": 53, "River": 17, "CoreWeave": 3, "OpenInference": 6, "BaseTen": 8, "Alibaba": 21, "Morph": 4, "AtlasCloud": 3, "Relace": 8, "Venice": 1, "InferenceNet": 6, "Reka": 1} |
| MiniMax（直连） | https://api.minimaxi.com/v1 | MiniMax-M3.1-Flash | {"MiniMax-M3.1-Flash": 462} | {"MiniMax (direct; inferred from endpoint)": 462} |

启动前查询 [DeepSeek endpoints](https://openrouter.ai/api/v1/models/deepseek/deepseek-v4.1-flash/endpoints)，快照保存在 results/v3/evidence/deepseek-endpoints.json；当时没有 provider_name=DeepSeek，因此未发送官方固定路由参数，使用 OpenRouter 默认路由。每个 api_response 保留完整响应及 actual_model/actual_provider；单次任务内部也可能切换提供方。MiniMax 预检返回 200，未启用 OpenRouter 回退；直连响应不含 provider 字段，记录明确标为基于端点推断，而不是伪称响应原文。

## 提示与结算语义

实际工具说明见 [act_tool_A_composed.md](prompts/act_tool_A_composed.md)。使用 v2 composed/A 的说明框架，仅替换结算段和示例；共同 system 与任务原文未改。接口移除 discard()，加入全局 commit()。只读或写后 reset 至基线无需提交；有净文件改动且未提交的正常返回仍为 Succeeded，副本不写回，receipt_summary 标注 uncommitted_reverted。commit 是本次调用的保留标记，不立即持久化；后续 fail、报错或超限仍撤销。

实际撤销提示原文（中文；其余提示主体沿用 v2 英文）：

> 未调用 commit()：本次对 N 个文件的修改已撤销；需要保留改动时请调用 commit()

交给模型的 tool 消息为序列化返回值，随后换行附提示；candidate_changes、候选正确性、任务判定和宿主事件只写审计日志，不交给模型。与 v2 的 outcome/value/error/line JSON 包装不同，这是为了让提示处在 content 的真实末尾；也属于比较中的可见格式差异。

ADR 示例适配项：lancet/dev 改为 v2 的 repo/env；省略 env 不支持的 cwd；保留 v2 命令白名单，将 cargo test --offline 写为 cargo test；TextView 仍需 str(...) 序列化。没有顺带开启 ADR 的 SetType/sum/Print、静态类型检查或完整资源沙箱，避免把本实验扩成整个 ADR 实现。

ADR 对提前 commit 未规定运行时拒绝；模拟器按整次调用标记执行，单独将“写入之前 commit”“commit 后 fail/错误”“测试失败之后 commit”“撤销后空提交（未重做修改）”列作误用观察，不擅自改变保留规则。

## 统计口径

| 指标 | 分母与判定 |
| --- | --- |
| 有效运行 | status=complete；api_error、harness_error、budget_stop 排除主分母并单列。失败运行保留，不因低通过率重跑 |
| 需要保留改动分组 | 按任务要求列 edit/batch/test 共 12 项 ×3；但 v2 的 4 个 test 只执行测试、汇报错误，expected 等于初始文件，根本不要求写入。真正写入组另列 edit/batch 8 项 ×3 |
| 遗漏 act | 正常返回、发生净变更、候选文件精确等于 spec.expected、未提交且被撤销。分母首选正常返回且候选文件正确的改动 act；同时给全部三类 act 分母 |
| 遗漏运行 | 至少一次上述遗漏；分母分别为 edit/batch/test 运行和真正写入运行。候选全判定通过的严格计数另列，区分文件正确与事实/先读后写时序 |
| 最终补救 | 分别报告最后完整任务判定通过、最后文件哈希正确 / 有遗漏运行；避免把旧时序判定失败误认为提交补救失败 |
| 下一次补救 | 撤销后紧邻 act 实际重新产生净变更且提交；必须来自后续 API 响应（同一响应的后续 tool_call 尚未看到提示，不算）。主分母为保留修改组全部提示；条件分母为确实存在下一次并看到提示的调用 |
| 回滚误提交 | 在任务规定的候选文件上执行指定测试并得到非零结果的 act 中，commit_called=true；另列实际在失败结果后调用，以及最终保留错误修改。提前 commit 后测失败同样是不安全标记 |
| 正确不提交 | 同一回滚测试分母中无 commit 调用，包含正常返回或 fail/error 撤销；最终任务完成还要求返回失败事实 |
| 只读多余提交 | read/probe 任务中的真实 commit 宿主事件，不将未执行分支的词法调用点算入 |
| 表 1–4 | 沿用 v2：解析与运行通过、任务完成、原六类错误、失败后下一次运行通过。下一次“运行通过”不等于任务完成；commit 误用另列可与原错误重叠 |
| tokens | 按 api_response 去重汇总，含最终答复与预算后答复；不重复相加各 act 关联的 usage |

## 试跑与异常状态

| 模型 | 试跑完成率 | 试跑状态 | 正式计划/有效 | 正式状态 |
| --- | --- | --- | --- | --- |
| DeepSeek（OpenRouter） | 6/6 (100.00%) | {"complete": 6} | 72/72 | {"complete": 72} |
| MiniMax（直连） | 6/6 (100.00%) | {"complete": 6} | 72/71 | {"complete": 71, "harness_error": 1} |

| 试跑未通过 run_id | 判定原因 |
| --- | --- |
| 无 | — |

## 中止批次与重新运行

初次正式运行发现 Git 隔离缺陷：临时夹具位于本 worktree 内，GIT_CEILING_DIRECTORIES 最初设为夹具自身，不能阻止 Git 向上查找。git status 因而返回研究仓库的状态并触发虚假的 OutputLimitExceeded。修复为夹具父目录，并新增端到端回归；不改变任务、验收或命令归一化表。

| 初次批次 | 数量 |
| --- | --- |
| 试跑 | 12 |
| 正式已启动 | 105 |
| 正式已结束 | 104 |
| 强制中止 | 1 |
| 尚未启动 | 39 |

发现后终止唯一驱动，保存全部原始记录、初次冻结清单和匹配其哈希的测台源码至 results/v3/attempt-1/，没有覆盖原始对话。初次正式批次整体排除主分析；随后重新执行 12 次试跑、重新冻结并运行完整 144 次网格，避免只重跑受影响或失败样本。此次额外运行偏离原计划，全部 OpenRouter 费用和已返回的 MiniMax usage 都纳入本实验成本。中止时一条 MiniMax 在途请求没有返回 usage，其 token 无法核算，不作零值估计。

| 初次批次阶段 | 已结束任务完成率（仅作审计） |
| --- | --- |
| pilot | 11/12 (91.67%) |
| formal | 98/104 (94.23%) |

| 初次批次未通过运行 | 阶段 | 原因 |
| --- | --- | --- |
| composed-deepseek-A-probe-02-r1 | pilot | 缺少事实: 16300；结构化返回的排序或合计不符 |
| composed-deepseek-A-probe-01-r2 | formal | 缺少事实: optional missing |
| composed-deepseek-A-read-03-r3 | formal | 行数未与文件对应: a.txt；行数未与文件对应: b.txt |
| composed-deepseek-A-probe-01-r3 | formal | 缺少事实: optional missing |
| composed-deepseek-A-probe-01-r1 | formal | 缺少事实: optional missing |
| composed-minimax-A-edit-01-r2 | formal | 未在较早一次 act 读取目标后再修改 |
| composed-minimax-A-batch-01-r3 | formal | 最终文件不符合期望 |

初次批次 API 重试事件 2 次，原文均保留在对应 JSONL；超时未返回 usage 的请求同样无法计入 MiniMax token 合计，以下仅报告已返回的用量。

初次批次 Git 命令审计仅见 diff、status、ls-files、rev-parse 等读取命令，见 attempt-1/evidence/git-command-audit.json；源文件与旧报告的最终完整性另由验证器检查。完整中止/未启动清单见 attempt-1/evidence/interruption.json，归档哈希见 attempt-1/evidence/archive-integrity.json。

## 表 1：每次 act 调用

| 模型 | act 数 | 解析通过率 | 运行通过率 |
| --- | --- | --- | --- |
| DeepSeek（OpenRouter） | 219 | 214/219 (97.72%) | 186/219 (84.93%) |
| MiniMax（直连） | 406 | 391/406 (96.31%) | 312/406 (76.85%) |
| 合计 | 625 | 605/625 (96.80%) | 498/625 (79.68%) |

## 表 2：每个任务

| 模型 | 任务完成率 | 首次 act 完成率 | 平均 act 次数 | 用尽 8 次预算 |
| --- | --- | --- | --- | --- |
| DeepSeek（OpenRouter） | 68/72 (94.44%) | 19/72 (26.39%) | 219/72 = 3.0417 | 3 |
| MiniMax（直连） | 67/71 (94.37%) | 14/71 (19.72%) | 406/71 = 5.7183 | 25 |
| 合计 | 135/143 (94.41%) | 33/143 (23.08%) | 625/143 = 4.3706 | 28 |

| 任务类别 | DeepSeek（OpenRouter） | MiniMax（直连） |
| --- | --- | --- |
| read | 12/12 (100.00%) | 12/12 (100.00%) |
| probe | 8/12 (66.67%) | 9/12 (75.00%) |
| edit | 12/12 (100.00%) | 12/12 (100.00%) |
| batch | 12/12 (100.00%) | 12/12 (100.00%) |
| test | 12/12 (100.00%) | 12/12 (100.00%) |
| rollback | 12/12 (100.00%) | 10/11 (90.91%) |

## 表 3：错误分类与 commit 误用

| 模型 | Python 习惯 | 接口误用 | 唯一匹配失败 | 命令未预置 | 超出上限 | 其他 | commit 误用 act |
| --- | --- | --- | --- | --- | --- | --- | --- |
| DeepSeek（OpenRouter） | 3 | 20 | 1 | 2 | 0 | 7 | 0 |
| MiniMax（直连） | 15 | 23 | 0 | 45 | 0 | 11 | 1 |
| 合计 | 18 | 43 | 1 | 47 | 0 | 18 | 1 |

原六类只统计 ran=false 的 act，互斥；新增 commit 误用是独立观察标签，可出现在正常返回或报错 act 中，不能直接与前六列相加当成总错误数。未提交遗漏也不属于解释器运行失败。

## 表 4：失败后下一次调用运行通过

| 模型 | 错误类别 | 下次运行通过 / 有下次 act | 无下次 act |
| --- | --- | --- | --- |
| DeepSeek（OpenRouter） | Python 习惯 | 1/3 (33.33%) | 0 |
| DeepSeek（OpenRouter） | 接口误用 | 18/20 (90.00%) | 0 |
| DeepSeek（OpenRouter） | 唯一匹配失败 | 1/1 (100.00%) | 0 |
| DeepSeek（OpenRouter） | 命令未预置 | 2/2 (100.00%) | 0 |
| DeepSeek（OpenRouter） | 超出上限 | 0/0（不可估计） | 0 |
| DeepSeek（OpenRouter） | 其他 | 6/7 (85.71%) | 0 |
| MiniMax（直连） | Python 习惯 | 7/15 (46.67%) | 0 |
| MiniMax（直连） | 接口误用 | 19/23 (82.61%) | 0 |
| MiniMax（直连） | 唯一匹配失败 | 0/0（不可估计） | 0 |
| MiniMax（直连） | 命令未预置 | 23/34 (67.65%) | 11 |
| MiniMax（直连） | 超出上限 | 0/0（不可估计） | 0 |
| MiniMax（直连） | 其他 | 7/11 (63.64%) | 0 |

| 模型 | 独立观察类别 | 下次运行通过 / 有下次 act | 无下次 act |
| --- | --- | --- | --- |
| DeepSeek（OpenRouter） | commit 误用 | 0/0（不可估计） | 0 |
| MiniMax（直连） | commit 误用 | 1/1 (100.00%) | 0 |

## 提交、遗漏与最终补救

| 指标 | DeepSeek（OpenRouter） | MiniMax（直连） | 合计 |
| --- | --- | --- | --- |
| edit/batch/test 完成率 | 36/36 (100.00%) | 36/36 (100.00%) | 72/72 (100.00%) |
| edit/batch 真正写入完成率 | 24/24 (100.00%) | 24/24 (100.00%) | 48/48 (100.00%) |
| 改动正确却遗漏 / 正常且文件正确的改动 act | 0/24 (0.00%) | 17/41 (41.46%) | 17/65 (26.15%) |
| 改动正确却遗漏 / 三类全部 act | 0/111 (0.00%) | 17/220 (7.73%) | 17/331 (5.14%) |
| 有遗漏运行 / 三类运行 | 0/36 (0.00%) | 17/36 (47.22%) | 17/72 (23.61%) |
| 有遗漏运行 / 真正写入运行 | 0/24 (0.00%) | 17/24 (70.83%) | 17/48 (35.42%) |
| 遗漏后最终完整判定通过 / 有遗漏运行 | 0/0（不可估计） | 17/17 (100.00%) | 17/17 (100.00%) |
| 遗漏后最终文件正确 / 有遗漏运行 | 0/0（不可估计） | 17/17 (100.00%) | 17/17 (100.00%) |

| 模型 | 三类全部 act | 有净变更 act（含失败） | 正常有净变更 act | 候选完整判定通过但被撤销 act | 全部未提交撤销 act |
| --- | --- | --- | --- | --- | --- |
| DeepSeek（OpenRouter） | 111 | 25 | 24 | 0 | 0 |
| MiniMax（直连） | 220 | 43 | 41 | 17 | 17 |
| 合计 | 331 | 68 | 65 | 17 | 17 |

| 遗漏 run_id | act | 候选完整判定通过 | 最终任务通过 |
| --- | --- | --- | --- |
| composed-minimax-A-batch-01-r2 | 5 | True | True |
| composed-minimax-A-batch-01-r3 | 3 | True | True |
| composed-minimax-A-batch-02-r1 | 4 | True | True |
| composed-minimax-A-batch-02-r2 | 5 | True | True |
| composed-minimax-A-batch-02-r3 | 5 | True | True |
| composed-minimax-A-batch-03-r1 | 5 | True | True |
| composed-minimax-A-batch-03-r2 | 3 | True | True |
| composed-minimax-A-batch-03-r3 | 7 | True | True |
| composed-minimax-A-batch-04-r1 | 4 | True | True |
| composed-minimax-A-batch-04-r3 | 6 | True | True |
| composed-minimax-A-edit-01-r3 | 3 | True | True |
| composed-minimax-A-edit-03-r1 | 4 | True | True |
| composed-minimax-A-edit-03-r2 | 3 | True | True |
| composed-minimax-A-edit-03-r3 | 3 | True | True |
| composed-minimax-A-edit-04-r1 | 3 | True | True |
| composed-minimax-A-edit-04-r2 | 2 | True | True |
| composed-minimax-A-edit-04-r3 | 2 | True | True |

## 回滚与错误保留

| 指标 | DeepSeek（OpenRouter） | MiniMax（直连） | 合计 |
| --- | --- | --- | --- |
| 测试失败 act 仍有提交标记 | 0/12 (0.00%) | 0/12 (0.00%) | 0/24 (0.00%) |
| 测试失败结果之后实际调用 commit | 0/12 (0.00%) | 0/12 (0.00%) | 0/24 (0.00%) |
| 测试失败 act 正确不提交 | 12/12 (100.00%) | 12/12 (100.00%) | 24/24 (100.00%) |
| 其中正常返回且不提交 | 12/12 (100.00%) | 12/12 (100.00%) | 24/24 (100.00%) |
| 其中 fail/错误中止且不提交 | 0/12 (0.00%) | 0/12 (0.00%) | 0/24 (0.00%) |
| 测试失败 act 最终保留错误修改 | 0/12 (0.00%) | 0/12 (0.00%) | 0/24 (0.00%) |
| 有误提交运行 / 观察到候选测试失败的运行 | 0/12 (0.00%) | 0/10 (0.00%) | 0/22 (0.00%) |
| 正确不提交运行 / 观察到候选测试失败的运行 | 12/12 (100.00%) | 10/10 (100.00%) | 22/22 (100.00%) |

未在规定候选修改上观察到测试失败的回滚运行：DeepSeek（OpenRouter） 0；MiniMax（直连） 1。这些运行仍计入任务完成率，不混入上述条件分母。

## 撤销提示后的下一次 act

| 范围 | 模型 | 下一次重做并提交 / 全部撤销 | 下一次重做并提交 / 已见提示且有下次 act | 无下次 act | 同 API 响应的下次 act |
| --- | --- | --- | --- | --- | --- |
| 正确改动遗漏 | DeepSeek（OpenRouter） | 0/0（不可估计） | 0/0（不可估计） | 0 | 0 |
| 正确改动遗漏 | MiniMax（直连） | 15/17 (88.24%) | 15/17 (88.24%) | 0 | 0 |
| 正确改动遗漏 | 合计 | 15/17 (88.24%) | 15/17 (88.24%) | 0 | 0 |
| 需保留组三类全部撤销 | DeepSeek（OpenRouter） | 0/0（不可估计） | 0/0（不可估计） | 0 | 0 |
| 需保留组三类全部撤销 | MiniMax（直连） | 15/17 (88.24%) | 15/17 (88.24%) | 0 | 0 |
| 需保留组三类全部撤销 | 合计 | 15/17 (88.24%) | 15/17 (88.24%) | 0 | 0 |
| 全部类别（含主动回滚） | DeepSeek（OpenRouter） | 0/12 (0.00%) | 0/6 (0.00%) | 6 | 0 |
| 全部类别（含主动回滚） | MiniMax（直连） | 15/30 (50.00%) | 15/28 (53.57%) | 2 | 0 |
| 全部类别（含主动回滚） | 合计 | 15/42 (35.71%) | 15/34 (44.12%) | 8 | 0 |

| 未在紧邻下一次 act 补救的遗漏 | 遗漏 act | 下一次实际结局 | 最终完整任务通过 |
| --- | --- | --- | --- |
| composed-minimax-A-batch-02-r2 | 5 | parse_error | True |
| composed-minimax-A-edit-03-r3 | 3 | read_only | True |

本轮两次延迟补救：batch-02-r2 的下一次脚本用了不支持的 assert，解析失败，再下一次成功；edit-03-r3 的下一次仅调用 commit() 并读取文件，没有重做已撤销的编辑，再下一次才重做并提交。这后一种是实测的跨调用空提交误用，不能记作即时补救成功。

全类别分母包含任务要求的正常回滚，下一次不重做/不提交通常是正确行为；该行只描述提示暴露，不可当作遗漏补救失败率。

遗漏/补救实例：[composed-minimax-A-batch-01-r2](results/v3/formal/composed-minimax-A-batch-01-r2.jsonl)，act 5 原文：

```python
for f in ["src/a.txt", "src/nested/b.txt"]:
    repo.edit(f, "OldClient", "NewClient", all=True)

after_src = repo.search("OldClient", glob = "src/**")
after_any = repo.search("OldClient", glob = "**")
status = repo.status()
diff = repo.diff()
return {
  "remaining_in_src": after_src,
  "remaining_anywhere": after_any,
  "status": status,
  "diff": diff,
}
```

该次返回的最后一行：`未调用 commit()：本次对 2 个文件的修改已撤销；需要保留改动时请调用 commit()`。

紧邻下一次 act 原文：

```python
for f in ["src/a.txt", "src/nested/b.txt"]:
    repo.edit(f, "OldClient", "NewClient", all=True)

remaining_src = repo.search("OldClient", glob = "src/**")
remaining_docs = repo.search("OldClient", glob = "docs/**")
files = {
  "src/a.txt": repo.read("src/a.txt"),
  "src/nested/b.txt": repo.read("src/nested/b.txt"),
  "src/untouched.txt": repo.read("src/untouched.txt"),
  "docs/history.txt": repo.read("docs/history.txt"),
}
commit()
return {"remaining_src": remaining_src, "remaining_docs": remaining_docs, "files": files, "status": repo.status()}
```

## 只读任务与 v2 对照

| 模型 | read/probe 完成率 | v2 composed/A read/probe | 多余 commit 事件数 | 有 commit 的 act / 只读类别 act | 有 commit 的运行 / 只读类别运行 |
| --- | --- | --- | --- | --- | --- |
| DeepSeek（OpenRouter） | 20/24 (83.33%) | 19/23 (82.61%) | 0 | 0/51 (0.00%) | 0/24 (0.00%) |
| MiniMax（直连） | 21/24 (87.50%) | 20/24 (83.33%) | 0 | 0/98 (0.00%) | 0/24 (0.00%) |

| 模型 | v2 composed/A 总完成率 | v3 composed/A 总完成率 | 差值（百分点） | v2 composed A+B 总完成率 | v2→v3 回滚完成率（A） |
| --- | --- | --- | --- | --- | --- |
| DeepSeek（OpenRouter） | 64/71 (90.14%) | 68/72 (94.44%) | +4.30 | 132/143 (92.31%) | 11/12 (91.67%) → 12/12 (100.00%) |
| MiniMax（直连） | 60/70 (85.71%) | 67/71 (94.37%) | +8.65 | 119/142 (83.80%) | 4/10 (40.00%) → 10/11 (90.91%) |
| 合计 | 124/141 (87.94%) | 135/143 (94.41%) | +6.46 | 251/285 (88.07%) | 15/22 (68.18%) → 22/23 (95.65%) |

v2 DeepSeek 请求 deepseek-flash，直连 https://api.deepseek.com；v3 请求 deepseek/deepseek-v4.1-flash，经 OpenRouter 且非官方固定提供方，不能确认两者为同一权重/服务。MiniMax 请求名与直连端点相同。v2 composed/A 的有效分母分别为 71、70（各 72 计划运行中的测台异常已排除）；A+B 为 143、142。比较同时受版本、路由、示例、返回包装、输出 token 上限与随机调度变化影响。

## 失败与误用个案

| run_id | 状态 | 判定原因或测台错误 | act 数 | 最终回复见原记录 |
| --- | --- | --- | --- | --- |
| composed-deepseek-A-probe-01-r1 | complete | 缺少事实: optional missing | 1 | [JSONL](results/v3/formal/composed-deepseek-A-probe-01-r1.jsonl) |
| composed-deepseek-A-probe-01-r2 | complete | 缺少事实: optional missing | 1 | [JSONL](results/v3/formal/composed-deepseek-A-probe-01-r2.jsonl) |
| composed-deepseek-A-probe-01-r3 | complete | 缺少事实: optional missing | 1 | [JSONL](results/v3/formal/composed-deepseek-A-probe-01-r3.jsonl) |
| composed-deepseek-A-probe-04-r2 | complete | 缺少事实: 5；缺少事实: true；结构化返回的排序或合计不符；未返回存在性布尔值 true | 2 | [JSONL](results/v3/formal/composed-deepseek-A-probe-04-r2.jsonl) |
| composed-minimax-A-probe-01-r1 | complete | 缺少事实: optional missing | 1 | [JSONL](results/v3/formal/composed-minimax-A-probe-01-r1.jsonl) |
| composed-minimax-A-probe-01-r2 | complete | 缺少事实: optional missing | 4 | [JSONL](results/v3/formal/composed-minimax-A-probe-01-r2.jsonl) |
| composed-minimax-A-probe-01-r3 | complete | 缺少事实: optional missing | 4 | [JSONL](results/v3/formal/composed-minimax-A-probe-01-r3.jsonl) |
| composed-minimax-A-rollback-02-r2 | harness_error | act-sim 超时，已终止整个进程组 | 6 | [JSONL](results/v3/formal/composed-minimax-A-rollback-02-r2.jsonl) |
| composed-minimax-A-rollback-03-r2 | complete | 未在要求的修改上运行测试 | 8 | [JSONL](results/v3/formal/composed-minimax-A-rollback-03-r2.jsonl) |

| run_id | act | commit 误用标签 |
| --- | --- | --- |
| composed-minimax-A-edit-03-r3 | 4 | 撤销后空提交（未重做修改） |

MiniMax rollback-02-r2 在已记录 6 次 act 后，下一次脚本对 / 递归 grep memory_budget，超过 60 秒并被终止进程组；没有该次的完整宿主回执。整次运行排除主分母，其前 6 条回执仍离线复核，最后一次请求的原脚本和 usage 在 api_response 中保留。rollback-03-r2 则未实际修改文件就耗尽 8 次预算，属任务失败，仍计入完成率分母。

以上严格完成率不作事后纠正。仅在最终自然语言给出事实、返回值格式未被旧判定器接受、同一 act 读写违反 edit 时序等情况，均须查原始对话再讨论验收口径；它们不能归因为忘记 commit。

## 费用与 token

| 项 | 值 |
| --- | --- |
| OpenRouter GET /key 开始 usage（USD） | 0 |
| OpenRouter GET /key 结束 usage（USD） | 0.101475548 |
| OpenRouter usage 差额（USD，含中止批次与两轮试跑） | 0.101475548 |
| OpenRouter 响应 usage.cost 合计（USD，交叉核对） | 0.101475648436 |
| 费用上限 | 5 USD；每次请求前检查 key usage 与本地响应成本较大值，并预留并发请求的费用上界 |
| MiniMax 预检 tokens | 208 |
| MiniMax 已返回 tokens（含预检、中止批次、两轮试跑和正式） | 2427802 |

| 模型 | 阶段 | prompt tokens | completion tokens | total tokens |
| --- | --- | --- | --- | --- |
| DeepSeek（OpenRouter） | 试跑 | 48325 | 2846 | 51171 |
| MiniMax（直连） | 试跑 | 109495 | 7005 | 116500 |
| DeepSeek（OpenRouter） | 正式 | 671727 | 46562 | 718289 |
| MiniMax（直连） | 正式 | 1352271 | 95341 | 1447612 |

| 中止批次 MiniMax 输入 | 中止批次 MiniMax 输出 | 中止批次 MiniMax 总 token |
| --- | --- | --- |
| 807370 | 56112 | 863482 |

| MiniMax 合计输入 | MiniMax 合计输出 | MiniMax 合计总 token |
| --- | --- | --- |
| 2269341 | 158461 | 2427802 |

修复后试跑与正式运行的 finish_reason 分布：{"tool_calls": 682, "stop": 128}。API 重试事件 0 次。单次请求的 8192 输出 token 上限是否实际截断，见 length 计数。

## 验证、交付与未覆盖范围

实际验证命令：

```sh
cargo build --manifest-path prototypes/starlark-pass-rate/Cargo.toml -j 4
TMPDIR="$PWD/prototypes/starlark-pass-rate/results/v3/tmp" python3 -m unittest discover -s prototypes/starlark-pass-rate/tests
python3 prototypes/starlark-pass-rate/freeze.py
python3 prototypes/starlark-pass-rate/validate_v3.py --phase pilot
python3 prototypes/starlark-pass-rate/validate_v3.py --phase formal
python3 prototypes/starlark-pass-rate/report_v3.py
python3 prototypes/starlark-pass-rate/crosscheck_v3.py
python3 prototypes/starlark-pass-rate/audit_v3.py
python3 prototypes/starlark-pass-rate/check_docs_v3.py
```

pilot 离线验证：`{"phase": "pilot", "runs": 12, "acts": 49, "grid_complete": true, "peaks": {"deepseek": 4, "minimax": 1}, "states_and_checks_mismatched": 0, "value_differences": 3, "old_evidence_and_tasks_unchanged": true, "docs_unchanged": true, "no_keys": true, "statuses": {"complete": 12}}`。

formal 离线验证：`{"phase": "formal", "runs": 144, "acts": 631, "grid_complete": true, "peaks": {"deepseek": 4, "minimax": 1}, "states_and_checks_mismatched": 0, "value_differences": 31, "old_evidence_and_tasks_unchanged": true, "docs_unchanged": true, "no_keys": true, "statuses": {"complete": 143, "harness_error": 1}}`。

文档链接验证：`{"command": "cargo xtask docs-check（临时 GIT_INDEX_FILE，包含未跟踪交付文件）", "exit_code": 0, "real_index_unchanged": true}`。直接在真实索引运行第一次检查时，尚未跟踪的 REPORT-v3.md 被判为不存在；该失败保留于 docs-check-untracked.txt。临时索引仅声明交付文件参与检查，没有修改检查器或真实暂存区。

独立 SQL 重算通过：完成率、正确改动遗漏 act/运行、最终补救、紧邻下一次补救、只读 commit 计数均与报告一致，见 independent-crosscheck.json。

回放返回值差异审计：{"ls 元数据": 32, "临时副本目录名与 ls 元数据": 2}；不修改原始值。

22 项单元/集成测试见 results/v3/evidence/tests.txt；覆盖正常/未提交/报错结算、提示真实传入下一轮、候选态判定、并发费用预留、48 个参考任务实例（24×A/B）、v2 历史假阴性、命令归一化、预算、reasoning 回填与密钥不落日志。测试先在旧语义上失败，证据为 tdd-before.txt。

旧 REPORT.md、REPORT-v2.md、results/v2/、v1 记录、原任务、check.py 与命令归一化表逐字节对比 origin/exp/act-v2；docs/ 对比本 worktree HEAD。正式记录、API 原文、工具提示、可见 tool 消息、事件、候选改动、最终文件哈希、指标与费用均在 results/v3/。

临时目录偏离：最初在旧实现上运行的 3 条 TDD 失败回归沿用了默认 /tmp，而非本 worktree；这些测试的临时目录已由清理回调删除。随后将模拟器副本、所有模型试跑/正式运行、回放及后续测试统一放入本 worktree 的 results/v3/tmp。没有修改其他 worktree 或 ~/Dev 下的源文件。

未提交、未推送、未写 GitHub、未修改 docs/adr/。没有运行全仓 CI：本次只改独立原型，使用该原型构建、测试与已记录回执的全量回放。没有扩展到生产的并发文件冲突、沙箱、seam、崩溃恢复或跨 act revert；v2 的命令白名单、静态 canned 测试输出和纯计算缺项仍可能影响模型表现。

## 建议维护者裁决

ADR 更新提案（本次未修改 ADR）：在‘已核实的事实’新增本轮冻结记录，注明正式完成率 135/143 (94.41%)、正确改动遗漏运行 17/72 (23.61%)、真正写入运行遗漏 17/48 (35.42%)、遗漏后下一次重做并提交 15/17 (88.24%)、回滚误提交 0/24 (0.00%)，并分模型引用本报告。第 3 节维持显式 commit 与 content 末尾提示；‘尚未核实’仍应保留复杂真实项目上的外推及提示因果效应。

1. 继续保留显式 commit 与未提交自动撤销；本轮结果应作为小样本行为证据，不将零事件写成“不会遗漏”。
2. 下一轮专门制造“正确写入但漏提交”的可控提示暴露，并设置有/无提示对照，才能测量提示补救效应；本轮不追加诱导样本污染自然遗漏率。
3. 是否将最终自然语言中正确事实纳入普通任务验收，以及 edit 是否允许同次 act 先读后写，应单独裁决；本轮保持 v2 判定。
4. 若要比较模型本身，应在下一轮固定同一可用提供方并记录版本，避免 OpenRouter 在一次任务内换提供方；当前没有官方 DeepSeek 端点。
5. 是否把撤销提示改为更明确的‘未调用 commit()：本次对 N 个文件的修改已撤销；需要保留时请重新执行修改，并在同一次 act 中调用 commit()’，以避免撤销后的空提交。本轮实际提示仍是前文默认原文，新措辞只是待裁决提案。提前 commit 的运行时限制也需另行裁决，当前遵循整次调用标记。

