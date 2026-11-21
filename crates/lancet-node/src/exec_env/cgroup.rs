// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! cgroup v2 委派根管理与每次调用的叶组生命周期。

use std::fs;
use std::io;
use std::os::fd::{AsFd as _, AsRawFd as _, BorrowedFd, FromRawFd as _, OwnedFd, RawFd};
use std::os::unix::process::{CommandExt as _, ExitStatusExt as _};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use anyhow::Context as _;
use rustix::fs::{Mode, OFlags};

/// 每次调用叶组固定的内存上限：8 GiB。
pub const MEMORY_MAX_BYTES: u64 = 8_589_934_592;

const CGROUP_MOUNT: &str = "/sys/fs/cgroup";
const SYSTEMD_RUN_HINT: &str =
    "systemd-run --user --scope -p Delegate=yes /绝对路径/lancet-node /绝对路径/node.toml";

/// 节点专用的 cgroup v2 委派根 `D`；节点自身位于 `D/node`，每次调用在 `D/call-<编号>` 下建叶组。
#[derive(Clone)]
pub struct CgroupRoot {
    inner: Arc<CgroupRootInner>,
}

struct CgroupRootInner {
    root_dir: PathBuf,
    next_call_id: AtomicU64,
}

/// 单次 `proc.run` 独占的 cgroup 叶组 `D/call-<编号>`。
pub struct CgroupLeaf {
    path: PathBuf,
    procs_fd: Option<OwnedFd>,
    events_fd: Option<OwnedFd>,
    removed: bool,
}

impl CgroupRoot {
    /// 校验当前进程拥有全新的专用 cgroup v2 委派根，将节点自身迁入新建的 `D/node`、开启 `+memory`，
    /// 并用探针叶组完整验证 `memory.max`、`memory.oom.group`、`cgroup.kill` 与 `cgroup.events`。
    pub fn init(init_path: &Path, env_label: &str) -> anyhow::Result<Self> {
        let res = (|| -> anyhow::Result<Self> {
            let root_dir = discover_delegated_root()?;
            verify_fresh_delegated_root(&root_dir)?;
            move_self_to_node_group(&root_dir)?;
            enable_memory_subtree(&root_dir)?;
            probe_cgroup_leaf(&root_dir, init_path)?;

            Ok(Self {
                inner: Arc::new(CgroupRootInner {
                    root_dir,
                    next_call_id: AtomicU64::new(1),
                }),
            })
        })();
        res.with_context(|| format!("执行环境 `{env_label}` 的 cgroup 委派初始化失败"))
    }

    #[cfg(test)]
    pub(crate) fn for_unit_test() -> Self {
        Self {
            inner: Arc::new(CgroupRootInner {
                root_dir: PathBuf::new(),
                next_call_id: AtomicU64::new(1),
            }),
        }
    }

    /// 返回节点委派根在 `/sys/fs/cgroup` 下的路径。
    pub fn root_dir(&self) -> &Path {
        &self.inner.root_dir
    }

    /// 为一次调用创建独占叶组 `D/call-<编号>`，写入 8 GiB `memory.max` 与 `memory.oom.group = 1`。
    pub fn create_leaf(&self) -> anyhow::Result<CgroupLeaf> {
        let id = self.inner.next_call_id.fetch_add(1, Ordering::Relaxed);
        let path = self.inner.root_dir.join(format!("call-{id}"));
        create_configured_leaf(path)
    }
}

