---
status: draft
---

# 子进程写拦截机制对比实验

## 结论

建议 ADR 0002 第 6 节把 **B2 作为继续验证的方向**，撤回“A 只停当前线程、检查路径后放行即可保证写前检查点”的确定性表述。**本次三个原型都没有满足完整要求，不能把 B2 直接标成可实施的安全边界。** 本报告是建议，没有修改 ADR 或产品合同。

决定性证据是受控的参数内存竞态：A、B1 各 600/600 次打开未登记的 `race-evil`；B2 为 0/600，全部打开复制参数所指的 `race-safe`。A 还独占 ptrace 调试位置。B2 保留调试能力，并能把检查点与实际操作绑定到节点持有的参数；这些差异比本轮基准的小幅耗时差异更有决策价值。

B2 仍需解决 inode/目录项稳定性、文件描述符来源、硬链接策略、元数据语义和失效联动。补测中，外部写者在检查后把 `race-safe` 替换成符号链接，三种机制均修改了未登记的 `race-evil`；B2 仅复制字符串仍不够。B2 也把目标 `umask(0077)` 下的创建结果从 0600 变成 0644。若 ADR 要求覆盖任意原生程序且不允许这些缺口，当前证据支持的结论是“尚无通过方案”，不是任选一个机制即可兑现承诺。

## 方法与口径

| 项目 | 本次方法 |
| --- | --- |
| 环境 | Rocky Linux 10.2；6.12.0-211.68.1+2.1.el10_2_ciq.x86_64；SELinux Enforcing、`deny_ptrace=off`；Yama 0；GCC 14.3.1；Rust 1.96.0 |
| 工作区 | `/home/gcp-vegas/.t3/worktrees/lancet/t3-cabb1e26/prototypes/write-intercept/`；环境、仓库 HEAD 和 FICLONE 验证见 [environment.json](results/environment.json) |
| 文件系统 | 副本、检查点、缓存均在该 worktree 的 XFS；实际 `ioctl(FICLONE)` 成功，修改源后 clone 保持原内容，设备号相同 |
| 启动 | C 启动垫片直接 `unshare` user/mount/PID/net，配置 uid/gid 映射、私有挂载和新 proc；PID 1 等待全部后代。未安装或提取 bubblewrap |
| A | `PTRACE_TRACEME` 启动、`TRACESECCOMP/FORK/VFORK/CLONE/EXEC`、`EXITKILL`；入口读寄存器与 `/proc/<tid>/mem`，拒绝时改 syscall 为 -1、返回值为 `-EACCES` |
| B1 | 启动垫片 `NEW_LISTENER` 安装过滤器，经 `SCM_RIGHTS` 交给宿主，关闭沙箱一侧监听 fd；节点回复 `CONTINUE` |
| B2 | 相同通知通道；复制字符串和 `open_how`，节点代执行 open/creat、unlink/rmdir、rename、truncate、link/symlink；open 用 `ADDFD | SEND` 原子注入并回复。没有对这些写调用回退 CONTINUE |
| 共同过滤器 | 写标志的 open/openat、全部 openat2、creat、unlink/unlinkat/rmdir、rename/renameat/renameat2、truncate、link/linkat、symlink/symlinkat；io_uring_setup 返回 ENOSYS；拒绝非 x86-64 及 x32 ABI |
| 检查点 | 常规文件单次 FICLONE，原本不存在则 Absent；clone、账本和目录在放行前 fsync。目录改名只枚举受影响子树，并登记源、目标文件路径 |
| 正确性判定 | 每个用例用独立夹具；运行前后 oracle 记录内容 SHA-256、类型、模式、时间、所有者、链接数、user xattr。把 clone 的 SHA-256 与运行前内容核对；非文件状态单独报告 |
| 写后摘要 | Python 在计时边界后仅遍历导出的登记集计算 SHA-256/Absent/符号链接文字，单列 `digest_s`；没有退出后整树扫描。oracle 的全树观测不属于节点算法 |
| 基础样本 | 78 个场景 × 3 机制 = 234 运行；[coverage.json](results/coverage.json) 是逐项结果，详细命令与输出在 [runs/](results/runs/)，前后 oracle 为 `*-oracle.json.gz` |
| 竞态样本 | 每机制 3 次 × 200 次写 open；调用方与改写线程用共享内存握手，在检查点后、实际系统调用前改写缓冲区；每次对返回 fd 做 fstat 确认实际 inode |
| 微基准 | 每配置 3 次，每次 20,000 次 `open(target/bench, O_WRONLY|O_CREAT)` 加 close；无检查点，用于测通知、解析、放行/注入的稳态成本 |
| 构建 | `git clone --local --no-hardlinks` 的 LATTICE 副本，HEAD 为 `034d3a10ec37aedeff4ae932ede40c4377036177`；独立 reflink Cargo registry；`cargo build --offline --workspace` 每次清空副本 target，然后修改 net-guard 的一个源文件再构建；测试为 `cargo test --offline -p net-guard`（4 个单测，0 doctest） |
| 构建控制 | 4 jobs、debug=0、`CARGO_INCREMENTAL=0`；“增量”指 Cargo 重用未变 crate 产物，不是 rustc incremental 模式；三轮轮换配置顺序，不清系统页缓存；所有构建串行 |
| 墙钟 | C runner 从启动到整棵进程树回收、关闭账本；含命名空间、过滤器、检查点，不含复制夹具/缓存、修改源的准备、oracle、写后摘要。原始结果另留外层 `process_wall_s` |
| 计数 | `calls` 是到达 tracer/listener 的次数；内核直接返回 ENOSYS 的 io_uring_setup 不计入。`registered` 含文件与 Absent；`clones` 只计成功 FICLONE |

