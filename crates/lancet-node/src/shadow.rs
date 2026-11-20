// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 持久 reflink 影子工作区、宿主同步、影子监听、待定改动与冲突管理。

mod model;
pub mod publish;
mod scanner;
mod watch;

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::ffi::OsStr;
use std::fmt::{self, Write as _};
use std::fs::{self, DirBuilder};
use std::io;
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::DirBuilderExt as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, TryLockError};
use std::thread;
use std::time::Instant;

use anyhow::Context as _;
use lancet_protocol::Residue;
use rustix::fs::{AtFlags, Dir, readlinkat, statat};
use sha2::{Digest as _, Sha256};

use crate::read_dir::{DirEntry, DirRoot, PathRules};

use self::model::{
    EMPTY_SHA256, EntryKind, EntrySnapshot, StatSig, TreeTag, WidenedLog, classify_stat,
    clear_or_create_tmp_dir, clone_init_regular_file_at, compute_unified_diff_at,
    copy_init_symlink, descendants_in_map, entries_semantically_equal_at, has_descendant_in_map,
    hash_host_file_via_ficlone_at, inspect_lstat_at, inspect_with_hash_at, install_dir_writable_at,
    install_file_atomic_at, install_symlink_atomic_at, is_dir_non_empty_at, now_realtime_ns,
    open_dir_nofollow, open_root_fd, path_depth, recover_widened_modes, rel_path_to_utf8,
    remove_single_entry_at, remove_tree_force, remove_tree_nofollow_at, set_dir_mode_nofollow,
    set_init_mode,
};
pub use self::model::{PathDiff, PendingAction, PendingChange};
use self::scanner::{BaselineMeta, HostScanner};
use self::watch::DirWatch;

/// 首次并行克隆常规文件的工作线程数。
const INIT_CLONE_WORKERS: usize = 8;

/// 同步或 `discard()` 在路径之间检查截止时间与取消标志时收到的中断。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShadowInterrupt {
    Deadline,
    Cancelled,
}

/// 宿主同步或影子核对期间可能返回的中断或真实 I/O 错误。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShadowSyncError {
    Interrupted(ShadowInterrupt),
    Failed(String),
}

impl fmt::Display for ShadowSyncError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Interrupted(ShadowInterrupt::Deadline) => write!(f, "操作已超时"),
            Self::Interrupted(ShadowInterrupt::Cancelled) => write!(f, "操作已取消"),
            Self::Failed(msg) => f.write_str(msg),
        }
    }
}

impl std::error::Error for ShadowSyncError {}

/// 宿主写方法（`write`、`reset`、`discard`）报告的单条实际变更。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WriteEffect {
    pub target: String,
    pub action: &'static str,
    pub residue: Residue,
}

