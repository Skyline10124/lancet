// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 沙箱内 PID 1 垫片程序（挂入 `/run/lancet-init`）。
//!
//! 职责：
//! 1. 在最终挂载视图里施加 Landlock 写规则并设 `PR_SET_NO_NEW_PRIVS`；
//! 2. 成功后按阶段自左向右滚动建管道并 `fork`/`execve`，确认当前阶段启动后再启动下一阶段；
//! 3. 首段确认后立即经状态 fd 向节点发 `O`，任一段启动失败则报阶段序号与错误并立即退出；
//! 4. 回收全部前台阶段（及收养的后代）并监听控制管道，全退后以 pipefail 码退出，控制管道 EOF 时立即退出。

use std::ffi::{CString, OsString};
use std::io;
use std::os::fd::RawFd;
use std::os::unix::ffi::OsStrExt as _;
use std::ptr;

const LANDLOCK_RULE_PATH_BENEATH: u32 = 1;

const LANDLOCK_ACCESS_FS_WRITE_FILE: u64 = 1 << 1;
const LANDLOCK_ACCESS_FS_REMOVE_DIR: u64 = 1 << 4;
const LANDLOCK_ACCESS_FS_REMOVE_FILE: u64 = 1 << 5;
const LANDLOCK_ACCESS_FS_MAKE_CHAR: u64 = 1 << 6;
const LANDLOCK_ACCESS_FS_MAKE_DIR: u64 = 1 << 7;
const LANDLOCK_ACCESS_FS_MAKE_REG: u64 = 1 << 8;
const LANDLOCK_ACCESS_FS_MAKE_SOCK: u64 = 1 << 9;
const LANDLOCK_ACCESS_FS_MAKE_FIFO: u64 = 1 << 10;
const LANDLOCK_ACCESS_FS_MAKE_BLOCK: u64 = 1 << 11;
const LANDLOCK_ACCESS_FS_MAKE_SYM: u64 = 1 << 12;
const LANDLOCK_ACCESS_FS_REFER: u64 = 1 << 13;
const LANDLOCK_ACCESS_FS_TRUNCATE: u64 = 1 << 14;

const HANDLED_ACCESS_FS: u64 = LANDLOCK_ACCESS_FS_WRITE_FILE
    | LANDLOCK_ACCESS_FS_REMOVE_DIR
    | LANDLOCK_ACCESS_FS_REMOVE_FILE
    | LANDLOCK_ACCESS_FS_MAKE_CHAR
    | LANDLOCK_ACCESS_FS_MAKE_DIR
    | LANDLOCK_ACCESS_FS_MAKE_REG
    | LANDLOCK_ACCESS_FS_MAKE_SOCK
    | LANDLOCK_ACCESS_FS_MAKE_FIFO
    | LANDLOCK_ACCESS_FS_MAKE_BLOCK
    | LANDLOCK_ACCESS_FS_MAKE_SYM
    | LANDLOCK_ACCESS_FS_REFER
    | LANDLOCK_ACCESS_FS_TRUNCATE;

/// 普通文件树所需的写权限：不含创建字符/块设备节点。
const WRITABLE_DIR_ACCESS: u64 =
    HANDLED_ACCESS_FS & !(LANDLOCK_ACCESS_FS_MAKE_CHAR | LANDLOCK_ACCESS_FS_MAKE_BLOCK);

#[repr(C)]
struct LandlockRulesetAttr {
    handled_access_fs: u64,
}

#[repr(C, packed)]
struct LandlockPathBeneathAttr {
    allowed_access: u64,
    parent_fd: i32,
}

struct InitArgs {
    ctrl_fd: RawFd,
    status_fd: RawFd,
    rw_paths: Vec<OsString>,
    envp: Vec<OsString>,
    stages: Vec<Vec<OsString>>,
}

fn main() {
    let mut raw_args = std::env::args_os();
    let _prog = raw_args.next();
    let args: Vec<OsString> = raw_args.collect();

    if let Some(first) = args.first().and_then(|s| s.to_str()) {
        match first {
            "--cgroup-probe" => run_cgroup_probe(&args[1..]),
            "--probe-sandbox" => run_sandbox_probe(),
            _ => {}
        }
    }

    let parsed = match parse_init_args(&args) {
        Ok(p) => p,
        Err(msg) => {
            eprintln!("lancet-init 参数错误：{msg}");
            exit_now(126);
        }
    };

    if let Err(err) = set_cloexec(parsed.ctrl_fd) {
        report_init_error(
            parsed.status_fd,
            &format!("设置控制 fd CLOEXEC 失败：{err}"),
        );
    }
    if let Err(err) = set_cloexec(parsed.status_fd) {
        report_init_error(
            parsed.status_fd,
            &format!("设置状态 fd CLOEXEC 失败：{err}"),
        );
    }

    if let Err(msg) = apply_landlock(&parsed.rw_paths) {
        report_init_error(parsed.status_fd, &msg);
    }

    run_foreground(parsed);
}

