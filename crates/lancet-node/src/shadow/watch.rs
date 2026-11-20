// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 基于 `rustix::fs::inotify` 的非阻塞目录树监听，供宿主跟踪与影子跟踪共用。

use std::collections::{BTreeSet, HashMap, HashSet};
use std::ffi::{OsStr, OsString};
use std::mem::MaybeUninit;
use std::os::fd::{AsFd, OwnedFd};
use std::os::unix::ffi::{OsStrExt as _, OsStringExt as _};
use std::path::{Path, PathBuf};

use anyhow::Context as _;
use rustix::fs::inotify::{self, CreateFlags, ReadFlags, WatchFlags};
use rustix::fs::{Dir, FileType};
use rustix::io::Errno;

use crate::read_dir::PathRules;
use crate::shadow::model::{EntryKind, inspect_lstat_at, open_dir_nofollow, open_root_fd};

fn watch_mask() -> WatchFlags {
    WatchFlags::MODIFY
        | WatchFlags::CLOSE_WRITE
        | WatchFlags::ATTRIB
        | WatchFlags::CREATE
        | WatchFlags::DELETE
        | WatchFlags::MOVED_FROM
        | WatchFlags::MOVED_TO
        | WatchFlags::MOVE_SELF
        | WatchFlags::DELETE_SELF
        | WatchFlags::ONLYDIR
        | WatchFlags::DONT_FOLLOW
}

/// 一次非阻塞排空产生的候选路径与降级信号。
#[derive(Debug, Default)]
pub(crate) struct DrainResult {
    pub paths: BTreeSet<PathBuf>,
    pub recursive: BTreeSet<PathBuf>,
    /// 收到 `MODIFY`/`CLOSE_WRITE`/`ATTRIB` 的非目录路径（供影子侧识别短命硬链接写入）。
    pub modified_files: BTreeSet<PathBuf>,
    /// 本批事件中被新建或移入（`CREATE`/`MOVED_TO`）的非目录路径（用于检出同批重建同名硬链接的写入）。
    pub created_or_moved_files: BTreeSet<PathBuf>,
    /// 同一批事件中配对成功的非目录改名 `from -> to`（用于区分正常原子替换与短命硬链接）。
    pub paired_file_moves: HashMap<PathBuf, PathBuf>,
    pub need_full_rescan: bool,
    pub overflowed: bool,
}

/// 递归监听单个目录树（宿主根或影子 `tree/` 根）的 inotify 封装。
pub(crate) struct DirWatch {
    root: PathBuf,
    root_fd: OwnedFd,
    rules: PathRules,
    /// `true` 表示宿主侧监听（跳过 `hidden` 目录）；`false` 表示影子侧监听（保留 `hidden` 项用于 `discard()`）。
    skip_hidden: bool,
    fd: OwnedFd,
    wd_to_path: HashMap<i32, PathBuf>,
    /// 已预期会收到 `IN_IGNORED` 的 wd（自身删除或主动摘除）；收到不在集合中的 `IN_IGNORED` 时重建监听。
    expected_ignored: HashSet<i32>,
    degraded: bool,
}

impl DirWatch {
    /// 初始化 inotify 并递归监听目录根下的全部目标目录；监视数用尽时报错并写明 `fs.inotify.max_user_watches`。
    pub fn new(root: PathBuf, rules: PathRules, skip_hidden: bool) -> anyhow::Result<Self> {
        let root_fd = open_root_fd(&root)
            .with_context(|| format!("打开监听根目录 {} 失败", root.display()))?;
        let fd = inotify::init(CreateFlags::NONBLOCK | CreateFlags::CLOEXEC)
            .with_context(|| format!("为目录 {} 初始化 inotify 失败", root.display()))?;
        let mut watch = Self {
            root,
            root_fd,
            rules,
            skip_hidden,
            fd,
            wd_to_path: HashMap::new(),
            expected_ignored: HashSet::new(),
            degraded: false,
        };
        watch.add_recursive_startup(Path::new(""))?;
        Ok(watch)
    }

