---
status: draft
---

# 写拦截第七轮：S3 性能与私有挂载

## 结论与验收口径

本报告来自 `/home/gcp-vegas/.t3/worktrees/lancet/t3-dda7fff2`，起点为 `origin/exp/write-intercept` 的前六轮原型。维护者已选择 S3；本轮没有修改 ADR、提交、推送或执行 GitHub 写操作。前六轮文件保持原样。

本轮实现将到期扫描移出 RPC 线程、按影子事件计算改动、保留影子 inode、直接登记合并后态、复用私有 Git index，并将执行环境的可写目录全部放进状态目录。五项指标在本轮已跑样本中达标，逐项判定见下表；独立合并基准仍有退步，不能解释成所有负载都已优化成功。数字是本机样本，不是最坏时延保证。

批次结局与跨文件同时可见分别报告：普通故障注入恢复到前态或后态，逐文件交换仍有中间可见窗口。故意在回滚期间再次改写已合并项的宿主写者仍能导致 `Indeterminate`；本轮没有证明任意不合作宿主写者下无条件的“全部合并或全部不合并”。因此不建议将该无条件承诺直接写入 ADR 第 6 节。

<!-- TARGETS -->

| 第 5 项验收目标 | 判定 | 实测依据 |
| --- | --- | --- |
| 10 万条目空 act ≤ 50 ms | 达标（前轮同口径） | 100,000 文件加 1,000 目录；性能最终版本九次 RPC，中位数 0.457 ms；不含 Git 仓库 |
| 情形乙 4,000 文件 checkout ≤ 2 s | 达标（中位数） | b→a 1.824377 s，a→b 1.816329 s；保留最大 3.09 s 的单次样本，不承诺每次低于 2 s |
| 构建开销与只有命名空间相当 | 达标（≤ 3% 口径） | S3 54.576742 s，ns 53.872406 s，相差 +1.31%；三样本不能证明统计等效 |
| 改动集合与全扫描/全内容对照完全一致 | 已跑样本全部一致 | 126 继承、200 随机、45 宿主同步、20 连续 mmap、真实溢出；有限测试，不是一般性完备证明 |
| 第六轮语义及恢复 27/27，多链接无 Indeterminate | 按本轮已定规则通过 | 24 次普通 SIGKILL +3 次多链接前置拒绝；Git 114/114，Git SIGKILL 18/18；回滚再遭宿主写入的 3 次 Indeterminate 单列 |

<!-- /TARGETS -->

## 实现