/// `discard()` 失败或被中断时的返回错误（始终保留已落地的部分变更供回执记录）。
#[derive(Debug)]
pub enum DiscardError {
    Interrupted {
        partial: Vec<WriteEffect>,
        interrupt: ShadowInterrupt,
    },
    Failed {
        partial: Vec<WriteEffect>,
        message: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PendingEntry {
    action: PendingAction,
    conflict: bool,
    /// `true` 表示对 `status`/`diff`/`visible_pending_count` 隐藏（仅基准无可见项、影子为 hidden 项时为 `true`）。
    hidden: bool,
    /// `true` 表示影子当前位置存在命中 `hidden` 规则的对象（包括用隐藏目录替换基准可见文件的情形）。
    tree_hidden: bool,
}

/// 为待定路径缓存的宿主侧观测，供每次同步快速重算全部待定路径的冲突状态。
#[derive(Debug, Clone)]
struct HostPendingObs {
    kind: EntryKind,
    stat: StatSig,
    hash: Option<[u8; 32]>,
    observed_at_ns: i128,
}

/// 单个可写工作目录对应的影子管理句柄。
#[derive(Clone)]
pub struct ShadowDir {
    inner: Arc<ShadowInner>,
}

/// 供 `borrow<read-dir>` 参数收窄使用的影子只读视图：只暴露读、列举、`status` 与 `diff`。
#[derive(Clone)]
pub struct ShadowReadView {
    inner: Arc<ShadowInner>,
}

struct ShadowInner {
    name: String,
    key: String,
    host_dev: u64,
    host_ino: u64,
    tree_root: PathBuf,
    base_root: PathBuf,
    host_root_fd: OwnedFd,
    tree_root_fd: OwnedFd,
    base_root_fd: OwnedFd,
    tmp_root_fd: OwnedFd,
    rules: PathRules,
    dir_root: DirRoot,
    /// 每个可写工作目录一把的 `act` 级互斥锁，由调用层按槽位顺序持有至结算完成。
    act_lock: Mutex<()>,
    /// 保护影子内存索引、监听与内部维护操作。
    state: Mutex<ShadowState>,
}

struct ShadowState {
    base_index: BTreeMap<PathBuf, EntrySnapshot>,
    tree_index: BTreeMap<PathBuf, EntrySnapshot>,
    shadow_inodes: HashMap<(u64, u64), BTreeSet<PathBuf>>,
    pending: BTreeMap<PathBuf, PendingEntry>,
    /// 被待定改动挡住、中断未完成或读取出错、尚待下次同步接纳的宿主候选路径。
    deferred_host_candidates: BTreeSet<PathBuf>,
    /// 上次宿主同步在展开目录事件时因中断或出错未能完成展开的递归目录候选，重试时需重新递归展开。
    deferred_host_recursive: BTreeSet<PathBuf>,
    /// 上次影子核对失败或中断时尚未处理完成的影子候选路径。
    deferred_shadow_candidates: BTreeSet<PathBuf>,
    /// 影子监听发生溢出、名称复用、短命硬链接写入或上次全量核对未完成时置位，直到完整全量核对成功才清除。
    shadow_need_full_rescan: bool,
    /// 每次同步核对全部待定路径冲突时复用的宿主侧观测缓存。
    host_pending_obs: BTreeMap<PathBuf, HostPendingObs>,
    /// 持久化的只读目录权限放宽日志（`shadows/<key>/widened`）。
    widened: WidenedLog,
    host_watch: DirWatch,
    shadow_watch: DirWatch,
    scanner: HostScanner,
}

impl ShadowReadView {
    pub fn name(&self) -> &str {
        &self.inner.name
    }

    pub fn read(&self, path: &str) -> Result<Option<String>, String> {
        self.inner.dir_root.read(path)
    }

    pub fn entries(&self, path: &str) -> Result<Option<Vec<DirEntry>>, String> {
        self.inner.dir_root.entries(path)
    }

    pub fn status(&self) -> Result<Vec<PendingChange>, String> {
        self.inner.status()
    }

    pub fn diff(&self) -> Result<Vec<PathDiff>, String> {
        self.inner.diff()
    }
}

impl ShadowDir {
    /// 打开或首次建立单个可写工作目录的持久影子。
    ///
    /// `state_dir` 须是规范化的绝对路径（节点加载配置时已规范化）：写入路径的长度按由它推出的影子绝对路径核对。
    ///
    /// - 首次建立：先安装宿主监听，再以 8 线程并行 `FICLONE` 到临时目录并原子改名就位；
    /// - 重启复用：先清空 `tmp/` 并按 `widened` 恢复未完成的目录权限放宽，再重建内存索引、
    ///   经 `FICLONE` 重新武装宿主文件写保护，并对照宿主重算冲突。
    pub fn open(
        state_dir: &Path,
        name: &str,
        host_root: &Path,
        hidden: &[String],
        regenerable: &[String],
    ) -> anyhow::Result<Self> {
        let canonical_host_root = fs::canonicalize(host_root).with_context(|| {
            format!(
                "规范化可写工作目录 `{name}` 宿主路径失败：{}",
                host_root.display()
            )
        })?;
        let rules = PathRules::build(name, hidden, regenerable, true)?;
        let key = compute_shadow_key(&canonical_host_root, hidden, regenerable);
        let shadows_root = state_dir.join("shadows");
        ensure_dir_0700(&shadows_root)?;

        let shadow_root = shadows_root.join(&key);
        let tree_root = shadow_root.join("tree");
        let base_root = shadow_root.join("base");
        let tmp_root = shadow_root.join("tmp");

        let (
            base_index,
            tree_index,
            shadow_inodes,
            pending,
            deferred_host_candidates,
            host_watch,
            shadow_watch,
            scanner,
            need_initial_sync,
        ) = if shadow_root.exists() {
            reopen_existing_shadow(
                &canonical_host_root,
                &shadow_root,
                &tree_root,
                &base_root,
                &tmp_root,
                &rules,
            )
            .with_context(|| format!("复用可写工作目录 `{name}` 的影子失败"))?
        } else {
            create_new_shadow(
                &canonical_host_root,
                &shadows_root,
                &shadow_root,
                &tree_root,
                &base_root,
                &tmp_root,
                &key,
                &rules,
            )
            .with_context(|| format!("首次建立可写工作目录 `{name}` 的影子失败"))?
        };

        let host_root_fd = open_root_fd(&canonical_host_root)
            .with_context(|| format!("打开宿主目录 `{name}` 根 fd 失败"))?;
        let host_root_stat = rustix::fs::fstat(&host_root_fd)
            .map_err(io::Error::from)
            .with_context(|| format!("读取宿主目录 `{name}` 根属性失败"))?;
        let tree_root_fd = open_root_fd(&tree_root)
            .with_context(|| format!("打开影子目录 `{name}` tree 根 fd 失败"))?;
        let base_root_fd = open_root_fd(&base_root)
            .with_context(|| format!("打开影子目录 `{name}` base 根 fd 失败"))?;
        let tmp_root_fd = open_root_fd(&tmp_root)
            .with_context(|| format!("打开影子目录 `{name}` tmp 根 fd 失败"))?;
        let widened = WidenedLog::new(&shadow_root, &tmp_root)
            .with_context(|| format!("初始化影子目录 `{name}` 权限放宽日志失败"))?;

        let dir_root = DirRoot::open_with_tmp(name, &tree_root, &tmp_root, rules.clone())
            .with_context(|| format!("打开可写工作目录 `{name}` 的影子根目录失败"))?;

        let inner = Arc::new(ShadowInner {
            name: name.to_owned(),
            key,
            host_dev: host_root_stat.st_dev,
            host_ino: host_root_stat.st_ino,
            tree_root,
            base_root,
            host_root_fd,
            tree_root_fd,
            base_root_fd,
            tmp_root_fd,
            rules,
            dir_root,
            act_lock: Mutex::new(()),
            state: Mutex::new(ShadowState {
                base_index,
                tree_index,
                shadow_inodes,
                pending,
                deferred_host_candidates,
                deferred_host_recursive: BTreeSet::new(),
                deferred_shadow_candidates: BTreeSet::new(),
                shadow_need_full_rescan: false,
                host_pending_obs: BTreeMap::new(),
                widened,
                host_watch,
                shadow_watch,
                scanner,
            }),
        });

        if need_initial_sync {
            let _ = inner.sync_from_host(None, None);
        }

        Ok(Self { inner })
    }

    pub fn name(&self) -> &str {
        &self.inner.name
    }

    pub fn shadow_key(&self) -> &str {
        &self.inner.key
    }

    pub fn tree_path(&self) -> &Path {
        &self.inner.tree_root
    }

    pub fn base_path(&self) -> &Path {
        &self.inner.base_root
    }

    /// 构造与本影子共享同一份状态的只读视图，用于 `borrow<read-dir>` 收窄。
    pub fn read_view(&self) -> ShadowReadView {
        ShadowReadView {
            inner: Arc::clone(&self.inner),
        }
    }

    /// 非阻塞尝试获取该可写目录的 `act` 级互斥锁。
    pub fn try_lock_act(&self) -> Result<MutexGuard<'_, ()>, TryLockError<MutexGuard<'_, ()>>> {
        self.inner.act_lock.try_lock()
    }

    /// 排空影子与宿主侧事件、消费后台候选、将无待定改动的宿主变化同步进影子与基准，并重算全部待定路径的冲突。
    pub fn sync_from_host(
        &self,
        deadline: Option<Instant>,
        cancelled: Option<&AtomicBool>,
    ) -> Result<(), ShadowSyncError> {
        self.inner.sync_from_host(deadline, cancelled)
    }

    pub fn read(&self, path: &str) -> Result<Option<String>, String> {
        self.inner.dir_root.read(path)
    }

    pub fn entries(&self, path: &str) -> Result<Option<Vec<DirEntry>>, String> {
        self.inner.dir_root.entries(path)
    }

    pub fn status(&self) -> Result<Vec<PendingChange>, String> {
        self.inner.status()
    }

    pub fn diff(&self) -> Result<Vec<PathDiff>, String> {
        self.inner.diff()
    }

    /// 排空影子侧事件并返回当前可见待定路径数量（不含 `hidden` 与 `regenerable`）。
    pub fn visible_pending_count(&self) -> usize {
        self.inner.visible_pending_count()
    }

    /// 整文件写入 UTF-8 文本到影子，实际改动时返回待记入回执的变更项。
    pub fn write(&self, path: &str, content: &str) -> Result<Option<WriteEffect>, String> {
        self.inner.write(path, content)
    }

    /// 丢弃指定路径的待定改动；冲突路径改为接受当前宿主版本（先影子、后基准）。
    pub fn reset(&self, path: &str) -> Result<Option<WriteEffect>, String> {
        self.inner.reset(path)
    }

    /// 恢复本影子全部常规待定路径为基准并清除影子中的 `hidden` 项，保留来自宿主的 `regenerable` 种子。
    pub fn discard(
        &self,
        deadline: Option<Instant>,
        cancelled: Option<&AtomicBool>,
    ) -> Result<Vec<WriteEffect>, DiscardError> {
        self.inner.discard(deadline, cancelled)
    }
}

impl ShadowInner {
    fn lock_state(&self) -> MutexGuard<'_, ShadowState> {
        self.state.lock().expect("影子状态互斥锁不应中毒")
    }

    fn status(&self) -> Result<Vec<PendingChange>, String> {
        let mut st = self.lock_state();
        self.reconcile_shadow_locked(&mut st, None, None)
            .map_err(|err| format!("核对待定改动失败：{err}"))?;
        let mut out = Vec::new();
        for (path, entry) in &st.pending {
            if entry.hidden {
                continue;
            }
            let utf8_path = rel_path_to_utf8(path)
                .ok_or_else(|| "待定改动包含不是有效 UTF-8 的路径".to_owned())?;
            out.push(PendingChange {
                path: utf8_path,
                action: entry.action,
                conflict: entry.conflict,
            });
        }
        out.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(out)
    }

    fn diff(&self) -> Result<Vec<PathDiff>, String> {
        let mut st = self.lock_state();
        self.reconcile_shadow_locked(&mut st, None, None)
            .map_err(|err| format!("核对待定改动失败：{err}"))?;
        let mut out = Vec::new();
        for (path, entry) in &st.pending {
            if entry.hidden {
                continue;
            }
            let utf8_path = rel_path_to_utf8(path)
                .ok_or_else(|| "待定改动包含不是有效 UTF-8 的路径".to_owned())?;
            let patch = compute_unified_diff_at(
                &utf8_path,
                path,
                entry.action,
                self.base_root_fd.as_fd(),
                self.tree_root_fd.as_fd(),
            );
            out.push(PathDiff {
                path: utf8_path,
                action: entry.action,
                conflict: entry.conflict,
                patch,
            });
        }
        out.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(out)
    }

    fn visible_pending_count(&self) -> usize {
        let mut st = self.lock_state();
        let _ = self.reconcile_shadow_locked(&mut st, None, None);
        st.pending.values().filter(|entry| !entry.hidden).count()
    }

    fn write(&self, path: &str, content: &str) -> Result<Option<WriteEffect>, String> {
        let mut st = self.lock_state();
        let _ = self.reconcile_shadow_locked(&mut st, None, None);

        let write_res = self.dir_root.write_file(path, content)?;

        if !write_res.changed {
            return Ok(None);
        }

        if let Some(rule) = write_res.regenerable_rule {
            // 若为了写入 regenerable 路径顺带新建了常规上级目录，立即将其纳入影子索引与 pending（供 discard 清理）。
            let regular_dirs: BTreeSet<PathBuf> = write_res
                .created_dirs
                .into_iter()
                .filter(|d| !self.rules.is_regenerable(d, true))
                .collect();
            if !regular_dirs.is_empty() {
                let _ = self.verify_shadow_paths_locked(&mut st, regular_dirs, None, None);
            }
            let _ = self.reconcile_shadow_locked(&mut st, None, None);
            return Ok(Some(WriteEffect {
                target: rule,
                action: "write",
                residue: Residue::Regenerable,
            }));
        }

        let mut verify_set = BTreeSet::new();
        for dir in write_res.created_dirs {
            verify_set.insert(dir);
        }
        verify_set.insert(write_res.real_rel.clone());
        self.verify_shadow_paths_locked(&mut st, verify_set, None, None)
            .map_err(|err| format!("写入 `{path}` 后更新影子状态失败：{err}"))?;
        // 消费本方法自身产生的 inotify 事件，保持事件队列干净。
        let _ = self.reconcile_shadow_locked(&mut st, None, None);

        let target = rel_path_to_utf8(&write_res.real_rel)
            .ok_or_else(|| format!("写入目标 `{path}` 解析后的路径不是有效 UTF-8"))?;
        Ok(Some(WriteEffect {
            target,
            action: "write",
            residue: Residue::None,
        }))
    }

    fn reset(&self, path: &str) -> Result<Option<WriteEffect>, String> {
        let mut st = self.lock_state();
        self.reconcile_shadow_locked(&mut st, None, None)
            .map_err(|err| format!("重置 `{path}` 前核对待定改动失败：{err}"))?;

        let target_rel = self.dir_root.resolve_for_reset(path, |rel| {
            st.base_index.get(rel).map(|e| e.kind == EntryKind::Dir)
        })?;
        let Some(&entry) = st.pending.get(&target_rel) else {
            return Ok(None);
        };
        if entry.hidden || entry.tree_hidden {
            return Err(format!("路径 `{path}` 命中 hidden 规则，禁止访问"));
        }
        // 确认存在可见待定改动后、在连带检查及任何磁盘修改之前完成真实路径 UTF-8 校验，
        // 避免修改已生效却报错且漏记 ε；没有待定改动时已在上方直接返回 Ok(None)。
        let target = rel_path_to_utf8(&target_rel)
            .ok_or_else(|| format!("重置目标 `{path}` 解析后的路径不是有效 UTF-8"))?;

        let desired = if entry.conflict {
            let raw = inspect_with_hash_at(self.host_root_fd.as_fd(), &target_rel)
                .map_err(|err| format!("读取宿主路径 `{path}` 失败：{err}"))?;
            raw.filter(|s| {
                let is_dir = s.kind == EntryKind::Dir;
                !self.rules.is_hidden(&target_rel, is_dir)
                    && !self.rules.is_regenerable(&target_rel, is_dir)
            })
        } else {
            st.base_index.get(&target_rel).cloned()
        };

        // 修改前同时检查影子、基准与待定后代及上级路径，避免先删草稿再报连带影响。
        self.check_reset_collateral(&st, path, &target_rel, desired.as_ref(), entry.conflict)?;

        if entry.conflict {
            // 冲突路径接受当前宿主版本：先完成影子的全部安装，再更新基准。
            let mut baseline_patches = Vec::new();
            self.apply_host_snapshot_to_tree_and_base(
                &mut st,
                &target_rel,
                desired.as_ref(),
                &mut baseline_patches,
            )
            .map_err(|err| format!("重置冲突路径 `{path}` 失败：{err}"))?;
            st.pending.remove(&target_rel);
            st.deferred_host_candidates.remove(&target_rel);
            st.host_pending_obs.remove(&target_rel);
            st.scanner.patch_baseline(baseline_patches);
        } else {
            // 非冲突路径仅把影子恢复为基准。
            self.restore_tree_from_base_entry(&mut st, &target_rel, desired.as_ref())
                .map_err(|err| format!("重置路径 `{path}` 失败：{err}"))?;
            st.pending.remove(&target_rel);
            st.host_pending_obs.remove(&target_rel);
        }

        let _ = self.reconcile_shadow_locked(&mut st, None, None);
        Ok(Some(WriteEffect {
            target,
            action: "reset",
            residue: Residue::None,
        }))
    }

    fn check_reset_collateral(
        &self,
        st: &ShadowState,
        raw_path: &str,
        target_rel: &Path,
        desired: Option<&EntrySnapshot>,
        is_conflict: bool,
    ) -> Result<(), String> {
        let tree_is_dir = st
            .tree_index
            .get(target_rel)
            .is_some_and(|e| e.kind == EntryKind::Dir);
        let base_is_dir = st
            .base_index
            .get(target_rel)
            .is_some_and(|e| e.kind == EntryKind::Dir);
        let desired_is_dir = desired.is_some_and(|e| e.kind == EntryKind::Dir);

        if (tree_is_dir || (is_conflict && base_is_dir)) && !desired_is_dir {
            if has_descendant_in_map(&st.pending, target_rel) {
                return Err(format!(
                    "无法重置 `{raw_path}`：会连带影响其下的其他待定路径"
                ));
            }
            if tree_is_dir
                && is_dir_non_empty_at(self.tree_root_fd.as_fd(), target_rel)
                    .map_err(|err| format!("检查影子目录 `{raw_path}` 是否为空失败：{err}"))?
            {
                return Err(format!(
                    "无法重置 `{raw_path}`：目录非空，需先处理其下的路径"
                ));
            }
            if is_conflict && base_is_dir {
                if has_descendant_in_map(&st.base_index, target_rel) {
                    return Err(format!(
                        "无法重置 `{raw_path}`：会连带影响其下的其他基准路径"
                    ));
                }
                if is_dir_non_empty_at(self.base_root_fd.as_fd(), target_rel)
                    .map_err(|err| format!("检查基准目录 `{raw_path}` 是否为空失败：{err}"))?
                {
                    return Err(format!(
                        "无法重置 `{raw_path}`：基准目录非空，需先处理其下的路径"
                    ));
                }
            }
        }

        if desired.is_some() {
            let mut cur = target_rel.parent();
            while let Some(anc) = cur {
                if anc.as_os_str().is_empty() {
                    break;
                }
                let tree_anc_is_dir = st
                    .tree_index
                    .get(anc)
                    .is_some_and(|e| e.kind == EntryKind::Dir);
                if !tree_anc_is_dir {
                    if st.pending.contains_key(anc) {
                        return Err(format!(
                            "无法重置 `{raw_path}`：上级路径存在待定改动，会受连带影响"
                        ));
                    }
                    return Err(format!("无法重置 `{raw_path}`：上级目录不存在或不是目录"));
                }
                if is_conflict {
                    let base_anc_is_dir = st
                        .base_index
                        .get(anc)
                        .is_some_and(|e| e.kind == EntryKind::Dir);
                    if !base_anc_is_dir {
                        return Err(format!(
                            "无法重置 `{raw_path}`：上级目录在基准中不存在或不是目录"
                        ));
                    }
                }
                cur = anc.parent();
            }
        }

        Ok(())
    }

    fn discard(
        &self,
        deadline: Option<Instant>,
        cancelled: Option<&AtomicBool>,
    ) -> Result<Vec<WriteEffect>, DiscardError> {
        let mut st_guard = self.lock_state();
        let st = &mut *st_guard;
        match self.reconcile_shadow_locked(st, deadline, cancelled) {
            Ok(()) => {}
            Err(ShadowSyncError::Interrupted(interrupt)) => {
                return Err(DiscardError::Interrupted {
                    partial: Vec::new(),
                    interrupt,
                });
            }
            Err(ShadowSyncError::Failed(msg)) => {
                return Err(DiscardError::Failed {
                    partial: Vec::new(),
                    message: format!("discard 前核对待定改动失败：{msg}"),
                });
            }
        }

        if st.pending.is_empty() {
            return Ok(Vec::new());
        }

        let all_pending: Vec<(PathBuf, PendingEntry)> =
            st.pending.iter().map(|(k, &v)| (k.clone(), v)).collect();
        let mut effects = Vec::new();
        let mut recorded_targets: BTreeSet<String> = BTreeSet::new();
        let mut dirs_to_fix_mode: Vec<PathBuf> = Vec::new();

        // 第一遍（由深到浅）：删除影子中多出的项、hidden 项，以及与基准类型不同的旧项。
        let mut pass1 = all_pending.clone();
        pass1.sort_by(|(a, _), (b, _)| path_depth(b).cmp(&path_depth(a)).then_with(|| a.cmp(b)));

        for (rel, entry) in &pass1 {
            if let Some(intr) = check_interrupt(deadline, cancelled) {
                self.finish_discard_dirs(st, &dirs_to_fix_mode);
                return Err(DiscardError::Interrupted {
                    partial: effects,
                    interrupt: intr,
                });
            }

            let base_opt = st.base_index.get(rel).cloned();
            let tree_kind = st.tree_index.get(rel).map(|e| e.kind);
            let need_remove = match (&base_opt, tree_kind) {
                (None, Some(_)) => true,
                (Some(b), Some(tk)) if b.kind != tk => true,
                (_, Some(_)) if entry.tree_hidden => true,
                _ => false,
            };

            if need_remove {
                // 新增的常规目录或隐藏目录连同其下内容一并递归删除（新增目录下的 regenerable 不可能来自宿主种子）。
                let remove_recursive =
                    tree_kind == Some(EntryKind::Dir) && (base_opt.is_none() || entry.tree_hidden);
                let remove_res = if remove_recursive {
                    remove_tree_nofollow_at(
                        TreeTag::Tree,
                        self.tree_root_fd.as_fd(),
                        rel,
                        &mut st.widened,
                    )
                } else {
                    remove_single_entry_at(
                        TreeTag::Tree,
                        self.tree_root_fd.as_fd(),
                        rel,
                        &mut st.widened,
                    )
                };

                if let Err(err) = remove_res {
                    self.finish_discard_dirs(st, &dirs_to_fix_mode);
                    let _ = self.reconcile_shadow_locked(st, None, None);
                    return Err(DiscardError::Failed {
                        partial: effects,
                        message: format!("discard 删除影子路径 `{}` 失败：{err}", rel.display()),
                    });
                }

                if remove_recursive {
                    for desc in descendants_in_map(&st.tree_index, rel) {
                        remove_tree_index_entry(&mut st.tree_index, &mut st.shadow_inodes, &desc);
                    }
                    for desc in descendants_in_map(&st.pending, rel) {
                        st.pending.remove(&desc);
                        st.host_pending_obs.remove(&desc);
                    }
                }
                remove_tree_index_entry(&mut st.tree_index, &mut st.shadow_inodes, rel);

                // 每完成一项变更立即记录（同一目标去重），确保后续路径失败时 partial 不漏记已落地改动。
                let target = discard_effect_target(&self.rules, rel, entry, tree_kind);
                push_unique_discard_effect(&mut effects, &mut recorded_targets, target);

                if base_opt.is_none() {
                    st.pending.remove(rel);
                    st.host_pending_obs.remove(rel);
                }
            } else if base_opt.is_none() {
                // 影子中本已不存在的 hidden/Added 项直接清出 pending。
                st.pending.remove(rel);
                st.host_pending_obs.remove(rel);
            }
        }

        // 第二遍（由浅到深）：把基准中存在的项全部恢复到影子（含先前被隐藏目录替换的基准可见文件）。
        let mut pass2 = all_pending;
        pass2.sort_by(|(a, _), (b, _)| path_depth(a).cmp(&path_depth(b)).then_with(|| a.cmp(b)));

        for (rel, entry) in &pass2 {
            let Some(base_snap) = st.base_index.get(rel).cloned() else {
                continue;
            };
            if let Some(intr) = check_interrupt(deadline, cancelled) {
                self.finish_discard_dirs(st, &dirs_to_fix_mode);
                return Err(DiscardError::Interrupted {
                    partial: effects,
                    interrupt: intr,
                });
            }

            let res = match base_snap.kind {
                EntryKind::Dir => {
                    let r = install_dir_writable_at(
                        TreeTag::Tree,
                        self.tree_root_fd.as_fd(),
                        rel,
                        &mut st.widened,
                        base_snap.mode,
                    );
                    if r.is_ok() {
                        dirs_to_fix_mode.push(rel.clone());
                        if let Ok(Some(snap)) = inspect_lstat_at(self.tree_root_fd.as_fd(), rel) {
                            set_tree_index_entry(
                                &mut st.tree_index,
                                &mut st.shadow_inodes,
                                rel,
                                snap,
                            );
                        }
                    }
                    r
                }
                EntryKind::File => install_file_atomic_at(
                    self.base_root_fd.as_fd(),
                    rel,
                    TreeTag::Tree,
                    self.tree_root_fd.as_fd(),
                    rel,
                    self.tmp_root_fd.as_fd(),
                    &mut st.widened,
                    base_snap.mode,
                    false,
                )
                .map(|(stat, _)| {
                    let snap = EntrySnapshot {
                        kind: EntryKind::File,
                        mode: base_snap.mode,
                        size: base_snap.size,
                        hash: base_snap.hash,
                        link_target: None,
                        stat,
                    };
                    set_tree_index_entry(&mut st.tree_index, &mut st.shadow_inodes, rel, snap);
                }),
                EntryKind::Symlink => {
                    let target = base_snap.link_target.as_deref().unwrap_or_default();
                    install_symlink_atomic_at(
                        target,
                        TreeTag::Tree,
                        self.tree_root_fd.as_fd(),
                        rel,
                        self.tmp_root_fd.as_fd(),
                        &mut st.widened,
                    )
                    .map(|stat| {
                        let snap = EntrySnapshot {
                            kind: EntryKind::Symlink,
                            mode: stat.mode,
                            size: stat.size,
                            hash: None,
                            link_target: base_snap.link_target.clone(),
                            stat,
                        };
                        set_tree_index_entry(&mut st.tree_index, &mut st.shadow_inodes, rel, snap);
                    })
                }
            };

            if let Err(err) = res {
                self.finish_discard_dirs(st, &dirs_to_fix_mode);
                let _ = self.reconcile_shadow_locked(st, None, None);
                return Err(DiscardError::Failed {
                    partial: effects,
                    message: format!("discard 恢复影子路径 `{}` 失败：{err}", rel.display()),
                });
            }

            st.pending.remove(rel);
            st.host_pending_obs.remove(rel);
            let target = discard_effect_target(&self.rules, rel, entry, Some(base_snap.kind));
            push_unique_discard_effect(&mut effects, &mut recorded_targets, target);
        }

        // 第三遍（由深到浅）：恢复目录精确权限，刷新索引，并清理影子监听队列。
        self.finish_discard_dirs(st, &dirs_to_fix_mode);
        let _ = self.reconcile_shadow_locked(st, None, None);
        Ok(effects)
    }

    fn finish_discard_dirs(&self, st: &mut ShadowState, dirs_to_fix_mode: &[PathBuf]) {
        st.widened
            .restore_all(self.tree_root_fd.as_fd(), self.base_root_fd.as_fd());
        for rel in dirs_to_fix_mode {
            if let Ok(Some(snap)) = inspect_lstat_at(self.tree_root_fd.as_fd(), rel) {
                set_tree_index_entry(&mut st.tree_index, &mut st.shadow_inodes, rel, snap);
            }
        }
    }

    fn sync_from_host(
        &self,
        deadline: Option<Instant>,
        cancelled: Option<&AtomicBool>,
    ) -> Result<(), ShadowSyncError> {
        let mut st_guard = self.lock_state();
        let st = &mut *st_guard;

        // 1. 先完整核对影子侧事件；核对失败或中断时立刻中止，避免覆盖尚未识别的影子草稿。
        self.reconcile_shadow_locked(st, deadline, cancelled)?;

        // 2. 排空宿主事件与后台候选；溢出或目录原名复用/交换时重建宿主监听并唤醒后台扫描。
        let host_ev = st.host_watch.drain();
        if host_ev.overflowed || host_ev.need_full_rescan {
            st.host_watch.rebuild();
            st.scanner.wake();
            st.host_pending_obs.clear();
        }
        let bg_candidates = st.scanner.take_ready_candidates();

        let mut recursive_set = std::mem::take(&mut st.deferred_host_recursive);
        recursive_set.extend(host_ev.recursive);

        // 仅对本次收到的宿主 inotify 路径事件立即失效其 host_pending_obs；
        // 后台扫描候选由 Pass 4 的 stat 与 racy 时间窗判定是否重读；
        // 递归目录事件放到下方带中断检查的目录展开循环中按子树范围失效。
        for p in &host_ev.paths {
            st.host_pending_obs.remove(p);
        }

        let mut candidates = host_ev.paths;
        candidates.extend(bg_candidates);
        candidates.extend(std::mem::take(&mut st.deferred_host_candidates));

        // 展开目录事件对应的已知与宿主现存后代（不跟随符号链接，且检查中断）；
        // 若中途中断或报错，尚未展开完成的目录仍作为递归候选保留在 `deferred_host_recursive` 中。
        let recursive_dirs: Vec<PathBuf> = recursive_set.into_iter().collect();
        for (idx, dir) in recursive_dirs.iter().enumerate() {
            if let Some(intr) = check_interrupt(deadline, cancelled) {
                st.deferred_host_candidates.extend(candidates);
                st.deferred_host_recursive
                    .extend(recursive_dirs[idx..].iter().cloned());
                st.scanner.wake();
                return Err(ShadowSyncError::Interrupted(intr));
            }
            // 按 BTreeMap::range 仅删除当前目录及其后代的观测，避免 O(目录数 × 观测数) 的全表扫描。
            let stale_obs: Vec<PathBuf> = st
                .host_pending_obs
                .range(dir.clone()..)
                .take_while(|(k, _)| k.starts_with(dir))
                .map(|(k, _)| k.clone())
                .collect();
            for k in stale_obs {
                st.host_pending_obs.remove(&k);
            }
            candidates.insert(dir.clone());
            candidates.extend(descendants_in_map(&st.base_index, dir));
            candidates.extend(descendants_in_map(&st.pending, dir));
            if let Err(err) = collect_live_host_descendants_at(
                self.host_root_fd.as_fd(),
                &self.rules,
                dir,
                &mut candidates,
                deadline,
                cancelled,
            ) {
                st.deferred_host_candidates.extend(candidates);
                st.deferred_host_recursive
                    .extend(recursive_dirs[idx..].iter().cloned());
                return Err(err);
            }
        }

        if !candidates.is_empty() {
            // 补入候选路径尚未存在于基准中的严格上级目录。
            let mut missing_ancestors = Vec::new();
            for p in &candidates {
                if let Some(intr) = check_interrupt(deadline, cancelled) {
                    st.deferred_host_candidates
                        .extend(candidates.iter().cloned());
                    return Err(ShadowSyncError::Interrupted(intr));
                }
                let mut cur = p.parent();
                while let Some(anc) = cur {
                    if anc.as_os_str().is_empty() {
                        break;
                    }
                    if !st.base_index.contains_key(anc) {
                        missing_ancestors.push(anc.to_path_buf());
                    }
                    cur = anc.parent();
                }
            }
            candidates.extend(missing_ancestors);

            // 宿主删除、换型（含目录换成符号链接或隐藏目录）或新建目录时，展开受影响的后代路径。
            let initial_list: Vec<PathBuf> = candidates.iter().cloned().collect();
            for p in initial_list {
                if let Some(intr) = check_interrupt(deadline, cancelled) {
                    st.deferred_host_candidates.extend(candidates);
                    return Err(ShadowSyncError::Interrupted(intr));
                }
                if self.rules.is_reserved_git(&p) {
                    candidates.remove(&p);
                    continue;
                }
                let host_lstat = match inspect_lstat_at(self.host_root_fd.as_fd(), &p) {
                    Ok(opt) => opt,
                    Err(err) => {
                        st.deferred_host_candidates.extend(candidates);
                        return Err(ShadowSyncError::Failed(format!(
                            "读取宿主路径 `{}` 属性失败：{err}",
                            p.display()
                        )));
                    }
                };
                // 宿主条目按宿主实际类型分类：若换成了 hidden/regenerable，则宿主可见视图视为缺失。
                let host_kind = host_lstat
                    .filter(|s| {
                        let is_dir = s.kind == EntryKind::Dir;
                        !self.rules.is_hidden(&p, is_dir) && !self.rules.is_regenerable(&p, is_dir)
                    })
                    .map(|s| s.kind);
                let base_was_dir = st
                    .base_index
                    .get(&p)
                    .is_some_and(|e| e.kind == EntryKind::Dir);
                if base_was_dir && host_kind != Some(EntryKind::Dir) {
                    candidates.extend(descendants_in_map(&st.base_index, &p));
                    candidates.extend(descendants_in_map(&st.pending, &p));
                }
                if host_kind == Some(EntryKind::Dir)
                    && !base_was_dir
                    && let Err(err) = collect_live_host_descendants_at(
                        self.host_root_fd.as_fd(),
                        &self.rules,
                        &p,
                        &mut candidates,
                        deadline,
                        cancelled,
                    )
                {
                    st.deferred_host_candidates.extend(candidates);
                    st.deferred_host_recursive.insert(p);
                    return Err(err);
                }
            }

            // 按各自树的实际类型过滤：只要基准或待定集合中有该路径，或宿主存在非 hidden/regenerable 条目，就必须保留。
            let mut filtered_candidates = BTreeSet::new();
            for p in &candidates {
                if let Some(intr) = check_interrupt(deadline, cancelled) {
                    st.deferred_host_candidates
                        .extend(candidates.iter().cloned());
                    return Err(ShadowSyncError::Interrupted(intr));
                }
                if self.rules.is_reserved_git(p) {
                    continue;
                }
                if st.base_index.contains_key(p) || st.pending.contains_key(p) {
                    filtered_candidates.insert(p.clone());
                    continue;
                }
                let host_lstat = match inspect_lstat_at(self.host_root_fd.as_fd(), p) {
                    Ok(opt) => opt,
                    Err(err) => {
                        let msg = format!("读取宿主路径 `{}` 属性失败：{err}", p.display());
                        st.deferred_host_candidates
                            .extend(candidates.iter().cloned());
                        return Err(ShadowSyncError::Failed(msg));
                    }
                };
                if let Some(s) = host_lstat {
                    let is_dir = s.kind == EntryKind::Dir;
                    if !self.rules.is_hidden(p, is_dir) && !self.rules.is_regenerable(p, is_dir) {
                        filtered_candidates.insert(p.clone());
                    }
                }
            }
            candidates = filtered_candidates;
        }

        // 已在 pending 中的路径不会在 Pass 1/2 安装或删除：
        // - 非 hidden 待定路径统一交由 Pass 4 复用 host_pending_obs 重算冲突，确保同一次同步最多读一次内容；
        // - 纯 hidden 待定路径留回延后候选，待 discard 清除后再接纳宿主同名可见条目。
        candidates.retain(|p| {
            if let Some(pe) = st.pending.get(p) {
                if pe.hidden {
                    st.deferred_host_candidates.insert(p.clone());
                }
                false
            } else {
                true
            }
        });

        if !candidates.is_empty() {
            let mut remaining_candidates: BTreeSet<PathBuf> = candidates.clone();

            // 3. 读取本次候选路径的当前宿主快照；真实 I/O 错误（如 EACCES）原样报错并保留全部候选，绝不当作已删除。
            let mut host_snaps: BTreeMap<PathBuf, Option<EntrySnapshot>> = BTreeMap::new();
            for p in &candidates {
                if let Some(intr) = check_interrupt(deadline, cancelled) {
                    st.deferred_host_candidates.extend(remaining_candidates);
                    return Err(ShadowSyncError::Interrupted(intr));
                }
                let raw_snap = match inspect_with_hash_at(self.host_root_fd.as_fd(), p) {
                    Ok(opt) => opt,
                    Err(err) => {
                        st.deferred_host_candidates.extend(remaining_candidates);
                        return Err(ShadowSyncError::Failed(format!(
                            "读取宿主路径 `{}` 失败：{err}",
                            p.display()
                        )));
                    }
                };
                let filtered = raw_snap.filter(|s| {
                    let is_dir = s.kind == EntryKind::Dir;
                    !self.rules.is_hidden(p, is_dir) && !self.rules.is_regenerable(p, is_dir)
                });
                host_snaps.insert(p.clone(), filtered);
            }

            let mut baseline_patches = Vec::new();
            let mut dirs_to_restore_tree: Vec<(PathBuf, u32)> = Vec::new();
            let mut dirs_to_restore_base: Vec<(PathBuf, u32)> = Vec::new();
            let mut touched_shadow = false;

            // Pass 1（由深到浅）：处理宿主删除与目录换型为非目录。
            let mut ordered_desc: Vec<PathBuf> = candidates.iter().cloned().collect();
            ordered_desc.sort_by(|a, b| path_depth(b).cmp(&path_depth(a)).then_with(|| a.cmp(b)));

            for p in &ordered_desc {
                if let Some(intr) = check_interrupt(deadline, cancelled) {
                    self.abort_sync_passes(
                        st,
                        remaining_candidates,
                        &dirs_to_restore_tree,
                        &dirs_to_restore_base,
                        baseline_patches,
                    );
                    return Err(ShadowSyncError::Interrupted(intr));
                }

                let mut host_opt = host_snaps.get(p).cloned().flatten();
                let equal = match entries_semantically_equal_at(
                    st.base_index.get_mut(p),
                    self.base_root_fd.as_fd(),
                    host_opt.as_mut(),
                    self.host_root_fd.as_fd(),
                    p,
                ) {
                    Ok(eq) => eq,
                    Err(err) => {
                        self.abort_sync_passes(
                            st,
                            remaining_candidates,
                            &dirs_to_restore_tree,
                            &dirs_to_restore_base,
                            baseline_patches,
                        );
                        return Err(ShadowSyncError::Failed(format!(
                            "比较宿主与基准路径 `{}` 失败：{err}",
                            p.display()
                        )));
                    }
                };
                let host_differs = !equal;

                if st.pending.contains_key(p) {
                    if let Some(pe) = st.pending.get_mut(p) {
                        pe.conflict = host_differs;
                    }
                    if host_differs {
                        st.deferred_host_candidates.insert(p.clone());
                    }
                    remaining_candidates.remove(p);
                    continue;
                }

                if !host_differs {
                    remaining_candidates.remove(p);
                    continue;
                }

                let want_remove_or_non_dir = match &host_opt {
                    None => true,
                    Some(h) => {
                        let tree_is_dir = st
                            .tree_index
                            .get(p)
                            .is_some_and(|e| e.kind == EntryKind::Dir);
                        let base_is_dir = st
                            .base_index
                            .get(p)
                            .is_some_and(|e| e.kind == EntryKind::Dir);
                        (tree_is_dir || base_is_dir) && h.kind != EntryKind::Dir
                    }
                };

                if !want_remove_or_non_dir {
                    // 留待 Pass 2 安装，绝不提前移出 `remaining_candidates`。
                    continue;
                }

                // 保护待定后代：若该目录下仍有待定路径，保留目录并留作下次同步候选。
                if has_descendant_in_map(&st.pending, p) {
                    st.deferred_host_candidates.insert(p.clone());
                    remaining_candidates.remove(p);
                    continue;
                }

                // 先更新影子，再更新基准。
                if remove_single_entry_at(
                    TreeTag::Tree,
                    self.tree_root_fd.as_fd(),
                    p,
                    &mut st.widened,
                )
                .is_err()
                {
                    st.deferred_host_candidates.insert(p.clone());
                    remaining_candidates.remove(p);
                    continue;
                }
                touched_shadow = true;
                remove_tree_index_entry(&mut st.tree_index, &mut st.shadow_inodes, p);

                if remove_single_entry_at(
                    TreeTag::Base,
                    self.base_root_fd.as_fd(),
                    p,
                    &mut st.widened,
                )
                .is_ok()
                {
                    st.base_index.remove(p);
                    baseline_patches.push((p.clone(), None));
                    if host_opt.is_none() {
                        remaining_candidates.remove(p);
                    }
                } else {
                    st.deferred_host_candidates.insert(p.clone());
                    remaining_candidates.remove(p);
                }
            }

            // Pass 2（由浅到深）：接纳宿主新增或修改的对象，先更新影子，再更新基准。
            let mut ordered_asc: Vec<PathBuf> = candidates.iter().cloned().collect();
            ordered_asc.sort_by(|a, b| path_depth(a).cmp(&path_depth(b)).then_with(|| a.cmp(b)));

            for p in &ordered_asc {
                if let Some(intr) = check_interrupt(deadline, cancelled) {
                    self.abort_sync_passes(
                        st,
                        remaining_candidates,
                        &dirs_to_restore_tree,
                        &dirs_to_restore_base,
                        baseline_patches,
                    );
                    return Err(ShadowSyncError::Interrupted(intr));
                }

                if !remaining_candidates.contains(p) {
                    continue;
                }

                let Some(mut host_snap) = host_snaps.get(p).cloned().flatten() else {
                    remaining_candidates.remove(p);
                    continue;
                };
                let equal = match entries_semantically_equal_at(
                    st.base_index.get_mut(p),
                    self.base_root_fd.as_fd(),
                    Some(&mut host_snap),
                    self.host_root_fd.as_fd(),
                    p,
                ) {
                    Ok(eq) => eq,
                    Err(err) => {
                        self.abort_sync_passes(
                            st,
                            remaining_candidates,
                            &dirs_to_restore_tree,
                            &dirs_to_restore_base,
                            baseline_patches,
                        );
                        return Err(ShadowSyncError::Failed(format!(
                            "比较宿主与基准路径 `{}` 失败：{err}",
                            p.display()
                        )));
                    }
                };
                let host_differs = !equal;

                if st.pending.contains_key(p) {
                    if let Some(pe) = st.pending.get_mut(p) {
                        pe.conflict = host_differs;
                    }
                    if host_differs {
                        st.deferred_host_candidates.insert(p.clone());
                    }
                    remaining_candidates.remove(p);
                    continue;
                }

                if !host_differs {
                    remaining_candidates.remove(p);
                    continue;
                }

                // 若任一严格上级在影子或基准中不是目录（例如被待定新增目录或文件挡住），暂留候选等待解除。
                if ancestor_blocks_sync(st, p) {
                    st.deferred_host_candidates.insert(p.clone());
                    remaining_candidates.remove(p);
                    continue;
                }

                match host_snap.kind {
                    EntryKind::Dir => {
                        if install_dir_writable_at(
                            TreeTag::Tree,
                            self.tree_root_fd.as_fd(),
                            p,
                            &mut st.widened,
                            host_snap.mode,
                        )
                        .is_err()
                        {
                            st.deferred_host_candidates.insert(p.clone());
                            remaining_candidates.remove(p);
                            continue;
                        }
                        touched_shadow = true;
                        dirs_to_restore_tree.push((p.clone(), host_snap.mode));

                        if install_dir_writable_at(
                            TreeTag::Base,
                            self.base_root_fd.as_fd(),
                            p,
                            &mut st.widened,
                            host_snap.mode,
                        )
                        .is_ok()
                        {
                            dirs_to_restore_base.push((p.clone(), host_snap.mode));
                            if let Ok(Some(ts)) = inspect_lstat_at(self.tree_root_fd.as_fd(), p) {
                                set_tree_index_entry(
                                    &mut st.tree_index,
                                    &mut st.shadow_inodes,
                                    p,
                                    ts,
                                );
                            }
                            let mut base_snap = host_snap.clone();
                            if let Ok(Some(bs)) = inspect_lstat_at(self.base_root_fd.as_fd(), p) {
                                base_snap.stat = bs.stat;
                            }
                            st.base_index.insert(p.clone(), base_snap.clone());
                            baseline_patches
                                .push((p.clone(), Some(BaselineMeta::from_snapshot(&base_snap))));
                        } else {
                            st.deferred_host_candidates.insert(p.clone());
                        }
                    }
                    EntryKind::File => {
                        let (tree_stat, tree_hash) = match install_file_atomic_at(
                            self.host_root_fd.as_fd(),
                            p,
                            TreeTag::Tree,
                            self.tree_root_fd.as_fd(),
                            p,
                            self.tmp_root_fd.as_fd(),
                            &mut st.widened,
                            host_snap.mode,
                            true,
                        ) {
                            Ok(v) => v,
                            Err(err) if err.kind() == io::ErrorKind::NotFound => {
                                st.deferred_host_candidates.insert(p.clone());
                                remaining_candidates.remove(p);
                                continue;
                            }
                            Err(err) => {
                                self.abort_sync_passes(
                                    st,
                                    remaining_candidates,
                                    &dirs_to_restore_tree,
                                    &dirs_to_restore_base,
                                    baseline_patches,
                                );
                                return Err(ShadowSyncError::Failed(format!(
                                    "同步文件 `{}` 到影子失败：{err}",
                                    p.display()
                                )));
                            }
                        };
                        touched_shadow = true;
                        let Ok((base_stat, _)) = install_file_atomic_at(
                            self.tree_root_fd.as_fd(),
                            p,
                            TreeTag::Base,
                            self.base_root_fd.as_fd(),
                            p,
                            self.tmp_root_fd.as_fd(),
                            &mut st.widened,
                            host_snap.mode,
                            false,
                        ) else {
                            st.deferred_host_candidates.insert(p.clone());
                            remaining_candidates.remove(p);
                            continue;
                        };

                        let final_hash = tree_hash.or(host_snap.hash);
                        let tree_snap = EntrySnapshot {
                            kind: EntryKind::File,
                            mode: host_snap.mode,
                            size: tree_stat.size,
                            hash: final_hash,
                            link_target: None,
                            stat: tree_stat,
                        };
                        let base_snap = EntrySnapshot {
                            kind: EntryKind::File,
                            mode: host_snap.mode,
                            size: base_stat.size,
                            hash: final_hash,
                            link_target: None,
                            stat: base_stat,
                        };
                        set_tree_index_entry(
                            &mut st.tree_index,
                            &mut st.shadow_inodes,
                            p,
                            tree_snap,
                        );
                        st.base_index.insert(p.clone(), base_snap.clone());
                        baseline_patches
                            .push((p.clone(), Some(BaselineMeta::from_snapshot(&base_snap))));
                    }
                    EntryKind::Symlink => {
                        let target = host_snap.link_target.as_deref().unwrap_or_default();
                        let Ok(tree_stat) = install_symlink_atomic_at(
                            target,
                            TreeTag::Tree,
                            self.tree_root_fd.as_fd(),
                            p,
                            self.tmp_root_fd.as_fd(),
                            &mut st.widened,
                        ) else {
                            st.deferred_host_candidates.insert(p.clone());
                            remaining_candidates.remove(p);
                            continue;
                        };
                        touched_shadow = true;
                        let Ok(base_stat) = install_symlink_atomic_at(
                            target,
                            TreeTag::Base,
                            self.base_root_fd.as_fd(),
                            p,
                            self.tmp_root_fd.as_fd(),
                            &mut st.widened,
                        ) else {
                            st.deferred_host_candidates.insert(p.clone());
                            remaining_candidates.remove(p);
                            continue;
                        };
                        let tree_snap = EntrySnapshot {
                            kind: EntryKind::Symlink,
                            mode: tree_stat.mode,
                            size: tree_stat.size,
                            hash: None,
                            link_target: host_snap.link_target.clone(),
                            stat: tree_stat,
                        };
                        let base_snap = EntrySnapshot {
                            kind: EntryKind::Symlink,
                            mode: base_stat.mode,
                            size: base_stat.size,
                            hash: None,
                            link_target: host_snap.link_target.clone(),
                            stat: base_stat,
                        };
                        set_tree_index_entry(
                            &mut st.tree_index,
                            &mut st.shadow_inodes,
                            p,
                            tree_snap,
                        );
                        st.base_index.insert(p.clone(), base_snap.clone());
                        baseline_patches
                            .push((p.clone(), Some(BaselineMeta::from_snapshot(&base_snap))));
                    }
                }
                remaining_candidates.remove(p);
            }

            // Pass 3（由深到浅）：恢复目录精确权限，刷新索引中的权限位与 stat，推送基准补丁。
            self.finish_sync_dirs(st, &dirs_to_restore_tree, &dirs_to_restore_base);
            st.scanner.patch_baseline(baseline_patches);

            if touched_shadow {
                self.reconcile_shadow_locked(st, deadline, cancelled)?;
            }
        }

        // Pass 4：无论本次是否有宿主候选，都对全部非 hidden 待定路径重算冲突状态（先看宿主 stat 观测，变了再读内容）。
        self.recompute_all_pending_conflicts_locked(st, deadline, cancelled)
    }

    fn finish_sync_dirs(
        &self,
        st: &mut ShadowState,
        dirs_to_restore_tree: &[(PathBuf, u32)],
        dirs_to_restore_base: &[(PathBuf, u32)],
    ) {
        st.widened
            .restore_all(self.tree_root_fd.as_fd(), self.base_root_fd.as_fd());
        for (p, mode) in dirs_to_restore_tree {
            if let Ok(Some(snap)) = inspect_lstat_at(self.tree_root_fd.as_fd(), p) {
                set_tree_index_entry(&mut st.tree_index, &mut st.shadow_inodes, p, snap);
            } else if let Some(e) = st.tree_index.get_mut(p) {
                e.mode = *mode & 0o7777;
                e.stat.mode = *mode & 0o7777;
            }
        }
        for (p, mode) in dirs_to_restore_base {
            if let Ok(Some(snap)) = inspect_lstat_at(self.base_root_fd.as_fd(), p) {
                st.base_index.insert(p.clone(), snap);
            } else if let Some(e) = st.base_index.get_mut(p) {
                e.mode = *mode & 0o7777;
                e.stat.mode = *mode & 0o7777;
            }
        }
    }

    fn abort_sync_passes(
        &self,
        st: &mut ShadowState,
        remaining_candidates: BTreeSet<PathBuf>,
        dirs_to_restore_tree: &[(PathBuf, u32)],
        dirs_to_restore_base: &[(PathBuf, u32)],
        baseline_patches: Vec<(PathBuf, Option<BaselineMeta>)>,
    ) {
        st.deferred_host_candidates.extend(remaining_candidates);
        self.finish_sync_dirs(st, dirs_to_restore_tree, dirs_to_restore_base);
        st.scanner.patch_baseline(baseline_patches);
    }

    fn recompute_all_pending_conflicts_locked(
        &self,
        st: &mut ShadowState,
        deadline: Option<Instant>,
        cancelled: Option<&AtomicBool>,
    ) -> Result<(), ShadowSyncError> {
        if st.pending.is_empty() {
            st.host_pending_obs.clear();
            return Ok(());
        }

        let pending_keys: Vec<PathBuf> = st
            .pending
            .iter()
            .filter(|(_, entry)| !entry.hidden)
            .map(|(p, _)| p.clone())
            .collect();
        st.host_pending_obs
            .retain(|k, _| st.pending.contains_key(k));

        let check_now_ns = now_realtime_ns();
        for p in pending_keys {
            if let Some(intr) = check_interrupt(deadline, cancelled) {
                return Err(ShadowSyncError::Interrupted(intr));
            }
            let raw_lstat = inspect_lstat_at(self.host_root_fd.as_fd(), &p).map_err(|err| {
                st.deferred_host_candidates.insert(p.clone());
                ShadowSyncError::Failed(format!(
                    "重算冲突读取宿主路径 `{}` 属性失败：{err}",
                    p.display()
                ))
            })?;
            let visible_lstat = raw_lstat.filter(|s| {
                let is_dir = s.kind == EntryKind::Dir;
                !self.rules.is_hidden(&p, is_dir) && !self.rules.is_regenerable(&p, is_dir)
            });

            let mut host_snap_opt = match visible_lstat {
                None => {
                    st.host_pending_obs.remove(&p);
                    None
                }
                Some(lstat_snap) => match lstat_snap.kind {
                    EntryKind::Dir | EntryKind::Symlink => {
                        st.host_pending_obs.insert(
                            p.clone(),
                            HostPendingObs {
                                kind: lstat_snap.kind,
                                stat: lstat_snap.stat,
                                hash: None,
                                observed_at_ns: check_now_ns,
                            },
                        );
                        Some(lstat_snap)
                    }
                    EntryKind::File => {
                        let cached_hash = if let Some(prev) = st.host_pending_obs.get(&p)
                            && prev.kind == EntryKind::File
                            && prev.stat == lstat_snap.stat
                            && !prev.stat.is_within_racy_window(prev.observed_at_ns)
                            && !lstat_snap.stat.is_within_racy_window(check_now_ns)
                        {
                            prev.hash
                        } else {
                            None
                        };

                        if let Some(h) = cached_hash {
                            Some(EntrySnapshot {
                                hash: Some(h),
                                ..lstat_snap
                            })
                        } else {
                            match hash_host_file_via_ficlone_at(
                                self.host_root_fd.as_fd(),
                                &p,
                                self.tmp_root_fd.as_fd(),
                            ) {
                                Ok(Some((post_stat, h))) => {
                                    st.host_pending_obs.insert(
                                        p.clone(),
                                        HostPendingObs {
                                            kind: EntryKind::File,
                                            stat: post_stat,
                                            hash: Some(h),
                                            observed_at_ns: check_now_ns,
                                        },
                                    );
                                    Some(EntrySnapshot {
                                        kind: EntryKind::File,
                                        mode: post_stat.mode,
                                        size: post_stat.size,
                                        hash: Some(h),
                                        link_target: None,
                                        stat: post_stat,
                                    })
                                }
                                Ok(None) => {
                                    st.host_pending_obs.remove(&p);
                                    None
                                }
                                Err(err) => {
                                    st.deferred_host_candidates.insert(p.clone());
                                    return Err(ShadowSyncError::Failed(format!(
                                        "重算冲突读取宿主文件 `{}` 失败：{err}",
                                        p.display()
                                    )));
                                }
                            }
                        }
                    }
                },
            };

            let equal = entries_semantically_equal_at(
                st.base_index.get_mut(&p),
                self.base_root_fd.as_fd(),
                host_snap_opt.as_mut(),
                self.host_root_fd.as_fd(),
                &p,
            )
            .map_err(|err| {
                ShadowSyncError::Failed(format!("重算冲突比较路径 `{}` 失败：{err}", p.display()))
            })?;
            let host_differs = !equal;
            if let Some(pe) = st.pending.get_mut(&p) {
                pe.conflict = host_differs;
            }
            if host_differs {
                st.deferred_host_candidates.insert(p);
            }
        }

        Ok(())
    }

    fn apply_host_snapshot_to_tree_and_base(
        &self,
        st: &mut ShadowState,
        rel: &Path,
        host_opt: Option<&EntrySnapshot>,
        baseline_patches: &mut Vec<(PathBuf, Option<BaselineMeta>)>,
    ) -> io::Result<()> {
        let tree_kind = st.tree_index.get(rel).map(|e| e.kind);
        let base_kind = st.base_index.get(rel).map(|e| e.kind);

        match host_opt {
            None => {
                remove_single_entry_at(
                    TreeTag::Tree,
                    self.tree_root_fd.as_fd(),
                    rel,
                    &mut st.widened,
                )?;
                remove_tree_index_entry(&mut st.tree_index, &mut st.shadow_inodes, rel);
                remove_single_entry_at(
                    TreeTag::Base,
                    self.base_root_fd.as_fd(),
                    rel,
                    &mut st.widened,
                )?;
                st.base_index.remove(rel);
                baseline_patches.push((rel.to_path_buf(), None));
            }
            Some(host_snap) => {
                // 允许换型时，先完整完成影子 `tree/` 的替换与安装，再修改 `base/`。
                if tree_kind.is_some_and(|tk| tk != host_snap.kind) {
                    remove_single_entry_at(
                        TreeTag::Tree,
                        self.tree_root_fd.as_fd(),
                        rel,
                        &mut st.widened,
                    )?;
                    remove_tree_index_entry(&mut st.tree_index, &mut st.shadow_inodes, rel);
                }

                match host_snap.kind {
                    EntryKind::File => {
                        let (tree_stat, tree_hash) = install_file_atomic_at(
                            self.host_root_fd.as_fd(),
                            rel,
                            TreeTag::Tree,
                            self.tree_root_fd.as_fd(),
                            rel,
                            self.tmp_root_fd.as_fd(),
                            &mut st.widened,
                            host_snap.mode,
                            true,
                        )?;
                        let hash = tree_hash.or(host_snap.hash);
                        let tree_snap = EntrySnapshot {
                            kind: EntryKind::File,
                            mode: host_snap.mode,
                            size: tree_stat.size,
                            hash,
                            link_target: None,
                            stat: tree_stat,
                        };
                        set_tree_index_entry(
                            &mut st.tree_index,
                            &mut st.shadow_inodes,
                            rel,
                            tree_snap,
                        );

                        if base_kind.is_some_and(|bk| bk != EntryKind::File) {
                            remove_single_entry_at(
                                TreeTag::Base,
                                self.base_root_fd.as_fd(),
                                rel,
                                &mut st.widened,
                            )?;
                        }
                        let (base_stat, _) = install_file_atomic_at(
                            self.tree_root_fd.as_fd(),
                            rel,
                            TreeTag::Base,
                            self.base_root_fd.as_fd(),
                            rel,
                            self.tmp_root_fd.as_fd(),
                            &mut st.widened,
                            host_snap.mode,
                            false,
                        )?;
                        let base_snap = EntrySnapshot {
                            kind: EntryKind::File,
                            mode: host_snap.mode,
                            size: base_stat.size,
                            hash,
                            link_target: None,
                            stat: base_stat,
                        };
                        st.base_index.insert(rel.to_path_buf(), base_snap.clone());
                        baseline_patches.push((
                            rel.to_path_buf(),
                            Some(BaselineMeta::from_snapshot(&base_snap)),
                        ));
                    }
                    EntryKind::Dir => {
                        install_dir_writable_at(
                            TreeTag::Tree,
                            self.tree_root_fd.as_fd(),
                            rel,
                            &mut st.widened,
                            host_snap.mode,
                        )?;
                        set_dir_mode_nofollow(self.tree_root_fd.as_fd(), rel, host_snap.mode)?;
                        st.widened
                            .restore_all(self.tree_root_fd.as_fd(), self.base_root_fd.as_fd());
                        if let Some(ts) = inspect_lstat_at(self.tree_root_fd.as_fd(), rel)? {
                            set_tree_index_entry(
                                &mut st.tree_index,
                                &mut st.shadow_inodes,
                                rel,
                                ts,
                            );
                        }

                        if base_kind.is_some_and(|bk| bk != EntryKind::Dir) {
                            remove_single_entry_at(
                                TreeTag::Base,
                                self.base_root_fd.as_fd(),
                                rel,
                                &mut st.widened,
                            )?;
                        }
                        install_dir_writable_at(
                            TreeTag::Base,
                            self.base_root_fd.as_fd(),
                            rel,
                            &mut st.widened,
                            host_snap.mode,
                        )?;
                        set_dir_mode_nofollow(self.base_root_fd.as_fd(), rel, host_snap.mode)?;
                        st.widened
                            .restore_all(self.tree_root_fd.as_fd(), self.base_root_fd.as_fd());

                        let mut base_snap = host_snap.clone();
                        if let Some(bs) = inspect_lstat_at(self.base_root_fd.as_fd(), rel)? {
                            base_snap.stat = bs.stat;
                        }
                        st.base_index.insert(rel.to_path_buf(), base_snap.clone());
                        baseline_patches.push((
                            rel.to_path_buf(),
                            Some(BaselineMeta::from_snapshot(&base_snap)),
                        ));
                    }
                    EntryKind::Symlink => {
                        let target = host_snap.link_target.as_deref().unwrap_or_default();
                        let tree_stat = install_symlink_atomic_at(
                            target,
                            TreeTag::Tree,
                            self.tree_root_fd.as_fd(),
                            rel,
                            self.tmp_root_fd.as_fd(),
                            &mut st.widened,
                        )?;
                        let tree_snap = EntrySnapshot {
                            kind: EntryKind::Symlink,
                            mode: tree_stat.mode,
                            size: tree_stat.size,
                            hash: None,
                            link_target: host_snap.link_target.clone(),
                            stat: tree_stat,
                        };
                        set_tree_index_entry(
                            &mut st.tree_index,
                            &mut st.shadow_inodes,
                            rel,
                            tree_snap,
                        );

                        if base_kind.is_some_and(|bk| bk != EntryKind::Symlink) {
                            remove_single_entry_at(
                                TreeTag::Base,
                                self.base_root_fd.as_fd(),
                                rel,
                                &mut st.widened,
                            )?;
                        }
                        let base_stat = install_symlink_atomic_at(
                            target,
                            TreeTag::Base,
                            self.base_root_fd.as_fd(),
                            rel,
                            self.tmp_root_fd.as_fd(),
                            &mut st.widened,
                        )?;
                        let base_snap = EntrySnapshot {
                            kind: EntryKind::Symlink,
                            mode: base_stat.mode,
                            size: base_stat.size,
                            hash: None,
                            link_target: host_snap.link_target.clone(),
                            stat: base_stat,
                        };
                        st.base_index.insert(rel.to_path_buf(), base_snap.clone());
                        baseline_patches.push((
                            rel.to_path_buf(),
                            Some(BaselineMeta::from_snapshot(&base_snap)),
                        ));
                    }
                }
            }
        }
        Ok(())
    }

    fn restore_tree_from_base_entry(
        &self,
        st: &mut ShadowState,
        rel: &Path,
        base_opt: Option<&EntrySnapshot>,
    ) -> io::Result<()> {
        let tree_kind = st.tree_index.get(rel).map(|e| e.kind);
        match base_opt {
            None => {
                remove_single_entry_at(
                    TreeTag::Tree,
                    self.tree_root_fd.as_fd(),
                    rel,
                    &mut st.widened,
                )?;
                remove_tree_index_entry(&mut st.tree_index, &mut st.shadow_inodes, rel);
            }
            Some(base_snap) => {
                if tree_kind.is_some_and(|tk| tk != base_snap.kind) {
                    remove_single_entry_at(
                        TreeTag::Tree,
                        self.tree_root_fd.as_fd(),
                        rel,
                        &mut st.widened,
                    )?;
                    remove_tree_index_entry(&mut st.tree_index, &mut st.shadow_inodes, rel);
                }
                match base_snap.kind {
                    EntryKind::File => {
                        let (stat, _) = install_file_atomic_at(
                            self.base_root_fd.as_fd(),
                            rel,
                            TreeTag::Tree,
                            self.tree_root_fd.as_fd(),
                            rel,
                            self.tmp_root_fd.as_fd(),
                            &mut st.widened,
                            base_snap.mode,
                            false,
                        )?;
                        let snap = EntrySnapshot {
                            kind: EntryKind::File,
                            mode: base_snap.mode,
                            size: base_snap.size,
                            hash: base_snap.hash,
                            link_target: None,
                            stat,
                        };
                        set_tree_index_entry(&mut st.tree_index, &mut st.shadow_inodes, rel, snap);
                    }
                    EntryKind::Dir => {
                        install_dir_writable_at(
                            TreeTag::Tree,
                            self.tree_root_fd.as_fd(),
                            rel,
                            &mut st.widened,
                            base_snap.mode,
                        )?;
                        set_dir_mode_nofollow(self.tree_root_fd.as_fd(), rel, base_snap.mode)?;
                        st.widened
                            .restore_all(self.tree_root_fd.as_fd(), self.base_root_fd.as_fd());
                        if let Some(ts) = inspect_lstat_at(self.tree_root_fd.as_fd(), rel)? {
                            set_tree_index_entry(
                                &mut st.tree_index,
                                &mut st.shadow_inodes,
                                rel,
                                ts,
                            );
                        }
                    }
                    EntryKind::Symlink => {
                        let target = base_snap.link_target.as_deref().unwrap_or_default();
                        let stat = install_symlink_atomic_at(
                            target,
                            TreeTag::Tree,
                            self.tree_root_fd.as_fd(),
                            rel,
                            self.tmp_root_fd.as_fd(),
                            &mut st.widened,
                        )?;
                        let snap = EntrySnapshot {
                            kind: EntryKind::Symlink,
                            mode: stat.mode,
                            size: stat.size,
                            hash: None,
                            link_target: base_snap.link_target.clone(),
                            stat,
                        };
                        set_tree_index_entry(&mut st.tree_index, &mut st.shadow_inodes, rel, snap);
                    }
                }
            }
        }
        Ok(())
    }

    fn reconcile_shadow_locked(
        &self,
        st: &mut ShadowState,
        deadline: Option<Instant>,
        cancelled: Option<&AtomicBool>,
    ) -> Result<(), ShadowSyncError> {
        if let Some(intr) = check_interrupt(deadline, cancelled) {
            return Err(ShadowSyncError::Interrupted(intr));
        }

        let ev = st.shadow_watch.drain();
        if ev.need_full_rescan || ev.overflowed {
            st.shadow_need_full_rescan = true;
        }

        // 短命硬链接兜底：若收到 MODIFY/CLOSE_WRITE/ATTRIB 的路径（跟随配对改名后）在排空时已从磁盘消失，
        // 且不在树索引中或在本批事件中被新建/移入过（如删除已有别名后重建为另一文件的硬链接），
        // 无法定位其 inode，按溢出触发全量核对。
        if !st.shadow_need_full_rescan && !ev.modified_files.is_empty() {
            for p in &ev.modified_files {
                if let Some(intr) = check_interrupt(deadline, cancelled) {
                    st.shadow_need_full_rescan = true;
                    return Err(ShadowSyncError::Interrupted(intr));
                }
                let final_p = follow_paired_move(&ev.paired_file_moves, p);
                let created_or_moved = ev.created_or_moved_files.contains(p)
                    || ev.created_or_moved_files.contains(&final_p);
                if !st.tree_index.contains_key(&final_p) || created_or_moved {
                    match inspect_lstat_at(self.tree_root_fd.as_fd(), &final_p) {
                        Ok(None) => {
                            st.shadow_need_full_rescan = true;
                            break;
                        }
                        Ok(Some(_)) => {}
                        Err(_) => {
                            st.shadow_need_full_rescan = true;
                            break;
                        }
                    }
                }
            }
        }

        if st.shadow_need_full_rescan {
            return self.full_reconcile_shadow_locked(st, deadline, cancelled);
        }

        let mut candidates = std::mem::take(&mut st.deferred_shadow_candidates);
        candidates.extend(ev.paths);

        let recursive_dirs: Vec<PathBuf> = ev.recursive.into_iter().collect();
        for dir in &recursive_dirs {
            if let Some(intr) = check_interrupt(deadline, cancelled) {
                st.shadow_need_full_rescan = true;
                return Err(ShadowSyncError::Interrupted(intr));
            }
            candidates.insert(dir.clone());
            candidates.extend(descendants_in_map(&st.tree_index, dir));
            candidates.extend(descendants_in_map(&st.base_index, dir));
            candidates.extend(descendants_in_map(&st.pending, dir));
            if let Err(err) = collect_live_shadow_descendants_at(
                self.tree_root_fd.as_fd(),
                &self.rules,
                dir,
                &mut candidates,
                deadline,
                cancelled,
            ) {
                st.shadow_need_full_rescan = true;
                return Err(err);
            }
        }

        if candidates.is_empty() {
            return Ok(());
        }

        // 影子硬链接展开：事件命中已在 inode 索引中的文件时，一并核实同 inode 的全部路径。
        let mut siblings = Vec::new();
        for p in &candidates {
            if let Some(intr) = check_interrupt(deadline, cancelled) {
                st.deferred_shadow_candidates
                    .extend(candidates.iter().cloned());
                return Err(ShadowSyncError::Interrupted(intr));
            }
            if let Some(old) = st.tree_index.get(p)
                && old.kind == EntryKind::File
                && let Some(group) = st.shadow_inodes.get(&(old.stat.dev, old.stat.ino))
            {
                siblings.extend(group.iter().cloned());
            }
            if let Ok(Some(cur)) = inspect_lstat_at(self.tree_root_fd.as_fd(), p)
                && cur.kind == EntryKind::File
                && let Some(group) = st.shadow_inodes.get(&(cur.stat.dev, cur.stat.ino))
            {
                siblings.extend(group.iter().cloned());
            }
        }
        candidates.extend(siblings);

        self.verify_shadow_paths_locked(st, candidates, deadline, cancelled)
    }

    fn verify_shadow_paths_locked(
        &self,
        st: &mut ShadowState,
        mut remaining: BTreeSet<PathBuf>,
        deadline: Option<Instant>,
        cancelled: Option<&AtomicBool>,
    ) -> Result<(), ShadowSyncError> {
        while let Some(p) = remaining.pop_first() {
            if let Some(intr) = check_interrupt(deadline, cancelled) {
                remaining.insert(p);
                st.deferred_shadow_candidates.extend(remaining);
                return Err(ShadowSyncError::Interrupted(intr));
            }
            if self.rules.is_reserved_git(&p) {
                continue;
            }
            let mut tree_opt = match inspect_with_hash_at(self.tree_root_fd.as_fd(), &p) {
                Ok(opt) => opt,
                Err(err) => {
                    let msg = format!("读取影子路径 `{}` 失败：{err}", p.display());
                    remaining.insert(p);
                    st.deferred_shadow_candidates.extend(remaining);
                    return Err(ShadowSyncError::Failed(msg));
                }
            };
            if let Some(ref snap) = tree_opt {
                let is_dir = snap.kind == EntryKind::Dir;
                if self.rules.is_regenerable(&p, is_dir) && !st.base_index.contains_key(&p) {
                    continue;
                }
            }

            let tree_hidden = tree_opt
                .as_ref()
                .is_some_and(|s| self.rules.is_hidden(&p, s.kind == EntryKind::Dir));

            let equal = if tree_hidden {
                false
            } else {
                match entries_semantically_equal_at(
                    st.base_index.get_mut(&p),
                    self.base_root_fd.as_fd(),
                    tree_opt.as_mut(),
                    self.tree_root_fd.as_fd(),
                    &p,
                ) {
                    Ok(eq) => eq,
                    Err(err) => {
                        let msg = format!("核对影子路径 `{}` 失败：{err}", p.display());
                        remaining.insert(p);
                        st.deferred_shadow_candidates.extend(remaining);
                        return Err(ShadowSyncError::Failed(msg));
                    }
                }
            };

            match tree_opt.clone() {
                Some(snap) => {
                    set_tree_index_entry(&mut st.tree_index, &mut st.shadow_inodes, &p, snap);
                }
                None => {
                    remove_tree_index_entry(&mut st.tree_index, &mut st.shadow_inodes, &p);
                }
            }

            if tree_hidden {
                if st.base_index.contains_key(&p) {
                    let prev_conflict = st.pending.get(&p).is_some_and(|e| e.conflict);
                    st.pending.insert(
                        p,
                        PendingEntry {
                            action: PendingAction::Deleted,
                            conflict: prev_conflict,
                            hidden: false,
                            tree_hidden: true,
                        },
                    );
                } else {
                    st.pending.insert(
                        p,
                        PendingEntry {
                            action: PendingAction::Added,
                            conflict: false,
                            hidden: true,
                            tree_hidden: true,
                        },
                    );
                }
                continue;
            }

            if equal {
                st.pending.remove(&p);
                st.host_pending_obs.remove(&p);
            } else {
                let action = match (st.base_index.contains_key(&p), tree_opt.is_some()) {
                    (false, true) => PendingAction::Added,
                    (true, false) => PendingAction::Deleted,
                    (true, true) => PendingAction::Modified,
                    (false, false) => continue,
                };
                let prev_conflict = st.pending.get(&p).is_some_and(|e| e.conflict);
                st.pending.insert(
                    p,
                    PendingEntry {
                        action,
                        conflict: prev_conflict,
                        hidden: false,
                        tree_hidden: false,
                    },
                );
            }
        }
        Ok(())
    }

    fn full_reconcile_shadow_locked(
        &self,
        st: &mut ShadowState,
        deadline: Option<Instant>,
        cancelled: Option<&AtomicBool>,
    ) -> Result<(), ShadowSyncError> {
        // 先重建监听再全量扫描，避免重建窗口内遗漏新事件；仅在全量核对完整成功后才替换索引与待定集合。
        st.shadow_watch.rebuild();

        let (mut new_tree_index, new_shadow_inodes) = scan_tree_lstat_at(
            self.tree_root_fd.as_fd(),
            &self.rules,
            false,
            deadline,
            cancelled,
        )?;
        let mut new_pending = BTreeMap::new();

        let all_keys: BTreeSet<PathBuf> = st
            .base_index
            .keys()
            .chain(new_tree_index.keys())
            .cloned()
            .collect();

        for p in all_keys {
            if let Some(intr) = check_interrupt(deadline, cancelled) {
                return Err(ShadowSyncError::Interrupted(intr));
            }
            if self.rules.is_reserved_git(&p) {
                continue;
            }
            let tree_hidden = new_tree_index
                .get(&p)
                .is_some_and(|s| self.rules.is_hidden(&p, s.kind == EntryKind::Dir));
            if tree_hidden {
                if st.base_index.contains_key(&p) {
                    let prev_conflict = st.pending.get(&p).is_some_and(|e| e.conflict);
                    new_pending.insert(
                        p,
                        PendingEntry {
                            action: PendingAction::Deleted,
                            conflict: prev_conflict,
                            hidden: false,
                            tree_hidden: true,
                        },
                    );
                } else {
                    new_pending.insert(
                        p,
                        PendingEntry {
                            action: PendingAction::Added,
                            conflict: false,
                            hidden: true,
                            tree_hidden: true,
                        },
                    );
                }
                continue;
            }

            let equal = entries_semantically_equal_at(
                st.base_index.get_mut(&p),
                self.base_root_fd.as_fd(),
                new_tree_index.get_mut(&p),
                self.tree_root_fd.as_fd(),
                &p,
            )
            .map_err(|err| {
                ShadowSyncError::Failed(format!("全量核对影子路径 `{}` 失败：{err}", p.display()))
            })?;
            if !equal {
                let action = match (
                    st.base_index.contains_key(&p),
                    new_tree_index.contains_key(&p),
                ) {
                    (false, true) => PendingAction::Added,
                    (true, false) => PendingAction::Deleted,
                    (true, true) => PendingAction::Modified,
                    (false, false) => continue,
                };
                let prev_conflict = st.pending.get(&p).is_some_and(|e| e.conflict);
                new_pending.insert(
                    p,
                    PendingEntry {
                        action,
                        conflict: prev_conflict,
                        hidden: false,
                        tree_hidden: false,
                    },
                );
            }
        }

        st.tree_index = new_tree_index;
        st.shadow_inodes = new_shadow_inodes;
        st.pending = new_pending;
        st.deferred_shadow_candidates.clear();
        st.shadow_need_full_rescan = false;
        st.host_pending_obs
            .retain(|k, _| st.pending.contains_key(k));
        Ok(())
    }
}

/// 计算可写工作目录的影子绑定键：`SHA-256(规范化宿主根路径, hidden 规则, regenerable 规则)`。
pub fn compute_shadow_key(
    canonical_host_root: &Path,
    hidden: &[String],
    regenerable: &[String],
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"lancet-shadow-v1\0");

