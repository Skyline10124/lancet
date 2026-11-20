// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 影子工作区的条目快照、按 stat 比较、内容摘要、不跟随符号链接的原子安装、
//! `shadows/<key>/tmp/` 与 `shadows/<key>/widened` 崩溃恢复以及 unified diff 工具函数。

use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::fmt::Write as _;
use std::fs::{self, DirBuilder, File, Metadata};
use std::io::{self, Seek as _, SeekFrom, Write as _};
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::os::unix::ffi::{OsStrExt as _, OsStringExt as _};
use std::os::unix::fs::{DirBuilderExt as _, MetadataExt as _, PermissionsExt as _, symlink};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use rustix::fs::{
    AtFlags, CWD, Dir, FileType, Mode, OFlags, ResolveFlags, Stat, chmodat, fchmod, fstat,
    ioctl_ficlone, mkdirat, openat, openat2, readlinkat, renameat, statat, symlinkat, unlinkat,
};
use rustix::io::Errno;
use sha2::{Digest as _, Sha256};
use similar::TextDiff;

use crate::dispatch::TOOL_MEMORY_LIMIT_BYTES;

/// 空文件的 SHA-256 摘要。
pub(crate) const EMPTY_SHA256: [u8; 32] = [
    0xe3, 0xb0, 0xc4, 0x42, 0x98, 0xfc, 0x1c, 0x14, 0x9a, 0xfb, 0xf4, 0xc8, 0x99, 0x6f, 0xb9, 0x24,
    0x27, 0xae, 0x41, 0xe4, 0x64, 0x9b, 0x93, 0x4c, 0xa4, 0x95, 0x99, 0x1b, 0x78, 0x52, 0xb8, 0x55,
];

/// 粗粒度时钟（XFS / Linux multigrain timestamp）下的 racy 比较时间窗（2 秒）。
pub(crate) const RACY_WINDOW_NS: i128 = 2_000_000_000;

static ATOMIC_TMP_SEQ: AtomicU64 = AtomicU64::new(0);

/// `status` 与 `diff` 使用的待定改动动作。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PendingAction {
    Added,
    Modified,
    Deleted,
}

impl PendingAction {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Added => "added",
            Self::Modified => "modified",
            Self::Deleted => "deleted",
        }
    }
}

/// `fs.status` 返回的单条可见待定改动。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingChange {
    pub path: String,
    pub action: PendingAction,
    pub conflict: bool,
}

/// `fs.diff` 返回的单条可见待定改动及文本补丁。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathDiff {
    pub path: String,
    pub action: PendingAction,
    pub conflict: bool,
    pub patch: Option<String>,
}

/// 影子管理跟踪的三类对象：常规文件、目录与符号链接（特殊文件不计入待定改动，见 `docs/architecture/lancet-node/shadow-workspace.md`“待定改动”）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EntryKind {
    File,
    Dir,
    Symlink,
}

/// 标明被放宽权限的目录属于 `tree/` 还是 `base/`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum TreeTag {
    Tree,
    Base,
}

impl TreeTag {
    fn as_str(self) -> &'static str {
        match self {
            Self::Tree => "tree",
            Self::Base => "base",
        }
    }

    fn parse(s: &str) -> Option<Self> {
        match s {
            "tree" => Some(Self::Tree),
            "base" => Some(Self::Base),
            _ => None,
        }
    }
}

/// 后台扫描与索引复用的 stat 签名。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct StatSig {
    pub dev: u64,
    pub ino: u64,
    pub mode: u32,
    pub nlink: u64,
    pub size: u64,
    pub mtime_sec: i64,
    pub mtime_nsec: i64,
    pub ctime_sec: i64,
    pub ctime_nsec: i64,
}

impl StatSig {
    pub fn from_metadata(m: &Metadata) -> Self {
        Self {
            dev: m.dev(),
            ino: m.ino(),
            mode: m.mode() & 0o7777,
            nlink: m.nlink(),
            size: m.len(),
            mtime_sec: m.mtime(),
            mtime_nsec: m.mtime_nsec(),
            ctime_sec: m.ctime(),
            ctime_nsec: m.ctime_nsec(),
        }
    }

    pub fn from_stat(st: &Stat) -> Self {
        Self {
            dev: st.st_dev,
            ino: st.st_ino,
            mode: st.st_mode & 0o7777,
            nlink: st.st_nlink,
            size: st.st_size.max(0) as u64,
            mtime_sec: st.st_mtime,
            mtime_nsec: st.st_mtime_nsec as i64,
            ctime_sec: st.st_ctime,
            ctime_nsec: st.st_ctime_nsec as i64,
        }
    }

    pub fn matches_stat(&self, st: &Stat) -> bool {
        self.dev == st.st_dev
            && self.ino == st.st_ino
            && self.mode == (st.st_mode & 0o7777)
            && self.nlink == st.st_nlink
            && self.size == (st.st_size.max(0) as u64)
            && self.mtime_sec == st.st_mtime
            && self.mtime_nsec == st.st_mtime_nsec as i64
            && self.ctime_sec == st.st_ctime
            && self.ctime_nsec == st.st_ctime_nsec as i64
    }

    pub fn is_within_racy_window(&self, ref_ns: i128) -> bool {
        let mtime_ns = (self.mtime_sec as i128) * 1_000_000_000 + (self.mtime_nsec as i128);
        let ctime_ns = (self.ctime_sec as i128) * 1_000_000_000 + (self.ctime_nsec as i128);
        mtime_ns + RACY_WINDOW_NS >= ref_ns || ctime_ns + RACY_WINDOW_NS >= ref_ns
    }
}

/// 单个路径在 `base/`、`tree/` 或宿主树中的语义与元数据快照。
#[derive(Debug, Clone)]
pub(crate) struct EntrySnapshot {
    pub kind: EntryKind,
    pub mode: u32,
    pub size: u64,
    /// 常规文件内容摘要（按需惰性计算后缓存；空文件直接填 `EMPTY_SHA256`）。
    pub hash: Option<[u8; 32]>,
    /// 符号链接指向的原始字节序列。
    pub link_target: Option<Vec<u8>>,
    pub stat: StatSig,
}

impl EntrySnapshot {
    pub fn ensure_hash_at(&mut self, root_fd: BorrowedFd<'_>, rel: &Path) -> io::Result<[u8; 32]> {
        if let Some(h) = self.hash {
            return Ok(h);
        }
        if self.size == 0 {
            self.hash = Some(EMPTY_SHA256);
            return Ok(EMPTY_SHA256);
        }
        let h = hash_file_at(root_fd, rel)?;
        self.hash = Some(h);
        Ok(h)
    }
}

