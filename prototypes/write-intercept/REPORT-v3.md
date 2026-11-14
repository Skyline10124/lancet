---
status: draft
---

# 写拦截第三轮：ADR 0002 第 6 节修订规则与性能

## 结论与验收口径

本轮尚不足以建议按现文接受第 6 节。Landlock 对“可写路径被换成常规路径”的阻断成立，但两个均获写权限的类别之间，检查后的 `link` / `rename` 参数改写仍能成功。完整代执行 167.57 µs 未达到 30 µs；动态 mount 上下文有两项差异。结构化改名正确，但未刷盘输入下的墙钟成本随规模增长，性能口径需要维护者裁决。

实验仅覆盖 ADR 的“尚未核实”最后一行和第 6 节性能要求。它是独立 C 原型及外置 oracle，没有接入节点、Starlark 或协议。基于 `origin/exp/write-intercept` 的第二轮源码迭代；第一、二轮报告、源码及结果保留原样。工作区为 `/home/gcp-vegas/.t3/worktrees/lancet/t3-2a72ab2f`。

环境命令与基准提交见 [environment.json](results/v3/environment.json)：Rocky Linux 10.2、Linux 6.12、SELinux Enforcing、Landlock ABI 6、XFS。相等性 oracle 比较内容、存在性、类型与权限；硬链接和结构化改名另核对 dev/inode/nlink。时间戳与所有者不在本轮相等性口径内。

<!-- RULES:START -->
| 规则 | 结论 | 有效证据与限制 |
| --- | --- | --- |
| 1 Landlock 与 CONTINUE | 部分通过 | 七类改写各 200 次，1,400/1,400 拒绝、常规路径变化 0；空/缺失 target 构建均完成。预建硬链接 200/200 绕过；可写树之间 link、rename 各 1/1 绕过类别约束 |
| 2 目录锁 | 通过 | 内部四类 80 次、全目录锁 20 次均无锁内插入；外部 100 次检测 40、漏检 60，按已接受残余风险记录；冲突路径保留、无冲突 b 恢复 |
| 3 硬链接锚点 | 通过 | 四类各 3 次：操作实际成功、锚点存在、不同恢复进程还原后 inode/内容/mode/nlink 精确相等 |
| 4 结构化目录改名 | 部分通过 | 正确性通过，记录数不随文件数变化；预刷盘夹具 1k/10k 各 5 次约 2.00/2.24 ms，未刷盘则 7.94/49.64 ms，墙钟要求需要明确输入持久化条件 |
| 5 合并落盘 | 通过 | 八前态并发×10，观察并发 fsync，所有执行许可晚于本批完成；4,000 文件检出四配置各 3 次，串行 Git 的 max_batch=1，无数值目标 |
| 6 常驻 worker | 部分通过 | 一个进程、启动进入 user/mount 一次、宿主 PID namespace；26 对上下文中 24 对一致，动态 mount 的 2 对因 Landlock EPERM 不一致；其余边缘/连续上下文通过 |
| 7 失效联动 | 通过 | 50 次 SIGKILL；15 次实际观察 ENOSYS，35 次未观察返回；旧 fd 写已登记对象 15 次、允许层级 15 次；最终存活 0；改变项保守 Indeterminate、未变项零残余 |
| 8 性能 | 未通过 | CONTINUE 29.097 µs：达标；代执行 167.567 µs：未达标；全量构建 +0.27%：达标；同类目录改名：结构成本达标、无条件墙钟成本未达标；密集检出已实测、目标待定 |
<!-- RULES:END -->

## 实现与逐项证据

### 1. Landlock 与 CONTINUE

启动垫片先关闭继承 fd，再建立 ABI 3 的文件系统写权限集合（本机查询 ABI 为 6），再安装 seccomp listener。允许启动时存在的 `target/`、`.git/`、显式临时目录、Cargo 缓存与四个设备文件。测试控制页有独立文件授权，属于实验握手设施。授权保存初始目录 dev/inode；不存在的 `target/` 不会在后续被误认为已授权。

