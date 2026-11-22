// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 执行环境宿主资源：每次调用新建的 bubblewrap 沙箱、Landlock、cgroup 叶组、
//! 私有家目录与缓存挂载、受控环境变量、双路有界输出与中断回收。

pub mod cache;
pub mod cgroup;

use std::collections::{BTreeMap, VecDeque};
use std::ffi::OsStr;
use std::fs::{self, DirBuilder};
use std::io;
use std::os::fd::{AsFd, AsRawFd as _, BorrowedFd, OwnedFd, RawFd};
use std::os::unix::fs::{DirBuilderExt as _, PermissionsExt as _};
use std::os::unix::process::{CommandExt as _, ExitStatusExt as _};
use std::path::{Component, Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::Context as _;
use rustix::fs::{
    Access, CWD, FileType, Mode, OFlags, ResolveFlags, access, fstat, mkdirat, openat, openat2,
};
use rustix::io::Errno;

use crate::dispatch::MAX_TOOL_RESULT_BYTES;
use crate::read_dir::HostDir;

use self::cgroup::{
    CgroupRoot, MEMORY_MAX_BYTES, clear_cloexec, create_cloexec_pipe, write_zero_to_cgroup_procs,
};

/// bubblewrap 固定绝对路径。
pub const BWRAP_PATH: &str = "/usr/bin/bwrap";

/// 基础默认 `PATH` 前缀。
const BASE_PATH: &str = "/usr/local/bin:/usr/bin:/bin";

/// 宿主单次 `poll` 等待上限（毫秒）。
const POLL_STEP_MS: i32 = 10;

/// 每轮 `poll` 对单路输出最多读取的字节数（64 KiB），读完即回到 `poll` 检查另一路与取消/截止。
const PIPE_READ_CHUNK_BYTES: usize = 64 * 1024;

/// 超出返回窗口时每路输出保留的最大尾部字节数（4 KiB）。
const OVERFLOW_TAIL_BYTES: usize = 4096;

/// 进程运行成功时的原始输出。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcOutput {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

/// 单次执行环境调用的结果分类。
#[derive(Debug)]
pub enum ExecRunOutcome {
    /// 目标进程正常结束（退出码可为非零，交由组件按 `check` 处理）。
    Finished(ProcOutput),
    /// 调用方参数、私有家目录挂载点异常、目标启动失败、超窗或沙箱 OOM（映射为 `ScriptOrToolError`）。
    ToolError(String),
    /// 节点取消标志置位。
    Cancelled,
    /// 调用截止时间到达。
    Deadline,
    /// 节点自身挂载、垫片、cgroup 或回收故障（映射为 `InternalError`）。
    InternalError(String),
}

/// 执行环境已解析的单个工作目录挂载条目。
#[derive(Clone)]
pub struct EnvMount {
    pub slot: u32,
    pub dir: HostDir,
    pub writable: bool,
}

/// 已固定来源 fd 的只读工具挂载条目。
pub struct PinnedTool {
    pub dest: PathBuf,
    pub source_fd: OwnedFd,
    pub is_dir: bool,
}

/// 已就位的私有缓存挂载条目。
pub struct PinnedCache {
    pub dest: PathBuf,
    pub private_dir_fd: OwnedFd,
}

/// 构造 `HostExecEnv` 所需的就绪配置与已固定 fd。
pub struct HostExecEnvSpec {
    pub name: String,
    pub mounts: Vec<EnvMount>,
    pub host_home: PathBuf,
    pub private_home_fd: OwnedFd,
    pub tools: Vec<PinnedTool>,
    pub caches: Vec<PinnedCache>,
    pub extra_path: Vec<PathBuf>,
    pub vars: BTreeMap<String, String>,
    pub cgroup_root: CgroupRoot,
    pub init_fd: Arc<OwnedFd>,
}

/// 绑定到单个 `[[env]]` 授权的执行环境宿主资源。
#[derive(Clone)]
pub struct HostExecEnv {
    inner: Arc<HostExecEnvInner>,
}

struct HostExecEnvInner {
    name: String,
    mounts: Vec<EnvMount>,
    host_home: PathBuf,
    private_home_fd: OwnedFd,
    tools: Vec<PinnedTool>,
    caches: Vec<PinnedCache>,
    envp: Vec<String>,
    cgroup_root: CgroupRoot,
    init_fd: Arc<OwnedFd>,
}

struct MountSpec<'a> {
    dest: &'a Path,
    source_fd: RawFd,
    writable: bool,
    is_dir: bool,
}

/// 定位随包提供的 `lancet-init` 垫片程序（位于当前可执行文件同级目录）。
pub fn locate_init_binary() -> anyhow::Result<PathBuf> {
    let exe = std::env::current_exe().context("无法获取当前可执行文件路径")?;
    let dir = exe
        .parent()
        .ok_or_else(|| anyhow::anyhow!("无法确定当前可执行文件所在目录：{}", exe.display()))?;
    Ok(dir.join("lancet-init"))
}

/// 校验 `/usr/bin/bwrap` 存在且非 setuid，且垫片 `lancet-init` 存在可执行，返回固定后的垫片 fd。
pub fn check_bwrap_and_init(init_path: &Path, env_label: &str) -> anyhow::Result<OwnedFd> {
    let bwrap = Path::new(BWRAP_PATH);
    let bwrap_meta = fs::metadata(bwrap).with_context(|| {
        format!("启动校验失败（执行环境 `{env_label}`）：找不到或无法访问 {BWRAP_PATH}")
    })?;
    if !bwrap_meta.is_file() {
        anyhow::bail!("启动校验失败（执行环境 `{env_label}`）：{BWRAP_PATH} 不是普通文件");
    }
    let bwrap_mode = bwrap_meta.permissions().mode();
    if (bwrap_mode & 0o4000) != 0 {
        anyhow::bail!(
            "启动校验失败（执行环境 `{env_label}`）：{BWRAP_PATH} 设置了 setuid 位（权限 {:04o}），本节点仅使用非 setuid 的用户命名空间模式",
            bwrap_mode & 0o7777
        );
    }
    access(bwrap, Access::EXEC_OK)
        .map_err(io::Error::from)
        .with_context(|| {
            format!("启动校验失败（执行环境 `{env_label}`）：{BWRAP_PATH} 不可执行")
        })?;

    let init_canonical = fs::canonicalize(init_path).with_context(|| {
        format!(
            "启动校验失败（执行环境 `{env_label}`）：找不到垫片程序 {}",
            init_path.display()
        )
    })?;
    let init_meta = fs::metadata(&init_canonical).with_context(|| {
        format!(
            "启动校验失败（执行环境 `{env_label}`）：无法读取垫片程序 {} 元数据",
            init_canonical.display()
        )
    })?;
    if !init_meta.is_file() {
        anyhow::bail!(
            "启动校验失败（执行环境 `{env_label}`）：垫片程序 {} 不是普通文件",
            init_canonical.display()
        );
    }
    access(&init_canonical, Access::EXEC_OK)
        .map_err(io::Error::from)
        .with_context(|| {
            format!(
                "启动校验失败（执行环境 `{env_label}`）：垫片程序 {} 不可执行",
                init_canonical.display()
            )
        })?;

    openat2(
        CWD,
        &init_canonical,
        OFlags::PATH | OFlags::CLOEXEC,
        Mode::empty(),
        ResolveFlags::NO_SYMLINKS,
    )
    .map_err(io::Error::from)
    .with_context(|| {
        format!(
            "启动校验失败（执行环境 `{env_label}`）：打开垫片程序 {} 失败",
            init_canonical.display()
        )
    })
}