`none` 是同一个 runner 直接 exec，无命名空间/过滤器；`ns` 仅命名空间。A/B 没有实现 ADR 的只读资源投影及 hidden 遮蔽挂载，因此这是机制比较，不是完整沙箱评估。小型进程树探针和补测曾与第一轮构建重叠；同机也观察到非本实验的 Cargo 进程，结果不具有独占主机的低噪声条件。3 次样本只用于粗略比较。

## 结果表

### 性能

<!-- PERFORMANCE:START -->
每格为中位数秒（相对无沙箱开销）；有效项各 3 次。失败项不提供性能估计。

| 工作负载 | 无沙箱 | 仅命名空间 | A | B1 | B2 |
| --- | --- | --- | --- | --- | --- |
| 全工作区冷构建 | 60.688995（+0.0%） | 61.826160（+1.9%） | 57.607504（-5.1%） | 61.901079（+2.0%） | 65.182623（+7.4%） |
| 单源改动后构建 | 8.792149（+0.0%） | 9.493565（+8.0%） | 7.634364（-13.2%） | 8.314270（-5.4%） | 8.555511（-2.7%） |
| net-guard 测试 | 4.681580（+0.0%） | 4.314119（-7.8%） | 4.323641（-7.6%） | 5.270910（+12.6%） | 4.776934（+2.0%） |
| git status | 0.024909（+0.0%） | 0.026625（+6.9%） | 0.026989（+8.4%） | 0.041816（+67.9%） | 0.035394（+42.1%） |
| git checkout -b | 0.003558（+0.0%） | 0.004728（+32.9%） | 0.006517（+83.1%） | 0.013643（+283.4%） | 0.007550（+112.2%） |
| git commit | 未完成（3 次失败） | 未完成（3 次失败） | 未完成（3 次失败） | 未完成（3 次失败） | 未完成（3 次失败） |
| 20,000 次 open+close | 0.069545（+0.0%） | 0.070925（+2.0%） | 1.205703（+1633.7%） | 1.172288（+1585.7%） | 1.241885（+1685.7%） |