节点复制参数后，用目标 root/cwd/dirfd 下的 `O_PATH` 解析对象或父目录；常规对象、未覆盖路径、多链接文件和 chmod 留在代执行路径。快路由接收线程直接回复 `CONTINUE`，避免为无检查点通知增加一次线程调度。参数通过 `process_vm_readv` 复制后验证通知 ID；参数只复制一次，代执行队列使用同一份副本。节点检查的分类不能代替 Landlock 对内核最终使用参数的检查。

`open`、`truncate`、`rename`、`unlink`、`link`、`symlink`、`mkdir` 各 200 次：先检查 `target/` 下的路径，再让同进程另一线程改写参数，最后回复 `CONTINUE`。原始结果见 [landlock-races.json](results/v3/landlock-races.json)。每组同时核对内核 errno、通知路径和常规文件变化，不能只把“文件没变”当作通过。

两个残余构造见 [landlock-residual.json](results/v3/landlock-residual.json)：

- 外部预先把常规 `race-evil` 硬链接到 `target/preexisting-alias`。检查时参数指向单链接的安全对象，检查后改成这个别名，200/200 次直接写入成功，没有普通对象前态。这正是修订 ADR 已列明的残余风险。
- 检查时 `link` / `rename` 两端均在 `target/`，检查后把目的端换成 `.git/`；两种调用各一次成功。Landlock 的权限边界没有表达“常规 / regenerable / .git”三种业务类别，两个可写树之间不因类别不同自动拒绝。它与“跨类别建链禁止”存在独立冲突，不能用“改成常规路径会拒绝”覆盖此例。

从已存在空 `target/` 与启动时无 `target/` 两种状态执行真实 `cargo build --offline --workspace`，见 [missing-target.json](results/v3/missing-target.json)。后者必须观察到代执行，而不是运行期间放宽 Landlock 规则。

建议维护者裁决：所有 `link*` 代执行；需要防止检查后跨类改名时，所有 `rename*` 也代执行。另一个方向是按类别叠加独立 Landlock 域来收窄 REFER，但本轮未实现、未证明可保留全部所需改名语义。已知的预建硬链接风险仍需显式接受，或由额外的输入约束处理。

### 2. 目录锁、外部替换与冲突恢复

节点在检查点、执行和后核对期间持有父目录锁，按 `(dev, ino)` 映射至 257 个互斥锁槽；碰撞只增加串行，不放松互斥。取锁后重新准备对象，避免等待锁期间产生陈旧前态。两个父目录按槽号顺序加锁。目录改名取工作区写锁；当前实现也对普通文件 rename 使用写锁，这是性能上偏保守的实现。已有文件的单纯内容写入不取目录锁，但仍受目录改名的工作区读写锁约束。

沙箱内第二个进程的四种序列分别是：删除后建硬链接、移走后建硬链接、目标端删除后移入、符号链接 ABA。共 80 次；握手记录攻击进程已开始，控制页另记录攻击是否在锁内完成，避免把“进程没有调度到”算作阻断。另有 20 次目录改名与不同子目录已有文件打开的竞争，验证工作区独占。见 [internal-races.json](results/v3/internal-races.json)、[directory-lock.json](results/v3/directory-lock.json)。

沙箱外写者不经过这些锁；检测率只代表人工检查窗口下的五组构造，不能外推为现实命中率。见 [external-races.json](results/v3/external-races.json)。检测到的冲突路径写入状态目录，环境被终止；独立恢复进程读取写后摘要，只恢复无冲突的登记项。每组先修改 `b` 作为无冲突对照，核对它已恢复，冲突路径保持检测后的现状。

### 3. 持久硬链接锚点