| 改动 | 实现与依据 |
| --- | --- |
| 到期扫描 | [background-v7.rs](shadow-v7/src/background-v7.rs) 的独立线程计算扫描结果；RPC 只取已经就绪的候选，不等扫描结束。100 ms 周期扫描缓存由节点索引增量更新。合并前置取消标志让扫描主动退出，节点不 join、不等待扫描 |
| 宿主同步 | 事件候选与后台候选只触发相应路径物化；后台仍检查漏通知的外部别名和长期 mmap。显式 `sync`、事件溢出或不确定事件仍可要求完整对照；这些不是“到期扫描到点就挡住 RPC” |
| 提交版本 | [main-v7.rs](shadow-v7/src/main-v7.rs) 从影子 FICLONE 到宿主目标目录的私有临时名，交换在目标目录内完成，换出的宿主 inode 移入状态目录前态区。新建父目录尚不存在时，先暂存状态目录，父目录应用后移到同目录临时名再安装。影子 inode 留在原处 |
| 准备与恢复 | 临时名清单先持久化，再克隆；事务日志仍在应用前持久化。恢复识别交换后、前态搬回状态目录前的窗口；未完成准备的临时名从持久清单清理。日志读取改为整块缓冲，避免 `serde_json::from_reader(File)` 的逐字节系统调用 |
| 自写索引 | 合并后将已验证的摘要及新 inode/stat 直接记入宿主索引，再给后台派生缓存打补丁；自写事件 stat 相同即跳过。扫描中的候选也核对最新索引，避免旧候选重新物化自写文件 |
| 影子事件 | 目录监听包括 `IN_MODIFY`、`IN_CLOSE_WRITE`、名字与属性变化。按事件扩展目录后代、硬链接组；`IN_Q_OVERFLOW` 或无法确认新目录事件顺序时全量退回。正常无改动结束不遍历整棵树；审计模式另跑全扫描，不进性能计时 |
| Git index | [git-v7.py](git-v7.py) 保存宿主 HEAD/index/packed-refs/refs 的 stat 指纹；未变时保持私有 index。宿主变化时重新读取 HEAD 与索引；丢弃使下一次同步重建私有 Git 状态。config/hooks 永不合并 |
| 初始化 | 普通目录先建立，独立文件和链接组由 8 个 Rust 工作者并行克隆，最后恢复目录元数据并落盘。`target/` 种子单独使用 `cp -a --reflink=always` |
| 多链接 | 在任何宿主应用前检查变更路径的基准与当前 `nlink`；宿主普通文件链接数大于 1 时整批 `Conflict`。不再原地写宿主文件；没有实现 XFS exchange-range 或外部别名启动拒绝 |
| hidden | 宿主顶层 `hidden` 不进入影子；影子侧仍监听新建的 `hidden`，改动集中有该路径时整批拒绝并恢复影子。不是在子进程新建入口逐个拒绝 |

`real_tree` 是节点发布的索引；后台的基准和扫描结果是派生缓存，不独立决定合并基准。合并仍按本次同步到影子的版本逐项检查宿主内容。后台扫描与候选同步不是单一全树快照；活动写者停止后的补齐由独立全内容 oracle 检查。

### 挂载与执行环境生命周期

[namespace-v7.c](namespace-v7.c) 建立独立 user/mount/PID/network 命名空间与私有根，命令结束时 PID namespace init 退出，内核终止其余进程。每次 `execute` 创建新的执行环境。

| 沙箱路径 | 本轮来源与权限 |
| --- | --- |
| `/usr`、`/etc`，`/bin` 等系统链接 | 宿主只读；不挂宿主根目录 |
| `$HOME` 原绝对路径 | `STATE/home` 可写，节点生成的 `.gitconfig` 只含 bench 用户名、邮箱 |
| Rust 工具链及 Cargo bin | 宿主 `.rustup` 和 `.cargo/bin` 只读绑定在原路径；其他显式只读资源由启动器参数指定 |
| Cargo 缓存 | `STATE/home/.cargo`，只 reflink `registry/cache` 和 `registry/index`；`registry/src` 由 Cargo 在私有目录解压 |
| 可写工作目录 | `STATE/shadow`，绑定到真实工作目录绝对路径 |
| `target/` | 影子私有种子，独立 bind；不合并 |
| `.git/` | 私有目录，独立 bind；宿主 objects 再单独只读 bind，alternates 指向该只读挂载 |
| 只读工作目录 | 宿主只读 bind；已有顶层 `hidden/` 以状态中的空目录遮蔽 |
| `/tmp` | `STATE/tmp` 可写 |
| `/proc`、`/dev` | 新 PID namespace 的 proc；状态目录中的私有 dev 目录，仅 bind null/zero/random/urandom 四个字符设备 |

节点启动检查所有默认可写目录及显式来源清单必须在规范化后的状态目录内；启动器每一次可写目录 bind 也检查实际来源，不接受可写宿主目录。挂载目标逐级拒绝符号链接，避免私有家目录里的路径重定向影响后续挂载准备。字符设备是最小设备接口，不是可写宿主目录；无特权原型使用 bind 设备节点，没有创建新的设备 inode。

