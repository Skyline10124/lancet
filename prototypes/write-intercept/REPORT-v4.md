---
status: draft
---

# 写拦截第四轮：持久 reflink 影子与 B3

## 结论

本报告评估 S 的原型，不修改 ADR 0002 或节点公共契约。建议本阶段采用 B3，继续研究 S 的批量文件修改路线；在完成下述边界裁决前，不把本轮原型当作可替换 B3 的产品实现。S 去掉每次首次写入前的同步等待，也去掉每次写调用的代理；代价是每轮全树扫描、持久影子索引、结束时合并以及共享 Git 索引往返。

工作区为 `/home/gcp-vegas/.t3/worktrees/lancet/t3-92eb11b2`。第三轮来源、主机配置和源码摘要见 [environment-v4.json](results/v4/environment-v4.json)、[benchmark-source-hashes-v4.json](results/v4/benchmark-source-hashes-v4.json)。按任务要求从 `origin/exp/write-intercept` 取回整个原型目录；前三轮源码、报告和结果不改动。没有提交、推送或 GitHub 写操作。所有工作负载和缓存均由指定第三轮 `.scratch/` 以 `cp -a --reflink=always` 复制到本 worktree；原副本只读。

## 七项验证

<!-- VALIDATION:START -->
| 验证项 | 结论 | 证据与限制 |
| --- | --- | --- |
| 1 正确性 | 适用覆盖通过，原型边界未完成 | 63 类 × commit/discard = 126/126；与 ns 实际行为一致，ε、提交、丢弃、新进程 revert 均核对；固定分类、xattr 策略、正式 fs.* 接线未实现 |
| 2 外部并发写 | 运行期归属/提交前冲突通过，合并窗口仍有风险 | 12/12 运行保留外部字节；其他文件不进 ε；比对后外部写仍可丢失，discard 历史影子不保证 |
| 3 提交崩溃恢复 | 55/55 SIGKILL 通过 | 11 阶段 × 5；新进程恢复前态/后态和账本；5/5 重启后下一 act 可用；未测断电/恢复中二次崩溃/停机外部写 |
| 4 Git 交互 | 成功提交无虚假普通变更；缓存与丢弃问题存在 | 4 配置 × 4 命令 × 3；共享索引重新比对、HEAD/普通文件丢弃不一致及 minimal 漏检反例均保留 |
| 5 跨 act 增量构建 | 见 Cargo 实际编译项 | act 3：Compiling lattice-cli v0.1.0 (/home/gcp-vegas/.t3/worktrees/lancet/t3-92eb11b2/prototypes/write-intercept/.scratch/lattice/crates/lattice-cli)；宿主随后：无 |
| 6 性能 | 要求的四配置及规模样本已完成 | 全量/checkout/两类 micro 各 3；3×3 同步、3 类提交、3 类初建各 3；checkout S/B3=70.8% |
| 7 复杂度 | 内核代理减少，持久同步/合并状态增加 | 附机制、进程、状态和实际行数；Python S 与 C B3 不等安全覆盖，行数仅描述原型 |
<!-- VALIDATION:END -->

## S 的实现与口径

[shadow-v4.py](shadow-v4.py) 是节点侧目录资源研究原型；[namespace-v4.c](namespace-v4.c) 建立用户、挂载、PID、网络命名空间。影子绑定到与真实目录相同的绝对路径，再绑定真实 `target/` 与 `.git/`。状态目录、`hidden/` 用只读空挂载遮蔽，exec 前关闭继承 fd、清除能力并设置 `no_new_privs`。挂载过程不需要 root、sudo、bubblewrap。没有 seccomp 写通知、代理 worker、ADDFD 或参数复制。

本轮分类固定为顶层 `target/`、`.git/`、`hidden/`，不声称实现任意 glob 规则、多个挂载根或完整恶意代码沙箱。未接入 Starlark、WASI `fs.*`、正式账本或协议回执。测试中的节点直接写影子模拟 `fs.*`；子进程使用真实挂载路径。原型仍可读宿主其他路径，也没有生产级 Landlock 写范围限制。C 启动器和 S 事务模块代码量不能作为同等安全覆盖的实现成本比较。