impl CgroupLeaf {
    /// 返回叶组 `cgroup.procs` 的只写 fd（带 `CLOEXEC`），供 `pre_exec` 写入 `0`。
    pub fn procs_fd(&self) -> BorrowedFd<'_> {
        self.procs_fd
            .as_ref()
            .expect("叶组回收前始终持有 cgroup.procs fd")
            .as_fd()
    }

    /// 向叶组的 `cgroup.kill` 写 `1`，终止组内全部进程。
    pub fn kill(&self) -> io::Result<()> {
        let kill_path = self.path.join("cgroup.kill");
        fs::write(&kill_path, "1")
    }

    /// 在统一的 `settle_deadline` 内等待 `cgroup.events` 出现 `populated 0`，
    /// 读取 `memory.events.local` 判断是否 OOM，再删除叶组目录。
    /// 返回 `Ok(true)` 表示该叶组触发了内核 OOM 终止。
    pub fn wait_empty_and_remove(mut self, settle_deadline: Instant) -> anyhow::Result<bool> {
        let events_fd = self
            .events_fd
            .take()
            .expect("叶组回收前始终持有 cgroup.events fd");
        wait_populated_zero(events_fd.as_raw_fd(), &self.path, settle_deadline)?;
        drop(events_fd);
        drop(self.procs_fd.take());

        let oom = read_oom_triggered(&self.path)?;
        remove_leaf_dir(&self.path, settle_deadline)?;
        self.removed = true;
        Ok(oom)
    }
}

impl Drop for CgroupLeaf {
    fn drop(&mut self) {
        if self.removed {
            return;
        }
        drop(self.events_fd.take());
        drop(self.procs_fd.take());
        let _ = fs::remove_dir(&self.path);
        self.removed = true;
    }
}

fn discover_delegated_root() -> anyhow::Result<PathBuf> {
    if !Path::new(CGROUP_MOUNT).join("cgroup.controllers").exists() {
        anyhow::bail!("未在 {CGROUP_MOUNT} 挂载 cgroup v2 统一层级");
    }

    let raw = fs::read_to_string("/proc/self/cgroup").context("无法读取 /proc/self/cgroup")?;
    let rel = raw
        .lines()
        .find_map(|line| line.strip_prefix("0::"))
        .ok_or_else(|| anyhow::anyhow!("/proc/self/cgroup 中未找到 cgroup v2 条目（0::）"))?
        .trim();

    if rel.is_empty() || rel == "/" {
        anyhow::bail!(
            "当前进程位于 cgroup 根组（{rel}），委派根不是本节点专用的；请使用 `{SYSTEMD_RUN_HINT}` 重新启动"
        );
    }

    Ok(Path::new(CGROUP_MOUNT).join(rel.trim_start_matches('/')))
}

fn verify_fresh_delegated_root(root_dir: &Path) -> anyhow::Result<()> {
    let controllers_path = root_dir.join("cgroup.controllers");
    let controllers = fs::read_to_string(&controllers_path).with_context(|| {
        format!(
            "无法读取 {}，请确认进程位于已委派的 cgroup v2 组中",
            controllers_path.display()
        )
    })?;
    if !controllers.split_whitespace().any(|c| c == "memory") {
        anyhow::bail!(
            "{} 缺少 `memory` 控制器（当前：{:?}）；请使用 `{SYSTEMD_RUN_HINT}` 重新启动",
            root_dir.display(),
            controllers.trim()
        );
    }

    let self_pid = std::process::id();
    let procs_path = root_dir.join("cgroup.procs");
    let procs = fs::read_to_string(&procs_path)
        .with_context(|| format!("无法读取 {}", procs_path.display()))?;
    let other_pids: Vec<&str> = procs
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && line.parse::<u32>().ok() != Some(self_pid))
        .collect();
    if !other_pids.is_empty() {
        anyhow::bail!(
            "委派根 {} 不是本节点专用的（cgroup.procs 中包含其他进程 {:?}），不迁移其他进程；请使用 `{SYSTEMD_RUN_HINT}` 重新启动",
            root_dir.display(),
            other_pids
        );
    }

    let mut subdirs = Vec::new();
    let entries = fs::read_dir(root_dir)
        .with_context(|| format!("扫描委派根 {} 失败", root_dir.display()))?;
    for entry in entries {
        let entry = entry.with_context(|| format!("读取委派根 {} 条目失败", root_dir.display()))?;
        let ft = entry
            .file_type()
            .with_context(|| format!("读取委派根 {} 条目类型失败", root_dir.display()))?;
        if ft.is_dir() {
            subdirs.push(entry.file_name().to_string_lossy().into_owned());
        }
    }
    if !subdirs.is_empty() {
        subdirs.sort();
        anyhow::bail!(
            "委派根 {} 不是本节点专用的（已存在子组 {:?}）；请使用 `{SYSTEMD_RUN_HINT}` 重新启动",
            root_dir.display(),
            subdirs
        );
    }

    Ok(())
}

