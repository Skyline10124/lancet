// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 宿主后台扫描线程：两轮间隔 1 秒，按 stat 比较并仅在 stat 变化或处于粗粒度时间窗时读内容求摘要，
//! 产出候选路径，不持有影子锁，不阻塞 `act` 请求。

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::os::unix::ffi::OsStrExt as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use anyhow::Context as _;
use rustix::fs::{AtFlags, Dir, readlinkat, statat};

use crate::read_dir::PathRules;
use crate::shadow::model::{
    EMPTY_SHA256, EntryKind, EntrySnapshot, StatSig, classify_stat, hash_file_at,
    hash_host_file_via_ficlone_at, now_realtime_ns, open_dir_nofollow, open_root_fd,
};

/// 后台两轮宿主扫描之间的间隔。
const BACKGROUND_SCAN_INTERVAL: Duration = Duration::from_secs(1);

/// 供后台扫描线程比较的基准元数据（摘要按需惰性计算）。
#[derive(Clone)]
pub(crate) struct BaselineMeta {
    pub kind: EntryKind,
    pub mode: u32,
    pub size: u64,
    pub hash: Option<[u8; 32]>,
    pub link_target: Option<Vec<u8>>,
}

impl BaselineMeta {
    pub fn from_snapshot(snap: &EntrySnapshot) -> Self {
        Self {
            kind: snap.kind,
            mode: snap.mode,
            size: snap.size,
            hash: snap.hash,
            link_target: snap.link_target.clone(),
        }
    }

    fn ensure_hash_at(&mut self, base_root_fd: BorrowedFd<'_>, rel: &Path) -> Option<[u8; 32]> {
        if let Some(h) = self.hash {
            return Some(h);
        }
        if self.size == 0 {
            self.hash = Some(EMPTY_SHA256);
            return Some(EMPTY_SHA256);
        }
        let h = hash_file_at(base_root_fd, rel).ok()?;
        self.hash = Some(h);
        Some(h)
    }
}

/// 后台线程持有的上一轮宿主观测。
#[derive(Clone)]
struct HostObs {
    kind: EntryKind,
    mode: u32,
    size: u64,
    stat: StatSig,
    hash: Option<[u8; 32]>,
    link_target: Option<Vec<u8>>,
    differed_from_baseline: bool,
}

enum ScannerMsg {
    PatchBaseline(Vec<(PathBuf, Option<BaselineMeta>)>),
    Wake,
    Stop,
}

/// 宿主后台扫描器句柄；析构时通知线程退出并等待回收。
pub(crate) struct HostScanner {
    tx: mpsc::Sender<ScannerMsg>,
    stop: Arc<AtomicBool>,
    ready_candidates: Arc<Mutex<BTreeSet<PathBuf>>>,
    thread: Option<JoinHandle<()>>,
}

impl HostScanner {
    pub fn spawn(
        host_root: PathBuf,
        base_root: PathBuf,
        tmp_dir: PathBuf,
        rules: PathRules,
        initial_baseline: BTreeMap<PathBuf, BaselineMeta>,
        initial_host_stats: BTreeMap<PathBuf, EntrySnapshot>,
        initial_scan_ns: i128,
    ) -> anyhow::Result<Self> {
        let host_root_fd = open_root_fd(&host_root)
            .with_context(|| format!("后台扫描打开宿主根目录 {} 失败", host_root.display()))?;
        let base_root_fd = open_root_fd(&base_root)
            .with_context(|| format!("后台扫描打开基准根目录 {} 失败", base_root.display()))?;
        let tmp_root_fd = open_root_fd(&tmp_dir)
            .with_context(|| format!("后台扫描打开临时目录 {} 失败", tmp_dir.display()))?;

        let (tx, rx) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let stop_bg = Arc::clone(&stop);
        let ready_candidates = Arc::new(Mutex::new(BTreeSet::new()));
        let ready_bg = Arc::clone(&ready_candidates);

        let thread = thread::Builder::new()
            .name("lancet-host-scan".to_owned())
            .spawn(move || {
                scanner_loop(
                    host_root_fd,
                    base_root_fd,
                    tmp_root_fd,
                    rules,
                    initial_baseline,
                    initial_host_stats,
                    initial_scan_ns,
                    rx,
                    stop_bg,
                    ready_bg,
                );
            })?;

        Ok(Self {
            tx,
            stop,
            ready_candidates,
            thread: Some(thread),
        })
    }

    /// 同步或重置更新基准后，把基准增量推送给后台扫描线程。
    pub fn patch_baseline(&self, patches: Vec<(PathBuf, Option<BaselineMeta>)>) {
        if !patches.is_empty() {
            let _ = self.tx.send(ScannerMsg::PatchBaseline(patches));
        }
    }

    /// 宿主侧 inotify 溢出或重建时立即唤醒后台扫描，不必等待整轮间隔。
    pub fn wake(&self) {
        let _ = self.tx.send(ScannerMsg::Wake);
    }