/// 使用正式 bwrap 参数启动一次短命冒烟探针，确认 bwrap 与垫片能施加 Landlock 并启动目标进程。
pub fn probe_sandbox(
    cgroup_root: &CgroupRoot,
    init_fd: BorrowedFd<'_>,
    env_label: &str,
) -> anyhow::Result<()> {
    let leaf = cgroup_root
        .create_leaf()
        .with_context(|| format!("执行环境 `{env_label}` 沙箱启动探针：创建探针叶组失败"))?;

    let probe_res = run_sandbox_invocation(
        leaf,
        None,
        &[],
        init_fd.as_raw_fd(),
        Path::new("/tmp"),
        &[PathBuf::from("/tmp")],
        &[format!("PATH={BASE_PATH}")],
        &["/run/lancet-init".to_owned(), "--probe-sandbox".to_owned()],
        Instant::now() + Duration::from_secs(10),
        &AtomicBool::new(false),
    );

    match probe_res {
        (true, ExecRunOutcome::Finished(out)) if out.code == 0 => Ok(()),
        (started, ExecRunOutcome::Finished(out)) => anyhow::bail!(
            "执行环境 `{env_label}` 沙箱启动探针失败（started={started}，退出码={}）：stdout={:?} stderr={:?}",
            out.code,
            out.stdout,
            out.stderr
        ),
        (_, ExecRunOutcome::ToolError(msg)) | (_, ExecRunOutcome::InternalError(msg)) => {
            anyhow::bail!("执行环境 `{env_label}` 沙箱启动探针失败：{msg}")
        }
        (_, ExecRunOutcome::Cancelled) => {
            anyhow::bail!("执行环境 `{env_label}` 沙箱启动探针被意外取消")
        }
        (_, ExecRunOutcome::Deadline) => anyhow::bail!("执行环境 `{env_label}` 沙箱启动探针超时"),
    }
}

/// 创建（如不存在）并打开 `state_dir/envs/<env_name>/home`（权限 `0700`）的根 fd。
pub fn open_or_create_private_home(state_dir: &Path, env_name: &str) -> anyhow::Result<OwnedFd> {
    let envs_dir = state_dir.join("envs");
    ensure_dir_0700(&envs_dir)?;
    let env_dir = envs_dir.join(env_name);
    ensure_dir_0700(&env_dir)?;
    let home_dir = env_dir.join("home");
    ensure_dir_0700(&home_dir)?;

    openat2(
        CWD,
        &home_dir,
        OFlags::PATH | OFlags::DIRECTORY | OFlags::CLOEXEC,
        Mode::empty(),
        ResolveFlags::NO_SYMLINKS,
    )
    .map_err(io::Error::from)
    .with_context(|| {
        format!(
            "执行环境 `{env_name}` 打开私有家目录 {} 失败",
            home_dir.display()
        )
    })
}

/// 固定单个只读工具来源路径（已规范化）的 `O_PATH` fd。
pub fn pin_tool_source(
    dest: PathBuf,
    canonical_src: &Path,
    env_name: &str,
) -> anyhow::Result<PinnedTool> {
    let fd = openat2(
        CWD,
        canonical_src,
        OFlags::PATH | OFlags::CLOEXEC,
        Mode::empty(),
        ResolveFlags::NO_SYMLINKS,
    )
    .map_err(io::Error::from)
    .with_context(|| {
        format!(
            "执行环境 `{env_name}` 固定工具来源 {}（目的地 {}）失败",
            canonical_src.display(),
            dest.display()
        )
    })?;
    let st = fstat(&fd).map_err(io::Error::from).with_context(|| {
        format!(
            "执行环境 `{env_name}` 读取工具来源 {} 元数据失败",
            canonical_src.display()
        )
    })?;
    let ft = FileType::from_raw_mode(st.st_mode);
    let is_dir = match ft {
        FileType::Directory => true,
        FileType::RegularFile => false,
        _ => {
            anyhow::bail!(
                "执行环境 `{env_name}` 工具来源 {} 不是目录或普通文件",
                canonical_src.display()
            );
        }
    };
    Ok(PinnedTool {
        dest,
        source_fd: fd,
        is_dir,
    })
}

impl HostExecEnv {
    /// 由已校验并固定 fd 的规格构造执行环境资源。
    pub fn new(spec: HostExecEnvSpec) -> Self {
        let envp = build_envp(&spec.host_home, &spec.extra_path, &spec.vars);
        Self {
            inner: Arc::new(HostExecEnvInner {
                name: spec.name,
                mounts: spec.mounts,
                host_home: spec.host_home,
                private_home_fd: spec.private_home_fd,
                tools: spec.tools,
                caches: spec.caches,
                envp,
                cgroup_root: spec.cgroup_root,
                init_fd: spec.init_fd,
            }),
        }
    }

    pub fn name(&self) -> &str {
        &self.inner.name
    }

    pub fn mounts(&self) -> &[EnvMount] {
        &self.inner.mounts
    }

    #[cfg(test)]
    pub(crate) fn for_unit_test(name: &str, mounts: Vec<EnvMount>) -> Self {
        let null_fd = openat2(
            CWD,
            "/dev/null",
            OFlags::PATH | OFlags::CLOEXEC,
            Mode::empty(),
            ResolveFlags::empty(),
        )
        .expect("单元测试打开 /dev/null 不应失败");
        let init_fd = Arc::new(
            openat2(
                CWD,
                "/dev/null",
                OFlags::PATH | OFlags::CLOEXEC,
                Mode::empty(),
                ResolveFlags::empty(),
            )
            .expect("单元测试打开 /dev/null 不应失败"),
        );
        Self {
            inner: Arc::new(HostExecEnvInner {
                name: name.to_owned(),
                mounts,
                host_home: PathBuf::from("/nonexistent"),
                private_home_fd: null_fd,
                tools: Vec::new(),
                caches: Vec::new(),
                envp: Vec::new(),
                cgroup_root: CgroupRoot::for_unit_test(),
                init_fd,
            }),
        }
    }