fn move_self_to_node_group(root_dir: &Path) -> anyhow::Result<()> {
    let node_dir = root_dir.join("node");
    if let Err(err) = fs::create_dir(&node_dir) {
        if err.kind() == io::ErrorKind::AlreadyExists {
            anyhow::bail!(
                "委派根 {} 不是本节点专用的（子组 {} 已存在）；请使用 `{SYSTEMD_RUN_HINT}` 重新启动",
                root_dir.display(),
                node_dir.display()
            );
        }
        return Err(err).with_context(|| {
            format!(
                "无法创建节点子组 {}（可能缺少委派权限，请使用 `{SYSTEMD_RUN_HINT}` 重新启动）",
                node_dir.display()
            )
        });
    }

    let node_procs = node_dir.join("cgroup.procs");
    let pid_str = std::process::id().to_string();
    fs::write(&node_procs, &pid_str).with_context(|| {
        format!(
            "无法将节点进程迁入 {}（请确认 cgroup 委派权限）",
            node_procs.display()
        )
    })?;
    Ok(())
}

fn enable_memory_subtree(root_dir: &Path) -> anyhow::Result<()> {
    let subtree_path = root_dir.join("cgroup.subtree_control");
    let current = fs::read_to_string(&subtree_path)
        .with_context(|| format!("无法读取 {}", subtree_path.display()))?;
    if !current.split_whitespace().any(|c| c == "memory") {
        fs::write(&subtree_path, "+memory").with_context(|| {
            format!(
                "向 {} 写入 `+memory` 失败（请确认已获 `memory` 控制器委派且委派根下无其他内部进程）",
                subtree_path.display()
            )
        })?;
    }
    Ok(())
}

