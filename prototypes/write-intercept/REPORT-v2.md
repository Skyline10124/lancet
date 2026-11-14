---
status: draft
---

# 写调用代执行第二轮实验

**结论：ADR 0002 第 6 节暂不具备接受条件。** B2 的稳定 fd 方案抵御了本轮参数改写和打开后路径替换，复制子进程上下文也能修复第一轮的 umask 与权限偏差。但本轮得到三个可重复反例：目录项事后核对不能辨认所有替换；节点退出与 PID 1 死亡之间仍能使用旧 fd 写入；删除已有硬链接别名后的最小恢复会拆散链接关系。不能把这些保证标为“已核实”。本报告只提供证据与建议，不修改 ADR。

## 实验对象与口径

- ADR 基线：`30c2c23539de9e617de8d03c3746f83178db9804`；第一轮：`05180709fda2a0cdaf2d226ad540470064050943`（`origin/exp/write-intercept`）。按任务要求 checkout 第一轮原型，保留其 [REPORT.md](REPORT.md) 与全部已有结果。
- 新实现：[intercept-v2.c](intercept-v2.c) 复用未修改的 [intercept.c](intercept.c)；新增 `B2v2`。`none`、`ns`、`B2` 仍执行第一轮实现。所有变更都在本 worktree，未接入产品、未修改 ADR、未提交、未推送。
- 主机：Rocky Linux 10.2、Linux 6.12、XFS；SELinux Enforcing、`deny_ptrace=off`、Yama 0。详细命令输出与源文件 SHA-256 见 [environment.json](results/v2/environment.json)。没有 root/sudo，也没有安装软件包。
- 通过 `cp -a --reflink=always` 从第一轮 worktree 的 `.scratch/lattice` 与 `.scratch/cargo-home` 复制工作负载。原副本只读；不访问或修改三个 `~/Dev` 工作区。实验输出、缓存、编译临时文件及检查点都在本 worktree。
- 确定性竞态通过共享内存握手固定窗口。本报告的分子/分母是这一调度下的实测次数，不代表自然攻击概率。外置 oracle 可扫描夹具；被测节点只登记写入口涉及的对象及 rename 子树，不扫描整个工作区。
- 实验程序受控，原型没有实现第 4 节的只读挂载投影、已存在 hidden 的遮蔽、出网代理或 cgroup。它不是可供运行任意不可信程序的安全沙箱。

## 九条规则的结论

<!-- RULES:START -->
| 规则 | 结论 | 用例与次数 | 原始数据（results/v2/） |
| --- | --- | --- | --- |
| 1 对象绑定 | **通过** | 参数改写 0/600 绕过；沙箱内/外路径替换各 20 次，evil 均未改 | [parameter-races.json](results/v2/parameter-races.json)、[replacements.json](results/v2/replacements.json) |
| 2 目录项 inode 核对 | **未通过** | 380 次目录项替换；unlink/rmdir remove-old、rename 目标端、symlink ABA 均存在 0/20 检出组 | [replacements.json](results/v2/replacements.json)、[supplement.json](results/v2/supplement.json) |
| 3 硬链接 | **部分通过** | 三种跨类新建 EXDEV；三种已有别名写入原 inode 还原；删除已有别名的恢复仍破坏拓扑，改名恢复已修复 | [coverage.json](results/v2/coverage.json)、[supplement.json](results/v2/supplement.json)、[benchmarks.json](results/v2/benchmarks.json) |
| 4 账本与还原 | **部分通过** | 112 个 B2v2 覆盖运行及最小还原无语义差异；额外硬链接目录项反例；40 次取消仅保留证据、不自动恢复 | [coverage.json](results/v2/coverage.json)、[supplement.json](results/v2/supplement.json)、[cancellations.json](results/v2/cancellations.json) |
| 5 hidden | **通过** | 15 种入口各 1 次，均 EACCES，目的对象未建 | [coverage.json](results/v2/coverage.json) |
| 6 fd 卫生 | **通过** | 3 种 inherited 探针均 EBADF，原文件未改；启动 socket 在 exec 前关闭 | [coverage.json](results/v2/coverage.json) |
| 7 失效联动 | **未通过** | 直接 PDEATHSIG 链仍 49/50 次旧 fd 写入；代执行后取消 40/40 进入 Indeterminate | [crashes.json](results/v2/crashes.json)、[cancellations.json](results/v2/cancellations.json) |
| 8 子进程上下文 | **部分通过** | cwd/root/dirfd、umask、五种 resolve 与权限正反例符合 ns；未完整实现并发 fs/凭据快照及全部 openat2/VFS 边缘 | [coverage.json](results/v2/coverage.json) |
| 9 开销 | **通过** | 四配置各 3 轮真实工作负载；20,000 次 open/close；1,000 文件目录 rename 及批量落盘各 3 次 | [benchmarks.json](results/v2/benchmarks.json)、[timings.json](results/v2/timings.json) |