    /// 在每次调用新建的 bubblewrap 沙箱中运行单个进程。
    ///
    /// 返回 `(target_started, outcome)`：当垫片报告目标进程已启动后，`target_started` 为 `true`，
    /// 调用方据此在回执中记录一条 `EffectEntry(env, Exec, [])`。
    pub fn run(
        &self,
        argv: &[String],
        cwd: Option<&str>,
        deadline: Instant,
        cancelled: &AtomicBool,
    ) -> (bool, ExecRunOutcome) {
        if cancelled.load(Ordering::Acquire) {
            return (false, ExecRunOutcome::Cancelled);
        }
        if Instant::now() >= deadline {
            return (false, ExecRunOutcome::Deadline);
        }

        if let Err(msg) = validate_argv(argv) {
            return (false, ExecRunOutcome::ToolError(msg));
        }

        let resolved_cwd = match self.resolve_cwd(cwd) {
            Ok(p) => p,
            Err(msg) => return (false, ExecRunOutcome::ToolError(msg)),
        };

        let mounts = self.sorted_mounts();
        if let Err(err) = self.verify_private_home_mountpoints(&mounts) {
            return match err {
                MountPrepError::Tool(msg) => (false, ExecRunOutcome::ToolError(msg)),
                MountPrepError::Internal(msg) => (false, ExecRunOutcome::InternalError(msg)),
            };
        }

        let leaf = match self.inner.cgroup_root.create_leaf() {
            Ok(leaf) => leaf,
            Err(err) => {
                return (
                    false,
                    ExecRunOutcome::InternalError(format!(
                        "执行环境 `{}` 创建调用 cgroup 叶组失败：{err:#}",
                        self.inner.name
                    )),
                );
            }
        };

        let mut rw_paths =
            Vec::with_capacity(2 + self.inner.mounts.len() + self.inner.caches.len());
        rw_paths.push(PathBuf::from("/tmp"));
        rw_paths.push(self.inner.host_home.clone());
        for m in &self.inner.mounts {
            if m.writable {
                rw_paths.push(m.dir.root_path().to_path_buf());
            }
        }
        for c in &self.inner.caches {
            rw_paths.push(c.dest.clone());
        }

        let (started, outcome) = run_sandbox_invocation(
            leaf,
            Some((
                self.inner.private_home_fd.as_raw_fd(),
                &self.inner.host_home,
            )),
            &mounts,
            self.inner.init_fd.as_raw_fd(),
            &resolved_cwd,
            &rw_paths,
            &self.inner.envp,
            argv,
            deadline,
            cancelled,
        );

        (started, outcome)
    }

    fn resolve_cwd(&self, cwd: Option<&str>) -> Result<PathBuf, String> {
        match cwd {
            None => Ok(self.inner.host_home.clone()),
            Some(name) => self
                .inner
                .mounts
                .iter()
                .find(|m| m.dir.name() == name)
                .map(|m| m.dir.root_path().to_path_buf())
                .ok_or_else(|| format!("工作目录 `{name}` 未挂载进执行环境 `{}`", self.inner.name)),
        }
    }

    /// 收集全部工作目录、工具与缓存挂载，按路径分量数升序（父先子后）排列。
    fn sorted_mounts(&self) -> Vec<MountSpec<'_>> {
        let mut mounts = Vec::with_capacity(
            self.inner.mounts.len() + self.inner.tools.len() + self.inner.caches.len(),
        );
        for m in &self.inner.mounts {
            mounts.push(MountSpec {
                dest: m.dir.root_path(),
                source_fd: m.dir.root_fd().as_raw_fd(),
                writable: m.writable,
                is_dir: true,
            });
        }
        for t in &self.inner.tools {
            mounts.push(MountSpec {
                dest: &t.dest,
                source_fd: t.source_fd.as_raw_fd(),
                writable: false,
                is_dir: t.is_dir,
            });
        }
        for c in &self.inner.caches {
            mounts.push(MountSpec {
                dest: &c.dest,
                source_fd: c.private_dir_fd.as_raw_fd(),
                writable: true,
                is_dir: true,
            });
        }
        mounts.sort_by(|a, b| {
            a.dest
                .components()
                .count()
                .cmp(&b.dest.components().count())
                .then_with(|| a.dest.cmp(b.dest))
        });
        mounts
    }

    /// 挂载前从私有家目录根 fd 逐级检查落在私有家目录里的子挂载点，不跟随符号链接；遇到链接立即报错。
    fn verify_private_home_mountpoints(
        &self,
        sorted_mounts: &[MountSpec<'_>],
    ) -> Result<(), MountPrepError> {
        let home = &self.inner.host_home;
        for (idx, mount) in sorted_mounts.iter().enumerate() {
            let Ok(rel) = mount.dest.strip_prefix(home) else {
                continue;
            };
            if rel.as_os_str().is_empty() {
                return Err(MountPrepError::Tool(format!(
                    "执行环境 `{}` 的挂载目的地 `{}` 不能覆盖私有家目录根",
                    self.inner.name,
                    mount.dest.display()
                )));
            }

            // 若某个更浅的挂载点已位于当前目的地之上，则当前挂载点落在该父挂载内部，检查父挂载内无链接。
            if let Some(parent_mount) = sorted_mounts[..idx]
                .iter()
                .rev()
                .find(|earlier| mount.dest.starts_with(earlier.dest))
            {
                let sub_rel = mount
                    .dest
                    .strip_prefix(parent_mount.dest)
                    .expect("已确认 starts_with");
                // SAFETY: parent_mount.source_fd 由 HostExecEnv 持有且有效。
                let parent_fd = unsafe { BorrowedFd::borrow_raw(parent_mount.source_fd) };
                verify_existing_no_symlinks(
                    parent_fd,
                    sub_rel,
                    mount.is_dir,
                    &self.inner.name,
                    mount.dest,
                )?;
                continue;
            }

            ensure_mountpoint_in_private_home(
                self.inner.private_home_fd.as_fd(),
                rel,
                mount.is_dir,
                &self.inner.name,
                mount.dest,
            )?;
        }
        Ok(())
    }
}

/// 准备私有家目录挂载点时的错误分类：符号链接与类型冲突归调用方错误，其余宿主 I/O 故障归内部错误。
enum MountPrepError {
    Tool(String),
    Internal(String),
}