fn probe_cgroup_leaf(root_dir: &Path, init_path: &Path) -> anyhow::Result<()> {
    let probe_path = root_dir.join(format!("call-probe-{}", std::process::id()));
    let leaf = create_configured_leaf(probe_path.clone())
        .context("cgroup 启动探针：创建并配置探针叶组失败")?;

    let read_max = fs::read_to_string(probe_path.join("memory.max")).with_context(|| {
        format!(
            "cgroup 启动探针：读回 {}/memory.max 失败",
            probe_path.display()
        )
    })?;
    if read_max.trim() != MEMORY_MAX_BYTES.to_string() {
        anyhow::bail!(
            "cgroup 启动探针：{}/memory.max 读回值 `{}` 与预期 `{MEMORY_MAX_BYTES}` 不符",
            probe_path.display(),
            read_max.trim()
        );
    }

    let read_oom_group =
        fs::read_to_string(probe_path.join("memory.oom.group")).with_context(|| {
            format!(
                "cgroup 启动探针：读回 {}/memory.oom.group 失败",
                probe_path.display()
            )
        })?;
    if read_oom_group.trim() != "1" {
        anyhow::bail!(
            "cgroup 启动探针：{}/memory.oom.group 读回值 `{}` 与预期 `1` 不符",
            probe_path.display(),
            read_oom_group.trim()
        );
    }

    let (ready_r, ready_w) = create_cloexec_pipe().context("cgroup 启动探针：创建通知管道失败")?;
    let (hold_r, hold_w) = create_cloexec_pipe().context("cgroup 启动探针：创建控制管道失败")?;
    let ready_w_raw = ready_w.as_raw_fd();
    let hold_r_raw = hold_r.as_raw_fd();
    let procs_raw = leaf.procs_fd().as_raw_fd();

    let mut cmd = Command::new(init_path);
    cmd.env_clear()
        .arg("--cgroup-probe")
        .arg(ready_w_raw.to_string())
        .arg(hold_r_raw.to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());

    // SAFETY: pre_exec 只调用 async-signal-safe 的 write 与 fcntl。
    unsafe {
        cmd.pre_exec(move || {
            write_zero_to_cgroup_procs(procs_raw)?;
            clear_cloexec(ready_w_raw)?;
            clear_cloexec(hold_r_raw)?;
            Ok(())
        });
    }

    let mut child = cmd
        .spawn()
        .with_context(|| format!("cgroup 启动探针：启动 {} 失败", init_path.display()))?;
    drop(cmd);
    drop(ready_w);
    drop(hold_r);

    let probe_grace = Duration::from_millis(crate::SETTLE_GRACE_MS);
    let ready_res = wait_single_byte(ready_r.as_raw_fd(), probe_grace);
    drop(ready_r);
    if let Err(err) = ready_res {
        let settle_deadline = Instant::now() + probe_grace;
        drop(hold_w);
        let _ = leaf.kill();
        let _ = wait_probe_child(&mut child, settle_deadline);
        let _ = leaf.wait_empty_and_remove(settle_deadline);
        return Err(err).context("cgroup 启动探针：等待探针子进程就绪失败");
    }

    let events_text = match fs::read_to_string(probe_path.join("cgroup.events")) {
        Ok(text) => text,
        Err(err) => {
            let settle_deadline = Instant::now() + probe_grace;
            drop(hold_w);
            let _ = leaf.kill();
            let _ = wait_probe_child(&mut child, settle_deadline);
            let _ = leaf.wait_empty_and_remove(settle_deadline);
            return Err(err).with_context(|| {
                format!(
                    "cgroup 启动探针：读取 {}/cgroup.events 失败",
                    probe_path.display()
                )
            });
        }
    };
    if parse_populated(&events_text) != Some(true) {
        let settle_deadline = Instant::now() + probe_grace;
        drop(hold_w);
        let _ = leaf.kill();
        let _ = wait_probe_child(&mut child, settle_deadline);
        let _ = leaf.wait_empty_and_remove(settle_deadline);
        anyhow::bail!(
            "cgroup 启动探针：探针子进程就绪后 {}/cgroup.events 未显示 populated 1（实际：{:?}）",
            probe_path.display(),
            events_text.trim()
        );
    }

    // 保持 hold_w 打开直到确认探针子进程被 SIGKILL 终止，避免控制管道 EOF 导致正常退出掩盖 cgroup.kill 失效。
    let settle_deadline = Instant::now() + probe_grace;
    if let Err(err) = leaf.kill() {
        drop(hold_w);
        let _ = wait_probe_child(&mut child, settle_deadline);
        let _ = leaf.wait_empty_and_remove(settle_deadline);
        return Err(err).with_context(|| {
            format!(
                "cgroup 启动探针：向 {}/cgroup.kill 写 1 失败",
                probe_path.display()
            )
        });
    }
    let status_res = wait_probe_child(&mut child, settle_deadline);
    drop(hold_w);
    let status = match status_res {
        Ok(st) => st,
        Err(err) => {
            let _ = leaf.wait_empty_and_remove(settle_deadline);
            return Err(err).context("cgroup 启动探针：回收探针子进程失败");
        }
    };
    if status.signal() != Some(libc::SIGKILL) {
        let _ = leaf.wait_empty_and_remove(settle_deadline);
        anyhow::bail!("cgroup 启动探针：期望探针子进程被 SIGKILL 终止，实际退出状态：{status}");
    }
    let _ = leaf
        .wait_empty_and_remove(settle_deadline)
        .context("cgroup 启动探针：等待探针叶组清空并删除失败")?;

    Ok(())
}