<!-- RULES:END -->

## 实现与证据

### 1. 对象绑定

节点从通知复制路径、标志和 `open_how`，之后不再读取这些参数。受控 worker 在目标 user/mount namespace 中，使用目标 root、cwd、复制的 dirfd、umask 和有效 capabilities 原生解析路径。已有文件先取得不带 `O_TRUNC` 的 fd，将这个 fd 经 SCM_RIGHTS 交给节点；节点按 fd 的实际路径分类、记 dev/inode、从稳定对象 FICLONE 并持久化，再让 worker 对同一对象执行截断，最后以 `ADDFD_FLAG_SEND` 注入 fd。父目录 fd 固定新建和目录项操作的父目录。

参数内存竞态为 3×200 次。路径替换为沙箱外写者与沙箱内第二个进程各 20 次；内部进程的 rename/symlink 也经同一通知循环代执行，没有绕过过滤器。等待握手时节点继续处理其它通知，避免将内部替换变成不可达的测试。记录均包含原 inode、后态、通知计数与握手次数。

这里证明的是取得 fd 后的对象稳定性，不是检查点与任意并发写入的事务一致性。另一进程已有同 inode 可写 fd 时，FICLONE 与随后的直接写入仍可互相交错；本轮没有宣称封住此类数据竞争。通过稳定对象写入也不会让外部 rename 自动变成可逆的目录项事件。

### 2. 目录项核对的反例

实现对 rename/link 的目的对象核对 dev/inode；`RENAME_EXCHANGE` 核对两端。unlink/rmdir 已无可查询的路径对象，只能检查路径消失和保留 fd 的链接数变化；symlink 检查新链接类型和文字。这两种后者是可实施的见证，**不是 ADR 所要求的完整 inode 身份保证**。

本轮分别执行 `rename`/`renameat`/`renameat2`、`unlink`/`unlinkat`、`rmdir`、`link`/`linkat`。每种有两组各 20 次：检查后把原对象移到 spare，再放入替代物；或检查后先删除原对象，再放入替代物。第二种攻击使原 fd 的 nlink 本身已经变成预期值，unlink/rmdir 的检查因此漏过。检测到时写出 `Indeterminate`、返回原型专用退出码 120 并 SIGKILL PID 1；漏过时仍正常返回，原始数据完整保留。

另有两个独立缺口：

- **rename 目标端替换**：检查点记录原目的文件之后，外部将它移走并放入另一个文件。rename 的结果仍是预期的源 inode，仅核对结果目的 inode 看不到刚刚覆盖了不同对象。
- **symlink ABA**：在代执行成功与后核对之间，将刚创建的链接移走，再放入同文字的不同 inode 链接。创建调用不返回所创建 inode 的引用，核对文字无法区分两个 inode。本实验保留被移走链接，保证替代 inode 确实不同。

以上均来自原型中的明确算法及人工窗口，不构成“所有实现均不可能”的证明。但它们足以否定仅写“操作后核对 inode”就已具备完整保证。可选方案是约束目录项的所有写者，或采用能原子返回/验证被操作对象身份的内核机制。暂存目录、交换名字等用户态改写会增加中间状态与新的竞态，需要维护者另行裁决，不能作为已完成修复。

