---
status: draft
---

# 写拦截第五轮：S2 的增量索引、提交与 Git 撤销

## 结论与适用范围

本轮在 `/home/gcp-vegas/.t3/worktrees/lancet/t3-08714b01` 实现并验证 Rust S2。**不建议据此把 S2 定为 B3 的替代机制。** 编译型实现与候选集合能消除第四轮每 act 全量 JSON 重写，但目录 inotify 不能覆盖所有外部写入；逐文件核对仍不是原子条件替换；共享 Git 的结束摘要不能区分 act 内的宿主写者。下面分别报告实测性能与语义边界，不把常见用例通过当作完整性证明。

本轮仓库改动仅为研究原型与本报告，没有修改 ADR、提交、推送或 GitHub 写操作。首次 Cargo 构建/格式化未显式指定 `CARGO_HOME`，可能更新了 `~/.cargo/.global-cache` 的缓存使用元数据，这是“只写当前 worktree”的执行偏差；发现后将所有后续 Cargo 缓存指定到自己的 `.scratch/cargo-home`。未修改用户项目源码或前轮 worktree。按任务要求取回 `origin/exp/write-intercept` 的原型目录，旧报告、旧实现与旧结果保留原字节；[来源完整性](results/v5/baseline-integrity-v5.json)核对所有 2,183 个来源文件。LATTICE 与 Cargo 缓存用 `cp -a --reflink=always` 从指定第四轮副本复制到自己的 `.scratch/`。原 worktree 与 `~/Dev/` 目录只读。

<!-- TARGETS -->
| 判断目标 | 结论 | 实测/理由 |
| --- | --- | --- |
| 100,000 条目无改动固定开销 ≤50 ms | 达标 | 普通树 0.341 ms（100,100 条目）；10 万跟踪文件含 Git 快照 16.400 ms |
| 情形乙 4,000 文件 checkout ≤2 s | 未达标 | b→a 3.301 s；a→b 3.277 s |
| 构建开销与 ns 相当 | 幅度相当，未做等效性统计 | 相对 ns +0.06% |
| ε 与全量扫描完全一致 | 未达标 | 常规与随机用例另表；跨 act mmap 与未监听别名有反例 |
<!-- /TARGETS -->

## 原型实现

[shadow-v5/src/main-v5.rs](shadow-v5/src/main-v5.rs) 是节点主体；[shadow-v5.py](shadow-v5.py) 是 JSON 行协议测试客户端，不承担同步或事务算法。复用 [namespace-v4.c](namespace-v4.c) 把影子绑定到真实绝对路径，直通 `.git/`、`target/`，遮蔽 `hidden/` 与状态目录。沿用第四轮固定分类、UTF-8 路径与本地 XFS 假设，样本中的普通文件均由当前用户拥有；没有实现正式 `fs.*`、回执协议、任意分类规则或生产级 Landlock 限制，不能把该启动器视为完整恶意代码沙箱。

### 状态与增量路径

| 状态 | 归属、更新与恢复 |
| --- | --- |
| 真实与影子条目 | Rust `Node` 各持有一棵路径索引；条目含类型、mode、stat、SHA-256、符号链接文字和常规硬链接组 |
| inode 到路径集合 | `Tree` 内维护，候选改动扩展到该 inode 的全部已知别名，避免漏掉其他常规硬链接名 |
| 目录监听 | 真实、影子各一个常驻线程、一个 inotify fd；eventfd 唤醒屏障，在 begin/finish 取走已收集候选 |
| 持久索引 | `index-v5.jsonl` 只追加变化条目与删除项，并 `fdatasync`；没有每 act 全表序列化。节点重启按要求全扫重建，并压缩日志；不把未经验证的残缺日志尾当成权威状态 |
| 普通文件事务 | `transactions/<id>/transaction.json` 与 `pre/`，阶段为 preparing → ready → complete；前态、结构化目录改名与多链接 inode 锚点一起归事务所有 |
| Git 撤销 | act 前保存 HEAD、index、packed-refs、loose refs；额外保存 reflog，使 stash 的可见历史一起恢复。Git 对象仍留存，不删除新生成但不再被引用的对象 |