持久索引保存真实与影子的 stat 及内容摘要。每次 `begin` 遍历真实目录，比较 `mtime_ns`、`ctime_ns`、size、dev/inode、类型、权限、nlink；只物化改变条目及其硬链接组，再遍历影子建立基线。未改变内容的 SHA-256 从索引复用；失配的文件读取内容。初建保留 mode、atime/mtime、符号链接文字以及常规路径之间的硬链接组。ctime、inode 必然不同，不能伪装成真实文件的身份。排除目录在影子里只有空挂载点，不复制其内容。

结束扫描仅在影子上进行。ε 比较内容、存在性、类型、mode、符号链接文字、常规硬链接拓扑；仅改时间戳或写后回到原内容不产生普通文件变更。目录按影子 dev/inode 识别改名，输出 `directory_renames`。**本原型的提交和同步仍把目录移动展开成路径增删，并不执行 B3 的常数条数结构化 rename 重放**；这保留最终语义，但多做克隆、改变真实目录 inode，不能宣称已经实现了目录移动的最佳复杂度。

事务流程：

1. 比较真实当前 stat 与 act 开始时 stat，任何涉及路径的不同都保守报冲突，整批不应用。普通外部同内容 touch 也可能冲突。
2. 写 `preparing` 日志；为待覆盖、删除的文件 `FICLONE` 前态；已有多链接 inode 建持久锚点。
3. `syncfs` 整批落盘，再持久写入 `ready`；只有此后才修改真实目录。
4. 单链接内容 reflink 到同目录临时文件后 rename 原子替换。仍对应同一影子 inode 的多链接内容写入，在真实原 inode 上写；改名覆盖别名则替换那个目录项，不错误地改掉其他别名。
5. 再次 `syncfs`，保存后态、写 `complete`，更新同步索引。无 ε 的 act 不建事务、不刷前态。

`syncfs` 是整个 XFS 的批量刷盘，不是只刷本事务文件：它可能连带主机其他脏数据，测试计时包含这部分等待。没有把一个用户态批次宣称为一次物理 XFS 日志提交。

对象前态沿用 B3 的 `ledger.jsonl`、`index.clone`、`dev-ino.anchor` 字段与文件命名；S 另加事务阶段、基线和更丰富的后态 stat。S 自己的恢复器用于三方保护与类型变更。原版 [recover-v3.py](recover-v3.py) 读取 S 简单文件修改前态的兼容验证见 [ledger-compat-v4.json](results/v4/ledger-compat-v4.json)：调用前需把后态投影到 B3 字段集合，所以这是对象记录兼容，不是正式节点账本的直接接线。

[recover-v4.py](recover-v4.py) 是重启入口。`preparing` 没有应用许可，只清锚点；`ready` 还原整批前态；`complete` 保留后态。然后从真实目录重新物化影子，修复被杀进程留下的脏影子和过期索引；重启允许整树克隆。单独调用事务恢复只修复真实目录，不能替代节点重启入口。

## 正确性、并发和崩溃证据

[verify-v4.py](verify-v4.py) 复用第三轮 C 探针与独立全树 oracle。63 类操作各测提交和不提交，并在新夹具上执行原版 `ns` 对照；要求退出码和实际后态与对照一致，ε 路径与独立 oracle 相同，提交前真实目录未变，提交后与影子语义相同，不提交后真实目录未变，另起恢复进程求逆后回到前态。覆盖普通 open 家族、失败/空写、存在性、权限、符号链接、普通硬链接、删除/覆盖别名、目录改名、改名后编辑、类型互换、FIFO、mmap、ftruncate、fallocate、fork/thread、两种 O_TMPFILE 链接入口。详见 [coverage-v4.json](results/v4/coverage-v4.json)。

硬链接 oracle 比较组内所有名字的拓扑；另在 [supplement-v4.json](results/v4/supplement-v4.json) 核对多链接内容写入/求逆前后的真实 inode、初建 mtime 与 mode、两个 act 提交后逆序 revert、revert 时外部冲突，以及外部增删、类型变化、目录移动、硬链接组变化后的同步。

[boundaries-v4.json](results/v4/boundaries-v4.json) 覆盖三类别之间六个方向的 link/rename，要求内核返回 `EXDEV`；`hidden` 的五种新建入口要求拒绝。只验证新建跨类别关系，**已有跨类别硬链接不会被 bind mount 拆散**：[supplement-v4.json](results/v4/supplement-v4.json) 的 `target/alias` 仍能写真实常规 inode，影子 ε 看不到。这与 B3 的既有跨类别别名风险相似，不能用新 link 返回 EXDEV 证明已消除。