/// 返回当前系统实时时钟纳秒数（与文件 mtime/ctime 同源）。
pub(crate) fn now_realtime_ns() -> i128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos() as i128)
}

/// 由 `rustix::fs::Stat` 分类对象类型；FIFO、套接字、设备等特殊文件返回 `None`。
pub(crate) fn classify_stat(st: &Stat) -> Option<EntryKind> {
    match FileType::from_raw_mode(st.st_mode) {
        FileType::RegularFile => Some(EntryKind::File),
        FileType::Directory => Some(EntryKind::Dir),
        FileType::Symlink => Some(EntryKind::Symlink),
        _ => None,
    }
}

/// 打开目录根 fd（`O_PATH | O_DIRECTORY | O_CLOEXEC`，不跟随符号链接）。
pub(crate) fn open_root_fd(path: &Path) -> io::Result<OwnedFd> {
    openat2(
        CWD,
        path,
        OFlags::PATH | OFlags::DIRECTORY | OFlags::CLOEXEC,
        Mode::empty(),
        ResolveFlags::NO_SYMLINKS,
    )
    .map_err(io::Error::from)
}

/// 以可读模式打开目录根 fd（支持 `fsync`）。
pub(crate) fn open_syncable_dir_fd(path: &Path) -> io::Result<OwnedFd> {
    openat2(
        CWD,
        path,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
        Mode::empty(),
        ResolveFlags::NO_SYMLINKS,
    )
    .map_err(io::Error::from)
}

/// 不跟随任何中间符号链接地打开 `rel` 的上级目录 fd 并返回叶名。
///
/// 仅当路径缺失、中间分量不是目录或中间分量是符号链接时返回 `Ok(None)`；
/// 权限拒绝（`EACCES`）等真实 I/O 错误原样返回 `Err`。
pub(crate) fn open_parent_nofollow<'a>(
    root_fd: BorrowedFd<'_>,
    rel: &'a Path,
) -> io::Result<Option<(Option<OwnedFd>, &'a OsStr)>> {
    let Some(leaf) = rel.file_name() else {
        return Err(io::Error::other("相对路径缺少文件名"));
    };
    let parent = rel.parent().unwrap_or_else(|| Path::new(""));
    if parent.as_os_str().is_empty() {
        return Ok(Some((None, leaf)));
    }
    match openat2(
        root_fd,
        parent,
        OFlags::PATH | OFlags::DIRECTORY | OFlags::CLOEXEC,
        Mode::empty(),
        ResolveFlags::BENEATH | ResolveFlags::NO_SYMLINKS,
    ) {
        Ok(fd) => Ok(Some((Some(fd), leaf))),
        Err(Errno::NOENT | Errno::NOTDIR | Errno::LOOP) => Ok(None),
        Err(err) => Err(io::Error::from(err)),
    }
}

/// 不跟随任何符号链接地打开 `rel` 对应的真实目录（用于枚举或递归遍历）。
pub(crate) fn open_dir_nofollow(
    root_fd: BorrowedFd<'_>,
    rel: &Path,
) -> io::Result<Option<OwnedFd>> {
    if rel.as_os_str().is_empty() {
        return openat(
            root_fd,
            c".",
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map(Some)
        .map_err(io::Error::from);
    }
    match openat2(
        root_fd,
        rel,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
        ResolveFlags::BENEATH | ResolveFlags::NO_SYMLINKS,
    ) {
        Ok(fd) => Ok(Some(fd)),
        Err(Errno::NOENT | Errno::NOTDIR | Errno::LOOP) => Ok(None),
        Err(err) => Err(io::Error::from(err)),
    }
}

/// 从 `root_fd` 不跟随任何符号链接地读取 `rel` 的 `lstat` 与符号链接目标；
/// 仅确认路径缺失（含中间段换型为符号链接）或为特殊文件时返回 `Ok(None)`，其余错误原样返回 `Err`。
pub(crate) fn inspect_lstat_at(
    root_fd: BorrowedFd<'_>,
    rel: &Path,
) -> io::Result<Option<EntrySnapshot>> {
    let Some((parent_owned, leaf)) = open_parent_nofollow(root_fd, rel)? else {
        return Ok(None);
    };
    let parent_fd = parent_owned.as_ref().map_or(root_fd, AsFd::as_fd);
    let st = match statat(parent_fd, leaf, AtFlags::SYMLINK_NOFOLLOW) {
        Ok(st) => st,
        Err(Errno::NOENT | Errno::NOTDIR) => return Ok(None),
        Err(err) => return Err(io::Error::from(err)),
    };
    let Some(kind) = classify_stat(&st) else {
        return Ok(None);
    };
    let stat = StatSig::from_stat(&st);
    let link_target = if kind == EntryKind::Symlink {
        let target = readlinkat(parent_fd, leaf, Vec::new()).map_err(io::Error::from)?;
        Some(target.into_bytes())
    } else {
        None
    };
    let hash = if kind == EntryKind::File && stat.size == 0 {
        Some(EMPTY_SHA256)
    } else {
        None
    };
    Ok(Some(EntrySnapshot {
        kind,
        mode: stat.mode,
        size: stat.size,
        hash,
        link_target,
        stat,
    }))
}

/// 从 `root_fd` 不跟随任何符号链接地读取 `rel` 并计算常规文件内容摘要。
pub(crate) fn inspect_with_hash_at(
    root_fd: BorrowedFd<'_>,
    rel: &Path,
) -> io::Result<Option<EntrySnapshot>> {
    let Some((parent_owned, leaf)) = open_parent_nofollow(root_fd, rel)? else {
        return Ok(None);
    };
    let parent_fd = parent_owned.as_ref().map_or(root_fd, AsFd::as_fd);
    let st = match statat(parent_fd, leaf, AtFlags::SYMLINK_NOFOLLOW) {
        Ok(st) => st,
        Err(Errno::NOENT | Errno::NOTDIR) => return Ok(None),
        Err(err) => return Err(io::Error::from(err)),
    };
    let Some(kind) = classify_stat(&st) else {
        return Ok(None);
    };
    let mut stat = StatSig::from_stat(&st);
    let link_target = if kind == EntryKind::Symlink {
        let target = readlinkat(parent_fd, leaf, Vec::new()).map_err(io::Error::from)?;
        Some(target.into_bytes())
    } else {
        None
    };
    let hash = if kind == EntryKind::File {
        if stat.size == 0 {
            Some(EMPTY_SHA256)
        } else {
            let fd = openat(
                parent_fd,
                leaf,
                OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            )
            .map_err(io::Error::from)?;
            let post_st = fstat(&fd).map_err(io::Error::from)?;
            stat = StatSig::from_stat(&post_st);
            let mut file = File::from(fd);
            Some(hash_reader(&mut file)?)
        }
    } else {
        None
    };
    Ok(Some(EntrySnapshot {
        kind,
        mode: stat.mode,
        size: stat.size,
        hash,
        link_target,
        stat,
    }))
}

/// 不跟随符号链接地计算 `root_fd/rel` 普通文件的 SHA-256 摘要。
pub(crate) fn hash_file_at(root_fd: BorrowedFd<'_>, rel: &Path) -> io::Result<[u8; 32]> {
    let Some((parent_owned, leaf)) = open_parent_nofollow(root_fd, rel)? else {
        return Err(io::Error::from(Errno::NOENT));
    };
    let parent_fd = parent_owned.as_ref().map_or(root_fd, AsFd::as_fd);
    let fd = openat(
        parent_fd,
        leaf,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(io::Error::from)?;
    let mut file = File::from(fd);
    hash_reader(&mut file)
}

pub(crate) fn hash_reader<R: io::Read>(reader: &mut R) -> io::Result<[u8; 32]> {
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 65_536];
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hasher.finalize().into())
}