fn wait_probe_child(child: &mut Child, settle_deadline: Instant) -> io::Result<ExitStatus> {
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(status);
        }
        let remaining = settle_deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "等待 cgroup 探针子进程退出超时",
            ));
        }
        std::thread::sleep(remaining.min(Duration::from_millis(5)));
    }
}

fn create_configured_leaf(path: PathBuf) -> anyhow::Result<CgroupLeaf> {
    fs::create_dir(&path).with_context(|| format!("创建 cgroup 叶组 {} 失败", path.display()))?;

    let setup = (|| -> anyhow::Result<(OwnedFd, OwnedFd)> {
        let max_path = path.join("memory.max");
        fs::write(&max_path, MEMORY_MAX_BYTES.to_string())
            .with_context(|| format!("写入 {} 失败", max_path.display()))?;

        let oom_group_path = path.join("memory.oom.group");
        fs::write(&oom_group_path, "1")
            .with_context(|| format!("写入 {} 失败", oom_group_path.display()))?;

        let procs_path = path.join("cgroup.procs");
        let procs_fd =
            rustix::fs::open(&procs_path, OFlags::WRONLY | OFlags::CLOEXEC, Mode::empty())
                .with_context(|| format!("打开 {} 失败", procs_path.display()))?;

        let events_path = path.join("cgroup.events");
        let events_fd = rustix::fs::open(
            &events_path,
            OFlags::RDONLY | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .with_context(|| format!("打开 {} 失败", events_path.display()))?;

        Ok((procs_fd, events_fd))
    })();

    match setup {
        Ok((procs_fd, events_fd)) => Ok(CgroupLeaf {
            path,
            procs_fd: Some(procs_fd),
            events_fd: Some(events_fd),
            removed: false,
        }),
        Err(err) => {
            let _ = fs::remove_dir(&path);
            Err(err)
        }
    }
}

fn wait_populated_zero(
    events_fd: RawFd,
    leaf_path: &Path,
    settle_deadline: Instant,
) -> anyhow::Result<()> {
    let mut buf = [0u8; 256];
    loop {
        // SAFETY: 使用 pread 从偏移 0 读取 cgroup.events，避免修改偏移量。
        let n = unsafe { libc::pread(events_fd, buf.as_mut_ptr().cast(), buf.len(), 0) };
        if n < 0 {
            let err = io::Error::last_os_error();
            if err.raw_os_error() == Some(libc::EINTR) {
                continue;
            }
            return Err(err)
                .with_context(|| format!("读取 {}/cgroup.events 失败", leaf_path.display()));
        }
        let text = std::str::from_utf8(&buf[..n as usize]).unwrap_or("");
        if parse_populated(text) == Some(false) {
            return Ok(());
        }
        let remaining = settle_deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            anyhow::bail!(
                "等待 cgroup 叶组 {} 清空（populated 0）超过结算时限，当前事件：{:?}",
                leaf_path.display(),
                text.trim()
            );
        }
        let wait_ms = remaining.as_millis().clamp(1, 10) as libc::c_int;
        let mut pfd = libc::pollfd {
            fd: events_fd,
            events: libc::POLLPRI | libc::POLLERR,
            revents: 0,
        };
        // SAFETY: 等待 cgroup.events 状态变化通知，单次最多等 10 ms。
        let _ = unsafe { libc::poll(&mut pfd, 1, wait_ms) };
    }
}

fn parse_populated(events_text: &str) -> Option<bool> {
    for line in events_text.lines() {
        let mut parts = line.split_whitespace();
        if parts.next() == Some("populated") {
            return match parts.next() {
                Some("0") => Some(false),
                Some("1") => Some(true),
                _ => None,
            };
        }
    }
    None
}

