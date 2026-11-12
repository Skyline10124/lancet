# Overlay 提交实验

ADR 0002 接受前实验 2 的 Python 3 原型、场景驱动和原始记录。实验数据见 [REPORT.md](REPORT.md)，一次运行一个 JSON，位于 [results/](results/)。不依赖根 Rust 工作区。

## 入口与边界

```sh
prototypes/overlay-commit/ovl-act run \
  --repo "$HOME/exp/overlay/repo" --script /absolute/script.sh \
  --git-policy P1 --check both --on-fail discard
```

| 参数 | 取值与行为 |
| --- | --- |
| `--git-policy` | `P1`：Git 元数据进入草稿；`P2`：真实 `.git` 只读绑定；`P3`：真实 `.git` 可写绑定 |
| `--check` | `C1`：开始时 lstat 快照；`C4`：当前 ctime > 文件系统 T0；`both`：都记录、按 C1 决定提交 |
| `--on-fail` | `discard`：失败丢弃；`keep-regenerable`：失败仅提交 `target/` |
| `--finish` | 默认 `commit`；`discard` 用于 S6 脚本成功后的丢弃对照 |
| `--run-id` / `--output` | 指定状态目录名和结果 JSON；默认随机 ID 和 `~/exp/overlay/runs/<id>.json` |

挂载目标就是仓库真实路径。P2/P3 在挂载 overlay 前先保留真实 `.git` 的 bind，再将其挂到合并视图中。子命名空间退出后由父进程分类、预检、留前态和提交。Git 状态观测使用 `--no-optional-locks`，噪声中的 `git status` 使用正常写索引行为。

| 路径 | 内容与生命周期 |
| --- | --- |
| `~/exp/overlay/repo` | 唯一实验克隆；驱动每次恢复基线，会丢弃其中实验改动 |
| `~/exp/overlay/baseline` | 预热后的 reflink 副本；驱动恢复来源，包含 `.git` 和 `target/` |
| `~/exp/overlay/state/<id>` | `upper/`、`work/`、reflink `preimage/`、子进程输出与同步点 |
| `~/exp/overlay/scripts` | 每次运行的脚本；全文也存在对应 JSON 中 |
| `results/calibration-*` | 校准时只读截取的完整 JSONL 行、SHA-256 和服务状态 |

驱动和噪声只对 `~/exp/overlay/repo` 写入；原型入口允许实验区内具有 `.git` 目录的仓库。原始 `~/Dev/lattice` 仅在准备时执行过一次 `git clone --no-hardlinks`。原型不是对不可信脚本的完整沙箱，不实现 WAL、崩溃回滚或提交互斥；整批“不提交”保证适用于预检阶段发现的冲突。

## 文件与验证

| 文件 | 职责 |
| --- | --- |
| `commit.py` | 上层分类、内容/权限比较、整批冲突检查、reflink 前态、剥离属性、rename 提交 |
| `ovl_act.py` / `ovl-act` | T0、快照、命名空间、P1/P2/P3 和每次运行 JSON |
| `test_commit.py` | 小临时 Git 仓库上的七项分类与提交测试 |
| `suite.py` | S1–S9、外部修改、并发同步、判定和重建计时 |
| `calibrate.py` | 冻结被动记录、两段覆盖统计、生成频率参数 |
| `noise.py` / `ide-noise.sh` | 在命名空间外作用于实验克隆的可配置噪声 |
| `report.py` | 从 JSON 生成数据表；不修改原始运行记录 |
| `verify_results.py` | 核对运行矩阵、判定复算、target 排除、属性扫描和校准摘要 |
| `cleanup.py` | 检查进程/挂载、恢复克隆、仅删除状态目录的 upper/work |

```sh
cd prototypes/overlay-commit
unshare --user --map-root-user --mount python3 -m unittest -v test_commit
python3 -m py_compile *.py
```

测试用真实字符设备 `0:0`，故在用户命名空间内执行。仅检查 `user.overlay.*`，保留 SELinux 标签。删除和不透明目录的前态包含原有后代。带 redirect 的目录及后代只记录。`opaque=y` 隐藏下层；`opaque=x` 仅为内核遍历提示，不按不透明目录处理。

## 场景与判定口径

以下命令从 LANCET 仓库根目录执行。

```sh
python3 prototypes/overlay-commit/suite.py --scenarios S1 S3 S4 S5 S7 S8 S9
```

首次准备时将实验克隆的 origin 配成 `https://github.com/Skyline10124/lattice.git`，供 `bot-run` 校验 bot 身份；实验不会向该远程推送。首批本地 origin 导致的失败保留在结果中，排除清单是 `results/excluded-runs.json`。

每个场景变体运行三次；S2/S6 包含三种 Git 策略，其他场景使用 P1。S5 两种失败策略各三次，S7 三种命令各三次，S9 每个文件关系有三组双运行。S9 两份草稿同时存在，通过同步点确定 A 先提交、B 随后检查。S3/S4 也在草稿完成后同步注入修改，避免靠延时猜测注入时机。

