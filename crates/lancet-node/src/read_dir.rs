// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 目录宿主资源与共用路径解析：基于 `openat2(RESOLVE_BENEATH | RESOLVE_NO_SYMLINKS)` 的逐分量安全解析、
//! `hidden`/`regenerable` 路径分类与保留 `.git` 拦截。

use std::collections::VecDeque;
use std::ffi::{OsStr, OsString};
use std::io::{Read as _, Write as _};
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::Context as _;
use ignore::Match;
use ignore::gitignore::{Gitignore, GitignoreBuilder};
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

/// 工作目录路径分类规则：`hidden`、`regenerable` 与可写目录保留的根 `.git`。
///
/// 同一路径同时匹配 `hidden` 与 `regenerable` 时，按 `hidden` 处理。
#[derive(Clone)]
pub struct PathRules {
    hidden: Gitignore,
    regenerable: Gitignore,
    reserve_root_git: bool,
}

impl PathRules {
    /// 编译配置中的 `hidden` 与 `regenerable` 路径规则。
    pub fn build(
        name: &str,
        hidden_patterns: &[String],
        regenerable_patterns: &[String],
        reserve_root_git: bool,
    ) -> anyhow::Result<Self> {
        let hidden = build_gitignore(name, "hidden", hidden_patterns)?;
        let regenerable = build_gitignore(name, "regenerable", regenerable_patterns)?;
        Ok(Self {
            hidden,
            regenerable,
            reserve_root_git,
        })
    }

    /// 返回该规则集是否保留目录根下的 `.git`。
    pub fn reserves_root_git(&self) -> bool {
        self.reserve_root_git
    }

    /// 判断相对路径是否落在可写目录根下保留的 `.git` 入口内。
    pub fn is_reserved_git(&self, rel: &Path) -> bool {
        self.reserve_root_git
            && matches!(rel.components().next(), Some(Component::Normal(c)) if c == OsStr::new(".git"))
    }

    /// 匹配单个路径是否直接命中 `hidden`，返回命中的规则原文。
    pub fn matches_hidden_single(&self, rel: &Path, is_dir: bool) -> Option<&str> {
        if self.hidden.is_empty() || rel.as_os_str().is_empty() {
            return None;
        }
        match self.hidden.matched(rel, is_dir) {
            Match::Ignore(glob) => Some(glob.original()),
            Match::None | Match::Whitelist(_) => None,
        }
    }

    /// 自根向下逐前缀检查路径是否命中 `hidden`，返回首个命中的规则原文。
    pub fn hidden_rule(&self, rel: &Path, is_dir: bool) -> Option<&str> {
        if self.hidden.is_empty() || rel.as_os_str().is_empty() {
            return None;
        }
        let comps = rel_components(rel);
        let mut cur = PathBuf::new();
        for (idx, comp) in comps.iter().enumerate() {
            cur.push(comp);
            let comp_is_dir = if idx + 1 == comps.len() { is_dir } else { true };
            if let Some(rule) = self.matches_hidden_single(&cur, comp_is_dir) {
                return Some(rule);
            }
        }
        None
    }

    /// 判断路径是否命中 `hidden`。
    pub fn is_hidden(&self, rel: &Path, is_dir: bool) -> bool {
        self.hidden_rule(rel, is_dir).is_some()
    }

    /// 路径类型未知时（如目标不存在）的 `hidden` 判定。
    pub fn matches_hidden_unknown(&self, rel: &Path, is_intermediate: bool) -> bool {
        if is_intermediate {
            self.matches_hidden_single(rel, true).is_some()
        } else {
            self.matches_hidden_single(rel, false).is_some()
                || self.matches_hidden_single(rel, true).is_some()
        }
    }

    /// 自根向下逐前缀检查路径是否命中 `regenerable`；同时命中 `hidden` 时按 `hidden` 处理（返回 `None`）。
    pub fn regenerable_rule(&self, rel: &Path, is_dir: bool) -> Option<&str> {
        if self.regenerable.is_empty() || rel.as_os_str().is_empty() {
            return None;
        }
        if self.is_hidden(rel, is_dir) {
            return None;
        }
        let comps = rel_components(rel);
        let mut cur = PathBuf::new();
        for (idx, comp) in comps.iter().enumerate() {
            cur.push(comp);
            let comp_is_dir = if idx + 1 == comps.len() { is_dir } else { true };
            if let Match::Ignore(glob) = self.regenerable.matched(&cur, comp_is_dir) {
                return Some(glob.original());
            }
        }
        None
    }

    /// 判断路径是否命中 `regenerable`（且未命中 `hidden`）。
    pub fn is_regenerable(&self, rel: &Path, is_dir: bool) -> bool {
        self.regenerable_rule(rel, is_dir).is_some()
    }
}