fn read_oom_triggered(leaf_path: &Path) -> anyhow::Result<bool> {
    let events_path = leaf_path.join("memory.events.local");
    let text = fs::read_to_string(&events_path)
        .with_context(|| format!("读取 {} 失败", events_path.display()))?;
    for line in text.lines() {
        let mut parts = line.split_whitespace();
        let Some(key) = parts.next() else {
            continue;
        };
        if key == "oom_kill" || key == "oom_group_kill" {
            let count = parts
                .next()
                .and_then(|v| v.parse::<u64>().ok())
                .unwrap_or(0);
            if count > 0 {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

fn remove_leaf_dir(path: &Path, settle_deadline: Instant) -> anyhow::Result<()> {
    loop {
        match fs::remove_dir(path) {
            Ok(()) => return Ok(()),
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(err) if err.raw_os_error() == Some(libc::EBUSY) => {
                let remaining = settle_deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    return Err(err).with_context(|| {
                        format!("删除 cgroup 叶组 {} 超过结算时限", path.display())
                    });
                }
                std::thread::sleep(remaining.min(Duration::from_millis(5)));
            }
            Err(err) => {
                return Err(err)
                    .with_context(|| format!("删除 cgroup 叶组 {} 失败", path.display()));
            }
        }
    }
}

/// 在 `pre_exec` 中向叶组 `cgroup.procs` 写入 `"0"`，只使用 async-signal-safe 的 `write`。
pub(crate) unsafe fn write_zero_to_cgroup_procs(cgroup_procs_fd: RawFd) -> io::Result<()> {
    let zero = b"0";
    loop {
        // SAFETY: 写入单个 ASCII 字符 '0'，async-signal-safe。
        let n = unsafe { libc::write(cgroup_procs_fd, zero.as_ptr().cast(), zero.len()) };
        if n == 1 {
            return Ok(());
        }
        let err = io::Error::last_os_error();
        if err.raw_os_error() == Some(libc::EINTR) {
            continue;
        }
        return Err(err);
    }
}

/// 在 `pre_exec` 中清除指定 fd 的 `FD_CLOEXEC`，只使用 async-signal-safe 的 `fcntl`。
pub(crate) unsafe fn clear_cloexec(fd: RawFd) -> io::Result<()> {
    // SAFETY: fcntl(F_GETFD / F_SETFD) 为 async-signal-safe。
    unsafe {
        let flags = libc::fcntl(fd, libc::F_GETFD);
        if flags < 0 {
            return Err(io::Error::last_os_error());
        }
        if libc::fcntl(fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) < 0 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}

/// 创建带 `O_CLOEXEC` 的管道。
pub(crate) fn create_cloexec_pipe() -> io::Result<(OwnedFd, OwnedFd)> {
    let mut fds = [0 as RawFd; 2];
    // SAFETY: pipe2 创建两个新 fd，均设置 O_CLOEXEC。
    if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: pipe2 成功返回两个全新有效 fd。
    let r = unsafe { OwnedFd::from_raw_fd(fds[0]) };
    let w = unsafe { OwnedFd::from_raw_fd(fds[1]) };
    Ok((r, w))
}

fn wait_single_byte(fd: RawFd, timeout: Duration) -> anyhow::Result<()> {
    let deadline = Instant::now() + timeout;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            anyhow::bail!("等待探针通知超时");
        }
        let wait_ms = remaining.as_millis().min(50) as libc::c_int;
        let mut pfd = libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: 轮询可读事件。
        let rc = unsafe { libc::poll(&mut pfd, 1, wait_ms) };
        if rc < 0 {
            let err = io::Error::last_os_error();
            if err.raw_os_error() == Some(libc::EINTR) {
                continue;
            }
            return Err(err.into());
        }
        if rc == 0 {
            continue;
        }
        let mut byte = [0u8; 1];
        // SAFETY: 读取 1 字节就绪信号。
        let n = unsafe { libc::read(fd, byte.as_mut_ptr().cast(), 1) };
        if n == 1 {
            return Ok(());
        }
        if n == 0 {
            anyhow::bail!("探针子进程在发送就绪信号前退出");
        }
        let err = io::Error::last_os_error();
        if err.raw_os_error() == Some(libc::EINTR) {
            continue;
        }
        return Err(err.into());
    }
}