下面每格为“拦截次数 / 登记路径 / FICLONE 数 / 写后摘要毫秒”的中位数。无沙箱与 ns 的三种计数均为 0；失败提交为 —。

| 工作负载 | A | B1 | B2 |
| --- | --- | --- | --- |
| 全工作区冷构建 | 11995 / 0 / 0 / 0.0003 | 11995 / 0 / 0 / 0.0002 | 11995 / 0 / 0 / 0.0003 |
| 单源改动后构建 | 258 / 0 / 0 / 0.0002 | 258 / 0 / 0 / 0.0003 | 262 / 0 / 0 / 0.0003 |
| net-guard 测试 | 736 / 0 / 0 / 0.0003 | 736 / 0 / 0 / 0.0003 | 736 / 0 / 0 / 0.0003 |
| git status | 1 / 0 / 0 / 0.0004 | 1 / 0 / 0 / 0.0003 | 1 / 0 / 0 / 0.0003 |
| git checkout -b | 24 / 0 / 0 / 0.0002 | 24 / 0 / 0 / 0.0003 | 24 / 0 / 0 / 0.0003 |
| git commit | — | — | — |
| 20,000 次 open+close | 20000 / 0 / 0 / 0.0003 | 20000 / 0 / 0 / 0.0002 | 20000 / 0 / 0 / 0.0002 |

微基准每次 open+close 的平均墙钟（取三轮中位数）为：none 3.48 µs、ns 3.55 µs、A 60.29 µs、B1 58.61 µs、B2 62.09 µs。含 close 和循环成本，不能解释为内核单个 syscall 的纯执行时间。

原始数据：[benchmarks.json](results/benchmarks.json)、[micro.json](results/micro.json)；机器可读汇总（含 min/max、相对 ns 开销）：[summary.csv](results/summary.csv)。
<!-- PERFORMANCE:END -->

构建、Git 和微基准主要写 `target/`、`.git/` 或副本外的环境缓存，正常情况下没有常规文件检查点。源文件改动是增量构建前的准备动作，不计入子进程登记集。故这些基准主要测“每次写入口仍需到用户态分类”的成本，不能代表大量源文件首次修改的持久化成本。

目录改名另有成本：移动 1,000 个常规文件时，A/B1/B2 各建立 1,000 个 clone、登记 2,000 条路径（源文件加目标 Absent），单次调用分别为 3.060/2.834/2.941 秒。这只是每机制 1 次覆盖样本，未作性能排名。它表明目录 rename 不再是接近 O(1) 的操作；硬链接和目录对象本身也未被该账本完整表达。

非空登记集的写后摘要样本如下；每格为单次毫秒，来自对应覆盖运行，不是三次性能中位数：

| 场景 | 登记 / clone | A | B1 | B2 |
| --- | --- | --- | --- | --- |
| 写一个现有 4 KiB 文件 | 1 / 1 | 0.1344 | 0.1101 | 0.1182 |
| 目录重命名，1,000 个文件 | 2,000 / 1,000 | 123.5578 | 123.0131 | 125.5445 |
| 外部写者改另一文件，沙箱改 a | 1 / 1 | 0.1401 | 0.1492 | 0.1473 |

### 文件内容与路径

下表“前态正确”指本用例受影响的常规文件内容已留存或 Absent 已登记；不意味着 inode、权限、链接拓扑及目录对象可完整恢复。234 个基础用例中，已建 clone 与 oracle 前态不一致为 0；这并不消除漏记。