`target/` 与 `.git/` 在启动前创建并分别挂载，正常路径与它们之间、它们彼此之间的硬链接均由内核返回 `EXDEV`。原型只实现固定顶层分类：运行中新增 `nested/target` 等未配置路径按常规路径监听、结算和合并，不因名字像缓存而豁免；顶层挂载点不能被删除后替换。推广到任意规则需要在启动时预建所有明确的类别挂载点，或者继续将未覆盖路径当常规路径。本轮没有动态挂载或持久后台环境。

测试只在 `.scratch/fake-home-v7` 创建宿主家目录哨兵，未在真实家目录写测试文件，未读取真实凭据文件。工具链和指定旧工作负载只读使用；所有 Cargo 写入都在当前 worktree 的私有缓存。

## 正确性证据

| 验证 | 本轮结果 | 原始证据 |
| --- | --- | --- |
| 63 类 × 提交/丢弃 | 126/126 | [coverage](results/v7/coverage-v7.json) |
| 随机宿主/影子负载 | 200/200 | [randomized](results/v7/randomized-v7.json) |
| 宿主同步与全内容 oracle | 45/45 | [synchronization](results/v7/synchronization-v7.json) |
| 连续 mmap 写者暂停后的补齐 | 20/20 | [continuous mmap](results/v7/continuous-mmap-v7.json) |
| 同文件宿主写入 | 40/40 Conflict | [concurrency](results/v7/concurrency-v7.json) |
| 异文件宿主写入 | 20/20 Committed | 同上 |
| 后项冲突、交换检查窗口、回滚再冲突 | 各 3 次；前两组 Conflict，回滚再冲突 Indeterminate | 同上 |
| 普通崩溃/多链接拒绝 | 27/27：24 次 SIGKILL 恢复，3 次在原“部分写”故障点前整批 Conflict | [crashes](results/v7/crashes-v7.json) |
| Git 操作矩阵与额外场景 | 96/96 + 18/18 | [matrix](results/v7/git-matrix-v7.json)、[extra](results/v7/git-extra-v7.json) |
| Git 崩溃 | 18/18 | [git crashes](results/v7/git-crashes-v7.json) |
| 跨类别硬链接、外部别名、假 HOME、hidden、启动拒绝、proc 生命周期 | 见逐项断言 | [isolation](results/v7/isolation-v7.json) |
| 真实队列溢出 | 暂停节点后产生 20,000 次创建/删除；观察到 overflow 并退回，全扫描/全内容一致 | 同上 |
| 原子交换后到前态归档前 SIGKILL | 另 3 次 | [exchange crashes](results/v7/exchange-crashes-v7.json) |
| 自写不重算摘要与影子 inode 保留 | 见计数与 inode 断言 | [self writes](results/v7/self-writes-v7.json) |
| 宿主 Git checkout 同步 | 两方向 ×3，全内容与 HEAD 一致 | [host Git sync](results/v7/host-git-sync-v7.json) |

多链接验收口径按维护者本轮决定更新，未将拒绝记为成功合并。原来的 `inplace-partial` 三个样本已不能触发那个写入阶段；现在核对“整批 Conflict、所有宿主路径保持前态、重启仍保持前态”，没有把未发生的 SIGKILL 算作一次杀进程恢复。覆盖与随机负载的独立 oracle 仍检查改动集合，非仅检查返回码。

继承的目录改名+硬链接四组合在旧“必须合并并保 inode”判据下为 0/4，保存在 [旧判据结果](results/v7/rename-links-v7.json)。本轮规则判据另存 [policy](results/v7/rename-links-policy-v7.json)，要求整批冲突且原 inode 与全部宿主内容不变；这是已决定规则的验收更新，不是扩大允许失败范围。

仅用 `IN_CLOSE_WRITE` 的历史反例在 [close-write](results/v7/close-write-v7.json) 复跑；本轮 `IN_MODIFY` 覆盖了只读标志截断。真正的跨类别别名由挂载隔离消除。随机测试还通过测试进程直接修改影子以构造复杂名字/链接变化；跨类别边界则在真实执行环境里另测。