    /// 排空已到达内核队列的所有事件，更新目录映射并返回候选路径与降级标志。
    pub fn drain(&mut self) -> DrainResult {
        let mut res = DrainResult::default();
        if self.degraded {
            res.need_full_rescan = true;
        }

        let mut raw_events = Vec::new();
        let mut buf = [MaybeUninit::<u8>::uninit(); 65_536];
        {
            let mut reader = inotify::Reader::new(&self.fd, &mut buf);
            loop {
                match reader.next() {
                    Ok(ev) => {
                        let name = ev
                            .file_name()
                            .map(|c| OsString::from_vec(c.to_bytes().to_vec()));
                        raw_events.push((ev.wd(), ev.events(), ev.cookie(), name));
                    }
                    Err(Errno::AGAIN) => break,
                    Err(Errno::INTR) => continue,
                    Err(_) => {
                        res.overflowed = true;
                        res.need_full_rescan = true;
                        break;
                    }
                }
            }
        }

        // 记录本批开始前已有的 wd，防止移出目录清理时误删本批新建的同路径 wd。
        let pre_existing_wds: HashSet<i32> = self.wd_to_path.keys().copied().collect();
        let mut moved_from_dirs: HashMap<u32, PathBuf> = HashMap::new();
        let mut moved_from_dir_paths: HashSet<PathBuf> = HashSet::new();
        let mut created_or_moved_to_dirs: HashSet<PathBuf> = HashSet::new();
        let mut move_self_wds: Vec<(i32, PathBuf)> = Vec::new();
        let mut file_moves: HashMap<u32, PathBuf> = HashMap::new();

        for (wd, flags, cookie, name_opt) in raw_events {
            if flags.contains(ReadFlags::QUEUE_OVERFLOW) {
                res.overflowed = true;
                res.need_full_rescan = true;
                continue;
            }
            if flags.contains(ReadFlags::IGNORED) {
                self.wd_to_path.remove(&wd);
                if !self.expected_ignored.remove(&wd) {
                    res.need_full_rescan = true;
                }
                continue;
            }

            let Some(base) = self.wd_to_path.get(&wd).cloned() else {
                res.need_full_rescan = true;
                continue;
            };

            if flags.contains(ReadFlags::DELETE_SELF) {
                self.expected_ignored.insert(wd);
            }
            if flags.contains(ReadFlags::MOVE_SELF) {
                move_self_wds.push((wd, base.clone()));
            }

            let rel = match name_opt {
                Some(ref name) if !name.is_empty() => join_rel(&base, name),
                _ => base,
            };

            let is_dir_ev = flags.contains(ReadFlags::ISDIR)
                || flags.intersects(ReadFlags::DELETE_SELF | ReadFlags::MOVE_SELF);
            if !rel.as_os_str().is_empty() && self.should_skip_event(&rel, is_dir_ev) {
                continue;
            }

            if !rel.as_os_str().is_empty() {
                res.paths.insert(rel.clone());
            }

            if is_dir_ev {
                if !rel.as_os_str().is_empty() {
                    res.recursive.insert(rel.clone());
                }

                if flags.contains(ReadFlags::MOVED_FROM) {
                    moved_from_dir_paths.insert(rel.clone());
                    if cookie != 0 {
                        moved_from_dirs.insert(cookie, rel.clone());
                    }
                }

                if flags.contains(ReadFlags::MOVED_TO) {
                    created_or_moved_to_dirs.insert(rel.clone());
                    if let Some(old_path) = moved_from_dirs.remove(&cookie) {
                        // 目标路径上若已有另一已监听目录（如 RENAME_EXCHANGE 或覆盖），无法安全消歧，直接全量重建。
                        let target_occupied = self.wd_to_path.values().any(|p| {
                            *p != old_path
                                && p.strip_prefix(&old_path).is_err()
                                && (*p == rel || p.strip_prefix(&rel).is_ok())
                        });
                        if target_occupied {
                            res.need_full_rescan = true;
                        } else {
                            for watched in self.wd_to_path.values_mut() {
                                if *watched == old_path {
                                    *watched = rel.clone();
                                } else if let Ok(suffix) = watched.strip_prefix(&old_path) {
                                    *watched = rel.join(suffix);
                                }
                            }
                        }
                    }
                    // 移入目录补装监听并经 `res.recursive` 核实子树；仅补装失败时降级为全量核对。
                    if self.add_recursive_runtime(&rel).is_err() {
                        self.degraded = true;
                        res.need_full_rescan = true;
                    }
                }

                if flags.contains(ReadFlags::CREATE) {
                    created_or_moved_to_dirs.insert(rel.clone());
                    // 新建目录补装监听并经 `res.recursive` 核实子树；仅补装失败时降级为全量核对。
                    if self.add_recursive_runtime(&rel).is_err() {
                        self.degraded = true;
                        res.need_full_rescan = true;
                    }
                }
            } else {
                if !rel.as_os_str().is_empty()
                    && flags.intersects(ReadFlags::CREATE | ReadFlags::MOVED_TO)
                {
                    res.created_or_moved_files.insert(rel.clone());
                }
                if !rel.as_os_str().is_empty()
                    && flags
                        .intersects(ReadFlags::MODIFY | ReadFlags::CLOSE_WRITE | ReadFlags::ATTRIB)
                {
                    res.modified_files.insert(rel.clone());
                }
                if flags.contains(ReadFlags::MOVED_FROM) && cookie != 0 {
                    file_moves.insert(cookie, rel.clone());
                }
                if flags.contains(ReadFlags::MOVED_TO)
                    && cookie != 0
                    && let Some(old_path) = file_moves.remove(&cookie)
                {
                    res.paired_file_moves.insert(old_path, rel.clone());
                }
            }
        }

        // 同批内目录移出后原名复用，或两目录交换导致路径复用，均重建监听并全量核对。
        let has_name_reuse = moved_from_dir_paths
            .iter()
            .any(|p| created_or_moved_to_dirs.contains(p));
        if has_name_reuse {
            res.need_full_rescan = true;
        }

        if !res.need_full_rescan && !moved_from_dirs.is_empty() {
            // 未配对的 MOVED_FROM 目录已被移出监听范围：优先按 `IN_MOVE_SELF` 的 wd 摘除，
            // 并只摘除本批前已存在的旧后代 wd，绝不误删新目录的监听。
            let mut stale_wds: HashSet<i32> = HashSet::new();
            for moved_out in moved_from_dirs.values() {
                for (wd, self_path) in &move_self_wds {
                    if self_path == moved_out && pre_existing_wds.contains(wd) {
                        stale_wds.insert(*wd);
                    }
                }
                for (&wd, p) in &self.wd_to_path {
                    if pre_existing_wds.contains(&wd)
                        && (*p == *moved_out || p.strip_prefix(moved_out).is_ok())
                    {
                        stale_wds.insert(wd);
                    }
                }
            }
            for wd in stale_wds {
                self.wd_to_path.remove(&wd);
                self.expected_ignored.insert(wd);
                let _ = inotify::remove_watch(&self.fd, wd);
            }
        }

        res
    }