检测到替换时，本原型保留检查点与现状、杀掉环境、结算 `Indeterminate`，**没有自动覆盖恢复这些冲突路径**。ADR 的“按检查点原地还原已登记的项”不能在未确定哪个对象被操作时直接套用；应决定哪些无冲突项可恢复，哪些证据必须保留。

逐组次数与检测率见 [replacement-rates.md](results/v2/replacement-rates.md)。本轮 380 次目录项替换中，可见源 inode 替换 260/260 检出；上述不可辨认组共 120/120 漏检。这是所选构造的分组汇总，不是现实攻击成功率。

### 3. 硬链接与恢复

跨常规、`target/`、`.git/` 分类的新建硬链接返回 `EXDEV`；同类别允许。已有 `target/alias` 或 `.git/alias` 的 nlink 大于 1 时，即使通过豁免路径打开也建立前态；普通 `hard/a` 别名同样登记。写入和写回都对原 inode 进行。覆盖测试核对完整内容及 inode 相等，而不是仅核对 clone 数量。

最小恢复器根据账本保存的 dev/inode，在已登记的后态路径中寻找可复用对象；不全树搜寻别名。`hardlink-alias`、`hardlink-normal`、`git-hardlink` 的**写入**均能恢复内容且保持所有原 inode。额外 `unlink-hardlink` 暴露局限：删除后没有已登记后态路径可取得原 inode，重建 `hard` 后内容正确但变成另一个 inode。`rename-hardlink` 最终已修复：先从已登记的目的端保留 fd，再以 proc fd 和 `linkat(AT_SYMLINK_FOLLOW)` 重建原名字，保留同一个仍有别名的 inode。详见 [supplement.json](results/v2/supplement.json)，初次两项都失败的数据保留于 [supplement-initial.json](results/v2/supplement-initial.json)。

建议运行期保留原 inode 的 fd，并在还有链接时用它重建名字；不能把 fd 当作重启后可用的持久状态。全部链接已删除、节点重启或需要还原未登记别名时，须选择持久的原 inode 锚点、完整别名拓扑账本，或收窄求逆保证。这是状态归属与公共契约的裁决，本原型没有偷偷增加另一套全树状态。

最终二进制的三轮 Cargo 全构建各有 49/49 次 link 成功，增量构建各 5/5，测试子集没有 link 调用；这些计数来自 `link_calls` / `link_success`。跨类别策略没有误拦这些构建内部链接。

### 4. 账本范围、未变更过滤与最小求逆

每条账本记录存在性、类型、mode、dev/inode/nlink，符号链接记录文字；常规内容另存 FICLONE。拦截 mkdir/mknod/chmod 族，含本机 `fchmodat2`。新目录及 FIFO 登记 `Absent`。六种 set/removexattr 调用统一 `ENOTSUP`。已存在 FIFO 只保存类型与权限，不保存其流中数据。

退出后的后态摘要只遍历登记集。`open-idle` 虽登记前态，内容、存在性、类型与权限均未变，不进入变更集；失败的 `O_EXCL` 在原生打开阶段即返回 `EEXIST`，没有登记，也没有变更。变更集不以“发生过一次通知”或“建立过 clone”为依据。

[experiment-v2.py](experiment-v2.py) 中的 `restore` 是离线最小求逆消费者，不是崩溃恢复服务：先核对已记录后态，再从深到浅删除新对象，从浅到深重建前态目录、文件、链接和权限。只处理登记集，运行在受控、无并发修改的恢复窗口中。文件写回保留还存在于该路径的 inode。恢复期间按需暂时增加宿主拥有者的写权限，完成后恢复原权限，以支持子进程凭 namespace capability 修改的只读对象。

覆盖 oracle 同时比较内容、存在性、类型、mode，并单独断言已有硬链接的 inode；时间戳、owner 不在相等性口径内。另验证：在摘要后修改已登记 `a` 会产生 Conflict 且保持外部内容；改动未登记 `outside` 不会被恢复器扫入。硬链接目录项恢复失败、运行中外部写与恢复器检查/执行之间的竞态仍未解决。