外部并发写见 [concurrent-v4.json](results/v4/concurrent-v4.json)：子进程写影子 `a`，通过直通 `target/ready` 握手后，外部进程在 act 未结束时写真实 `outside` 或 `a`；分别测 commit/discard，各重复三次。其他文件没有混入 ε；同文件 commit 报 `Conflict`，外部字节保留。

未提交的恢复采用题目允许的“从真实目录重同步”做法。如果 act 期间外部写了同一个真实文件，此时原始字节在真实与影子中都可能不存在，故只能恢复到**最新真实内容**，不能精确恢复 act 开始的历史影子。测试保留这个差别。若必须保证旧影子字节，需要每 act 保存不可变基线或其他写时保留机制，成本与设计需另行裁决。

三方比对也不是对外部写者的原子 compare-and-swap。[commit-window-v4.json](results/v4/commit-window-v4.json) 在比对及前态落盘后、应用前注入同路径外部写，外部字节被覆盖。节点内部互斥锁约束不了不合作的外部编辑器；本轮没有解决这项合并窗口风险。同步扫描同样不是原子文件系统快照，没有证明外部持续写入时的统一时点视图。

崩溃实验见 [crashes-v4.json](results/v4/crashes-v4.json)：11 个阶段各 5 次，共 55 次真实 `SIGKILL`，覆盖日志前后、前态落盘前后、`ready` 后、第 1/4 项应用后、后态落盘前后、完成标记后及多链接原 inode 只写入首字节后。每次以新进程恢复，并独立检查路径必须为前态或后态、整批应为预期一侧、账本终态一致。恢复之后再开始 act 的证据见 [restart-v4.json](results/v4/restart-v4.json)。

这不是电源故障、设备掉盘或恢复进程再次被杀的验证。测试没有在停机期间加入外部编辑；`ready` 的恢复会强制还原前态，在停机期间有不合作外部写者时可能覆盖它。多链接原 inode 的写入在节点被杀前可能是部分字节，恢复后才回到完整前态。外部读者也可能观察到混合批次，整批提交不是跨路径原子事务。

## Git 与跨 act 构建