fn ensure_mountpoint_in_private_home(
    home_fd: BorrowedFd<'_>,
    rel: &Path,
    expect_dir: bool,
    env_name: &str,
    full_dest: &Path,
) -> Result<(), MountPrepError> {
    let comps: Vec<&OsStr> = rel
        .components()
        .map(|c| match c {
            Component::Normal(part) => Ok(part),
            _ => Err(MountPrepError::Tool(format!(
                "执行环境 `{env_name}` 的挂载路径 `{}` 包含非法路径分量",
                full_dest.display()
            ))),
        })
        .collect::<Result<Vec<_>, _>>()?;

    let Some((leaf, parents)) = comps.split_last() else {
        return Ok(());
    };

    let mut cur_owned: Option<OwnedFd> = None;
    for part in parents {
        let cur_fd = cur_owned.as_ref().map_or(home_fd, AsFd::as_fd);
        let next_fd = match openat2(
            cur_fd,
            *part,
            OFlags::PATH | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
            ResolveFlags::BENEATH | ResolveFlags::NO_SYMLINKS,
        ) {
            Ok(fd) => {
                let st = fstat(&fd).map_err(|err| {
                    MountPrepError::Internal(format!(
                        "执行环境 `{env_name}` 读取私有家目录挂载父路径 `{}` 属性失败：{err}",
                        full_dest.display()
                    ))
                })?;
                let ft = FileType::from_raw_mode(st.st_mode);
                if ft == FileType::Symlink {
                    return Err(MountPrepError::Tool(format!(
                        "执行环境 `{env_name}` 的私有家目录挂载点 `{}` 上级包含符号链接 `{}`，拒绝挂载",
                        full_dest.display(),
                        part.to_string_lossy()
                    )));
                }
                if ft != FileType::Directory {
                    return Err(MountPrepError::Tool(format!(
                        "执行环境 `{env_name}` 的私有家目录挂载点 `{}` 上级 `{}` 不是目录",
                        full_dest.display(),
                        part.to_string_lossy()
                    )));
                }
                fd
            }
            Err(Errno::NOENT) => {
                match mkdirat(cur_fd, *part, Mode::from_raw_mode(0o755)) {
                    Ok(()) | Err(Errno::EXIST) => {}
                    Err(err) => {
                        return Err(MountPrepError::Internal(format!(
                            "执行环境 `{env_name}` 在私有家目录为 `{}` 创建上级目录失败：{err}",
                            full_dest.display()
                        )));
                    }
                }
                openat2(
                    cur_fd,
                    *part,
                    OFlags::PATH | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                    Mode::empty(),
                    ResolveFlags::BENEATH | ResolveFlags::NO_SYMLINKS,
                )
                .map_err(|err| {
                    classify_open_mount_err(err, env_name, full_dest, "校验私有家目录挂载父目录")
                })?
            }
            Err(err) => {
                return Err(classify_open_mount_err(
                    err,
                    env_name,
                    full_dest,
                    "打开私有家目录挂载父路径",
                ));
            }
        };
        cur_owned = Some(next_fd);
    }

    let parent_fd = cur_owned.as_ref().map_or(home_fd, AsFd::as_fd);
    match openat2(
        parent_fd,
        *leaf,
        OFlags::PATH | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
        ResolveFlags::BENEATH | ResolveFlags::NO_SYMLINKS,
    ) {
        Ok(fd) => check_leaf_type(&fd, expect_dir, env_name, full_dest),
        Err(Errno::NOENT) => {
            if expect_dir {
                match mkdirat(parent_fd, *leaf, Mode::from_raw_mode(0o755)) {
                    Ok(()) | Err(Errno::EXIST) => {}
                    Err(err) => {
                        return Err(MountPrepError::Internal(format!(
                            "执行环境 `{env_name}` 在私有家目录创建挂载点目录 `{}` 失败：{err}",
                            full_dest.display()
                        )));
                    }
                }
            } else {
                match openat(
                    parent_fd,
                    *leaf,
                    OFlags::WRONLY
                        | OFlags::CREATE
                        | OFlags::EXCL
                        | OFlags::NOFOLLOW
                        | OFlags::CLOEXEC,
                    Mode::from_raw_mode(0o600),
                ) {
                    Ok(_) | Err(Errno::EXIST) => {}
                    Err(err) => {
                        return Err(MountPrepError::Internal(format!(
                            "执行环境 `{env_name}` 在私有家目录创建挂载点文件 `{}` 失败：{err}",
                            full_dest.display()
                        )));
                    }
                }
            }
            let fd = openat2(
                parent_fd,
                *leaf,
                OFlags::PATH | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
                ResolveFlags::BENEATH | ResolveFlags::NO_SYMLINKS,
            )
            .map_err(|err| {
                classify_open_mount_err(err, env_name, full_dest, "校验私有家目录挂载点")
            })?;
            check_leaf_type(&fd, expect_dir, env_name, full_dest)
        }
        Err(err) => Err(classify_open_mount_err(
            err,
            env_name,
            full_dest,
            "打开私有家目录挂载点",
        )),
    }
}

fn verify_existing_no_symlinks(
    base_fd: BorrowedFd<'_>,
    rel: &Path,
    expect_dir: bool,
    env_name: &str,
    full_dest: &Path,
) -> Result<(), MountPrepError> {
    let comps: Vec<&OsStr> = rel
        .components()
        .map(|c| match c {
            Component::Normal(part) => Ok(part),
            _ => Err(MountPrepError::Tool(format!(
                "执行环境 `{env_name}` 的挂载路径 `{}` 包含非法路径分量",
                full_dest.display()
            ))),
        })
        .collect::<Result<Vec<_>, _>>()?;

    let mut cur_owned: Option<OwnedFd> = None;
    for (i, part) in comps.iter().enumerate() {
        let cur_fd = cur_owned.as_ref().map_or(base_fd, AsFd::as_fd);
        let is_last = i + 1 == comps.len();
        match openat2(
            cur_fd,
            *part,
            OFlags::PATH | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
            ResolveFlags::BENEATH | ResolveFlags::NO_SYMLINKS,
        ) {
            Ok(fd) => {
                if is_last {
                    check_leaf_type(&fd, expect_dir, env_name, full_dest)?;
                } else {
                    let st = fstat(&fd).map_err(|err| {
                        MountPrepError::Internal(format!(
                            "执行环境 `{env_name}` 读取挂载路径 `{}` 属性失败：{err}",
                            full_dest.display()
                        ))
                    })?;
                    let ft = FileType::from_raw_mode(st.st_mode);
                    if ft == FileType::Symlink {
                        return Err(MountPrepError::Tool(format!(
                            "执行环境 `{env_name}` 的挂载点 `{}` 上级包含符号链接，拒绝挂载",
                            full_dest.display()
                        )));
                    }
                    if ft != FileType::Directory {
                        return Err(MountPrepError::Tool(format!(
                            "执行环境 `{env_name}` 的挂载点 `{}` 上级不是目录",
                            full_dest.display()
                        )));
                    }
                    cur_owned = Some(fd);
                }
            }
            Err(Errno::NOENT) => return Ok(()),
            Err(err) => {
                return Err(classify_open_mount_err(
                    err,
                    env_name,
                    full_dest,
                    "校验挂载点",
                ));
            }
        }
    }
    Ok(())
}

fn classify_open_mount_err(
    err: Errno,
    env_name: &str,
    full_dest: &Path,
    action: &str,
) -> MountPrepError {
    match err {
        Errno::LOOP | Errno::XDEV => MountPrepError::Tool(format!(
            "执行环境 `{env_name}` 的私有家目录挂载点 `{}` 包含符号链接，拒绝挂载",
            full_dest.display()
        )),
        Errno::NOTDIR | Errno::ISDIR => MountPrepError::Tool(format!(
            "执行环境 `{env_name}` 的私有家目录挂载点 `{}` 类型不匹配：{err}",
            full_dest.display()
        )),
        other => MountPrepError::Internal(format!(
            "执行环境 `{env_name}` {action} `{}` 失败：{other}",
            full_dest.display()
        )),
    }
}

fn check_leaf_type(
    fd: &OwnedFd,
    expect_dir: bool,
    env_name: &str,
    full_dest: &Path,
) -> Result<(), MountPrepError> {
    let st = fstat(fd).map_err(|err| {
        MountPrepError::Internal(format!(
            "执行环境 `{env_name}` 读取挂载点 `{}` 属性失败：{err}",
            full_dest.display()
        ))
    })?;
    let ft = FileType::from_raw_mode(st.st_mode);
    if ft == FileType::Symlink {
        return Err(MountPrepError::Tool(format!(
            "执行环境 `{env_name}` 的私有家目录挂载点 `{}` 是符号链接，拒绝挂载",
            full_dest.display()
        )));
    }
    if expect_dir && ft != FileType::Directory {
        return Err(MountPrepError::Tool(format!(
            "执行环境 `{env_name}` 的私有家目录挂载点 `{}` 应为目录，实际不是目录",
            full_dest.display()
        )));
    }
    if !expect_dir && ft != FileType::RegularFile {
        return Err(MountPrepError::Tool(format!(
            "执行环境 `{env_name}` 的私有家目录挂载点 `{}` 应为普通文件，实际不是普通文件",
            full_dest.display()
        )));
    }
    Ok(())
}