fn build_gitignore(name: &str, field: &str, patterns: &[String]) -> anyhow::Result<Gitignore> {
    let mut builder = GitignoreBuilder::new(".");
    for pattern in patterns {
        builder
            .add_line(None, pattern)
            .with_context(|| format!("目录 `{name}` 的 {field} 规则 `{pattern}` 无效"))?;
    }
    builder
        .build()
        .with_context(|| format!("构建目录 `{name}` 的 {field} 匹配器失败"))
}

/// 绑定到单个只读工作目录的宿主资源。
#[derive(Clone)]
pub struct ReadDir {
    root: DirRoot,
}

impl ReadDir {
    /// 打开只读目录句柄并构建配置中的 `hidden` 规则匹配器。
    pub fn open(
        name: &str,
        canonical_root: &Path,
        hidden_patterns: &[String],
    ) -> anyhow::Result<Self> {
        let rules = PathRules::build(name, hidden_patterns, &[], false)?;
        let root = DirRoot::open(name, canonical_root, rules)?;
        Ok(Self { root })
    }

    /// 返回该只读目录在配置中的句柄名。
    pub fn name(&self) -> &str {
        self.root.name()
    }

    /// 读取目录内 `path` 处的文本文件；目标不存在且未命中 `hidden` 时返回 `Ok(None)`。
    pub fn read(&self, path: &str) -> Result<Option<String>, String> {
        self.root.read(path)
    }

    /// 列出目录内 `path` 处的直接子项；目标不存在且未命中 `hidden` 时返回 `Ok(None)`。
    pub fn entries(&self, path: &str) -> Result<Option<Vec<DirEntry>>, String> {
        self.root.entries(path)
    }
}

/// `DirRoot::write_file` 的写入结果。
pub(crate) struct WriteFileResult {
    /// 实际写入的根相对路径（原始字节路径）。
    pub real_rel: PathBuf,
    /// 写入过程中新建的上级目录（由浅到深）。
    pub created_dirs: Vec<PathBuf>,
    /// 文件是否实际发生变化（内容相同时为 `false`，未落盘）。
    pub changed: bool,
    /// 若写入落在 `regenerable` 路径，为命中的规则原文。
    pub regenerable_rule: Option<String>,
}

/// 任意目录根（只读宿主根或可写影子 `tree/` 根）共用的安全路径解析与读写视图。
#[derive(Clone)]
pub(crate) struct DirRoot {
    inner: Arc<DirRootInner>,
}

struct DirRootInner {
    name: String,
    root_path: PathBuf,
    root_fd: OwnedFd,
    tmp_root_fd: Option<OwnedFd>,
    rules: PathRules,
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

enum WalkErr {
    ReservedGit(String),
    Other(String),
}

impl WalkErr {
    fn into_string(self) -> String {
        match self {
            Self::ReservedGit(msg) | Self::Other(msg) => msg,
        }
    }
}

impl DirRoot {
    /// 打开给定目录根的 `O_PATH` fd 并绑定路径规则。
    pub fn open(name: &str, canonical_root: &Path, rules: PathRules) -> anyhow::Result<Self> {
        Self::open_internal(name, canonical_root, None, rules)
    }

    /// 打开影子目录根并绑定同文件系统的 `shadows/<key>/tmp/`，避免在 `tree/` 内创建临时文件。
    pub fn open_with_tmp(
        name: &str,
        canonical_root: &Path,
        tmp_root: &Path,
        rules: PathRules,
    ) -> anyhow::Result<Self> {
        let tmp_fd = openat2(
            CWD,
            tmp_root,
            OFlags::PATH | OFlags::DIRECTORY | OFlags::CLOEXEC,
            Mode::empty(),
            ResolveFlags::NO_SYMLINKS,
        )
        .with_context(|| format!("打开目录 `{name}` 临时目录失败"))?;
        Self::open_internal(name, canonical_root, Some(tmp_fd), rules)
    }

    fn open_internal(
        name: &str,
        canonical_root: &Path,
        tmp_root_fd: Option<OwnedFd>,
        rules: PathRules,
    ) -> anyhow::Result<Self> {
        let root_fd = openat2(
            CWD,
            canonical_root,
            OFlags::PATH | OFlags::DIRECTORY | OFlags::CLOEXEC,
            Mode::empty(),
            ResolveFlags::NO_SYMLINKS,
        )
        .with_context(|| format!("打开目录 `{name}` 失败"))?;
        Ok(Self {
            inner: Arc::new(DirRootInner {
                name: name.to_owned(),
                root_path: canonical_root.to_path_buf(),
                root_fd,
                tmp_root_fd,
                rules,
            }),
        })
    }

    pub fn name(&self) -> &str {
        &self.inner.name
    }

    /// 读取目录内 `path` 处的 UTF-8 文本文件；目标不存在且未命中 `hidden`/`.git` 时返回 `Ok(None)`。
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

