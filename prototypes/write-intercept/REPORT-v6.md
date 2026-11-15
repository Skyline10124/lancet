---
status: draft
---

# 写拦截第六轮：分离工作区与批次合并（S3）

## 结论

本轮区分三种性质：沙箱与宿主写入分离、失败时批次回滚、多个文件对宿主同时可见。S3 原型实现前两者的常见路径；逐文件交换不能提供第三者。对于任意不合作的宿主写者和多链接 inode 的原地写回，本轮没有证明无条件的批次原子性。实测的多链接部分写崩溃会留下 `Indeterminate`，不能将“普通文件的崩溃样本通过”扩大为“恢复从不部分合并”。

新鲜度按本轮决定单独评估：宿主变化漏掉 inotify 时，影子可以暂时过期；合并仍逐项比较该条目同步时的内容基准，不能将同步时刻的延迟直接列为隔离失效。

本报告与原始结果来自 `/home/gcp-vegas/.t3/worktrees/lancet/t3-ed9fe41d`。仅修改研究原型，未修改 ADR，未提交、推送或执行 GitHub 写操作。前五轮 2,456 个来源文件逐个与 `origin/exp/write-intercept` 的 blob 核对，无差异，见 [来源完整性](results/v6/baseline-integrity-v6.json)。工作负载、Cargo 缓存和旧二进制均从指定第五轮副本用 `cp -a --reflink=always` 复制；第一次 Cargo 命令起使用本轮 `.scratch/cargo-home`，所有构建均加 `--offline`。

## 实现与边界

| 组件 | 实现 |
| --- | --- |
| 普通路径 | [Rust 节点](shadow-v6/src/main-v6.rs) 复用第五轮的对象模型、监听与物化逻辑，持久影子位于宿主工作目录之外的节点状态目录 |
| 挂载 | [namespace-v6.c](namespace-v6.c) 将影子挂到真实绝对路径；`target/` 只在影子内；`.git/` 私有；仅宿主对象目录只读绑定到影子 `.git/host-objects-v6` |
| 扫描 | [scan-v6.rs](shadow-v6/src/scan-v6.rs) 并行分配一级子树，扁平根目录分配文件；比较 dev/ino、size、mode、nlink、ctime、mtime。10 ms 保守时间窗口内的“近似干净”条目从绑定的宿主 fd 向匿名 O_TMPFILE 执行 FICLONE，然后读稳定快照摘要；克隆重新武装后续 mmap 首次脏页时间戳更新 |
| 事件 | 沿用双侧目录 inotify，只提供候选加速；宿主精确扫描在 act 间运行，周期 100 ms；到期时即便请求连续到达也执行，因此请求可能等待扫描。影子终止后扫描求改动集合。独立 Python 全内容 oracle 不参与性能计时 |
| 合并 | [transaction-v6.rs](shadow-v6/src/transaction-v6.rs) 持久 intent；已有普通对象交换、新建 `RENAME_NOREPLACE`、删除移入前态区；同类别目录移动一条 rename。多链接内容在原 inode 写回并保留 reflink 前态 |
| 冲突 | 应用前内容核对；交换后再核对换出的前态，能发现部分检查窗口内的宿主写入。后项冲突时逆序核对并恢复已应用项；回滚中又变化则 `Indeterminate` |
| Git | [git-v6.py](git-v6.py) 准备私有 HEAD/index/refs/config；alternates 指向只读宿主对象；gix 0.85 校验新 loose object，Git plumbing 准备路径级 index 与引用更新，统一交普通事务应用 |
| 持久化 | 源数据与事务阶段使用批量 `syncfs`；阶段日志统一保存，不按每文件 `fsync`。`syncfs` 覆盖同一 XFS 的其他写者，不等同于一次物理日志提交 |