## 性能

[五配置交互比较图](results/v7/performance-v7.html)。

无沙箱、只有命名空间与本轮 S3 重新测量；B3 与 S3 v6 引用第六轮原始结果。所有表取至少三样本中位数，保留离群样本。页缓存未清空，未使用 root；LATTICE 的 Cargo 参数与前轮一致：4 jobs、offline、debug=0、incremental=0，每次清空对应 target。私有 Cargo 源码先解压，解压与种子复制成本单列。

情形甲每次由宿主复位到 a；情形乙只在预热前由宿主初始化，之后同一节点交替 a/b。完整 act 的墙钟覆盖 begin、命令、finish、commit 和期间等待。空 act 的最终验证每次先等待 110 ms，使后台扫描有机会到期；该等待在 RPC 发出前，不计入 RPC 墙钟。无沙箱/ns/B3 用 `true`，与不创建进程的 S3 空 RPC 口径不同。

<!-- PERFORMANCE -->

五配置总时间（秒；每格至少三个样本，历史两列明确标记）：

| 负载 | none | ns | B3（v6） | S3（v6） | S3（v7） |
| --- | --- | --- | --- | --- | --- |
| LATTICE 全量构建 | 54.077734 | 53.872406 | 54.897473 | 54.127087 | 54.576742 |
| checkout 甲 a→b | 0.250647 | 0.277825 | 13.801717 | 10.213363 | 5.017621 |
| checkout 乙 b→a | 0.245152 | 0.251378 | 12.560764 | 5.228439 | 1.824377 |
| checkout 乙 a→b | 0.244770 | 0.246313 | 12.671173 | 5.180129 | 1.816329 |


20,000 次 open+close，探针内部单次中位数（µs）：

| 路径 | none | ns | B3（v6） | S3（v6） | S3（v7） |
| --- | --- | --- | --- | --- | --- |
| /dev/null | 3.393 | 3.400 | 28.927 | 3.394 | 3.399 |
| a | 3.187 | 3.174 | 172.173 | 4.613 | 4.755 |


空 act RPC 墙钟（ms；none/ns/B3 含 true，S3 为 begin+finish+commit）：

| 文件数（另加 1,000 目录） | none | ns | B3（v6） | S3（v6） | S3（v7） |
| --- | --- | --- | --- | --- | --- |
| 3000 | 0.807 | 1.858 | 2.399 | 9.952 | 0.453 |
| 10000 | 0.762 | 1.780 | 2.371 | 25.832 | 0.451 |
| 100000 | 0.779 | 1.817 | 2.374 | 653.784 | 0.457 |


本轮情形甲/乙分项（秒；每列独立中位数）：

| 方向 | begin | 命令 | 结束候选 | 准备 | 应用/落盘 | 索引 | 重建 | RPC差额 | 总计 |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| 甲 a→b | 2.196951 | 0.388015 | 0.086100 | 0.924356 | 0.322827 | 0.019937 | 0.000000 | 1.063475 | 5.017621 |
| 乙 b→a | 0.097418 | 0.384331 | 0.085179 | 0.922347 | 0.304466 | 0.020044 | 0.000000 | 0.006349 | 1.824377 |
| 乙 a→b | 0.095065 | 0.386343 | 0.085959 | 0.899066 | 0.324400 | 0.020302 | 0.000000 | 0.006357 | 1.816329 |


性能最终版本空 act 分项（ms；每样本前静置 110 ms，允许后台扫描到期）：

| 文件数 | begin | finish | commit | RPC差额 | 总计 |
| --- | --- | --- | --- | --- | --- |
| 3000 | 0.123 | 0.039 | 0.007 | 0.275 | 0.453 |
| 10000 | 0.126 | 0.036 | 0.008 | 0.296 | 0.451 |
| 100000 | 0.137 | 0.031 | 0.008 | 0.287 | 0.457 |