    /// 列出目录内 `path` 处的直接子项；目标不存在且未命中 `hidden`/`.git` 时返回 `Ok(None)`。
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
            if target.real_rel.as_os_str().is_empty()
                && self.inner.rules.reserve_root_git
                && name_bytes == b".git"
            {
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
                    if self.inner.rules.is_hidden(&real_child, is_dir)
                        || self.inner.rules.is_hidden(&access_child, is_dir)
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

            // `hidden` 与 `.git` 检查通过后再做 UTF-8 校验：被隐藏或略去的非 UTF-8 条目保持不可见。
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

    /// 逐分量核验越界、保留 `.git` 与 `hidden` 后，经已核验的上级目录 fd 写入临时文件再改名就位。
    pub fn write_file(&self, path: &str, content: &str) -> Result<WriteFileResult, String> {
        let access_rel = self.lexical_rel(path)?;
        if access_rel.as_os_str().is_empty() {
            return Err(format!("目标 `{path}` 是目录，无法作为文件写入"));
        }
        if self.inner.rules.is_reserved_git(&access_rel) {
            return Err(reserved_git_msg(path));
        }

        let mut queue = rel_to_queue(&access_rel);
        let mut dirs: Vec<OsString> = Vec::new();
        let mut symlinks = 0usize;

        while let Some(comp) = queue.pop_front() {
            if comp.as_os_str() == OsStr::new("..") {
                if dirs.pop().is_none() {
                    return Err(self.out_of_bounds_msg(path));
                }
                continue;
            }
            if comp.as_os_str().as_bytes().contains(&0) {
                return Err(format!("路径 `{path}` 包含非法空字节"));
            }
            if dirs.is_empty()
                && self.inner.rules.reserve_root_git
                && comp.as_os_str() == OsStr::new(".git")
            {
                return Err(reserved_git_msg(path));
            }

            let mut loc = parts_to_path(&dirs);
            loc.push(&comp);

            match self.open_rel_component(&loc, OFlags::empty()) {
                Ok(fd) => {
                    let st =
                        fstat(&fd).map_err(|err| format!("读取路径 `{path}` 属性失败：{err}"))?;
                    let ft = FileType::from_raw_mode(st.st_mode);
                    let is_dir = ft == FileType::Directory;
                    if self
                        .inner
                        .rules
                        .matches_hidden_single(&loc, is_dir)
                        .is_some()
                    {
                        return Err(hidden_msg(path));
                    }

                    if ft == FileType::Symlink {
                        symlinks += 1;
                        if symlinks > MAX_SYMLINK_DEPTH {
                            return Err(format!("解析路径 `{path}` 时符号链接层数过多"));
                        }
                        self.push_symlink_target(&fd, path, &mut queue)
                            .map_err(WalkErr::into_string)?;
                    } else if is_dir && !queue.is_empty() {
                        dirs.push(comp);
                    } else if is_dir {
                        self.check_hidden_by_type(&access_rel, &loc, Some(true), path)?;
                        return Err(format!("目标 `{path}` 是目录，无法作为文件写入"));
                    } else if !queue.is_empty() {
                        return Err(format!("路径 `{path}` 的上级不是目录"));
                    } else {
                        self.check_hidden_by_type(&access_rel, &loc, Some(false), path)?;
                        if ft != FileType::RegularFile {
                            return Err(format!("目标 `{path}` 不是普通文件，无法写入"));
                        }
                        let real_rel = loc.clone();
                        self.check_write_path_limits(&real_rel, path)?;
                        let regenerable_rule = self
                            .inner
                            .rules
                            .regenerable_rule(&loc, false)
                            .map(str::to_owned);
                        // 常规路径变更项需以 UTF-8 路径记入回执，必须在落盘前校验；regenerable 记规则名，无需校验。
                        if regenerable_rule.is_none() && real_rel.to_str().is_none() {
                            return Err(format!("写入目标 `{path}` 解析后的路径不是有效 UTF-8"));
                        }

                        // 内容字节完全一致时视为无操作，不落盘，也不产生 effect。
                        if st.st_size as u64 == content.len() as u64 {
                            let proc_path = format!("/proc/self/fd/{}", fd.as_raw_fd());
                            if let Ok(rd_fd) =
                                open(proc_path, OFlags::RDONLY | OFlags::CLOEXEC, Mode::empty())
                            {
                                let mut existing = Vec::with_capacity(content.len());
                                if std::fs::File::from(rd_fd)
                                    .read_to_end(&mut existing)
                                    .is_ok()
                                    && existing == content.as_bytes()
                                {
                                    return Ok(WriteFileResult {
                                        real_rel,
                                        created_dirs: Vec::new(),
                                        changed: false,
                                        regenerable_rule,
                                    });
                                }
                            }
                        }
                        drop(fd);

                        let existing_mode = Mode::from_raw_mode(st.st_mode & 0o7777);
                        let parent_owned = self.open_verified_parent(&dirs, path)?;
                        let parent_fd = parent_owned
                            .as_ref()
                            .map_or_else(|| self.inner.root_fd.as_fd(), AsFd::as_fd);
                        let tmp_dir_fd = self
                            .inner
                            .tmp_root_fd
                            .as_ref()
                            .map_or(parent_fd, AsFd::as_fd);
                        write_atomic_in_dir(
                            tmp_dir_fd,
                            parent_fd,
                            comp.as_os_str(),
                            content.as_bytes(),
                            Some(existing_mode),
                            path,
                        )?;
                        return Ok(WriteFileResult {
                            real_rel,
                            created_dirs: Vec::new(),
                            changed: true,
                            regenerable_rule,
                        });
                    }
                }
                Err(Errno::NOENT) => {
                    // 先校验所有待创建的缺失段（越界、.git、hidden），再实际创建目录。
                    let mut remaining = Vec::with_capacity(queue.len() + 1);
                    remaining.push(comp);
                    remaining.extend(queue);

                    let mut sim_parts = dirs.clone();
                    let total = remaining.len();
                    let mut has_parent_dir_segment = false;
                    for (idx, item) in remaining.iter().enumerate() {
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
                        if sim_parts.is_empty()
                            && self.inner.rules.reserve_root_git
                            && item.as_os_str() == OsStr::new(".git")
                        {
                            return Err(reserved_git_msg(path));
                        }
                        sim_parts.push(item.clone());
                        let cur = parts_to_path(&sim_parts);
                        let is_intermediate = idx + 1 < total;
                        if is_intermediate {
                            if self.inner.rules.matches_hidden_single(&cur, true).is_some() {
                                return Err(hidden_msg(path));
                            }
                        } else {
                            // 末项缺失：与 reset 共用同一套未知类型判定（文件与目录各判一次）。
                            self.check_hidden_by_type(&access_rel, &cur, None, path)?;
                        }
                    }
                    if has_parent_dir_segment {
                        return Err(format!("路径 `{path}` 在缺失目录后包含 `..`"));
                    }
                    let final_rel_path = parts_to_path(&sim_parts);
                    self.check_write_path_limits(&final_rel_path, path)?;

                    let regenerable_rule = self
                        .inner
                        .rules
                        .regenerable_rule(&final_rel_path, false)
                        .map(str::to_owned);
                    // 常规路径在创建上级目录与落盘前先检查真实路径是否为有效 UTF-8；regenerable 记规则名，无需检查。
                    if regenerable_rule.is_none() && final_rel_path.to_str().is_none() {
                        return Err(format!("写入目标 `{path}` 解析后的路径不是有效 UTF-8"));
                    }

                    let (leaf, missing_parents) = remaining
                        .split_last()
                        .expect("缺失路径至少包含一个叶文件名");

                    // 写入途中新建的每个常规上级目录（非 regenerable）都会进入待定集合，须在首次 mkdirat 前核对 UTF-8。
                    let mut check_dir = parts_to_path(&dirs);
                    for dir_comp in missing_parents {
                        check_dir.push(dir_comp);
                        if !self.inner.rules.is_regenerable(&check_dir, true)
                            && check_dir.to_str().is_none()
                        {
                            return Err(format!(
                                "写入目标 `{path}` 需新建的上级目录解析后的路径不是有效 UTF-8"
                            ));
                        }
                    }

                    let mut created_dirs: Vec<PathBuf> = Vec::new();
                    let mut cur_rel = parts_to_path(&dirs);

                    let write_res = (|| -> Result<(), String> {
                        let mut cur_owned = self.open_verified_parent(&dirs, path)?;
                        for dir_comp in missing_parents {
                            let parent_fd = cur_owned
                                .as_ref()
                                .map_or_else(|| self.inner.root_fd.as_fd(), AsFd::as_fd);
                            let actually_created = match mkdirat(
                                parent_fd,
                                dir_comp.as_os_str(),
                                Mode::from_raw_mode(0o755),
                            ) {
                                Ok(()) => true,
                                Err(Errno::EXIST) => false,
                                Err(err) => {
                                    return Err(format!("为路径 `{path}` 创建上级目录失败：{err}"));
                                }
                            };
                            // mkdirat 一成功立即登记，确保随后 openat2 失败（如 EMFILE）时也能回滚该目录。
                            cur_rel.push(dir_comp);
                            if actually_created {
                                created_dirs.push(cur_rel.clone());
                            }
                            let next_fd = openat2(
                                parent_fd,
                                dir_comp.as_os_str(),
                                OFlags::PATH
                                    | OFlags::DIRECTORY
                                    | OFlags::NOFOLLOW
                                    | OFlags::CLOEXEC,
                                Mode::empty(),
                                ResolveFlags::BENEATH | ResolveFlags::NO_SYMLINKS,
                            )
                            .map_err(|err| format!("打开新建上级目录 `{path}` 失败：{err}"))?;
                            cur_owned = Some(next_fd);
                        }

                        let parent_fd = cur_owned
                            .as_ref()
                            .map_or_else(|| self.inner.root_fd.as_fd(), AsFd::as_fd);
                        let tmp_dir_fd = self
                            .inner
                            .tmp_root_fd
                            .as_ref()
                            .map_or(parent_fd, AsFd::as_fd);
                        write_atomic_in_dir(
                            tmp_dir_fd,
                            parent_fd,
                            leaf.as_os_str(),
                            content.as_bytes(),
                            None,
                            path,
                        )
                    })();

                    if let Err(err) = write_res {
                        // 闭包退出已释放持有的目录 fd，回滚本次新建的空目录使失败不留痕迹。
                        self.rollback_created_dirs(&created_dirs);
                        return Err(err);
                    }

                    return Ok(WriteFileResult {
                        real_rel: final_rel_path,
                        created_dirs,
                        changed: true,
                        regenerable_rule,
                    });
                }
                Err(Errno::NOTDIR) => return Err(format!("路径 `{path}` 的上级不是目录")),
                Err(Errno::NAMETOOLONG) => {
                    self.check_hidden_by_type(&access_rel, &loc, None, path)?;
                    return Err(format!("写入路径 `{path}` 超出系统路径长度上限"));
                }
                Err(Errno::LOOP | Errno::XDEV) => return Err(self.out_of_bounds_msg(path)),
                Err(err) => return Err(format!("访问路径 `{path}` 失败：{err}")),
            }
        }

        Err(format!("目标 `{path}` 是目录，无法作为文件写入"))
    }

    /// 核对写入目标各分量不超过 `NAME_MAX`（255 字节），且影子中的绝对路径不超过 `PATH_MAX - 1`（4095 字节）。
    ///
    /// 报错只写出调用者传入的 `raw_path`，不泄露影子目录的绝对路径。
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

    fn rollback_created_dirs(&self, created_dirs: &[PathBuf]) {
        for rel in created_dirs.iter().rev() {
            let Some(leaf) = rel.file_name() else {
                continue;
            };
            let parent = rel.parent().unwrap_or_else(|| Path::new(""));
            let parent_owned = if parent.as_os_str().is_empty() {
                None
            } else {
                match openat2(
                    self.inner.root_fd.as_fd(),
                    parent,
                    OFlags::PATH | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                    Mode::empty(),
                    ResolveFlags::BENEATH | ResolveFlags::NO_SYMLINKS,
                ) {
                    Ok(fd) => Some(fd),
                    Err(_) => continue,
                }
            };
            let parent_fd = parent_owned
                .as_ref()
                .map_or_else(|| self.inner.root_fd.as_fd(), AsFd::as_fd);
            let _ = unlinkat(parent_fd, leaf, AtFlags::REMOVEDIR);
        }
    }

    /// 核验 `reset(path)` 的目标路径：拒绝越界、根目录、`hidden` 与保留的 `.git`（含经符号链接）。
    ///
    /// 与 `write_file` 共用同一套 `hidden` 类型判定：类型已知（取影子实际类型，否则取基准类型）时按实际类型匹配；
    /// 末项缺失时按文件与目录各判定一次。
    pub fn resolve_for_reset(
        &self,
        path: &str,
        base_is_dir: impl Fn(&Path) -> Option<bool>,
    ) -> Result<PathBuf, String> {
        let access_rel = self.lexical_rel(path)?;
        if access_rel.as_os_str().is_empty() {
            return Err(format!(
                "不能对根目录 `{path}` 执行 reset，请指定具体路径或使用 discard()"
            ));
        }
        if self.inner.rules.is_reserved_git(&access_rel) {
            return Err(reserved_git_msg(path));
        }

        let comps = rel_components(&access_rel);
        let (leaf, parent_comps) = comps.split_last().expect("已检查非空路径");
        let real_rel = if parent_comps.is_empty() {
            PathBuf::from(leaf)
        } else {
            let parent_rel = parent_comps.iter().fold(PathBuf::new(), |mut acc, c| {
                acc.push(c);
                acc
            });
            match self.walk_parent_for_reset(path, &parent_rel, leaf)? {
                ResetParent::Resolved(real_parent) => join_rel(&real_parent, leaf),
                ResetParent::MissingPath(full_rel) => full_rel,
            }
        };

        if self.inner.rules.is_reserved_git(&real_rel) {
            return Err(reserved_git_msg(path));
        }

        // 先按影子中的实际类型判定；影子缺失时回退基准中的实际类型；均缺失时按未知类型判定。
        let tree_ft = match self.open_rel_component(&real_rel, OFlags::empty()) {
            Ok(fd) => Some(
                fstat(&fd)
                    .map_err(|err| format!("读取路径 `{path}` 属性失败：{err}"))
                    .map(|st| FileType::from_raw_mode(st.st_mode))?,
            ),
            Err(Errno::NOENT | Errno::NOTDIR | Errno::LOOP) => None,
            Err(err) => return Err(format!("访问路径 `{path}` 失败：{err}")),
        };

        let known_is_dir = match tree_ft {
            Some(ft) => Some(ft == FileType::Directory),
            None => base_is_dir(&real_rel),
        };
        self.check_hidden_by_type(&access_rel, &real_rel, known_is_dir, path)?;

        // 若目标本身是符号链接，检查其解析是否触及越界、hidden 或保留的 .git。
        if tree_ft == Some(FileType::Symlink) {
            self.resolve_rel(&real_rel, path)?;
        }

        Ok(real_rel)
    }

    /// `write_file` 与 `resolve_for_reset` 共用的 `hidden` 类型判定入口：
    /// 已知类型时按实际类型匹配；末项缺失（`None`）时按文件与目录各判一次。
    fn check_hidden_by_type(
        &self,
        access_rel: &Path,
        real_rel: &Path,
        known_is_dir: Option<bool>,
        raw_path: &str,
    ) -> Result<(), String> {
        let hit = match known_is_dir {
            Some(is_dir) => {
                self.inner.rules.is_hidden(access_rel, is_dir)
                    || self.inner.rules.is_hidden(real_rel, is_dir)
            }
            None => {
                self.inner.rules.is_hidden(access_rel, false)
                    || self.inner.rules.is_hidden(access_rel, true)
                    || self.inner.rules.is_hidden(real_rel, false)
                    || self.inner.rules.is_hidden(real_rel, true)
            }
        };
        if hit {
            Err(hidden_msg(raw_path))
        } else {
            Ok(())
        }
    }

    fn walk_parent_for_reset(
        &self,
        raw_path: &str,
        parent_rel: &Path,
        leaf: &OsStr,
    ) -> Result<ResetParent, String> {
        let mut queue = rel_to_queue(parent_rel);
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
            if dirs.is_empty()
                && self.inner.rules.reserve_root_git
                && comp.as_os_str() == OsStr::new(".git")
            {
                return Err(reserved_git_msg(raw_path));
            }

            let mut loc = parts_to_path(&dirs);
            loc.push(&comp);
            match self.open_rel_component(&loc, OFlags::empty()) {
                Ok(fd) => {
                    let st = fstat(&fd)
                        .map_err(|err| format!("读取路径 `{raw_path}` 属性失败：{err}"))?;
                    let ft = FileType::from_raw_mode(st.st_mode);
                    let is_dir = ft == FileType::Directory;
                    if self
                        .inner
                        .rules
                        .matches_hidden_single(&loc, is_dir)
                        .is_some()
                    {
                        return Err(hidden_msg(raw_path));
                    }
                    if ft == FileType::Symlink {
                        symlinks += 1;
                        if symlinks > MAX_SYMLINK_DEPTH {
                            return Err(format!("解析路径 `{raw_path}` 时符号链接层数过多"));
                        }
                        self.push_symlink_target(&fd, raw_path, &mut queue)
                            .map_err(WalkErr::into_string)?;
                    } else if is_dir {
                        dirs.push(comp);
                    } else {
                        // 上级在影子中已被换型成非目录，仍保留逻辑路径交由 reset 检查连带待定改动。
                        let mut full = parts_to_path(&dirs);
                        full.push(&comp);
                        for rest in queue {
                            if rest.as_os_str() == OsStr::new("..") {
                                return Err(self.out_of_bounds_msg(raw_path));
                            }
                            full.push(rest);
                        }
                        full.push(leaf);
                        return Ok(ResetParent::MissingPath(full));
                    }
                }
                Err(Errno::NOENT) => {
                    let mut remaining = VecDeque::with_capacity(queue.len() + 2);
                    remaining.push_back(comp);
                    remaining.extend(queue);
                    remaining.push_back(leaf.to_os_string());
                    let full = self
                        .check_enoent_for_reset(raw_path, &dirs, remaining)
                        .map_err(WalkErr::into_string)?;
                    return Ok(ResetParent::MissingPath(full));
                }
                Err(Errno::NOTDIR) => {
                    let mut full = parts_to_path(&dirs);
                    full.push(&comp);
                    for rest in queue {
                        if rest.as_os_str() == OsStr::new("..") {
                            return Err(self.out_of_bounds_msg(raw_path));
                        }
                        full.push(rest);
                    }
                    full.push(leaf);
                    return Ok(ResetParent::MissingPath(full));
                }
                Err(Errno::LOOP | Errno::XDEV) => return Err(self.out_of_bounds_msg(raw_path)),
                Err(err) => return Err(format!("访问路径 `{raw_path}` 失败：{err}")),
            }
        }

        Ok(ResetParent::Resolved(parts_to_path(&dirs)))
    }