    /// 取出后台已经就绪的候选路径集合，不等待进行中的扫描轮次。
    pub fn take_ready_candidates(&self) -> BTreeSet<PathBuf> {
        let mut guard = self
            .ready_candidates
            .lock()
            .expect("后台候选集合互斥锁不应中毒");
        std::mem::take(&mut *guard)
    }
}

impl Drop for HostScanner {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        let _ = self.tx.send(ScannerMsg::Stop);
        if let Some(handle) = self.thread.take() {
            let _ = handle.join();
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn scanner_loop(
    host_root_fd: OwnedFd,
    base_root_fd: OwnedFd,
    tmp_root_fd: OwnedFd,
    rules: PathRules,
    mut baseline: BTreeMap<PathBuf, BaselineMeta>,
    initial_host_stats: BTreeMap<PathBuf, EntrySnapshot>,
    mut last_round_ns: i128,
    rx: mpsc::Receiver<ScannerMsg>,
    stop: Arc<AtomicBool>,
    ready_candidates: Arc<Mutex<BTreeSet<PathBuf>>>,
) {
    let mut prev_obs: BTreeMap<PathBuf, HostObs> = initial_host_stats
        .into_iter()
        .map(|(path, snap)| {
            (
                path,
                HostObs {
                    kind: snap.kind,
                    mode: snap.mode,
                    size: snap.size,
                    stat: snap.stat,
                    hash: snap.hash,
                    link_target: snap.link_target,
                    differed_from_baseline: false,
                },
            )
        })
        .collect();

    let mut next_scan_at = Instant::now() + BACKGROUND_SCAN_INTERVAL;

    loop {
        let wait = next_scan_at.saturating_duration_since(Instant::now());
        match rx.recv_timeout(wait) {
            Ok(ScannerMsg::Stop) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Ok(ScannerMsg::PatchBaseline(patches)) => {
                apply_patches(&mut baseline, patches);
                let mut should_wake = false;
                let mut should_stop = false;
                while let Ok(msg) = rx.try_recv() {
                    match msg {
                        ScannerMsg::PatchBaseline(more) => apply_patches(&mut baseline, more),
                        ScannerMsg::Wake => should_wake = true,
                        ScannerMsg::Stop => {
                            should_stop = true;
                            break;
                        }
                    }
                }
                if should_stop {
                    break;
                }
                if !should_wake && Instant::now() < next_scan_at {
                    continue;
                }
            }
            Ok(ScannerMsg::Wake) | Err(mpsc::RecvTimeoutError::Timeout) => {
                // 扫描前先消费已到达的基准补丁。
                let mut should_stop = false;
                while let Ok(msg) = rx.try_recv() {
                    match msg {
                        ScannerMsg::PatchBaseline(patches) => apply_patches(&mut baseline, patches),
                        ScannerMsg::Wake => {}
                        ScannerMsg::Stop => {
                            should_stop = true;
                            break;
                        }
                    }
                }
                if should_stop {
                    break;
                }
            }
        }

        if stop.load(Ordering::Acquire) {
            break;
        }

        let round_start_ns = now_realtime_ns();
        let mut new_obs = BTreeMap::new();
        let mut candidates = BTreeSet::new();

        scan_host_dir(
            host_root_fd.as_fd(),
            base_root_fd.as_fd(),
            tmp_root_fd.as_fd(),
            &rules,
            Path::new(""),
            last_round_ns,
            &stop,
            &prev_obs,
            &mut baseline,
            &mut new_obs,
            &mut candidates,
        );

        if stop.load(Ordering::Acquire) {
            break;
        }

        // 上一轮存在或基准中存在、本轮已消失的路径成为候选。
        for old_path in prev_obs.keys() {
            if !new_obs.contains_key(old_path) {
                candidates.insert(old_path.clone());
            }
        }
        for base_path in baseline.keys() {
            if !new_obs.contains_key(base_path) {
                candidates.insert(base_path.clone());
            }
        }

        prev_obs = new_obs;
        last_round_ns = round_start_ns;

        if !candidates.is_empty() {
            let mut guard = ready_candidates.lock().expect("后台候选集合互斥锁不应中毒");
            guard.extend(candidates);
        }

        next_scan_at = Instant::now() + BACKGROUND_SCAN_INTERVAL;
    }
}

fn apply_patches(
    baseline: &mut BTreeMap<PathBuf, BaselineMeta>,
    patches: Vec<(PathBuf, Option<BaselineMeta>)>,
) {
    for (path, meta) in patches {
        if let Some(m) = meta {
            baseline.insert(path, m);
        } else {
            baseline.remove(&path);
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn scan_host_dir(
    host_root_fd: BorrowedFd<'_>,
    base_root_fd: BorrowedFd<'_>,
    tmp_root_fd: BorrowedFd<'_>,
    rules: &PathRules,
    rel_dir: &Path,
    last_round_ns: i128,
    stop: &AtomicBool,
    prev_obs: &BTreeMap<PathBuf, HostObs>,
    baseline: &mut BTreeMap<PathBuf, BaselineMeta>,
    new_obs: &mut BTreeMap<PathBuf, HostObs>,
    candidates: &mut BTreeSet<PathBuf>,
) {
    if stop.load(Ordering::Relaxed) {
        return;
    }
    let Ok(Some(read_fd)) = open_dir_nofollow(host_root_fd, rel_dir) else {
        return;
    };
    let Ok(stat_fd) = rustix::io::dup(&read_fd) else {
        return;
    };
    let Ok(mut dir) = Dir::new(read_fd) else {
        return;
    };

    let mut subdirs = Vec::new();
    while let Some(next) = dir.read() {
        if stop.load(Ordering::Relaxed) {
            return;
        }
        let Ok(entry) = next else {
            continue;
        };
        let bytes = entry.file_name().to_bytes();
        if bytes == b"." || bytes == b".." {
            continue;
        }
        let name_os = OsStr::from_bytes(bytes);
        let rel = if rel_dir.as_os_str().is_empty() {
            PathBuf::from(name_os)
        } else {
            rel_dir.join(name_os)
        };
        if rules.is_reserved_git(&rel) {
            continue;
        }
        let Ok(st) = statat(&stat_fd, name_os, AtFlags::SYMLINK_NOFOLLOW) else {
            continue;
        };
        let Some(kind) = classify_stat(&st) else {
            continue;
        };
        let is_dir = kind == EntryKind::Dir;
        if rules.is_hidden(&rel, is_dir) || rules.is_regenerable(&rel, is_dir) {
            continue;
        }

        let cur_stat = StatSig::from_stat(&st);
        let old = prev_obs.get(&rel);
        let stat_changed = old.is_none_or(|o| o.kind != kind || !o.stat.matches_stat(&st));
        let is_racy = kind == EntryKind::File
            && (old.is_some_and(|o| o.stat.is_within_racy_window(last_round_ns))
                || cur_stat.is_within_racy_window(last_round_ns));

        let obs = if stat_changed || is_racy {
            let (final_stat, hash, link_target) = match kind {
                EntryKind::File => {
                    match hash_host_file_via_ficlone_at(host_root_fd, &rel, tmp_root_fd) {
                        Ok(Some((post_stat, h))) => (post_stat, Some(h), None),
                        Ok(None) => continue,
                        Err(_) => {
                            // 权限受限或并发消失时跳过本轮，由同步核查或下轮再查。
                            continue;
                        }
                    }
                }
                EntryKind::Dir => (cur_stat, None, None),
                EntryKind::Symlink => {
                    let Ok(target) = readlinkat(&stat_fd, name_os, Vec::new()) else {
                        continue;
                    };
                    (cur_stat, None, Some(target.into_bytes()))
                }
            };

            let differs = match baseline.get_mut(&rel) {
                None => true,
                Some(base_meta) => {
                    if base_meta.kind != kind {
                        true
                    } else {
                        match kind {
                            EntryKind::Dir => base_meta.mode != final_stat.mode,
                            EntryKind::Symlink => base_meta.link_target != link_target,
                            EntryKind::File => {
                                if base_meta.mode != final_stat.mode
                                    || base_meta.size != final_stat.size
                                {
                                    true
                                } else {
                                    let base_h = base_meta.ensure_hash_at(base_root_fd, &rel);
                                    base_h != hash
                                }
                            }
                        }
                    }
                }
            };

            // 与基准不同时发出候选，或从不同恢复为基准时发出一次；稳定冲突是否重读由 Pass 4 决定。
            if differs || old.is_some_and(|o| o.differed_from_baseline) {
                candidates.insert(rel.clone());
            }

            HostObs {
                kind,
                mode: final_stat.mode,
                size: final_stat.size,
                stat: final_stat,
                hash,
                link_target,
                differed_from_baseline: differs,
            }
        } else {
            let prev = old.expect("stat 未变化时必有上一轮观测");
            let differs = prev.differed_from_baseline;
            if differs {
                // 若上次与基准不同，再确认基准是否已被同步更新；仍不同则继续发出候选。
                let still_differs = match baseline.get_mut(&rel) {
                    None => true,
                    Some(base_meta) => {
                        if base_meta.kind != prev.kind {
                            true
                        } else {
                            match prev.kind {
                                EntryKind::Dir => base_meta.mode != prev.mode,
                                EntryKind::Symlink => base_meta.link_target != prev.link_target,
                                EntryKind::File => {
                                    base_meta.mode != prev.mode
                                        || base_meta.size != prev.size
                                        || base_meta.ensure_hash_at(base_root_fd, &rel) != prev.hash
                                }
                            }
                        }
                    }
                };
                if still_differs {
                    candidates.insert(rel.clone());
                }
                HostObs {
                    differed_from_baseline: still_differs,
                    ..prev.clone()
                }
            } else {
                prev.clone()
            }
        };

        new_obs.insert(rel.clone(), obs);

        if is_dir {
            subdirs.push(rel);
        }
    }
    drop(dir);
    drop(stat_fd);

    for sub in subdirs {
        scan_host_dir(
            host_root_fd,
            base_root_fd,
            tmp_root_fd,
            rules,
            &sub,
            last_round_ns,
            stop,
            prev_obs,
            baseline,
            new_obs,
            candidates,
        );
    }
}