fn validate_argv(argv: &[String]) -> Result<(), String> {
    if argv.is_empty() {
        return Err("argv 不能为空".to_owned());
    }
    if argv[0].is_empty() {
        return Err("argv[0] 不能为空".to_owned());
    }
    for (idx, item) in argv.iter().enumerate() {
        if item.as_bytes().contains(&0) {
            return Err(format!("argv[{idx}] 包含非法空字节"));
        }
    }
    Ok(())
}

fn build_envp(
    host_home: &Path,
    extra_path: &[PathBuf],
    vars: &BTreeMap<String, String>,
) -> Vec<String> {
    let mut map = BTreeMap::new();
    map.insert("HOME".to_owned(), host_home.to_string_lossy().into_owned());

    let mut path_val = String::from(BASE_PATH);
    for entry in extra_path {
        path_val.push(':');
        path_val.push_str(&entry.to_string_lossy());
    }
    map.insert("PATH".to_owned(), path_val);

    for (k, v) in vars {
        map.insert(k.clone(), v.clone());
    }

    map.into_iter().map(|(k, v)| format!("{k}={v}")).collect()
}

#[allow(clippy::too_many_arguments)]
fn run_sandbox_invocation(
    leaf: cgroup::CgroupLeaf,
    private_home: Option<(RawFd, &Path)>,
    mounts: &[MountSpec<'_>],
    init_fd: RawFd,
    cwd: &Path,
    rw_paths: &[PathBuf],
    envp: &[String],
    argv: &[String],
    deadline: Instant,
    cancelled: &AtomicBool,
) -> (bool, ExecRunOutcome) {
    let (stdout_r, stdout_w) = match create_cloexec_pipe() {
        Ok(p) => p,
        Err(err) => {
            return (
                false,
                ExecRunOutcome::InternalError(format!("创建 stdout 管道失败：{err}")),
            );
        }
    };
    let (stderr_r, stderr_w) = match create_cloexec_pipe() {
        Ok(p) => p,
        Err(err) => {
            return (
                false,
                ExecRunOutcome::InternalError(format!("创建 stderr 管道失败：{err}")),
            );
        }
    };
    let (ctrl_r, ctrl_w) = match create_cloexec_pipe() {
        Ok(p) => p,
        Err(err) => {
            return (
                false,
                ExecRunOutcome::InternalError(format!("创建控制管道失败：{err}")),
            );
        }
    };
    let (status_r, status_w) = match create_cloexec_pipe() {
        Ok(p) => p,
        Err(err) => {
            return (
                false,
                ExecRunOutcome::InternalError(format!("创建状态管道失败：{err}")),
            );
        }
    };

    for fd in [
        stdout_r.as_raw_fd(),
        stderr_r.as_raw_fd(),
        status_r.as_raw_fd(),
    ] {
        if let Err(err) = set_nonblocking(fd) {
            return (
                false,
                ExecRunOutcome::InternalError(format!("设置管道非阻塞模式失败：{err}")),
            );
        }
    }

    let mut pass_fds: Vec<RawFd> = Vec::with_capacity(4 + mounts.len());
    pass_fds.push(ctrl_r.as_raw_fd());
    pass_fds.push(status_w.as_raw_fd());
    if let Some((home_fd, _)) = private_home {
        pass_fds.push(home_fd);
    }
    for m in mounts {
        pass_fds.push(m.source_fd);
    }
    pass_fds.push(init_fd);

    // SAFETY: getuid / getgid 无前置条件。
    let uid = unsafe { libc::getuid() };
    let gid = unsafe { libc::getgid() };

    let mut cmd = Command::new(BWRAP_PATH);
    cmd.env_clear()
        .args([
            "--unshare-all",
            "--unshare-user",
            "--die-with-parent",
            "--as-pid-1",
            "--new-session",
            "--clearenv",
            "--uid",
        ])
        .arg(uid.to_string())
        .arg("--gid")
        .arg(gid.to_string())
        .args(["--ro-bind", "/usr", "/usr"]);

    if let Err(err) = append_root_sys_mounts(&mut cmd) {
        return (
            false,
            ExecRunOutcome::InternalError(format!("构造系统目录挂载参数失败：{err:#}")),
        );
    }

    cmd.args(["--ro-bind", "/etc", "/etc"])
        .args(["--perms", "01777", "--tmpfs", "/tmp"])
        .args(["--proc", "/proc"])
        .args(["--tmpfs", "/dev"])
        .args(["--dev-bind", "/dev/null", "/dev/null"])
        .args(["--dev-bind", "/dev/zero", "/dev/zero"])
        .args(["--dev-bind", "/dev/random", "/dev/random"])
        .args(["--dev-bind", "/dev/urandom", "/dev/urandom"]);

    if let Some((home_fd, home_path)) = private_home {
        cmd.arg("--bind-fd").arg(home_fd.to_string()).arg(home_path);
    }

    for m in mounts {
        let flag = if m.writable {
            "--bind-fd"
        } else {
            "--ro-bind-fd"
        };
        cmd.arg(flag).arg(m.source_fd.to_string()).arg(m.dest);
    }

    cmd.arg("--ro-bind-fd")
        .arg(init_fd.to_string())
        .arg("/run/lancet-init")
        .args(["--remount-ro", "/"])
        .arg("--chdir")
        .arg(cwd)
        .arg("--")
        .arg("/run/lancet-init")
        .arg("--ctrl-fd")
        .arg(ctrl_r.as_raw_fd().to_string())
        .arg("--status-fd")
        .arg(status_w.as_raw_fd().to_string());

    for rw in rw_paths {
        cmd.arg("--rw").arg(rw);
    }
    for kv in envp {
        cmd.arg("--env").arg(kv);
    }
    cmd.arg("--").args(argv);

    cmd.stdin(Stdio::null())
        .stdout(Stdio::from(stdout_w))
        .stderr(Stdio::from(stderr_w));

    let procs_fd = leaf.procs_fd().as_raw_fd();
    // SAFETY: pre_exec 仅调用 async-signal-safe 的 write 与 fcntl。
    unsafe {
        cmd.pre_exec(move || {
            write_zero_to_cgroup_procs(procs_fd)?;
            for &fd in &pass_fds {
                clear_cloexec(fd)?;
            }
            Ok(())
        });
    }

    let child = match cmd.spawn() {
        Ok(c) => c,
        Err(err) => {
            drop(cmd);
            let grace = Duration::from_millis(crate::SETTLE_GRACE_MS);
            let now = Instant::now();
            let settle_deadline = if cancelled.load(Ordering::Acquire) {
                now + grace
            } else {
                now.max(deadline) + grace
            };
            let kill_res = leaf.kill();
            let leaf_res = leaf.wait_empty_and_remove(settle_deadline);
            if let Err(kill_err) = kill_res {
                return (
                    false,
                    ExecRunOutcome::InternalError(format!(
                        "启动 {BWRAP_PATH} 失败（{err}），且向叶组 cgroup.kill 写 1 失败：{kill_err}"
                    )),
                );
            }
            if let Err(leaf_err) = leaf_res {
                return (
                    false,
                    ExecRunOutcome::InternalError(format!(
                        "启动 {BWRAP_PATH} 失败（{err}），且回收 cgroup 叶组失败：{leaf_err:#}"
                    )),
                );
            }
            if err.raw_os_error() == Some(libc::E2BIG) {
                return (
                    false,
                    ExecRunOutcome::ToolError(format!(
                        "启动程序 `{}` 失败：参数列表过长（E2BIG：{err}）",
                        argv[0]
                    )),
                );
            }
            return (
                false,
                ExecRunOutcome::InternalError(format!("启动 {BWRAP_PATH} 失败：{err}")),
            );
        }
    };

    // 子进程已继承这些 fd；立即释放 cmd（关闭父进程持有的 stdout_w/stderr_w）以及 ctrl_r/status_w，使 EOF 及时可见。
    drop(cmd);
    drop(ctrl_r);
    drop(status_w);

    monitor_and_teardown(
        child, leaf, ctrl_w, status_r, stdout_r, stderr_r, &argv[0], deadline, cancelled,
    )
}

#[allow(clippy::too_many_arguments)]
fn monitor_and_teardown(
    mut child: Child,
    leaf: cgroup::CgroupLeaf,
    ctrl_w: OwnedFd,
    status_r: OwnedFd,
    stdout_r: OwnedFd,
    stderr_r: OwnedFd,
    program: &str,
    deadline: Instant,
    cancelled: &AtomicBool,
) -> (bool, ExecRunOutcome) {
    let mut ctrl_w = Some(ctrl_w);
    let mut status_fd = Some(status_r);
    let mut stdout_fd = Some(stdout_r);
    let mut stderr_fd = Some(stderr_r);

    let mut status_buf = Vec::with_capacity(64);
    let mut target_started = false;
    let mut init_status: Option<InitStatus> = None;
    let mut streams = DualStreamCapture::new(MAX_TOOL_RESULT_BYTES);
    let mut exit_status: Option<ExitStatus> = None;
    let mut interrupted: Option<InterruptKind> = None;
    let mut loop_err: Option<String> = None;
    let mut kill_err: Option<io::Error> = None;
    let mut kill_sent = false;

    loop {
        if cancelled.load(Ordering::Acquire) {
            interrupted = Some(InterruptKind::Cancelled);
            break;
        }
        if Instant::now() >= deadline {
            interrupted = Some(InterruptKind::Deadline);
            break;
        }

        if exit_status.is_none() {
            match child.try_wait() {
                Ok(Some(st)) => {
                    exit_status = Some(st);
                    if !kill_sent {
                        kill_sent = true;
                        if let Err(err) = leaf.kill() {
                            kill_err = Some(err);
                            break;
                        }
                    }
                }
                Ok(None) => {}
                Err(err) => {
                    loop_err = Some(format!("查询 bwrap 子进程状态失败：{err}"));
                    break;
                }
            }
        }

        if exit_status.is_some()
            && status_fd.is_none()
            && stdout_fd.is_none()
            && stderr_fd.is_none()
        {
            break;
        }

        let mut pollfds = Vec::with_capacity(3);
        let status_slot = status_fd.as_ref().map(|fd| {
            let idx = pollfds.len();
            pollfds.push(libc::pollfd {
                fd: fd.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            });
            idx
        });
        let stdout_slot = stdout_fd.as_ref().map(|fd| {
            let idx = pollfds.len();
            pollfds.push(libc::pollfd {
                fd: fd.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            });
            idx
        });
        let stderr_slot = stderr_fd.as_ref().map(|fd| {
            let idx = pollfds.len();
            pollfds.push(libc::pollfd {
                fd: fd.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            });
            idx
        });

        let wait_ms = compute_poll_timeout_ms(deadline);
        if !pollfds.is_empty() {
            // SAFETY: pollfds 均来自本函数持有的有效 OwnedFd。
            let rc =
                unsafe { libc::poll(pollfds.as_mut_ptr(), pollfds.len() as libc::nfds_t, wait_ms) };
            if rc < 0 && io::Error::last_os_error().raw_os_error() != Some(libc::EINTR) {
                loop_err = Some(format!("轮询沙箱管道失败：{}", io::Error::last_os_error()));
                break;
            }
        } else {
            // 管道已全部关闭，只等待 bwrap 退出；按最多 10 ms 步长等待并保持可取消。
            std::thread::sleep(Duration::from_millis(wait_ms.clamp(1, POLL_STEP_MS) as u64));
        }

        if let Some(idx) = status_slot
            && pollfds[idx].revents != 0
            && let Some(fd) = status_fd.as_ref()
        {
            match read_status_pipe(fd.as_raw_fd(), &mut status_buf) {
                StatusRead::Pending => {
                    if let Some(parsed) = try_parse_init_status(&status_buf, false) {
                        status_fd = None;
                        if matches!(parsed, InitStatus::Started) {
                            target_started = true;
                        }
                        init_status = Some(parsed);
                    }
                }
                StatusRead::Eof => {
                    status_fd = None;
                    if let Some(parsed) = try_parse_init_status(&status_buf, true) {
                        if matches!(parsed, InitStatus::Started) {
                            target_started = true;
                        }
                        init_status = Some(parsed);
                    }
                }
                StatusRead::Error(err) => {
                    if matches!(
                        try_parse_init_status(&status_buf, false),
                        Some(InitStatus::Started)
                    ) {
                        target_started = true;
                    }
                    status_fd = None;
                    loop_err = Some(format!("读取沙箱状态管道失败：{err}"));
                    break;
                }
            }
        }

        if let Some(idx) = stdout_slot
            && pollfds[idx].revents != 0
            && let Some(fd) = stdout_fd.as_ref()
            && drain_pipe_nonblocking(fd.as_raw_fd(), &mut streams, StreamKind::Stdout)
        {
            stdout_fd = None;
        }

        if let Some(idx) = stderr_slot
            && pollfds[idx].revents != 0
            && let Some(fd) = stderr_fd.as_ref()
            && drain_pipe_nonblocking(fd.as_raw_fd(), &mut streams, StreamKind::Stderr)
        {
            stderr_fd = None;
        }
    }

    // 统一收尾时限只确定一次：取消取 now + 5 s，正常结束与截止取 max(now, deadline) + 5 s；
    // 先杀组、回收 bwrap，再把状态管道读到 EOF，然后等待 populated 0、读 OOM 并删除叶组。
    let grace = Duration::from_millis(crate::SETTLE_GRACE_MS);
    let now = Instant::now();
    let settle_deadline = match interrupted {
        Some(InterruptKind::Cancelled) => now + grace,
        Some(InterruptKind::Deadline) | None => now.max(deadline) + grace,
    };

    drop(ctrl_w.take());
    drop(stdout_fd.take());
    drop(stderr_fd.take());

    if !kill_sent && let Err(err) = leaf.kill() {
        kill_err = Some(err);
    }

    let reap_res = match exit_status {
        Some(st) => Ok(st),
        None => wait_child_until(&mut child, settle_deadline),
    };

    let mut status_err: Option<String> = None;
    if let Some(fd) = status_fd.take() {
        let drain_res = drain_status_to_eof(fd.as_raw_fd(), &mut status_buf, settle_deadline);
        let eof = matches!(drain_res, StatusDrain::Eof);
        if let Some(parsed) = try_parse_init_status(&status_buf, eof) {
            if matches!(parsed, InitStatus::Started) {
                target_started = true;
            }
            if init_status.is_none() {
                init_status = Some(parsed);
            }
        }
        match drain_res {
            StatusDrain::Eof => {}
            StatusDrain::TimedOut => {
                status_err = Some("等待沙箱状态管道 EOF 超过结算时限".to_owned());
            }
            StatusDrain::IoError(err) => {
                status_err = Some(format!("读取沙箱状态管道失败：{err}"));
            }
        }
    }

    let leaf_res = leaf.wait_empty_and_remove(settle_deadline);

    if let Some(msg) = loop_err {
        return (target_started, ExecRunOutcome::InternalError(msg));
    }
    if let Some(err) = kill_err {
        return (
            target_started,
            ExecRunOutcome::InternalError(format!("向叶组 cgroup.kill 写 1 失败：{err}")),
        );
    }
    let final_status = match reap_res {
        Ok(st) => st,
        Err(err) => {
            return (
                target_started,
                ExecRunOutcome::InternalError(format!("回收 bwrap 子进程失败：{err}")),
            );
        }
    };
    let oom = match leaf_res {
        Ok(oom) => oom,
        Err(err) => {
            return (
                target_started,
                ExecRunOutcome::InternalError(format!("回收 cgroup 叶组失败：{err:#}")),
            );
        }
    };
    if let Some(msg) = status_err {
        return (target_started, ExecRunOutcome::InternalError(msg));
    }

    if let Some(kind) = interrupted {
        let outcome = match kind {
            InterruptKind::Cancelled => ExecRunOutcome::Cancelled,
            InterruptKind::Deadline => ExecRunOutcome::Deadline,
        };
        return (target_started, outcome);
    }

    if oom {
        return (
            target_started,
            ExecRunOutcome::ToolError(format!(
                "进程 `{program}` 超出沙箱内存上限 8 GiB（{MEMORY_MAX_BYTES} 字节），进程组已被内核 OOM 终止"
            )),
        );
    }

    match init_status.unwrap_or(InitStatus::UnexpectedEof) {
        InitStatus::Started => {}
        InitStatus::ExecFailed(err_no) => {
            return (
                false,
                ExecRunOutcome::ToolError(format_exec_error(program, err_no)),
            );
        }
        InitStatus::InitFailed(detail) => {
            return (
                false,
                ExecRunOutcome::InternalError(format!("沙箱垫片初始化失败：{detail}")),
            );
        }
        InitStatus::UnexpectedEof => {
            let stderr_hint = streams.stderr_preview();
            let suffix = if stderr_hint.is_empty() {
                String::new()
            } else {
                format!("：{stderr_hint}")
            };
            return (
                false,
                ExecRunOutcome::InternalError(format!(
                    "bubblewrap 沙箱在垫片就绪前退出（{final_status}）{suffix}"
                )),
            );
        }
    }

    let code = exit_status_to_code(final_status);
    if streams.overflowed {
        let mut msg = if code == 0 {
            format!(
                "进程 `{program}` 的输出合计 {} 字节，超过单次返回窗口 8 MiB（{MAX_TOOL_RESULT_BYTES} 字节）；请改用 `proc.pipe` 过滤后再返回",
                streams.total_bytes()
            )
        } else {
            format!(
                "进程 `{program}` 以退出码 {code} 结束，且输出合计 {} 字节超过单次返回窗口 8 MiB（{MAX_TOOL_RESULT_BYTES} 字节）；请改用 `proc.pipe` 过滤后再返回",
                streams.total_bytes()
            )
        };
        let out_tail = streams.stdout_tail();
        if !out_tail.is_empty() {
            msg.push_str(&format!("\nstdout 尾部（已截断）：\n{out_tail}"));
        }
        let err_tail = streams.stderr_tail();
        if !err_tail.is_empty() {
            msg.push_str(&format!("\nstderr 尾部（已截断）：\n{err_tail}"));
        }
        return (target_started, ExecRunOutcome::ToolError(msg));
    }

    let (stdout, stderr) = streams.into_strings();
    (
        target_started,
        ExecRunOutcome::Finished(ProcOutput {
            code,
            stdout,
            stderr,
        }),
    )
}

#[derive(Clone, Copy)]
enum InterruptKind {
    Cancelled,
    Deadline,
}

enum InitStatus {
    Started,
    ExecFailed(i32),
    InitFailed(String),
    UnexpectedEof,
}

/// 当状态消息已完整（`O` 1 字节、`E` + 4 字节 errno）或读到 EOF 时返回解析结果。
/// 外层 bwrap 可能继续持有写端直到退出，因此 `O` 与 `E` 不能等 EOF 才判定。
fn try_parse_init_status(buf: &[u8], eof: bool) -> Option<InitStatus> {
    match buf.first() {
        Some(b'O') => Some(InitStatus::Started),
        Some(b'E') if buf.len() >= 5 => {
            let err_no = i32::from_le_bytes([buf[1], buf[2], buf[3], buf[4]]);
            Some(InitStatus::ExecFailed(err_no))
        }
        Some(b'I') if eof => Some(InitStatus::InitFailed(
            String::from_utf8_lossy(&buf[1..]).into_owned(),
        )),
        _ if eof => Some(InitStatus::UnexpectedEof),
        _ => None,
    }
}

fn format_exec_error(program: &str, err_no: i32) -> String {
    let os_err = io::Error::from_raw_os_error(err_no);
    match err_no {
        libc::ENOENT => format!("启动程序 `{program}` 失败：找不到可执行文件（ENOENT：{os_err}）"),
        libc::ENOEXEC => {
            format!("启动程序 `{program}` 失败：可执行文件格式无效（ENOEXEC：{os_err}）")
        }
        libc::EACCES => format!("启动程序 `{program}` 失败：缺少执行权限（EACCES：{os_err}）"),
        _ => format!("启动程序 `{program}` 失败：{os_err}"),
    }
}

fn exit_status_to_code(status: ExitStatus) -> i32 {
    if let Some(code) = status.code() {
        code
    } else if let Some(sig) = status.signal() {
        128 + sig
    } else {
        1
    }
}

fn compute_poll_timeout_ms(deadline: Instant) -> i32 {
    let remaining = deadline.saturating_duration_since(Instant::now());
    let ms = remaining.as_millis().min(POLL_STEP_MS as u128) as i32;
    ms.max(1)
}

enum StatusRead {
    Pending,
    Eof,
    Error(io::Error),
}

enum StatusDrain {
    Eof,
    TimedOut,
    IoError(io::Error),
}

fn read_status_pipe(fd: RawFd, buf: &mut Vec<u8>) -> StatusRead {
    let mut chunk = [0u8; 256];
    loop {
        // SAFETY: 从非阻塞状态管道读取。
        let n = unsafe { libc::read(fd, chunk.as_mut_ptr().cast(), chunk.len()) };
        if n > 0 {
            if buf.len() < 4096 {
                let take = (n as usize).min(4096 - buf.len());
                buf.extend_from_slice(&chunk[..take]);
            }
        } else if n == 0 {
            return StatusRead::Eof;
        } else {
            let err = io::Error::last_os_error();
            let err_no = err.raw_os_error();
            if err_no == Some(libc::EINTR) {
                continue;
            }
            if err_no == Some(libc::EAGAIN) || err_no == Some(libc::EWOULDBLOCK) {
                return StatusRead::Pending;
            }
            return StatusRead::Error(err);
        }
    }
}

#[derive(Clone, Copy)]
enum StreamKind {
    Stdout,
    Stderr,
}

struct SingleStream {
    data: Vec<u8>,
    tail: VecDeque<u8>,
    total: usize,
}

impl SingleStream {
    fn new() -> Self {
        Self {
            data: Vec::new(),
            tail: VecDeque::new(),
            total: 0,
        }
    }

    fn seed_tail_from_data(&mut self) {
        let start = self.data.len().saturating_sub(OVERFLOW_TAIL_BYTES);
        self.tail.clear();
        self.tail.extend(&self.data[start..]);
        self.data.clear();
        self.data.shrink_to_fit();
    }

    fn push_tail(&mut self, bytes: &[u8]) {
        if bytes.len() >= OVERFLOW_TAIL_BYTES {
            self.tail.clear();
            self.tail
                .extend(&bytes[bytes.len() - OVERFLOW_TAIL_BYTES..]);
        } else {
            let excess = (self.tail.len() + bytes.len()).saturating_sub(OVERFLOW_TAIL_BYTES);
            if excess > 0 {
                self.tail.drain(..excess);
            }
            self.tail.extend(bytes);
        }
    }

    fn tail_string(&self) -> String {
        let mut slice = Vec::with_capacity(self.tail.len());
        let (a, b) = self.tail.as_slices();
        slice.extend_from_slice(a);
        slice.extend_from_slice(b);
        lossy_trim_leading_continuation(&slice)
    }
}

struct DualStreamCapture {
    remaining_window: usize,
    overflowed: bool,
    stdout: SingleStream,
    stderr: SingleStream,
}

impl DualStreamCapture {
    fn new(window_bytes: usize) -> Self {
        Self {
            remaining_window: window_bytes,
            overflowed: false,
            stdout: SingleStream::new(),
            stderr: SingleStream::new(),
        }
    }

    fn push(&mut self, kind: StreamKind, bytes: &[u8]) {
        if !self.overflowed && bytes.len() > self.remaining_window {
            self.overflowed = true;
            self.stdout.seed_tail_from_data();
            self.stderr.seed_tail_from_data();
        }
        let target = match kind {
            StreamKind::Stdout => &mut self.stdout,
            StreamKind::Stderr => &mut self.stderr,
        };
        target.total = target.total.saturating_add(bytes.len());
        if !self.overflowed {
            target.data.extend_from_slice(bytes);
            self.remaining_window -= bytes.len();
        } else {
            target.push_tail(bytes);
        }
    }

    fn total_bytes(&self) -> usize {
        self.stdout.total.saturating_add(self.stderr.total)
    }

    fn stdout_tail(&self) -> String {
        self.stdout.tail_string()
    }

    fn stderr_tail(&self) -> String {
        self.stderr.tail_string()
    }

    fn stderr_preview(&self) -> String {
        if self.overflowed {
            self.stderr.tail_string()
        } else {
            String::from_utf8_lossy(&self.stderr.data).trim().to_owned()
        }
    }

    fn into_strings(self) -> (String, String) {
        (
            String::from_utf8_lossy(&self.stdout.data).into_owned(),
            String::from_utf8_lossy(&self.stderr.data).into_owned(),
        )
    }
}

fn lossy_trim_leading_continuation(bytes: &[u8]) -> String {
    let mut start = 0usize;
    while start < bytes.len() && (bytes[start] & 0b1100_0000) == 0b1000_0000 {
        start += 1;
    }
    String::from_utf8_lossy(&bytes[start..]).into_owned()
}

fn wait_child_until(child: &mut Child, settle_deadline: Instant) -> io::Result<ExitStatus> {
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(status);
        }
        let remaining = settle_deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "等待 bwrap 退出超过结算时限",
            ));
        }
        std::thread::sleep(remaining.min(Duration::from_millis(5)));
    }
}