| 用例 | A | B1 | B2 | 含义 |
| --- | --- | --- | --- | --- |
| open/openat/creat：WRONLY、RDWR、APPEND、TRUNC、CREAT/EXCL；仅 RDONLY|TRUNC | 前态正确 | 前态正确 | 前态正确 | Linux 的只读打开加 TRUNC 也会修改内容，已覆盖 |
| 只读 open | 0 通知、0 登记 | 同左 | 同左 | 不为只读路径建检查点 |
| openat2，resolve=0 | 前态正确 | 前态正确 | 前态正确 | open_how 在用户内存，BPF 无法只按写标志过滤 |
| openat2，RESOLVE_BENEATH | 成功、前态正确 | 同左 | EOPNOTSUPP | B2 原型明确不实现 resolve 语义；不是内核不支持 |
| O_TMPFILE 后 linkat 发布，proc fd 与 AT_EMPTY_PATH 两种 | 目标 Absent | 同左 | 同左 | 未命名阶段无路径；发布时登记目的地 |
| O_PATH 后 `/proc/self/fd/N` 写重开 | 前态正确 | 同左 | 同左 | 初始 O_PATH 不授写权，重开入口被拦 |
| unlink、unlinkat、truncate | 前态正确 | 同左 | 同左 | 普通文件内容可保留 |
| rename、renameat、RENAME_EXCHANGE | 两端前态正确 | 同左 | 同左 | 不能只记源；EXCHANGE 两侧都要登记 |
| RENAME_NOREPLACE 失败 | 登记 2、无实际内容变更 | 同左 | 同左 | 预登记可能过量，摘要后须排除未变路径 |
| O_CREAT|O_EXCL 失败 | 登记 1、无实际内容变更 | 同左 | 同左 | 不能把“登记”直接当作“改动” |
| 目录 rename：3 / 1,000 个文件 | 文件内容覆盖，目录对象未记录 | 同左 | 同左 | 枚举局部子树，未扫描整个工作区；空目录与目录元数据缺失 |
| link、linkat 新链接 | 目的地 Absent | 同左 | 同左 | 可删除新路径，但没有保存/还原完整硬链接关系 |
| symlink、symlinkat 新链接 | 目的地 Absent | 同左 | 同左 | 可辨识需删除的新路径；已有符号链接的前态另需账本类型 |
| 删除已有符号链接 | 漏记链接文字 | 同左 | 同左 | FICLONE 只支持常规文件内容，无法据此恢复链接 |
| 工作区内符号链接到现有常规文件 | 实际文件前态正确 | 同左 | 同左 | 本次解析器会跟随有效链接 |
| 悬空符号链接后 O_CREAT | 漏记被创建的实际目标 | 同左 | 同左 | 原型解析器缺口，见 [supplement.json](results/supplement.json)；需要正确处理不存在的最终目标 |
| `target/alias` 与常规 `a` 同 inode | a 改变，0 登记 | 同左 | 同左 | 机制共有的按路径豁免漏洞，不能只优化解析器解决 |
| 两个常规路径 hard/a 同 inode | 只登记 hard，漏 a | 同左 | 同左 | 即使克隆了同一份旧内容，原子替换某一路径会拆开链接关系 |
| 相对 dirfd、改变 cwd、proc/self/cwd、proc/self/root | 前态正确 | 同左 | 同左 | 单线程、稳定上下文；并发 chdir/dirfd 关闭复用未验证 |
| hidden 新 open、rename、link、symlink | EACCES | EACCES | EACCES | 目的地拒绝，0 检查点 |
| hidden 下 mkdir | 成功，漏管 | 同左 | 同左 | ADR 列表没有 mkdir，hidden 规则不能仅覆盖原列举的写入口 |
| target/ 与 .git/ 写 | 0 登记、0 clone | 同左 | 同左 | 仍经过用户态路径分类；硬链接反例见上 |
| 外部编辑器并发写另一个文件 | outside 不登记 | 同左 | 同左 | 每种机制各 1 次；退出后只有沙箱写过的 a 被摘要 |

### ADR 列表外的状态变化与求逆