/// 启动期 cgroup 探针：通知就绪后监听节点持有的管道，读到 EOF 时立即退出，供 `cgroup.kill` 校验。
fn run_cgroup_probe(rest: &[OsString]) -> ! {
    let (Some(ready_str), Some(ctrl_str)) = (
        rest.first().and_then(|s| s.to_str()),
        rest.get(1).and_then(|s| s.to_str()),
    ) else {
        eprintln!("lancet-init --cgroup-probe 缺少就绪 fd 或控制 fd");
        exit_now(2);
    };
    let Ok(ready_fd) = ready_str.parse::<RawFd>() else {
        eprintln!("lancet-init --cgroup-probe 无效就绪 fd：{ready_str}");
        exit_now(2);
    };
    let Ok(ctrl_fd) = ctrl_str.parse::<RawFd>() else {
        eprintln!("lancet-init --cgroup-probe 无效控制 fd：{ctrl_str}");
        exit_now(2);
    };
    let byte = [1u8];
    let _ = write_all_raw(ready_fd, &byte);
    // SAFETY: 关闭自己持有的探针通知 fd。
    unsafe {
        libc::close(ready_fd);
    }

    let mut pfd = libc::pollfd {
        fd: ctrl_fd,
        events: libc::POLLIN,
        revents: 0,
    };
    loop {
        pfd.revents = 0;
        // SAFETY: 监听节点持有的管道，节点退出或关闭写端时收到 EOF/POLLHUP 立即退出。
        let rc = unsafe { libc::poll(&mut pfd, 1, -1) };
        if rc < 0 {
            if io::Error::last_os_error().raw_os_error() == Some(libc::EINTR) {
                continue;
            }
            exit_now(0);
        }
        if (pfd.revents & (libc::POLLHUP | libc::POLLERR | libc::POLLNVAL)) != 0 {
            exit_now(0);
        }
        if (pfd.revents & libc::POLLIN) != 0 {
            let mut buf = [0u8; 64];
            // SAFETY: 读取控制管道检测 EOF。
            let n = unsafe { libc::read(ctrl_fd, buf.as_mut_ptr().cast(), buf.len()) };
            if n <= 0 {
                exit_now(0);
            }
        }
    }
}

/// 启动期沙箱冒烟探针：确认 bwrap 与垫片能施加 Landlock 并启动目标进程。
fn run_sandbox_probe() -> ! {
    exit_now(0);
}

fn parse_init_args(args: &[OsString]) -> Result<InitArgs, String> {
    let mut ctrl_fd: Option<RawFd> = None;
    let mut status_fd: Option<RawFd> = None;
    let mut rw_paths = Vec::new();
    let mut envp = Vec::new();
    let mut idx = 0usize;

    while idx < args.len() {
        let flag = args[idx].to_str().unwrap_or("");
        match flag {
            "--ctrl-fd" => {
                idx += 1;
                let val = args
                    .get(idx)
                    .and_then(|s| s.to_str())
                    .ok_or_else(|| "缺少 --ctrl-fd 值".to_owned())?;
                ctrl_fd = Some(
                    val.parse::<RawFd>()
                        .map_err(|e| format!("无效 --ctrl-fd `{val}`：{e}"))?,
                );
            }
            "--status-fd" => {
                idx += 1;
                let val = args
                    .get(idx)
                    .and_then(|s| s.to_str())
                    .ok_or_else(|| "缺少 --status-fd 值".to_owned())?;
                status_fd = Some(
                    val.parse::<RawFd>()
                        .map_err(|e| format!("无效 --status-fd `{val}`：{e}"))?,
                );
            }
            "--rw" => {
                idx += 1;
                let val = args.get(idx).ok_or_else(|| "缺少 --rw 值".to_owned())?;
                rw_paths.push(val.clone());
            }
            "--env" => {
                idx += 1;
                let val = args.get(idx).ok_or_else(|| "缺少 --env 值".to_owned())?;
                envp.push(val.clone());
            }
            "--" => {
                idx += 1;
                break;
            }
            other => return Err(format!("未知选项 `{other}`")),
        }
        idx += 1;
    }

    let ctrl_fd = ctrl_fd.ok_or_else(|| "缺少 --ctrl-fd".to_owned())?;
    let status_fd = status_fd.ok_or_else(|| "缺少 --status-fd".to_owned())?;
    if ctrl_fd < 3 || status_fd < 3 || ctrl_fd == status_fd {
        return Err(format!("无效 fd 编号：ctrl={ctrl_fd} status={status_fd}"));
    }

    let mut stages = Vec::new();
    while idx < args.len() {
        let stage_no = stages.len() + 1;
        let count_str = args[idx]
            .to_str()
            .ok_or_else(|| format!("第 {stage_no} 阶段 argc 不是合法 UTF-8"))?;
        if count_str.is_empty() || !count_str.bytes().all(|b| b.is_ascii_digit()) {
            return Err(format!(
                "第 {stage_no} 阶段 argc `{count_str}` 不是十进制非负整数"
            ));
        }
        let argc = count_str
            .parse::<usize>()
            .map_err(|e| format!("第 {stage_no} 阶段 argc `{count_str}` 无效：{e}"))?;
        if argc == 0 {
            return Err(format!("第 {stage_no} 阶段 argc 不能为 0"));
        }
        idx += 1;
        let end = idx
            .checked_add(argc)
            .ok_or_else(|| format!("第 {stage_no} 阶段 argc 溢出"))?;
        if end > args.len() {
            return Err(format!(
                "第 {stage_no} 阶段参数不完整：声明 {argc} 个，实际剩余 {} 个",
                args.len() - idx
            ));
        }
        stages.push(args[idx..end].to_vec());
        idx = end;
    }
    if stages.is_empty() {
        return Err("缺少目标阶段".to_owned());
    }

    Ok(InitArgs {
        ctrl_fd,
        status_fd,
        rw_paths,
        envp,
        stages,
    })
}