S5 使用预热后自有产物最小的 `net-guard`，每次恢复相同的 `target/` 基线，修改现有测试的一个断言，运行 `cargo build -p net-guard` 和 `cargo test -p net-guard`。失败处理后在真实路径运行下一次 `cargo build -p net-guard`；源码已恢复，只比较构建产物的保留策略。自有产物和共享依赖的大小分别记录在 `results/crate-selection.json`。

S6 脚本前在下层修改 README，保证 `git commit` 有内容可提交。命令为 `git add -A && bot-run git commit -m t`、`git checkout -b t2`、`git stash`；提交使用项目规定 bot 身份。`set -e` 使 P2 在第一条写 Git 命令失败后停止。提交对照预期 Git 内容与合并视图一致；丢弃对照预期回到脚本前 HEAD、分支、status。P3 的直接 Git 写入是否保留按原始数据判定。提交后工作区无新改动，`git stash` 实际返回 `No local changes to save`；未覆盖非空 stash。

误报/漏报单位是“运行 × 路径”，C1/C4 分别计算。S3 的 README 和 S9 同路径组 B 的 README 是真冲突；S4 的外部修改未进入提交集合，所以不是真冲突。其余变动是背景噪声或策略自身写入，不标为外部真实冲突。失败/主动丢弃也记录检查结果，但不据此执行提交。误报路径和漏报路径保留在每次 JSON 的 `confusion` 中。

基础设施异常连续两次则保存 `blocked-*` 记录并跳过该变体余下重复；S5 预置测试失败、P2 写入拒绝和 S7 的 EXDEV 是实验结局。每个运行前检查 15 GiB 阈值。可用空间低于阈值时停止。

## 校准与重跑 S2、S6

先读取当前记录生成新配置，不停止、不重启、不修改 `lattice-write-recorder`：

```sh
python3 prototypes/overlay-commit/calibrate.py \
  --output prototypes/overlay-commit/noise-config.json --duration 30
python3 prototypes/overlay-commit/suite.py --scenarios S2 S6 \
  --noise-config prototypes/overlay-commit/noise-config.json --repetitions 3
python3 prototypes/overlay-commit/verify_results.py --allow-reruns
python3 prototypes/overlay-commit/report.py
```

重跑以新 ID 追加 JSON；报告列出全部运行，并分配置来源保留校准信息。`verify_results.py` 默认核对一次完整实验的 84 次运行；追加重跑用 `--allow-reruns` 核对每个变体至少三次且为三的倍数；配置来源在报告噪声表中逐次列出，追加记录不计作原始 84 次。`noise-config.json` 中 `intervals_s` 是秒/次，`null` 禁用该项；`duration_s` 是每次噪声窗口。起始相位在一个周期内均匀取样，种子由 `seed` 与运行 ID 的 SHA-256 派生，保存在运行记录中。30 秒窗口可能没有低频事件，实际产生的事件逐条记录，不能把配置频率当成每次实际次数。

| 类型 | 频率依据与实现 |
| --- | --- |
| Git status | 只读轮询本身不可被 inotify 推出；保留方案每秒一次 |
| cargo check | 空 target 记录不能测出检查启动频率；保留方案启动时一次 |
| source touch | 非点目录路径的 attrib 次数 / 覆盖秒数；本次为 0，禁用 |
| index refresh | 主索引锁的 create 次数；`git update-index --refresh` |
| FETCH_HEAD | close_write 次数；原内容写回，复现元数据变化 |
| 其他 Agent 沙箱 | `.codex/` create 次数；同时创建 `.codex/`、`.aws/`，0.1 秒后删除 |
| checkpoint | 独立检查点索引 create 次数；独立 index 下 add/write-tree，再更新 `refs/t3/experiment` |
| config / packed refs / remote HEAD 锁 | 对应锁 create 次数；独占创建、关闭、删除 |
| worktree index | 工作树索引锁 create 次数；映射到独立 index 的 read-tree，避免伪造工作树注册信息 |

未从记录复制源码、对象内容或远程引用的值。Git 活动按类型合成，不进行网络 fetch。校准第一段以最后一条事件作为已观测覆盖下界；第二段以冻结记录时间结束。原始两段时间与全部路径频数在报告和配置里。

## 收尾

确认没有实验命名空间或进程后，先恢复当前用户所属的 mode=000 工作目录的遍历权限，再仅删除 `~/exp/overlay/state/*/upper` 和 `work`；保留 `repo`、JSON、脚本、前态。中断恢复先检查 JSON 完整性和挂载，再恢复 `baseline`；不复用中断时未完成的提交。最终清理清单保存在 `results/cleanup.json`。

```sh
python3 prototypes/overlay-commit/cleanup.py
```

原始结果总量超过 20 MiB 时，每次运行 JSON 使用 gzip；`report.py`、`verify_results.py` 同时读取 `.json` 和 `.json.gz`。可用 `gzip -dc results/<运行>.json.gz` 读取原始 JSON。