删除或覆盖多链接文件之前，节点从稳定 fd 经 `linkat(..., AT_SYMLINK_FOLLOW)` 在同一状态目录生成按 dev/inode 命名的锚点，和前态、账本一起持久化。锚点目录 fd 经执行环境的 `/proc/<init>/root` 打开，使源 fd 与目的目录 fd 处于同一挂载视图。仅仅底层都是 XFS 不够：初版把 worker 传回的源 fd 链到宿主挂载视图的状态目录，实际返回 `EXDEV`。

初版错误及错误 oracle 保留在 [anchors-initial-invalid.json](results/v3/anchors-initial-invalid.json)：操作没有执行，前后态相等不构成恢复成功。最终判定同时要求操作成功、实际生成锚点、无恢复错误、原 inode/内容/模式/nlink 完全一致。

用例包括删一个别名、改名一个别名、删除全部工作区名字、改名覆盖多链接目的对象，各三次。正常节点进程退出后，另起 [recover-v3.py](recover-v3.py) 读取持久账本、锚点、clone 与写后摘要完成恢复，不使用旧进程 fd。恢复器也保留登记集后态中的源 inode，防止恢复 rename 源文件时凭空换 inode。成功恢复并丢弃本次检查点时删除锚点，使链接计数回到原值。见 [anchors.json](results/v3/anchors.json)。

这是受控重启窗口中的恢复；没有写后摘要的崩溃按第 7 项保守结算，不把停机期间的外部编辑强制覆盖。

### 4. 同类别目录改名

普通 rename、`RENAME_NOREPLACE` 与 `RENAME_EXCHANGE` 的同类别目录变换记结构化事件，含原路径、新路径、inode 与交换端身份。普通覆盖只保存目的端前态；跨类别变换沿用逐个 clone。

1,000 与 10,000 文件目录各五次，计时取探针中 syscall 周围的时钟；夹具生成、全树 oracle 与恢复不计时。初版夹具未先刷盘，出现随规模增长的延迟，保留在 [directories-unflushed.json](results/v3/directories-unflushed.json)。补充对照先以八线程 fsync 全部夹具文件及父目录，准备耗时单独记录、位于计时之外。目录中没有内容改写时，记录数、clone 数不随文件数增长。另测改名后修改 `moved/f0000`：恢复日志必须先恢复文件，再把目录改回原名；跨类别目录改名核对普通文件 clone 数。见 [directories.json](results/v3/directories.json)、[directory-order.json](results/v3/directory-order.json)。

### 5. 前态合并落盘与密集负载

节点并发处理至多八个通知。唯一账本互斥锁管理 clone 与记录，200 µs 收集窗口形成提交代号。多前态批次由常驻刷盘线程并发 `fsync` 不同 inode，再同步一次账本及状态目录，最后发布该代号完成；每个执行许可在自己的代号完成之后发送。空批次不刷盘。单个串行调用不能为了凑批次而提前执行写入。

八个独立子目录并发首次写入，共十次，原始 fsync 起止、提交完成与执行许可时间见 [grouped.json](results/v3/grouped.json)。这是系统调用完成顺序证据；没有 `perf` 或内核块层跟踪，不能宣称直接测到了 XFS 的物理日志提交次数，也未模拟断电。

LATTICE 副本只有 485 个跟踪文件。本轮构造两个真实 Git 提交，在 64 个子目录中改变 4,000 个常规文本文件，使用普通 bench 身份提交，未调用 bot-run。两提交 SHA、全部路径和来源见 [checkout-commits.json](results/v3/checkout-commits.json)。这是 LATTICE 仓库上的受控合成差异，不是声称找到两个相差几千源码文件的上游历史提交。

四配置均执行同一个默认串行 `git checkout --quiet --detach`。前态密集数字见性能表；节点的 batch 统计用于辨认此负载实际有没有并发前态。该项尚无 ADR 数值目标，报告只提供测量，不擅自补阈值。

### 6. 常驻 worker 与上下文