### 5. hidden 与 fd 卫生

测试根 `hidden/` 下的 open/openat/openat2/creat、mkdir/mkdirat、mknod/mknodat、symlink/symlinkat、link/linkat，以及 rename/renameat/renameat2，均应为 `EACCES`，目的对象保持不存在。策略仅为固定前缀，不实现 `.gitignore` 匹配器；现有 hidden 遮蔽属于第 4 节，未在本轮宣称实现。

runner 给三个标准流提供管道。启动垫片把唯一受控启动 socket 放到 fd 3，先 `close_range(4, UINT_MAX)` 清掉全部其它继承 fd，安装过滤器并发送 listener，再关闭 listener 与 fd 3 后 exec。这个临时控制 fd 是发送 listener 所必需，不能把“安装前关闭全部非标准 fd”解释成连通道也关闭。三个 inherited 探针分别对预先传入的 fd 做 ftruncate、fallocate、MAP_SHARED 写，结果均为 `EBADF`，文件未改。运行期 SCM_RIGHTS、pidfd_getfd 等外来 fd 来源的约束不在此启动测试的保证内。

### 6. 节点死亡与回复窗口

最终实现让 PID 1 通过 `clone(CLONE_PARENT)` 成为节点的直接子进程，设置 `PR_SET_PDEATHSIG(SIGKILL)`，并用父 pidfd 检查设置之前的死亡竞态。namespace 启动仍由 user/mount/pid/net 垫片完成。启动消息有 pipe 握手固定顺序。先前的两跳父子链实验也保留在 [crashes-two-hop.json](results/v2/crashes-two-hop.json)，最终结论使用 [crashes.json](results/v2/crashes.json)。

SIGKILL 探针先**经过过滤器**打开 `b` 得到合法可写 fd，再停在 `race-safe` 的检查点后。外部驱动对真实节点 PID 发 SIGKILL；程序如果收到 listener 消失导致的 `ENOSYS`，立即通过旧 fd 写 `b`。第一轮 B2 为 50/50 写入；第二轮直接父子联动仍为 49/50 写入。节点与 PID 1 的退出不是同步撤销文件写权限，调度窗口在本机可重复。全部存活后代最终被终止；这与“节点死后不能再写”的字面保证不同。

另对“文件已截断但还没注入 fd”与“unlink 已完成但还没回复”各取消通知 20 次。子进程由其兄弟进程 SIGKILL 后，节点看到 `ADDFD` 或 `NOTIF_SEND` 失败，记录 `Indeterminate` 并终止 PID 1。40 次均检测到，前态可读，实际副作用仍存在。本原型不假装回复失败意味着调用没有执行，也不重放调用或删除证据。

维护者需选择：把规则改成有限的“节点退出后最终终止整个环境，崩溃窗口按账本进入 Indeterminate”，还是增加能在数据面撤销写权限的机制。仅增加用户态看门狗、缩短父子链或改为 pidfd 通知不足以证明严格的零窗口保证；本轮没有实现内核级写权限撤销。

### 7. 子进程上下文

worker 每通知 fork 一次，复制 `/proc/<tid>/status` 的 umask/CapEff，复制 cwd/root/dirfd，再进入目标 user/mount namespace、chroot/fchdir、设置有效 capabilities。通过目标 `/proc/<namespace-pid>` 处理 `/proc/self` 魔术路径；symlink 的目标文字保持原文，不作路径重写。native `openat2` 保留五个 resolve 标志，已有对象与新建 `RESOLVE_IN_ROOT` 各有用例。

对照组 `ns` 让子进程直接执行 syscall。覆盖正反例：BENEATH 正常及 `..` 越界、IN_ROOT 绝对路径及创建、NO_SYMLINKS 普通路径与符号链接/悬空链接、NO_MAGICLINKS 普通路径与 proc 魔术路径、NO_XDEV 同挂载与 `/dev/null` 跨挂载；同时覆盖 O_NOFOLLOW、O_EXCL、O_TMPFILE 两种发布方式、FD_CLOEXEC、chdir、相对 dirfd、chroot 与 umask 0077。

