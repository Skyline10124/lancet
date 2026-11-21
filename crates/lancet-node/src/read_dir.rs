// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 宿主目录资源与共用路径解析：基于 `openat2(RESOLVE_BENEATH | RESOLVE_NO_SYMLINKS)` 的逐分量安全解析、
//! 只读文件/目录访问与原子写入。

use std::collections::VecDeque;
use std::ffi::{OsStr, OsString};
use std::io::{Read as _, Write as _};
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::Context as _;
use rustix::fs::{
    AtFlags, CWD, Dir, FileType, Mode, OFlags, ResolveFlags, fchmod, fstat, mkdirat, open, openat,
    openat2, readlinkat, renameat, unlinkat,
};
use rustix::io::Errno;

use crate::dispatch::TOOL_MEMORY_LIMIT_BYTES;

/// 跟随符号链接的最大累计跳转次数，与 Linux `MAXSYMLINKS` 对齐。
const MAX_SYMLINK_DEPTH: usize = 40;
/// 单个路径分量的最大字节数，与 Linux `NAME_MAX` 对齐。
const MAX_PATH_COMPONENT_BYTES: usize = 255;
/// 目标绝对路径的最大字节数，与 Linux `PATH_MAX`（4,096 字节，含结尾 NUL）对齐。
const MAX_ABS_PATH_BYTES: usize = 4095;

static WRITE_TMP_SEQ: AtomicU64 = AtomicU64::new(0);

/// `entries` 返回的直接子项元数据。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirEntry {
    pub name: String,
    pub is_dir: bool,
    pub is_link: bool,
}

/// `HostDir::write_file` 的写入结果。
pub(crate) struct WriteFileResult {
    /// 实际写入的根相对路径（有效 UTF-8）。
    pub real_rel: String,
    /// 文件是否实际发生变化（内容相同时为 `false`，未落盘）。
    pub changed: bool,
}

/// 绑定到单个经核验宿主工作目录根 fd 的目录资源，读与写共用同一个根 fd。
#[derive(Clone)]
pub struct HostDir {
    inner: Arc<HostDirInner>,
}

struct HostDirInner {
    name: String,
    root_path: PathBuf,
    root_fd: OwnedFd,
}

struct FoundTarget {
    /// `None` 表示根目录本身，使用 `root_fd`。
    fd: Option<OwnedFd>,
    real_rel: PathBuf,
    file_type: FileType,
}

impl FoundTarget {
    fn as_fd<'a>(&'a self, root_fd: &'a OwnedFd) -> BorrowedFd<'a> {
        self.fd
            .as_ref()
            .map_or_else(|| root_fd.as_fd(), AsFd::as_fd)
    }
}

impl HostDir {
    /// 打开给定宿主目录根的 `O_PATH` fd。
    pub fn open(name: &str, canonical_root: &Path) -> anyhow::Result<Self> {
        let root_fd = openat2(
            CWD,
            canonical_root,
            OFlags::PATH | OFlags::DIRECTORY | OFlags::CLOEXEC,
            Mode::empty(),
            ResolveFlags::NO_SYMLINKS,
        )
        .with_context(|| format!("打开目录 `{name}` 失败"))?;
        Ok(Self {
            inner: Arc::new(HostDirInner {
                name: name.to_owned(),
                root_path: canonical_root.to_path_buf(),
                root_fd,
            }),
        })
    }

    /// 返回该目录在配置中的句柄名。
    pub fn name(&self) -> &str {
        &self.inner.name
    }

    /// 返回该目录在宿主上的真实绝对路径。
    pub fn root_path(&self) -> &Path {
        &self.inner.root_path
    }