    /// 溢出、监听丢失、目录原名复用或补装失败后重建全新的 inotify 实例并重新递归安装监听。
    pub fn rebuild(&mut self) {
        let Ok(new_fd) = inotify::init(CreateFlags::NONBLOCK | CreateFlags::CLOEXEC) else {
            self.degraded = true;
            return;
        };
        self.fd = new_fd;
        self.wd_to_path.clear();
        self.expected_ignored.clear();
        self.degraded = false;
        if self.add_recursive_runtime(Path::new("")).is_err() {
            self.degraded = true;
        }
    }

    fn should_skip_dir(&self, rel: &Path) -> bool {
        if rel.as_os_str().is_empty() {
            return false;
        }
        if self.rules.is_reserved_git(rel) {
            return true;
        }
        if self.skip_hidden && self.rules.is_hidden(rel, true) {
            return true;
        }
        self.rules.is_regenerable(rel, true)
    }

    fn should_skip_event(&self, rel: &Path, is_dir: bool) -> bool {
        if self.rules.is_reserved_git(rel) {
            return true;
        }
        if self.skip_hidden && self.rules.is_hidden(rel, is_dir) {
            return true;
        }
        self.rules.is_regenerable(rel, is_dir)
    }

    fn add_recursive_startup(&mut self, rel: &Path) -> anyhow::Result<()> {
        if self.should_skip_dir(rel) {
            return Ok(());
        }
        let Some(dir_fd) = open_dir_nofollow(self.root_fd.as_fd(), rel).with_context(|| {
            format!(
                "打开目录 {} 以安装 inotify 监听失败",
                join_abs(&self.root, rel).display()
            )
        })?
        else {
            return Ok(());
        };
        let abs = join_abs(&self.root, rel);
        let wd = match inotify::add_watch(&self.fd, &abs, watch_mask()) {
            Ok(wd) => wd,
            Err(Errno::NOSPC) => {
                anyhow::bail!(
                    "启动时为目录 {} 安装 inotify 监听失败：监视数已耗尽，请调大 sysctl `fs.inotify.max_user_watches`",
                    abs.display()
                );
            }
            Err(Errno::NOENT | Errno::NOTDIR | Errno::LOOP) => return Ok(()),
            Err(err) => {
                return Err(std::io::Error::from(err))
                    .with_context(|| format!("为目录 {} 安装 inotify 监听失败", abs.display()));
            }
        };
        self.expected_ignored.remove(&wd);
        self.wd_to_path.insert(wd, rel.to_path_buf());

        let mut dir = Dir::new(dir_fd)
            .map_err(std::io::Error::from)
            .with_context(|| format!("遍历目录 {} 以安装 inotify 监听失败", abs.display()))?;
        let mut subdirs = Vec::new();
        while let Some(next) = dir.read() {
            let entry = next
                .map_err(std::io::Error::from)
                .with_context(|| format!("读取目录 {} 子项失败", abs.display()))?;
            let bytes = entry.file_name().to_bytes();
            if bytes == b"." || bytes == b".." {
                continue;
            }
            let child_rel = join_rel(rel, OsStr::from_bytes(bytes));
            let is_dir = match entry.file_type() {
                FileType::Directory => true,
                FileType::Unknown => inspect_lstat_at(self.root_fd.as_fd(), &child_rel)
                    .ok()
                    .flatten()
                    .is_some_and(|s| s.kind == EntryKind::Dir),
                _ => false,
            };
            if is_dir {
                subdirs.push(child_rel);
            }
        }
        drop(dir);

        for child_rel in subdirs {
            self.add_recursive_startup(&child_rel)?;
        }
        Ok(())
    }