原型沿用固定顶层分类、UTF-8 路径、单个本地 XFS 根、当前用户拥有文件的研究范围。没有接入正式 Starlark `commit()`、协议回执、WASI、生产级 Landlock、任意分类规则、多工作目录协调或所有恶意路径解析防护。原型的 `commit`/`discard` RPC 对应本轮的提交/丢弃决定；`Conflict` 映射到正式接口的 `Failed(Conflict)`，路径在 JSON `conflicts` 中，丢弃路径在 `uncommitted_reverted` 中。正式 `content` 撤销提示与回执接线尚未实现；不应将启动器当作完整不可信代码沙箱。执行环境退出依赖 PID 命名空间 init 退出清理全部后代。

[工作区模型冒烟检查](results/v6/workspace-model-v6.json) 确认普通文件与 config 的宿主/影子 inode 不同，向只读宿主对象挂载写入返回 `EROFS`，host target 不变，影子 target 缓存在丢弃与节点重启后保留。[宿主 Git checkout 同步](results/v6/host-git-sync-v6.json) 两个方向各三次，普通树的独立全内容 oracle 与 HEAD 全部一致。

## 正确性与完整性

<!-- CORRECTNESS -->
| 验证 | 结果 | 证据 |
| --- | --- | --- |
| 继承的 63 类 × commit/discard | 126/126 | [coverage](results/v6/coverage-v6.json) |
| 两侧随机，5 seeds ×20 act | 200/200 | [randomized](results/v6/randomized-v6.json) |
| 宿主同步 vs 全内容 | 45/45 | [synchronization](results/v6/synchronization-v6.json) |
| act 内宿主同文件写/编辑器改名 | 40/40 Conflict | [concurrency](results/v6/concurrency-v6.json) |
| 宿主改异文件 | 20/20 Committed | 同上 |
| 后项冲突与回滚再冲突 | 3 Conflict、3 Indeterminate | 同上 |
| 普通事务 SIGKILL | 24/27 前态或后态 | [crashes](results/v6/crashes-v6.json) |
| Git SIGKILL | 18/18 HEAD/工作树一致且无锁残留 | [git-crashes](results/v6/git-crashes-v6.json) |

同步补齐延迟（ms，写者停止后轮询独立 oracle）：

| 用例 | 样本数 | 中位数 | 最大 |
| --- | --- | --- | --- |
| editor-rename | 3 | 111.298 | 111.927 |
| fake-mtime | 3 | 112.623 | 118.340 |
| long-lived-mmap | 30 | 101.129 | 106.972 |
| outside-hardlink | 3 | 111.428 | 114.060 |
| same-tick | 3 | 108.510 | 111.523 |
| tmpfile-linkat | 3 | 110.004 | 112.374 |

多链接部分写的 3 个 SIGKILL 样本恢复为 `Indeterminate`，不能算成功恢复。Git 崩溃的“前态/后态”比较工作树、HEAD 与可见 status，不要求新写入且无引用的 objects 被删除。
<!-- /CORRECTNESS -->

`IN_CLOSE_WRITE` 不能作为终止后扫描的完备替代。实测普通可写打开后进程退出、先关闭 fd 后 mmap 写入都有通知；经未监听目录的硬链接别名写入、`O_RDONLY|O_TRUNC` 没有通知。监听的目录不是 inode 的所有名字。见 [close-write 探针](results/v6/close-write-v6.json)。

“同一时钟刻度”探针记录了实际时间戳；普通连续等长 `write` 的三组样本没有产生完全相同的 ctime，不能宣称复现了该具体内核时序。长寿命 mmap 的已脏页面反例覆盖了内容改变但 stat 不再变化的情形。原型使用 10 ms 保守窗口，而不是声称测出了该文件系统在所有情况下的时间分辨率。

## Git 语义

每次 act 的 `.git/HEAD`、refs 与 config 从宿主同步到私有目录，objects 独立保留。私有 index 用 `read-tree HEAD` 初始化：宿主既有暂存不会自动进入 agent 的 commit，宿主 index 单独保存为冲突基准。agent 只 stage 未 commit 的 index 内容不回宿主，列入 `staged_not_merged`；普通工作树改动仍按 `commit()` 决定是否合并。config、hooks 及其余未列入允许集合的 Git 文件列入 `blocked`，不回宿主。