    /// 返回该目录已核验的 `O_PATH` 根 fd。
    pub fn root_fd(&self) -> BorrowedFd<'_> {
        self.inner.root_fd.as_fd()
    }

    /// 读取目录内 `path` 处的 UTF-8 文本文件；目标不存在（`ENOENT`）时返回 `Ok(None)`。
    pub fn read(&self, path: &str) -> Result<Option<String>, String> {
        let Some(target) = self.resolve_target(path)? else {
            return Ok(None);
        };

        if target.file_type == FileType::Directory {
            return Err(format!("目标 `{path}` 是目录，不是文件"));
        }
        if target.file_type != FileType::RegularFile {
            return Err(format!("目标 `{path}` 不是普通文件"));
        }

        let proc_path = format!(
            "/proc/self/fd/{}",
            target.as_fd(&self.inner.root_fd).as_raw_fd()
        );
        let rd_fd = open(proc_path, OFlags::RDONLY | OFlags::CLOEXEC, Mode::empty())
            .map_err(|err| format!("打开文件 `{path}` 失败：{err}"))?;
        drop(target);

        let limit = TOOL_MEMORY_LIMIT_BYTES as u64;
        let file = std::fs::File::from(rd_fd);
        let mut buf = Vec::new();
        file.take(limit + 1)
            .read_to_end(&mut buf)
            .map_err(|err| format!("读取文件 `{path}` 失败：{err}"))?;
        if buf.len() as u64 > limit {
            return Err(format!("文件 `{path}` 大小超过单次读取上限 {limit} 字节"));
        }

        String::from_utf8(buf)
            .map(Some)
            .map_err(|err| format!("文件 `{path}` 不是有效的 UTF-8 文本：{err}"))
    }

    /// 列出目录内 `path` 处的直接子项；目标不存在（`ENOENT`）时返回 `Ok(None)`。
    pub fn entries(&self, path: &str) -> Result<Option<Vec<DirEntry>>, String> {
        let (access_dir_rel, require_dir) = self.lexical_rel(path)?;
        let Some(target) = self.resolve_rel(&access_dir_rel, require_dir, path)? else {
            return Ok(None);
        };

        if target.file_type != FileType::Directory {
            return Err(format!("路径 `{path}` 不是目录"));
        }

        let rd_fd = openat(
            target.as_fd(&self.inner.root_fd),
            c".",
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(|err| format!("打开目录 `{path}` 失败：{err}"))?;
        let mut dir =
            Dir::new(rd_fd).map_err(|err| format!("读取目录 `{path}` 条目失败：{err}"))?;

        let mut entries = Vec::new();
        while let Some(next) = dir.read() {
            let entry = next.map_err(|err| format!("遍历目录 `{path}` 失败：{err}"))?;
            let name_bytes = entry.file_name().to_bytes();
            if name_bytes == b"." || name_bytes == b".." {
                continue;
            }

            let name_os = OsStr::from_bytes(name_bytes);
            let access_child = join_rel(&access_dir_rel, name_os);
            let real_child = join_rel(&target.real_rel, name_os);

            let Ok(child_fd) = self.open_rel_component(&real_child, OFlags::empty()) else {
                continue;
            };
            let Ok(st) = fstat(&child_fd) else {
                continue;
            };
            let ft = FileType::from_raw_mode(st.st_mode);
            drop(child_fd);

            let (is_dir, is_link) = match ft {
                FileType::RegularFile | FileType::Directory => (ft == FileType::Directory, false),
                FileType::Symlink => {
                    let Ok(Some(found)) = self.resolve_rel(&access_child, false, path) else {
                        continue;
                    };
                    if !matches!(found.file_type, FileType::RegularFile | FileType::Directory) {
                        continue;
                    }
                    (found.file_type == FileType::Directory, true)
                }
                _ => continue,
            };

            let Ok(name) = std::str::from_utf8(name_bytes) else {
                return Err(format!("目录 `{path}` 中有名称不是有效 UTF-8 的条目"));
            };
            entries.push(DirEntry {
                name: name.to_owned(),
                is_dir,
                is_link,
            });
        }

        entries.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(Some(entries))
    }

    /// 逐分量核验越界后，在已核验的上级目录内写临时文件并改名就位；内容相同时不写。
    pub(crate) fn write_file(&self, path: &str, content: &str) -> Result<WriteFileResult, String> {
        let (access_rel, require_dir) = self.lexical_rel(path)?;
        if access_rel.as_os_str().is_empty() {
            return Err(format!("目标 `{path}` 是目录，无法作为文件写入"));
        }

        let mut queue = rel_to_queue(&access_rel, require_dir);
        let mut dirs: Vec<OsString> = Vec::new();
        let mut symlinks = 0usize;

        while let Some(comp) = queue.pop_front() {
            if comp.as_os_str() == OsStr::new(".") {
                continue;
            }
            if comp.as_os_str() == OsStr::new("..") {
                if dirs.pop().is_none() {
                    return Err(self.out_of_bounds_msg(path));
                }
                continue;
            }
            if comp.as_os_str().as_bytes().contains(&0) {
                return Err(format!("路径 `{path}` 包含非法空字节"));
            }

            let mut loc = parts_to_path(&dirs);
            loc.push(&comp);

            match self.open_rel_component(&loc, OFlags::empty()) {
                Ok(fd) => {
                    let st =
                        fstat(&fd).map_err(|err| format!("读取路径 `{path}` 属性失败：{err}"))?;
                    let ft = FileType::from_raw_mode(st.st_mode);
                    let is_dir = ft == FileType::Directory;

                    if ft == FileType::Symlink {
                        symlinks += 1;
                        if symlinks > MAX_SYMLINK_DEPTH {
                            return Err(format!("解析路径 `{path}` 时符号链接层数过多"));
                        }
                        self.push_symlink_target(&fd, path, &mut queue)?;
                    } else if is_dir && !queue.is_empty() {
                        dirs.push(comp);
                    } else if is_dir {
                        return Err(format!("目标 `{path}` 是目录，无法作为文件写入"));
                    } else if !queue.is_empty() {
                        if queue.iter().all(|c| c.as_os_str() == OsStr::new(".")) {
                            return Err(format!("路径 `{path}` 不是目录"));
                        }
                        return Err(format!("路径 `{path}` 的上级不是目录"));
                    } else {
                        if ft != FileType::RegularFile {
                            return Err(format!("目标 `{path}` 不是普通文件，无法写入"));
                        }
                        self.check_write_path_limits(&loc, path)?;
                        let real_rel = loc
                            .to_str()
                            .ok_or_else(|| format!("写入目标 `{path}` 解析后的路径不是有效 UTF-8"))?
                            .to_owned();

                        // 大小一致时必须实际读取比较内容；读不了则报错，只有确认内容不同才替换。
                        if u64::try_from(st.st_size).ok() == Some(content.len() as u64) {
                            let proc_path = format!("/proc/self/fd/{}", fd.as_raw_fd());
                            let rd_fd =
                                open(proc_path, OFlags::RDONLY | OFlags::CLOEXEC, Mode::empty())
                                    .map_err(|err| {
                                        format!("打开文件 `{path}` 比较内容失败：{err}")
                                    })?;
                            let mut existing = Vec::with_capacity(content.len());
                            std::fs::File::from(rd_fd)
                                .take(content.len() as u64 + 1)
                                .read_to_end(&mut existing)
                                .map_err(|err| format!("读取文件 `{path}` 比较内容失败：{err}"))?;
                            if existing == content.as_bytes() {
                                return Ok(WriteFileResult {
                                    real_rel,
                                    changed: false,
                                });
                            }
                        }
                        drop(fd);

                        let existing_mode = Mode::from_raw_mode(st.st_mode & 0o7777);
                        let parent_owned = self.open_verified_parent(&dirs, path)?;
                        let parent_fd = parent_owned
                            .as_ref()
                            .map_or_else(|| self.inner.root_fd.as_fd(), AsFd::as_fd);
                        write_atomic_in_dir(
                            parent_fd,
                            comp.as_os_str(),
                            content.as_bytes(),
                            Some(existing_mode),
                            path,
                        )?;
                        return Ok(WriteFileResult {
                            real_rel,
                            changed: true,
                        });
                    }
                }
                Err(Errno::NOENT) => {
                    // 先校验所有待创建的缺失段，再实际创建上级目录。
                    let mut remaining = Vec::with_capacity(queue.len() + 1);
                    remaining.push(comp);
                    remaining.extend(queue);

                    let mut sim_parts = dirs.clone();
                    let mut has_parent_dir_segment = false;
                    let mut clean_remaining = Vec::with_capacity(remaining.len());
                    let mut trailing_dir = false;
                    for item in &remaining {
                        if item.as_os_str() == OsStr::new(".") {
                            trailing_dir = true;
                            continue;
                        }
                        trailing_dir = false;
                        if item.as_os_str() == OsStr::new("..") {
                            if sim_parts.pop().is_none() {
                                return Err(self.out_of_bounds_msg(path));
                            }
                            has_parent_dir_segment = true;
                            continue;
                        }
                        if item.as_os_str().as_bytes().contains(&0) {
                            return Err(format!("路径 `{path}` 包含非法空字节"));
                        }
                        sim_parts.push(item.clone());
                        clean_remaining.push(item.clone());
                    }
                    if has_parent_dir_segment {
                        return Err(format!("路径 `{path}` 在缺失目录后包含 `..`"));
                    }
                    if trailing_dir {
                        return Err(format!("路径 `{path}` 末尾要求目录，无法写入普通文件"));
                    }
                    let final_rel_path = parts_to_path(&sim_parts);
                    self.check_write_path_limits(&final_rel_path, path)?;
                    let real_rel = final_rel_path
                        .to_str()
                        .ok_or_else(|| format!("写入目标 `{path}` 解析后的路径不是有效 UTF-8"))?
                        .to_owned();

                    let (leaf, missing_parents) = clean_remaining
                        .split_last()
                        .expect("缺失路径至少包含一个叶文件名");

                    let mut cur_owned = self.open_verified_parent(&dirs, path)?;
                    for dir_comp in missing_parents {
                        let parent_fd = cur_owned
                            .as_ref()
                            .map_or_else(|| self.inner.root_fd.as_fd(), AsFd::as_fd);
                        match mkdirat(parent_fd, dir_comp.as_os_str(), Mode::from_raw_mode(0o755)) {
                            Ok(()) | Err(Errno::EXIST) => {}
                            Err(err) => {
                                return Err(format!("为路径 `{path}` 创建上级目录失败：{err}"));
                            }
                        }
                        let next_fd = openat2(
                            parent_fd,
                            dir_comp.as_os_str(),
                            OFlags::PATH | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                            Mode::empty(),
                            ResolveFlags::BENEATH | ResolveFlags::NO_SYMLINKS,
                        )
                        .map_err(|err| format!("打开新建上级目录 `{path}` 失败：{err}"))?;
                        cur_owned = Some(next_fd);
                    }

                    let parent_fd = cur_owned
                        .as_ref()
                        .map_or_else(|| self.inner.root_fd.as_fd(), AsFd::as_fd);
                    write_atomic_in_dir(
                        parent_fd,
                        leaf.as_os_str(),
                        content.as_bytes(),
                        None,
                        path,
                    )?;

                    return Ok(WriteFileResult {
                        real_rel,
                        changed: true,
                    });
                }
                Err(Errno::NOTDIR) => return Err(format!("路径 `{path}` 的上级不是目录")),
                Err(Errno::NAMETOOLONG) => {
                    return Err(format!("写入路径 `{path}` 超出系统路径长度上限"));
                }
                Err(Errno::LOOP | Errno::XDEV) => return Err(self.out_of_bounds_msg(path)),
                Err(err) => return Err(format!("访问路径 `{path}` 失败：{err}")),
            }
        }

        Err(format!("目标 `{path}` 是目录，无法作为文件写入"))
    }

    /// 核对写入目标各分量不超过 `NAME_MAX`（255 字节），且宿主绝对路径不超过 `PATH_MAX - 1`（4095 字节）。
    ///
    /// 报错只写出调用者传入的 `raw_path`，不泄露宿主目录的绝对路径。
    fn check_write_path_limits(&self, real_rel: &Path, raw_path: &str) -> Result<(), String> {
        for comp in real_rel.components() {
            if let Component::Normal(name) = comp
                && name.as_bytes().len() > MAX_PATH_COMPONENT_BYTES
            {
                return Err(format!(
                    "写入路径 `{raw_path}` 包含超过 {MAX_PATH_COMPONENT_BYTES} 字节的分量"
                ));
            }
        }
        let abs_len = self
            .inner
            .root_path
            .join(real_rel)
            .as_os_str()
            .as_bytes()
            .len();
        if abs_len > MAX_ABS_PATH_BYTES {
            return Err(format!(
                "写入路径 `{raw_path}` 超出 {MAX_ABS_PATH_BYTES} 字节路径长度上限"
            ));
        }
        Ok(())
    }

    fn open_verified_parent(
        &self,
        dirs: &[OsString],
        raw_path: &str,
    ) -> Result<Option<OwnedFd>, String> {
        if dirs.is_empty() {
            return Ok(None);
        }
        let parent_rel = parts_to_path(dirs);
        let fd = openat2(
            self.inner.root_fd.as_fd(),
            &parent_rel,
            OFlags::PATH | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
            ResolveFlags::BENEATH | ResolveFlags::NO_SYMLINKS,
        )
        .map_err(|err| format!("打开路径 `{raw_path}` 的上级目录失败：{err}"))?;
        Ok(Some(fd))
    }

    fn resolve_target(&self, path: &str) -> Result<Option<FoundTarget>, String> {
        let (access_rel, require_dir) = self.lexical_rel(path)?;
        self.resolve_rel(&access_rel, require_dir, path)
    }

    fn resolve_rel(
        &self,
        access_rel: &Path,
        require_dir: bool,
        err_path: &str,
    ) -> Result<Option<FoundTarget>, String> {
        let queue = rel_to_queue(access_rel, require_dir);
        self.walk(err_path, queue)
    }

    fn walk(
        &self,
        raw_path: &str,
        mut queue: VecDeque<OsString>,
    ) -> Result<Option<FoundTarget>, String> {
        let mut dirs: Vec<OsString> = Vec::new();
        let mut symlinks = 0usize;

        while let Some(comp) = queue.pop_front() {
            if comp.as_os_str() == OsStr::new(".") {
                continue;
            }
            if comp.as_os_str() == OsStr::new("..") {
                if dirs.pop().is_none() {
                    return Err(self.out_of_bounds_msg(raw_path));
                }
                continue;
            }

            if comp.as_os_str().as_bytes().contains(&0) {
                return Err(format!("路径 `{raw_path}` 包含非法空字节"));
            }

            let mut loc = parts_to_path(&dirs);
            loc.push(&comp);

            let fd = match self.open_rel_component(&loc, OFlags::empty()) {
                Ok(fd) => fd,
                Err(Errno::NOENT) => return Ok(None),
                Err(Errno::NOTDIR) => return Err(format!("路径 `{raw_path}` 的上级不是目录")),
                Err(Errno::LOOP | Errno::XDEV) => return Err(self.out_of_bounds_msg(raw_path)),
                Err(err) => return Err(format!("访问路径 `{raw_path}` 失败：{err}")),
            };

            let st = fstat(&fd).map_err(|err| format!("读取路径 `{raw_path}` 属性失败：{err}"))?;
            let ft = FileType::from_raw_mode(st.st_mode);

            if ft == FileType::Symlink {
                symlinks += 1;
                if symlinks > MAX_SYMLINK_DEPTH {
                    return Err(format!("解析路径 `{raw_path}` 时符号链接层数过多"));
                }
                self.push_symlink_target(&fd, raw_path, &mut queue)?;
            } else if ft == FileType::Directory && !queue.is_empty() {
                dirs.push(comp);
            } else if !queue.is_empty() {
                if queue.iter().all(|c| c.as_os_str() == OsStr::new(".")) {
                    return Err(format!("路径 `{raw_path}` 不是目录"));
                }
                return Err(format!("路径 `{raw_path}` 的上级不是目录"));
            } else {
                return Ok(Some(FoundTarget {
                    fd: Some(fd),
                    real_rel: loc,
                    file_type: ft,
                }));
            }
        }

        let real_rel = parts_to_path(&dirs);
        if real_rel.as_os_str().is_empty() {
            return Ok(Some(FoundTarget {
                fd: None,
                real_rel,
                file_type: FileType::Directory,
            }));
        }

        let fd = match self.open_rel_component(&real_rel, OFlags::DIRECTORY) {
            Ok(fd) => fd,
            Err(Errno::NOENT) => return Ok(None),
            Err(Errno::NOTDIR) => return Err(format!("路径 `{raw_path}` 不是目录")),
            Err(Errno::LOOP | Errno::XDEV) => return Err(self.out_of_bounds_msg(raw_path)),
            Err(err) => return Err(format!("访问路径 `{raw_path}` 失败：{err}")),
        };
        let st = fstat(&fd).map_err(|err| format!("读取路径 `{raw_path}` 属性失败：{err}"))?;
        let ft = FileType::from_raw_mode(st.st_mode);
        Ok(Some(FoundTarget {
            fd: Some(fd),
            real_rel,
            file_type: ft,
        }))
    }

    fn push_symlink_target(
        &self,
        fd: &OwnedFd,
        raw_path: &str,
        queue: &mut VecDeque<OsString>,
    ) -> Result<(), String> {
        let target_cstr = readlinkat(fd, c"", Vec::new())
            .map_err(|err| format!("读取路径 `{raw_path}` 符号链接目标失败：{err}"))?;
        let target_bytes = target_cstr.as_bytes();
        if target_bytes.first() == Some(&b'/') {
            return Err(self.out_of_bounds_msg(raw_path));
        }
        let target_os = OsStr::from_bytes(target_bytes);
        let mut target_parts = Vec::new();
        for part in Path::new(target_os).components() {
            match part {
                Component::Prefix(_) | Component::RootDir => {
                    return Err(self.out_of_bounds_msg(raw_path));
                }
                Component::CurDir => {}
                Component::Normal(s) => target_parts.push(s.to_os_string()),
                Component::ParentDir => target_parts.push(OsString::from("..")),
            }
        }
        if target_bytes.ends_with(b"/") || target_bytes == b"." || target_bytes.ends_with(b"/.") {
            target_parts.push(OsString::from("."));
        }
        for part in target_parts.into_iter().rev() {
            queue.push_front(part);
        }
        Ok(())
    }

    fn open_rel_component(&self, rel: &Path, extra_flags: OFlags) -> rustix::io::Result<OwnedFd> {
        openat2(
            self.inner.root_fd.as_fd(),
            rel,
            OFlags::PATH | OFlags::NOFOLLOW | OFlags::CLOEXEC | extra_flags,
            Mode::empty(),
            ResolveFlags::BENEATH | ResolveFlags::NO_SYMLINKS,
        )
    }

    fn lexical_rel(&self, path: &str) -> Result<(PathBuf, bool), String> {
        let mut rel = PathBuf::new();
        for comp in Path::new(path).components() {
            match comp {
                Component::Prefix(_) | Component::RootDir | Component::ParentDir => {
                    return Err(self.out_of_bounds_msg(path));
                }
                Component::CurDir => {}
                Component::Normal(part) => rel.push(part),
            }
        }
        let require_dir = path.ends_with('/') || path.ends_with("/.");
        Ok((rel, require_dir))
    }

    fn out_of_bounds_msg(&self, path: &str) -> String {
        format!("路径 `{path}` 越出目录 `{}` 范围", self.inner.name)
    }
}

