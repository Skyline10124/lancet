// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 只读目录宿主资源实现：基于 `openat2(RESOLVE_BENEATH | RESOLVE_NO_SYMLINKS)` 的逐分量安全解析与 `hidden` 过滤。

use std::collections::VecDeque;
use std::ffi::{OsStr, OsString};
use std::io::Read as _;
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use anyhow::Context as _;
use ignore::gitignore::{Gitignore, GitignoreBuilder};
use rustix::fs::{
    CWD, Dir, FileType, Mode, OFlags, ResolveFlags, fstat, open, openat, openat2, readlinkat,
};
use rustix::io::Errno;

use crate::dispatch::TOOL_MEMORY_LIMIT_BYTES;

/// 跟随符号链接的最大累计跳转次数，与 Linux `MAXSYMLINKS` 对齐。
const MAX_SYMLINK_DEPTH: usize = 40;

/// `entries` 返回的直接子项元数据。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirEntry {
    pub name: String,
    pub is_dir: bool,
    pub is_link: bool,
}

/// 绑定到单个只读工作目录的宿主资源。
#[derive(Clone)]
pub struct ReadDir {
    inner: Arc<ReadDirInner>,
}

struct ReadDirInner {
    name: String,
    root_fd: OwnedFd,
    hidden: Gitignore,
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

enum WalkOutcome {
    Found(FoundTarget),
    Missing,
}

impl ReadDir {
    /// 打开只读目录句柄并构建配置中的 `hidden` 规则匹配器。
    pub fn open(
        name: &str,
        canonical_root: &Path,
        hidden_patterns: &[String],
    ) -> anyhow::Result<Self> {
        let root_fd = openat2(
            CWD,
            canonical_root,
            OFlags::PATH | OFlags::DIRECTORY | OFlags::CLOEXEC,
            Mode::empty(),
            ResolveFlags::NO_SYMLINKS,
        )
        .with_context(|| format!("打开只读目录 `{name}` 失败"))?;

        let mut builder = GitignoreBuilder::new(".");
        for pattern in hidden_patterns {
            builder
                .add_line(None, pattern)
                .with_context(|| format!("目录 `{name}` 的 hidden 规则 `{pattern}` 无效"))?;
        }
        let hidden = builder
            .build()
            .with_context(|| format!("构建目录 `{name}` 的 hidden 匹配器失败"))?;

        Ok(Self {
            inner: Arc::new(ReadDirInner {
                name: name.to_owned(),
                root_fd,
                hidden,
            }),
        })
    }

    /// 返回该只读目录在配置中的句柄名。
    pub fn name(&self) -> &str {
        &self.inner.name
    }

    /// 读取目录内 `path` 处的文本文件；目标不存在且未命中 `hidden` 时返回 `Ok(None)`。
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