监听覆盖题目列出的事件，并增加 `IN_MODIFY`。只用 `IN_CLOSE_WRITE` 等原清单时，`truncate()` 与 `O_RDONLY|O_TRUNC` 的 4 个 commit/discard 用例失败；[首次结果](results/v5/coverage-initial-v5.json)保留该证据，修正后复跑。新目录立即递归加 watch，同时标记子树待扫描，补上建立监听前已出现的文件；目录移动按 cookie 更新 watch 的路径映射。`IN_Q_OVERFLOW` 退回全量内容扫描。随机 seed=3 的真实侧曾暴露“临时硬链接写后即删”的漏项（[首次失败](results/v5/randomized-initial-v5.json)）；新增目录的监听空窗、创建后又删除的条目和未配对的新条目移出也保守回退，结果用 `uncertain_history` 与内核 overflow 区分。该额外 O(N) 回退是本轮相对最初计划的必要补充。候选文件重新算摘要，不依赖 stat 相同来证明内容未改。

正常 `begin` 只核对真实侧候选、同步对应影子条目，并消费同步自身产生的影子事件；正常 `finish` 只核对影子候选。ε 比较内容、类型、mode、符号链接文字和硬链接拓扑，忽略单纯时间戳变化。空 act 不建普通事务，不写条目索引。测试用 `begin(full=True)` 与 `finish(overflow=True)` 强制两侧全内容扫描，量化完整核对的成本。

### 合并、目录改名与恢复

普通提交先核对涉及真实路径是否仍为 act 基线，写 preparing。源数据在 FICLONE 之前通过一次 `syncfs` 批量落盘，再克隆前态、保留多链接锚点，整批刷盘后持久写 ready。`syncfs` 范围是整个 XFS，可能受同文件系统其他写者影响；它不是单事务专用刷盘。

单文件先从已刷盘影子 reflink 到同目录临时文件，**在 rename 替换之前再次核对 act 基线**。准备阶段重新读到的外部内容不升格为新基线。多链接写保留原 inode，删除或覆盖别名按最终拓扑处理。正常冲突只撤销此前已经应用的项，冲突路径保留；批次不是跨文件原子可见。

同类别目录改名通过影子 inode 识别，进行源子树与目的路径三方核对后，在真实目录执行一次 `rename`。改名后编辑只对真正变化子项另建前态和应用。额外组合测试覆盖目录内硬链接、目录外常规别名和改名后编辑（[rename-links](results/v5/rename-links-v5.json)）；首次实现把自己的持久锚点变动误判为外部冲突，修正了该核对及改名后的原 inode 写入判断，[首次失败](results/v5/rename-links-initial-v5.json)保留。纯目录移动不逐文件克隆，但候选展开、子树冲突核对、ε 与后态记录仍为 O(N)，不能宣称整个事务常数时间。

重启时 preparing 不应用；ready 回滚前态；complete 保留后态；随后全量核对真实与影子。`recover <transaction> --revert` 先核对已提交后态再求逆。恢复器为本轮独立格式，没有把它宣称为 B3 账本 ABI 的直接兼容实现。

## 完整性、提交窗口与崩溃

<!-- CORRECTNESS -->
| 验证 | 结果 | 口径 |
| --- | --- | --- |
| 第四轮 63 类 × commit/discard | 126/126 | 独立 oracle、namespace 对照、ε、应用、丢弃、新进程 revert |
| 随机 5 seed × 两侧 ×20 act | 200/200 | 每 act 20 次选择，共 4,000 次（缺失目标可跳过）；92 个 act 因历史不确定回退全扫 |
| 运行中外部并发写 | 12/12 | 同文件 commit 报冲突；其他文件不混入 ε |
| 真实 IN_Q_OVERFLOW | True | 暂停节点后产生 20,000 文件事件；回退结果与独立全扫一致 |
| SIGKILL 后重启及下一 act | 60/60 | 12 阶段 ×5，含目录 rename 后 |
| 提交准备后外部写 | 见 commit-window 原始结果 | before-expected 与 after-ready 两点；真实最后核对保护 |
| 最后核对后外部写 | 仍可覆盖 | after-check 注入；不是原子 CAS |