每个执行环境只有一个 worker 进程，在启动时进入一次 user/mount namespace，保持宿主 PID namespace。其八个线程各自 `unshare(CLONE_FS)`；每个请求重设目标 root、cwd、dirfd、umask 与有效 capabilities。它持有的宿主 proc fd 只用于读取目标上下文，不交给子进程。见 [worker-visibility.json](results/v3/worker-visibility.json)。

上下文对照覆盖 cwd、相对 dirfd、chroot、umask、capability 权限正反例、namespace tmpfs 权限正反例、五类 `openat2.resolve`、O_NOFOLLOW、O_EXCL 与 CLOEXEC；另测扩展 `open_how`、未知高位 flag、IN_ROOT 悬空链接创建。第二轮的两个 tmpfs 探针在本轮均先于写调用因 `mount` 返回 `EPERM` 而结束，不能据此声称 worker 的 tmpfs 权限测试通过；其余 24 对指定上下文与 ns 相同。连续上下文用例让两个进程以不同 cwd/umask 各写入 20 轮，并核对权限及写入的实际 cwd。另有每配置 1,000 次无效 dirfd 与有效打开交替测试，见 [invalid-dirfd.json](results/v3/invalid-dirfd.json)。见 [coverage.json](results/v3/coverage.json)、[context-edges.json](results/v3/context-edges.json)、[context-alternating.json](results/v3/context-alternating.json)。

这些是指定上下文的对照，不是对任意并发 chdir/chroot、fd close/reuse、凭据更改和动态挂载的完整 VFS 证明。继承第二轮的 syscall 代理边界；原型不作为通用恶意代码沙箱交付。

### 7. 节点死亡与最小恢复

50 次对真实节点发送 `SIGKILL`。探针先经过滤器登记只打开不写的 `a` 和可写 `b`，另取得允许层级 `target/a` 的 fd；在 `race-safe` 前态之后暂停，再杀节点。控制页在可能的死亡窗口直接记录新打开的 errno 与旧 fd 写结果，避免 stdout 缓冲在被杀后丢失证据。

每轮检查 init、launcher、worker 是否仍在运行；新恢复进程只扫描账本登记集，当前内容仍等于前态的路径不留残余；没有可靠后态且内容变更的路径保留现状并标为 `Indeterminate`。允许层级写入不混入普通对象的回滚。见 [crashes.json](results/v3/crashes.json)。

进程在返回 syscall 结果之前被终止的样本记为“未观察到返回”，不能伪造为一次 ENOSYS。该机制仍不保证节点死亡瞬间撤销旧 fd，也没有验证断电、磁盘故障、任意停机期间外部写或完整节点 `hello` 生命周期。

## 性能

<!-- PERFORMANCE:START -->
三次成功运行的中位数，单位秒。所有样本及范围见 [performance.csv](results/v3/performance.csv)、[summary.json](results/v3/summary.json)。

| 工作负载 | 无沙箱 | 仅命名空间 | 第二轮 B2v2 | 本轮 B3 |
| --- | ---: | ---: | ---: | ---: |
| LATTICE 全量构建 | 56.404813 | 54.705477 | 59.055121 | 54.852606 |
| 4,000 文件 checkout | 0.258667 | 0.275973 | 12.375601 | 12.748429 |

全量构建 B3 / ns 的中位数比值开销为 **+0.27%**，3% 指标**达标**。

| 20,000 次 open+close | 单次 µs | ≤30 µs |
| --- | ---: | --- |
| 无沙箱 | 3.391 | 对照 |
| 仅命名空间 | 3.406 | 对照 |
| B2v2 代执行 | 393.081 | 对照 |
| B3 CONTINUE | 29.097 | 达标 |
| B3 代执行 | 167.567 | 未达标 |

不建检查点微基准均打开 `/dev/null`；B3 代执行使用 `WI_FORCE_EMULATE=1` 关闭快路，保留完整 worker 上下文复制和 ADDFD。两种路径均核对 `registered=0`。