fn apply_landlock(rw_paths: &[OsString]) -> Result<(), String> {
    let attr = LandlockRulesetAttr {
        handled_access_fs: HANDLED_ACCESS_FS,
    };
    // SAFETY: 结构体布局与 Linux UAPI `landlock_ruleset_attr` 对齐。
    let ruleset_fd = unsafe {
        libc::syscall(
            libc::SYS_landlock_create_ruleset,
            &attr as *const LandlockRulesetAttr,
            std::mem::size_of::<LandlockRulesetAttr>(),
            0u32,
        )
    };
    if ruleset_fd < 0 {
        return Err(format!(
            "landlock_create_ruleset 失败：{}",
            io::Error::last_os_error()
        ));
    }
    let ruleset_fd = ruleset_fd as RawFd;

    let res = (|| -> Result<(), String> {
        for path in rw_paths {
            add_landlock_path_rule(ruleset_fd, path, WRITABLE_DIR_ACCESS)?;
        }
        add_landlock_path_rule(
            ruleset_fd,
            &OsString::from("/dev/null"),
            LANDLOCK_ACCESS_FS_WRITE_FILE,
        )?;

        // SAFETY: prctl(PR_SET_NO_NEW_PRIVS, 1) 为 landlock_restrict_self 的前置要求。
        if unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) } != 0 {
            return Err(format!(
                "prctl(PR_SET_NO_NEW_PRIVS) 失败：{}",
                io::Error::last_os_error()
            ));
        }

        // SAFETY: ruleset_fd 有效且已设 NO_NEW_PRIVS。
        if unsafe { libc::syscall(libc::SYS_landlock_restrict_self, ruleset_fd, 0u32) } != 0 {
            return Err(format!(
                "landlock_restrict_self 失败：{}",
                io::Error::last_os_error()
            ));
        }
        Ok(())
    })();

    // SAFETY: 关闭规则集 fd。
    unsafe {
        libc::close(ruleset_fd);
    }
    res
}

fn add_landlock_path_rule(
    ruleset_fd: RawFd,
    path: &OsString,
    allowed_access: u64,
) -> Result<(), String> {
    let c_path = CString::new(path.as_bytes())
        .map_err(|_| format!("Landlock 路径含 NUL 字节：{}", path.to_string_lossy()))?;
    // SAFETY: 使用 O_PATH | O_CLOEXEC 打开已挂载路径。
    let fd = unsafe { libc::open(c_path.as_ptr(), libc::O_PATH | libc::O_CLOEXEC) };
    if fd < 0 {
        return Err(format!(
            "打开 Landlock 规则路径 `{}` 失败：{}",
            path.to_string_lossy(),
            io::Error::last_os_error()
        ));
    }
    let rule = LandlockPathBeneathAttr {
        allowed_access,
        parent_fd: fd,
    };
    // SAFETY: ruleset_fd 与 fd 有效，结构体 packed 布局与 Linux UAPI 一致。
    let rc = unsafe {
        libc::syscall(
            libc::SYS_landlock_add_rule,
            ruleset_fd,
            LANDLOCK_RULE_PATH_BENEATH,
            &rule as *const LandlockPathBeneathAttr,
            0u32,
        )
    };
    let err = if rc < 0 {
        Some(io::Error::last_os_error())
    } else {
        None
    };
    // SAFETY: 立即关闭路径 fd。
    unsafe {
        libc::close(fd);
    }
    if let Some(err) = err {
        return Err(format!(
            "为路径 `{}` 添加 Landlock 规则失败：{err}",
            path.to_string_lossy()
        ));
    }
    Ok(())
}