HEAD 移动时，仅将旧 HEAD 到新 HEAD 的实际改动路径从新提交树写入宿主索引，其余宿主暂存与索引标记继续保留。若同一路径的宿主 index 项不同于 act 基准，整批报冲突。引用以持有对应 lockfile 后比较基准的方式更新；这是遵守 Git lockfile 的写者之间的比较后交换，不是对绕过锁文件的任意写者提供内核 CAS。

离线 `gix 0.85.0` 构建成功，见 [离线探针](results/v6/gix-offline-v6.log)。本轮仅使用 gix 解析和校验新对象；引用、树差异和索引生成仍使用 Git plumbing。这是实现分工相对“全部采用 gix”的偏离。新 loose object 在引用前校验后复制；新 pack、reftable、SHA-256 仓库、linked worktree `.git` 文件没有实现，不得外推本轮结果。

<!-- GIT -->
| 操作 | 提交/丢弃样本 | status 一致 | 宿主暂存保留 |
| --- | --- | --- | --- |
| checkout | 24 | 24 | 24 |
| commit | 24 | 24 | 24 |
| stash | 24 | 24 | 24 |
| packed-refs | 24 | 24 | 24 |

每项覆盖 none、after-end-index、during-act-refresh、during-act-stage 四种时序，提交与丢弃分别重复 3 次。“暂存保留”列包括原本无暂存的样本；实际注入暂存的样本单独可从 JSON 筛出。

| 新增用例 | 样本 | status 一致 | 暂存保留 | config/hooks 保留 |
| --- | --- | --- | --- | --- |
| hooks-config | 3 | 3 | 3 | 3 |
| host-commit | 3 | 3 | 3 | 3 |
| host-stage-before | 3 | 3 | 3 | 3 |
| host-stage-other | 3 | 3 | 3 | 3 |
| host-stage-same | 3 | 3 | 3 | 3 |
| stage-only | 3 | 3 | 3 | 3 |

1 万/10 万跟踪文件，单路径提交时的中位数（ms）：

| 跟踪文件 | Git 同步 helper | 完整 begin | Git 合并准备 helper | 完整提交含重建 | index 字节 |
| --- | --- | --- | --- | --- | --- |
| 10000 | 76.331 | 110.942 | 131.646 | 183.656 | 830029 |
| 100000 | 212.375 | 251.058 | 468.438 | 576.517 | 8031028 |

helper 时间不含 Python 进程启动；完整 begin/提交列含启动成本及普通文件工作。原始结果：[矩阵](results/v6/git-matrix-v6.json)、[新增](results/v6/git-extra-v6.json)、[规模](results/v6/git-scale-v6.json)。
<!-- /GIT -->

## 性能与口径

[benchmarks-v6.py](benchmarks-v6.py) 复用第三轮 none/ns/B3 驱动与第五轮 S2 二进制，顺序运行。LATTICE debug 全量构建每次清空对应配置的 target；Cargo 4 jobs、debug=0、incremental=0、offline；页缓存保留，未请求 root 清缓存。S3 的 target 独立，不把宿主 target 绑定回沙箱。每项至少三次，报告中位数，不宣称三样本足以证明统计等效。

情形甲：每次由宿主恢复到 a，再由 agent checkout b。情形乙：宿主只在预热前初始化，同一常驻节点连续 a/b 交替，两个方向分列。S2/S3 总时间包括同步、命令、结束扫描、提交及影子重建；初始化单列。最终 S3 的周期扫描在主循环调度，内部用并行线程扫描，但到期时会阻塞下一个 RPC，因此这部分等待计入 act 关键路径。分项表将 RPC 与到期扫描等待的差额单列，不能只相加 begin/finish 内部计时来声称固定开销达标。none/ns/B3 沿用驱动内部 wall 口径，外层进程计时也保存。无改动 S2/S3 的 RPC 成本与执行 `true` 的其他配置不完全同口径，表中注明。