    let root_bytes = canonical_host_root.as_os_str().as_bytes();
    hasher.update((root_bytes.len() as u64).to_le_bytes());
    hasher.update(root_bytes);

    hasher.update((hidden.len() as u64).to_le_bytes());
    for pattern in hidden {
        let bytes = pattern.as_bytes();
        hasher.update((bytes.len() as u64).to_le_bytes());
        hasher.update(bytes);
    }

    hasher.update((regenerable.len() as u64).to_le_bytes());
    for pattern in regenerable {
        let bytes = pattern.as_bytes();
        hasher.update((bytes.len() as u64).to_le_bytes());
        hasher.update(bytes);
    }

    let digest = hasher.finalize();
    let mut out = String::with_capacity(64);
    for byte in digest {
        let _ = write!(&mut out, "{byte:02x}");
    }
    out
}

type OpenedShadowState = (
    BTreeMap<PathBuf, EntrySnapshot>,
    BTreeMap<PathBuf, EntrySnapshot>,
    HashMap<(u64, u64), BTreeSet<PathBuf>>,
    BTreeMap<PathBuf, PendingEntry>,
    BTreeSet<PathBuf>,
    DirWatch,
    DirWatch,
    HostScanner,
    bool,
);

struct InitWalkItems {
    regular_dirs: Vec<(PathBuf, u32)>,
    regular_files: Vec<(PathBuf, u32)>,
    regular_symlinks: Vec<(PathBuf, Vec<u8>)>,
    seed_dirs: Vec<(PathBuf, u32)>,
    seed_files: Vec<(PathBuf, u32)>,
    seed_symlinks: Vec<(PathBuf, Vec<u8>)>,
}