struct PreparedStage {
    argv_c: Vec<CString>,
    exec_candidates: Vec<CString>,
    is_direct_path: bool,
}

fn run_foreground(parsed: InitArgs) -> ! {
    // 预先在 fork 之前构造全部阶段的 CString 与候选路径，确保子进程只执行 async-signal-safe 系统调用。
    let envp_c = match to_cstrings(&parsed.envp) {
        Ok(v) => v,
        Err(msg) => report_init_error(parsed.status_fd, &format!("构造 envp 失败：{msg}")),
    };
    let mut envp_ptrs: Vec<*const libc::c_char> = envp_c.iter().map(|s| s.as_ptr()).collect();
    envp_ptrs.push(ptr::null());

    let mut prepared_stages = Vec::with_capacity(parsed.stages.len());
    for (idx, stage_argv) in parsed.stages.iter().enumerate() {
        let stage_no = idx + 1;
        let argv_c = match to_cstrings(stage_argv) {
            Ok(v) => v,
            Err(msg) => report_init_error(
                parsed.status_fd,
                &format!("构造第 {stage_no} 阶段 argv 失败：{msg}"),
            ),
        };
        let (exec_candidates, is_direct_path) =
            match build_exec_candidates(&stage_argv[0], &parsed.envp) {
                Ok(v) => v,
                Err(msg) => report_init_error(
                    parsed.status_fd,
                    &format!("构造第 {stage_no} 阶段可执行路径失败：{msg}"),
                ),
            };
        prepared_stages.push(PreparedStage {
            argv_c,
            exec_candidates,
            is_direct_path,
        });
    }

    // 阻塞 SIGCHLD 并建立 signalfd，避免 fork 后子进程秒退丢失信号。
    let mut sig_mask: libc::sigset_t = unsafe { std::mem::zeroed() };
    let mut orig_mask: libc::sigset_t = unsafe { std::mem::zeroed() };
    // SAFETY: 初始化信号集并阻塞 SIGCHLD。
    unsafe {
        libc::sigemptyset(&mut sig_mask);
        libc::sigaddset(&mut sig_mask, libc::SIGCHLD);
        if libc::sigprocmask(libc::SIG_BLOCK, &sig_mask, &mut orig_mask) != 0 {
            report_init_error(
                parsed.status_fd,
                &format!("阻塞 SIGCHLD 失败：{}", io::Error::last_os_error()),
            );
        }
    }

    // SAFETY: signalfd 监听已阻塞的 SIGCHLD。
    let sig_fd = unsafe { libc::signalfd(-1, &sig_mask, libc::SFD_NONBLOCK | libc::SFD_CLOEXEC) };
    if sig_fd < 0 {
        report_init_error(
            parsed.status_fd,
            &format!("创建 signalfd 失败：{}", io::Error::last_os_error()),
        );
    }

    let stage_count = prepared_stages.len();
    let mut stage_pids = Vec::with_capacity(stage_count);
    let mut prev_read_fd: Option<RawFd> = None;
    let mut reported_started = false;

    for (stage_idx, stage) in prepared_stages.iter().enumerate() {
        let stage_no = stage_idx + 1;
        let has_next = stage_idx + 1 < stage_count;

        let mut argv_ptrs: Vec<*const libc::c_char> =
            stage.argv_c.iter().map(|s| s.as_ptr()).collect();
        argv_ptrs.push(ptr::null());

        let (next_read_fd, next_write_fd) = if has_next {
            let mut data_pipe = [0 as RawFd; 2];
            // SAFETY: 相邻阶段间滚动建立带 O_CLOEXEC 的数据管道。
            if unsafe { libc::pipe2(data_pipe.as_mut_ptr(), libc::O_CLOEXEC) } != 0 {
                report_init_error(
                    parsed.status_fd,
                    &format!(
                        "创建第 {stage_no} 阶段输出管道失败：{}",
                        io::Error::last_os_error()
                    ),
                );
            }
            (Some(data_pipe[0]), Some(data_pipe[1]))
        } else {
            (None, None)
        };

        let mut exec_pipe = [0 as RawFd; 2];
        // SAFETY: 创建 CLOEXEC 内部管道用于本阶段子进程报告启动结果。
        if unsafe { libc::pipe2(exec_pipe.as_mut_ptr(), libc::O_CLOEXEC) } != 0 {
            report_init_error(
                parsed.status_fd,
                &format!(
                    "创建第 {stage_no} 阶段 exec 状态管道失败：{}",
                    io::Error::last_os_error()
                ),
            );
        }
        let exec_r = exec_pipe[0];
        let exec_w = exec_pipe[1];

        // SAFETY: 单线程 PID 1，子进程只调用 async-signal-safe 系统调用。
        let child_pid = unsafe { libc::fork() };
        if child_pid < 0 {
            report_init_error(
                parsed.status_fd,
                &format!(
                    "fork 第 {stage_no} 阶段子进程失败：{}",
                    io::Error::last_os_error()
                ),
            );
        }

        if child_pid == 0 {
            // SAFETY: 子进程恢复信号掩码与 SIGPIPE，接好标准流，关闭多余 fd 后直接 execve。
            unsafe {
                if libc::sigprocmask(libc::SIG_SETMASK, &orig_mask, ptr::null_mut()) != 0 {
                    report_child_pipe_err(exec_w, b'C', last_errno(), 126);
                }
                if libc::signal(libc::SIGPIPE, libc::SIG_DFL) == libc::SIG_ERR {
                    report_child_pipe_err(exec_w, b'C', last_errno(), 126);
                }
                if let Some(in_fd) = prev_read_fd
                    && let Err(err_no) = dup2_retry(in_fd, 0)
                {
                    report_child_pipe_err(exec_w, b'C', err_no, 126);
                }
                if let Some(out_fd) = next_write_fd
                    && let Err(err_no) = dup2_retry(out_fd, 1)
                {
                    report_child_pipe_err(exec_w, b'C', err_no, 126);
                }
                if let Err(err_no) = close_all_fds_except(exec_w) {
                    report_child_pipe_err(exec_w, b'C', err_no, 126);
                }

                let err_no = exec_candidates_now(
                    &stage.exec_candidates,
                    stage.is_direct_path,
                    argv_ptrs.as_ptr(),
                    envp_ptrs.as_ptr(),
                );
                report_child_pipe_err(exec_w, b'E', err_no, 127);
            }
        }

        // 父进程（PID 1）：立即关闭上一段读端、本段数据写端与报告写端，仅保留下一段读端。
        // SAFETY: 这些 fd 均归当前进程持有且不再使用。
        unsafe {
            libc::close(exec_w);
            if let Some(old_r) = prev_read_fd.take() {
                libc::close(old_r);
            }
            if let Some(out_w) = next_write_fd {
                libc::close(out_w);
            }
        }
        prev_read_fd = next_read_fd;

        let exec_res = wait_stage_exec(exec_r, parsed.ctrl_fd);
        // SAFETY: 读完关闭本段 exec_r。
        unsafe {
            libc::close(exec_r);
        }

        let (err_buf, read_len) = match exec_res {
            Ok(v) => v,
            Err(err) => report_init_error(
                parsed.status_fd,
                &format!("等待第 {stage_no} 阶段启动结果失败：{err}"),
            ),
        };

        if read_len > 0 {
            if read_len == 5 && err_buf[0] == b'E' {
                let mut msg = [0u8; 9];
                msg[0] = b'E';
                msg[1..5].copy_from_slice(&(stage_no as u32).to_le_bytes());
                msg[5..9].copy_from_slice(&err_buf[1..5]);
                let _ = write_all_raw(parsed.status_fd, &msg);
                // SAFETY: 关闭状态 fd 后退出（内核随之终止命名空间内其余进程）。
                unsafe {
                    libc::close(parsed.status_fd);
                }
                exit_now(127);
            }
            if read_len == 5 && err_buf[0] == b'C' {
                let err_no = i32::from_le_bytes([err_buf[1], err_buf[2], err_buf[3], err_buf[4]]);
                let os_err = io::Error::from_raw_os_error(err_no);
                report_init_error(
                    parsed.status_fd,
                    &format!(
                        "第 {stage_no} 阶段子进程准备失败（信号、标准流或关闭多余 fd）：{os_err}"
                    ),
                );
            }
            report_init_error(
                parsed.status_fd,
                &format!("读取第 {stage_no} 阶段子进程启动状态失败"),
            );
        }

        // 0 字节 EOF：本阶段已成功 execve；首段确认后立即向节点发送 O。
        stage_pids.push(child_pid);
        if !reported_started {
            let _ = write_all_raw(parsed.status_fd, b"O");
            reported_started = true;
        }
    }

    // 状态 fd 保持打开到 PID 1 退出，垫片自身在回收期间的故障仍以 `I` 报告。
    let mut stage_codes = vec![None; stage_pids.len()];

    // 先非阻塞检查一次，防止已有前台阶段在启动后续阶段期间退出。
    if let Some(code) = reap_children(&stage_pids, &mut stage_codes) {
        exit_now(code);
    }

    let mut pollfds = [
        libc::pollfd {
            fd: parsed.ctrl_fd,
            events: libc::POLLIN,
            revents: 0,
        },
        libc::pollfd {
            fd: sig_fd,
            events: libc::POLLIN,
            revents: 0,
        },
    ];

    loop {
        pollfds[0].revents = 0;
        pollfds[1].revents = 0;
        // SAFETY: 轮询控制管道与 signalfd。
        let rc = unsafe { libc::poll(pollfds.as_mut_ptr(), pollfds.len() as libc::nfds_t, -1) };
        if rc < 0 {
            let err = io::Error::last_os_error();
            if err.raw_os_error() == Some(libc::EINTR) {
                continue;
            }
            report_init_error(
                parsed.status_fd,
                &format!("回收前台阶段时轮询控制管道与 signalfd 失败：{err}"),
            );
        }

        if pollfds[1].revents != 0 {
            drain_signalfd(sig_fd);
            if let Some(code) = reap_children(&stage_pids, &mut stage_codes) {
                exit_now(code);
            }
        }

        if pollfds[0].revents != 0 {
            if (pollfds[0].revents & (libc::POLLHUP | libc::POLLERR | libc::POLLNVAL)) != 0 {
                exit_now(1);
            }
            if (pollfds[0].revents & libc::POLLIN) != 0 {
                let mut tmp = [0u8; 64];
                // SAFETY: 读取控制管道检测 EOF。
                let n = unsafe { libc::read(parsed.ctrl_fd, tmp.as_mut_ptr().cast(), tmp.len()) };
                if n <= 0 {
                    exit_now(1);
                }
            }
        }
    }
}