<!-- PERFORMANCE -->
五配置中位数（秒）。[比较图](results/v6/performance-v6.html) 另存为独立 HTML。

| 工作负载 | none | ns | B3 | S2 | S3 |
| --- | --- | --- | --- | --- | --- |
| LATTICE 全量构建 | 53.743435 | 53.838741 | 54.897473 | 54.288105 | 54.127087 |
| checkout 甲 a→b | 0.249446 | 0.274362 | 13.801717 | 5.601941 | 10.213363 |
| checkout 乙 b→a | 0.247768 | 0.243855 | 12.560764 | 3.206912 | 5.228439 |
| checkout 乙 a→b | 0.246222 | 0.245992 | 12.671173 | 3.248424 | 5.180129 |

S3 情形乙分项（秒，各列独立取中位数）：

| 方向 | begin | 命令 | 结束扫描 | 提交准备 | 应用/落盘 | 影子重建 | RPC/到期扫描等待 | 总计 |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| b→a | 0.152823 | 0.372791 | 0.068517 | 0.626284 | 0.202238 | 1.961658 | 1.820899 | 5.228439 |
| a→b | 0.152773 | 0.369205 | 0.068489 | 0.635284 | 0.189499 | 1.976552 | 1.814135 | 5.180129 |

20,000 次 open+close，探针内部单次中位数（µs）：

| 路径 | none | ns | B3 | S2 | S3 |
| --- | --- | --- | --- | --- | --- |
| /dev/null | 3.398 | 3.394 | 28.927 | 3.402 | 3.394 |
| a | 3.207 | 3.178 | 172.173 | 4.683 | 4.613 |

无改动 act（ms；S3 为 begin+finish+commit RPC，其他配置含 true）：

| 文件数（另加 1,000 目录） | none | ns | B3 | S2 | S3 | S3 8线程后台扫描 |
| --- | --- | --- | --- | --- | --- | --- |
| 3000 | 0.771 | 1.794 | 2.399 | 2.598 | 9.952 | 6.081 |
| 10000 | 0.771 | 1.782 | 2.371 | 2.700 | 25.832 | 14.731 |
| 100000 | 0.754 | 1.749 | 2.374 | 3.255 | 653.784 | 145.637 |

宿主并行精确扫描（ms，不含子进程）：

| 文件数 | 1线程 | 4线程 | 8线程 |
| --- | --- | --- | --- |
| 3000 | 27.159 | 8.154 | 6.081 |
| 10000 | 61.879 | 19.657 | 14.731 |
| 100000 | 533.919 | 185.526 | 145.637 |

S3 初次建立影子（秒，含批量写回及初始化）：

| 文件数 | 中位数 |
| --- | --- |
| 3000 | 0.766382 |
| 10000 | 4.761833 |
| 100000 | 73.861152 |

求改动集合及合并（ms）：

| 配置 | 文件数 | 结束扫描 | 合并含重建 | S3 影子重建 |
| --- | --- | --- | --- | --- |
| S2 | 1 | 0.061 | 8.897 | 0.000 |
| S2 | 100 | 1.909 | 73.251 | 0.000 |
| S2 | 4000 | 79.150 | 2641.593 | 0.000 |
| S3 | 1 | 8.132 | 6.061 | 0.700 |
| S3 | 100 | 8.355 | 71.339 | 53.300 |
| S3 | 4000 | 18.158 | 2352.359 | 1930.490 |

目录 rename 完整 act（秒；宿主复位及其同步在计时前完成，S3 合并后重建计时在内）：