| 同类别目录改名 / 夹具 | 次数 | syscall 中位数 ms | 结构记录 / clone |
| --- | ---: | ---: | --- |
| 1,000 文件 / 已刷盘 | 5 | 2.000 | 1 / 0 |
| 10,000 文件 / 已刷盘 | 5 | 2.235 | 1 / 0 |
| 1,000 文件 / 未刷盘 | 5 | 7.942 | 1 / 0 |
| 10,000 文件 / 未刷盘 | 5 | 49.644 | 1 / 0 |

结构记录的算法没有遍历子树。未刷盘夹具的墙钟时间随规模增长，预刷盘后主要增长消失；这支持前次创建的持久化负担影响本次 fsync 的解释，但没有内核日志跟踪。原文没有指定输入持久化状态，本轮不据干净夹具宣称无条件墙钟成本达标。

独立通知下限（同步唤醒，三次中位数，µs）：native 3.41；cont 10.40；cont-read 12.30；addfd-read 23.18。见 [notification-floor.json](results/v3/notification-floor.json)。

图见 [performance.html](results/v3/performance.html)。
<!-- PERFORMANCE:END -->

构建使用自己的 reflink 副本、Rust 1.96.0、四个编译作业、debug=0、`CARGO_INCREMENTAL=0`，每轮清空 target。所有配置均为 `cargo build --offline --workspace`。计时取节点 `stats.wall_s`，包含启动、通知服务和退出，不含 Python oracle 的后态摘要；`process_wall_s` 和 `digest_s` 也保留。微基准取探针内部 20,000 次 open+close 的耗时，不把一次性启动摊成 syscall 成本。

初版 B3 的分类使用逐段 `realpath` 且每次 CONTINUE 经处理线程，最终改成内核 `O_PATH` 解析并在接收线程回复；有变化的实现重新测量。初版原始数字保留在 [builds-before-optimization.json](results/v3/builds-before-optimization.json)、[checkouts-before-optimization.json.gz](results/v3/checkouts-before-optimization.json.gz)、[micro-before-vmread.json](results/v3/micro-before-vmread.json)，不能与最终样本混成中位数。最终构建重跑 ns/B3 成对样本，保留实现未变的 none/B2v2 样本；最终 B3 checkout 也重跑三次。[final-benchmarks-v3.py](final-benchmarks-v3.py) 记录这一过程。通知下限源码来自任务提供路径，但数字全部由本轮重新运行；不含命名空间上下文、检查点或完整策略，不能替代集成路径指标。

初版基线期间有少量本轮编译与冒烟检查；最终 ns/B3 成对重测、微基准与最终目录对照均独立顺序运行。共享主机、未清页缓存，每配置三次。表中阈值判定是这些样本的中位数判断，不等于统计置信区间保证；离群样本不删除，不能把失败构建的时间放进有效中位数。

## 偏离、未完成与待维护者裁决