/// 在等待单阶段 `execve` 结果期间同时监听控制管道；控制管道 EOF 时立即退出 PID 1。
fn wait_stage_exec(exec_r: RawFd, ctrl_fd: RawFd) -> io::Result<([u8; 5], usize)> {
    let mut err_buf = [0u8; 5];
    let mut read_len = 0usize;
    let mut pollfds = [
        libc::pollfd {
            fd: ctrl_fd,
            events: libc::POLLIN,
            revents: 0,
        },
        libc::pollfd {
            fd: exec_r,
            events: libc::POLLIN,
            revents: 0,
        },
    ];

    while read_len < err_buf.len() {
        pollfds[0].revents = 0;
        pollfds[1].revents = 0;
        // SAFETY: 轮询控制管道与本段启动报告管道。
        let rc = unsafe { libc::poll(pollfds.as_mut_ptr(), pollfds.len() as libc::nfds_t, -1) };
        if rc < 0 {
            let err = io::Error::last_os_error();
            if err.raw_os_error() == Some(libc::EINTR) {
                continue;
            }
            return Err(err);
        }

        if pollfds[0].revents != 0 {
            if (pollfds[0].revents & (libc::POLLHUP | libc::POLLERR | libc::POLLNVAL)) != 0 {
                exit_now(1);
            }
            if (pollfds[0].revents & libc::POLLIN) != 0 {
                let mut tmp = [0u8; 64];
                // SAFETY: 读取控制管道检测 EOF。
                let n = unsafe { libc::read(ctrl_fd, tmp.as_mut_ptr().cast(), tmp.len()) };
                if n <= 0 {
                    exit_now(1);
                }
            }
        }

        if pollfds[1].revents != 0 {
            if (pollfds[1].revents & libc::POLLNVAL) != 0 {
                return Err(io::Error::from_raw_os_error(libc::EBADF));
            }
            // SAFETY: 读取本段子进程可能写回的 5 字节错误消息（'E' + errno 或 'C' + errno）或 EOF。
            let n = unsafe {
                libc::read(
                    exec_r,
                    err_buf[read_len..].as_mut_ptr().cast(),
                    err_buf.len() - read_len,
                )
            };
            if n > 0 {
                read_len += n as usize;
            } else if n == 0 {
                break;
            } else {
                let err = io::Error::last_os_error();
                if err.raw_os_error() == Some(libc::EINTR) {
                    continue;
                }
                return Err(err);
            }
        }
    }

    Ok((err_buf, read_len))
}