    /// 列出目录内 `path` 处的直接子项；目标不存在且未命中 `hidden` 时返回 `Ok(None)`。
    pub fn entries(&self, path: &str) -> Result<Option<Vec<DirEntry>>, String> {
        let access_dir_rel = self.lexical_rel(path)?;
        let Some(target) = self.resolve_rel(&access_dir_rel, path)? else {
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
                FileType::RegularFile | FileType::Directory => {
                    let is_dir = ft == FileType::Directory;
                    if self.is_hidden(&real_child, is_dir) || self.is_hidden(&access_child, is_dir)
                    {
                        continue;
                    }
                    (is_dir, false)
                }
                FileType::Symlink => {
                    let Ok(Some(found)) = self.resolve_rel(&access_child, path) else {
                        continue;
                    };
                    if !matches!(found.file_type, FileType::RegularFile | FileType::Directory) {
                        continue;
                    }
                    (found.file_type == FileType::Directory, true)
                }
                _ => continue,
            };

            // `hidden` 检查通过后再做 UTF-8 校验：被隐藏或略去的非 UTF-8 条目保持不可见。
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

    fn resolve_target(&self, path: &str) -> Result<Option<FoundTarget>, String> {
        let access_rel = self.lexical_rel(path)?;
        self.resolve_rel(&access_rel, path)
    }

    fn resolve_rel(
        &self,
        access_rel: &Path,
        err_path: &str,
    ) -> Result<Option<FoundTarget>, String> {
        let queue = rel_to_queue(access_rel);
        match self.walk(err_path, queue) {
            Ok(WalkOutcome::Found(found)) => {
                let is_dir = found.file_type == FileType::Directory;
                if self.is_hidden(access_rel, is_dir) {
                    return Err(hidden_msg(err_path));
                }
                Ok(Some(found))
            }
            Ok(WalkOutcome::Missing) => {
                if self.is_hidden(access_rel, false) || self.is_hidden(access_rel, true) {
                    return Err(hidden_msg(err_path));
                }
                Ok(None)
            }
            Err(orig_err) => {
                let comps = rel_components(access_rel);
                let n = comps.len();
                let mut prefix = PathBuf::new();
                for (idx, comp) in comps.iter().enumerate() {
                    prefix.push(comp);
                    let is_intermediate = idx + 1 < n;
                    let prefix_queue: VecDeque<OsString> =
                        comps[..=idx].iter().map(|s| s.to_os_string()).collect();
                    match self.walk(err_path, prefix_queue) {
                        Ok(WalkOutcome::Found(found)) => {
                            let is_dir = found.file_type == FileType::Directory;
                            if self.matches_single(&prefix, is_dir) {
                                return Err(hidden_msg(err_path));
                            }
                        }
                        Ok(WalkOutcome::Missing) => {
                            if self.matches_unknown(&prefix, is_intermediate) {
                                return Err(hidden_msg(err_path));
                            }
                        }
                        Err(_) => {
                            if self.matches_unknown(&prefix, false) {
                                return Err(hidden_msg(err_path));
                            }
                            break;
                        }
                    }
                }
                Err(orig_err)
            }
        }
    }

    fn walk(&self, raw_path: &str, mut queue: VecDeque<OsString>) -> Result<WalkOutcome, String> {
        let mut dirs: Vec<OsString> = Vec::new();
        let mut symlinks = 0usize;

        while let Some(comp) = queue.pop_front() {
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
                Err(Errno::NOENT) => {
                    return self.check_enoent_hidden(raw_path, &dirs, comp, queue);
                }
                Err(Errno::NOTDIR) => {
                    return Err(format!("路径 `{raw_path}` 的上级不是目录"));
                }
                Err(Errno::LOOP | Errno::XDEV) => {
                    return Err(self.out_of_bounds_msg(raw_path));
                }
                Err(err) => return Err(format!("访问路径 `{raw_path}` 失败：{err}")),
            };

            let st = fstat(&fd).map_err(|err| format!("读取路径 `{raw_path}` 属性失败：{err}"))?;
            let ft = FileType::from_raw_mode(st.st_mode);
            // 符号链接按文件判定，与 Git 规则一致。
            let is_dir = ft == FileType::Directory;
            if self.matches_single(&loc, is_dir) {
                return Err(hidden_msg(raw_path));
            }

            if ft == FileType::Symlink {
                symlinks += 1;
                if symlinks > MAX_SYMLINK_DEPTH {
                    return Err(format!("解析路径 `{raw_path}` 时符号链接层数过多"));
                }
                let target_cstr = readlinkat(&fd, c"", Vec::new())
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
                for part in target_parts.into_iter().rev() {
                    queue.push_front(part);
                }
            } else if is_dir && !queue.is_empty() {
                dirs.push(comp);
            } else if !queue.is_empty() {
                return Err(format!("路径 `{raw_path}` 的上级不是目录"));
            } else {
                return Ok(WalkOutcome::Found(FoundTarget {
                    fd: Some(fd),
                    real_rel: loc,
                    file_type: ft,
                }));
            }
        }

        let real_rel = parts_to_path(&dirs);
        if real_rel.as_os_str().is_empty() {
            return Ok(WalkOutcome::Found(FoundTarget {
                fd: None,
                real_rel,
                file_type: FileType::Directory,
            }));
        }

        let fd = match self.open_rel_component(&real_rel, OFlags::DIRECTORY) {
            Ok(fd) => fd,
            Err(Errno::NOENT) => return Ok(WalkOutcome::Missing),
            Err(Errno::NOTDIR) => return Err(format!("路径 `{raw_path}` 不是目录")),
            Err(Errno::LOOP | Errno::XDEV) => return Err(self.out_of_bounds_msg(raw_path)),
            Err(err) => return Err(format!("访问路径 `{raw_path}` 失败：{err}")),
        };
        let st = fstat(&fd).map_err(|err| format!("读取路径 `{raw_path}` 属性失败：{err}"))?;
        let ft = FileType::from_raw_mode(st.st_mode);
        Ok(WalkOutcome::Found(FoundTarget {
            fd: Some(fd),
            real_rel,
            file_type: ft,
        }))
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

    fn check_enoent_hidden(
        &self,
        raw_path: &str,
        dirs: &[OsString],
        first_comp: OsString,
        mut remaining: VecDeque<OsString>,
    ) -> Result<WalkOutcome, String> {
        let mut parts: Vec<OsString> = dirs.to_vec();
        remaining.push_front(first_comp);
        let total = remaining.len();
        for (idx, item) in remaining.into_iter().enumerate() {
            if item.as_os_str() == OsStr::new("..") {
                if parts.pop().is_none() {
                    return Err(self.out_of_bounds_msg(raw_path));
                }
                continue;
            }
            parts.push(item);
            let cur = parts_to_path(&parts);
            if self.matches_unknown(&cur, idx + 1 < total) {
                return Err(hidden_msg(raw_path));
            }
        }
        Ok(WalkOutcome::Missing)
    }

    fn lexical_rel(&self, path: &str) -> Result<PathBuf, String> {
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
        Ok(rel)
    }

    fn matches_single(&self, rel: &Path, is_dir: bool) -> bool {
        !self.inner.hidden.is_empty()
            && !rel.as_os_str().is_empty()
            && self.inner.hidden.matched(rel, is_dir).is_ignore()
    }

    fn matches_unknown(&self, rel: &Path, is_intermediate: bool) -> bool {
        if is_intermediate {
            self.matches_single(rel, true)
        } else {
            self.matches_single(rel, false) || self.matches_single(rel, true)
        }
    }

    fn is_hidden(&self, rel: &Path, is_dir: bool) -> bool {
        if self.inner.hidden.is_empty() || rel.as_os_str().is_empty() {
            return false;
        }
        let comps = rel_components(rel);
        let mut cur = PathBuf::new();
        for (idx, comp) in comps.iter().enumerate() {
            cur.push(comp);
            let comp_is_dir = if idx + 1 == comps.len() { is_dir } else { true };
            if self.matches_single(&cur, comp_is_dir) {
                return true;
            }
        }
        false
    }

    fn out_of_bounds_msg(&self, path: &str) -> String {
        format!("路径 `{path}` 越出目录 `{}` 范围", self.inner.name)
    }
}

fn hidden_msg(path: &str) -> String {
    format!("路径 `{path}` 命中 hidden 规则，禁止访问")
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

fn rel_to_queue(rel: &Path) -> VecDeque<OsString> {
    rel_components(rel)
        .into_iter()
        .map(OsStr::to_os_string)
        .collect()
}