| 状态变化 | 实测结果（A/B1/B2 相同） | 对原地还原与 revert 的影响 |
| --- | --- | --- |
| mkdir/mkdirat、rmdir/unlinkat(AT_REMOVEDIR) | 可创建/删除目录，账本没有目录对象；rmdir 已额外拦截仍未记录 | 空目录、目录权限和时间不能恢复；路径父目录重建也需显式规则 |
| chmod/fchmod/fchmodat | 模式改变，0 登记 | 内容摘要不变不能代表状态相等，执行位等不能还原 |
| chown/lchown/fchown/fchownat | 调用成功，0 登记；测试仅设置到当前 uid/gid，可观察 ctime | 未验证跨 uid/gid 所有权变更；若产品允许，须保存所有者及相关权限状态 |
| utimensat | 时间改变，0 登记 | 内容 hash 看不到时间变化 |
| setxattr/lsetxattr/fsetxattr | user.probe 属性改变，0 登记 | 内容 clone 不包含 xattr，无法恢复该变化 |
| mknod/mknodat（FIFO） | 成功，0 登记 | 特殊文件没有内容 clone 语义；设备节点未测 |
| ftruncate、fallocate(ZERO_RANGE)、MAP_SHARED 写，经受拦截 O_RDWR 获得 fd | 先建 a 的 clone，再修改成功 | 单靠 open 取得写权的前提在本用例成立 |
| 同样三项，fd 在安装过滤器前继承 | a 内容改变，0 登记 | 薄过滤器直接漏记；需收紧/登记初始 fd，处理 SCM_RIGHTS、pidfd_getfd 等 fd 来源 |

这些是账本语义和覆盖要求，不是 A 与 B 的性能取舍。`FICLONE` 只克隆内容块；原型 clone 的权限为 0600，没有保存原模式、owner、xattr、链接拓扑、稀疏布局等恢复信息。原型未执行实际 `revert`，本表给出缺失信息造成的可推导限制。

### 线程竞态、进程树与兼容性

| 实验 | A | B1 | B2 |
| --- | --- | --- | --- |
| 检查后改写 open(buf)，3×200 次 | 600 个 fd 指向未登记 evil | 600 个 fd 指向未登记 evil | 0 个指向 evil，600 个指向已登记 safe |
| 检查后外部替换路径为指向 evil 的链接，1 次 | evil 被改、未登记 | 同左 | 同左 |
| fork、vfork、pthread clone、非主线程 execve | 各自捕捉到 child 的 Absent | 同左 | 同左 |
| 孤儿孙进程、setsid 双 fork daemon | init 等待后代，child 登记 | 同左 | 同左 |
| 2 秒到期杀 PID namespace init | runner 124，后代回收 | 同左 | 同左 |
| 子进程 PTRACE_TRACEME / PTRACE_ATTACH | 均 EPERM | 均成功 | 均成功 |
| 再安装全 ALLOW seccomp 过滤器后写文件 | 成功、仍登记 | 同左 | 同左 |
| 再安装 USER_NOTIF/NEW_LISTENER，内层自行 CONTINUE hidden open | 绕过外层 TRACE，hidden 文件写成功 | 安装 EBUSY | 安装 EBUSY |
| PR_SET_DUMPABLE(0) 后写文件 | 本机仍成功登记 | 同左 | 同左 |
| io_uring_setup | ENOSYS | ENOSYS | ENOSYS |
| 目标 umask(0077) 创建 0666 文件 | 0600 | 0600 | **0644**（宿主 umask），语义错误 |

参数竞态来自可变用户内存。A 停住触发 syscall 的线程，没有停住修改缓冲区的另一个线程；B1 继续执行时内核也重新读取缓冲区。B2 不再使用后续用户内存，因此抵御了这个特定攻击。握手人为扩大并固定窗口，600/600 是“该调度下可重复绕过”，不是现实攻击频率。文件系统路径替换则发生在 copied-string 之后，必须用稳定的对象引用与目录操作方案另外处理。