    fn check_enoent_for_reset(
        &self,
        raw_path: &str,
        dirs: &[OsString],
        remaining: VecDeque<OsString>,
    ) -> Result<PathBuf, WalkErr> {
        let mut parts: Vec<OsString> = dirs.to_vec();
        let total = remaining.len();
        for (idx, item) in remaining.into_iter().enumerate() {
            if item.as_os_str() == OsStr::new("..") {
                if parts.pop().is_none() {
                    return Err(WalkErr::Other(self.out_of_bounds_msg(raw_path)));
                }
                continue;
            }
            if parts.is_empty()
                && self.inner.rules.reserve_root_git
                && item.as_os_str() == OsStr::new(".git")
            {
                return Err(WalkErr::ReservedGit(reserved_git_msg(raw_path)));
            }
            parts.push(item);
            let cur = parts_to_path(&parts);
            // 中间段必为目录，末项的 hidden 判定交给 `resolve_for_reset` 按实际/基准类型统一处理。
            if idx + 1 < total && self.inner.rules.matches_hidden_single(&cur, true).is_some() {
                return Err(WalkErr::Other(hidden_msg(raw_path)));
            }
        }
        Ok(parts_to_path(&parts))
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
        let access_rel = self.lexical_rel(path)?;
        self.resolve_rel(&access_rel, path)
    }