#[allow(clippy::too_many_arguments)]
fn create_new_shadow(
    host_root: &Path,
    shadows_root: &Path,
    shadow_root: &Path,
    tree_root: &Path,
    base_root: &Path,
    final_tmp_root: &Path,
    key: &str,
    rules: &PathRules,
) -> anyhow::Result<OpenedShadowState> {
    // 先安装宿主侧监听，再遍历宿主克隆。
    let host_watch = DirWatch::new(host_root.to_path_buf(), rules.clone(), true)?;
    let host_root_fd = open_root_fd(host_root)
        .with_context(|| format!("打开宿主根目录 {} 失败", host_root.display()))?;
    let init_start_ns = now_realtime_ns();

    let staging_root = shadows_root.join(format!(".init-{key}-{}", std::process::id()));
    remove_tree_force(&staging_root)?;
    ensure_dir_0700(&staging_root)?;

    let init_res = (|| -> anyhow::Result<OpenedShadowState> {
        let tmp_base = staging_root.join("base");
        let tmp_tree = staging_root.join("tree");
        let tmp_scratch = staging_root.join("tmp");
        ensure_dir_0700(&tmp_base)?;
        ensure_dir_0700(&tmp_tree)?;
        ensure_dir_0700(&tmp_scratch)?;

        let mut items = InitWalkItems {
            regular_dirs: Vec::new(),
            regular_files: Vec::new(),
            regular_symlinks: Vec::new(),
            seed_dirs: Vec::new(),
            seed_files: Vec::new(),
            seed_symlinks: Vec::new(),
        };
        walk_host_for_init_at(
            host_root_fd.as_fd(),
            rules,
            Path::new(""),
            false,
            &mut items,
        )?;

        items
            .regular_dirs
            .sort_by(|(a, _), (b, _)| path_depth(a).cmp(&path_depth(b)).then_with(|| a.cmp(b)));
        items
            .seed_dirs
            .sort_by(|(a, _), (b, _)| path_depth(a).cmp(&path_depth(b)).then_with(|| a.cmp(b)));

        for (rel, mode) in &items.regular_dirs {
            let writable = (*mode & 0o7777) | 0o700;
            DirBuilder::new()
                .mode(writable)
                .create(tmp_base.join(rel))
                .with_context(|| format!("创建基准目录 `{}` 失败", rel.display()))?;
            DirBuilder::new()
                .mode(writable)
                .create(tmp_tree.join(rel))
                .with_context(|| format!("创建影子目录 `{}` 失败", rel.display()))?;
        }
        for (rel, mode) in &items.seed_dirs {
            let writable = (*mode & 0o7777) | 0o700;
            DirBuilder::new()
                .mode(writable)
                .create(tmp_tree.join(rel))
                .with_context(|| format!("创建种子目录 `{}` 失败", rel.display()))?;
        }

        // 用固定 8 个工作线程并行 FICLONE 普通文件与种子文件。
        enum FileJob<'a> {
            Regular(&'a Path, u32),
            Seed(&'a Path, u32),
        }
        let mut jobs = Vec::with_capacity(items.regular_files.len() + items.seed_files.len());
        for (rel, mode) in &items.regular_files {
            jobs.push(FileJob::Regular(rel.as_path(), *mode));
        }
        for (rel, mode) in &items.seed_files {
            jobs.push(FileJob::Seed(rel.as_path(), *mode));
        }

        type ClonedRecord = (PathBuf, u32, StatSig, StatSig, StatSig);
        let next_idx = AtomicUsize::new(0);
        let worker_count = INIT_CLONE_WORKERS.min(jobs.len().max(1));
        let host_fd_ref = host_root_fd.as_fd();
        let worker_outputs: Vec<Vec<ClonedRecord>> = thread::scope(|scope| {
            let mut handles = Vec::with_capacity(worker_count);
            for _ in 0..worker_count {
                let jobs = &jobs;
                let next_idx = &next_idx;
                let tmp_base = &tmp_base;
                let tmp_tree = &tmp_tree;
                handles.push(scope.spawn(move || -> anyhow::Result<Vec<ClonedRecord>> {
                    let mut local = Vec::new();
                    loop {
                        let i = next_idx.fetch_add(1, Ordering::Relaxed);
                        if i >= jobs.len() {
                            break;
                        }
                        match jobs[i] {
                            FileJob::Regular(rel, mode) => {
                                match clone_init_regular_file_at(
                                    host_fd_ref,
                                    rel,
                                    Some(&tmp_base.join(rel)),
                                    &tmp_tree.join(rel),
                                    mode,
                                ) {
                                    Ok((host_st, Some(base_st), tree_st)) => {
                                        local.push((
                                            rel.to_path_buf(),
                                            mode,
                                            host_st,
                                            base_st,
                                            tree_st,
                                        ));
                                    }
                                    Ok(_) => {}
                                    Err(err) if err.kind() == io::ErrorKind::NotFound => {}
                                    Err(err) => {
                                        return Err(err).with_context(|| {
                                            format!("克隆常规文件 `{}` 到影子失败", rel.display())
                                        });
                                    }
                                }
                            }
                            FileJob::Seed(rel, mode) => {
                                match clone_init_regular_file_at(
                                    host_fd_ref,
                                    rel,
                                    None,
                                    &tmp_tree.join(rel),
                                    mode,
                                ) {
                                    Ok(_) => {}
                                    Err(err) if err.kind() == io::ErrorKind::NotFound => {}
                                    Err(err) => {
                                        return Err(err).with_context(|| {
                                            format!("克隆种子文件 `{}` 到影子失败", rel.display())
                                        });
                                    }
                                }
                            }
                        }
                    }
                    Ok(local)
                }));
            }
            handles
                .into_iter()
                .map(|h| h.join().expect("初始化克隆工作线程不应 panic"))
                .collect::<anyhow::Result<Vec<_>>>()
        })?;

        let mut base_index = BTreeMap::new();
        let mut tree_index = BTreeMap::new();
        let mut shadow_inodes: HashMap<(u64, u64), BTreeSet<PathBuf>> = HashMap::new();
        let mut host_stats = BTreeMap::new();

        for batch in worker_outputs {
            for (rel, mode, host_st, base_st, tree_st) in batch {
                let hash = if base_st.size == 0 {
                    Some(EMPTY_SHA256)
                } else {
                    None
                };
                base_index.insert(
                    rel.clone(),
                    EntrySnapshot {
                        kind: EntryKind::File,
                        mode: mode & 0o7777,
                        size: base_st.size,
                        hash,
                        link_target: None,
                        stat: base_st,
                    },
                );
                set_tree_index_entry(
                    &mut tree_index,
                    &mut shadow_inodes,
                    &rel,
                    EntrySnapshot {
                        kind: EntryKind::File,
                        mode: mode & 0o7777,
                        size: tree_st.size,
                        hash,
                        link_target: None,
                        stat: tree_st,
                    },
                );
                host_stats.insert(
                    rel,
                    EntrySnapshot {
                        kind: EntryKind::File,
                        mode: host_st.mode,
                        size: host_st.size,
                        hash,
                        link_target: None,
                        stat: host_st,
                    },
                );
            }
        }

        for (rel, target) in &items.regular_symlinks {
            let base_st = copy_init_symlink(target, &tmp_base.join(rel))
                .with_context(|| format!("复制基准符号链接 `{}` 失败", rel.display()))?;
            let tree_st = copy_init_symlink(target, &tmp_tree.join(rel))
                .with_context(|| format!("复制影子符号链接 `{}` 失败", rel.display()))?;
            let host_st = inspect_lstat_at(host_root_fd.as_fd(), rel)
                .ok()
                .flatten()
                .map_or(base_st, |s| s.stat);

            base_index.insert(
                rel.clone(),
                EntrySnapshot {
                    kind: EntryKind::Symlink,
                    mode: base_st.mode,
                    size: base_st.size,
                    hash: None,
                    link_target: Some(target.clone()),
                    stat: base_st,
                },
            );
            set_tree_index_entry(
                &mut tree_index,
                &mut shadow_inodes,
                rel,
                EntrySnapshot {
                    kind: EntryKind::Symlink,
                    mode: tree_st.mode,
                    size: tree_st.size,
                    hash: None,
                    link_target: Some(target.clone()),
                    stat: tree_st,
                },
            );
            host_stats.insert(
                rel.clone(),
                EntrySnapshot {
                    kind: EntryKind::Symlink,
                    mode: host_st.mode,
                    size: host_st.size,
                    hash: None,
                    link_target: Some(target.clone()),
                    stat: host_st,
                },
            );
        }
        for (rel, target) in &items.seed_symlinks {
            copy_init_symlink(target, &tmp_tree.join(rel))
                .with_context(|| format!("复制种子符号链接 `{}` 失败", rel.display()))?;
        }

        // 最后由深到浅恢复目录权限。
        for (rel, mode) in items.regular_dirs.iter().rev() {
            set_init_mode(&tmp_base.join(rel), *mode)?;
            set_init_mode(&tmp_tree.join(rel), *mode)?;
            let base_st = StatSig::from_metadata(&fs::symlink_metadata(tmp_base.join(rel))?);
            let tree_st = StatSig::from_metadata(&fs::symlink_metadata(tmp_tree.join(rel))?);
            let host_st = inspect_lstat_at(host_root_fd.as_fd(), rel)
                .ok()
                .flatten()
                .map_or(base_st, |s| s.stat);

            base_index.insert(
                rel.clone(),
                EntrySnapshot {
                    kind: EntryKind::Dir,
                    mode: *mode & 0o7777,
                    size: base_st.size,
                    hash: None,
                    link_target: None,
                    stat: base_st,
                },
            );
            set_tree_index_entry(
                &mut tree_index,
                &mut shadow_inodes,
                rel,
                EntrySnapshot {
                    kind: EntryKind::Dir,
                    mode: *mode & 0o7777,
                    size: tree_st.size,
                    hash: None,
                    link_target: None,
                    stat: tree_st,
                },
            );
            host_stats.insert(
                rel.clone(),
                EntrySnapshot {
                    kind: EntryKind::Dir,
                    mode: host_st.mode,
                    size: host_st.size,
                    hash: None,
                    link_target: None,
                    stat: host_st,
                },
            );
        }
        for (rel, mode) in items.seed_dirs.iter().rev() {
            set_init_mode(&tmp_tree.join(rel), *mode)?;
        }

        // 整体原子改名就位。
        fs::rename(&staging_root, shadow_root).with_context(|| {
            format!(
                "将临时影子目录 {} 改名就位到 {} 失败",
                staging_root.display(),
                shadow_root.display()
            )
        })?;
        sync_dir_path(shadows_root)?;

        let shadow_watch = DirWatch::new(tree_root.to_path_buf(), rules.clone(), false)?;
        let initial_baseline: BTreeMap<PathBuf, BaselineMeta> = base_index
            .iter()
            .map(|(k, v)| (k.clone(), BaselineMeta::from_snapshot(v)))
            .collect();
        let scanner = HostScanner::spawn(
            host_root.to_path_buf(),
            base_root.to_path_buf(),
            final_tmp_root.to_path_buf(),
            rules.clone(),
            initial_baseline,
            host_stats,
            init_start_ns,
        )?;

        Ok((
            base_index,
            tree_index,
            shadow_inodes,
            BTreeMap::new(),
            BTreeSet::new(),
            host_watch,
            shadow_watch,
            scanner,
            false,
        ))
    })();

    if init_res.is_err() {
        let _ = remove_tree_force(&staging_root);
    }
    init_res
}

fn reopen_existing_shadow(
    host_root: &Path,
    shadow_root: &Path,
    tree_root: &Path,
    base_root: &Path,
    tmp_root: &Path,
    rules: &PathRules,
) -> anyhow::Result<OpenedShadowState> {
    if !tree_root.is_dir() || !base_root.is_dir() {
        anyhow::bail!(
            "影子目录 {} 结构不完整（缺少 tree/ 或 base/）",
            shadow_root.display()
        );
    }

    // 崩溃恢复：在重建索引之前先清空 `shadows/<key>/tmp/`，并按 `shadows/<key>/widened` 恢复未完成的目录权限放宽。
    clear_or_create_tmp_dir(tmp_root)
        .with_context(|| format!("清空影子临时目录 {} 失败", tmp_root.display()))?;

    let host_root_fd = open_root_fd(host_root)
        .with_context(|| format!("打开宿主根目录 {} 失败", host_root.display()))?;
    let tree_root_fd = open_root_fd(tree_root)
        .with_context(|| format!("打开影子 tree 目录 {} 失败", tree_root.display()))?;
    let base_root_fd = open_root_fd(base_root)
        .with_context(|| format!("打开影子 base 目录 {} 失败", base_root.display()))?;
    let tmp_root_fd = open_root_fd(tmp_root)
        .with_context(|| format!("打开影子 tmp 目录 {} 失败", tmp_root.display()))?;

    recover_widened_modes(shadow_root, tree_root_fd.as_fd(), base_root_fd.as_fd())
        .with_context(|| format!("恢复影子 {} 权限放宽记录失败", shadow_root.display()))?;

    let host_watch = DirWatch::new(host_root.to_path_buf(), rules.clone(), true)?;
    let shadow_watch = DirWatch::new(tree_root.to_path_buf(), rules.clone(), false)?;
    let reopen_start_ns = now_realtime_ns();

    let (mut base_index, _) = scan_tree_lstat_at(base_root_fd.as_fd(), rules, true, None, None)
        .map_err(|err| anyhow::anyhow!("扫描基准目录失败：{err}"))?;
    let (mut tree_index, shadow_inodes) =
        scan_tree_lstat_at(tree_root_fd.as_fd(), rules, false, None, None)
            .map_err(|err| anyhow::anyhow!("扫描影子目录失败：{err}"))?;
    let (mut host_stats, _) = scan_tree_lstat_at(host_root_fd.as_fd(), rules, true, None, None)
        .map_err(|err| anyhow::anyhow!("扫描宿主目录失败：{err}"))?;

    // 重启重建宿主观测时，对非空普通文件经 FICLONE 求摘要，以重新武装跨重启持有的 MAP_SHARED 写保护。
    let mut deferred_host_candidates = BTreeSet::new();
    let mut host_to_remove = Vec::new();
    for (p, snap) in &mut host_stats {
        if snap.kind == EntryKind::File && snap.size > 0 {
            match hash_host_file_via_ficlone_at(host_root_fd.as_fd(), p, tmp_root_fd.as_fd()) {
                Ok(Some((post_stat, h))) => {
                    snap.stat = post_stat;
                    snap.mode = post_stat.mode;
                    snap.size = post_stat.size;
                    snap.hash = Some(h);
                }
                Ok(None) => {
                    host_to_remove.push(p.clone());
                }
                Err(_) => {
                    host_to_remove.push(p.clone());
                    deferred_host_candidates.insert(p.clone());
                }
            }
        }
    }
    for p in host_to_remove {
        host_stats.remove(&p);
    }

    // 核对 base 与 tree，重建待定集合（按各自树的实际类型分类）。
    let mut pending = BTreeMap::new();
    let bt_keys: Vec<PathBuf> = base_index
        .keys()
        .chain(tree_index.keys())
        .cloned()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();

    for p in &bt_keys {
        let tree_hidden = tree_index
            .get(p)
            .is_some_and(|ts| rules.is_hidden(p, ts.kind == EntryKind::Dir));
        if tree_hidden {
            if base_index.contains_key(p) {
                pending.insert(
                    p.clone(),
                    PendingEntry {
                        action: PendingAction::Deleted,
                        conflict: false,
                        hidden: false,
                        tree_hidden: true,
                    },
                );
            } else {
                pending.insert(
                    p.clone(),
                    PendingEntry {
                        action: PendingAction::Added,
                        conflict: false,
                        hidden: true,
                        tree_hidden: true,
                    },
                );
            }
            continue;
        }
        let equal = entries_semantically_equal_at(
            base_index.get_mut(p),
            base_root_fd.as_fd(),
            tree_index.get_mut(p),
            tree_root_fd.as_fd(),
            p,
        )?;
        if !equal {
            let action = match (base_index.contains_key(p), tree_index.contains_key(p)) {
                (false, true) => PendingAction::Added,
                (true, false) => PendingAction::Deleted,
                (true, true) => PendingAction::Modified,
                (false, false) => continue,
            };
            pending.insert(
                p.clone(),
                PendingEntry {
                    action,
                    conflict: false,
                    hidden: false,
                    tree_hidden: false,
                },
            );
        }
    }

    // 对照宿主与基准，把不同项放入待同步候选，随后由 sync_from_host 接纳或标为冲突。
    let hb_keys: Vec<PathBuf> = host_stats
        .keys()
        .chain(base_index.keys())
        .cloned()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    for p in &hb_keys {
        let equal = entries_semantically_equal_at(
            base_index.get_mut(p),
            base_root_fd.as_fd(),
            host_stats.get_mut(p),
            host_root_fd.as_fd(),
            p,
        )?;
        if !equal {
            deferred_host_candidates.insert(p.clone());
        }
    }

    let initial_baseline: BTreeMap<PathBuf, BaselineMeta> = base_index
        .iter()
        .map(|(k, v)| (k.clone(), BaselineMeta::from_snapshot(v)))
        .collect();
    let scanner = HostScanner::spawn(
        host_root.to_path_buf(),
        base_root.to_path_buf(),
        tmp_root.to_path_buf(),
        rules.clone(),
        initial_baseline,
        host_stats,
        reopen_start_ns,
    )?;

    Ok((
        base_index,
        tree_index,
        shadow_inodes,
        pending,
        deferred_host_candidates,
        host_watch,
        shadow_watch,
        scanner,
        true,
    ))
}

fn walk_host_for_init_at(
    host_root_fd: BorrowedFd<'_>,
    rules: &PathRules,
    rel_dir: &Path,
    in_seed: bool,
    items: &mut InitWalkItems,
) -> anyhow::Result<()> {
    let Some(read_fd) = open_dir_nofollow(host_root_fd, rel_dir)
        .with_context(|| format!("读取宿主目录 `{}` 失败", rel_dir.display()))?
    else {
        return Ok(());
    };
    let stat_fd = rustix::io::dup(&read_fd)
        .map_err(io::Error::from)
        .with_context(|| format!("复制宿主目录 `{}` fd 失败", rel_dir.display()))?;
    let mut dir = Dir::new(read_fd)
        .map_err(io::Error::from)
        .with_context(|| format!("遍历宿主目录 `{}` 失败", rel_dir.display()))?;

    let mut subdirs = Vec::new();
    while let Some(next) = dir.read() {
        let entry = next
            .map_err(io::Error::from)
            .with_context(|| format!("读取宿主目录 `{}` 子项失败", rel_dir.display()))?;
        let bytes = entry.file_name().to_bytes();
        if bytes == b"." || bytes == b".." {
            continue;
        }
        let name_os = OsStr::from_bytes(bytes);
        let rel = join_rel(rel_dir, name_os);
        if rules.is_reserved_git(&rel) {
            continue;
        }
        let st = match statat(&stat_fd, name_os, AtFlags::SYMLINK_NOFOLLOW) {
            Ok(st) => st,
            Err(rustix::io::Errno::NOENT) => continue,
            Err(err) => {
                return Err(io::Error::from(err))
                    .with_context(|| format!("读取宿主条目 `{}` 属性失败", rel.display()));
            }
        };
        let Some(kind) = classify_stat(&st) else {
            continue;
        };
        let is_dir = kind == EntryKind::Dir;
        if rules.is_hidden(&rel, is_dir) {
            continue;
        }
        let is_seed = in_seed || rules.is_regenerable(&rel, is_dir);
        let mode = st.st_mode & 0o7777;

        match kind {
            EntryKind::Dir => {
                if is_seed {
                    items.seed_dirs.push((rel.clone(), mode));
                } else {
                    items.regular_dirs.push((rel.clone(), mode));
                }
                subdirs.push((rel, is_seed));
            }
            EntryKind::File => {
                if is_seed {
                    items.seed_files.push((rel, mode));
                } else {
                    items.regular_files.push((rel, mode));
                }
            }
            EntryKind::Symlink => {
                let target = readlinkat(&stat_fd, name_os, Vec::new())
                    .map_err(io::Error::from)
                    .with_context(|| format!("读取符号链接 `{}` 失败", rel.display()))?
                    .into_bytes();
                if is_seed {
                    items.seed_symlinks.push((rel, target));
                } else {
                    items.regular_symlinks.push((rel, target));
                }
            }
        }
    }
    drop(dir);
    drop(stat_fd);

    for (sub_rel, sub_seed) in subdirs {
        walk_host_for_init_at(host_root_fd, rules, &sub_rel, sub_seed, items)?;
    }
    Ok(())
}

type TreeLstatScan = (
    BTreeMap<PathBuf, EntrySnapshot>,
    HashMap<(u64, u64), BTreeSet<PathBuf>>,
);

fn scan_tree_lstat_at(
    root_fd: BorrowedFd<'_>,
    rules: &PathRules,
    skip_hidden: bool,
    deadline: Option<Instant>,
    cancelled: Option<&AtomicBool>,
) -> Result<TreeLstatScan, ShadowSyncError> {
    let mut entries = BTreeMap::new();
    let mut inodes: HashMap<(u64, u64), BTreeSet<PathBuf>> = HashMap::new();
    walk_lstat_rec_at(
        root_fd,
        rules,
        skip_hidden,
        Path::new(""),
        deadline,
        cancelled,
        &mut entries,
        &mut inodes,
    )?;
    Ok((entries, inodes))
}

#[allow(clippy::too_many_arguments)]
fn walk_lstat_rec_at(
    root_fd: BorrowedFd<'_>,
    rules: &PathRules,
    skip_hidden: bool,
    rel_dir: &Path,
    deadline: Option<Instant>,
    cancelled: Option<&AtomicBool>,
    entries: &mut BTreeMap<PathBuf, EntrySnapshot>,
    inodes: &mut HashMap<(u64, u64), BTreeSet<PathBuf>>,
) -> Result<(), ShadowSyncError> {
    if let Some(intr) = check_interrupt(deadline, cancelled) {
        return Err(ShadowSyncError::Interrupted(intr));
    }
    let Some(read_fd) = open_dir_nofollow(root_fd, rel_dir).map_err(|err| {
        ShadowSyncError::Failed(format!("打开目录 `{}` 失败：{err}", rel_dir.display()))
    })?
    else {
        return Ok(());
    };
    let stat_fd = rustix::io::dup(&read_fd)
        .map_err(io::Error::from)
        .map_err(|err| {
            ShadowSyncError::Failed(format!("复制目录 `{}` fd 失败：{err}", rel_dir.display()))
        })?;
    let mut dir = Dir::new(read_fd).map_err(io::Error::from).map_err(|err| {
        ShadowSyncError::Failed(format!("遍历目录 `{}` 失败：{err}", rel_dir.display()))
    })?;

    let mut subdirs = Vec::new();
    while let Some(next) = dir.read() {
        if let Some(intr) = check_interrupt(deadline, cancelled) {
            return Err(ShadowSyncError::Interrupted(intr));
        }
        let entry = next.map_err(io::Error::from).map_err(|err| {
            ShadowSyncError::Failed(format!("读取目录 `{}` 子项失败：{err}", rel_dir.display()))
        })?;
        let bytes = entry.file_name().to_bytes();
        if bytes == b"." || bytes == b".." {
            continue;
        }
        let name_os = OsStr::from_bytes(bytes);
        let rel = join_rel(rel_dir, name_os);
        if rules.is_reserved_git(&rel) {
            continue;
        }
        let st = match statat(&stat_fd, name_os, AtFlags::SYMLINK_NOFOLLOW) {
            Ok(st) => st,
            Err(rustix::io::Errno::NOENT) => continue,
            Err(err) => {
                return Err(ShadowSyncError::Failed(format!(
                    "读取路径 `{}` 属性失败：{}",
                    rel.display(),
                    io::Error::from(err)
                )));
            }
        };
        let Some(kind) = classify_stat(&st) else {
            continue;
        };
        let is_dir = kind == EntryKind::Dir;
        if skip_hidden && rules.is_hidden(&rel, is_dir) {
            continue;
        }
        if rules.is_regenerable(&rel, is_dir) {
            continue;
        }
        let stat = StatSig::from_stat(&st);
        let link_target = if kind == EntryKind::Symlink {
            let target = readlinkat(&stat_fd, name_os, Vec::new())
                .map_err(io::Error::from)
                .map_err(|err| {
                    ShadowSyncError::Failed(format!("读取符号链接 `{}` 失败：{err}", rel.display()))
                })?;
            Some(target.into_bytes())
        } else {
            None
        };
        let hash = if kind == EntryKind::File && stat.size == 0 {
            Some(EMPTY_SHA256)
        } else {
            None
        };
        if kind == EntryKind::File {
            inodes
                .entry((stat.dev, stat.ino))
                .or_default()
                .insert(rel.clone());
        }
        entries.insert(
            rel.clone(),
            EntrySnapshot {
                kind,
                mode: stat.mode,
                size: stat.size,
                hash,
                link_target,
                stat,
            },
        );
        if is_dir {
            subdirs.push(rel);
        }
    }
    drop(dir);
    drop(stat_fd);

    for sub in subdirs {
        walk_lstat_rec_at(
            root_fd,
            rules,
            skip_hidden,
            &sub,
            deadline,
            cancelled,
            entries,
            inodes,
        )?;
    }
    Ok(())
}

fn collect_live_host_descendants_at(
    host_root_fd: BorrowedFd<'_>,
    rules: &PathRules,
    rel_dir: &Path,
    out: &mut BTreeSet<PathBuf>,
    deadline: Option<Instant>,
    cancelled: Option<&AtomicBool>,
) -> Result<(), ShadowSyncError> {
    if let Some(intr) = check_interrupt(deadline, cancelled) {
        return Err(ShadowSyncError::Interrupted(intr));
    }
    let Some(read_fd) = open_dir_nofollow(host_root_fd, rel_dir).map_err(|err| {
        ShadowSyncError::Failed(format!("遍历宿主目录 `{}` 失败：{err}", rel_dir.display()))
    })?
    else {
        return Ok(());
    };
    let stat_fd = rustix::io::dup(&read_fd)
        .map_err(io::Error::from)
        .map_err(|err| {
            ShadowSyncError::Failed(format!(
                "复制宿主目录 `{}` fd 失败：{err}",
                rel_dir.display()
            ))
        })?;
    let mut dir = Dir::new(read_fd).map_err(io::Error::from).map_err(|err| {
        ShadowSyncError::Failed(format!("遍历宿主目录 `{}` 失败：{err}", rel_dir.display()))
    })?;

    let mut subdirs = Vec::new();
    while let Some(next) = dir.read() {
        if let Some(intr) = check_interrupt(deadline, cancelled) {
            return Err(ShadowSyncError::Interrupted(intr));
        }
        let entry = next.map_err(io::Error::from).map_err(|err| {
            ShadowSyncError::Failed(format!(
                "读取宿主目录 `{}` 子项失败：{err}",
                rel_dir.display()
            ))
        })?;
        let bytes = entry.file_name().to_bytes();
        if bytes == b"." || bytes == b".." {
            continue;
        }
        let name_os = OsStr::from_bytes(bytes);
        let rel = join_rel(rel_dir, name_os);
        if rules.is_reserved_git(&rel) {
            continue;
        }
        let st = match statat(&stat_fd, name_os, AtFlags::SYMLINK_NOFOLLOW) {
            Ok(st) => st,
            Err(rustix::io::Errno::NOENT) => continue,
            Err(err) => {
                return Err(ShadowSyncError::Failed(format!(
                    "读取宿主条目 `{}` 属性失败：{}",
                    rel.display(),
                    io::Error::from(err)
                )));
            }
        };
        let Some(kind) = classify_stat(&st) else {
            continue;
        };
        let is_dir = kind == EntryKind::Dir;
        if rules.is_hidden(&rel, is_dir) || rules.is_regenerable(&rel, is_dir) {
            continue;
        }
        out.insert(rel.clone());
        if is_dir {
            subdirs.push(rel);
        }
    }
    drop(dir);
    drop(stat_fd);

    for sub in subdirs {
        collect_live_host_descendants_at(host_root_fd, rules, &sub, out, deadline, cancelled)?;
    }
    Ok(())
}

fn collect_live_shadow_descendants_at(
    tree_root_fd: BorrowedFd<'_>,
    rules: &PathRules,
    rel_dir: &Path,
    out: &mut BTreeSet<PathBuf>,
    deadline: Option<Instant>,
    cancelled: Option<&AtomicBool>,
) -> Result<(), ShadowSyncError> {
    if let Some(intr) = check_interrupt(deadline, cancelled) {
        return Err(ShadowSyncError::Interrupted(intr));
    }
    let Some(read_fd) = open_dir_nofollow(tree_root_fd, rel_dir).map_err(|err| {
        ShadowSyncError::Failed(format!("遍历影子目录 `{}` 失败：{err}", rel_dir.display()))
    })?
    else {
        return Ok(());
    };
    let stat_fd = rustix::io::dup(&read_fd)
        .map_err(io::Error::from)
        .map_err(|err| {
            ShadowSyncError::Failed(format!(
                "复制影子目录 `{}` fd 失败：{err}",
                rel_dir.display()
            ))
        })?;
    let mut dir = Dir::new(read_fd).map_err(io::Error::from).map_err(|err| {
        ShadowSyncError::Failed(format!("遍历影子目录 `{}` 失败：{err}", rel_dir.display()))
    })?;

    let mut subdirs = Vec::new();
    while let Some(next) = dir.read() {
        if let Some(intr) = check_interrupt(deadline, cancelled) {
            return Err(ShadowSyncError::Interrupted(intr));
        }
        let entry = next.map_err(io::Error::from).map_err(|err| {
            ShadowSyncError::Failed(format!(
                "读取影子目录 `{}` 子项失败：{err}",
                rel_dir.display()
            ))
        })?;
        let bytes = entry.file_name().to_bytes();
        if bytes == b"." || bytes == b".." {
            continue;
        }
        let name_os = OsStr::from_bytes(bytes);
        let rel = join_rel(rel_dir, name_os);
        if rules.is_reserved_git(&rel) {
            continue;
        }
        let st = match statat(&stat_fd, name_os, AtFlags::SYMLINK_NOFOLLOW) {
            Ok(st) => st,
            Err(rustix::io::Errno::NOENT) => continue,
            Err(err) => {
                return Err(ShadowSyncError::Failed(format!(
                    "读取影子条目 `{}` 属性失败：{}",
                    rel.display(),
                    io::Error::from(err)
                )));
            }
        };
        let Some(kind) = classify_stat(&st) else {
            continue;
        };
        let is_dir = kind == EntryKind::Dir;
        if rules.is_regenerable(&rel, is_dir) {
            continue;
        }
        out.insert(rel.clone());
        if is_dir {
            subdirs.push(rel);
        }
    }
    drop(dir);
    drop(stat_fd);

    for sub in subdirs {
        collect_live_shadow_descendants_at(tree_root_fd, rules, &sub, out, deadline, cancelled)?;
    }
    Ok(())
}

fn follow_paired_move(moves: &HashMap<PathBuf, PathBuf>, start: &Path) -> PathBuf {
    let mut cur = start.to_path_buf();
    for _ in 0..16 {
        let Some(next) = moves.get(&cur) else {
            break;
        };
        cur = next.clone();
    }
    cur
}

fn discard_effect_target(
    rules: &PathRules,
    rel: &Path,
    entry: &PendingEntry,
    kind: Option<EntryKind>,
) -> String {
    if entry.hidden {
        rules
            .hidden_rule(rel, kind == Some(EntryKind::Dir))
            .unwrap_or("hidden")
            .to_owned()
    } else {
        rel_path_to_utf8(rel).unwrap_or_else(|| rel.to_string_lossy().into_owned())
    }
}

fn push_unique_discard_effect(
    effects: &mut Vec<WriteEffect>,
    recorded_targets: &mut BTreeSet<String>,
    target: String,
) {
    if recorded_targets.insert(target.clone()) {
        effects.push(WriteEffect {
            target,
            action: "discard",
            residue: Residue::None,
        });
    }
}

fn join_rel(base: &Path, name: &OsStr) -> PathBuf {
    if base.as_os_str().is_empty() {
        PathBuf::from(name)
    } else {
        base.join(name)
    }
}

fn set_tree_index_entry(
    tree_index: &mut BTreeMap<PathBuf, EntrySnapshot>,
    shadow_inodes: &mut HashMap<(u64, u64), BTreeSet<PathBuf>>,
    rel: &Path,
    snap: EntrySnapshot,
) {
    remove_tree_index_entry(tree_index, shadow_inodes, rel);
    if snap.kind == EntryKind::File {
        shadow_inodes
            .entry((snap.stat.dev, snap.stat.ino))
            .or_default()
            .insert(rel.to_path_buf());
    }
    tree_index.insert(rel.to_path_buf(), snap);
}

fn remove_tree_index_entry(
    tree_index: &mut BTreeMap<PathBuf, EntrySnapshot>,
    shadow_inodes: &mut HashMap<(u64, u64), BTreeSet<PathBuf>>,
    rel: &Path,
) {
    if let Some(old) = tree_index.remove(rel)
        && old.kind == EntryKind::File
    {
        let key = (old.stat.dev, old.stat.ino);
        if let Some(group) = shadow_inodes.get_mut(&key) {
            group.remove(rel);
            if group.is_empty() {
                shadow_inodes.remove(&key);
            }
        }
    }
}

fn ancestor_blocks_sync(st: &ShadowState, rel: &Path) -> bool {
    let mut cur = rel.parent();
    while let Some(anc) = cur {
        if anc.as_os_str().is_empty() {
            break;
        }
        let tree_is_dir = st
            .tree_index
            .get(anc)
            .is_some_and(|e| e.kind == EntryKind::Dir);
        let base_is_dir = st
            .base_index
            .get(anc)
            .is_some_and(|e| e.kind == EntryKind::Dir);
        if !tree_is_dir || !base_is_dir {
            return true;
        }
        cur = anc.parent();
    }
    false
}

fn check_interrupt(
    deadline: Option<Instant>,
    cancelled: Option<&AtomicBool>,
) -> Option<ShadowInterrupt> {
    if cancelled.is_some_and(|c| c.load(Ordering::Acquire)) {
        return Some(ShadowInterrupt::Cancelled);
    }
    if deadline.is_some_and(|d| Instant::now() >= d) {
        return Some(ShadowInterrupt::Deadline);
    }
    None
}

fn ensure_dir_0700(path: &Path) -> anyhow::Result<()> {
    DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path)
        .with_context(|| format!("无法创建目录：{}", path.display()))
}

fn sync_dir_path(path: &Path) -> anyhow::Result<()> {
    let file = fs::File::from(
        rustix::fs::open(
            path,
            rustix::fs::OFlags::RDONLY
                | rustix::fs::OFlags::DIRECTORY
                | rustix::fs::OFlags::CLOEXEC,
            rustix::fs::Mode::empty(),
        )
        .with_context(|| format!("打开目录刷盘失败：{}", path.display()))?,
    );
    file.sync_all()
        .with_context(|| format!("刷盘目录失败：{}", path.display()))?;
    Ok(())
}