嵌套 seccomp 的结果符合动作优先级：USER_NOTIF 高于 TRACE，故 A 不会收到这一调用；B 已存在 listener 时再 NEW_LISTENER 返回 EBUSY。一个 ALLOW filter 不会解除已有拦截。不能把本次 EBUSY 推广成“任意 seccomp 叠加都安全”；更高优先级的 ERRNO/TRAP 等及内层代理语义须整体审计。[内核 seccomp 文档](https://www.kernel.org/doc/html/latest/userspace-api/seccomp_filter.html)、[seccomp(2)](https://man7.org/linux/man-pages/man2/seccomp.2.html)。

构建与测试所需工具在 `io_uring_setup = ENOSYS` 下仍能运行，细节见基准原始输出。这没有证明所有发行版工具、使用 io_uring 专用 API 的应用或异步 fd 来源兼容。

### 监听方退出与崩溃

| 注入点 | 实测 | 结论 |
| --- | --- | --- |
| A 收到第一个受拦截调用时 tracer `_exit(88)` | 没有执行写入，被跟踪进程被 EXITKILL 终止 | 本次树的失败关闭成立，依赖设置 EXITKILL；原型没有模拟系统掉电 |
| B1/B2 收到第一个通知时关闭最后一个监听 fd | 等待中的 open 返回 ENOSYS（38） | 受拦截调用失败，不会自动杀整个进程树 |
| 同一 B 进程随后向继承的已有 fd 写入 | write 返回 6，内容改变 | listener 消失不等于数据面完全关闭，需要独立死亡联动来杀 PID namespace |
| 检查点后 SIGKILL 真正节点（各机制 1 次） | A 未执行待放行的 open 或旧 fd 写入；B1/B2 的 open 返回 ENOSYS，随后旧 fd 的 write 返回 6 | 区分真实节点死亡与仅关闭监听 fd；三种机制均已留下该路径的完整前态 clone |
| 预先有孤儿 daemon，到期杀 namespace init | 2 秒结束并回收后代 | 超时终止可覆盖脱离进程组的后代，不能只 kill 主进程 |

原始退出结果见 [failures.json](results/failures.json)、[supplement.json](results/supplement.json) 和 [crash.json](results/crash.json)。关闭 listener 测试隔离了内核的响应，SIGKILL 测试则真正杀死节点。后者的 B 工作进程在旧 fd 写入后自行退出，原型并没有因 broker 死亡主动终止它。如何确保 init 一起死、以及已完成代执行但未成功回复的补偿，仍需后续设计。

## 平台要求

| 机制/接口 | 上游版本下限 | 本次依赖与限制 |
| --- | --- | --- |
| A：seccomp BPF/TRACE、TRACESECCOMP | Linux 3.5 | 需 CONFIG_SECCOMP_FILTER 与 ptrace；EXITKILL 为 3.8；4.8 前 ptrace 后不重新检查 seccomp，不适合作为本方案基线 |
| B1：USER_NOTIF / NEW_LISTENER | Linux 5.0 | CONTINUE 为 5.5，故 B1 功能下限 5.5 |
| B2：NOTIF_ADDFD | Linux 5.9 | ADDFD_FLAG_SEND 为 5.14，故题设 B2 功能下限 5.14；发行版可能回移，应实际探测 ioctl |
| openat2 | Linux 5.6 | 本次 host 支持；B2 原型仅 resolve=0 |
| FICLONE 通用 ioctl | Linux 4.5 | 仍需具体文件系统 reflink 支持及同挂载文件系统；本机 XFS 实测通过 |
| 非特权 user namespace | Linux 3.8 起完整支持 | 仍受发行版 sysctl、LSM、namespace 配额和挂载策略影响；原型 B loop 还使用 Linux 5.3 的 pidfd_open 监测 supervisor 退出 |

版本依据：[seccomp(2)](https://man7.org/linux/man-pages/man2/seccomp.2.html)、[seccomp_unotify(2)](https://man7.org/linux/man-pages/man2/seccomp_unotify.2.html)、[ptrace(2)](https://man7.org/linux/man-pages/man2/ptrace.2.html)、[FICLONE(2)](https://man7.org/linux/man-pages/man2/FICLONE.2const.html)、[openat2(2)](https://man7.org/linux/man-pages/man2/openat2.2.html)、[user_namespaces(7)](https://man7.org/linux/man-pages/man7/user_namespaces.7.html)。上述是接口版本，不是本原型在所有旧内核上的兼容性承诺。

B **不占用 tracer 身份，不等于不依赖 ptrace 访问权限**。A 的 attach/seize 需要访问检查；B 在读取指针参数时打开 `/proc/<tid>/mem`，同样受 `PTRACE_MODE_ATTACH_FSCREDS` 检查。改用 `process_vm_readv` 仍是 `PTRACE_MODE_ATTACH_REALCREDS`；用 `pidfd_getfd` 复制目标 fd 也有 attach 检查。Yama、SELinux、凭据、dumpable 和 user namespace capability 都相关。本机父进程对其新建 user namespace 内目标具有相应能力，`PR_SET_DUMPABLE(0)` 的成功不能外推到不同 uid 或其他 LSM 策略。[proc_pid_mem(5)](https://man7.org/linux/man-pages/man5/proc_pid_mem.5.html)、[process_vm_readv(2)](https://man7.org/linux/man-pages/man2/process_vm_readv.2.html)、[pidfd_getfd(2)](https://man7.org/linux/man-pages/man2/pidfd_getfd.2.html)、[Yama](https://docs.kernel.org/admin-guide/LSM/Yama.html)。

没有权限修改主机 Yama/SELinux，未测试 `deny_ptrace=on` 或 scope 1/2/3。B 使用通知 fd 的接收/回复本身不需要 ptrace attach，但这一原型的路径参数读取需要；部署检查不能因此删除。监听 ID 在打开 mem 后和代执行前检查有效性；这防止一部分 PID 复用错误，不冻结整个文件系统，也不把文件变更和回复变成事务。

## 建议与取舍

1. **方向选 B2，验收保持待决。** 复制参数并代执行是消除此次内存竞态的必要改进，也不会占据目标的 ptrace 槽。B1 仅改变传输与事件模型，不能修复按路径检查后 CONTINUE 的根本问题。A 若要安全，需要冻结所有可能改参数/共享 fs 上下文的任务或改成代执行，已经超出 ADR 当前“停当前线程”的算法。
2. **用稳定对象绑定检查点与写入。** 对已有文件，研究先取得不带 TRUNC 的稳定 fd、核对实际 inode 与规则、FICLONE 后再对同一对象 truncate/注入。对创建、unlink、rename，研究父目录 fd、约束解析和受控目录项修改。不能把“字符串已复制”写成“消除了全部路径竞态”。外部同路径写者无法被沙箱内部串行化，须明确冲突定义和支持边界。
3. **先裁决硬链接与元数据合同。** `target/alias` 可改常规文件这一反例同时违反常规文件必留前态和 regenerable 免登记的简单路径模型。可考虑限制跨规则硬链接、按 inode 管理别名或改变豁免策略；这些会改变能力/状态归属，需要维护者决定。目录、链接、权限、xattr 是否可逆也应明确，不能用内容 hash 代替完整状态。
4. **失效时终止整个 PID namespace。** A 保留 EXITKILL；B 增加监听方死亡与 PID 1 生命周期联动，处理执行完毕但通知取消、ADDFD 失败、节点崩溃窗口。已有 fd、共享映射、SCM_RIGHTS、pidfd_getfd 等来源要在启动和运行期一并约束。
5. **按目标上下文代执行。** B2 需要匹配 mount/root/cwd、dirfd 对象、umask、凭据、权限/LSM、openat2 resolve、O_NOFOLLOW/O_EXCL 等语义；本次 umask 反例已说明宿主直接 open 不是透明代理。可以用受控 worker，但这会增加状态与进程模型，应另行设计，而非在本原型中接入产品。
6. **保留按登记集结算。** 本次其他文件的外部并发写未进入账本；不应为修复覆盖漏洞改成整树扫描。失败调用的预登记和未实际写入的写打开需要按后态去除，不能直接生成变更回执。

## 未验证事项与偏离

| 项目 | 本次状态/原因 |
| --- | --- |
| git commit 性能 | 规定的 bot-run 在实验 HOME 找不到 github-app-token，入口失败；没有完成实际 commit。失败输出已留存，不计入有效性能；未替换身份或写宿主认证目录 |
| 完整沙箱 | 未实现只读挂载投影、已有 hidden 的遮蔽、cgroup、网络代理；本次只比较通知机制，运行受控命令 |
| 产品接口 | 未接入 lancet-node、Starlark、协议、fs 工具账本；未改 docs/adr |
| 全状态恢复/崩溃恢复 | 没有真实 revert、重启恢复、断电、文件系统故障注入；有 clone/账本 fsync 和退出注入，不足以证明持久恢复合同 |
| 并发语义 | 未测非合作调度的自然竞态概率、共享 VM 的独立进程、并发 chdir/dirfd close/reuse、并发 rename 的全部交错、同路径外部写冲突判定；文件系统替换反例已足够否定全面免疫 |
| ABI 与边缘 syscall | 仅 x86-64；x32/其他 arch 被拒绝。未测 RENAME_WHITEOUT、跨挂载、设备节点、所有 ioctl/fcntl 数据修改、removexattr、copy_file_range/splice、SCM_RIGHTS 外来可写 fd、open_by_handle_at、pidfd_getfd、execveat 和 clone3 的全部变体 |
| ptrace 信号语义 | A 只实现本次 fork/exec/线程探针需要的事件循环；尚未区分所有 group-stop 与真实 SIGTRAP，不能视作完整调试兼容层 |
| 权限 | 不修改主机 Yama/SELinux；只验证当前策略。chown 只测当前 uid/gid，未测多用户或 setuid/setgid；B2 权限上下文不完整 |
| 工具兼容性 | 构建为全工作区，测试只选 net-guard 子集；没跑全仓测试或 gdb/strace，ptrace 由小程序替代 |
| 性能 | 3 次样本、共享主机、页缓存未清、debug=0 且 rustc incremental 关闭；不应把小百分比差异当成稳定排名。已有 75 GiB target 没复制，full 从空 target 编译，incremental 使用本轮产物 |
| 运行中修正 | 初次 benchmark 驱动有非 ASCII bytes 字面量语法错误；追加 race 观测时有括号错误，均在运行探针前暴露并修复。正式数据来自修复后运行；没有隐去系统调用失败与机制缺口 |

## 交付验证

已运行命令包括 `make`、`python3 -m py_compile *.py`、`python3 experiment.py all`、增加逐次 inode 观测后的 `python3 experiment.py races`、`python3 supplement.py`、`python3 crash.py`、`python3 benchmarks.py`。逐项原始结果可核对退出码、errno、clone 内容及漏记；实验预期失败和不满足要求的案例保留在表中。

`python3 summarize.py` 与 `python3 verify.py` 完成原始数据汇总和一致性检查。`cargo --offline --locked xtask docs-check` 成功检查 17 个已跟踪 Markdown 文件，输出见 [docs-check.json](results/docs-check.json)；该命令不覆盖未跟踪的新原型文档，所以另由 `verify.py` 核对本目录 README、REPORT 和结果表的相对链接。没有执行全仓 CI：变更未接入任何业务代码。

最终汇总与进程清理结果由 [verification.json](results/verification.json) 记录。副本和检查点留在 `.scratch/`，不进 Git；本 worktree 未提交、未推送、未进行 GitHub 写操作。父 Agent 审查后决定是否修改 ADR、补实验及提交。
