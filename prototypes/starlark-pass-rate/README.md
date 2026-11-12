# Starlark 通过率实验

本目录是 ADR 0002 接受前实验 1 的独立测试台，自带 Cargo workspace。v1 报告和原始记录保留在 [REPORT.md](REPORT.md)、`results/formal/`、`results/pilot/`；v2 见 [REPORT-v2.md](REPORT-v2.md) 与 `results/v2/`。重放 v1 应检出其原提交；本分支的宿主、判定器和驱动用于 v2。

## 运行与复核

在仓库根目录执行：

```sh
cargo build --manifest-path prototypes/starlark-pass-rate/Cargo.toml -j 4
python3 -m unittest discover -s prototypes/starlark-pass-rate/tests
python3 prototypes/starlark-pass-rate/language_probe.py
python3 prototypes/starlark-pass-rate/validate.py
python3 prototypes/starlark-pass-rate/report.py
```

首次实验依次运行 `driver.py --phase pilot --workers 4`、`freeze.py`、`driver.py --phase formal --workers 4`。试跑为 24 次，正式为 864 次。已有完成记录跳过，未结束记录拒绝覆盖；冻结清单只能创建一次。已有正式数据时不要重新生成输入。

驱动从 `~/.config/llm-keys/deepseek.key` 与 `~/.config/llm-keys/minimax-cn.key` 运行时读取密钥，在内存去首尾空白及末尾 `~`。MiniMax 并发固定为 1，deepseek 不超过 4；两家 assistant 消息字段（含 `reasoning_content`）完整回填，不记录请求头。

## 文件与记录

| 文件 | 职责 |
| --- | --- |
| `act-sim/` | Starlark 0.14.2、事务副本与 repo/env/text 接口 |
| `prompts/` | base/overview 共用 A/B 工具说明；composed 仅替换示例；共同 system 公开 8 次预算 |
| `tasks/`、`check.py` | 24 个目标、fixture、预置输出和统一判定；read-03 修复；其余 v1 判定保留 |
| `command-normalization.json` | 显式归一化表；Rust 宿主和 Python 判定器共用 |
| `driver.py` | 三组运行、独立模型线程池、重试、完整对话、最多 8 次 act |
| `metrics.py` | 宿主事件与脚本词法调用点分列；探索行为与 print 计数 |
| `freeze.py` | 试跑后冻结输入、运行代码、二进制与试跑哈希 |
| `tests/` | 事务语义、48 个参考用例、6 条历史假阴性、归一化、组合示例、预算与重试 |
| `validate.py` | 完整网格、冻结哈希、全部正式脚本离线回放、判定/指标/token 重算 |
| `report.py` | 从 v2 原始记录重建数据表和脚本原文附录 |
| `audit.py` | 读取 v1 冻结记录的原始 wc 假阴性审计；本轮不改写 v1 evidence |
| `create_tasks.py`、`prepare_prompts.py` | 历史生成器；正式运行后禁止执行 |

每次运行一个 JSONL，包含 `start`、`api_response`、`api_retry`、`act`、`budget_stop`、`summary`。`act` 保留脚本、结局、判定、指标、对应 API 响应序号与完整 usage；`summary` 包含最终文件哈希、首次写 act、预算耗尽与去重汇总 token。API 错误整次排除分母，单列数量。判定结果与观测指标不发送给被测模型。

`metrics.interfaces` 来自进入宿主的执行事件；`source_interfaces` 是排除注释和字符串的词法调用点。`print` 没有全局实现，按源代码调用点计数，含因此报错的脚本，不声称是动态循环次数。只读 act 要求至少一个读事件且无写、执行、discard、fail；单接口按不同接口种数为 1 计，另记仅一次调用。

`act-sim` 使用临时 fixture 副本，成功才提交；`reset/status/diff` 基线为本次 act 开始状态。返回 JSON 超过 65536 UTF-8 字节则丢弃。未实现沙箱，命令 cwd 为合成夹具副本；绝对路径仍可能读取副本之外的位置。本轮相关命令尝试与回放差异在报告中单列。