| 子文件数 | none | ns | B3 | S2 | S3 |
| --- | --- | --- | --- | --- | --- |
| 1000 | 0.001339 | 0.002382 | 0.005025 | 0.101655 | 0.846511 |
| 10000 | 0.001433 | 0.002456 | 0.005452 | 0.920158 | 8.411384 |

普通合并的核对开始至替换完成窗口：中位数 28.580 µs，范围 25.980–88.540 µs。含 lstat/读取摘要与 syscall，是本机样本包络，不是最坏上界。

合并应用期间可见中间态的计时包络（ms；从首项核对开始到末项应用后）：

| 文件数 | 中位数 |
| --- | --- |
| 1 | 0.048 |
| 100 | 3.925 |
| 4000 | 171.306 |

一个文件的该计时不代表跨文件部分可见；多链接原地写自身另有部分字节可见。三样本中位数没有覆盖共享主机的最坏调度延迟。
<!-- /PERFORMANCE -->

Git 锁文件带节点所有者标记，准备阶段另留清单；重启只删除内容仍匹配的锁。初版在 `before-log` 与 `after-complete` 各 3 次留下锁，最终修复后 18/18 通过。新 objects 在引用前入宿主库，失败或回滚后可能留无引用对象，不提供整个 `.git` 字节级回到前态的承诺。

## 目标逐项判定

<!-- TARGETS -->
| 目标 | 判定 | 证据/范围 |
| --- | --- | --- |
| 10 万条目无改动固定开销 ≤50 ms | 未达标 | 101,000 条目，653.784 ms；含可能等待到期后台扫描 |
| 乙 4,000 文件 checkout ≤2 s | 未达标 | 较慢方向 5.228439 s |
| 构建与 namespace 相当 | 幅度相当，未证明统计等效 | 相对 ns +0.54% |
| 同步与改动集合完全一致 | 未完全验证（已跑样本通过） | 同步 45/45；随机 200/200；继承覆盖 126/126；同刻度连续 write 未精确复现 |
| 崩溃恢复从不部分合并 | 未达标 | 多链接部分写 3 次均 Indeterminate |
| act 期间同文件宿主写 100% 冲突 | 已跑样本达标 | 40/40；不包括合并后旧 fd 写入的一般证明 |
| Git 暂存全部保留、status 一致、config/hooks 不合并 | 已跑用例达标 | 矩阵 96、新增 18；只涵盖本地 SHA-1 loose objects/普通 refs 仓库 |
<!-- /TARGETS -->

## 未解决与 ADR 建议

1. 建议第 6 节明确区分“失败时全部撤销”“成功后的持久结局”“跨文件对外同时可见”。逐文件 `RENAME_EXCHANGE` 加日志不能让宿主看不到中间状态，本轮测其可见窗口，不将它写成目录树级原子操作。
2. 多链接原地写保 inode 与任意时刻崩溃、任意不合作宿主写之间存在冲突：只根据前态、期望后态与当前部分字节，无法判断部分写来自节点还是宿主。需要维护者裁决是否要求宿主合作排他、是否允许 `Indeterminate`，或是否改变多链接处理边界。当前实现选择保留无法证明归属的内容。
3. 外部别名以 inode 的 `nlink` 大于扫描到的名字数检测。仅扫描工作目录不能确定别名在哪个缓存目录。只读 bind 挂载也挡不住经可写别名对同一 inode 写入。建议第一版对含无法归属外部别名的可写与只读工作目录均拒绝启动执行环境；只有在可完整核对全部可写挂载的 inode 集且持续维持该条件时，才考虑“只拒绝沙箱可写别名”。本轮报告检测与绕过，不把该建议当作已经落地的生产准入策略。
4. 宿主在 act 运行期间同文件写与合并系统调用窗口的竞态分别陈述：前者在本轮用例中全部冲突；后者仍没有内核条件交换，交换后的旧 fd/mmap 与原地写窗口需要额外约束。
5. 扫描时宿主持续变化没有单一全树快照时刻；有界延迟应定义为写者停止后到下一轮成功扫描完成，并注明 act 期间暂停同步、共享机器调度与 I/O 影响，不能承诺实时系统最坏界限。