| 项目 | 事实或待决定事项 |
| --- | --- |
| Landlock 的业务类别 | 两个均可写类别之间的参数改写不被拒绝；建议 link 与 rename 一律代执行，或单独研究分层规则 |
| 硬链接风险 | 预建跨类别别名仍可在 CONTINUE 竞态下绕过普通对象账本；现文已列为风险，是否接受由维护者决定 |
| 锚点初版 | `EXDEV` 使操作未发生，早期“拓扑相等”判定是假阳性；保留失败数据并修正实现和 oracle，没有计入通过次数 |
| mkdir 参数探针初版 | 错换到已存在文件导致 EEXIST，不能证明 Landlock 拒绝；改用不存在的常规路径，保留 [landlock-races-initial.json](results/v3/landlock-races-initial.json) |
| 常驻 worker 错误路径 | 并发构建暴露：复制远程 dirfd 失败时，清理误关了 worker 的同号 fd，造成 IPC 退出 126；改为只关闭实际取得的 fd，并以无效 dirfd 交替探针与缺失 target 全量构建回归。失败记录见 [missing-target-worker-failure.json](results/v3/missing-target-worker-failure.json) |
| 跨类目录恢复顺序 | 初版逆序消费者在恢复父目录之前先恢复文件，触发 ENOENT；根据已登记的目录前态补齐祖先后通过。初版见 [directory-order-initial.json](results/v3/directory-order-initial.json) |
| 动态 mount | Landlock 使第二轮两个 tmpfs 探针在 mount 返回 EPERM；需要决定是否支持沙箱进程动态挂载，以及如何在相同边界下验证该类权限上下文 |
| 只读父目录的恢复 | 初版 oracle 没有临时增加父目录拥有者写权限，`perm-dir` 恢复失败；修复恢复实现后重跑，保留 [coverage-initial.json](results/v3/coverage-initial.json) |
| 密集提交来源 | 4,000 文件差异为副本中的受控合成提交；是否再提供真实上游大变更历史，由维护者决定 |
| 目录改名性能口径 | 结构记录数为常数，但未刷盘输入的墙钟成本随文件数增长；建议明确验收是节点增量算法成本，还是包含先前脏数据持久化的端到端墙钟成本。两种夹具数据都保留，不擅自修改验收条件 |
| 单调用性能 | 完整代执行仍需复制进程上下文、两阶段 worker IPC 与 ADDFD。建议研究 worker 直接接收通知、无前态路径单阶段执行等方案后再验收；未通过修改阈值或用通知下限冒充完整路径达标 |
| 同步持久化目标 | 不把用户态 batch 数冒充物理日志提交次数；没有电源故障、磁盘故障或文件系统崩溃测试 |
| 完整边界 | 沿用第二轮的单 uid、x86-64 原型；未实现所有 mount/外来 fd/ioctl/fcntl 入口的通用隔离，也未穷尽 VFS 组合标志与凭据竞态 |
| 产品与权限 | 未修改 ADR、未接产品代码、未提交推送或执行 GitHub 写操作；只在当前 worktree 写文件，工作负载提交只存在于自己的副本 |

## 复现与交付检查

在本目录执行。工作负载和 Cargo 缓存先从任务指定副本以 `cp --reflink=always` 复制到自己的 `.scratch/`；不要指向开发仓库或原 worktree。重跑前另存第三轮结果，避免覆盖这次证据。

```bash
make v3 .scratch/notification-floor-v3
python3 -m py_compile *-v3.py
python3 races-v3.py landlock internal external anchors crashes directory_lock
python3 context-v3.py
python3 context-edge-v3.py
python3 timings-v3.py
python3 floor-v3.py
python3 benchmarks-v3.py full checkout missing_target
python3 summarize-v3.py
python3 verify-v3.py
python3 docs-check-v3.py
```

大体积检出账本以 gzip 无损保存，解压字节哈希见 [compression.json](results/v3/compression.json)，可用 `gzip -dc results/v3/checkouts.json.gz` 读取。

最终数据一致性、旧文件哈希、文档检查和残留进程结果见 [verification.json](results/v3/verification.json) 与 [docs-check-temporary-index.json](results/v3/docs-check-temporary-index.json)。文档检查使用本 worktree 内的临时索引，不改变真实索引。没有运行产品全仓 CI：本轮只修改原型。父 Agent 核对证据后负责提交与 ADR 裁决。

相对当前 HEAD 的 `git diff --check` 会检查整批检入的旧原型，因前两轮原始日志的尾空格与 CSV 的 CRLF 返回 2；保留这些受保护文件原样。相对 `origin/exp/write-intercept` 的本轮已跟踪改动检查另行执行，原始输出见 [diff-check-imported.log](results/v3/diff-check-imported.log) 与 [diff-check-v3.log](results/v3/diff-check-v3.log)。