[git-v4.py](git-v4.py) 使用相同的 4,000 文件合成差异，在真实 `.git/` 上执行 sandbox `status`、`add`、`commit`、`checkout`，S 结算后执行宿主 `status`，随后再次执行影子 `status`。每个配置与命令三次。`GIT_TRACE2_EVENT` 的 `refresh/sum_scan` 是 Git refresh 中进入内容核对路径的计数，不能把遍历 stat 次数等同于重读内容；计数位置见 [Git read-cache.c](https://github.com/git/git/blob/master/read-cache.c)。`add` 预期有一个 staged M；其余成功提交场景预期干净。

<!-- GIT:START -->
| 配置 | 沙箱命令 | 宿主 status ms | 宿主 sum_scan | 再入沙箱 status ms（含启动） | 影子 sum_scan | 预期输出 |
| --- | --- | --- | --- | --- | --- | --- |
| default | status | 64.086 | 4485 | 65.910 | 4485 | 3/3 |
| default | add | 7.698 | 1 | 66.123 | 4485 | 3/3 |
| default | commit | 65.174 | 4485 | 65.079 | 4485 | 3/3 |
| default | checkout | 64.208 | 4485 | 65.238 | 4485 | 3/3 |
| minimal-inside | status | 6.237 | 0 | 7.413 | 0 | 3/3 |
| minimal-inside | add | 7.628 | 1 | 8.890 | 0 | 3/3 |
| minimal-inside | commit | 7.337 | 1 | 8.659 | 0 | 3/3 |
| minimal-inside | checkout | 42.770 | 4000 | 7.617 | 0 | 3/3 |
| minimal-noctime-inside | status | 6.219 | 0 | 8.593 | 0 | 3/3 |
| minimal-noctime-inside | add | 7.626 | 1 | 8.994 | 0 | 3/3 |
| minimal-noctime-inside | commit | 7.301 | 1 | 8.595 | 0 | 3/3 |
| minimal-noctime-inside | checkout | 42.684 | 4000 | 7.650 | 0 | 3/3 |
| minimal-noctime-both | status | 6.156 | 0 | 7.352 | 0 | 3/3 |
| minimal-noctime-both | add | 7.671 | 0 | 8.843 | 0 | 3/3 |
| minimal-noctime-both | commit | 7.337 | 0 | 8.722 | 0 | 3/3 |
| minimal-noctime-both | checkout | 41.839 | 0 | 7.648 | 0 | 3/3 |

原始结果见 [git-v4.json](results/v4/git-v4.json)。`*-inside` 只设沙箱进程环境变量，没有修改仓库配置；`*-both` 额外改变本次宿主进程设置用于诊断。计数是实测，不假定 minimal 一定减少全部扫描。
<!-- GIT:END -->

`core.checkStat=minimal` 的文档描述与此版本实现有细微差异：[git-config 官方文档](https://git-scm.com/docs/git-config#Documentation/git-config.txt-corecheckStat) 描述忽略 inode、uid/gid、子秒时间但保留受信的整秒 ctime；[Git 2.52.0 statinfo.c](https://github.com/git/git/blob/v2.52.0/statinfo.c#L58) 的 ctime 比较还以 `check_stat` 为条件，minimal 下也跳过。因此本机无需另加 `core.trustctime=false` 才能绕开影子 ctime 差异，以实测为准。减弱检查损失变更识别信号：[git-minimal-only-risk-v4.json](results/v4/git-minimal-only-risk-v4.json) 与 [git-minimal-risk-v4.json](results/v4/git-minimal-risk-v4.json) 分别测试 minimal 单独启用及与 trustctime=false 组合，在等长改写且保留旧 mtime 时的漏检。单独 minimal 的最终独立对照跨越 ctime 整秒：minimal 输出干净，默认配置能够检测变更。较早组合实验在同秒内默认配置也输出干净，不能据此声称发生了索引污染。

`.git/` 是直通的不可逆内部状态。未调用 S `commit()` 的 `git checkout`、`git commit` 反例见 [git-discard-v4.json](results/v4/git-discard-v4.json)：普通文件被丢弃而 HEAD/索引已经改变，真实 `git status` 因而显示不一致。S 没有自动恢复 HEAD/refs，也没有把 Git 提交当作普通文件事务。

[incremental-v4.json](results/v4/incremental-v4.json) 在 LATTICE 上执行：act 1 全 workspace 构建；act 2 修改 CLI 的一个源文件并提交；act 3 同路径再构建；最后宿主再构建。用 verbose Cargo 输出核对重编 crate。这里开启 `CARGO_INCREMENTAL=1`，四配置全量基准保持第三轮的 `CARGO_INCREMENTAL=0`，两项数字不能混用。

<!-- INCREMENTAL:START -->
act 3 编译项：`Compiling lattice-cli v0.1.0 (/home/gcp-vegas/.t3/worktrees/lancet/t3-92eb11b2/prototypes/write-intercept/.scratch/lattice/crates/lattice-cli)`；宿主随后编译项：`无`。act 1 / act 3 总时间分别 58.151 / 1.108 秒；宿主 0.263 秒。
<!-- INCREMENTAL:END -->

## 性能

同一主机顺序执行，同一时间只有一套重型实验。Rust 1.96.0、4 个 Cargo 作业、debug=0、离线 workspace 构建，每次删 target；保留页缓存，不使用 root 清缓存。共享主机、每组至少三次，表格取中位数，不能把百分比差异解读成统计置信保证。构建期间只编写脚本与报告、读取轻量状态，没有并行跑第二套构建。

B3、none、ns 直接调用第三轮 `benchmarks-v3.py` / `experiment-v3.py` 驱动及原始二进制实现，把输出重定向至 v4。主表对照取其 `stats.wall_s`，沿用第三轮“启动至退出，不含外置 oracle 摘要”口径，同时保存 `process_wall_s`。S 主表是 `begin + exec + finish + commit`，包含同步、扫描、冲突检查、日志与落盘；初建单列，持久对象打开/JSON 反序列化发生在计时前，属于常驻节点对象加载假设。没有只拿子进程时间冒充 S 总时间。

<!-- PERFORMANCE:START -->
四配置中位数，秒；原始样本见 [builds-v4.json](results/v4/builds-v4.json)、[checkouts-v4.json](results/v4/checkouts-v4.json)。

| 工作负载 | 无沙箱 | 仅命名空间 | B3 | S（总计） |
| --- | --- | --- | --- | --- |
| LATTICE 全量构建 | 54.760041 | 54.292310 | 55.220613 | 54.161933 |
| 4,000 文件 checkout | 0.250877 | 0.271530 | 12.622207 | 8.931661 |

相对 ns，全量构建 B3 +1.71%，S -0.24%。S checkout 总时间为 B3 的 70.8%（1.41 倍速度）；不等于无沙箱成本。

S 分项中位数，秒；各列独立取中位数，故相加不一定等于总计。

| 工作负载 | 同步 | 子进程 | 结束扫描 | 提交（含准备） | 总计 |
| --- | --- | --- | --- | --- | --- |
| 全量构建 | 0.058010 | 54.093478 | 0.010416 | 0.000000 | 54.161933 |
| checkout | 4.637415 | 0.315898 | 0.155346 | 3.712990 | 8.931661 |

20,000 次 O_WRONLY open+close 的探针内部单次中位数，µs。普通文件没有实际改写，B3 只在第一次打开建前态。见 [micro-v4.json](results/v4/micro-v4.json)。

| 路径 | 无沙箱 | 仅命名空间 | B3 | S |
| --- | --- | --- | --- | --- |
| /dev/null | 3.400 | 3.380 | 28.953 | 3.387 |
| a | 3.201 | 3.177 | 170.276 | 3.391 |

act 前同步中位数，秒；包含两棵树 stat、变更内容摘要、增量物化与两个 JSON 快照落盘。目录条目另有 100 个。见 [sync-v4.json](results/v4/sync-v4.json)。

| 规模 | 外部改 0 文件 | 外部改 10 文件 | 外部改 1,000 文件 |
| --- | --- | --- | --- |
| 3100 条目（3000 文件） | 0.179091 | 0.189158 | 0.858405 |
| 10100 条目（10000 文件） | 0.635936 | 0.641398 | 1.307507 |
| 100100 条目（100000 文件） | 7.688587 | 7.678454 | 8.328069 |

结束扫描与提交中位数，秒；固定 4,000 个常规文件。准备包含真实目录三方核对、前态与 ready 日志；应用包含后态及索引落盘。见 [settlement-v4.json](results/v4/settlement-v4.json)。

| 改变文件数 | 扫描 | 准备 | 应用及结束 | 提交总计 |
| --- | --- | --- | --- | --- |
| 1 | 0.054964 | 0.054037 | 0.063014 | 0.117004 |
| 100 | 0.056426 | 0.068655 | 0.133715 | 0.202378 |
| 4000 | 0.138341 | 0.571891 | 2.754029 | 3.330625 |

初次建立中位数，各规模三次；秒 / MiB。包含初次摘要与索引建立，未预刷盘输入。三次均新建相同字节的输入文件，未把已刷盘输入混入初建中位数。

| 文件数 | 初建秒 | 影子 apparent MiB | 影子 allocated MiB | 整个状态 allocated MiB |
| --- | --- | --- | --- | --- |
| 3000 | 3.289502 | 1.06 | 12.11 | 16.69 |
| 10000 | 11.135725 | 3.63 | 39.85 | 54.86 |
| 100000 | 100.722825 | 39.33 | 395.32 | 544.52 |

统计范围和全部样本见 [summary-v4.json](results/v4/summary-v4.json)，图见 [performance-v4.html](results/v4/performance-v4.html)。
<!-- PERFORMANCE:END -->

初建磁盘占用同时报告 `du` apparent 与 allocated。reflink 的共享块会被 `du` 在两个名字下分别计算，不能把两个 allocated 之和当作新增物理占用；[initial-v4.json](results/v4/initial-v4.json) 的 `filefrag` 样本记录共享 extent。没有全文件系统独占物理块计量。

补充诊断 [profile-v4.json](results/v4/profile-v4.json) 在三组交错对照中，比较克隆前是否先对真实目录执行一次 `syncfs`，并包裹 clone 函数累计其耗时。该诊断用 `checkout -f` 复位，会重建 4,485 个文件，不能与主表的 4,000 文件样本混合。预刷盘计入总时间：总计中位数由 9.256 秒降为 7.661 秒；预刷盘本身 0.259 秒，同步阶段 clone 累计由 2.432 秒降为 0.525 秒。提交阶段 clone 累计约 0.82–0.84 秒。这支持源数据刷盘影响串行 reflink 成本，不能据此把余下时间都归因于物理日志。原型主路径未改成此策略，主表保留原始 S 定义的完整实测；是否引入整文件系统预刷盘应同时权衡与其他工作区的干扰。

S 当前用 Python 字典和 JSON 持久化全量索引；每 act 仍有 O(N) 扫描、序列化和内存成本。100,000 条目的结果是该原型实测，不是 C/Rust 实现的理论下限。它已避免每轮整树 FICLONE，但还没有以增量索引事务优化全量 JSON 写入。

## 与 B3 的复杂度比较

| 项目 | B3 | S 本原型 |
| --- | --- | --- |
| 写入路径 | 原地；首次写前留前态并等持久化 | 常规路径写持久影子；结束时合并 |
| 内核机制 | namespaces、Landlock、seccomp USER_NOTIF、process_vm_readv、O_PATH、ADDFD、FICLONE、硬链接锚点、fsync | namespaces、bind mount、FICLONE、普通 rename/link、锚点、syncfs；无写调用代理 |
| 进程/线程 | 节点、launcher、PID init、子进程，另一个常驻 worker；通知与批量刷盘线程 | Python 节点、launcher、PID init、子进程；无每写调用 IPC/线程 |
| 持久状态 | 首次写对象账本、clone、锚点、后态与冲突记录 | 整树影子、两个 stat/摘要索引、act 基线，加对象账本、clone、锚点与事务阶段 |
| 每 act 固定成本 | 按被登记对象结算 | 遍历 N 个条目并持久化全量索引；结束再扫描影子 |
| 密集串行修改 | 写进程逐次等待首次写的持久化 | 子进程直接写；已知 ε 后整批留前态、应用、落盘 |
| 目录改名 | B3 结构化记录，不逐个 clone | 按 inode 识别，但当前应用展开路径，非等复杂度 |
| 外部写入 | 依靠登记来源与后态保护；VFS 竞态残余 | 运行期影子归属清楚；合并比对之后仍有外部写竞态 |
| 额外产品职责 | 代理 syscall 与进程上下文语义 | 同步索引、三方合并、影子恢复、Git stat 缓存往返 |

<!-- LOC:START -->
物理行数（含空行、注释，非可维护性评分）：B3 执行/恢复源共 1348 行；S 执行/恢复源共 514 行。B3 统计 `intercept.c, intercept-v3.c, v3-node.h, v3-runtime.h, recover-v3.py`；S 统计 `shadow-v4.py, namespace-v4.c, recover-v4.py`。测试和结果生成脚本另计，不混入实现行数。
<!-- LOC:END -->

## 与 ADR 否决理由的关系、未完成项及待裁决

S 不用 overlayfs，没有 lower/upper、whiteout 和 lower 目录 rename 的 EXDEV；普通目录改名用例成功。它保留真实 regenerable 目录，未因丢弃 act 清理 target。**但共享 `.git/` 后丢弃普通文件的状态撕裂仍然存在，不能宣称完全绕过 overlayfs 被否决的全部理由。** 若沿用现 ADR 对 `.git/` 的不可逆内部状态例外，需要明确允许该风险、如何呈现回执，并接受 Git 索引开销。

S 不在每次 proc 启动时克隆整树，持久影子隔离使外部其他文件不进入 ε，这避开“整树基线加事后扫描”最严重的来源混淆。但是每 act 的扫描与索引成本仍在，100,000 项的实测决定它是否符合目标延迟。这里的差别是来源隔离和摊销克隆，不是“扫描成本已经消失”。

| 事项 | 本轮状态 / 需维护者裁决 |
| --- | --- |
| 是否采用 S | 建议当前保留 B3，继续优化 S 后再决定是否引入可选执行模式；本轮不建议直接增加产品的双执行路径。若以后组合，两者必须由同一目录资源与账本持有者管理，不能并行写同一工作目录 |
| Git 未提交语义 | 是否接受 HEAD/索引变化作为不可逆 `InternalState`；若要求普通文件与 `.git` 一起回滚，当前 S 定义不满足 |
| 外部并发保证 | 接受比对之后的合并竞态，或引入协作式工作区锁等机制；不合作外部写者下的无丢失更新尚未证明 |
| 持久基线 | 外部同文件写入后，discard 只同步最新真实内容；如要求恢复影子旧字节，需要额外不可变基线机制 |
| 崩溃恢复 | 仅 SIGKILL 和新进程重启；未测断电、磁盘故障、恢复中再次崩溃和停机期间外部写入 |
| 目录移动 | 当前按 inode 识别、路径展开应用；后续需设计三方保护下的结构化改名与恢复，避免用整个子树克隆替代 |
| 安全与分类 | 固定顶层分类，没有任意 regenerable/hidden 模式或完整宿主写范围约束；现有跨类别硬链接仍能写穿真实常规文件；未实现 xattr 的 ENOTSUP 策略 |
| fs.* / 正式账本 | 只模拟共享影子入口、验证对象前态格式；未接入正式 API、节点 invocation 生命周期、residue 与保留期 |
| 缓存与成本 | 全量 JSON 索引、哈希缓存、syncfs 整个 XFS 和目录展开影响结果；建议用同一资源组件实现增量索引后再作产品级性能验收 |

## 复现、失败记录与检查

```sh
make -C prototypes/write-intercept v3
TMPDIR="$PWD/prototypes/write-intercept/.scratch" gcc -O2 -g -Wall -Wextra \
  prototypes/write-intercept/namespace-v4.c -o prototypes/write-intercept/.scratch/namespace-v4
python3 prototypes/write-intercept/verify-v4.py
python3 prototypes/write-intercept/benchmarks-v4.py
python3 prototypes/write-intercept/git-v4.py
python3 prototypes/write-intercept/supplement-v4.py
python3 prototypes/write-intercept/supplement-v4.py initial_repeats
python3 prototypes/write-intercept/profile-v4.py
python3 prototypes/write-intercept/summarize-v4.py
python3 prototypes/write-intercept/audit-v4.py
python3 prototypes/write-intercept/docs-check-v4.py
```

先按任务路径复制 LATTICE/Cargo 副本再执行这些命令；脚本不下载包、不安装系统软件。运行时只写本 worktree `.scratch/` 与 `results/v4/`。正式基准全部成功才进入中位数。采集后只把 B3 驱动的结果文件名从 `v4-` 前缀统一为 `-v4` 后缀，并同步驱动的输出名；计算逻辑不变，内部 run ID 保留采集时名称，映射见 [artifact-renames-v4.json](results/v4/artifact-renames-v4.json)。

初始失败记录保留在 [coverage-invalid-mount-v4.json](results/v4/coverage-invalid-mount-v4.json) 和 [coverage-before-fixes-v4.json](results/v4/coverage-before-fixes-v4.json)：首版挂载失败，但旧 oracle 只看到前后不变，错误地标了通过；这些记录整体无效，不计数。修正后必须同时满足实际执行与 ns 对照。随后修正了改名覆盖硬链接时错误原 inode 写入、目录转文件时错误按 nlink 判断，以及恢复清理路径父对象已变文件的错误。

<!-- CHECKS:START -->
- `make -C prototypes/write-intercept v3`、最终 `gcc -O2 -g -Wall -Wextra` 与所有 v4 Python 文件的 `py_compile` 通过。见 [compile-v4.log](results/v4/compile-v4.log)、[compile-final-v4.json](results/v4/compile-final-v4.json)。
- `python3 prototypes/write-intercept/audit-v4.py` 的 24 项结果矩阵检查全部通过；这也检查已知反例确实触发，不能把“反例检查通过”解释为风险已消除。见 [audit-v4.json](results/v4/audit-v4.json)。
- `python3 prototypes/write-intercept/docs-check-v4.py` 用临时索引执行 `cargo --offline --locked xtask docs-check`，通过，真实 Git 索引校验和不变。见 [docs-check-temporary-index-v4.json](results/v4/docs-check-temporary-index-v4.json)。
- 对 `origin/exp/write-intercept` 下 2,092 个原有文件逐一核对 Git blob 摘要，无差异。见 [baseline-integrity-v4.json](results/v4/baseline-integrity-v4.json)。
- 新增文件的完整绝对路径清单见 [files-v4.json](results/v4/files-v4.json)。结束前的进程核对见 [cleanup-v4.json](results/v4/cleanup-v4.json)。没有提交、推送、GitHub 写操作或 ADR 改动。
- 没有运行全仓 CI：本轮只改隔离的原型与实验报告，没有修改产品 crate；验证范围及尚未实现的生产能力见前文。
<!-- CHECKS:END -->