fn last_errno() -> i32 {
    io::Error::last_os_error()
        .raw_os_error()
        .unwrap_or(libc::EINVAL)
}

unsafe fn dup2_retry(old_fd: RawFd, new_fd: RawFd) -> Result<(), i32> {
    loop {
        // SAFETY: dup2 为 async-signal-safe，old_fd >= 3 != new_fd(0/1) 时自动清除 CLOEXEC。
        if unsafe { libc::dup2(old_fd, new_fd) } >= 0 {
            return Ok(());
        }
        let err_no = last_errno();
        if err_no != libc::EINTR {
            return Err(err_no);
        }
    }
}

unsafe fn report_child_pipe_err(exec_w: RawFd, tag: u8, err_no: i32, exit_code: i32) -> ! {
    let mut msg = [0u8; 5];
    msg[0] = tag;
    msg[1..5].copy_from_slice(&err_no.to_le_bytes());
    let _ = write_all_raw(exec_w, &msg);
    // SAFETY: 写完关闭启动报告写端并立即退出子进程。
    unsafe {
        libc::close(exec_w);
        libc::_exit(exit_code);
    }
}

/// 在子进程中依次尝试候选路径执行 `execve`，全部失败时返回应上报的 errno。
unsafe fn exec_candidates_now(
    candidates: &[CString],
    is_direct_path: bool,
    argv: *const *const libc::c_char,
    envp: *const *const libc::c_char,
) -> i32 {
    if candidates.is_empty() {
        return libc::ENOENT;
    }

    let mut saved_errno = 0i32;
    for candidate in candidates {
        // SAFETY: candidate、argv 与 envp 均为合法的 NUL 结尾 C 字符串指针数组。
        unsafe {
            libc::execve(candidate.as_ptr(), argv, envp);
        }
        let err_no = io::Error::last_os_error()
            .raw_os_error()
            .unwrap_or(libc::ENOEXEC);
        if is_direct_path {
            return err_no;
        }
        match err_no {
            libc::ENOENT | libc::ENOTDIR => continue,
            libc::EACCES | libc::EISDIR | libc::ELOOP | libc::ENAMETOOLONG => {
                if saved_errno == 0 {
                    saved_errno = err_no;
                }
            }
            other => return other,
        }
    }

    if saved_errno != 0 {
        saved_errno
    } else {
        libc::ENOENT
    }
}