/// 通过向 `tmp_root_fd`（`shadows/<key>/tmp/`，同文件系统且不污染宿主/影子/基准）中的匿名
/// `O_TMPFILE` 执行 `FICLONE`，获取一致的文件快照摘要并重新武装源文件的 `MAP_SHARED` 页表保护。
pub(crate) fn hash_host_file_via_ficlone_at(
    host_root_fd: BorrowedFd<'_>,
    rel: &Path,
    tmp_root_fd: BorrowedFd<'_>,
) -> io::Result<Option<(StatSig, [u8; 32])>> {
    let Some((parent_owned, leaf)) = open_parent_nofollow(host_root_fd, rel)? else {
        return Ok(None);
    };
    let parent_fd = parent_owned.as_ref().map_or(host_root_fd, AsFd::as_fd);
    let src_fd = match openat(
        parent_fd,
        leaf,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    ) {
        Ok(fd) => fd,
        Err(Errno::NOENT | Errno::NOTDIR | Errno::LOOP) => return Ok(None),
        Err(err) => return Err(io::Error::from(err)),
    };
    let st_before = fstat(&src_fd).map_err(io::Error::from)?;
    if FileType::from_raw_mode(st_before.st_mode) != FileType::RegularFile {
        return Ok(None);
    }

    if st_before.st_size == 0 {
        return Ok(Some((StatSig::from_stat(&st_before), EMPTY_SHA256)));
    }

    let tmp_fd = openat(
        tmp_root_fd,
        c".",
        OFlags::TMPFILE | OFlags::RDWR | OFlags::CLOEXEC,
        Mode::RUSR | Mode::WUSR,
    )
    .map_err(io::Error::from)?;
    ioctl_ficlone(&tmp_fd, &src_fd).map_err(io::Error::from)?;
    let st_after = fstat(&src_fd).map_err(io::Error::from)?;

    let mut tmp_file = File::from(tmp_fd);
    tmp_file.seek(SeekFrom::Start(0))?;
    let hash = hash_reader(&mut tmp_file)?;
    Ok(Some((StatSig::from_stat(&st_after), hash)))
}

/// 按 `docs/architecture/lancet-node/shadow-workspace.md`“待定改动”的口径比较两条快照是否语义相等：
/// 逐路径按存在性、类型、权限、文件内容与符号链接目标独立比较，不比较硬链接关系与时间戳。
pub(crate) fn entries_semantically_equal_at(
    a: Option<&mut EntrySnapshot>,
    a_root_fd: BorrowedFd<'_>,
    b: Option<&mut EntrySnapshot>,
    b_root_fd: BorrowedFd<'_>,
    rel: &Path,
) -> io::Result<bool> {
    match (a, b) {
        (None, None) => Ok(true),
        (Some(a), Some(b)) => {
            if a.kind != b.kind {
                return Ok(false);
            }
            match a.kind {
                EntryKind::Dir => Ok(a.mode == b.mode),
                EntryKind::Symlink => Ok(a.link_target == b.link_target),
                EntryKind::File => {
                    if a.mode != b.mode || a.size != b.size {
                        return Ok(false);
                    }
                    let ha = a.ensure_hash_at(a_root_fd, rel)?;
                    let hb = b.ensure_hash_at(b_root_fd, rel)?;
                    Ok(ha == hb)
                }
            }
        }
        _ => Ok(false),
    }
}

/// 跟踪并持久化当前放宽了权限的只读目录（`shadows/<key>/widened`）。
///
/// 仅在放宽期间存在磁盘文件；所有放宽目录恢复原权限后自动删除该文件。
pub(crate) struct WidenedLog {
    shadow_root_fd: OwnedFd,
    tmp_root_fd: OwnedFd,
    active: BTreeMap<(TreeTag, PathBuf), u32>,
}

impl WidenedLog {
    pub fn new(shadow_root: &Path, tmp_root: &Path) -> io::Result<Self> {
        let shadow_root_fd = open_syncable_dir_fd(shadow_root)?;
        let tmp_root_fd = open_root_fd(tmp_root)?;
        Ok(Self {
            shadow_root_fd,
            tmp_root_fd,
            active: BTreeMap::new(),
        })
    }