fn drain_status_to_eof(fd: RawFd, buf: &mut Vec<u8>, settle_deadline: Instant) -> StatusDrain {
    loop {
        match read_status_pipe(fd, buf) {
            StatusRead::Eof => return StatusDrain::Eof,
            StatusRead::Error(err) => return StatusDrain::IoError(err),
            StatusRead::Pending => {}
        }
        let remaining = settle_deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return StatusDrain::TimedOut;
        }
        let wait_ms = remaining.as_millis().clamp(1, POLL_STEP_MS as u128) as libc::c_int;
        let mut pfd = libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: 在结算时限内等待状态管道剩余字节或 EOF。
        let rc = unsafe { libc::poll(&mut pfd, 1, wait_ms) };
        if rc < 0 {
            let err = io::Error::last_os_error();
            if err.raw_os_error() == Some(libc::EINTR) {
                continue;
            }
            return StatusDrain::IoError(err);
        }
        if (pfd.revents & libc::POLLNVAL) != 0 {
            return StatusDrain::IoError(io::Error::from_raw_os_error(libc::EBADF));
        }
    }
}

fn drain_pipe_nonblocking(fd: RawFd, capture: &mut DualStreamCapture, kind: StreamKind) -> bool {
    let mut chunk = [0u8; PIPE_READ_CHUNK_BYTES];
    loop {
        // SAFETY: 从非阻塞管道最多读取一个固定块（64 KiB）后返回 poll，保证另一路输出与取消/截止及时检查。
        let n = unsafe { libc::read(fd, chunk.as_mut_ptr().cast(), chunk.len()) };
        if n > 0 {
            capture.push(kind, &chunk[..n as usize]);
            return false;
        } else if n == 0 {
            return true;
        } else {
            let err_no = io::Error::last_os_error().raw_os_error();
            if err_no == Some(libc::EINTR) {
                continue;
            }
            if err_no == Some(libc::EAGAIN) || err_no == Some(libc::EWOULDBLOCK) {
                return false;
            }
            return true;
        }
    }
}