unsafe fn close_all_fds_except(keep: RawFd) -> Result<(), i32> {
    if keep > 3 {
        // SAFETY: Linux 5.9+ close_range 系统调用。
        if unsafe { libc::syscall(libc::SYS_close_range, 3u32, (keep - 1) as u32, 0u32) } != 0 {
            return Err(io::Error::last_os_error()
                .raw_os_error()
                .unwrap_or(libc::EINVAL));
        }
    }
    // SAFETY: 关闭 keep 之上的所有 fd。
    if unsafe { libc::syscall(libc::SYS_close_range, (keep + 1) as u32, u32::MAX, 0u32) } != 0 {
        return Err(io::Error::last_os_error()
            .raw_os_error()
            .unwrap_or(libc::EINVAL));
    }
    Ok(())
}

fn drain_signalfd(sig_fd: RawFd) {
    let mut info: libc::signalfd_siginfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of::<libc::signalfd_siginfo>();
    loop {
        // SAFETY: 非阻塞读取 signalfd_siginfo。
        let n = unsafe {
            libc::read(
                sig_fd,
                (&mut info as *mut libc::signalfd_siginfo).cast(),
                size,
            )
        };
        if n <= 0 {
            break;
        }
    }
}

/// 回收全部已退出子进程（含收养的后代）；当全部前台阶段都已退出时，返回 pipefail 退出码。
fn reap_children(stage_pids: &[libc::pid_t], stage_codes: &mut [Option<i32>]) -> Option<i32> {
    loop {
        let mut status: libc::c_int = 0;
        // SAFETY: 非阻塞回收任意子进程。
        let pid = unsafe { libc::waitpid(-1, &mut status, libc::WNOHANG) };
        if pid <= 0 {
            break;
        }
        if let Some(idx) = stage_pids.iter().position(|&p| p == pid)
            && stage_codes[idx].is_none()
        {
            stage_codes[idx] = Some(wait_status_to_exit_code(status));
        }
    }
    if stage_codes.iter().all(Option::is_some) {
        Some(pipefail_exit_code(stage_codes))
    } else {
        None
    }
}

/// pipefail 规则：从右向左取第一个非零码，全为 0 时取 0。
fn pipefail_exit_code(stage_codes: &[Option<i32>]) -> i32 {
    for code in stage_codes.iter().rev().flatten() {
        if *code != 0 {
            return *code;
        }
    }
    0
}

fn wait_status_to_exit_code(status: libc::c_int) -> i32 {
    if libc::WIFEXITED(status) {
        libc::WEXITSTATUS(status)
    } else if libc::WIFSIGNALED(status) {
        128 + libc::WTERMSIG(status)
    } else {
        1
    }
}

fn to_cstrings(items: &[OsString]) -> Result<Vec<CString>, String> {
    let mut out = Vec::with_capacity(items.len());
    for item in items {
        let c = CString::new(item.as_bytes())
            .map_err(|_| format!("参数包含非法 NUL 字节：{}", item.to_string_lossy()))?;
        out.push(c);
    }
    Ok(out)
}

fn build_exec_candidates(
    argv0: &OsString,
    envp: &[OsString],
) -> Result<(Vec<CString>, bool), String> {
    let bytes = argv0.as_bytes();
    if bytes.is_empty() {
        return Ok((Vec::new(), false));
    }
    if bytes.contains(&b'/') {
        let c = CString::new(bytes).map_err(|_| "argv[0] 包含非法 NUL 字节".to_owned())?;
        return Ok((vec![c], true));
    }

    let path_val = envp
        .iter()
        .find_map(|entry| entry.as_bytes().strip_prefix(b"PATH="))
        .unwrap_or(b"");

    let mut candidates = Vec::new();
    for dir in path_val.split(|&b| b == b':') {
        let mut joined = Vec::with_capacity(dir.len() + 1 + bytes.len());
        if dir.is_empty() {
            joined.push(b'.');
        } else {
            joined.extend_from_slice(dir);
        }
        if !joined.ends_with(b"/") {
            joined.push(b'/');
        }
        joined.extend_from_slice(bytes);
        if let Ok(c) = CString::new(joined) {
            candidates.push(c);
        }
    }
    Ok((candidates, false))
}