    fn add_single_runtime(&mut self, rel: &Path) -> Result<(), ()> {
        let abs = join_abs(&self.root, rel);
        match inotify::add_watch(&self.fd, &abs, watch_mask()) {
            Ok(wd) => {
                self.expected_ignored.remove(&wd);
                self.wd_to_path.insert(wd, rel.to_path_buf());
                Ok(())
            }
            Err(Errno::NOENT | Errno::NOTDIR | Errno::ACCESS | Errno::LOOP) => Ok(()),
            Err(_) => Err(()),
        }
    }

    fn add_recursive_runtime(&mut self, rel: &Path) -> Result<(), ()> {
        if self.should_skip_dir(rel) {
            return Ok(());
        }
        let Ok(Some(dir_fd)) = open_dir_nofollow(self.root_fd.as_fd(), rel) else {
            return Ok(());
        };
        self.add_single_runtime(rel)?;

        let Ok(mut dir) = Dir::new(dir_fd) else {
            return Ok(());
        };
        let mut subdirs = Vec::new();
        while let Some(next) = dir.read() {
            let Ok(entry) = next else {
                continue;
            };
            let bytes = entry.file_name().to_bytes();
            if bytes == b"." || bytes == b".." {
                continue;
            }
            let child_rel = join_rel(rel, OsStr::from_bytes(bytes));
            let is_dir = match entry.file_type() {
                FileType::Directory => true,
                FileType::Unknown => inspect_lstat_at(self.root_fd.as_fd(), &child_rel)
                    .ok()
                    .flatten()
                    .is_some_and(|s| s.kind == EntryKind::Dir),
                _ => false,
            };
            if is_dir {
                subdirs.push(child_rel);
            }
        }
        drop(dir);

        for child_rel in subdirs {
            self.add_recursive_runtime(&child_rel)?;
        }
        Ok(())
    }
}

fn join_rel(base: &Path, name: &OsStr) -> PathBuf {
    if base.as_os_str().is_empty() {
        PathBuf::from(name)
    } else {
        base.join(name)
    }
}

fn join_abs(root: &Path, rel: &Path) -> PathBuf {
    if rel.as_os_str().is_empty() {
        root.to_path_buf()
    } else {
        root.join(rel)
    }
}