无注入时核对开始到 rename 完成（含 stat 的窗口包络）：中位数 **28.580 µs**，范围 28.560–29.500 µs（3 次）。[窗口原始结果](results/v5/commit-window-v5.json)、[候选完整性反例](results/v5/completeness-v5.json)、[随机负载](results/v5/randomized-v5.json)、[崩溃与重启](results/v5/crashes-v5.json)、[补充验证](results/v5/supplement-v5.json)、[停机外部写](results/v5/downtime-v5.json)均保留。
<!-- /CORRECTNESS -->

[research-v5.py](research-v5.py) 使用第四轮的独立全树 oracle，随机负载既作用于影子，又作用于真实目录；包含内容写、编辑器式临时文件加 rename、chmod、删除、硬链接、mmap 与目录移动。第四轮短生命周期 mmap 用例通过，在映射关闭/进程退出时有写 fd 事件供核对。影子侧的跨 act 反例由测试进程直接持有映射，用于检验事件跟踪器本身，不声称该进程能越过启动器的 PID 生命周期清理；真实侧的长寿命映射则是普通宿主行为，不依赖任何沙箱逃逸。两者都说明不能把目录事件机制本身视为无条件完备。[Linux inotify 手册](https://man7.org/linux/man-pages/man7/inotify.7.html)明确列出 mmap/msync 不产生相应修改通知、目录监听不递归和通知不带写者身份；本轮另用实际反例验证，而不是只凭文档否决。

目录监听还看不到经未监听目录中的硬链接别名写同一 inode。对**已出现候选**扩展 inode 组，不能补出根本没有事件的写入。启动全扫与溢出回退也不会自动检测没有 overflow 标记的 mmap 改写。因此“ε 与全量扫描完全一致”不能对这个不限制宿主写法的方案成立。全量内容核对保留在原型中作为明确的替代成本，不悄悄计入或排除性能表。

提交窗口测试在节点内的测试钩子写真实路径，模拟不合作宿主写者，分别放在前态准备后、最后核对后；这是确定性时序注入，不是独立进程竞速成功率统计。前者应被逐文件核对挡住；后者仍能被 rename 覆盖。计时记录从最后一次核对开始到 rename 完成（包含 lstat 与比较本身），不含前态刷盘；它给出本机无竞争样本的暴露窗口的测量包络，不是调度延迟下的最坏上界。该风险与 B3 已记录的检查后替换风险有共同的外部不合作前提，但不能因为 B3 也有风险就把 S2 标为已消除。

SIGKILL 覆盖日志前后、前态落盘前后、ready 后、第一/第四项应用后、后态落盘前后、complete 后、多链接原 inode 部分写和目录 rename 后。没有测断电、损坏文件系统、恢复过程中再杀进程；停机期间外部写的保护另列反例。Git active 快照没有持久的结束归属证明，崩溃恢复明确报告 `Indeterminate`，不自动强制还原共享 Git；[9 个 Git 崩溃样本](results/v5/git-crashes-v5.json)保留这项未完成语义。正常提交/丢弃会落盘 settled 标记。

## Git 撤销与索引往返

[git-v5.py](git-v5.py) 对 `checkout`、`commit`、`stash`、`packed-refs` 各测试正常丢弃、结束后宿主改索引、act 内宿主刷新索引、act 内宿主暂存新文件，每项三次。正常丢弃将普通影子恢复到最新真实内容，将 HEAD/refs/index/reflog 恢复至 act 前；stash 的初态故意含未提交文件，判断的是 `git status` 回到同样的初态，而非一律要求干净。

恢复前比较 act 结束摘要，并获取相关 Git lockfile，再比较一次。任一 Git 项冲突时保留整个 Git 组并报告 Conflict，避免核对冲突造成 HEAD/index 部分恢复。这不是跨文件原子 Git 事务，恢复中 I/O 失败或崩溃仍可能留下部分替换。这是保守的组级策略；应由维护者裁决正式 API 是否采用它。它能保护**结束以后**的改动，却无法判断**act 内**最后写入者是沙箱还是 IDE/宿主 Git。测试单独记录被回滚的宿主 stage，不能以“IDE 只刷新 cache，最终 status 正常”掩盖归属缺失。

<!-- GIT -->
| 干扰时序 | 样本数 | status 回初态 | 报告 Conflict | 保留外部 index 字节 | 保留宿主 stage |
| --- | --- | --- | --- | --- | --- |
| none | 12 | 12 | 0 | 0 | 0 |
| after-end-index | 12 | 6 | 12 | 12 | 0 |
| during-act-refresh | 12 | 12 | 0 | 3 | 0 |
| during-act-stage | 12 | 12 | 0 | 0 | 0 |

索引往返中位数（ms）；`sum_scan` 是 Git 内容核对路径计数，不是 stat 次数。沙箱列包含 namespace 启动：

| 文件数 | 索引 | 宿主 status | 宿主 sum_scan | 再次沙箱 status | 沙箱 sum_scan | 索引字节 |
| --- | --- | --- | --- | --- | --- | --- |
| 10000 | shared | 94.711 | 10000 | 96.739 | 10000 | 803171 |
| 10000 | private | 9.285 | 0 | 11.516 | 0 | 803171 |
| 100000 | shared | 993.495 | 100000 | 1027.820 | 100000 | 8003272 |
| 100000 | private | 42.711 | 0 | 45.794 | 0 | 8003272 |

[Git 撤销样本](results/v5/git-discard-v5.json)、[索引往返](results/v5/git-index-v5.json)、[私有索引语义反例](results/v5/git-private-semantics-v5.json)。
<!-- /GIT -->

私有索引实验只使用 `GIT_INDEX_FILE` 指向共享 `.git/` 中的另一个测试文件，验证减少 stat 缓存往返的收益；它没有自动合并两套 staged 状态。[Git 官方文档](https://git-scm.com/docs/git/2.52.0#Documentation/git.txt-codeGITINDEXFILEcode)将该变量定义为替代索引文件。宿主 `git add` 后，私有索引的 `diff --cached` 看不到该 stage，说明索引不仅是可丢弃缓存。建议仅在明确授予独立暂存区语义时使用私有索引；不能把它默认为透明性能优化。当前私有索引只是性能探针，未纳入 act 撤销；采用它还需定义独立暂存区的生命周期与冲突规则。

本轮没有推荐 `core.checkStat=minimal`：它会削弱变更信号，第四轮已有等长保留 mtime 的反例，本轮也复跑了[单独 minimal、恢复原索引的独立对照](results/v5/git-minimal-only-risk-v5.json)。额外复跑第四轮 4 配置 × 4 Git 命令 × 3 的[回归](results/v5/git-regression-v5.json)，与私有索引实验分开统计。

## 性能与规模

[benchmarks-v5.py](benchmarks-v5.py) 顺序运行，无沙箱/ns/B3 直接调用第三轮驱动，S 直接调用第四轮 Python 实现。各配置每项至少三次，表格取中位数。工具链 1.96.0，Cargo 4 作业、debug=0、`CARGO_INCREMENTAL=0`、offline workspace；每次删 target，保留页缓存，不以 root 清缓存。完整[环境](results/v5/environment-v5.json)另存。共享机器上的三样本不足以声明统计等效，只报告幅度。

S/S2 的主表包括同步、子进程、结束求 ε、提交与持久化。初始化单列，正常 act 复用节点；S 的 JSON 对象加载与 S2 启动全扫均在持久会话初始化内。无沙箱/ns/B3 延续前轮 `stats.wall_s` 口径，同时保存进程外层计时。微基准取探针内部每次 open+close，不拿它代替完整 act 总时间。性能用例关闭独立 oracle，它的扫描时间另存于正确性与强制全扫结果。

情形甲每次从宿主将真实目录复位至 a；情形乙只在预热前由宿主初始化，之后在同一常驻节点内 a、b 交替 checkout 并提交，两个方向分别计时；检查状态使用 `--no-optional-locks`，避免测试自身在两 act 间刷新索引。试跑曾直接向 File 序列化事务 JSON，产生大量细粒度 write；最终实现先序列化到内存缓冲，再整批写日志。连同基线核对与预热条件修正前的试跑保存在 `*-before-fix-v5.json`，主表使用优化完成后的 S2 重测样本。随后补充逐文件 Conflict 分支的临时文件清理，复跑窗口及 126 个用例。复核还发现早期跨配置常驻的 S2 真实侧监听会增加 none/ns 的 open+close 成本；最终主表在非 S2 配置运行前关闭全部 S2 监听，并完整重跑五配置构建、两种 checkout 和微基准。`*-with-observer-v5.json` 只保存被排除的试跑。实际计时二进制/源码摘要见 [benchmark-provenance](results/v5/benchmark-provenance-v5.json)，交付版本摘要见 [source-hashes](results/v5/source-hashes-v5.json)。

<!-- PERFORMANCE -->
五配置总时间中位数，单位秒；[构建原始样本](results/v5/builds-v5.json)、[checkout 样本](results/v5/checkouts-v5.json)。

| 工作负载 | none | ns | B3 | S | S2 |
| --- | --- | --- | --- | --- | --- |
| LATTICE 全量构建 | 54.171668 | 53.638672 | 54.532975 | 53.774522 | 53.670997 |
| checkout 甲 a→b | 0.254262 | 0.275257 | 12.673445 | 8.738180 | 5.732193 |
| checkout 乙 b→a | 0.247665 | 0.249253 | 12.458948 | 4.470121 | 3.300720 |
| checkout 乙 a→b | 0.245706 | 0.247514 | 12.457433 | 4.385036 | 3.277187 |

S2 分项中位数（秒，列独立取中位数，不保证相加等于总列）：

| 负载 | 同步（含 Git 前态） | 子进程 | 求 ε（含 Git 后态） | 提交 | 总计 |
| --- | --- | --- | --- | --- | --- |
| 全量构建 | 0.009237 | 53.652393 | 0.000887 | 0.000000 | 53.670997 |
| checkout 甲 | 2.510247 | 0.362593 | 0.085302 | 2.763076 | 5.732193 |
| checkout 乙 b→a | 0.117103 | 0.335959 | 0.083721 | 2.758343 | 3.300720 |
| checkout 乙 a→b | 0.119670 | 0.345636 | 0.084087 | 2.718907 | 3.277187 |

20,000 次 open+close 的探针内部单次中位数（µs）：

| 路径 | none | ns | B3 | S | S2 |
| --- | --- | --- | --- | --- | --- |
| /dev/null | 3.412 | 3.388 | 28.942 | 3.396 | 3.405 |
| a | 3.381 | 3.376 | 169.541 | 3.381 | 4.696 |

普通路径的 S2 open+close 明显高于 S/ns；S2 在这里还承担常驻监听和事件消费。该微基准的增幅没有在全量构建中按同样比例出现。

无改动固定开销中位数（ms），每树额外 100 个目录，该夹具没有 `.git/`。S2/S2-full 为 begin+finish+commit 的 RPC 总耗时，不含子进程；其他配置执行 `true`，因此还包含其启动成本，不能用该表声称 S2 启动比 none 快。S2-full 强制两侧全内容核对。

| 条目数 | none | ns | B3 | S | S2 | S2-full |
| --- | --- | --- | --- | --- | --- | --- |
| 3100 | 0.772 | 1.807 | 2.426 | 239.439 | 0.325 | 84.783 |
| 10100 | 0.772 | 1.767 | 2.520 | 810.426 | 0.358 | 276.620 |
| 100100 | 0.794 | 1.820 | 2.456 | 9442.265 | 0.341 | 2847.449 |

含真实 Git 索引的无改动 S2 act 另测三次，仍为 begin+finish+commit，不运行子进程；包含 Git 前态快照、结束摘要与 settled 标记。启动全扫不计入该常驻成本。

| 跟踪文件数 | index 字节 | 完整 act 中位数 ms | 最小 ms | 最大 ms |
| --- | --- | --- | --- | --- |
| 10000 | 803127 | 8.092 | 7.010 | 15.120 |
| 100000 | 8003226 | 16.400 | 16.367 | 66.993 |

固定开销目标按题目的中位数口径判断；10 万文件含 Git 的首个样本为 66.993 ms，不能宣称每次均低于 50 ms。[Git 无改动 act 原始样本](results/v5/git-fixed-v5.json)。

提交应用中位数（ms）：

| 配置 | 改变文件数 | 求 ε | 准备 | 应用及持久化 | 提交总计 |
| --- | --- | --- | --- | --- | --- |
| S | 1 | 54.808 | 55.326 | 62.031 | 119.765 |
| S2 | 1 | 0.056 | 5.050 | 4.011 | 9.698 |
| S | 100 | 56.725 | 67.774 | 133.126 | 201.098 |
| S2 | 100 | 1.953 | 14.504 | 57.714 | 72.218 |
| S | 4000 | 140.527 | 571.002 | 2801.747 | 3387.989 |
| S2 | 4000 | 79.536 | 324.495 | 2128.386 | 2456.403 |

目录 rename 完整 act 中位数（秒）：

| 目录内文件数 | none | ns | B3 | S | S2 |
| --- | --- | --- | --- | --- | --- |
| 1000 | 0.001356 | 0.002362 | 0.005195 | 1.047558 | 0.100979 |
| 10000 | 0.001343 | 0.002351 | 0.005145 | 10.068778 | 0.921256 |

S2 初次建影子中位数（秒、MiB），每规模三次新建脏输入，预刷盘计入初始化：

| 文件数 | 节点初始化秒 | 影子 apparent MiB | 影子 allocated MiB | 状态 allocated MiB |
| --- | --- | --- | --- | --- |
| 3000 | 1.681790 | 1.06 | 12.11 | 13.68 |
| 10000 | 6.326007 | 3.63 | 39.85 | 52.91 |
| 100000 | 80.583179 | 39.33 | 395.32 | 499.13 |

同主机重新运行 S 初建（三次，秒）：

| 文件数 | S | S2 |
| --- | --- | --- |
| 3000 | 3.307259 | 1.681790 |
| 10000 | 11.076556 | 6.326007 |
| 100000 | 101.808181 | 80.583179 |

无沙箱/ns/B3 不建立影子；提交应用表仅适用于 S/S2，其余配置原地修改，没有此合并阶段。

当前提交应用与初建仍有显著逐文件成本；这些数值是本原型的结果，不是 S 路线的性能下限。

`du` 的 allocated 包括 reflink 共享块，不能当成新增物理块。初建、同步、提交、目录改名原始结果分别见 [initial](results/v5/initial-v5.json)、[sync](results/v5/sync-v5.json)、[settlement](results/v5/settlement-v5.json)、[renames](results/v5/renames-v5.json)。
<!-- /PERFORMANCE -->

<!-- WATCHES -->
| 条目数 | 真实 watch | 影子 watch | 节点 RSS MiB（中位数） |
| --- | --- | --- | --- |
| 3100 | 101 | 101 | 11.05 |
| 10100 | 101 | 101 | 30.17 |
| 100100 | 101 | 101 | 260.59 |

监听上限实测为 **250,895 / UID**。两棵树均需要每目录一个 watch，忽略同 UID 其他程序时最多约 125,447 个被跟踪目录；文件数本身不等于 watch 数。该规模夹具把文件分在 100 个目录，故不能用 100,000 文件通过证明任意目录布局都可用。额外 [watch 密度样本](results/v5/watch-density-v5.json)记录 1,000/10,000 目录布局。RSS 包括 Rust 两棵索引和线程等进程内存，不含内核 watch slab；没有权限隔离同 UID 其他消费者，也没有将内核内存估算冒充测量。
<!-- /WATCHES -->

## 复现与交付

所有命令在本 worktree 根目录执行，构建产物与工作负载只在原型 `.scratch/`：

```bash
make -C prototypes/write-intercept v3
gcc -O2 -Wall -Wextra prototypes/write-intercept/namespace-v4.c -o prototypes/write-intercept/.scratch/namespace-v4
CARGO_HOME="$PWD/prototypes/write-intercept/.scratch/cargo-home" CARGO_TARGET_DIR="$PWD/prototypes/write-intercept/.scratch/rust-target-v5" cargo build --offline --release --manifest-path prototypes/write-intercept/shadow-v5/Cargo.toml
python3 prototypes/write-intercept/verify-v5.py coverage boundaries external_writes
python3 prototypes/write-intercept/research-v5.py randomized completeness windows crashes supplements density downtime rename_links index_recovery ledger_compat
python3 prototypes/write-intercept/git-v5.py discard roundtrip recovery ref_conflicts minimal_risk fixed_git
python3 prototypes/write-intercept/git-regression-v5.py
python3 prototypes/write-intercept/benchmarks-v5.py full checkout micro scales renames baseline_initial incremental
python3 prototypes/write-intercept/summarize-v5.py
python3 prototypes/write-intercept/audit-v5.py
python3 prototypes/write-intercept/docs-check-v5.py
```

<!-- DELIVERY -->
交付主文件为本报告、`shadow-v5/` crate、Python 驱动与 `results/v5/`；完整绝对路径列表见 [manifest](results/v5/manifest-v5.json)。状态目录、负载副本、Cargo 缓存和二进制仅在 `.scratch/`，不进 Git。

验证入口与结果：[最终编译与格式检查](results/v5/compile-final-v5.json)记录 Rust release build、fmt 与 Python 驱动编译检查通过；[矩阵审计](results/v5/audit-v5.json)的 45 项检查验证覆盖、规模样本与反例记录自洽。审计的 `passed` 表示证据矩阵完整，不表示采用目标全部达到。文档链接使用本 worktree 的临时 Git 索引检查，原 Git 索引保持不变，结果见 [docs-check](results/v5/docs-check-temporary-index-v5.json)。结束前核对没有遗留本任务的沙箱、监听或后台负载进程，见[清理核对](results/v5/cleanup-v5.json)。

[增量构建](results/v5/incremental-v5.json)只重编 `lattice-cli`，随后宿主不重编。[B3 对象兼容](results/v5/ledger-compat-v5.json)把 S2 简单文件修改的事务前态投影成 B3 的 ledger/poststate 与前态文件命名后，调用原版 recover-v3 求逆并核对内容；这不是原生账本 ABI 兼容，也不是正式回执接线。B3/S 实现直接复用。无全仓 CI 或正式 API 测试，因为改动范围是独立研究原型。
<!-- /DELIVERY -->

## 待维护者裁决与尚未完成

1. **完整性前提**：是否允许限制宿主写入方式；若不允许，inotify 只能用作加速提示，需要完整内容核对或另外的强制写入跟踪。不能同时宣称“不限制外部 mmap”与“纯事件候选保证不漏”。
2. **提交冲突语义**：是否接受实测的最后核对窗口，还是需要宿主写者配合的排他协议。用户态逐文件比对不能自行实现原子条件 rename。
3. **Git 归属**：共享 HEAD/refs/index 的 act 内外部改动无法靠结束摘要归属。需要明确序列化宿主 Git、独立暂存区/引用、或其他 Git 事务边界；这些会改变可见语义，原型不擅自修改 ADR。
4. **恢复责任**：普通事务 SIGKILL 恢复不等于停机外部写保护；Git 无结束证明时的恢复、断电与恢复中二次崩溃仍需设计及验证。
5. **生产范围**：已有跨类别别名、uid/gid 与 ACL/扩展属性保真及 xattr 策略、UTF-8 限制、多根目录、Git worktree 的 `.git` 文件、split-index/reftable、路径替换攻击、`.s2-v5-*` 临时名碰撞防护、正式回执与前态保留期均未完成。监听启动与同步不是原子快照；监听异常、目录移出树后的 watch 回收与长期日志增长也需要生产化设计。

对 ADR 的建议仅在本报告：保留第 6 节机制选择待定，记录 S2 已验证的性能收益与上述未解决前提；先明确 Git 的可逆范围和外部写者冲突合同，再决定 B3 或 S 系列的产品实现。