只读文件和无写权限目录，在本机映射到用户的 user namespace 中，带 CAP_DAC_OVERRIDE 的子进程直接调用确实成功；清掉 CAP_DAC_OVERRIDE/CAP_DAC_READ_SEARCH 后为 `EACCES`。B2v2 四种结果一致。namespace 自有 tmpfs 上同样一正一负，结果一致。不能按“宿主 uid 相同”推断权限一定相同。

仍是**部分通过**：这组上下文读取不是多个线程共享 fs/凭据的原子快照；任意动态 mount、dirfd close/reuse、并发 chroot/chdir 没有全交错验证。创建路径的拆分解析仍需完整 VFS 审计，未覆盖所有末尾斜杠和组合标志情形。额外补测发现并修复零填充的 32 字节 `open_how`、未知高位 flags 和 `IN_ROOT` 下悬空链接创建；修复前后分别见 [context-edges-initial.json](results/v2/context-edges-initial.json) 与 [context-edges.json](results/v2/context-edges.json)，最终三组与原生结果一致。现有文件对象绑定与这些命名/上下文边界应分开验收。

## 性能

<!-- PERFORMANCE:START -->
三次有效运行的中位数，单位秒；范围与每个样本见 [performance.csv](results/v2/performance.csv)。

| 工作负载 | 无沙箱 | 仅命名空间 | 第一轮 B2 | 第二轮 B2 |
| --- | ---: | ---: | ---: | ---: |
| build-full | 54.892550 | 54.621773 | 56.088630 | 58.877582 |
| build-incremental | 7.787009 | 7.552575 | 7.516150 | 8.045646 |
| test-subset | 4.076246 | 4.134175 | 4.104978 | 4.356370 |
| git-status | 0.025245 | 0.026280 | 0.006819 | 0.007184 |
| git-checkout | 0.003615 | 0.004624 | 0.006394 | 0.014999 |
| git-commit | 0.027353 | 0.028538 | 0.010980 | 0.026314 |
| micro | 0.069458 | 0.070571 | 1.216720 | 7.785950 |
| rename-large | 0.000813 | 0.002000 | 2.963279 | 2.972359 |

B2v2 批量持久化的目录 rename：1.278415 s（n=3）。图见 [performance.html](results/v2/performance.html)，原始工作负载见 [benchmarks.json](results/v2/benchmarks.json)，微基准见 [timings.json](results/v2/timings.json)。

<!-- PERFORMANCE:END -->

LATTICE 使用独立 reflink 副本、第一轮缓存副本、Rust 1.96.0，4 个编译作业，debug=0、`CARGO_INCREMENTAL=0`。full 每次删除副本 target 后执行 `cargo build --offline --workspace`；incremental 在 net-guard 源码追加一行注释后重建；test 为 `cargo test --offline -p net-guard`。Git commit 在副本执行 `git -c user.name=bench -c user.email=bench@example.invalid -c commit.gpgsign=false commit --allow-empty ...`，成功产生真实空提交；不调用 bot-run。各配置 3 次；最初轮换顺序，修复 openat2 边缘用例后又在末尾顺序重跑最终 B2v2 三组，保留其它未变基线。前一版 B2v2 样本见 [benchmarks-before-final.json](results/v2/benchmarks-before-final.json)，最终二进制及 C 源文件哈希见 [final-binary.json](results/v2/final-binary.json)。源码改动准备与删除 target 不计入计时。

微基准 20,000 次 open+close，目录基准含 1,000 个常规文件。`WI_BATCH=1` 只把该 syscall 内的账本与目录 fsync 合并到全部 clone 持久化之后、释放 worker 之前；各 clone 仍 fsync，没有把安全前置落盘推迟到写调用之后。该选项不改变目录遍历的 O(n) 成本。

表中的墙钟取节点 `stats.wall_s` 的中位数；另存 runner `process_wall_s`、后态摘要 `digest_s` 和探针内部 seconds。共享机器、未清页缓存、每配置只有 3 次；不能把小差异当作稳定排名。前段构建期间有本轮轻量覆盖/竞态探针，可能影响个别样本；失败的 full 构建不混入有效中位数。worker fork/IPC 的成本计入 B2v2，不等价于未来常驻 worker 的性能上限。

