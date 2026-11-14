---
status: draft
---

# 子进程写拦截对比原型

本目录用同一份检查点算法比较 `TRACE + ptrace`（A）、通知后 `CONTINUE`（B1）和通知后代执行（B2）。结论、覆盖缺口及测量口径见 [REPORT.md](REPORT.md)。这是 Linux x86-64 研究原型，不是安全沙箱或节点实现。

## 运行

需要 GCC、Python 3、Git、支持非特权 user/mount/PID/net 命名空间的 Linux，以及与工作负载同文件系统的 XFS reflink。无需 `libseccomp-devel`、root、bubblewrap 或系统软件安装。运行目录是本 worktree 的 `prototypes/write-intercept/`。

```bash
make
python3 experiment.py all
python3 supplement.py
python3 crash.py
python3 benchmarks.py
python3 summarize.py
python3 verify.py
```

`experiment.py` 可单独运行 `environment`、`coverage`、`races`、`failures`、`micro`。`make check` 运行基础覆盖；`python3 -m py_compile *.py` 检查驱动语法。`benchmarks.py` 需要约二十分钟，顺序执行，固定 `rustc 1.96.0` 和 4 个编译作业。它从 `/home/gcp-vegas/Dev/lattice` 建立本地克隆，从 `~/.cargo/registry` 建立 reflink 缓存副本；这两个源只读。更换机器时修改脚本中的源和工具链路径。

副本、缓存、二进制、检查点和测试夹具都在被忽略的 `.scratch/` 内。每次运行的命令、退出码、stdout、stderr、计数与账本导出到 `results/runs/`；覆盖测试的前后观测另存为压缩 JSON。原型本身退出后只摘要已登记路径；外置 oracle 的全树观测仅用于核对漏记，未参与被测算法或性能计时。

单独执行命令：

```bash
mkdir -p .scratch/manual-work .scratch/manual-state
.scratch/intercept B2 "$PWD/.scratch/manual-work" "$PWD/.scratch/manual-state" 10 -- /bin/sh -c 'echo example > file'
```

`MODE` 取 `none`、`ns`、`A`、`B1`、`B2`。`STATE` 每次须使用空目录，且位于 `ROOT` 之外。`ns` 只建命名空间；A/B 都使用同一套命名空间。PID 1 垫片等待全部后代，到期杀掉 PID 命名空间 init。三种机制仅拦 ADR 所列写入口，额外包含 `rmdir`；这使覆盖缺口能直接观测。`openat2` 的标志位在内存中，全部通知用户态后检查。

测试用环境变量 `WI_RACE_CONTROL`、`WI_CLOSE_AFTER`、`WI_CRASH_AFTER` 分别提供确定性竞态握手、关闭最后一个监听 fd、退出 tracer。它们不属于拟议产品接口。并发改写参数的握手发生在参数复制和检查点之后、恢复或代执行之前；结果证明可达性，不能当作无辅助攻击的自然命中率。

## 文件

| 文件 | 用途 |
| --- | --- |
| [intercept.c](intercept.c) | BPF、命名空间启动、SCM_RIGHTS、ptrace/notifier 事件循环、检查点与 B2 代执行 |
| [probe.c](probe.c) | 系统调用、进程树、线程竞态及兼容性探针 |
| [experiment.py](experiment.py) | 夹具、前后观测、仅登记集摘要、基础覆盖和微基准 |
| [supplement.py](supplement.py) | 悬空链接、umask、文件系统竞态和已有 fd 失效探针 |
| [crash.py](crash.py) | 检查点后 SIGKILL 节点，验证真正退出时的行为 |
| [benchmarks.py](benchmarks.py) | LATTICE 副本上的五配置基准 |
| [summarize.py](summarize.py) | 由原始 JSON 重建结果表、CSV 与 HTML 图 |
| [verify.py](verify.py) | 核对样本数、关键反例、交付链接及残留进程 |
| [results/](results/) | 原始结果与可再生成的汇总 |

## 实验边界

路径策略固定为根目录下的 `hidden/` 拒绝、`target/` 与 `.git/` 跳过。没有实现 `.gitignore` 模式匹配、资源投影、hidden 遮蔽挂载、完整 VFS 解析、权限上下文重建或恢复引擎。B2 对 `openat2.resolve != 0` 返回 `EOPNOTSUPP`。命名空间没有把宿主文件树变成只读；只运行本目录生成的受控探针，不能把任意不可信程序交给这个原型。

`FICLONE` 文件、账本和检查点目录在放行前执行 `fsync`。写前 SHA-256 在退出后读取 clone 验证，未在放行前持久保存摘要；断电恢复与 ADR 的持久回执语义未实现。原始结果保留失败项，脚本退出 0 只代表探针执行完毕，不代表覆盖保证成立，应检查 `rc`、`errno`、`missing` 和 `prestate_bad`。

Git 提交基准调用规定的 `bot-run` 入口。实验 HOME 未提供其认证辅助工具，因此本次该项失败，不能把失败时间视为提交性能。未更换提交身份或复制宿主认证材料。其他 Git 基准只作用于 `.scratch/lattice`；本 worktree 不提交、不推送。