目录改名完整 act（秒；宿主复位同步在计时前）：

| 目录内文件数 | none | ns | B3（v6） | S3（v6） | S3（v7） |
| --- | --- | --- | --- | --- | --- |
| 1000 | 0.001374 | 0.002378 | 0.005025 | 0.846511 | 0.097930 |
| 10000 | 0.001401 | 0.002386 | 0.005452 | 8.411384 | 1.131041 |


合并阶段（ms；none/ns/B3 原地写入，没有独立合并阶段）：

| 配置 | 文件数 | 求改动集合 | 合并含索引/重建 | 中间可见包络 |
| --- | --- | --- | --- | --- |
| S3 v6 | 1 | 8.132 | 6.061 | 0.048 |
| S3 v7 | 1 | 0.072 | 164.915 | 0.088 |
| S3 v6 | 100 | 8.355 | 71.339 | 3.925 |
| S3 v7 | 100 | 2.055 | 71.186 | 6.781 |
| S3 v6 | 4000 | 18.158 | 2352.359 | 171.306 |
| S3 v7 | 4000 | 95.940 | 3441.552 | 316.605 |


初次建立影子（客户端墙钟秒；每规模三次）：

| 文件数 | S3 v6 | S3 v7 |
| --- | --- | --- |
| 3000 | 0.766382 | 0.880979 |
| 10000 | 4.761833 | 4.944799 |
| 100000 | 73.861152 | 67.410504 |


Git 索引（ms）：

| 跟踪文件数 | 未变化 helper | 未变化 begin RPC | 宿主变化 helper | 提交总计 |
| --- | --- | --- | --- | --- |
| 10000 | 0.634 | 34.005 | 80.754 | 179.367 |
| 100000 | 0.818 | 41.639 | 253.754 | 722.731 |


私有缓存及 target 种子（秒；各三次）：

| 操作 | 中位数 |
| --- | --- |
| Cargo cache/index reflink | 0.179113 |
| 首次 cargo metadata 与源码解压 | 4.752733 |
| LATTICE target reflink | 0.297870 |


<!-- /PERFORMANCE -->

**独立合并基准存在退步且原因未完全定位。** 本轮 1 文件的三个样本为 306.791 / 164.915 / 10.378 ms；4,000 文件为 0.889 / 3.442 / 5.189 s，中位数比 v6 的 2.352 s 更慢。后两个 4,000 文件样本的准备阶段分别为 2.878 / 4.861 s，当前分项不足以再区分克隆、日志与 `syncfs` 的贡献；不能未经额外测量就归咎于共享主机。该退步是后续性能排查事项，未通过选择更快样本或反复重跑改变中位数。

候选发布与影子物化仍在节点主线程；全量扫描不再阻塞请求，并不意味着大量宿主改动的同步也无等待。情形甲的 RPC 差额中位数为 1.063 s，当前未对发布/调度进一步分项，不能将这部分说成已经消除。情形乙没有宿主每次复位，RPC 差额约 6 ms。

独立合并阶段的 none/ns/B3 不适用，不能用原地写入时间冒充合并时间。目录改名仍要核对后代，成本随文件数增加；本轮没有宣称 O(1)。中间可见窗口记录首项应用核对开始到末项应用完成的包络，不是多个文件同时可见，也不是外部轮询测到的严格窗口上界。

初次创建的三样本来自较早、克隆逻辑相同的第七轮二进制，摘要见 [scale binary](results/v7/scale-binary-v7.sha256)；后续变更为扫描让出合并窗口、日志缓冲读取与计数。性能最终版本另测全部三个规模的空 RPC，checkout/合并/改名/构建也全部重测，计时二进制摘要见 [benchmark binary](results/v7/benchmark-binary-v7.sha256)。交付前最后收紧默认可写来源的启动校验和客户端写入私有 HOME 配置前的校验（配置文件用 `O_NOFOLLOW` 打开）；这只增加初始化检查，不改变已测 act 阶段。该收紧后复跑 126 项覆盖与隔离检查。性能试跑及中途停止的日志与数据保留，不用于最终表。首次尝试中还发生过启动器根 bind 隐藏子挂载、缓存脚本重复指定 `CARGO_HOME` 的错误，修正后复跑，未将失败构建或未执行命令计作样本。