## 偏离、失败与未验证范围

| 项目 | 如实记录 |
| --- | --- |
| 初次覆盖 | 发现 O_EXCL 符号链接 errno、O_TMPFILE proc fd 发布、IN_ROOT 创建、chroot 中无 `/dev/null` 占位 fd、rename 恢复路径复用、只读权限还原等实现错误，修复后重跑全部矩阵；初次数据见 [coverage-initial.json](results/v2/coverage-initial.json) |
| 初次构建 | `aws-lc-sys` 创建末尾带 `/` 的 include 目录失败（ENOENT）；[benchmark-blocker.json](results/v2/benchmark-blocker.json)、[debug-build-errors.jsonl](results/v2/debug-build-errors.jsonl) 保留错误；修复 mkdir 解析后重跑，不拿失败耗时当性能 |
| 重跑夹具 | 只读目录使旧 `shutil.rmtree(ignore_errors=True)` 未完全清理；已改为仅对本轮夹具恢复目录可写后删除；进度日志保留正式运行，报告保留此偏离 |
| 生命周期迭代 | 先测两跳 PDEATHSIG 链，再用直接父子链复核；均未实现严格零窗口。最终 9 条规则以直接父子链为准 |
| 求逆边界 | 只有退出后、无并发窗口的最小恢复；硬链接目录项的拓扑恢复失败。不实现崩溃重启、掉电、磁盘故障、回执保留期、跨调用求逆 |
| 目录项核对 | unlink/rmdir 用 nlink 见证，symlink 用文字见证；明确弱于 ADR 的 inode 保证。没有通过更改验收标准把负例判成通过 |
| 完整安全边界 | 不限制 mount/chroot/外来 fd、所有 ioctl、fcntl、SCM_RIGHTS 等入口；不验证任意恶意工作负载。单 x86-64 ABI，非完整内核 syscall 代理 |
| 上下文范围 | 映射单宿主 uid，当前 SELinux/Yama；未测试多用户、其他 LSM、并发共享 fs/凭据的所有交错和全部扩展 open_how/组合 flags |
| 性能范围 | 完整 workspace build、net-guard 测试子集；未跑 LATTICE 全套测试。没有安装 perf/strace，也不声称采集了 CPU profile |

## 复现与交付验证

在当前 worktree 的本目录运行：

```bash
make v2
python3 -m py_compile *-v2.py
python3 experiment-v2.py coverage
python3 races-v2.py
python3 supplement-v2.py
python3 context-edge-v2.py
python3 benchmarks-v2.py
python3 timings-v2.py
python3 summarize-v2.py
python3 verify-v2.py
python3 docs-check-v2.py
```

工作负载副本需预先按任务要求 reflink 复制；`benchmarks-v2.py` 会依据 `results/v2/benchmarks.json` 跳过已完成的整组，失败组重跑。要做完全新一轮，应保留旧结果另选实验目录与分支前缀，不直接覆盖已有证据。第一轮脚本仍写 `results/`，本轮不要运行它们。

源文件、驱动和结果索引见 [README.md](README.md)。最终一致性校验、旧报告与旧结果是否未变、文档链接与残留进程检查见 [verification.json](results/v2/verification.json)。未运行全仓 CI：改动不接入任何产品代码。父 Agent 核对实现与原始证据后负责审查、提交与 ADR 裁决。

常规 `cargo --offline --locked xtask docs-check` 因新增文件尚未进入 Git 索引而报告 11 个目标不存在，原始输出见 [docs-check.log](results/v2/docs-check.log)。最终由 [docs-check-v2.py](docs-check-v2.py) 使用 `.scratch/docs-index` 临时索引让相同检查器包含新增路径；不修改真实索引、不写 Git 对象、不提交，结果见 [docs-check-temporary-index.json](results/v2/docs-check-temporary-index.json)。`verify-v2.py` 另从文件系统检查交付相对链接。