连续快速 act 的额外验证在 88 个 act、102.797 ms 内补齐一处漏通知 mmap 写入。真正并发的长寿命 mmap 写者另做 20 轮，全部在写者暂停后达到全内容一致，最长 107.261 ms；无 begin/finish 错误。见 [公平调度](results/v6/scan-fairness-v6.json) 与 [连续 mmap](results/v6/continuous-mmap-v6.json)。

## 复现与未覆盖

```sh
export CARGO_HOME="$PWD/prototypes/write-intercept/.scratch/cargo-home"
export CARGO_TARGET_DIR="$PWD/prototypes/write-intercept/.scratch/rust-target-v6"
cargo build --offline --release --manifest-path prototypes/write-intercept/shadow-v6/Cargo.toml
cc -O2 -Wall -Wextra prototypes/write-intercept/namespace-v6.c -o prototypes/write-intercept/.scratch/namespace-v6
python3 prototypes/write-intercept/verify-v6.py coverage
python3 prototypes/write-intercept/research-v6.py
python3 prototypes/write-intercept/verify-git-v6.py matrix extra scale
python3 prototypes/write-intercept/supplement-v6.py
python3 prototypes/write-intercept/benchmarks-v6.py full checkout micro scales merges renames
```

初版及中间失败分别保存在 `coverage-initial-v6.json`、`coverage-second-v6.json`、`git-crashes-initial-v6.json`、`rename-links-initial-v6.json`、`scan-fairness-initial-v6.json`；性能的早期 S3 数字保存在 `*-before-fix-v6.json`，主表以最终 S3 重测值为准。

独立“合并”阶段只比较 S2/S3：none、ns、B3 原地执行，没有该阶段，不能捏造同名计时。初建影子本轮只测 S3，S2 的历史初始化数据保留在第五轮报告。无改动 RPC 的原型调度没有达到真正脱离请求关键路径的后台发布；这是性能失败的实现原因，不是对所有分离工作区设计的性能下界证明。

执行中发生一次用户中断：100,000 文件的前两轮已完整保存，恢复后只重跑尚未完成的第三轮；三个规模最终各有三次初始化、每线程九次扫描和九次空 act 样本。目录改名的第一组计入宿主复位后同步，另存 [原始口径](results/v6/renames-with-host-sync-v6.json)，最终表重测并按第五轮将复位同步放在计时前。

本轮未测掉电、磁盘损坏、写回错误、内核崩溃或恢复过程中再次被杀；`kill -9` 是节点进程故障，不等于机器失去持久化状态。没有在真实用户仓库运行实验，没有读取 API key。未运行整个业务仓库测试；研究原型没有接入业务 crate。


## 交付检查

[交付文件完整列表（绝对路径）](results/v6/files-v6.txt)。

- `cargo build --offline --release --manifest-path prototypes/write-intercept/shadow-v6/Cargo.toml`：通过；使用本 worktree 的 Cargo 缓存与 target。
- `cargo --offline fmt --manifest-path prototypes/write-intercept/shadow-v6/Cargo.toml -- --check`、第六轮 Python 的 `py_compile`、C 启动器语法检查：通过。
- `cargo --offline xtask docs-check`：通过，25 个已跟踪 Markdown。新报告未暂存，另以路径解析检查其相对链接。
- 原始数据保存在 [results/v6](results/v6/)，[汇总](results/v6/summary-v6.json)、[最终计时来源](results/v6/final-benchmark-provenance-v6.json)、[交付源摘要](results/v6/source-hashes-v6.json)、[交付核验](results/v6/validation-v6.json) 可用于审查。
- 未提交或推送；全部本轮沙箱、节点、监听与后台测试进程已退出，进程核对保存在 [清理结果](results/v6/cleanup-v6.json)。