    /// 放宽 `rel` 目录权限以允许写入/删除，放宽前把原权限持久落盘到 `shadows/<key>/widened`。
    /// 返回本次调用是否新加入了放宽记录（供单步操作结束后精准恢复）。
    pub fn widen_dir(
        &mut self,
        tag: TreeTag,
        root_fd: BorrowedFd<'_>,
        rel: &Path,
    ) -> io::Result<bool> {
        if rel.as_os_str().is_empty() {
            return Ok(false);
        }
        let key = (tag, rel.to_path_buf());
        if self.active.contains_key(&key) {
            return Ok(false);
        }
        let Some((parent_owned, leaf)) = open_parent_nofollow(root_fd, rel)? else {
            return Ok(false);
        };
        let parent_fd = parent_owned.as_ref().map_or(root_fd, AsFd::as_fd);
        let st = match statat(parent_fd, leaf, AtFlags::SYMLINK_NOFOLLOW) {
            Ok(st) => st,
            Err(Errno::NOENT | Errno::NOTDIR) => return Ok(false),
            Err(err) => return Err(io::Error::from(err)),
        };
        if FileType::from_raw_mode(st.st_mode) != FileType::Directory {
            return Ok(false);
        }
        let orig_mode = st.st_mode & 0o7777;
        if orig_mode & 0o700 == 0o700 {
            return Ok(false);
        }

        // 先落盘记录再放宽权限，确保放宽后立即 SIGKILL 也能在重启时恢复原权限。
        self.active.insert(key.clone(), orig_mode);
        if let Err(err) = self.persist() {
            self.active.remove(&key);
            return Err(err);
        }
        if let Err(err) = chmodat(
            parent_fd,
            leaf,
            Mode::from_raw_mode(orig_mode | 0o700),
            AtFlags::empty(),
        ) {
            self.active.remove(&key);
            let _ = self.persist();
            return Err(io::Error::from(err));
        }
        Ok(true)
    }

    /// 为新建或待后续恢复权限的目录登记最终权限（在放宽期间落盘到 `shadows/<key>/widened`）。
    pub fn register_deferred_mode(
        &mut self,
        tag: TreeTag,
        rel: &Path,
        final_mode: u32,
    ) -> io::Result<()> {
        let final_mode = final_mode & 0o7777;
        if final_mode & 0o700 == 0o700 {
            return Ok(());
        }
        let key = (tag, rel.to_path_buf());
        let prev = self.active.insert(key.clone(), final_mode);
        if prev != Some(final_mode)
            && let Err(err) = self.persist()
        {
            if let Some(old) = prev {
                self.active.insert(key, old);
            } else {
                self.active.remove(&key);
            }
            return Err(err);
        }
        Ok(())
    }

    /// 恢复单个目录的原权限并从 `shadows/<key>/widened` 中移除该条目。
    pub fn restore_dir(&mut self, tag: TreeTag, root_fd: BorrowedFd<'_>, rel: &Path) {
        let key = (tag, rel.to_path_buf());
        let Some(orig_mode) = self.active.remove(&key) else {
            return;
        };
        let _ = set_dir_mode_nofollow(root_fd, rel, orig_mode);
        let _ = self.persist();
    }

    /// 由深到浅恢复所有当前放宽的目录权限，随后删除 `shadows/<key>/widened`。
    pub fn restore_all(&mut self, tree_root_fd: BorrowedFd<'_>, base_root_fd: BorrowedFd<'_>) {
        if self.active.is_empty() {
            return;
        }
        let mut entries: Vec<((TreeTag, PathBuf), u32)> =
            std::mem::take(&mut self.active).into_iter().collect();
        entries.sort_by(|((ta, pa), _), ((tb, pb), _)| {
            path_depth(pb)
                .cmp(&path_depth(pa))
                .then_with(|| ta.cmp(tb))
                .then_with(|| pa.cmp(pb))
        });
        for ((tag, rel), mode) in entries {
            let root_fd = match tag {
                TreeTag::Tree => tree_root_fd,
                TreeTag::Base => base_root_fd,
            };
            let _ = set_dir_mode_nofollow(root_fd, &rel, mode);
        }
        let _ = self.persist();
    }

    fn persist(&self) -> io::Result<()> {
        if self.active.is_empty() {
            match unlinkat(self.shadow_root_fd.as_fd(), c"widened", AtFlags::empty()) {
                Ok(()) | Err(Errno::NOENT) => {}
                Err(err) => return Err(io::Error::from(err)),
            }
            sync_dir_fd(self.shadow_root_fd.as_fd())?;
            return Ok(());
        }

        let mut buf = String::new();
        for ((tag, rel), mode) in &self.active {
            let _ = writeln!(
                buf,
                "{} {:04o} {}",
                tag.as_str(),
                mode & 0o7777,
                encode_hex(rel.as_os_str().as_bytes())
            );
        }

        let (tmp_name, tmp_fd) = create_unique_tmp_file(self.tmp_root_fd.as_fd())?;
        let res = (|| -> io::Result<()> {
            let mut file = File::from(tmp_fd);
            file.write_all(buf.as_bytes())?;
            file.sync_all()?;
            drop(file);
            renameat(
                self.tmp_root_fd.as_fd(),
                &tmp_name,
                self.shadow_root_fd.as_fd(),
                c"widened",
            )
            .map_err(io::Error::from)?;
            sync_dir_fd(self.shadow_root_fd.as_fd())?;
            Ok(())
        })();
        if res.is_err() {
            let _ = unlinkat(self.tmp_root_fd.as_fd(), &tmp_name, AtFlags::empty());
        }
        res
    }
}

/// 启动时在重建索引之前按 `shadows/<key>/widened` 恢复未完成的目录权限放宽，随后删除该记录文件。
pub(crate) fn recover_widened_modes(
    shadow_root: &Path,
    tree_root_fd: BorrowedFd<'_>,
    base_root_fd: BorrowedFd<'_>,
) -> io::Result<()> {
    let widened_path = shadow_root.join("widened");
    let content = match fs::read_to_string(&widened_path) {
        Ok(c) => c,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(err) => return Err(err),
    };

    let mut entries: Vec<(TreeTag, PathBuf, u32)> = Vec::new();
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let mut parts = trimmed.split_whitespace();
        let (Some(tag_s), Some(mode_s), Some(hex_rel)) = (parts.next(), parts.next(), parts.next())
        else {
            continue;
        };
        let Some(tag) = TreeTag::parse(tag_s) else {
            continue;
        };
        let Ok(mode) = u32::from_str_radix(mode_s, 8) else {
            continue;
        };
        let Some(rel_bytes) = decode_hex(hex_rel) else {
            continue;
        };
        let rel = PathBuf::from(OsString::from_vec(rel_bytes));
        entries.push((tag, rel, mode & 0o7777));
    }

    entries.sort_by(|(ta, pa, _), (tb, pb, _)| {
        path_depth(pb)
            .cmp(&path_depth(pa))
            .then_with(|| ta.cmp(tb))
            .then_with(|| pa.cmp(pb))
    });

    for (tag, rel, mode) in entries {
        let root_fd = match tag {
            TreeTag::Tree => tree_root_fd,
            TreeTag::Base => base_root_fd,
        };
        let _ = set_dir_mode_nofollow(root_fd, &rel, mode);
    }

    match fs::remove_file(&widened_path) {
        Ok(()) => {}
        Err(err) if err.kind() == io::ErrorKind::NotFound => {}
        Err(err) => return Err(err),
    }
    let root_fd = open_syncable_dir_fd(shadow_root)?;
    sync_dir_fd(root_fd.as_fd())?;
    Ok(())
}