    fn resolve_rel(
        &self,
        access_rel: &Path,
        err_path: &str,
    ) -> Result<Option<FoundTarget>, String> {
        if self.inner.rules.is_reserved_git(access_rel) {
            return Err(reserved_git_msg(err_path));
        }

        let queue = rel_to_queue(access_rel);
        match self.walk(err_path, queue) {
            Ok(WalkOutcome::Found(found)) => {
                let is_dir = found.file_type == FileType::Directory;
                if self.inner.rules.is_hidden(access_rel, is_dir) {
                    return Err(hidden_msg(err_path));
                }
                Ok(Some(found))
            }
            Ok(WalkOutcome::Missing) => {
                if self.inner.rules.is_hidden(access_rel, false)
                    || self.inner.rules.is_hidden(access_rel, true)
                {
                    return Err(hidden_msg(err_path));
                }
                Ok(None)
            }
            Err(WalkErr::ReservedGit(msg)) => Err(msg),
            Err(WalkErr::Other(orig_err)) => {
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
                            if self
                                .inner
                                .rules
                                .matches_hidden_single(&prefix, is_dir)
                                .is_some()
                            {
                                return Err(hidden_msg(err_path));
                            }
                        }
                        Ok(WalkOutcome::Missing) => {
                            if self
                                .inner
                                .rules
                                .matches_hidden_unknown(&prefix, is_intermediate)
                            {
                                return Err(hidden_msg(err_path));
                            }
                        }
                        Err(_) => {
                            if self.inner.rules.matches_hidden_unknown(&prefix, false) {
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

    fn walk(&self, raw_path: &str, mut queue: VecDeque<OsString>) -> Result<WalkOutcome, WalkErr> {
        let mut dirs: Vec<OsString> = Vec::new();
        let mut symlinks = 0usize;

        while let Some(comp) = queue.pop_front() {
            if comp.as_os_str() == OsStr::new("..") {
                if dirs.pop().is_none() {
                    return Err(WalkErr::Other(self.out_of_bounds_msg(raw_path)));
                }
                continue;
            }

            if comp.as_os_str().as_bytes().contains(&0) {
                return Err(WalkErr::Other(format!("路径 `{raw_path}` 包含非法空字节")));
            }

            if dirs.is_empty()
                && self.inner.rules.reserve_root_git
                && comp.as_os_str() == OsStr::new(".git")
            {
                return Err(WalkErr::ReservedGit(reserved_git_msg(raw_path)));
            }

            let mut loc = parts_to_path(&dirs);
            loc.push(&comp);

            let fd = match self.open_rel_component(&loc, OFlags::empty()) {
                Ok(fd) => fd,
                Err(Errno::NOENT) => {
                    return self.check_enoent_hidden(raw_path, &dirs, comp, queue);
                }
                Err(Errno::NOTDIR) => {
                    return Err(WalkErr::Other(format!("路径 `{raw_path}` 的上级不是目录")));
                }
                Err(Errno::LOOP | Errno::XDEV) => {
                    return Err(WalkErr::Other(self.out_of_bounds_msg(raw_path)));
                }
                Err(err) => {
                    return Err(WalkErr::Other(format!("访问路径 `{raw_path}` 失败：{err}")));
                }
            };

            let st = fstat(&fd)
                .map_err(|err| WalkErr::Other(format!("读取路径 `{raw_path}` 属性失败：{err}")))?;
            let ft = FileType::from_raw_mode(st.st_mode);
            // 符号链接按文件判定，与 Git 规则一致。
            let is_dir = ft == FileType::Directory;
            if self
                .inner
                .rules
                .matches_hidden_single(&loc, is_dir)
                .is_some()
            {
                return Err(WalkErr::Other(hidden_msg(raw_path)));
            }

            if ft == FileType::Symlink {
                symlinks += 1;
                if symlinks > MAX_SYMLINK_DEPTH {
                    return Err(WalkErr::Other(format!(
                        "解析路径 `{raw_path}` 时符号链接层数过多"
                    )));
                }
                self.push_symlink_target(&fd, raw_path, &mut queue)?;
            } else if is_dir && !queue.is_empty() {
                dirs.push(comp);
            } else if !queue.is_empty() {
                return Err(WalkErr::Other(format!("路径 `{raw_path}` 的上级不是目录")));
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
            Err(Errno::NOTDIR) => {
                return Err(WalkErr::Other(format!("路径 `{raw_path}` 不是目录")));
            }
            Err(Errno::LOOP | Errno::XDEV) => {
                return Err(WalkErr::Other(self.out_of_bounds_msg(raw_path)));
            }
            Err(err) => return Err(WalkErr::Other(format!("访问路径 `{raw_path}` 失败：{err}"))),
        };
        let st = fstat(&fd)
            .map_err(|err| WalkErr::Other(format!("读取路径 `{raw_path}` 属性失败：{err}")))?;
        let ft = FileType::from_raw_mode(st.st_mode);
        Ok(WalkOutcome::Found(FoundTarget {
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
    ) -> Result<(), WalkErr> {
        let target_cstr = readlinkat(fd, c"", Vec::new()).map_err(|err| {
            WalkErr::Other(format!("读取路径 `{raw_path}` 符号链接目标失败：{err}"))
        })?;
        let target_bytes = target_cstr.as_bytes();
        if target_bytes.first() == Some(&b'/') {
            return Err(WalkErr::Other(self.out_of_bounds_msg(raw_path)));
        }
        let target_os = OsStr::from_bytes(target_bytes);
        let mut target_parts = Vec::new();
        for part in Path::new(target_os).components() {
            match part {
                Component::Prefix(_) | Component::RootDir => {
                    return Err(WalkErr::Other(self.out_of_bounds_msg(raw_path)));
                }
                Component::CurDir => {}
                Component::Normal(s) => target_parts.push(s.to_os_string()),
                Component::ParentDir => target_parts.push(OsString::from("..")),
            }
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

    fn check_enoent_hidden(
        &self,
        raw_path: &str,
        dirs: &[OsString],
        first_comp: OsString,
        mut remaining: VecDeque<OsString>,
    ) -> Result<WalkOutcome, WalkErr> {
        let mut parts: Vec<OsString> = dirs.to_vec();
        remaining.push_front(first_comp);
        let total = remaining.len();
        for (idx, item) in remaining.into_iter().enumerate() {
            if item.as_os_str() == OsStr::new("..") {
                if parts.pop().is_none() {
                    return Err(WalkErr::Other(self.out_of_bounds_msg(raw_path)));
                }
                continue;
            }
            if parts.is_empty()
                && self.inner.rules.reserve_root_git
                && item.as_os_str() == OsStr::new(".git")
            {
                return Err(WalkErr::ReservedGit(reserved_git_msg(raw_path)));
            }
            parts.push(item);
            let cur = parts_to_path(&parts);
            if self
                .inner
                .rules
                .matches_hidden_unknown(&cur, idx + 1 < total)
            {
                return Err(WalkErr::Other(hidden_msg(raw_path)));
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

    fn out_of_bounds_msg(&self, path: &str) -> String {
        format!("路径 `{path}` 越出目录 `{}` 范围", self.inner.name)
    }
}

enum ResetParent {
    Resolved(PathBuf),
    MissingPath(PathBuf),
}

fn write_atomic_in_dir(
    tmp_dir_fd: BorrowedFd<'_>,
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
        let mode = preserve_mode.unwrap_or_else(|| Mode::from_raw_mode(0o644));
        match openat(
            tmp_dir_fd,
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
        renameat(tmp_dir_fd, &tmp_name, parent_fd, leaf)
            .map_err(|err| format!("替换文件 `{raw_path}` 失败：{err}"))
    })();

    if write_res.is_err() {
        let _ = unlinkat(tmp_dir_fd, &tmp_name, AtFlags::empty());
    }
    write_res
}

fn hidden_msg(path: &str) -> String {
    format!("路径 `{path}` 命中 hidden 规则，禁止访问")
}

fn reserved_git_msg(path: &str) -> String {
    format!("路径 `{path}` 无法访问：私有 Git 尚不可用")
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
