# Starlark 通过率实验

本目录是 ADR 0002 接受前实验 1 的独立测试台。正式数据和统计见 [REPORT.md](REPORT.md)，原始数据位于 `results/formal/`，试跑位于 `results/pilot/`。本目录自带 Cargo workspace。

## 运行与复核

在仓库根目录执行：

```sh
cargo build --manifest-path prototypes/starlark-pass-rate/Cargo.toml -j 4
python3 -m unittest discover -s prototypes/starlark-pass-rate/tests
python3 prototypes/starlark-pass-rate/validate.py
python3 prototypes/starlark-pass-rate/audit.py
python3 prototypes/starlark-pass-rate/report.py
```

重新调用模型会产生 API 费用。`driver.py --phase pilot|formal --workers 4` 从 `~/.config/llm-keys/deepseek.key` 和 `~/.config/llm-keys/minimax-cn.key` 读取密钥；只在内存中去首尾空白及末尾 `~`，不修改文件。两家 assistant 消息的原始字段（包括 `reasoning_content`）完整回送给 API。运行日志不记录请求头。

正式运行前必须存在 `evidence/freeze.json`，驱动在开始与结束时核验 `prompts/`、`tasks/`、`check.py` 的 SHA256；已完成 run_id 不重复调用，不覆盖未结束日志。再次实验须使用独立副本并保留原始记录。

## 文件与记录

| 文件 | 职责 |
| --- | --- |
| `act-sim/` | Starlark 0.14.2、事务副本、repo/env/text 接口 |
| `prompts/` | 从指定草稿提取并作两处事实修正的模型提示词 |
| `tasks/` | 24 个 task.md、fixture、canned.json、spec.json、check.py |
| `check.py` | 统一判定；任务目录 check.py 只是 CLI 入口 |
| `driver.py` | 4 并发、请求重试、完整对话历史、最多 4 次 act |
| `language_probe.py` | 35 项语法/标准库的 A/B 实测 |
| `tests/` | 宿主事务与返回边界、48 个参考解、重试与分类 |
| `validate.py` | 网格完整性、哈希、所有正式脚本离线回放、摘要重算 |
| `audit.py` | 单列冻结判定器的 wc -l 排列假阴性，保持正式统计 |
| `report.py` | 从原始记录重建表格和错误脚本附录 |
| `create_tasks.py`、`prepare_prompts.py` | 冻结前生成器；正式运行后不执行 |

每个运行一个 JSONL，记录类型为 `start`、`api_response`、`api_retry`、`act`、`budget_stop`、`summary`。`act.outcome` 保留解析/运行状态、原始及换算后的错误、返回值、接口调用事件和本次草稿改动。`act.check` 是该次调用后的判定，`summary` 包含最终判定、最终回复、变更文件内容、删除列表和全量文件哈希。API 错误单列并从通过率分母排除。判定结果不发送给被测模型。

`act-sim` 接受 `--fixture DIR --script FILE --mode A|B --canned FILE`，成功结束才把临时副本提交到 `--fixture`。`reset/status/diff` 以每次 act 开始的状态为基线。返回值 JSON 超过 65536 UTF-8 字节时整体丢弃。

宿主没有实现安全沙箱；只对合成夹具运行，不对真实检出运行被测脚本。