/// 启动时清空 `shadows/<key>/tmp/`（不存在时创建 `0700` 目录）。
pub(crate) fn clear_or_create_tmp_dir(tmp_dir: &Path) -> io::Result<()> {
    if tmp_dir.exists() {
        remove_tree_force(tmp_dir)?;
    }
    DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(tmp_dir)
}

/// 不跟随符号链接地设置 `root_fd/rel` 目录的权限位（非目录对象与符号链接直接跳过）。
pub(crate) fn set_dir_mode_nofollow(
    root_fd: BorrowedFd<'_>,
    rel: &Path,
    mode: u32,
) -> io::Result<()> {
    let Some((parent_owned, leaf)) = open_parent_nofollow(root_fd, rel)? else {
        return Ok(());
    };
    let parent_fd = parent_owned.as_ref().map_or(root_fd, AsFd::as_fd);
    let st = match statat(parent_fd, leaf, AtFlags::SYMLINK_NOFOLLOW) {
        Ok(st) => st,
        Err(Errno::NOENT | Errno::NOTDIR) => return Ok(()),
        Err(err) => return Err(io::Error::from(err)),
    };
    if FileType::from_raw_mode(st.st_mode) != FileType::Directory {
        return Ok(());
    }
    chmodat(
        parent_fd,
        leaf,
        Mode::from_raw_mode(mode & 0o7777),
        AtFlags::empty(),
    )
    .map_err(io::Error::from)
}