## AIO、io_uring 与私有缓存

父 Agent 的 C/Python 探针已复制为 [asyncwrite-v7.c](asyncwrite-v7.c) 与 [async-probe-v7.py](async-probe-v7.py)。本轮按 `aio` 模式各写 1 GiB，提交后立即自发 SIGKILL。原探针在 `wait()` 返回后立刻检查 17 个 O_DIRECT 采样点，本轮另加 1 GiB 全内容验证；这比“17 点全到达”更强，但不将顺序读取等同于瞬时全盘快照。

本次 submit→reap 分别为 997.2 / 1555.9 ms（2/2）；17/17 即时采样及随后全内容校验均通过。实测与时间线见 [AIO](results/v7/aio-v7.json)。两次回收后均未观察到继续到达的采样或后续 inotify，完整内容验证通过。观察支持本机内核会在进程回收前等待该 AIO 写结束；不是任意异步 I/O 的通用证明。`kernel.io_uring_disabled = 2`，只读取并记录 [内核开关](results/v7/io-uring-v7.txt)，未改 sysctl、未试图启用 io_uring。

[cache-v7.py](cache-v7.py) 每次只复制压缩包与索引，确认解压前不存在 `registry/src`，再用 `cargo metadata --offline` 触发源码解压；该时间包括 metadata 解析，不声称是纯解压 CPU 时间。后续全量构建使用已解压的私有缓存，跟无沙箱/ns 的 `.scratch/cargo-home` 构建对照。

## ADR 第 6 节建议与未解决事项

1. 将 S3 写成已选方向，把 B3 的 seccomp 代执行设计移到否决方案，引用第五至七轮证据；不要继续把执行环境可写层级绑定到宿主缓存。
2. 明确常规文件合并基于影子基准逐项比较；普通路径、Git 引用与索引经同一事务结算。config/hooks 永不合并。多链接宿主文件在变更集中即整批 Conflict；不引入原地写回或 exchange-range。
3. 将“结局原子性”与“多文件同时可见”分开。27 个继承故障点及新增交换窗口只证明这些 SIGKILL 样本；没有测机器掉电、磁盘损坏、写回错误或恢复过程中再次崩溃。回滚再遇宿主写入的 Indeterminate 仍需维护者明确契约，不能写成已解决。
4. 写入完整挂载表，并在实现入口检查可写目录来源。无需检测或拒绝宿主外部硬链接别名；隔离依赖任何可写挂载都来自私有状态。工具链与 objects 为只读例外，最小设备接口单列。
5. 写明类别挂载及新建未覆盖路径规则。原型仅固定顶层 `target`、`.git`、`hidden`；任意 glob、多工作目录、linked-worktree 的 `.git` 文件、pack/reftable/SHA-256、生产级恶意路径竞态防护都未覆盖。
6. 私有 Git index 可保留，但宿主 HEAD/index 确实变化时仍重建；表中将两种成本分开。已有阶段性 stage 不隐式合并，暂存、config/hooks 的语义仍由 Git 矩阵核对。
7. 独立合并基准出现上述退步，准备阶段仍需进一步分解和优化；是否将 1/100/4,000 文件合并单独设为硬性时延验收线，交维护者决定。初次影子建立仍昂贵，应作为节点初始化成本披露。本轮不把初始化成本摊入空 act，也不因此承诺所有规模的初始化时延上限。

本原型没有接入正式 `proc.*` API、Starlark `commit()`、协议回执、WASI、生产级配置加载或完整不可信代码沙箱。`commit`/`discard` RPC 是研究结算入口；范围之外的产品接线需要后续实现。没有设计持久执行环境或后台进程，没有兼容其他操作系统或文件系统的方案。