fn set_cloexec(fd: RawFd) -> io::Result<()> {
    // SAFETY: 读取并设置 CLOEXEC 标志。
    unsafe {
        let flags = libc::fcntl(fd, libc::F_GETFD);
        if flags < 0 {
            return Err(io::Error::last_os_error());
        }
        if libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC) < 0 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}

fn write_all_raw(fd: RawFd, mut buf: &[u8]) -> io::Result<()> {
    while !buf.is_empty() {
        // SAFETY: 写入调用方提供的有效切片。
        let n = unsafe { libc::write(fd, buf.as_ptr().cast(), buf.len()) };
        if n > 0 {
            buf = &buf[n as usize..];
        } else if n < 0 {
            let err = io::Error::last_os_error();
            if err.raw_os_error() == Some(libc::EINTR) {
                continue;
            }
            return Err(err);
        } else {
            return Err(io::Error::new(io::ErrorKind::WriteZero, "写入返回 0"));
        }
    }
    Ok(())
}

fn report_init_error(status_fd: RawFd, msg: &str) -> ! {
    let mut payload = Vec::with_capacity(1 + msg.len());
    payload.push(b'I');
    payload.extend_from_slice(msg.as_bytes());
    let _ = write_all_raw(status_fd, &payload);
    // SAFETY: 关闭状态 fd 后立即退出。
    unsafe {
        libc::close(status_fd);
    }
    exit_now(126);
}

fn exit_now(code: i32) -> ! {
    // SAFETY: 立即终止进程（PID 1 退出时内核终止同命名空间内全部剩余进程）。
    unsafe {
        libc::_exit(code);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn os_args(items: &[&str]) -> Vec<OsString> {
        items.iter().map(OsString::from).collect()
    }

    #[test]
    fn parse_init_args_decodes_single_and_multi_stage_preserving_special_tokens() {
        let args = os_args(&[
            "--ctrl-fd",
            "3",
            "--status-fd",
            "4",
            "--rw",
            "/tmp",
            "--env",
            "PATH=/usr/bin",
            "--",
            "5",
            "printf",
            "%s",
            "--",
            "",
            "2",
            "1",
            "cat",
        ]);
        let parsed = parse_init_args(&args).expect("合法多阶段参数应解析成功");
        assert_eq!(parsed.ctrl_fd, 3);
        assert_eq!(parsed.status_fd, 4);
        assert_eq!(parsed.rw_paths, os_args(&["/tmp"]));
        assert_eq!(parsed.envp, os_args(&["PATH=/usr/bin"]));
        assert_eq!(
            parsed.stages,
            vec![os_args(&["printf", "%s", "--", "", "2"]), os_args(&["cat"]),]
        );
    }

    #[test]
    fn parse_init_args_rejects_empty_zero_and_truncated_stages() {
        for (label, raw) in [
            ("无阶段", &["--ctrl-fd", "3", "--status-fd", "4", "--"][..]),
            ("缺少 --", &["--ctrl-fd", "3", "--status-fd", "4"][..]),
            (
                "零计数",
                &["--ctrl-fd", "3", "--status-fd", "4", "--", "0"][..],
            ),
            (
                "非数字计数",
                &["--ctrl-fd", "3", "--status-fd", "4", "--", "+1", "true"][..],
            ),
            (
                "首段不完整",
                &["--ctrl-fd", "3", "--status-fd", "4", "--", "2", "echo"][..],
            ),
            (
                "后段不完整",
                &[
                    "--ctrl-fd",
                    "3",
                    "--status-fd",
                    "4",
                    "--",
                    "1",
                    "true",
                    "2",
                    "cat",
                ][..],
            ),
            (
                "后段零计数",
                &["--ctrl-fd", "3", "--status-fd", "4", "--", "1", "true", "0"][..],
            ),
        ] {
            assert!(
                parse_init_args(&os_args(raw)).is_err(),
                "用例 `{label}` 应被拒绝"
            );
        }
    }

    #[test]
    fn pipefail_picks_rightmost_nonzero_or_zero() {
        assert_eq!(pipefail_exit_code(&[Some(0), Some(0), Some(0)]), 0);
        assert_eq!(pipefail_exit_code(&[Some(7), Some(23), Some(0)]), 23);
        assert_eq!(pipefail_exit_code(&[Some(141), Some(0)]), 141);
        assert_eq!(pipefail_exit_code(&[Some(7), Some(0), Some(5)]), 5);
    }
}