/// 把 `src_root_fd/src_rel` 经 `FICLONE` 克隆到 `tmp_root_fd`（`shadows/<key>/tmp/`）中的临时文件，
/// 再原子 `renameat` 到 `dst_root_fd/dst_rel`。全程不跟随中间符号链接。
#[allow(clippy::too_many_arguments)]
pub(crate) fn install_file_atomic_at(
    src_root_fd: BorrowedFd<'_>,
    src_rel: &Path,
    dst_tag: TreeTag,
    dst_root_fd: BorrowedFd<'_>,
    dst_rel: &Path,
    tmp_root_fd: BorrowedFd<'_>,
    widened: &mut WidenedLog,
    mode: u32,
    compute_hash: bool,
) -> io::Result<(StatSig, Option<[u8; 32]>)> {
    let dst_parent = dst_rel.parent().unwrap_or_else(|| Path::new(""));
    let did_widen = widened.widen_dir(dst_tag, dst_root_fd, dst_parent)?;

    let res = (|| -> io::Result<(StatSig, Option<[u8; 32]>)> {
        let Some((src_parent_owned, src_leaf)) = open_parent_nofollow(src_root_fd, src_rel)? else {
            return Err(io::Error::from(Errno::NOENT));
        };
        let src_parent_fd = src_parent_owned.as_ref().map_or(src_root_fd, AsFd::as_fd);
        let src_fd = openat(
            src_parent_fd,
            src_leaf,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(io::Error::from)?;
        let src_st = fstat(&src_fd).map_err(io::Error::from)?;

        let Some((dst_parent_owned, dst_leaf)) = open_parent_nofollow(dst_root_fd, dst_rel)? else {
            return Err(io::Error::from(Errno::NOENT));
        };
        let dst_parent_fd = dst_parent_owned.as_ref().map_or(dst_root_fd, AsFd::as_fd);

        let (tmp_name, tmp_fd) = create_unique_tmp_file(tmp_root_fd)?;
        let install_res = (|| -> io::Result<(StatSig, Option<[u8; 32]>)> {
            if src_st.st_size > 0 {
                ioctl_ficlone(&tmp_fd, &src_fd).map_err(io::Error::from)?;
            }
            let hash = if compute_hash {
                if src_st.st_size == 0 {
                    Some(EMPTY_SHA256)
                } else {
                    let mut f = File::from(
                        openat(
                            tmp_root_fd,
                            &tmp_name,
                            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                            Mode::empty(),
                        )
                        .map_err(io::Error::from)?,
                    );
                    Some(hash_reader(&mut f)?)
                }
            } else {
                None
            };
            fchmod(&tmp_fd, Mode::from_raw_mode(mode & 0o7777)).map_err(io::Error::from)?;
            let tmp_st = fstat(&tmp_fd).map_err(io::Error::from)?;
            drop(tmp_fd);
            renameat(tmp_root_fd, &tmp_name, dst_parent_fd, dst_leaf).map_err(io::Error::from)?;
            Ok((StatSig::from_stat(&tmp_st), hash))
        })();

        if install_res.is_err() {
            let _ = unlinkat(tmp_root_fd, &tmp_name, AtFlags::empty());
        }
        install_res
    })();

    if did_widen {
        widened.restore_dir(dst_tag, dst_root_fd, dst_parent);
    }
    res
}

/// 在 `tmp_root_fd`（`shadows/<key>/tmp/`）中创建临时符号链接，再原子 `renameat` 到 `dst_root_fd/dst_rel`。
pub(crate) fn install_symlink_atomic_at(
    target_bytes: &[u8],
    dst_tag: TreeTag,
    dst_root_fd: BorrowedFd<'_>,
    dst_rel: &Path,
    tmp_root_fd: BorrowedFd<'_>,
    widened: &mut WidenedLog,
) -> io::Result<StatSig> {
    let dst_parent = dst_rel.parent().unwrap_or_else(|| Path::new(""));
    let did_widen = widened.widen_dir(dst_tag, dst_root_fd, dst_parent)?;

    let res = (|| -> io::Result<StatSig> {
        let Some((dst_parent_owned, dst_leaf)) = open_parent_nofollow(dst_root_fd, dst_rel)? else {
            return Err(io::Error::from(Errno::NOENT));
        };
        let dst_parent_fd = dst_parent_owned.as_ref().map_or(dst_root_fd, AsFd::as_fd);

        let pid = std::process::id();
        let tmp_name = loop {
            let seq = ATOMIC_TMP_SEQ.fetch_add(1, Ordering::Relaxed);
            let candidate = format!(".lancet-s-{pid}-{seq}");
            match symlinkat(OsStr::from_bytes(target_bytes), tmp_root_fd, &candidate) {
                Ok(()) => break candidate,
                Err(Errno::EXIST) => continue,
                Err(err) => return Err(io::Error::from(err)),
            }
        };

        if let Err(err) = renameat(tmp_root_fd, &tmp_name, dst_parent_fd, dst_leaf) {
            let _ = unlinkat(tmp_root_fd, &tmp_name, AtFlags::empty());
            return Err(io::Error::from(err));
        }

        let st =
            statat(dst_parent_fd, dst_leaf, AtFlags::SYMLINK_NOFOLLOW).map_err(io::Error::from)?;
        Ok(StatSig::from_stat(&st))
    })();

    if did_widen {
        widened.restore_dir(dst_tag, dst_root_fd, dst_parent);
    }
    res
}

/// 在 `dst_root_fd/dst_rel` 安装目录（若原处为非目录则先删除），设置 `mode | 0o700` 以便写入子项，
/// 并在 `final_mode` 缺写位时提前登记到 `WidenedLog`（供 Pass 3 或崩溃恢复还原）。
pub(crate) fn install_dir_writable_at(
    dst_tag: TreeTag,
    dst_root_fd: BorrowedFd<'_>,
    dst_rel: &Path,
    widened: &mut WidenedLog,
    final_mode: u32,
) -> io::Result<()> {
    let dst_parent = dst_rel.parent().unwrap_or_else(|| Path::new(""));
    let did_widen = widened.widen_dir(dst_tag, dst_root_fd, dst_parent)?;

    let res = (|| -> io::Result<()> {
        let Some((dst_parent_owned, dst_leaf)) = open_parent_nofollow(dst_root_fd, dst_rel)? else {
            return Err(io::Error::from(Errno::NOENT));
        };
        let dst_parent_fd = dst_parent_owned.as_ref().map_or(dst_root_fd, AsFd::as_fd);
        let writable_mode = Mode::from_raw_mode((final_mode & 0o7777) | 0o700);

        widened.register_deferred_mode(dst_tag, dst_rel, final_mode)?;

        match statat(dst_parent_fd, dst_leaf, AtFlags::SYMLINK_NOFOLLOW) {
            Ok(st) if FileType::from_raw_mode(st.st_mode) == FileType::Directory => {
                chmodat(dst_parent_fd, dst_leaf, writable_mode, AtFlags::empty())
                    .map_err(io::Error::from)?;
            }
            Ok(_) => {
                unlinkat(dst_parent_fd, dst_leaf, AtFlags::empty()).map_err(io::Error::from)?;
                mkdirat(dst_parent_fd, dst_leaf, writable_mode).map_err(io::Error::from)?;
                chmodat(dst_parent_fd, dst_leaf, writable_mode, AtFlags::empty())
                    .map_err(io::Error::from)?;
            }
            Err(Errno::NOENT) => {
                mkdirat(dst_parent_fd, dst_leaf, writable_mode).map_err(io::Error::from)?;
                chmodat(dst_parent_fd, dst_leaf, writable_mode, AtFlags::empty())
                    .map_err(io::Error::from)?;
            }
            Err(err) => return Err(io::Error::from(err)),
        }
        Ok(())
    })();

    if did_widen {
        widened.restore_dir(dst_tag, dst_root_fd, dst_parent);
    }
    res
}

/// 删除 `root_fd/rel` 处的单个文件、符号链接或空目录（不跟随任何中间符号链接）；不存在则返回 `Ok(())`。
pub(crate) fn remove_single_entry_at(
    tag: TreeTag,
    root_fd: BorrowedFd<'_>,
    rel: &Path,
    widened: &mut WidenedLog,
) -> io::Result<()> {
    let parent = rel.parent().unwrap_or_else(|| Path::new(""));
    let did_widen = widened.widen_dir(tag, root_fd, parent)?;

    let res = (|| -> io::Result<()> {
        let Some((parent_owned, leaf)) = open_parent_nofollow(root_fd, rel)? else {
            return Ok(());
        };
        let parent_fd = parent_owned.as_ref().map_or(root_fd, AsFd::as_fd);
        match statat(parent_fd, leaf, AtFlags::SYMLINK_NOFOLLOW) {
            Ok(st) => {
                if FileType::from_raw_mode(st.st_mode) == FileType::Directory {
                    unlinkat(parent_fd, leaf, AtFlags::REMOVEDIR).map_err(io::Error::from)
                } else {
                    unlinkat(parent_fd, leaf, AtFlags::empty()).map_err(io::Error::from)
                }
            }
            Err(Errno::NOENT | Errno::NOTDIR) => Ok(()),
            Err(err) => Err(io::Error::from(err)),
        }
    })();

    if did_widen {
        widened.restore_dir(tag, root_fd, parent);
    }
    res
}

/// 递归删除 `root_fd/rel` 下的全部内容及目录本身（用于 `discard` 删除新增目录及其下 `regenerable`/特殊文件），
/// 全程不跟随任何符号链接。
pub(crate) fn remove_tree_nofollow_at(
    tag: TreeTag,
    root_fd: BorrowedFd<'_>,
    rel: &Path,
    widened: &mut WidenedLog,
) -> io::Result<()> {
    let Some((parent_owned, leaf)) = open_parent_nofollow(root_fd, rel)? else {
        return Ok(());
    };
    let parent_fd = parent_owned.as_ref().map_or(root_fd, AsFd::as_fd);
    let st = match statat(parent_fd, leaf, AtFlags::SYMLINK_NOFOLLOW) {
        Ok(st) => st,
        Err(Errno::NOENT | Errno::NOTDIR) => return Ok(()),
        Err(err) => return Err(io::Error::from(err)),
    };
    if FileType::from_raw_mode(st.st_mode) != FileType::Directory {
        return remove_single_entry_at(tag, root_fd, rel, widened);
    }

    // 先确保自身与上级都具备可遍历/可删权限。
    let did_widen_self = widened.widen_dir(tag, root_fd, rel)?;
    let child_res = (|| -> io::Result<()> {
        let Some(dir_fd) = open_dir_nofollow(root_fd, rel)? else {
            return Ok(());
        };
        let mut names = Vec::new();
        let mut dir = Dir::new(dir_fd).map_err(io::Error::from)?;
        while let Some(next) = dir.read() {
            let entry = next.map_err(io::Error::from)?;
            let bytes = entry.file_name().to_bytes();
            if bytes == b"." || bytes == b".." {
                continue;
            }
            names.push(OsString::from_vec(bytes.to_vec()));
        }
        drop(dir);

        for name in names {
            let child_rel = rel.join(&name);
            remove_tree_nofollow_at(tag, root_fd, &child_rel, widened)?;
        }
        Ok(())
    })();

    if child_res.is_err() {
        if did_widen_self {
            widened.restore_dir(tag, root_fd, rel);
        }
        return child_res;
    }

    // 先删除目录本身，删除成功后再从 widened 清除其记录；若删除失败则恢复其原权限。
    let rm_res = remove_single_entry_at(tag, root_fd, rel, widened);
    if rm_res.is_ok() {
        if did_widen_self {
            widened.active.remove(&(tag, rel.to_path_buf()));
            let _ = widened.persist();
        }
    } else if did_widen_self {
        widened.restore_dir(tag, root_fd, rel);
    }
    rm_res
}

/// 检查 `root_fd/rel` 作为真实目录时是否包含任意子项（不跟随符号链接）。
pub(crate) fn is_dir_non_empty_at(root_fd: BorrowedFd<'_>, rel: &Path) -> io::Result<bool> {
    let Some(dir_fd) = open_dir_nofollow(root_fd, rel)? else {
        return Ok(false);
    };
    let mut dir = Dir::new(dir_fd).map_err(io::Error::from)?;
    while let Some(next) = dir.read() {
        let entry = next.map_err(io::Error::from)?;
        let bytes = entry.file_name().to_bytes();
        if bytes != b"." && bytes != b".." {
            return Ok(true);
        }
    }
    Ok(false)
}

/// 递归删除临时初始化目录或 `tmp/` 目录（仅用于状态目录内非树路径）。
pub(crate) fn remove_tree_force(path: &Path) -> io::Result<()> {
    if !path.exists() && fs::symlink_metadata(path).is_err() {
        return Ok(());
    }
    fn chmod_dirs(dir: &Path) {
        let _ = fs::set_permissions(dir, fs::Permissions::from_mode(0o700));
        if let Ok(entries) = fs::read_dir(dir) {
            for entry in entries.flatten() {
                if let Ok(ft) = entry.file_type()
                    && ft.is_dir()
                {
                    chmod_dirs(&entry.path());
                }
            }
        }
    }
    if fs::symlink_metadata(path).is_ok_and(|m| m.is_dir()) {
        chmod_dirs(path);
        fs::remove_dir_all(path)
    } else {
        fs::remove_file(path)
    }
}

fn create_unique_tmp_file(tmp_root_fd: BorrowedFd<'_>) -> io::Result<(String, OwnedFd)> {
    let pid = std::process::id();
    loop {
        let seq = ATOMIC_TMP_SEQ.fetch_add(1, Ordering::Relaxed);
        let candidate = format!(".lancet-s-{pid}-{seq}");
        match openat(
            tmp_root_fd,
            &candidate,
            OFlags::RDWR | OFlags::CREATE | OFlags::EXCL | OFlags::CLOEXEC,
            Mode::RUSR | Mode::WUSR,
        ) {
            Ok(fd) => return Ok((candidate, fd)),
            Err(Errno::EXIST) => continue,
            Err(err) => return Err(io::Error::from(err)),
        }
    }
}

fn sync_dir_fd(dir_fd: BorrowedFd<'_>) -> io::Result<()> {
    rustix::fs::fsync(dir_fd).map_err(io::Error::from)
}

/// 初始化阶段直接把单个宿主文件经 `FICLONE` 复制到 `base` 再复制到 `tree`（或仅复制到 `tree` 作种子）。
pub(crate) fn clone_init_regular_file_at(
    host_root_fd: BorrowedFd<'_>,
    rel: &Path,
    base_dst: Option<&Path>,
    tree_dst: &Path,
    mode: u32,
) -> io::Result<(StatSig, Option<StatSig>, StatSig)> {
    let Some((host_parent_owned, host_leaf)) = open_parent_nofollow(host_root_fd, rel)? else {
        return Err(io::Error::from(Errno::NOENT));
    };
    let host_parent_fd = host_parent_owned.as_ref().map_or(host_root_fd, AsFd::as_fd);
    let host_fd = openat(
        host_parent_fd,
        host_leaf,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(io::Error::from)?;
    let host_st = fstat(&host_fd).map_err(io::Error::from)?;
    let file_mode = Mode::from_raw_mode(mode & 0o7777);

    if let Some(base_path) = base_dst {
        let base_fd = openat(
            CWD,
            base_path,
            OFlags::RDWR | OFlags::CREATE | OFlags::EXCL | OFlags::CLOEXEC,
            Mode::RUSR | Mode::WUSR,
        )
        .map_err(io::Error::from)?;
        if host_st.st_size > 0 {
            ioctl_ficlone(&base_fd, &host_fd).map_err(io::Error::from)?;
        }
        let tree_fd = openat(
            CWD,
            tree_dst,
            OFlags::RDWR | OFlags::CREATE | OFlags::EXCL | OFlags::CLOEXEC,
            Mode::RUSR | Mode::WUSR,
        )
        .map_err(io::Error::from)?;
        if host_st.st_size > 0 {
            ioctl_ficlone(&tree_fd, &base_fd).map_err(io::Error::from)?;
        }
        fchmod(&base_fd, file_mode).map_err(io::Error::from)?;
        fchmod(&tree_fd, file_mode).map_err(io::Error::from)?;
        let base_st = fstat(&base_fd).map_err(io::Error::from)?;
        let tree_st = fstat(&tree_fd).map_err(io::Error::from)?;
        Ok((
            StatSig::from_stat(&host_st),
            Some(StatSig::from_stat(&base_st)),
            StatSig::from_stat(&tree_st),
        ))
    } else {
        let tree_fd = openat(
            CWD,
            tree_dst,
            OFlags::RDWR | OFlags::CREATE | OFlags::EXCL | OFlags::CLOEXEC,
            Mode::RUSR | Mode::WUSR,
        )
        .map_err(io::Error::from)?;
        if host_st.st_size > 0 {
            ioctl_ficlone(&tree_fd, &host_fd).map_err(io::Error::from)?;
        }
        fchmod(&tree_fd, file_mode).map_err(io::Error::from)?;
        let tree_st = fstat(&tree_fd).map_err(io::Error::from)?;
        Ok((
            StatSig::from_stat(&host_st),
            None,
            StatSig::from_stat(&tree_st),
        ))
    }
}

/// 初始化阶段创建符号链接。
pub(crate) fn copy_init_symlink(target: &[u8], dst: &Path) -> io::Result<StatSig> {
    symlink(OsStr::from_bytes(target), dst)?;
    let m = fs::symlink_metadata(dst)?;
    Ok(StatSig::from_metadata(&m))
}

/// 初始化阶段为目录/文件恢复精确权限（仅操作尚未就位的临时树）。
pub(crate) fn set_init_mode(path: &Path, mode: u32) -> io::Result<()> {
    let m = match fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(err) => return Err(err),
    };
    if m.file_type().is_symlink() {
        return Ok(());
    }
    fs::set_permissions(path, fs::Permissions::from_mode(mode & 0o7777))
}

enum FileText {
    Absent,
    Text(String),
    NoPatch,
}

/// 为单条可见待定改动生成从基准到影子的 unified diff（上下文 3 行）；无文本块时返回 `None`。
pub(crate) fn compute_unified_diff_at(
    rel_utf8: &str,
    rel_path: &Path,
    action: PendingAction,
    base_root_fd: BorrowedFd<'_>,
    tree_root_fd: BorrowedFd<'_>,
) -> Option<String> {
    let base_text = match action {
        PendingAction::Added => FileText::Absent,
        PendingAction::Deleted | PendingAction::Modified => {
            read_utf8_text_if_regular_at(base_root_fd, rel_path)
        }
    };
    let tree_text = match action {
        PendingAction::Deleted => FileText::Absent,
        PendingAction::Added | PendingAction::Modified => {
            read_utf8_text_if_regular_at(tree_root_fd, rel_path)
        }
    };

    let (old_header, old_str, new_header, new_str) = match (&base_text, &tree_text) {
        (FileText::Absent, FileText::Text(new_s)) => (
            "/dev/null".to_owned(),
            "",
            format!("b/{rel_utf8}"),
            new_s.as_str(),
        ),
        (FileText::Text(old_s), FileText::Absent) => (
            format!("a/{rel_utf8}"),
            old_s.as_str(),
            "/dev/null".to_owned(),
            "",
        ),
        (FileText::Text(old_s), FileText::Text(new_s)) => {
            if old_s == new_s {
                return None;
            }
            (
                format!("a/{rel_utf8}"),
                old_s.as_str(),
                format!("b/{rel_utf8}"),
                new_s.as_str(),
            )
        }
        _ => return None,
    };

    let patch = TextDiff::from_lines(old_str, new_str)
        .unified_diff()
        .context_radius(3)
        .header(&old_header, &new_header)
        .to_string();
    if patch.is_empty() { None } else { Some(patch) }
}

/// 不跟随符号链接地读取普通文本文件：不存在返回 `Absent`，有效无 NUL 的 UTF-8 普通文件返回 `Text`，
/// 目录、符号链接、二进制或超 128 MiB 文件返回 `NoPatch`。
fn read_utf8_text_if_regular_at(root_fd: BorrowedFd<'_>, rel: &Path) -> FileText {
    let Ok(Some((parent_owned, leaf))) = open_parent_nofollow(root_fd, rel) else {
        return FileText::Absent;
    };
    let parent_fd = parent_owned.as_ref().map_or(root_fd, AsFd::as_fd);
    let st = match statat(parent_fd, leaf, AtFlags::SYMLINK_NOFOLLOW) {
        Ok(st) => st,
        Err(Errno::NOENT | Errno::NOTDIR) => return FileText::Absent,
        Err(_) => return FileText::NoPatch,
    };
    if FileType::from_raw_mode(st.st_mode) != FileType::RegularFile {
        return FileText::NoPatch;
    }
    if (st.st_size.max(0) as u64) > TOOL_MEMORY_LIMIT_BYTES as u64 {
        return FileText::NoPatch;
    }
    let Ok(fd) = openat(
        parent_fd,
        leaf,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    ) else {
        return FileText::NoPatch;
    };
    let mut file = File::from(fd);
    let mut bytes = Vec::with_capacity(st.st_size.max(0) as usize);
    if io::Read::read_to_end(&mut file, &mut bytes).is_err() {
        return FileText::NoPatch;
    }
    if bytes.contains(&0) {
        return FileText::NoPatch;
    }
    match String::from_utf8(bytes) {
        Ok(text) => FileText::Text(text),
        Err(_) => FileText::NoPatch,
    }
}

/// 相对路径转换为标准 `/` 分隔的 UTF-8 字符串（仅在 WIT 出口边界调用）。
pub(crate) fn rel_path_to_utf8(rel: &Path) -> Option<String> {
    let mut out = String::new();
    for comp in rel.components() {
        let Component::Normal(s) = comp else {
            continue;
        };
        let part = s.to_str()?;
        if !out.is_empty() {
            out.push('/');
        }
        out.push_str(part);
    }
    Some(out)
}

/// 路径深度（目录分量数减 1），用于按目录层次排序。
pub(crate) fn path_depth(rel: &Path) -> usize {
    rel.as_os_str()
        .as_bytes()
        .iter()
        .filter(|&&b| b == b'/')
        .count()
}

/// 返回 `map` 中位于 `dir/` 之下的全部严格后代路径。
pub(crate) fn descendants_in_map<V>(map: &BTreeMap<PathBuf, V>, dir: &Path) -> Vec<PathBuf> {
    map.range(dir.to_path_buf()..)
        .take_while(|(k, _)| k.starts_with(dir))
        .filter(|(k, _)| k.as_path() != dir)
        .map(|(k, _)| k.clone())
        .collect()
}

/// 判断 `map` 中是否存在任何位于 `dir/` 之下的严格后代路径。
pub(crate) fn has_descendant_in_map<V>(map: &BTreeMap<PathBuf, V>, dir: &Path) -> bool {
    map.range(dir.to_path_buf()..)
        .take_while(|(k, _)| k.starts_with(dir))
        .any(|(k, _)| k.as_path() != dir)
}

fn encode_hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(&mut out, "{b:02x}");
    }
    out
}

fn decode_hex(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) {
        return None;
    }
    let mut out = Vec::with_capacity(s.len() / 2);
    let raw = s.as_bytes();
    for chunk in raw.chunks_exact(2) {
        let hi = hex_nibble(chunk[0])?;
        let lo = hex_nibble(chunk[1])?;
        out.push((hi << 4) | lo);
    }
    Some(out)
}

fn hex_nibble(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}