## 复现与交付检查

```sh
export CARGO_HOME="$PWD/prototypes/write-intercept/.scratch/cargo-home"
export CARGO_TARGET_DIR="$PWD/prototypes/write-intercept/.scratch/rust-target-v7"
cargo build --offline --release --manifest-path prototypes/write-intercept/shadow-v7/Cargo.toml
cc -O2 -Wall -Wextra prototypes/write-intercept/namespace-v7.c -o prototypes/write-intercept/.scratch/namespace-v7
python3 prototypes/write-intercept/verify-v7.py
python3 prototypes/write-intercept/research-v7.py
python3 prototypes/write-intercept/isolation-v7.py
python3 prototypes/write-intercept/verify-git-v7.py matrix extra scale
python3 prototypes/write-intercept/supplement-v7.py git_crashes reverts starvation model host_git_sync
python3 prototypes/write-intercept/cache-v7.py
python3 prototypes/write-intercept/benchmarks-v7.py checkout micro scales merges renames full
python3 prototypes/write-intercept/summarize-v7.py
```

初次工作负载与编译缓存使用 `cp -a --reflink=always` 从指定第六轮副本复制；源码来自用户指定的 Git checkout。只读旧副本不作为写入路径。完整交付清单、来源摘要、基线完整性、检查结果和进程清理记录分别保存在 `results/v7/`，不包含 `.scratch/` 和编译产物。


最终交付检查：

- `cargo build --offline --release --manifest-path prototypes/write-intercept/shadow-v7/Cargo.toml` 通过；C 编译无警告。
- `cargo --offline fmt ... -- --check`、所有 Rust include 文件的 `rustfmt --check`、`python3 -m py_compile prototypes/write-intercept/*-v7.py`、C `-fsyntax-only` 通过。
- `cargo --offline xtask docs-check` 通过（26 个已跟踪 Markdown）；新报告未暂存，另行验证其所有相对链接。
- [汇总断言](results/v7/validation-v7.json) 28/28 通过；[前六轮完整性](results/v7/baseline-integrity-v7.json) 无差异；[环境与副本来源](results/v7/environment-v7.json)、[源文件及二进制摘要](results/v7/source-hashes-v7.json) 可复核。
- [完整交付文件清单（绝对路径）](results/v7/files-v7.txt)。[进程清理](results/v7/cleanup-v7.json) 未发现本轮沙箱、节点、异步写探针残留。
- 未运行全仓业务 CI；本次只新增独立研究原型，没有业务 crate 或 ADR 改动。导入前六轮的文件因指定的 `git checkout ... -- prototypes/write-intercept` 已在索引中；新增第七轮文件保持未跟踪，未提交或推送。

## 审查补充（主 Agent）

- **独立合并的波动集中在调用 `syncfs` 的阶段。** 每次提交调用三次 `syncfs`：准备阶段两次，应用阶段一次。`syncfs` 要写回整个文件系统的脏数据，合并时延因此取决于无关写入。本机实测（[脚本](syncfs-review-v7.py)、[结果](results/v7/syncfs-review-v7.json)）：文件系统空闲时 `syncfs` 0.5 ms；另有 1 GiB 无关脏数据时 7.2 s，2 GiB 时 14.5 s；同样条件下只对一个小文件 `fsync` 为 117 ms 与 184 ms。构建刚写完 `target/` 就提交是 agent 的常见步骤，实现时合并只落盘本批文件与所在目录，不用 `syncfs`。
- **`isolation-v7.py` 的 mounts 用例有一条断言恒真。** 它比较宿主文件 `a` 与其外部硬链接别名，两者是同一个 inode。测试后直接读取：宿主文件仍是初始内容 `a:BEFORE…`，只读工作目录的文件仍是 `ro-host`，沙箱没有写到这两个 inode。