fn write_atomic_in_dir(
    parent_fd: BorrowedFd<'_>,
    leaf: &OsStr,
    bytes: &[u8],
    preserve_mode: Option<Mode>,
    raw_path: &str,
) -> Result<(), String> {
    let pid = std::process::id();
    let (tmp_name, tmp_fd) = loop {
        let seq = WRITE_TMP_SEQ.fetch_add(1, Ordering::Relaxed);
        let candidate = format!(".lancet-w-{pid}-{seq}");
        if OsStr::new(&candidate) == leaf {
            continue;
        }
        let mode = preserve_mode.unwrap_or_else(|| Mode::from_raw_mode(0o644));
        match openat(
            parent_fd,
            &candidate,
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::CLOEXEC,
            mode,
        ) {
            Ok(fd) => break (candidate, fd),
            Err(Errno::EXIST) => continue,
            Err(err) => return Err(format!("创建文件 `{raw_path}` 临时文件失败：{err}")),
        }
    };

    let write_res = (|| -> Result<(), String> {
        let mut file = std::fs::File::from(tmp_fd);
        file.write_all(bytes)
            .map_err(|err| format!("写入文件 `{raw_path}` 失败：{err}"))?;
        if let Some(mode) = preserve_mode {
            fchmod(&file, mode).map_err(|err| format!("恢复文件 `{raw_path}` 权限失败：{err}"))?;
        }
        drop(file);
        renameat(parent_fd, &tmp_name, parent_fd, leaf)
            .map_err(|err| format!("替换文件 `{raw_path}` 失败：{err}"))
    })();

    if write_res.is_err() {
        let _ = unlinkat(parent_fd, &tmp_name, AtFlags::empty());
    }
    write_res
}

fn join_rel(base: &Path, child: &OsStr) -> PathBuf {
    if base.as_os_str().is_empty() {
        PathBuf::from(child)
    } else if child.is_empty() {
        base.to_path_buf()
    } else {
        base.join(child)
    }
}

fn parts_to_path(parts: &[OsString]) -> PathBuf {
    let mut path = PathBuf::new();
    for part in parts {
        path.push(part);
    }
    path
}

fn rel_components(rel: &Path) -> Vec<&OsStr> {
    rel.components()
        .filter_map(|c| match c {
            Component::Normal(s) => Some(s),
            _ => None,
        })
        .collect()
}

fn rel_to_queue(rel: &Path, require_dir: bool) -> VecDeque<OsString> {
    let mut queue: VecDeque<OsString> = rel_components(rel)
        .into_iter()
        .map(OsStr::to_os_string)
        .collect();
    if require_dir {
        queue.push_back(OsString::from("."));
    }
    queue
}