fn append_root_sys_mounts(cmd: &mut Command) -> anyhow::Result<()> {
    for path_str in ["/bin", "/sbin", "/lib", "/lib64"] {
        let path = Path::new(path_str);
        let meta = match fs::symlink_metadata(path) {
            Ok(m) => m,
            Err(err) if err.kind() == io::ErrorKind::NotFound => continue,
            Err(err) => {
                return Err(err).with_context(|| format!("读取宿主路径 {path_str} 元数据失败"));
            }
        };
        let ft = meta.file_type();
        if ft.is_symlink() {
            let target = fs::read_link(path)
                .with_context(|| format!("读取宿主符号链接 {path_str} 目标失败"))?;
            let target_str = target.to_string_lossy();
            if target_str.starts_with("usr/") || target_str.starts_with("/usr/") {
                cmd.arg("--symlink").arg(&target).arg(path_str);
            } else if path.is_dir() {
                cmd.arg("--ro-bind").arg(path_str).arg(path_str);
            }
        } else if ft.is_dir() {
            cmd.arg("--ro-bind").arg(path_str).arg(path_str);
        }
    }
    Ok(())
}

fn set_nonblocking(fd: RawFd) -> io::Result<()> {
    // SAFETY: 读取并设置 O_NONBLOCK 标志。
    unsafe {
        let flags = libc::fcntl(fd, libc::F_GETFL);
        if flags < 0 {
            return Err(io::Error::last_os_error());
        }
        if libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) < 0 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}

fn ensure_dir_0700(path: &Path) -> anyhow::Result<()> {
    match DirBuilder::new().mode(0o700).create(path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {
            if !path.is_dir() {
                anyhow::bail!("路径 {} 已存在但不是目录", path.display());
            }
            Ok(())
        }
        Err(err) => Err(err).with_context(|| format!("创建目录 {} 失败", path.display())),
    }
}
