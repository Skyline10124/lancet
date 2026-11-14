# Starlark 通过率实验

本目录是 ADR 0002 接受前实验 1 的独立测台，自带 Cargo workspace。当前实现用于 v3：产生净文件变更的脚本必须调用 `commit()` 才保留，正常返回但未提交时自动撤销并附提示。实验范围、分母、端点与限制见 [REPORT-v3.md](REPORT-v3.md)。

首轮因 Git 向上发现研究仓库的隔离缺陷而整体中止，记录与冻结源码保留于 `results/v3/attempt-1/`；修复后重新试跑和运行完整网格，主分析仅使用重新运行的数据，费用包含两批。

历史报告及原始记录保持原样：[REPORT.md](REPORT.md) 对应 v1；[REPORT-v2.md](REPORT-v2.md) 对应 v2。重放或重新生成历史实验须检出对应版本；不要在当前实现上运行旧 `report.py`、`validate.py`、`prepare_prompts.py` 或 `create_tasks.py`。

## 复核

在仓库根目录执行：

```sh
cargo build --manifest-path prototypes/starlark-pass-rate/Cargo.toml -j 4
mkdir -p prototypes/starlark-pass-rate/results/v3/tmp
TMPDIR="$PWD/prototypes/starlark-pass-rate/results/v3/tmp" python3 -m unittest discover -s prototypes/starlark-pass-rate/tests
python3 prototypes/starlark-pass-rate/validate_v3.py --phase pilot
python3 prototypes/starlark-pass-rate/validate_v3.py --phase formal
python3 prototypes/starlark-pass-rate/report_v3.py
python3 prototypes/starlark-pass-rate/crosscheck_v3.py
python3 prototypes/starlark-pass-rate/audit_v3.py
python3 prototypes/starlark-pass-rate/check_docs_v3.py
```

验证器离线回放全部已有回执的脚本，核对最终文件、撤销前候选态、判定、token、提示和并发；对比 v2 的历史证据、24 个任务、`check.py` 与命令归一化表，保证未改。密钥检查只在内存读取 OpenRouter 和 MiniMax 密钥并检查记录中不包含值，不访问旧 DeepSeek 密钥。`report_v3.py` 只写 `REPORT-v3.md` 和 v3 派生指标。

## 实验运行

本次已经完成的记录不能覆盖。`driver.py --phase pilot --workers 4` 运行每模型每类任务一个实例，共 12 次；`freeze.py` 在无 API/测台异常的试跑后仅创建一次冻结清单；`driver.py --phase formal --workers 4` 运行 composed/A ×2 模型 ×24 任务 ×3 次，共 144 次。已有完成记录跳过，未结束记录拒绝覆盖；每任务最多 8 次 act，预算耗尽后允许一条最终答复。

运行前必须创建 `results/v3/evidence/usage-start.json` 与 DeepSeek endpoints 快照；这些文件与本次数据绑定，不可借旧基线另开收费实验。请求密钥在运行时从用户指定的文件读取，去首尾空白与末尾 `~`，不写请求头、不输出值。当前配置为 DeepSeek 经 OpenRouter、MiniMax 直连；端点查询与直连预检见 v3 evidence。DeepSeek 最多 4 并发、MiniMax 固定 1 并发。

`budget.py` 在每次 OpenRouter 请求前查 key usage，并与本地响应成本交叉核对；在途请求按 UTF-8 请求字节数、8192 输出 token 上限、端点最高单价的两倍预留。费用与预留超过 5 美元时停止新增请求。结束后查询 usage 并保存 `usage-end.json`；这份查询及响应成本共同用于报告，不能只拿 token 估算实际金额。

## 文件与证据

| 路径 | 职责 |
| --- | --- |
| `act-sim/` | Starlark 0.14.2、事务副本、repo/env/text、commit 标记、撤销提示 |
| `driver.py`、`budget.py` | 对话驱动、预算、并发、请求成本保护、候选态与可见消息记录 |
| `prompts/act_tool_A_composed.md` | 本次唯一使用的工具说明与 ADR 控制流程示例；其余提示保留历史版本 |
| `prompts/system.md`、`tasks/`、`check.py` | 沿用 v2 的系统提示、24 个任务及判定器 |
| `metrics.py` | 分离真实宿主事件与脚本词法调用点 |
| `freeze.py` | 冻结输入、测台、测试、二进制与试跑哈希 |
| `validate_v3.py`、`report_v3.py` | 离线全量回放、核对与指标重算、报告生成 |
| `tests/` | 事务、提示、候选态、费用保护、旧判定器和重试的回归测试 |
| `results/v3/pilot/`、`results/v3/formal/` | 每次运行的完整 JSONL 对话、API 响应、act 脚本和事件 |
| `results/v3/evidence/` | 端点快照、费用、冻结、测试、回放差异、指标与审计 |

成功的 act 交回序列化返回值，未提交撤销时在末尾追加中文提示；候选改动、正确性与评测结果仅写日志，不交给模型。`commit()` 是整次调用标记，后续错误仍撤销。固定为提示预留 256 字节，返回值预算为 65280 字节，交回内容总上限 65536 字节。

本原型不是生产沙箱。`status/diff/reset` 基线为当次 act 开始，事务用临时副本模拟；真实命令仅按 v2 白名单运行，测试命令使用固定输出。未实现外部写者冲突检测、检查点文件类型语义、静态类型检查、完整标准库或网络隔离。
