// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 影子发布事务、冲突检查、逆序撤回与重启崩溃恢复。
//!
//! 本模块是 `CallRecord` 与 `PublishJournal` 磁盘格式的唯一真源，同时提供发布准备
//! （可中断）、临界区执行/逆序撤回与启动期崩溃恢复。

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::fmt::{self, Write as _};
use std::fs::{self, DirBuilder, File};
use std::io;
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::DirBuilderExt as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::time::Instant;

use anyhow::Context as _;
use lancet_protocol::{
    CatalogDigest, Change, EffectEntry, EffectKind, Labels, OutcomeKind, Receipt, Residue,
};
use rustix::fs::{
    AtFlags, Dir, FileType, Mode, OFlags, RenameFlags, chmodat, fchmod, fstat, ioctl_ficlone,
    mkdirat, openat, readlinkat, renameat, renameat_with, statat, symlinkat, unlinkat,
};
use rustix::io::Errno;
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use super::model::{
    EMPTY_SHA256, EntryKind, EntrySnapshot, StatSig, TreeTag, WidenedLog, classify_stat,
    clear_or_create_tmp_dir, compute_unified_diff_at, hash_host_file_via_ficlone_at, hash_reader,
    inspect_lstat_at, open_dir_nofollow, open_parent_nofollow, open_root_fd, open_syncable_dir_fd,
    path_depth, recover_widened_modes, rel_path_to_utf8, remove_tree_force, sync_dir_fd,
};
use super::scanner::BaselineMeta;
use super::{ShadowDir, ShadowInterrupt, ShadowSyncError, check_interrupt, compute_shadow_key};

/// 冲突消息与 `Indeterminate` 原因的统一字节上限（16 KiB，含标题、截断提示与待定提示）。
pub const MAX_DIAGNOSTIC_BYTES: usize = 16_384;

/// 计算调用记录文件与发布事务目录共用的 64 位小写十六进制 SHA-256 键。
pub fn call_record_hash(invocation_id: &str) -> String {
    let digest = Sha256::digest(invocation_id.as_bytes());
    let mut out = String::with_capacity(64);
    for byte in digest {
        let _ = write!(&mut out, "{byte:02x}");
    }
    out
}

/// 返回调用记录文件名（`<call_hash>.json`）。
pub fn call_record_file_name(invocation_id: &str) -> String {
    format!("{}.json", call_record_hash(invocation_id))
}

/// 持久调用记录账本文件结构（`<state_dir>/calls/<call_hash>.json`）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "state")]
pub enum CallRecord {
    Fenced,
    Pending {
        invocation_id: String,
        tool: String,
        catalog_digest: CatalogDigest,
        labels: Labels,
        #[serde(default)]
        writes_possible: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        publish: Option<PublishJournal>,
    },
    Settled {
        settled_at_ms: u64,
        receipt: Receipt,
    },
    Expired {
        outcome: OutcomeKind,
    },
}

/// 发布事务持久化日志，嵌在 `CallRecord::Pending.publish` 中。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "phase")]
pub enum PublishJournal {
    Undo {
        call_hash: String,
        roots: Vec<JournalRoot>,
        ops: Vec<JournalOp>,
        paths: Vec<BatchPath>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        rollback_reason: Option<String>,
    },
    Finish {
        call_hash: String,
        roots: Vec<JournalRoot>,
        paths: Vec<BatchPath>,
        outcome: PublishFinish,
    },
}

impl PublishJournal {
    pub fn call_hash(&self) -> &str {
        match self {
            Self::Undo { call_hash, .. } | Self::Finish { call_hash, .. } => call_hash,
        }
    }
}

/// 发布事务涉及的单个可写目录身份（恢复时按 `shadow_key` 与宿主根 `(dev, ino)` 核验）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JournalRoot {
    pub handle: String,
    pub shadow_key: String,
    pub host_dev: u64,
    pub host_ino: u64,
}

/// 文件系统对象身份 `(st_dev, st_ino)`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileId {
    pub dev: u64,
    pub ino: u64,
}

/// 发布事务中记录的单个路径对象语义快照。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum ObjectState {
    File {
        mode: u32,
        size: u64,
        sha256: [u8; 32],
    },
    Dir {
        mode: u32,
    },
    Symlink {
        target: Vec<u8>,
    },
}

impl ObjectState {
    fn from_snapshot_at(
        snap: &mut EntrySnapshot,
        root_fd: BorrowedFd<'_>,
        rel: &Path,
    ) -> io::Result<Self> {
        match snap.kind {
            EntryKind::File => {
                let sha256 = snap.ensure_hash_at(root_fd, rel)?;
                Ok(Self::File {
                    mode: snap.mode & 0o7777,
                    size: snap.size,
                    sha256,
                })
            }
            EntryKind::Dir => Ok(Self::Dir {
                mode: snap.mode & 0o7777,
            }),
            EntryKind::Symlink => Ok(Self::Symlink {
                target: snap.link_target.clone().unwrap_or_default(),
            }),
        }
    }

    fn is_dir(&self) -> bool {
        matches!(self, Self::Dir { .. })
    }
}

/// 提前在临界区外准备好的基准后态对象（位于 `<state_dir>/publish/<call_hash>/base/<slot>`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct StagedBase {
    pub slot: u64,
    pub id: FileId,
}

/// 发布批次中的单条路径描述（按 `(root_idx, rel)` 排好序）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BatchPath {
    pub root_idx: usize,
    pub rel: String,
    pub before: Option<ObjectState>,
    pub after: Option<ObjectState>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub staged_base: Option<StagedBase>,
}

/// 发布事务五阶段中的单条持久化操作。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op")]
pub enum JournalOp {
    /// 阶段 1（放宽已有只读目录）或阶段 5（收紧/恢复目录最终权限）。
    ChmodDir {
        op_id: u64,
        root_idx: usize,
        rel: String,
        before_mode: u32,
        after_mode: u32,
        /// 若该目录曾由阶段 1 临时放宽，记录其原始权限以便恢复时识别未执行与避免无谓改写。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        original_mode: Option<u32>,
        target_id: FileId,
    },
    /// 阶段 2（删除文件/符号链接/空目录）。
    Delete {
        op_id: u64,
        root_idx: usize,
        rel: String,
        before: ObjectState,
    },
    /// 阶段 3（新建目录）或阶段 4（新建文件/符号链接）。
    Create {
        op_id: u64,
        root_idx: usize,
        rel: String,
        staged_id: FileId,
        after: ObjectState,
    },
    /// 阶段 2（目录换叶）、阶段 3（叶换目录）或阶段 4（叶换叶）。
    Exchange {
        op_id: u64,
        root_idx: usize,
        rel: String,
        staged_id: FileId,
        host_tmp_name: String,
        before: ObjectState,
        after: ObjectState,
    },
}

impl JournalOp {
    fn op_id(&self) -> u64 {
        match self {
            Self::ChmodDir { op_id, .. }
            | Self::Delete { op_id, .. }
            | Self::Create { op_id, .. }
            | Self::Exchange { op_id, .. } => *op_id,
        }
    }

    fn root_idx(&self) -> usize {
        match self {
            Self::ChmodDir { root_idx, .. }
            | Self::Delete { root_idx, .. }
            | Self::Create { root_idx, .. }
            | Self::Exchange { root_idx, .. } => *root_idx,
        }
    }

    fn rel(&self) -> &str {
        match self {
            Self::ChmodDir { rel, .. }
            | Self::Delete { rel, .. }
            | Self::Create { rel, .. }
            | Self::Exchange { rel, .. } => rel,
        }
    }
}

/// 部分撤回时每条批次路径的最终去向。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PathDisposition {
    Published,
    KeptHostEdit,
    RolledBack,
    NotExecuted,
}

impl PathDisposition {
    pub fn as_zh(self) -> &'static str {
        match self {
            Self::Published => "已发布",
            Self::KeptHostEdit => "保留宿主修改",
            Self::RolledBack => "已撤回",
            Self::NotExecuted => "未执行",
        }
    }

    fn stays_on_host(self) -> bool {
        matches!(self, Self::Published | Self::KeptHostEdit)
    }
}

/// `PublishFinish::Indeterminate` 中按 `(root_idx, rel)` 记录的单条路径去向。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PathDispositionEntry {
    pub root_idx: usize,
    pub rel: String,
    pub disposition: PathDisposition,
}

/// 发布事务进入 `Finish` 阶段的最终结论。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "result")]
pub enum PublishFinish {
    Committed,
    RolledBack,
    Indeterminate {
        dispositions: Vec<PathDispositionEntry>,
    },
}

/// 发布前检查的三类冲突原因（枚举顺序亦对应排序顺序）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ConflictReason {
    Hidden,
    MultipleLinks(u64),
    HostChanged,
}

impl fmt::Display for ConflictReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Hidden => f.write_str("hidden 路径"),
            Self::MultipleLinks(n) => write!(f, "多个链接（{n}）"),
            Self::HostChanged => f.write_str("宿主已改"),
        }
    }
}

/// 单条发布冲突项。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublishConflict {
    pub slot_idx: usize,
    pub handle: String,
    pub rel: String,
    pub reason: ConflictReason,
}

/// 按决定 2 格式与 16 KiB 上限排版冲突消息，为调用方待附加的 `\n{pending_hint}` 预留字节。
pub fn format_conflicts(conflicts: &[PublishConflict], pending_hint: Option<&str>) -> String {
    let lines: Vec<String> = conflicts
        .iter()
        .map(|c| format!("{} {:?}：{}", c.handle, c.rel, c.reason))
        .collect();
    let reserve = pending_hint.map_or(0, |h| 1 + h.len());
    let limit = MAX_DIAGNOSTIC_BYTES.saturating_sub(reserve);
    truncate_lines_to_limit(&lines, limit)
}

/// 按决定 2 格式与 16 KiB 上限排版部分撤回的 `Indeterminate` 原因。
pub fn format_indeterminate_reason(
    header: &str,
    roots: &[JournalRoot],
    dispositions: &[PathDispositionEntry],
) -> String {
    let lines: Vec<String> = dispositions
        .iter()
        .map(|d| {
            let handle = roots.get(d.root_idx).map_or("?", |r| r.handle.as_str());
            format!("{handle} {:?}：{}", d.rel, d.disposition.as_zh())
        })
        .collect();
    let limit = MAX_DIAGNOSTIC_BYTES.saturating_sub(header.len());
    let body = truncate_lines_to_limit(&lines, limit);
    format!("{header}{body}")
}

fn truncate_lines_to_limit(lines: &[String], limit: usize) -> String {
    if lines.is_empty() {
        return String::new();
    }
    let full_len: usize = lines.iter().map(String::len).sum::<usize>() + lines.len() - 1;
    if full_len <= limit {
        return lines.join("\n");
    }

    let mut best_kept = 0usize;
    let mut acc_len = 0usize;
    for (idx, line) in lines.iter().enumerate() {
        let next_len = if idx == 0 {
            line.len()
        } else {
            acc_len + 1 + line.len()
        };
        let kept_count = idx + 1;
        if kept_count == lines.len() {
            break;
        }
        let omitted = lines.len() - kept_count;
        let suffix = format!("\n另有 {omitted} 条未列出");
        if next_len + suffix.len() <= limit {
            best_kept = kept_count;
            acc_len = next_len;
        } else {
            break;
        }
    }

    let omitted = lines.len() - best_kept;
    if best_kept == 0 {
        let fallback = format!("另有 {omitted} 条未列出");
        if fallback.len() <= limit {
            fallback
        } else {
            let end = fallback.floor_char_boundary(limit);
            fallback[..end].to_owned()
        }
    } else {
        let mut out = lines[..best_kept].join("\n");
        let _ = write!(&mut out, "\n另有 {omitted} 条未列出");
        out
    }
}

/// `prepare_publish` 成功时的三种结果。
pub enum PublishPrep {
    /// 全部可写目录均无待定改动（零磁盘日志、零发布目录）。
    Empty,
    /// 发布前检查发现一处或多处冲突（未创建发布目录、未写宿主）。
    Conflicts(Vec<PublishConflict>),
    /// 全部暂存对象已就位并刷盘，准备进入不可中断发布临界区。
    Ready(PreparedPublish),
}

/// `prepare_publish` 期间的中断或 I/O 失败（可安全清理暂存目录并返回）。
#[derive(Debug)]
pub enum PublishPrepareError {
    Interrupted(ShadowInterrupt),
    Failed(String),
}

/// 临界区发布执行完毕后的非致命结果。
pub enum PublishOutcome {
    /// 全部变更已发布、基准已推进、事务目录已清理。
    Committed { effects: Vec<EffectEntry> },
    /// 遇到晚到冲突，已完整逆序撤回（宿主与基准均未改变，草稿保留）。
    RolledBackConflict { conflicts: Vec<PublishConflict> },
    /// 宿主操作遇到 I/O 错误，已完整逆序撤回（宿主与基准均未改变，草稿保留）。
    RolledBackIoError { message: String },
    /// 撤回期间发现部分路径已被宿主修改，已保留宿主修改并仅推进保留路径的基准。
    Indeterminate {
        reason: String,
        effects: Vec<EffectEntry>,
    },
}

/// 临界区内无法继续安全推进的致命错误（需由节点终止进程，待重启后经日志恢复）。
#[derive(Debug)]
pub struct PublishFatalError(pub anyhow::Error);

impl fmt::Display for PublishFatalError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:#}", self.0)
    }
}

impl std::error::Error for PublishFatalError {}

/// 已完成冲突检查、diff 计算与对象暂存的发布句柄。
pub struct PreparedPublish {
    state_dir: PathBuf,
    call_hash: String,
    call_dir: PathBuf,
    undo_journal: PublishJournal,
    /// `(root_idx, rel) -> unified diff`（仅内存保存用于本次回执，不写入日志）。
    diffs: BTreeMap<(usize, String), Option<String>>,
}

impl PreparedPublish {
    pub fn undo_journal(&self) -> &PublishJournal {
        &self.undo_journal
    }

    pub fn call_dir(&self) -> &Path {
        &self.call_dir
    }

    /// 未进入临界区（例如准入前发现已取消、超时或写日志失败）时清理暂存目录。
    pub fn cleanup_staging(self) {
        let _ = cleanup_publish_dir(&self.state_dir, &self.call_hash);
    }

    /// 在不可中断的发布临界区内执行已持久化的 `Undo` 事务（绝不读取 `cancelled`）。
    ///
    /// 调用方必须已先将 `self.undo_journal()` 持久写入调用记录账本；本方法仅在遇到
    /// 撤回/完成阶段转换时通过 `persist_journal` 更新调用记录。
    pub fn execute<F>(
        self,
        shadows: &[&ShadowDir],
        mut persist_journal: F,
    ) -> Result<PublishOutcome, PublishFatalError>
    where
        F: FnMut(&PublishJournal) -> anyhow::Result<()>,
    {
        let PublishJournal::Undo {
            call_hash,
            roots,
            ops,
            paths,
            ..
        } = self.undo_journal
        else {
            unreachable!("PreparedPublish 只持有 Undo 阶段日志");
        };

        let stage_dir = self.call_dir.join("stage");
        let pre_dir = self.call_dir.join("pre");
        let base_stage_dir = self.call_dir.join("base");
        let stage_fd = open_syncable_dir_fd(&stage_dir)
            .with_context(|| format!("打开暂存目录失败：{}", stage_dir.display()))
            .map_err(PublishFatalError)?;
        let pre_fd = open_syncable_dir_fd(&pre_dir)
            .with_context(|| format!("打开备份目录失败：{}", pre_dir.display()))
            .map_err(PublishFatalError)?;
        let base_stage_fd = open_syncable_dir_fd(&base_stage_dir)
            .with_context(|| format!("打开基准暂存目录失败：{}", base_stage_dir.display()))
            .map_err(PublishFatalError)?;

        let host_fds: Vec<BorrowedFd<'_>> = shadows
            .iter()
            .map(|s| s.inner.host_root_fd.as_fd())
            .collect();

        let mut touched_dirs: BTreeSet<(usize, PathBuf)> = BTreeSet::new();
        let mut failure: Option<(usize, ApplyFailure)> = None;

        for (idx, op) in ops.iter().enumerate() {
            match apply_op_with_idx(
                idx,
                op,
                host_fds[op.root_idx()],
                stage_fd.as_fd(),
                pre_fd.as_fd(),
                &mut touched_dirs,
            ) {
                Ok(()) => {}
                Err(err) => {
                    failure = Some((idx, err));
                    break;
                }
            }
        }

        if let Some((fail_idx, apply_err)) = failure {
            let (mutated_host, reason_text) = match &apply_err {
                ApplyFailure::LateConflict {
                    op_idx,
                    mutated_host,
                } => {
                    let op = &ops[*op_idx];
                    (
                        *mutated_host,
                        format!("晚到冲突：{} {:?}", roots[op.root_idx()].handle, op.rel()),
                    )
                }
                ApplyFailure::IoError {
                    mutated_host,
                    message,
                    ..
                } => (*mutated_host, message.clone()),
            };
            let undo_with_reason = PublishJournal::Undo {
                call_hash: call_hash.clone(),
                roots: roots.clone(),
                ops: ops.clone(),
                paths: paths.clone(),
                rollback_reason: Some(reason_text),
            };
            persist_journal(&undo_with_reason)
                .context("持久化撤回原因失败")
                .map_err(PublishFatalError)?;

            let rollback_slice = if mutated_host {
                &ops[..=fail_idx]
            } else {
                &ops[..fail_idx]
            };
            let rb = rollback_ops(
                rollback_slice,
                &paths,
                &host_fds,
                stage_fd.as_fd(),
                pre_fd.as_fd(),
                &mut touched_dirs,
            )
            .context("发布撤回失败")
            .map_err(PublishFatalError)?;

            sync_host_and_publish_dirs(&host_fds, &touched_dirs, stage_fd.as_fd(), pre_fd.as_fd())
                .context("撤回后刷盘宿主与发布目录失败")
                .map_err(PublishFatalError)?;

            if !rb.has_host_stays {
                let finish = PublishJournal::Finish {
                    call_hash: call_hash.clone(),
                    roots: roots.clone(),
                    paths: paths.clone(),
                    outcome: PublishFinish::RolledBack,
                };
                persist_journal(&finish)
                    .context("持久化 RolledBack 完成日志失败")
                    .map_err(PublishFatalError)?;
                cleanup_publish_dir(&self.state_dir, &call_hash)
                    .context("清理已撤回发布事务目录失败")
                    .map_err(PublishFatalError)?;

                // 刷新冲突状态，使晚到冲突反映在后续 status() 中。
                for shadow in shadows {
                    let mut st = shadow.inner.lock_state();
                    let _ = shadow
                        .inner
                        .recompute_all_pending_conflicts_locked(&mut st, None, None);
                }

                return match apply_err {
                    ApplyFailure::LateConflict { op_idx, .. } => {
                        let op = &ops[op_idx];
                        Ok(PublishOutcome::RolledBackConflict {
                            conflicts: vec![PublishConflict {
                                slot_idx: op.root_idx(),
                                handle: roots[op.root_idx()].handle.clone(),
                                rel: op.rel().to_owned(),
                                reason: ConflictReason::HostChanged,
                            }],
                        })
                    }
                    ApplyFailure::IoError { message, .. } => {
                        Ok(PublishOutcome::RolledBackIoError { message })
                    }
                };
            }

            // 部分路径已被宿主修改：记录 Finish(Indeterminate)，仅推进留在宿主路径的基准。
            let dispositions = rb.dispositions;
            let finish = PublishJournal::Finish {
                call_hash: call_hash.clone(),
                roots: roots.clone(),
                paths: paths.clone(),
                outcome: PublishFinish::Indeterminate {
                    dispositions: dispositions.clone(),
                },
            };
            persist_journal(&finish)
                .context("持久化 Indeterminate 完成日志失败")
                .map_err(PublishFatalError)?;

            advance_live_shadows_partial(
                Some(base_stage_fd.as_fd()),
                shadows,
                &paths,
                &dispositions,
            )
            .context("部分撤回后推进保留路径基准失败")
            .map_err(PublishFatalError)?;

            cleanup_publish_dir(&self.state_dir, &call_hash)
                .context("清理部分撤回发布事务目录失败")
                .map_err(PublishFatalError)?;

            let kept_set: BTreeSet<(usize, &str)> = dispositions
                .iter()
                .filter(|d| d.disposition.stays_on_host())
                .map(|d| (d.root_idx, d.rel.as_str()))
                .collect();
            let effects = build_publish_effects(&roots, &paths, Some(&kept_set), Some(&self.diffs));
            let reason = format_indeterminate_reason(
                "发布撤回时部分路径已被宿主修改，已保留宿主修改：\n",
                &roots,
                &dispositions,
            );
            return Ok(PublishOutcome::Indeterminate { reason, effects });
        }

        // 全部宿主操作成功：刷盘后写入 Finish(Committed)，再推进基准并清理事务目录。
        sync_host_and_publish_dirs(&host_fds, &touched_dirs, stage_fd.as_fd(), pre_fd.as_fd())
            .context("发布后刷盘宿主与发布目录失败")
            .map_err(PublishFatalError)?;

        let finish = PublishJournal::Finish {
            call_hash: call_hash.clone(),
            roots: roots.clone(),
            paths: paths.clone(),
            outcome: PublishFinish::Committed,
        };
        persist_journal(&finish)
            .context("持久化 Committed 完成日志失败")
            .map_err(PublishFatalError)?;

        advance_live_shadows_committed(Some(base_stage_fd.as_fd()), shadows, &paths)
            .context("发布完成后推进基准失败")
            .map_err(PublishFatalError)?;

        cleanup_publish_dir(&self.state_dir, &call_hash)
            .context("清理已完成发布事务目录失败")
            .map_err(PublishFatalError)?;

        let effects = build_publish_effects(&roots, &paths, None, Some(&self.diffs));
        Ok(PublishOutcome::Committed { effects })
    }
}

/// 临界区之外的发布准备（可响应 `deadline` 与 `cancelled`）：
/// 1. 核对每个可写目录的影子待定改动；
/// 2. 全部为空时直接返回 `Empty`（零磁盘写入）；
/// 3. 逐路径执行发布前冲突检查（hidden / 多个链接 / 宿主已改 / 非空目录）；
/// 4. 计算待定路径的 unified diff；
/// 5. 在 `<state_dir>/publish/<call_hash>/{stage,pre,base}` 预暂存宿主与基准后态对象、刷盘并构造五阶段操作序列。
pub fn prepare_publish(
    state_dir: &Path,
    call_hash: &str,
    shadows: &[&ShadowDir],
    deadline: Option<Instant>,
    cancelled: Option<&AtomicBool>,
) -> Result<PublishPrep, PublishPrepareError> {
    if shadows.is_empty() {
        return Ok(PublishPrep::Empty);
    }

    // 1. 核对全部可写目录的影子事件与索引，并检查是否全部为空。
    let mut any_pending = false;
    for shadow in shadows {
        if let Some(intr) = check_interrupt(deadline, cancelled) {
            return Err(PublishPrepareError::Interrupted(intr));
        }
        let mut st = shadow.inner.lock_state();
        shadow
            .inner
            .reconcile_shadow_locked(&mut st, deadline, cancelled)
            .map_err(map_sync_err)?;
        if !st.pending.is_empty() {
            any_pending = true;
        }
    }
    if !any_pending {
        return Ok(PublishPrep::Empty);
    }

    // 2. 跨全部可写目录执行发布前冲突检查与快照采集。
    let mut conflicts: Vec<PublishConflict> = Vec::new();
    let mut per_root_plans: Vec<RootPlan> = Vec::with_capacity(shadows.len());

    for (slot_idx, shadow) in shadows.iter().enumerate() {
        if let Some(intr) = check_interrupt(deadline, cancelled) {
            return Err(PublishPrepareError::Interrupted(intr));
        }
        let mut st = shadow.inner.lock_state();
        let plan = inspect_and_check_root(
            slot_idx,
            shadow,
            &mut st,
            deadline,
            cancelled,
            &mut conflicts,
        )?;
        per_root_plans.push(plan);
    }

    if !conflicts.is_empty() {
        conflicts.sort_by(|a, b| {
            a.slot_idx
                .cmp(&b.slot_idx)
                .then_with(|| a.rel.as_bytes().cmp(b.rel.as_bytes()))
                .then_with(|| a.reason.cmp(&b.reason))
        });
        return Ok(PublishPrep::Conflicts(conflicts));
    }

    // 3. 计算 unified diff（不落日志，仅用于本次回执）。
    let mut diffs: BTreeMap<(usize, String), Option<String>> = BTreeMap::new();
    for (root_idx, (shadow, plan)) in shadows.iter().zip(per_root_plans.iter()).enumerate() {
        for item in &plan.items {
            if let Some(intr) = check_interrupt(deadline, cancelled) {
                return Err(PublishPrepareError::Interrupted(intr));
            }
            let patch = compute_unified_diff_at(
                &item.rel_utf8,
                &item.rel_path,
                item.action,
                shadow.inner.base_root_fd.as_fd(),
                shadow.inner.tree_root_fd.as_fd(),
            );
            diffs.insert((root_idx, item.rel_utf8.clone()), patch);
        }
    }

    // 4. 创建 `<state_dir>/publish/<call_hash>/{stage,pre,base}` 并暂存宿主与基准对象、刷盘与编排五阶段操作。
    let publish_root = state_dir.join("publish");
    ensure_publish_root(&publish_root, state_dir)
        .map_err(|err| PublishPrepareError::Failed(format!("创建发布根目录失败：{err:#}")))?;

    let call_dir = publish_root.join(call_hash);
    let stage_dir = call_dir.join("stage");
    let pre_dir = call_dir.join("pre");
    let base_stage_dir = call_dir.join("base");

    let stage_res = (|| -> Result<PreparedPublish, PublishPrepareError> {
        let _ = remove_tree_force(&call_dir);
        DirBuilder::new()
            .mode(0o700)
            .create(&call_dir)
            .map_err(|e| PublishPrepareError::Failed(format!("创建发布事务目录失败：{e}")))?;
        DirBuilder::new()
            .mode(0o700)
            .create(&stage_dir)
            .map_err(|e| PublishPrepareError::Failed(format!("创建暂存目录失败：{e}")))?;
        DirBuilder::new()
            .mode(0o700)
            .create(&pre_dir)
            .map_err(|e| PublishPrepareError::Failed(format!("创建备份目录失败：{e}")))?;
        DirBuilder::new()
            .mode(0o700)
            .create(&base_stage_dir)
            .map_err(|e| PublishPrepareError::Failed(format!("创建基准暂存目录失败：{e}")))?;

        let stage_fd = open_syncable_dir_fd(&stage_dir)
            .map_err(|e| PublishPrepareError::Failed(format!("打开暂存目录失败：{e}")))?;
        let pre_fd = open_syncable_dir_fd(&pre_dir)
            .map_err(|e| PublishPrepareError::Failed(format!("打开备份目录失败：{e}")))?;
        let base_stage_fd = open_syncable_dir_fd(&base_stage_dir)
            .map_err(|e| PublishPrepareError::Failed(format!("打开基准暂存目录失败：{e}")))?;
        let call_dir_fd = open_syncable_dir_fd(&call_dir)
            .map_err(|e| PublishPrepareError::Failed(format!("打开事务目录失败：{e}")))?;
        let publish_root_fd = open_syncable_dir_fd(&publish_root)
            .map_err(|e| PublishPrepareError::Failed(format!("打开发布根目录失败：{e}")))?;

        let mut roots = Vec::with_capacity(shadows.len());
        let mut ops = Vec::new();
        let mut paths = Vec::new();
        let mut next_op_id = 1u64;
        let mut next_base_slot = 1u64;

        for (root_idx, (shadow, plan)) in shadows.iter().zip(per_root_plans).enumerate() {
            roots.push(JournalRoot {
                handle: shadow.inner.name.clone(),
                shadow_key: shadow.inner.key.clone(),
                host_dev: shadow.inner.host_dev,
                host_ino: shadow.inner.host_ino,
            });
            if plan.items.is_empty() {
                continue;
            }
            build_root_ops_and_stage(
                root_idx,
                call_hash,
                shadow.inner.tree_root_fd.as_fd(),
                stage_fd.as_fd(),
                base_stage_fd.as_fd(),
                plan,
                &mut next_op_id,
                &mut next_base_slot,
                &mut ops,
                &mut paths,
                deadline,
                cancelled,
            )?;
        }

        if let Some(intr) = check_interrupt(deadline, cancelled) {
            return Err(PublishPrepareError::Interrupted(intr));
        }

        sync_dir_fd(stage_fd.as_fd())
            .map_err(|e| PublishPrepareError::Failed(format!("刷盘暂存目录失败：{e}")))?;
        sync_dir_fd(pre_fd.as_fd())
            .map_err(|e| PublishPrepareError::Failed(format!("刷盘备份目录失败：{e}")))?;
        sync_dir_fd(base_stage_fd.as_fd())
            .map_err(|e| PublishPrepareError::Failed(format!("刷盘基准暂存目录失败：{e}")))?;
        sync_dir_fd(call_dir_fd.as_fd())
            .map_err(|e| PublishPrepareError::Failed(format!("刷盘事务目录失败：{e}")))?;
        sync_dir_fd(publish_root_fd.as_fd())
            .map_err(|e| PublishPrepareError::Failed(format!("刷盘发布根目录失败：{e}")))?;

        let undo_journal = PublishJournal::Undo {
            call_hash: call_hash.to_owned(),
            roots,
            ops,
            paths,
            rollback_reason: None,
        };

        Ok(PreparedPublish {
            state_dir: state_dir.to_path_buf(),
            call_hash: call_hash.to_owned(),
            call_dir: call_dir.clone(),
            undo_journal,
            diffs,
        })
    })();

    if stage_res.is_err() {
        let _ = cleanup_publish_dir(state_dir, call_hash);
    }
    stage_res.map(PublishPrep::Ready)
}

struct RootPlan {
    items: Vec<PlannedItem>,
    /// 本根目录中需要阶段 1 放宽的现有只读目录（含批次路径的只读祖先、被删除/换型/改权的只读目录）。
    /// `rel -> (FileId, orig_mode)`
    host_dir_ids: BTreeMap<PathBuf, (FileId, u32)>,
}

struct PlannedItem {
    rel_path: PathBuf,
    rel_utf8: String,
    action: super::PendingAction,
    before: Option<ObjectState>,
    after: Option<ObjectState>,
    /// 若 `before` 为目录，记录宿主现有目录的 `(dev, ino)`。
    before_dir_id: Option<FileId>,
}

enum RawHostEntry {
    Missing,
    ParentInvalid,
    Special,
    Regular(EntrySnapshot),
}

fn inspect_host_raw_at(host_root_fd: BorrowedFd<'_>, rel: &Path) -> io::Result<RawHostEntry> {
    let Some((parent_owned, leaf)) = open_parent_nofollow(host_root_fd, rel)? else {
        return Ok(RawHostEntry::ParentInvalid);
    };
    let parent_fd = parent_owned.as_ref().map_or(host_root_fd, AsFd::as_fd);
    let st = match statat(parent_fd, leaf, AtFlags::SYMLINK_NOFOLLOW) {
        Ok(st) => st,
        Err(Errno::NOENT | Errno::NOTDIR) => return Ok(RawHostEntry::Missing),
        Err(err) => return Err(io::Error::from(err)),
    };
    let Some(kind) = classify_stat(&st) else {
        return Ok(RawHostEntry::Special);
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
    Ok(RawHostEntry::Regular(EntrySnapshot {
        kind,
        mode: stat.mode,
        size: stat.size,
        hash,
        link_target,
        stat,
    }))
}

fn inspect_and_check_root(
    slot_idx: usize,
    shadow: &ShadowDir,
    st: &mut super::ShadowState,
    deadline: Option<Instant>,
    cancelled: Option<&AtomicBool>,
    conflicts: &mut Vec<PublishConflict>,
) -> Result<RootPlan, PublishPrepareError> {
    let handle = shadow.inner.name.clone();
    let host_root_fd = shadow.inner.host_root_fd.as_fd();
    let base_root_fd = shadow.inner.base_root_fd.as_fd();
    let tree_root_fd = shadow.inner.tree_root_fd.as_fd();
    let tmp_root_fd = shadow.inner.tmp_root_fd.as_fd();

    // 预计算将被本批次创建或转换为目录的路径，以及将被本批次删除的路径。
    let mut dirs_created_in_batch: BTreeSet<PathBuf> = BTreeSet::new();
    let mut paths_deleted_in_batch: BTreeSet<PathBuf> = BTreeSet::new();
    for (rel, entry) in &st.pending {
        if entry.hidden || entry.tree_hidden {
            continue;
        }
        let base_kind = st.base_index.get(rel).map(|e| e.kind);
        let tree_kind = st.tree_index.get(rel).map(|e| e.kind);
        if tree_kind == Some(EntryKind::Dir) && base_kind != Some(EntryKind::Dir) {
            dirs_created_in_batch.insert(rel.clone());
        }
        if base_kind.is_some() && tree_kind.is_none() {
            paths_deleted_in_batch.insert(rel.clone());
        }
    }

    let pending_list: Vec<(PathBuf, super::PendingEntry)> =
        st.pending.iter().map(|(k, &v)| (k.clone(), v)).collect();
    let mut items = Vec::with_capacity(pending_list.len());
    let mut host_dir_ids: BTreeMap<PathBuf, (FileId, u32)> = BTreeMap::new();

    for (rel, entry) in pending_list {
        if let Some(intr) = check_interrupt(deadline, cancelled) {
            return Err(PublishPrepareError::Interrupted(intr));
        }
        let rel_utf8 = rel_path_to_utf8(&rel).ok_or_else(|| {
            PublishPrepareError::Failed("待定改动包含不是有效 UTF-8 的路径".to_owned())
        })?;

        if entry.hidden || entry.tree_hidden {
            conflicts.push(PublishConflict {
                slot_idx,
                handle: handle.clone(),
                rel: rel_utf8.clone(),
                reason: ConflictReason::Hidden,
            });
            // 纯 hidden 新增项（基准无可见项）不检查宿主，避免泄露宿主隐藏项存在性。
            if entry.hidden {
                continue;
            }
        }

        let base_before = match st.base_index.get_mut(&rel) {
            Some(base_snap) => Some(
                ObjectState::from_snapshot_at(base_snap, base_root_fd, &rel).map_err(|err| {
                    PublishPrepareError::Failed(format!(
                        "读取基准路径 `{}` 失败：{err}",
                        rel.display()
                    ))
                })?,
            ),
            None => None,
        };

        let host_raw = inspect_host_raw_at(host_root_fd, &rel).map_err(|err| {
            PublishPrepareError::Failed(format!("检查宿主路径 `{}` 失败：{err}", rel.display()))
        })?;

        let mut before_dir_id = None;
        let mut host_changed = false;

        match (&base_before, host_raw) {
            (None, RawHostEntry::Missing) => {}
            (None, RawHostEntry::ParentInvalid) => {
                // 上级不存在仅当某个严格祖先在本批次被新建或转为目录时合法。
                let has_new_dir_anc = rel
                    .ancestors()
                    .skip(1)
                    .any(|anc| !anc.as_os_str().is_empty() && dirs_created_in_batch.contains(anc));
                if !has_new_dir_anc {
                    host_changed = true;
                }
            }
            (None, RawHostEntry::Special | RawHostEntry::Regular(_)) => {
                host_changed = true;
            }
            (
                Some(_),
                RawHostEntry::Missing | RawHostEntry::ParentInvalid | RawHostEntry::Special,
            ) => {
                host_changed = true;
            }
            (Some(base_state), RawHostEntry::Regular(host_snap)) => {
                if host_snap.kind == EntryKind::File && host_snap.stat.nlink > 1 {
                    conflicts.push(PublishConflict {
                        slot_idx,
                        handle: handle.clone(),
                        rel: rel_utf8.clone(),
                        reason: ConflictReason::MultipleLinks(host_snap.stat.nlink),
                    });
                }

                match base_state {
                    ObjectState::Dir { mode } => {
                        if host_snap.kind != EntryKind::Dir || (host_snap.mode & 0o7777) != *mode {
                            host_changed = true;
                        } else {
                            let id = FileId {
                                dev: host_snap.stat.dev,
                                ino: host_snap.stat.ino,
                            };
                            before_dir_id = Some(id);
                            host_dir_ids.insert(rel.clone(), (id, *mode));
                        }
                    }
                    ObjectState::Symlink { target } => {
                        if host_snap.kind != EntryKind::Symlink
                            || host_snap.link_target.as_ref() != Some(target)
                        {
                            host_changed = true;
                        }
                    }
                    ObjectState::File { mode, size, sha256 } => {
                        if host_snap.kind != EntryKind::File
                            || (host_snap.mode & 0o7777) != *mode
                            || host_snap.size != *size
                        {
                            host_changed = true;
                        } else {
                            match hash_host_file_via_ficlone_at(host_root_fd, &rel, tmp_root_fd) {
                                Ok(Some((post_stat, actual_hash))) => {
                                    if (post_stat.mode & 0o7777) != *mode
                                        || post_stat.size != *size
                                        || actual_hash != *sha256
                                    {
                                        host_changed = true;
                                    }
                                }
                                Ok(None) => host_changed = true,
                                Err(err) => {
                                    return Err(PublishPrepareError::Failed(format!(
                                        "校验宿主文件 `{}` 内容失败：{err}",
                                        rel.display()
                                    )));
                                }
                            }
                        }
                    }
                }
            }
        }

        // 若该路径在基准中是目录，且本批次要删除它或将其换型为非目录，核查宿主实际子项是否均在批次删除列表中。
        let tree_is_dir = st
            .tree_index
            .get(&rel)
            .is_some_and(|e| e.kind == EntryKind::Dir && !entry.tree_hidden);
        if !host_changed && matches!(base_before, Some(ObjectState::Dir { .. })) && !tree_is_dir {
            let has_extra_child =
                host_dir_has_undeleted_child(host_root_fd, &rel, &paths_deleted_in_batch).map_err(
                    |err| {
                        PublishPrepareError::Failed(format!(
                            "检查宿主目录 `{}` 子项失败：{err}",
                            rel.display()
                        ))
                    },
                )?;
            if has_extra_child {
                host_changed = true;
            }
        }

        if host_changed {
            conflicts.push(PublishConflict {
                slot_idx,
                handle: handle.clone(),
                rel: rel_utf8.clone(),
                reason: ConflictReason::HostChanged,
            });
            continue;
        }

        if entry.tree_hidden {
            continue;
        }

        let after_state = match st.tree_index.get_mut(&rel) {
            Some(tree_snap) => Some(
                ObjectState::from_snapshot_at(tree_snap, tree_root_fd, &rel).map_err(|err| {
                    PublishPrepareError::Failed(format!(
                        "读取影子路径 `{}` 失败：{err}",
                        rel.display()
                    ))
                })?,
            ),
            None => None,
        };

        items.push(PlannedItem {
            rel_path: rel,
            rel_utf8,
            action: entry.action,
            before: base_before,
            after: after_state,
            before_dir_id,
        });
    }

    // 只有在当前无冲突时才收集只读祖先目录身份，避免冲突场景多做系统调用。
    if conflicts.is_empty() {
        let mut ancestors_to_check: BTreeSet<PathBuf> = BTreeSet::new();
        for item in &items {
            let mut cur = item.rel_path.parent();
            while let Some(anc) = cur {
                if anc.as_os_str().is_empty() {
                    break;
                }
                if st
                    .base_index
                    .get(anc)
                    .is_some_and(|e| e.kind == EntryKind::Dir)
                    && !host_dir_ids.contains_key(anc)
                {
                    ancestors_to_check.insert(anc.to_path_buf());
                }
                cur = anc.parent();
            }
        }
        for anc in ancestors_to_check {
            if let Some(intr) = check_interrupt(deadline, cancelled) {
                return Err(PublishPrepareError::Interrupted(intr));
            }
            // 不在本批中的上级目录只要求仍是可按需访问的目录，不与基准比较权限（C5）；
            // 若当前权限缺少 0o700，则按宿主当前权限在阶段 1 放宽、阶段 5 恢复原宿主权限。
            match inspect_host_raw_at(host_root_fd, &anc).map_err(|err| {
                PublishPrepareError::Failed(format!(
                    "检查宿主上级目录 `{}` 失败：{err}",
                    anc.display()
                ))
            })? {
                RawHostEntry::Regular(h) if h.kind == EntryKind::Dir => {
                    let cur_mode = h.mode & 0o7777;
                    if cur_mode & 0o700 != 0o700 {
                        host_dir_ids.insert(
                            anc,
                            (
                                FileId {
                                    dev: h.stat.dev,
                                    ino: h.stat.ino,
                                },
                                cur_mode,
                            ),
                        );
                    }
                }
                _ => {
                    // 上级目录在检查窗口内消失或被换成非目录：把本批位于其下的路径报为宿主已改。
                    for item in &items {
                        if item.rel_path.starts_with(&anc) {
                            conflicts.push(PublishConflict {
                                slot_idx,
                                handle: handle.clone(),
                                rel: item.rel_utf8.clone(),
                                reason: ConflictReason::HostChanged,
                            });
                        }
                    }
                }
            }
        }
    }

    items.sort_by(|a, b| a.rel_utf8.cmp(&b.rel_utf8));
    Ok(RootPlan {
        items,
        host_dir_ids,
    })
}

fn host_dir_has_undeleted_child(
    host_root_fd: BorrowedFd<'_>,
    rel_dir: &Path,
    paths_deleted_in_batch: &BTreeSet<PathBuf>,
) -> io::Result<bool> {
    let Some(dir_fd) = open_dir_nofollow(host_root_fd, rel_dir)? else {
        return Ok(false);
    };
    let mut dir = Dir::new(dir_fd).map_err(io::Error::from)?;
    while let Some(next) = dir.read() {
        let entry = next.map_err(io::Error::from)?;
        let bytes = entry.file_name().to_bytes();
        if bytes == b"." || bytes == b".." {
            continue;
        }
        let child_rel = rel_dir.join(OsStr::from_bytes(bytes));
        if !paths_deleted_in_batch.contains(&child_rel) {
            return Ok(true);
        }
    }
    Ok(false)
}

#[allow(clippy::too_many_arguments)]
fn build_root_ops_and_stage(
    root_idx: usize,
    call_hash: &str,
    tree_root_fd: BorrowedFd<'_>,
    stage_fd: BorrowedFd<'_>,
    base_stage_fd: BorrowedFd<'_>,
    plan: RootPlan,
    next_op_id: &mut u64,
    next_base_slot: &mut u64,
    ops: &mut Vec<JournalOp>,
    paths: &mut Vec<BatchPath>,
    deadline: Option<Instant>,
    cancelled: Option<&AtomicBool>,
) -> Result<(), PublishPrepareError> {
    let short_hash = &call_hash[..call_hash.len().min(16)];

    // 记录每个批次目录（Dir -> Dir）的目标最终权限，供阶段 1 与阶段 5 协调。
    let mut batch_dir_target_mode: BTreeMap<PathBuf, u32> = BTreeMap::new();
    let mut batch_dir_removed_or_replaced: BTreeSet<PathBuf> = BTreeSet::new();
    for item in &plan.items {
        if matches!(item.before, Some(ObjectState::Dir { .. })) {
            match item.after {
                Some(ObjectState::Dir { mode }) => {
                    batch_dir_target_mode.insert(item.rel_path.clone(), mode);
                }
                _ => {
                    batch_dir_removed_or_replaced.insert(item.rel_path.clone());
                }
            }
        }
    }

    // 阶段 1（由浅到深）：放宽缺失 0o700 权限的已有宿主目录（只读祖先、待删/换型目录、待改权目录）。
    let mut phase1_dirs: Vec<(PathBuf, FileId, u32, u32)> = Vec::new();
    // 记录经过阶段 1 放宽后的当前权限：rel -> (FileId, orig_mode, widened_mode)。
    let mut widened_existing_dirs: BTreeMap<PathBuf, (FileId, u32, u32)> = BTreeMap::new();

    for (rel, &(id, orig_mode)) in &plan.host_dir_ids {
        if orig_mode & 0o700 == 0o700 {
            continue;
        }
        let target_mode = if let Some(&final_mode) = batch_dir_target_mode.get(rel)
            && final_mode & 0o700 == 0o700
        {
            final_mode
        } else {
            orig_mode | 0o700
        };
        phase1_dirs.push((rel.clone(), id, orig_mode, target_mode));
        widened_existing_dirs.insert(rel.clone(), (id, orig_mode, target_mode));
    }
    phase1_dirs.sort_by(|a, b| {
        path_depth(&a.0)
            .cmp(&path_depth(&b.0))
            .then_with(|| a.0.cmp(&b.0))
    });

    for (rel, id, orig_mode, target_mode) in phase1_dirs {
        let rel_utf8 = rel_path_to_utf8(&rel)
            .ok_or_else(|| PublishPrepareError::Failed("路径不是有效 UTF-8".to_owned()))?;
        let op_id = alloc_op_id(next_op_id);
        ops.push(JournalOp::ChmodDir {
            op_id,
            root_idx,
            rel: rel_utf8,
            before_mode: orig_mode,
            after_mode: target_mode,
            original_mode: None,
            target_id: id,
        });
    }

    // 分类批次项到阶段 2 / 3 / 4 / 5。
    let mut phase2_items: Vec<&PlannedItem> = Vec::new();
    let mut phase3_items: Vec<&PlannedItem> = Vec::new();
    let mut phase4_items: Vec<&PlannedItem> = Vec::new();
    let mut phase5_chmods: Vec<(PathBuf, String, u32, u32, Option<u32>, FileId)> = Vec::new();

    for item in &plan.items {
        if let Some(intr) = check_interrupt(deadline, cancelled) {
            return Err(PublishPrepareError::Interrupted(intr));
        }
        // A 节：对基准后态会变成普通文件或符号链接的项，提前在临界区外克隆/建立并落盘至 publish/<hash>/base/<slot>。
        let staged_base = match &item.after {
            Some(after @ (ObjectState::File { .. } | ObjectState::Symlink { .. })) => {
                let slot = alloc_op_id(next_base_slot);
                let id = stage_object(tree_root_fd, &item.rel_path, base_stage_fd, slot, after)?;
                Some(StagedBase { slot, id })
            }
            _ => None,
        };
        paths.push(BatchPath {
            root_idx,
            rel: item.rel_utf8.clone(),
            before: item.before.clone(),
            after: item.after.clone(),
            staged_base,
        });
        match (&item.before, &item.after) {
            (Some(_), None) => phase2_items.push(item),
            (Some(b), Some(a)) if b.is_dir() && !a.is_dir() => phase2_items.push(item),
            (None, Some(a)) if a.is_dir() => phase3_items.push(item),
            (Some(b), Some(a)) if !b.is_dir() && a.is_dir() => phase3_items.push(item),
            (
                Some(ObjectState::Dir { mode: before_mode }),
                Some(ObjectState::Dir { mode: after_mode }),
            ) => {
                let id = item.before_dir_id.expect("已有目录必有 FileId");
                if let Some(&(_, orig_mode, cur_mode)) = widened_existing_dirs.get(&item.rel_path) {
                    if cur_mode != *after_mode {
                        phase5_chmods.push((
                            item.rel_path.clone(),
                            item.rel_utf8.clone(),
                            cur_mode,
                            *after_mode,
                            Some(orig_mode),
                            id,
                        ));
                    }
                } else if before_mode != after_mode {
                    phase5_chmods.push((
                        item.rel_path.clone(),
                        item.rel_utf8.clone(),
                        *before_mode,
                        *after_mode,
                        None,
                        id,
                    ));
                }
            }
            (None, Some(_)) | (Some(_), Some(_)) => phase4_items.push(item),
            (None, None) => {}
        }
    }

    // 阶段 1 临时放宽但不在批次内（或未改权）的只读祖先目录，在阶段 5 恢复原始权限。
    for (rel, &(id, orig_mode, cur_mode)) in &widened_existing_dirs {
        if batch_dir_target_mode.contains_key(rel) || batch_dir_removed_or_replaced.contains(rel) {
            continue;
        }
        let rel_utf8 = rel_path_to_utf8(rel)
            .ok_or_else(|| PublishPrepareError::Failed("路径不是有效 UTF-8".to_owned()))?;
        phase5_chmods.push((
            rel.clone(),
            rel_utf8,
            cur_mode,
            orig_mode,
            Some(orig_mode),
            id,
        ));
    }

    // 阶段 2（由深到浅）：删除对象与目录换型为叶对象。
    phase2_items.sort_by(|a, b| {
        path_depth(&b.rel_path)
            .cmp(&path_depth(&a.rel_path))
            .then_with(|| a.rel_utf8.cmp(&b.rel_utf8))
    });
    for item in phase2_items {
        if let Some(intr) = check_interrupt(deadline, cancelled) {
            return Err(PublishPrepareError::Interrupted(intr));
        }
        let mut before = item.before.clone().expect("阶段 2 必有 before");
        if let ObjectState::Dir { mode } = &mut before
            && let Some(&(_, _, cur_mode)) = widened_existing_dirs.get(&item.rel_path)
        {
            *mode = cur_mode;
        }
        let op_id = alloc_op_id(next_op_id);
        match &item.after {
            None => {
                ops.push(JournalOp::Delete {
                    op_id,
                    root_idx,
                    rel: item.rel_utf8.clone(),
                    before,
                });
            }
            Some(after) => {
                let staged_id = stage_object(tree_root_fd, &item.rel_path, stage_fd, op_id, after)?;
                ops.push(JournalOp::Exchange {
                    op_id,
                    root_idx,
                    rel: item.rel_utf8.clone(),
                    staged_id,
                    host_tmp_name: format!(".lancet-pub-{short_hash}-{op_id}"),
                    before,
                    after: after.clone(),
                });
            }
        }
    }

    // 阶段 3（由浅到深）：新建目录与叶换型为目录（以 mode | 0o700 暂存，阶段 5 收紧）。
    phase3_items.sort_by(|a, b| {
        path_depth(&a.rel_path)
            .cmp(&path_depth(&b.rel_path))
            .then_with(|| a.rel_utf8.cmp(&b.rel_utf8))
    });
    for item in phase3_items {
        if let Some(intr) = check_interrupt(deadline, cancelled) {
            return Err(PublishPrepareError::Interrupted(intr));
        }
        let Some(ObjectState::Dir { mode: final_mode }) = item.after else {
            unreachable!("阶段 3 必为目标目录");
        };
        let writable_mode = final_mode | 0o700;
        let staged_after = ObjectState::Dir {
            mode: writable_mode,
        };
        let op_id = alloc_op_id(next_op_id);
        let staged_id = stage_object(tree_root_fd, &item.rel_path, stage_fd, op_id, &staged_after)?;
        match &item.before {
            None => {
                ops.push(JournalOp::Create {
                    op_id,
                    root_idx,
                    rel: item.rel_utf8.clone(),
                    staged_id,
                    after: staged_after,
                });
            }
            Some(before) => {
                ops.push(JournalOp::Exchange {
                    op_id,
                    root_idx,
                    rel: item.rel_utf8.clone(),
                    staged_id,
                    host_tmp_name: format!(".lancet-pub-{short_hash}-{op_id}"),
                    before: before.clone(),
                    after: staged_after,
                });
            }
        }
        if writable_mode != final_mode {
            phase5_chmods.push((
                item.rel_path.clone(),
                item.rel_utf8.clone(),
                writable_mode,
                final_mode,
                None,
                staged_id,
            ));
        }
    }

    // 阶段 4（路径升序）：叶新建与叶间交换。
    phase4_items.sort_by(|a, b| a.rel_utf8.cmp(&b.rel_utf8));
    for item in phase4_items {
        if let Some(intr) = check_interrupt(deadline, cancelled) {
            return Err(PublishPrepareError::Interrupted(intr));
        }
        let after = item.after.as_ref().expect("阶段 4 必有 after");
        let op_id = alloc_op_id(next_op_id);
        let staged_id = stage_object(tree_root_fd, &item.rel_path, stage_fd, op_id, after)?;
        match &item.before {
            None => {
                ops.push(JournalOp::Create {
                    op_id,
                    root_idx,
                    rel: item.rel_utf8.clone(),
                    staged_id,
                    after: after.clone(),
                });
            }
            Some(before) => {
                ops.push(JournalOp::Exchange {
                    op_id,
                    root_idx,
                    rel: item.rel_utf8.clone(),
                    staged_id,
                    host_tmp_name: format!(".lancet-pub-{short_hash}-{op_id}"),
                    before: before.clone(),
                    after: after.clone(),
                });
            }
        }
    }

    // 阶段 5（由深到浅）：目录收紧与最终权限设置。
    phase5_chmods.sort_by(|a, b| {
        path_depth(&b.0)
            .cmp(&path_depth(&a.0))
            .then_with(|| a.1.cmp(&b.1))
    });
    for (_, rel_utf8, before_mode, after_mode, original_mode, target_id) in phase5_chmods {
        let op_id = alloc_op_id(next_op_id);
        ops.push(JournalOp::ChmodDir {
            op_id,
            root_idx,
            rel: rel_utf8,
            before_mode,
            after_mode,
            original_mode,
            target_id,
        });
    }

    Ok(())
}

fn alloc_op_id(counter: &mut u64) -> u64 {
    let id = *counter;
    *counter += 1;
    id
}

fn stage_object(
    tree_root_fd: BorrowedFd<'_>,
    rel: &Path,
    stage_fd: BorrowedFd<'_>,
    op_id: u64,
    state: &ObjectState,
) -> Result<FileId, PublishPrepareError> {
    let op_name = op_id.to_string();
    match state {
        ObjectState::File { mode, size, .. } => {
            let Some((src_parent_owned, src_leaf)) = open_parent_nofollow(tree_root_fd, rel)
                .map_err(|e| {
                    PublishPrepareError::Failed(format!(
                        "打开影子上级目录 `{}` 失败：{e}",
                        rel.display()
                    ))
                })?
            else {
                return Err(PublishPrepareError::Failed(format!(
                    "暂存文件 `{}` 时影子路径消失",
                    rel.display()
                )));
            };
            let src_parent_fd = src_parent_owned.as_ref().map_or(tree_root_fd, AsFd::as_fd);
            let src_fd = openat(
                src_parent_fd,
                src_leaf,
                OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            )
            .map_err(|e| {
                PublishPrepareError::Failed(format!(
                    "打开影子文件 `{}` 失败：{}",
                    rel.display(),
                    io::Error::from(e)
                ))
            })?;

            let dst_fd = openat(
                stage_fd,
                &op_name,
                OFlags::RDWR | OFlags::CREATE | OFlags::EXCL | OFlags::CLOEXEC,
                Mode::RUSR | Mode::WUSR,
            )
            .map_err(|e| {
                PublishPrepareError::Failed(format!(
                    "创建暂存文件 `{op_name}` 失败：{}",
                    io::Error::from(e)
                ))
            })?;
            if *size > 0 {
                ioctl_ficlone(&dst_fd, &src_fd).map_err(|e| {
                    PublishPrepareError::Failed(format!(
                        "FICLONE 暂存文件 `{}` 失败：{}",
                        rel.display(),
                        io::Error::from(e)
                    ))
                })?;
            }
            fchmod(&dst_fd, Mode::from_raw_mode(*mode & 0o7777)).map_err(|e| {
                PublishPrepareError::Failed(format!(
                    "设置暂存文件 `{}` 权限失败：{}",
                    rel.display(),
                    io::Error::from(e)
                ))
            })?;
            rustix::fs::fsync(&dst_fd).map_err(|e| {
                PublishPrepareError::Failed(format!(
                    "刷盘暂存文件 `{}` 失败：{}",
                    rel.display(),
                    io::Error::from(e)
                ))
            })?;
            let st = fstat(&dst_fd).map_err(|e| {
                PublishPrepareError::Failed(format!(
                    "读取暂存文件 `{}` 属性失败：{}",
                    rel.display(),
                    io::Error::from(e)
                ))
            })?;
            Ok(FileId {
                dev: st.st_dev,
                ino: st.st_ino,
            })
        }
        ObjectState::Dir { mode } => {
            let dir_mode = Mode::from_raw_mode(*mode & 0o7777);
            mkdirat(stage_fd, &op_name, dir_mode).map_err(|e| {
                PublishPrepareError::Failed(format!(
                    "创建暂存目录 `{}` 失败：{}",
                    rel.display(),
                    io::Error::from(e)
                ))
            })?;
            chmodat(stage_fd, &op_name, dir_mode, AtFlags::empty()).map_err(|e| {
                PublishPrepareError::Failed(format!(
                    "设置暂存目录 `{}` 权限失败：{}",
                    rel.display(),
                    io::Error::from(e)
                ))
            })?;
            let dir_fd = openat(
                stage_fd,
                &op_name,
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            )
            .map_err(|e| {
                PublishPrepareError::Failed(format!(
                    "打开暂存目录 `{}` 失败：{}",
                    rel.display(),
                    io::Error::from(e)
                ))
            })?;
            rustix::fs::fsync(&dir_fd).map_err(|e| {
                PublishPrepareError::Failed(format!(
                    "刷盘暂存目录 `{}` 失败：{}",
                    rel.display(),
                    io::Error::from(e)
                ))
            })?;
            let st = fstat(&dir_fd).map_err(|e| {
                PublishPrepareError::Failed(format!(
                    "读取暂存目录 `{}` 属性失败：{}",
                    rel.display(),
                    io::Error::from(e)
                ))
            })?;
            Ok(FileId {
                dev: st.st_dev,
                ino: st.st_ino,
            })
        }
        ObjectState::Symlink { target } => {
            symlinkat(OsStr::from_bytes(target), stage_fd, &op_name).map_err(|e| {
                PublishPrepareError::Failed(format!(
                    "创建暂存符号链接 `{}` 失败：{}",
                    rel.display(),
                    io::Error::from(e)
                ))
            })?;
            let st = statat(stage_fd, &op_name, AtFlags::SYMLINK_NOFOLLOW).map_err(|e| {
                PublishPrepareError::Failed(format!(
                    "读取暂存符号链接 `{}` 属性失败：{}",
                    rel.display(),
                    io::Error::from(e)
                ))
            })?;
            Ok(FileId {
                dev: st.st_dev,
                ino: st.st_ino,
            })
        }
    }
}

#[derive(Debug)]
enum ApplyFailure {
    LateConflict { op_idx: usize, mutated_host: bool },
    IoError { mutated_host: bool, message: String },
}

fn apply_op_with_idx(
    idx: usize,
    op: &JournalOp,
    host_root_fd: BorrowedFd<'_>,
    stage_fd: BorrowedFd<'_>,
    pre_fd: BorrowedFd<'_>,
    touched_dirs: &mut BTreeSet<(usize, PathBuf)>,
) -> Result<(), ApplyFailure> {
    let rel_path = Path::new(op.rel());
    let parent_rel = rel_path
        .parent()
        .unwrap_or_else(|| Path::new(""))
        .to_path_buf();
    let root_idx = op.root_idx();
    let op_name = op.op_id().to_string();

    match op {
        JournalOp::ChmodDir {
            before_mode,
            after_mode,
            target_id,
            ..
        } => {
            let Some((parent_owned, leaf)) =
                open_parent_nofollow(host_root_fd, rel_path).map_err(|e| {
                    ApplyFailure::IoError {
                        mutated_host: false,
                        message: format!("打开宿主上级目录 `{}` 失败：{e}", op.rel()),
                    }
                })?
            else {
                return Err(ApplyFailure::LateConflict {
                    op_idx: idx,
                    mutated_host: false,
                });
            };
            let parent_fd = parent_owned.as_ref().map_or(host_root_fd, AsFd::as_fd);
            match openat(
                parent_fd,
                leaf,
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            ) {
                Ok(dir_fd) => {
                    let st = fstat(&dir_fd).map_err(|e| ApplyFailure::IoError {
                        mutated_host: false,
                        message: format!(
                            "读取宿主目录 `{}` 属性失败：{}",
                            op.rel(),
                            io::Error::from(e)
                        ),
                    })?;
                    if st.st_dev != target_id.dev
                        || st.st_ino != target_id.ino
                        || (st.st_mode & 0o7777) != (*before_mode & 0o7777)
                    {
                        return Err(ApplyFailure::LateConflict {
                            op_idx: idx,
                            mutated_host: false,
                        });
                    }
                    fchmod(&dir_fd, Mode::from_raw_mode(*after_mode & 0o7777)).map_err(|e| {
                        ApplyFailure::IoError {
                            mutated_host: false,
                            message: format!(
                                "修改宿主目录 `{}` 权限失败：{}",
                                op.rel(),
                                io::Error::from(e)
                            ),
                        }
                    })?;
                    // 先刷盘再关闭 fd，即使目标权限为 0000 也能安全落盘；
                    // 本目录（含阶段 2/3/4 在其中产生的目录项改动及本次权限变更）均已落盘，
                    // 从 touched_dirs 移除以避免批尾重复落盘（C8）。
                    rustix::fs::fsync(&dir_fd).map_err(|e| ApplyFailure::IoError {
                        mutated_host: true,
                        message: format!(
                            "刷盘宿主目录 `{}` 失败：{}",
                            op.rel(),
                            io::Error::from(e)
                        ),
                    })?;
                    touched_dirs.remove(&(root_idx, rel_path.to_path_buf()));
                    Ok(())
                }
                Err(Errno::NOENT | Errno::NOTDIR | Errno::LOOP) => {
                    Err(ApplyFailure::LateConflict {
                        op_idx: idx,
                        mutated_host: false,
                    })
                }
                Err(Errno::ACCESS) => {
                    let st = statat(parent_fd, leaf, AtFlags::SYMLINK_NOFOLLOW).map_err(|e| {
                        ApplyFailure::IoError {
                            mutated_host: false,
                            message: format!(
                                "读取受限宿主目录 `{}` 属性失败：{}",
                                op.rel(),
                                io::Error::from(e)
                            ),
                        }
                    })?;
                    if FileType::from_raw_mode(st.st_mode) != FileType::Directory
                        || st.st_dev != target_id.dev
                        || st.st_ino != target_id.ino
                        || (st.st_mode & 0o7777) != (*before_mode & 0o7777)
                    {
                        return Err(ApplyFailure::LateConflict {
                            op_idx: idx,
                            mutated_host: false,
                        });
                    }
                    chmodat(
                        parent_fd,
                        leaf,
                        Mode::from_raw_mode(*after_mode & 0o7777),
                        AtFlags::empty(),
                    )
                    .map_err(|e| ApplyFailure::IoError {
                        mutated_host: false,
                        message: format!(
                            "修改受限宿主目录 `{}` 权限失败：{}",
                            op.rel(),
                            io::Error::from(e)
                        ),
                    })?;
                    touched_dirs.insert((root_idx, rel_path.to_path_buf()));
                    Ok(())
                }
                Err(e) => Err(ApplyFailure::IoError {
                    mutated_host: false,
                    message: format!("打开宿主目录 `{}` 失败：{}", op.rel(), io::Error::from(e)),
                }),
            }
        }
        JournalOp::Create { .. } => {
            let Some((parent_owned, leaf)) =
                open_parent_nofollow(host_root_fd, rel_path).map_err(|e| {
                    ApplyFailure::IoError {
                        mutated_host: false,
                        message: format!("打开宿主上级目录 `{}` 失败：{e}", op.rel()),
                    }
                })?
            else {
                return Err(ApplyFailure::LateConflict {
                    op_idx: idx,
                    mutated_host: false,
                });
            };
            let parent_fd = parent_owned.as_ref().map_or(host_root_fd, AsFd::as_fd);
            match renameat_with(stage_fd, &op_name, parent_fd, leaf, RenameFlags::NOREPLACE) {
                Ok(()) => {
                    touched_dirs.insert((root_idx, parent_rel));
                    Ok(())
                }
                Err(
                    Errno::EXIST | Errno::NOTEMPTY | Errno::ISDIR | Errno::NOTDIR | Errno::NOENT,
                ) => Err(ApplyFailure::LateConflict {
                    op_idx: idx,
                    mutated_host: false,
                }),
                Err(e) => Err(ApplyFailure::IoError {
                    mutated_host: false,
                    message: format!("在宿主创建 `{}` 失败：{}", op.rel(), io::Error::from(e)),
                }),
            }
        }
        JournalOp::Delete { before, .. } => {
            let Some((parent_owned, leaf)) =
                open_parent_nofollow(host_root_fd, rel_path).map_err(|e| {
                    ApplyFailure::IoError {
                        mutated_host: false,
                        message: format!("打开宿主上级目录 `{}` 失败：{e}", op.rel()),
                    }
                })?
            else {
                return Err(ApplyFailure::LateConflict {
                    op_idx: idx,
                    mutated_host: false,
                });
            };
            let parent_fd = parent_owned.as_ref().map_or(host_root_fd, AsFd::as_fd);
            if before.is_dir() {
                let matched =
                    verify_object_at(parent_fd, leaf, before, None, true).map_err(|e| {
                        ApplyFailure::IoError {
                            mutated_host: false,
                            message: format!("检查待删宿主目录 `{}` 失败：{e}", op.rel()),
                        }
                    })?;
                if !matched {
                    return Err(ApplyFailure::LateConflict {
                        op_idx: idx,
                        mutated_host: false,
                    });
                }
            }
            match renameat_with(parent_fd, leaf, pre_fd, &op_name, RenameFlags::NOREPLACE) {
                Ok(()) => {
                    touched_dirs.insert((root_idx, parent_rel));
                }
                Err(Errno::NOENT | Errno::NOTDIR | Errno::ISDIR) => {
                    return Err(ApplyFailure::LateConflict {
                        op_idx: idx,
                        mutated_host: false,
                    });
                }
                Err(e) => {
                    return Err(ApplyFailure::IoError {
                        mutated_host: false,
                        message: format!(
                            "移除宿主路径 `{}` 失败：{}",
                            op.rel(),
                            io::Error::from(e)
                        ),
                    });
                }
            }
            // 移入私有 pre/ 后再次校验语义，捕获删除窗口内的晚到写入或非空目录。
            let verified =
                verify_object_at(pre_fd, OsStr::new(&op_name), before, None, before.is_dir())
                    .map_err(|e| ApplyFailure::IoError {
                        mutated_host: true,
                        message: format!("校验已移出宿主路径 `{}` 失败：{e}", op.rel()),
                    })?;
            if !verified {
                return Err(ApplyFailure::LateConflict {
                    op_idx: idx,
                    mutated_host: true,
                });
            }
            Ok(())
        }
        JournalOp::Exchange {
            host_tmp_name,
            before,
            ..
        } => {
            let Some((parent_owned, leaf)) =
                open_parent_nofollow(host_root_fd, rel_path).map_err(|e| {
                    ApplyFailure::IoError {
                        mutated_host: false,
                        message: format!("打开宿主上级目录 `{}` 失败：{e}", op.rel()),
                    }
                })?
            else {
                return Err(ApplyFailure::LateConflict {
                    op_idx: idx,
                    mutated_host: false,
                });
            };
            let parent_fd = parent_owned.as_ref().map_or(host_root_fd, AsFd::as_fd);
            if before.is_dir() {
                let matched =
                    verify_object_at(parent_fd, leaf, before, None, true).map_err(|e| {
                        ApplyFailure::IoError {
                            mutated_host: false,
                            message: format!("检查待换型宿主目录 `{}` 失败：{e}", op.rel()),
                        }
                    })?;
                if !matched {
                    return Err(ApplyFailure::LateConflict {
                        op_idx: idx,
                        mutated_host: false,
                    });
                }
            }

            // 步骤 A：把暂存对象移入宿主同目录临时名。
            match renameat_with(
                stage_fd,
                &op_name,
                parent_fd,
                host_tmp_name,
                RenameFlags::NOREPLACE,
            ) {
                Ok(()) => {
                    touched_dirs.insert((root_idx, parent_rel.clone()));
                }
                Err(Errno::EXIST) => {
                    return Err(ApplyFailure::LateConflict {
                        op_idx: idx,
                        mutated_host: false,
                    });
                }
                Err(e) => {
                    return Err(ApplyFailure::IoError {
                        mutated_host: false,
                        message: format!(
                            "移入宿主临时对象 `{}` 失败：{}",
                            op.rel(),
                            io::Error::from(e)
                        ),
                    });
                }
            }

            // 步骤 B：原子 RENAME_EXCHANGE 交换临时名与目标路径。
            if let Err(e) = renameat_with(
                parent_fd,
                host_tmp_name,
                parent_fd,
                leaf,
                RenameFlags::EXCHANGE,
            ) {
                // 交换未发生：退回暂存目录后再报错。
                let _ = renameat_with(
                    parent_fd,
                    host_tmp_name,
                    stage_fd,
                    &op_name,
                    RenameFlags::NOREPLACE,
                );
                return match e {
                    Errno::NOENT | Errno::NOTDIR | Errno::ISDIR => {
                        Err(ApplyFailure::LateConflict {
                            op_idx: idx,
                            mutated_host: false,
                        })
                    }
                    other => Err(ApplyFailure::IoError {
                        mutated_host: true,
                        message: format!(
                            "原子替换宿主路径 `{}` 失败：{}",
                            op.rel(),
                            io::Error::from(other)
                        ),
                    }),
                };
            }

            // 步骤 C：把换出的原宿主对象移入私有 pre/<op_id>。
            if let Err(e) = renameat_with(
                parent_fd,
                host_tmp_name,
                pre_fd,
                &op_name,
                RenameFlags::NOREPLACE,
            ) {
                return Err(ApplyFailure::IoError {
                    mutated_host: true,
                    message: format!("归档原宿主对象 `{}` 失败：{}", op.rel(), io::Error::from(e)),
                });
            }

            // 步骤 D：在私有 pre/ 中校验换出的原宿主对象仍与基准一致，捕获晚到写入。
            let verified =
                verify_object_at(pre_fd, OsStr::new(&op_name), before, None, before.is_dir())
                    .map_err(|e| ApplyFailure::IoError {
                        mutated_host: true,
                        message: format!("校验换出的原宿主对象 `{}` 失败：{e}", op.rel()),
                    })?;
            if !verified {
                return Err(ApplyFailure::LateConflict {
                    op_idx: idx,
                    mutated_host: true,
                });
            }
            Ok(())
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OpRollbackResult {
    NotExecuted,
    RolledBack,
    Published,
    KeptHostEdit,
}

struct RollbackOpOutcome {
    result: OpRollbackResult,
    /// 该目录已被宿主替换、删除或无法恢复原目录（其下子项不得再放回宿主目录，见 C1）。
    dir_replaced_by_host: bool,
}

struct RollbackSummary {
    has_host_stays: bool,
    dispositions: Vec<PathDispositionEntry>,
}

/// 按幂等状态逆序撤回 `ops`，并汇总每条 `BatchPath` 的最终去向。
fn rollback_ops(
    ops: &[JournalOp],
    paths: &[BatchPath],
    host_fds: &[BorrowedFd<'_>],
    stage_fd: BorrowedFd<'_>,
    pre_fd: BorrowedFd<'_>,
    touched_dirs: &mut BTreeSet<(usize, PathBuf)>,
) -> anyhow::Result<RollbackSummary> {
    let mut op_outcomes: Vec<OpRollbackResult> = vec![OpRollbackResult::NotExecuted; ops.len()];
    let mut stayed_paths: BTreeSet<(usize, PathBuf)> = BTreeSet::new();
    let mut kept_host_replaced_dirs: BTreeSet<(usize, PathBuf)> = BTreeSet::new();
    // B1：直接从 BatchPath.after 读取本批次目录的目标最终权限，
    // 确保实时部分撤回仅传入阶段 5 之前的操作前缀时，也能为标为“已发布”的目录恢复最终权限。
    let batch_dir_final_modes: BTreeMap<(usize, PathBuf), u32> = paths
        .iter()
        .filter_map(|bp| match bp.after {
            Some(ObjectState::Dir { mode }) => {
                Some(((bp.root_idx, PathBuf::from(&bp.rel)), mode & 0o7777))
            }
            _ => None,
        })
        .collect();

    for idx in (0..ops.len()).rev() {
        let op = &ops[idx];
        let outcome = rollback_single_op(
            idx,
            ops,
            host_fds[op.root_idx()],
            stage_fd,
            pre_fd,
            touched_dirs,
            &stayed_paths,
            &kept_host_replaced_dirs,
            &batch_dir_final_modes,
        )
        .with_context(|| format!("撤回操作 #{}（{}）失败", op.op_id(), op.rel()))?;
        op_outcomes[idx] = outcome.result;
        if matches!(
            outcome.result,
            OpRollbackResult::Published | OpRollbackResult::KeptHostEdit
        ) {
            stayed_paths.insert((op.root_idx(), PathBuf::from(op.rel())));
        }
        if outcome.dir_replaced_by_host {
            kept_host_replaced_dirs.insert((op.root_idx(), PathBuf::from(op.rel())));
        }
    }

    // 把每条操作结果归并到对应的 BatchPath（优先级：KeptHostEdit > Published > RolledBack > NotExecuted）。
    let mut per_path: BTreeMap<(usize, &str), PathDisposition> = BTreeMap::new();
    for (op, &res) in ops.iter().zip(op_outcomes.iter()) {
        let key = (op.root_idx(), op.rel());
        let mapped = match res {
            OpRollbackResult::KeptHostEdit => PathDisposition::KeptHostEdit,
            OpRollbackResult::Published => PathDisposition::Published,
            OpRollbackResult::RolledBack => PathDisposition::RolledBack,
            OpRollbackResult::NotExecuted => PathDisposition::NotExecuted,
        };
        let entry = per_path.entry(key).or_insert(PathDisposition::NotExecuted);
        *entry = merge_disposition(*entry, mapped);
    }

    let mut has_host_stays = false;
    let mut dispositions = Vec::with_capacity(paths.len());
    for bp in paths {
        let disp = per_path
            .get(&(bp.root_idx, bp.rel.as_str()))
            .copied()
            .unwrap_or(PathDisposition::NotExecuted);
        if disp.stays_on_host() {
            has_host_stays = true;
        }
        dispositions.push(PathDispositionEntry {
            root_idx: bp.root_idx,
            rel: bp.rel.clone(),
            disposition: disp,
        });
    }

    Ok(RollbackSummary {
        has_host_stays,
        dispositions,
    })
}

fn merge_disposition(cur: PathDisposition, next: PathDisposition) -> PathDisposition {
    match (cur, next) {
        (PathDisposition::KeptHostEdit, _) | (_, PathDisposition::KeptHostEdit) => {
            PathDisposition::KeptHostEdit
        }
        (PathDisposition::Published, _) | (_, PathDisposition::Published) => {
            PathDisposition::Published
        }
        (PathDisposition::RolledBack, _) | (_, PathDisposition::RolledBack) => {
            PathDisposition::RolledBack
        }
        (PathDisposition::NotExecuted, PathDisposition::NotExecuted) => {
            PathDisposition::NotExecuted
        }
    }
}

fn has_strict_ancestor_in(root_idx: usize, rel: &Path, set: &BTreeSet<(usize, PathBuf)>) -> bool {
    let mut cur = rel.parent();
    while let Some(anc) = cur {
        if anc.as_os_str().is_empty() {
            break;
        }
        if set.contains(&(root_idx, anc.to_path_buf())) {
            return true;
        }
        cur = anc.parent();
    }
    false
}

fn op_involves_dir(op: &JournalOp) -> bool {
    match op {
        JournalOp::ChmodDir { .. } => true,
        JournalOp::Create { after, .. } => after.is_dir(),
        JournalOp::Delete { before, .. } => before.is_dir(),
        JournalOp::Exchange { before, after, .. } => before.is_dir() || after.is_dir(),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DirChildrenState {
    Empty,
    OnlyStayedBatchChildren,
    HasForeignChildren,
}

#[allow(clippy::too_many_arguments)]
fn classify_dir_children(
    parent_fd: BorrowedFd<'_>,
    leaf: &OsStr,
    expected: &ObjectState,
    target_id: FileId,
    root_idx: usize,
    rel_dir: &Path,
    stayed_paths: &BTreeSet<(usize, PathBuf)>,
) -> io::Result<DirChildrenState> {
    let dir_fd = match openat(
        parent_fd,
        leaf,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    ) {
        Ok(fd) => fd,
        Err(Errno::NOENT | Errno::NOTDIR | Errno::LOOP) => {
            return Ok(DirChildrenState::HasForeignChildren);
        }
        Err(Errno::ACCESS) => {
            // 宿主可能在撤回期间改掉目录权限（如 0111），使 O_RDONLY 打开得到 EACCES：
            // 复核身份与临时权限，已变则按宿主改动处理，仍匹配才上抛。
            if !verify_object_at(parent_fd, leaf, expected, Some(target_id), false)? {
                return Ok(DirChildrenState::HasForeignChildren);
            }
            return Err(io::Error::from(Errno::ACCESS));
        }
        Err(err) => return Err(io::Error::from(err)),
    };
    let mut dir = Dir::new(dir_fd).map_err(io::Error::from)?;
    let mut saw_any = false;
    while let Some(next) = dir.read() {
        let entry = next.map_err(io::Error::from)?;
        let bytes = entry.file_name().to_bytes();
        if bytes == b"." || bytes == b".." {
            continue;
        }
        saw_any = true;
        let child_rel = rel_dir.join(OsStr::from_bytes(bytes));
        if !stayed_paths.contains(&(root_idx, child_rel)) {
            return Ok(DirChildrenState::HasForeignChildren);
        }
    }
    if saw_any {
        Ok(DirChildrenState::OnlyStayedBatchChildren)
    } else {
        Ok(DirChildrenState::Empty)
    }
}

#[allow(clippy::too_many_arguments)]
fn rollback_single_op(
    idx: usize,
    all_ops: &[JournalOp],
    host_root_fd: BorrowedFd<'_>,
    stage_fd: BorrowedFd<'_>,
    pre_fd: BorrowedFd<'_>,
    touched_dirs: &mut BTreeSet<(usize, PathBuf)>,
    stayed_paths: &BTreeSet<(usize, PathBuf)>,
    kept_host_replaced_dirs: &BTreeSet<(usize, PathBuf)>,
    batch_dir_final_modes: &BTreeMap<(usize, PathBuf), u32>,
) -> io::Result<RollbackOpOutcome> {
    let op = &all_ops[idx];
    let rel_path = Path::new(op.rel());
    let parent_rel = rel_path
        .parent()
        .unwrap_or_else(|| Path::new(""))
        .to_path_buf();
    let root_idx = op.root_idx();
    let op_name = op.op_id().to_string();

    // C1：若上级目录已保留宿主对象，绝不把子项放回宿主对象或修改其下权限；
    // 已执行过的子项处置同样记为保留宿主修改。
    if has_strict_ancestor_in(root_idx, rel_path, kept_host_replaced_dirs) {
        let was_executed = match op {
            JournalOp::Delete { .. } => entry_exists_at(pre_fd, &op_name)?,
            JournalOp::Create { .. } | JournalOp::Exchange { .. } => {
                !entry_exists_at(stage_fd, &op_name)?
            }
            JournalOp::ChmodDir { .. } => {
                // 仅当该路径没有其他同路径操作（例如仅改权的目录）时标记为保留宿主修改。
                !all_ops.iter().enumerate().any(|(i, other)| {
                    i != idx
                        && other.root_idx() == root_idx
                        && other.rel() == op.rel()
                        && !matches!(other, JournalOp::ChmodDir { .. })
                })
            }
        };
        return if was_executed {
            Ok(RollbackOpOutcome {
                result: OpRollbackResult::KeptHostEdit,
                dir_replaced_by_host: op_involves_dir(op),
            })
        } else {
            Ok(RollbackOpOutcome {
                result: OpRollbackResult::NotExecuted,
                dir_replaced_by_host: false,
            })
        };
    }

    match op {
        JournalOp::ChmodDir {
            before_mode,
            after_mode,
            original_mode,
            target_id,
            ..
        } => {
            // B2：若该 ChmodDir 针对阶段 3 新建/换型生成的目录，必须综合 stage/、宿主临时名与 pre/
            // 核实阶段 3 是否真的把目标目录安装到了目标路径上；未安装到目标上时绝不能把目标处原宿主对象判成“保留宿主修改”。
            let phase3_creator = all_ops[..idx].iter().find(|prev| match prev {
                JournalOp::Create {
                    root_idx: pr,
                    rel: prel,
                    staged_id,
                    ..
                }
                | JournalOp::Exchange {
                    root_idx: pr,
                    rel: prel,
                    staged_id,
                    ..
                } => *pr == root_idx && prel == op.rel() && *staged_id == *target_id,
                _ => false,
            });

            if let Some(creator) = phase3_creator {
                match creator {
                    JournalOp::Create { op_id: pid, .. } => {
                        let pid_name = pid.to_string();
                        if entry_exists_at(stage_fd, &pid_name)?
                            || entry_exists_at(pre_fd, &pid_name)?
                        {
                            return Ok(RollbackOpOutcome {
                                result: OpRollbackResult::NotExecuted,
                                dir_replaced_by_host: false,
                            });
                        }
                    }
                    JournalOp::Exchange {
                        op_id: pid,
                        host_tmp_name,
                        ..
                    } => {
                        let pid_name = pid.to_string();
                        if entry_exists_at(stage_fd, &pid_name)? {
                            return Ok(RollbackOpOutcome {
                                result: OpRollbackResult::NotExecuted,
                                dir_replaced_by_host: false,
                            });
                        }
                        let parent_opt = open_parent_nofollow(host_root_fd, rel_path)?;
                        let tmp_state = if let Some((ref parent_owned, _)) = parent_opt {
                            let pfd = parent_owned.as_ref().map_or(host_root_fd, AsFd::as_fd);
                            match statat(pfd, host_tmp_name, AtFlags::SYMLINK_NOFOLLOW) {
                                Ok(st) => Some(FileId {
                                    dev: st.st_dev,
                                    ino: st.st_ino,
                                }),
                                Err(Errno::NOENT) => None,
                                Err(err) => return Err(io::Error::from(err)),
                            }
                        } else {
                            None
                        };
                        if tmp_state == Some(*target_id) {
                            // 崩溃点 1（或反向交换后尚未归还 stage）：staged 目录在宿主临时名中，尚未安装到目标。
                            return Ok(RollbackOpOutcome {
                                result: OpRollbackResult::NotExecuted,
                                dir_replaced_by_host: false,
                            });
                        }
                        if tmp_state.is_none() && !entry_exists_at(pre_fd, &pid_name)? {
                            // 缺少材料，交给阶段 3 Exchange 撤回时统一报错。
                            return Ok(RollbackOpOutcome {
                                result: OpRollbackResult::NotExecuted,
                                dir_replaced_by_host: false,
                            });
                        }
                    }
                    _ => {}
                }
            }

            let Some((parent_owned, leaf)) = open_parent_nofollow(host_root_fd, rel_path)? else {
                return Ok(RollbackOpOutcome {
                    result: OpRollbackResult::KeptHostEdit,
                    dir_replaced_by_host: true,
                });
            };
            let parent_fd = parent_owned.as_ref().map_or(host_root_fd, AsFd::as_fd);
            let st = match statat(parent_fd, leaf, AtFlags::SYMLINK_NOFOLLOW) {
                Ok(st) => st,
                Err(Errno::NOENT | Errno::NOTDIR) => {
                    return Ok(RollbackOpOutcome {
                        result: OpRollbackResult::KeptHostEdit,
                        dir_replaced_by_host: true,
                    });
                }
                Err(err) => return Err(io::Error::from(err)),
            };
            if FileType::from_raw_mode(st.st_mode) != FileType::Directory
                || st.st_dev != target_id.dev
                || st.st_ino != target_id.ino
            {
                return Ok(RollbackOpOutcome {
                    result: OpRollbackResult::KeptHostEdit,
                    dir_replaced_by_host: true,
                });
            }
            let cur_mode = st.st_mode & 0o7777;
            let bm = *before_mode & 0o7777;
            let am = *after_mode & 0o7777;

            if cur_mode == am {
                // 若该阶段 5 操作仅是未改动祖先目录的权限恢复（original_mode == Some(after_mode)），
                // 仅当其下存在已执行待撤回操作时才需先放宽回 before_mode。
                if *original_mode == Some(am)
                    && !has_executed_descendant_op(
                        &all_ops[..idx],
                        root_idx,
                        rel_path,
                        stage_fd,
                        pre_fd,
                    )
                {
                    return Ok(RollbackOpOutcome {
                        result: OpRollbackResult::NotExecuted,
                        dir_replaced_by_host: false,
                    });
                }
                set_exact_dir_mode_by_id(parent_fd, leaf, *target_id, bm)?;
                touched_dirs.remove(&(root_idx, rel_path.to_path_buf()));
                Ok(RollbackOpOutcome {
                    result: OpRollbackResult::RolledBack,
                    dir_replaced_by_host: false,
                })
            } else if cur_mode == bm || *original_mode == Some(cur_mode) {
                Ok(RollbackOpOutcome {
                    result: OpRollbackResult::NotExecuted,
                    dir_replaced_by_host: false,
                })
            } else {
                // 同一目录 inode 仍在，但宿主修改了其权限位。
                Ok(RollbackOpOutcome {
                    result: OpRollbackResult::KeptHostEdit,
                    dir_replaced_by_host: false,
                })
            }
        }
        JournalOp::Create {
            staged_id, after, ..
        } => {
            if entry_exists_at(stage_fd, &op_name)? {
                return Ok(RollbackOpOutcome {
                    result: OpRollbackResult::NotExecuted,
                    dir_replaced_by_host: false,
                });
            }
            // C6：恢复时对前态槽中撤回进来的新建项重做后置核对；
            // 若对象的身份、权限或内容不等于发布版本，无覆盖放回宿主并记为保留宿主修改。
            if entry_exists_at(pre_fd, &op_name)? {
                if verify_object_at(
                    pre_fd,
                    OsStr::new(&op_name),
                    after,
                    Some(*staged_id),
                    after.is_dir(),
                )? {
                    // B3：恢复重入时已有明确磁盘证据表明新建项曾被移入 pre/，
                    // 把宿主上级目录记入待落盘集合以防上次撤回在批尾落盘前中断。
                    touched_dirs.insert((root_idx, parent_rel));
                    return Ok(RollbackOpOutcome {
                        result: OpRollbackResult::RolledBack,
                        dir_replaced_by_host: false,
                    });
                }
                if let Some((parent_owned, leaf)) = open_parent_nofollow(host_root_fd, rel_path)? {
                    let parent_fd = parent_owned.as_ref().map_or(host_root_fd, AsFd::as_fd);
                    match renameat_with(pre_fd, &op_name, parent_fd, leaf, RenameFlags::NOREPLACE) {
                        Ok(()) => {
                            touched_dirs.insert((root_idx, parent_rel));
                        }
                        Err(Errno::EXIST | Errno::NOTEMPTY | Errno::NOTDIR | Errno::ISDIR) => {}
                        Err(err) => return Err(io::Error::from(err)),
                    }
                }
                return Ok(RollbackOpOutcome {
                    result: OpRollbackResult::KeptHostEdit,
                    dir_replaced_by_host: after.is_dir(),
                });
            }
            let Some((parent_owned, leaf)) = open_parent_nofollow(host_root_fd, rel_path)? else {
                return Ok(RollbackOpOutcome {
                    result: OpRollbackResult::KeptHostEdit,
                    dir_replaced_by_host: after.is_dir(),
                });
            };
            let parent_fd = parent_owned.as_ref().map_or(host_root_fd, AsFd::as_fd);
            if !verify_object_at(parent_fd, leaf, after, Some(*staged_id), false)? {
                return Ok(RollbackOpOutcome {
                    result: OpRollbackResult::KeptHostEdit,
                    dir_replaced_by_host: after.is_dir(),
                });
            }
            // C7 + B1：若新建目录自身身份与临时权限仍是发布后态，仅因保留了宿主修改的子项而无法删除，
            // 依据 BatchPath.after 恢复最终权限并落盘，处置为“已发布”而非“保留宿主修改”。
            if let ObjectState::Dir { mode: temp_mode } = after {
                match classify_dir_children(
                    parent_fd,
                    leaf,
                    after,
                    *staged_id,
                    root_idx,
                    rel_path,
                    stayed_paths,
                )? {
                    DirChildrenState::Empty => {}
                    DirChildrenState::OnlyStayedBatchChildren => {
                        let expected_temp = *temp_mode & 0o7777;
                        if let Some(&final_mode) =
                            batch_dir_final_modes.get(&(root_idx, rel_path.to_path_buf()))
                            && final_mode != expected_temp
                        {
                            if !restore_published_dir_mode_if_unchanged(
                                parent_fd,
                                leaf,
                                *staged_id,
                                expected_temp,
                                final_mode,
                            )? {
                                return Ok(RollbackOpOutcome {
                                    result: OpRollbackResult::KeptHostEdit,
                                    dir_replaced_by_host: false,
                                });
                            }
                            touched_dirs.remove(&(root_idx, rel_path.to_path_buf()));
                        }
                        return Ok(RollbackOpOutcome {
                            result: OpRollbackResult::Published,
                            dir_replaced_by_host: false,
                        });
                    }
                    DirChildrenState::HasForeignChildren => {
                        return Ok(RollbackOpOutcome {
                            result: OpRollbackResult::KeptHostEdit,
                            dir_replaced_by_host: false,
                        });
                    }
                }
            }
            match renameat_with(parent_fd, leaf, pre_fd, &op_name, RenameFlags::NOREPLACE) {
                Ok(()) => {
                    touched_dirs.insert((root_idx, parent_rel));
                }
                Err(Errno::NOENT | Errno::NOTDIR) => {
                    return Ok(RollbackOpOutcome {
                        result: OpRollbackResult::KeptHostEdit,
                        dir_replaced_by_host: after.is_dir(),
                    });
                }
                Err(err) => return Err(io::Error::from(err)),
            }
            // 移入私有 pre/ 后复检，防止撤回窗口内宿主刚写过该对象。
            if !verify_object_at(
                pre_fd,
                OsStr::new(&op_name),
                after,
                Some(*staged_id),
                after.is_dir(),
            )? {
                match renameat_with(pre_fd, &op_name, parent_fd, leaf, RenameFlags::NOREPLACE) {
                    Ok(()) | Err(Errno::EXIST | Errno::NOTEMPTY | Errno::NOTDIR | Errno::ISDIR) => {
                    }
                    Err(err) => return Err(io::Error::from(err)),
                }
                return Ok(RollbackOpOutcome {
                    result: OpRollbackResult::KeptHostEdit,
                    dir_replaced_by_host: after.is_dir(),
                });
            }
            Ok(RollbackOpOutcome {
                result: OpRollbackResult::RolledBack,
                dir_replaced_by_host: false,
            })
        }
        JournalOp::Delete { before, .. } => {
            if !entry_exists_at(pre_fd, &op_name)? {
                return Ok(RollbackOpOutcome {
                    result: OpRollbackResult::NotExecuted,
                    dir_replaced_by_host: false,
                });
            }
            let Some((parent_owned, leaf)) = open_parent_nofollow(host_root_fd, rel_path)? else {
                return Ok(RollbackOpOutcome {
                    result: OpRollbackResult::KeptHostEdit,
                    dir_replaced_by_host: before.is_dir(),
                });
            };
            let parent_fd = parent_owned.as_ref().map_or(host_root_fd, AsFd::as_fd);
            match renameat_with(pre_fd, &op_name, parent_fd, leaf, RenameFlags::NOREPLACE) {
                Ok(()) => {
                    touched_dirs.insert((root_idx, parent_rel));
                    Ok(RollbackOpOutcome {
                        result: OpRollbackResult::RolledBack,
                        dir_replaced_by_host: false,
                    })
                }
                Err(Errno::EXIST | Errno::NOTEMPTY | Errno::NOTDIR | Errno::ISDIR) => {
                    Ok(RollbackOpOutcome {
                        result: OpRollbackResult::KeptHostEdit,
                        dir_replaced_by_host: before.is_dir(),
                    })
                }
                Err(err) => Err(io::Error::from(err)),
            }
        }
        JournalOp::Exchange {
            staged_id,
            host_tmp_name,
            before,
            after,
            ..
        } => {
            if entry_exists_at(stage_fd, &op_name)? {
                return Ok(RollbackOpOutcome {
                    result: OpRollbackResult::NotExecuted,
                    dir_replaced_by_host: false,
                });
            }
            let Some((parent_owned, leaf)) = open_parent_nofollow(host_root_fd, rel_path)? else {
                return Ok(RollbackOpOutcome {
                    result: OpRollbackResult::KeptHostEdit,
                    dir_replaced_by_host: before.is_dir() || after.is_dir(),
                });
            };
            let parent_fd = parent_owned.as_ref().map_or(host_root_fd, AsFd::as_fd);

            // 检查宿主目录中是否遗留临时名（崩溃点 1 或崩溃点 2）。
            match statat(parent_fd, host_tmp_name, AtFlags::SYMLINK_NOFOLLOW) {
                Ok(tmp_st) => {
                    if tmp_st.st_dev == staged_id.dev && tmp_st.st_ino == staged_id.ino {
                        // 崩溃点 1（或反向交换后尚未归还 stage）：直接把暂存对象移回 stage/。
                        renameat_with(
                            parent_fd,
                            host_tmp_name,
                            stage_fd,
                            &op_name,
                            RenameFlags::NOREPLACE,
                        )
                        .map_err(io::Error::from)?;
                        touched_dirs.insert((root_idx, parent_rel));
                        return Ok(RollbackOpOutcome {
                            result: OpRollbackResult::RolledBack,
                            dir_replaced_by_host: false,
                        });
                    }
                    // 崩溃点 2（已交换但尚未归档至 pre/）：先归档到 pre/<op_id>，再继续按 pre/ 流程撤回。
                    renameat_with(
                        parent_fd,
                        host_tmp_name,
                        pre_fd,
                        &op_name,
                        RenameFlags::NOREPLACE,
                    )
                    .map_err(io::Error::from)?;
                    touched_dirs.insert((root_idx, parent_rel.clone()));
                }
                Err(Errno::NOENT) => {}
                Err(err) => return Err(io::Error::from(err)),
            }

            if !entry_exists_at(pre_fd, &op_name)? {
                return Err(io::Error::other(format!(
                    "交换操作 #{}（{}）缺少 stage/pre 对象，无法安全撤回",
                    op.op_id(),
                    op.rel()
                )));
            }

            // 校验当前宿主目标仍是本批次发布的 staged 对象且未被宿主改动。
            if !verify_object_at(parent_fd, leaf, after, Some(*staged_id), false)? {
                return Ok(RollbackOpOutcome {
                    result: OpRollbackResult::KeptHostEdit,
                    dir_replaced_by_host: before.is_dir() || after.is_dir(),
                });
            }
            if let ObjectState::Dir { mode: temp_mode } = after {
                match classify_dir_children(
                    parent_fd,
                    leaf,
                    after,
                    *staged_id,
                    root_idx,
                    rel_path,
                    stayed_paths,
                )? {
                    DirChildrenState::Empty => {}
                    DirChildrenState::OnlyStayedBatchChildren => {
                        let expected_temp = *temp_mode & 0o7777;
                        if let Some(&final_mode) =
                            batch_dir_final_modes.get(&(root_idx, rel_path.to_path_buf()))
                            && final_mode != expected_temp
                        {
                            if !restore_published_dir_mode_if_unchanged(
                                parent_fd,
                                leaf,
                                *staged_id,
                                expected_temp,
                                final_mode,
                            )? {
                                return Ok(RollbackOpOutcome {
                                    result: OpRollbackResult::KeptHostEdit,
                                    dir_replaced_by_host: false,
                                });
                            }
                            touched_dirs.remove(&(root_idx, rel_path.to_path_buf()));
                        }
                        return Ok(RollbackOpOutcome {
                            result: OpRollbackResult::Published,
                            dir_replaced_by_host: false,
                        });
                    }
                    DirChildrenState::HasForeignChildren => {
                        return Ok(RollbackOpOutcome {
                            result: OpRollbackResult::KeptHostEdit,
                            dir_replaced_by_host: false,
                        });
                    }
                }
            }

            // 把 pre/<op_id> 移回宿主临时名并原子换回目标路径。
            match renameat_with(
                pre_fd,
                &op_name,
                parent_fd,
                host_tmp_name,
                RenameFlags::NOREPLACE,
            ) {
                Ok(()) => {}
                Err(Errno::EXIST) => {
                    return Ok(RollbackOpOutcome {
                        result: OpRollbackResult::KeptHostEdit,
                        dir_replaced_by_host: before.is_dir() || after.is_dir(),
                    });
                }
                Err(err) => return Err(io::Error::from(err)),
            }

            if let Err(err) = renameat_with(
                parent_fd,
                host_tmp_name,
                parent_fd,
                leaf,
                RenameFlags::EXCHANGE,
            ) {
                let _ = renameat_with(
                    parent_fd,
                    host_tmp_name,
                    pre_fd,
                    &op_name,
                    RenameFlags::NOREPLACE,
                );
                return match err {
                    Errno::NOENT | Errno::NOTDIR | Errno::ISDIR => Ok(RollbackOpOutcome {
                        result: OpRollbackResult::KeptHostEdit,
                        dir_replaced_by_host: before.is_dir() || after.is_dir(),
                    }),
                    other => Err(io::Error::from(other)),
                };
            }
            touched_dirs.insert((root_idx, parent_rel));

            // 反向交换后复检换回来的对象仍与 staged 一致；若窗口内被宿主改过，立刻换回并保留宿主修改！
            if !verify_object_at(
                parent_fd,
                OsStr::new(host_tmp_name),
                after,
                Some(*staged_id),
                after.is_dir(),
            )? {
                renameat_with(
                    parent_fd,
                    host_tmp_name,
                    parent_fd,
                    leaf,
                    RenameFlags::EXCHANGE,
                )
                .map_err(io::Error::from)?;
                renameat_with(
                    parent_fd,
                    host_tmp_name,
                    pre_fd,
                    &op_name,
                    RenameFlags::NOREPLACE,
                )
                .map_err(io::Error::from)?;
                return Ok(RollbackOpOutcome {
                    result: OpRollbackResult::KeptHostEdit,
                    dir_replaced_by_host: before.is_dir() || after.is_dir(),
                });
            }

            renameat_with(
                parent_fd,
                host_tmp_name,
                stage_fd,
                &op_name,
                RenameFlags::NOREPLACE,
            )
            .map_err(io::Error::from)?;
            Ok(RollbackOpOutcome {
                result: OpRollbackResult::RolledBack,
                dir_replaced_by_host: false,
            })
        }
    }
}

fn has_executed_descendant_op(
    earlier_ops: &[JournalOp],
    root_idx: usize,
    dir_rel: &Path,
    stage_fd: BorrowedFd<'_>,
    pre_fd: BorrowedFd<'_>,
) -> bool {
    for op in earlier_ops {
        if op.root_idx() != root_idx {
            continue;
        }
        let op_path = Path::new(op.rel());
        if op_path == dir_rel || !op_path.starts_with(dir_rel) {
            continue;
        }
        let op_name = op.op_id().to_string();
        let executed = match op {
            JournalOp::Create { .. } | JournalOp::Exchange { .. } => {
                !entry_exists_at(stage_fd, &op_name).unwrap_or(true)
            }
            JournalOp::Delete { .. } => entry_exists_at(pre_fd, &op_name).unwrap_or(false),
            JournalOp::ChmodDir { .. } => false,
        };
        if executed {
            return true;
        }
    }
    false
}

fn restore_published_dir_mode_if_unchanged(
    parent_fd: BorrowedFd<'_>,
    leaf: &OsStr,
    target_id: FileId,
    expected_mode: u32,
    final_mode: u32,
) -> io::Result<bool> {
    let dir_fd = match openat(
        parent_fd,
        leaf,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    ) {
        Ok(fd) => fd,
        Err(Errno::NOENT | Errno::NOTDIR | Errno::LOOP) => return Ok(false),
        Err(Errno::ACCESS) => {
            // 第 4 轮项 1：宿主可能在撤回枚举子项期间改掉目录权限（如 0111）导致 O_RDONLY 返回 EACCES；
            // 用 verify_object_at 跳过空目录检查复核身份与临时权限：已变则返回 Ok(false) 记为保留宿主修改，仍匹配才上抛。
            let expected = ObjectState::Dir {
                mode: expected_mode & 0o7777,
            };
            if !verify_object_at(parent_fd, leaf, &expected, Some(target_id), false)? {
                return Ok(false);
            }
            return Err(io::Error::from(Errno::ACCESS));
        }
        Err(err) => return Err(io::Error::from(err)),
    };
    let st = fstat(&dir_fd).map_err(io::Error::from)?;
    if st.st_dev != target_id.dev
        || st.st_ino != target_id.ino
        || (st.st_mode & 0o7777) != (expected_mode & 0o7777)
    {
        return Ok(false);
    }
    fchmod(&dir_fd, Mode::from_raw_mode(final_mode & 0o7777)).map_err(io::Error::from)?;
    rustix::fs::fsync(&dir_fd).map_err(io::Error::from)?;
    Ok(true)
}

fn set_exact_dir_mode_by_id(
    parent_fd: BorrowedFd<'_>,
    leaf: &OsStr,
    target_id: FileId,
    new_mode: u32,
) -> io::Result<()> {
    match openat(
        parent_fd,
        leaf,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    ) {
        Ok(dir_fd) => {
            let st = fstat(&dir_fd).map_err(io::Error::from)?;
            if st.st_dev == target_id.dev && st.st_ino == target_id.ino {
                fchmod(&dir_fd, Mode::from_raw_mode(new_mode & 0o7777)).map_err(io::Error::from)?;
                rustix::fs::fsync(&dir_fd).map_err(io::Error::from)?;
            }
            Ok(())
        }
        Err(Errno::ACCESS) => chmodat(
            parent_fd,
            leaf,
            Mode::from_raw_mode(new_mode & 0o7777),
            AtFlags::empty(),
        )
        .map_err(io::Error::from),
        Err(err) => Err(io::Error::from(err)),
    }
}

fn entry_exists_at(dir_fd: BorrowedFd<'_>, name: &str) -> io::Result<bool> {
    match statat(dir_fd, name, AtFlags::SYMLINK_NOFOLLOW) {
        Ok(_) => Ok(true),
        Err(Errno::NOENT) => Ok(false),
        Err(err) => Err(io::Error::from(err)),
    }
}

fn verify_object_at(
    parent_fd: BorrowedFd<'_>,
    leaf: &OsStr,
    expected: &ObjectState,
    expected_id: Option<FileId>,
    require_empty_dir: bool,
) -> io::Result<bool> {
    let st = match statat(parent_fd, leaf, AtFlags::SYMLINK_NOFOLLOW) {
        Ok(st) => st,
        Err(Errno::NOENT | Errno::NOTDIR) => return Ok(false),
        Err(err) => return Err(io::Error::from(err)),
    };
    if let Some(id) = expected_id
        && (st.st_dev != id.dev || st.st_ino != id.ino)
    {
        return Ok(false);
    }
    match expected {
        ObjectState::File { mode, size, sha256 } => {
            if FileType::from_raw_mode(st.st_mode) != FileType::RegularFile {
                return Ok(false);
            }
            if (st.st_mode & 0o7777) != (*mode & 0o7777) || (st.st_size.max(0) as u64) != *size {
                return Ok(false);
            }
            let fd = match openat(
                parent_fd,
                leaf,
                OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            ) {
                Ok(fd) => fd,
                Err(Errno::NOENT | Errno::LOOP) => return Ok(false),
                Err(err) => return Err(io::Error::from(err)),
            };
            let fst = fstat(&fd).map_err(io::Error::from)?;
            if fst.st_dev != st.st_dev
                || fst.st_ino != st.st_ino
                || (fst.st_mode & 0o7777) != (*mode & 0o7777)
                || (fst.st_size.max(0) as u64) != *size
            {
                return Ok(false);
            }
            if *size == 0 {
                return Ok(*sha256 == EMPTY_SHA256);
            }
            let mut file = File::from(fd);
            let actual = hash_reader(&mut file)?;
            Ok(actual == *sha256)
        }
        ObjectState::Dir { mode } => {
            if FileType::from_raw_mode(st.st_mode) != FileType::Directory {
                return Ok(false);
            }
            if (st.st_mode & 0o7777) != (*mode & 0o7777) {
                return Ok(false);
            }
            if require_empty_dir {
                let dir_fd = match openat(
                    parent_fd,
                    leaf,
                    OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                    Mode::empty(),
                ) {
                    Ok(fd) => fd,
                    Err(Errno::NOENT | Errno::NOTDIR | Errno::LOOP) => return Ok(false),
                    Err(err) => return Err(io::Error::from(err)),
                };
                let fst = fstat(&dir_fd).map_err(io::Error::from)?;
                if fst.st_dev != st.st_dev
                    || fst.st_ino != st.st_ino
                    || (fst.st_mode & 0o7777) != (*mode & 0o7777)
                {
                    return Ok(false);
                }
                let mut dir = Dir::new(dir_fd).map_err(io::Error::from)?;
                while let Some(next) = dir.read() {
                    let entry = next.map_err(io::Error::from)?;
                    let bytes = entry.file_name().to_bytes();
                    if bytes != b"." && bytes != b".." {
                        return Ok(false);
                    }
                }
            }
            Ok(true)
        }
        ObjectState::Symlink { target } => {
            if FileType::from_raw_mode(st.st_mode) != FileType::Symlink {
                return Ok(false);
            }
            let actual = match readlinkat(parent_fd, leaf, Vec::new()) {
                Ok(c) => c.into_bytes(),
                Err(Errno::NOENT | Errno::INVAL) => return Ok(false),
                Err(err) => return Err(io::Error::from(err)),
            };
            Ok(actual == *target)
        }
    }
}

fn sync_host_and_publish_dirs(
    host_fds: &[BorrowedFd<'_>],
    touched_dirs: &BTreeSet<(usize, PathBuf)>,
    stage_fd: BorrowedFd<'_>,
    pre_fd: BorrowedFd<'_>,
) -> io::Result<()> {
    for (root_idx, rel_dir) in touched_dirs {
        sync_rel_dir_at(host_fds[*root_idx], rel_dir)?;
    }
    sync_dir_fd(stage_fd)?;
    sync_dir_fd(pre_fd)?;
    Ok(())
}

fn sync_rel_dir_at(root_fd: BorrowedFd<'_>, rel_dir: &Path) -> io::Result<()> {
    match open_dir_nofollow(root_fd, rel_dir) {
        Ok(Some(fd)) => sync_dir_fd(fd.as_fd()),
        // 目录可能已在后续操作中被删除或权限收紧（收紧前已在 fchmod 时刷盘），跳过即可。
        Ok(None) => Ok(()),
        Err(err) if err.kind() == io::ErrorKind::PermissionDenied => Ok(()),
        Err(err) => Err(err),
    }
}

fn ensure_publish_root(publish_root: &Path, state_dir: &Path) -> anyhow::Result<()> {
    let created = match DirBuilder::new().mode(0o700).create(publish_root) {
        Ok(()) => true,
        Err(err) if err.kind() == io::ErrorKind::AlreadyExists => false,
        Err(err) => {
            return Err(err)
                .with_context(|| format!("创建发布根目录失败：{}", publish_root.display()));
        }
    };
    if created {
        let state_fd = open_syncable_dir_fd(state_dir)
            .with_context(|| format!("打开状态目录刷盘失败：{}", state_dir.display()))?;
        sync_dir_fd(state_fd.as_fd())
            .with_context(|| format!("刷盘状态目录失败：{}", state_dir.display()))?;
    }
    Ok(())
}

/// 删除 `<state_dir>/publish/<call_hash>`（若存在）并刷盘 `<state_dir>/publish`。
pub fn cleanup_publish_dir(state_dir: &Path, call_hash: &str) -> io::Result<()> {
    let publish_root = state_dir.join("publish");
    let call_dir = publish_root.join(call_hash);
    if call_dir.exists() || fs::symlink_metadata(&call_dir).is_ok() {
        remove_tree_force(&call_dir)?;
        if publish_root.is_dir() {
            let root_fd = open_syncable_dir_fd(&publish_root)?;
            sync_dir_fd(root_fd.as_fd())?;
        }
    }
    Ok(())
}

/// 过滤部分撤回时应推进基准的路径集合（C1）：
/// - 仅保留 `stays_on_host()`（`Published` | `KeptHostEdit`）路径；
/// - 若某目录路径（前后任一态为目录）处置为 `KeptHostEdit`，则该目录及其整棵子树的基准保持原样，
///   确保逐路径处置总能在同一棵基准树上实现。
fn filter_partial_base_paths<'a>(
    root_idx: usize,
    paths: &'a [BatchPath],
    dispositions: &[PathDispositionEntry],
) -> Vec<&'a BatchPath> {
    let disp_map: BTreeMap<(usize, &str), PathDisposition> = dispositions
        .iter()
        .map(|d| ((d.root_idx, d.rel.as_str()), d.disposition))
        .collect();

    let mut skip_subtrees: BTreeSet<(usize, PathBuf)> = BTreeSet::new();
    for bp in paths {
        if bp.root_idx != root_idx {
            continue;
        }
        let is_dir_path = bp.before.as_ref().is_some_and(ObjectState::is_dir)
            || bp.after.as_ref().is_some_and(ObjectState::is_dir);
        if is_dir_path
            && disp_map.get(&(root_idx, bp.rel.as_str())) == Some(&PathDisposition::KeptHostEdit)
        {
            skip_subtrees.insert((root_idx, PathBuf::from(&bp.rel)));
        }
    }

    paths
        .iter()
        .filter(|bp| {
            if bp.root_idx != root_idx {
                return false;
            }
            if !disp_map
                .get(&(root_idx, bp.rel.as_str()))
                .is_some_and(|d| d.stays_on_host())
            {
                return false;
            }
            let rel = PathBuf::from(&bp.rel);
            !skip_subtrees.contains(&(root_idx, rel.clone()))
                && !has_strict_ancestor_in(root_idx, &rel, &skip_subtrees)
        })
        .collect()
}

fn sync_base_stage_once(
    base_stage_fd: Option<BorrowedFd<'_>>,
    any_staged_base: bool,
) -> anyhow::Result<()> {
    if any_staged_base && let Some(bs_fd) = base_stage_fd {
        sync_dir_fd(bs_fd).context("刷盘基准暂存目录失败")?;
    }
    Ok(())
}

/// 推进已打开影子工作区的 `base/`、内存索引与后台扫描基线（全部提交）。
fn advance_live_shadows_committed(
    base_stage_fd: Option<BorrowedFd<'_>>,
    shadows: &[&ShadowDir],
    paths: &[BatchPath],
) -> anyhow::Result<()> {
    let mut any_staged_base = false;
    for (root_idx, shadow) in shadows.iter().enumerate() {
        let root_paths: Vec<&BatchPath> =
            paths.iter().filter(|bp| bp.root_idx == root_idx).collect();
        if root_paths.is_empty() {
            continue;
        }
        if root_paths.iter().any(|bp| bp.staged_base.is_some()) {
            any_staged_base = true;
        }
        let mut st = shadow.inner.lock_state();
        advance_single_live_shadow(base_stage_fd, shadow, &mut st, &root_paths)?;
    }
    sync_base_stage_once(base_stage_fd, any_staged_base)?;
    Ok(())
}

/// 部分撤回时仅推进可安全推进的保留路径之 `base/`，并把 `KeptHostEdit` 路径放入宿主延后候选。
fn advance_live_shadows_partial(
    base_stage_fd: Option<BorrowedFd<'_>>,
    shadows: &[&ShadowDir],
    paths: &[BatchPath],
    dispositions: &[PathDispositionEntry],
) -> anyhow::Result<()> {
    let disp_map: BTreeMap<(usize, &str), PathDisposition> = dispositions
        .iter()
        .map(|d| ((d.root_idx, d.rel.as_str()), d.disposition))
        .collect();

    let mut any_staged_base = false;
    for (root_idx, shadow) in shadows.iter().enumerate() {
        let advance_paths = filter_partial_base_paths(root_idx, paths, dispositions);
        let mut st = shadow.inner.lock_state();
        if !advance_paths.is_empty() {
            if advance_paths.iter().any(|bp| bp.staged_base.is_some()) {
                any_staged_base = true;
            }
            advance_single_live_shadow(base_stage_fd, shadow, &mut st, &advance_paths)?;
        }
        for bp in paths.iter().filter(|bp| bp.root_idx == root_idx) {
            if disp_map.get(&(root_idx, bp.rel.as_str())) == Some(&PathDisposition::KeptHostEdit) {
                st.deferred_host_candidates.insert(PathBuf::from(&bp.rel));
            }
        }
        let _ = shadow
            .inner
            .recompute_all_pending_conflicts_locked(&mut st, None, None);
    }
    sync_base_stage_once(base_stage_fd, any_staged_base)?;
    Ok(())
}

fn advance_single_live_shadow(
    base_stage_fd: Option<BorrowedFd<'_>>,
    shadow: &ShadowDir,
    st: &mut super::ShadowState,
    root_paths: &[&BatchPath],
) -> anyhow::Result<()> {
    let base_root_fd = shadow.inner.base_root_fd.as_fd();

    let updated = advance_base_on_disk_with_widened(
        base_stage_fd,
        base_root_fd,
        &mut st.widened,
        root_paths,
    )?;

    let mut baseline_patches = Vec::with_capacity(updated.len());
    for (rel, snap_opt) in updated {
        st.pending.remove(&rel);
        st.host_pending_obs.remove(&rel);
        match snap_opt {
            Some(snap) => {
                baseline_patches.push((rel.clone(), Some(BaselineMeta::from_snapshot(&snap))));
                st.base_index.insert(rel, snap);
            }
            None => {
                st.base_index.remove(&rel);
                baseline_patches.push((rel, None));
            }
        }
    }
    st.scanner.patch_baseline(baseline_patches);
    Ok(())
}

/// 在磁盘上按实况幂等地把 `root_paths` 的 `after` 状态推进到 `base/` 并刷盘（A、B1、C3、C8）：
/// - 仅从预准备的 `base_stage_fd` 改名安装文件/符号链接，绝不读取 `tree/`；
/// - Pass 1 按 `base/` 磁盘当前实际对象类型决定是否移除，已是目标目录时直接复用（B1）；
/// - Pass 3 严格传播所有 `fchmod`/`fsync`/`widened.persist` 错误，跨目录两端上级目录均落盘，
///   且同一目录在本阶段内仅落盘一次（先设目录最终权限再刷盘该目录，C3 + C8）。
fn advance_base_on_disk_with_widened(
    base_stage_fd: Option<BorrowedFd<'_>>,
    base_root_fd: BorrowedFd<'_>,
    widened: &mut WidenedLog,
    root_paths: &[&BatchPath],
) -> anyhow::Result<Vec<(PathBuf, Option<EntrySnapshot>)>> {
    let mut touched_base_dirs: BTreeSet<PathBuf> = BTreeSet::new();
    let mut removed_base_dirs: BTreeSet<PathBuf> = BTreeSet::new();
    let mut final_dir_modes: BTreeMap<PathBuf, u32> = BTreeMap::new();
    let mut results: BTreeMap<PathBuf, Option<EntrySnapshot>> = BTreeMap::new();

    // 1. 删除与异类清理（由深到浅），按磁盘上当前实际对象类型决定是否移除（支持任意重入，B1 + A1）。
    let mut pass1: Vec<&BatchPath> = root_paths.to_vec();
    pass1.sort_by(|a, b| {
        path_depth(Path::new(&b.rel))
            .cmp(&path_depth(Path::new(&a.rel)))
            .then_with(|| a.rel.cmp(&b.rel))
    });
    for bp in pass1 {
        let rel = PathBuf::from(&bp.rel);
        let parent = rel.parent().unwrap_or_else(|| Path::new("")).to_path_buf();
        let was_dir_now_not_dir = bp.before.as_ref().is_some_and(ObjectState::is_dir)
            && !bp.after.as_ref().is_some_and(ObjectState::is_dir);
        widened
            .widen_ancestors(TreeTag::Base, base_root_fd, &rel)
            .with_context(|| format!("放宽基准上级目录 `{}` 失败", bp.rel))?;
        let cur = inspect_lstat_at(base_root_fd, &rel)
            .with_context(|| format!("读取基准当前状态 `{}` 失败", bp.rel))?;
        let Some(cur_snap) = cur else {
            if was_dir_now_not_dir {
                removed_base_dirs.insert(rel.clone());
                touched_base_dirs.remove(&rel);
            }
            if bp.after.is_none() {
                touched_base_dirs.insert(parent);
                results.insert(rel, None);
            }
            continue;
        };
        let need_remove = match &bp.after {
            None => true,
            Some(after) => {
                // 仅当磁盘实际为目录而目标为非目录，或磁盘实际为非目录而目标为目录时需先移除；
                // 已是目标目录时直接复用，非目录之间由 Pass 2 的 renameat 原子覆盖。
                (cur_snap.kind == EntryKind::Dir) != after.is_dir()
            }
        };
        if !need_remove {
            if was_dir_now_not_dir {
                // A1：前态为目录、后态为叶项且已安装好时，同样记入已移除目录并仅落盘仍存在的上级目录。
                touched_base_dirs.remove(&rel);
                removed_base_dirs.insert(rel);
                touched_base_dirs.insert(parent);
            }
            continue;
        }
        remove_base_entry_keep_widened(base_root_fd, &rel, cur_snap.kind == EntryKind::Dir)
            .with_context(|| format!("移除基准路径 `{}` 失败", bp.rel))?;
        if cur_snap.kind == EntryKind::Dir {
            removed_base_dirs.insert(rel.clone());
            touched_base_dirs.remove(&rel);
        }
        touched_base_dirs.insert(parent);
        if bp.after.is_none() {
            results.insert(rel, None);
        }
    }

    // 2. 创建与修改（由浅到深）：目录按需新建并以 mode | 0o700 保持可写；
    //    文件与符号链接从 `base_stage_fd` 直接改名就位（重放时若槽已空则核验目标 FileId）。
    let mut pass2: Vec<&BatchPath> = root_paths.to_vec();
    pass2.sort_by(|a, b| {
        path_depth(Path::new(&a.rel))
            .cmp(&path_depth(Path::new(&b.rel)))
            .then_with(|| a.rel.cmp(&b.rel))
    });
    for bp in pass2 {
        let Some(after) = &bp.after else {
            continue;
        };
        let rel = PathBuf::from(&bp.rel);
        let parent = rel.parent().unwrap_or_else(|| Path::new("")).to_path_buf();
        widened
            .widen_ancestors(TreeTag::Base, base_root_fd, &rel)
            .with_context(|| format!("放宽基准上级目录 `{}` 失败", bp.rel))?;

        match after {
            ObjectState::Dir { mode } => {
                let _created = ensure_base_dir_writable_at(base_root_fd, &rel, widened, *mode)
                    .with_context(|| format!("推进基准目录 `{}` 失败", bp.rel))?;
                removed_base_dirs.remove(&rel);
                final_dir_modes.insert(rel.clone(), *mode & 0o7777);
                touched_base_dirs.insert(parent);
                touched_base_dirs.insert(rel);
            }
            ObjectState::File { mode, size, sha256 } => {
                let stat = install_base_from_staged_at(
                    base_stage_fd,
                    base_root_fd,
                    &rel,
                    bp.staged_base,
                    FileType::RegularFile,
                )
                .with_context(|| format!("推进基准文件 `{}` 失败", bp.rel))?;
                touched_base_dirs.insert(parent);
                results.insert(
                    rel,
                    Some(EntrySnapshot {
                        kind: EntryKind::File,
                        mode: *mode & 0o7777,
                        size: *size,
                        hash: Some(*sha256),
                        link_target: None,
                        stat,
                    }),
                );
            }
            ObjectState::Symlink { target } => {
                let stat = install_base_from_staged_at(
                    base_stage_fd,
                    base_root_fd,
                    &rel,
                    bp.staged_base,
                    FileType::Symlink,
                )
                .with_context(|| format!("推进基准符号链接 `{}` 失败", bp.rel))?;
                touched_base_dirs.insert(parent);
                results.insert(
                    rel,
                    Some(EntrySnapshot {
                        kind: EntryKind::Symlink,
                        mode: stat.mode,
                        size: stat.size,
                        hash: None,
                        link_target: Some(target.clone()),
                        stat,
                    }),
                );
            }
        }
    }

    // 3. 落盘与设权（C3 + C8；基准暂存源目录 `base_stage_fd` 由整批各根推进完后统一落盘一次，见 B2）：
    //    - 收集需要设置最终权限/恢复放宽的目录（`chmod_plan`）；
    //    - 先趁所有祖先仍处于放宽态，刷盘不在 `chmod_plan` 中的受影响目录（每目录仅一次）；
    //    - 再由深到浅对 `chmod_plan` 中每个目录 `open -> fchmod -> fsync(&dir_fd)`（每目录仅一次）；
    //    - 最后清除 `widened` 中的 Base 记录并持久落盘；全部错误严格向上传播。
    let mut chmod_plan: BTreeMap<PathBuf, u32> = BTreeMap::new();
    for (rel, orig_mode) in widened.base_active_modes() {
        if !removed_base_dirs.contains(&rel) {
            chmod_plan.insert(rel, orig_mode & 0o7777);
        }
    }
    for (rel, &final_mode) in &final_dir_modes {
        chmod_plan.insert(rel.clone(), final_mode & 0o7777);
    }

    let mut plain_sync_dirs: Vec<PathBuf> = touched_base_dirs
        .into_iter()
        .filter(|d| !removed_base_dirs.contains(d) && !chmod_plan.contains_key(d))
        .collect();
    plain_sync_dirs.sort_by(|a, b| path_depth(b).cmp(&path_depth(a)).then_with(|| a.cmp(b)));
    for rel_dir in plain_sync_dirs {
        sync_base_dir_strict_at(base_root_fd, &rel_dir)
            .with_context(|| format!("刷盘基准目录 `{}` 失败", rel_dir.display()))?;
    }

    let mut chmod_ordered: Vec<(PathBuf, u32)> = chmod_plan.into_iter().collect();
    chmod_ordered
        .sort_by(|(a, _), (b, _)| path_depth(b).cmp(&path_depth(a)).then_with(|| a.cmp(b)));
    for (rel, mode) in chmod_ordered {
        let dir_fd = open_dir_nofollow(base_root_fd, &rel)
            .with_context(|| format!("打开基准目录 `{}` 设权刷盘失败", rel.display()))?
            .ok_or_else(|| anyhow::anyhow!("基准目录 `{}` 在设权刷盘前消失", rel.display()))?;
        fchmod(&dir_fd, Mode::from_raw_mode(mode & 0o7777))
            .map_err(io::Error::from)
            .with_context(|| format!("设置基准目录 `{}` 权限 {mode:o} 失败", rel.display()))?;
        rustix::fs::fsync(&dir_fd)
            .map_err(io::Error::from)
            .with_context(|| format!("刷盘已设权基准目录 `{}` 失败", rel.display()))?;
        if final_dir_modes.contains_key(&rel) {
            let st = fstat(&dir_fd)
                .map_err(io::Error::from)
                .with_context(|| format!("读取基准目录 `{}` 属性失败", rel.display()))?;
            let mut stat = StatSig::from_stat(&st);
            stat.mode = mode & 0o7777;
            results.insert(
                rel,
                Some(EntrySnapshot {
                    kind: EntryKind::Dir,
                    mode: mode & 0o7777,
                    size: stat.size,
                    hash: None,
                    link_target: None,
                    stat,
                }),
            );
        }
    }

    widened
        .clear_base_active_checked()
        .context("清除基准权限放宽记录失败")?;

    Ok(results.into_iter().collect())
}

fn remove_base_entry_keep_widened(
    base_root_fd: BorrowedFd<'_>,
    rel: &Path,
    is_dir: bool,
) -> io::Result<()> {
    let Some((parent_owned, leaf)) = open_parent_nofollow(base_root_fd, rel)? else {
        return Ok(());
    };
    let parent_fd = parent_owned.as_ref().map_or(base_root_fd, AsFd::as_fd);
    let flags = if is_dir {
        AtFlags::REMOVEDIR
    } else {
        AtFlags::empty()
    };
    match unlinkat(parent_fd, leaf, flags) {
        Ok(()) | Err(Errno::NOENT) => Ok(()),
        Err(err) => Err(io::Error::from(err)),
    }
}

fn ensure_base_dir_writable_at(
    base_root_fd: BorrowedFd<'_>,
    rel: &Path,
    widened: &mut WidenedLog,
    final_mode: u32,
) -> io::Result<bool> {
    let Some((parent_owned, leaf)) = open_parent_nofollow(base_root_fd, rel)? else {
        return Err(io::Error::from(Errno::NOENT));
    };
    let parent_fd = parent_owned.as_ref().map_or(base_root_fd, AsFd::as_fd);
    let writable_mode = Mode::from_raw_mode((final_mode & 0o7777) | 0o700);
    widened.register_deferred_mode(TreeTag::Base, rel, final_mode)?;

    match statat(parent_fd, leaf, AtFlags::SYMLINK_NOFOLLOW) {
        Ok(st) if FileType::from_raw_mode(st.st_mode) == FileType::Directory => {
            chmodat(parent_fd, leaf, writable_mode, AtFlags::empty()).map_err(io::Error::from)?;
            Ok(false)
        }
        Err(Errno::NOENT) => {
            mkdirat(parent_fd, leaf, writable_mode).map_err(io::Error::from)?;
            chmodat(parent_fd, leaf, writable_mode, AtFlags::empty()).map_err(io::Error::from)?;
            Ok(true)
        }
        Ok(_) => Err(io::Error::from(Errno::EXIST)),
        Err(err) => Err(io::Error::from(err)),
    }
}

fn install_base_from_staged_at(
    base_stage_fd: Option<BorrowedFd<'_>>,
    base_root_fd: BorrowedFd<'_>,
    rel: &Path,
    staged_base: Option<StagedBase>,
    expected_ft: FileType,
) -> anyhow::Result<StatSig> {
    let sb = staged_base.ok_or_else(|| {
        anyhow::anyhow!("日志路径 `{}` 缺少预准备的基准后态对象身份", rel.display())
    })?;
    let Some((dst_parent_owned, dst_leaf)) = open_parent_nofollow(base_root_fd, rel)? else {
        anyhow::bail!("基准上级目录 `{}` 不存在", rel.display());
    };
    let dst_parent_fd = dst_parent_owned.as_ref().map_or(base_root_fd, AsFd::as_fd);
    let slot_name = sb.slot.to_string();

    if let Some(bs_fd) = base_stage_fd {
        match statat(bs_fd, &slot_name, AtFlags::SYMLINK_NOFOLLOW) {
            Ok(st) => {
                if st.st_dev != sb.id.dev
                    || st.st_ino != sb.id.ino
                    || FileType::from_raw_mode(st.st_mode) != expected_ft
                {
                    anyhow::bail!(
                        "基准暂存对象 `{slot_name}` 与日志记录的身份不匹配（路径 `{}`）",
                        rel.display()
                    );
                }
                renameat(bs_fd, &slot_name, dst_parent_fd, dst_leaf).map_err(io::Error::from)?;
                return Ok(StatSig::from_stat(&st));
            }
            Err(Errno::NOENT) => {}
            Err(err) => return Err(io::Error::from(err).into()),
        }
    }

    // 重入恢复（包含在清理 publish/<hash> 之后、写 Settled 之前崩溃的窗口）：
    // 基准暂存目录或槽位已不在，核验目标处对象身份是否已是日志记录的 StagedBase。
    let dst_st = statat(dst_parent_fd, dst_leaf, AtFlags::SYMLINK_NOFOLLOW)
        .map_err(io::Error::from)
        .with_context(|| {
            format!(
                "基准暂存槽 `{slot_name}` 不存在且目标 `{}` 无法读取",
                rel.display()
            )
        })?;
    if dst_st.st_dev != sb.id.dev
        || dst_st.st_ino != sb.id.ino
        || FileType::from_raw_mode(dst_st.st_mode) != expected_ft
    {
        anyhow::bail!(
            "基准暂存槽 `{slot_name}` 缺失且目标 `{}` 身份与日志不匹配",
            rel.display()
        );
    }
    Ok(StatSig::from_stat(&dst_st))
}

fn sync_base_dir_strict_at(base_root_fd: BorrowedFd<'_>, rel_dir: &Path) -> io::Result<()> {
    let Some(fd) = open_dir_nofollow(base_root_fd, rel_dir)? else {
        return Err(io::Error::from(Errno::NOENT));
    };
    sync_dir_fd(fd.as_fd())
}

fn build_publish_effects(
    roots: &[JournalRoot],
    paths: &[BatchPath],
    filter_kept: Option<&BTreeSet<(usize, &str)>>,
    diffs: Option<&BTreeMap<(usize, String), Option<String>>>,
) -> Vec<EffectEntry> {
    let mut by_root: Vec<Vec<Change>> = vec![Vec::new(); roots.len()];
    for bp in paths {
        if let Some(kept) = filter_kept
            && !kept.contains(&(bp.root_idx, bp.rel.as_str()))
        {
            continue;
        }
        let diff = diffs
            .and_then(|map| map.get(&(bp.root_idx, bp.rel.clone())))
            .and_then(Clone::clone);
        by_root[bp.root_idx].push(Change {
            target: bp.rel.clone(),
            action: "publish".to_owned(),
            diff,
            diff_truncated: false,
            residue: Residue::NotRetained,
        });
    }

    let mut out = Vec::new();
    for (root_idx, changes) in by_root.into_iter().enumerate() {
        if changes.is_empty() {
            continue;
        }
        out.push(
            EffectEntry::new(&roots[root_idx].handle, EffectKind::Write, changes)
                .expect("Write + NotRetained 组合必合法"),
        );
    }
    out
}

fn map_sync_err(err: ShadowSyncError) -> PublishPrepareError {
    match err {
        ShadowSyncError::Interrupted(intr) => PublishPrepareError::Interrupted(intr),
        ShadowSyncError::Failed(msg) => PublishPrepareError::Failed(msg),
    }
}

// =============================================================================
// 启动期崩溃恢复（在 ShadowDir::open 之前执行）
// =============================================================================

/// 启动期供发布事务崩溃恢复使用的可写目录根句柄（打开在 `ShadowDir::open` 之前）。
pub struct RecoveryRoot {
    pub handle: String,
    pub shadow_key: String,
    pub host_dev: u64,
    pub host_ino: u64,
    host_root_fd: OwnedFd,
    shadow_root: PathBuf,
}

impl RecoveryRoot {
    /// 打开配置中的单个可写工作目录；若已有影子目录则先清空 `tmp/` 并恢复 4a `widened` 权限放宽。
    pub fn open(
        state_dir: &Path,
        handle: &str,
        host_root: &Path,
        hidden: &[String],
        regenerable: &[String],
    ) -> anyhow::Result<Self> {
        let canonical = fs::canonicalize(host_root).with_context(|| {
            format!(
                "规范化可写工作目录 `{handle}` 宿主路径失败：{}",
                host_root.display()
            )
        })?;
        let shadow_key = compute_shadow_key(&canonical, hidden, regenerable);
        let host_root_fd = open_root_fd(&canonical)
            .with_context(|| format!("打开宿主工作目录 `{handle}` 根 fd 失败"))?;
        let st = fstat(&host_root_fd)
            .map_err(io::Error::from)
            .with_context(|| format!("读取宿主工作目录 `{handle}` 根属性失败"))?;

        let shadow_root = state_dir.join("shadows").join(&shadow_key);
        if shadow_root.is_dir() {
            let tree_root = shadow_root.join("tree");
            let base_root = shadow_root.join("base");
            let tmp_root = shadow_root.join("tmp");
            if tree_root.is_dir() && base_root.is_dir() {
                clear_or_create_tmp_dir(&tmp_root)
                    .with_context(|| format!("清空影子临时目录 {} 失败", tmp_root.display()))?;
                let tree_fd = open_root_fd(&tree_root)
                    .with_context(|| format!("打开影子 tree 目录 {} 失败", tree_root.display()))?;
                let base_fd = open_root_fd(&base_root)
                    .with_context(|| format!("打开影子 base 目录 {} 失败", base_root.display()))?;
                recover_widened_modes(&shadow_root, tree_fd.as_fd(), base_fd.as_fd())
                    .with_context(|| {
                        format!("恢复影子 {} 权限放宽记录失败", shadow_root.display())
                    })?;
            }
        }

        Ok(Self {
            handle: handle.to_owned(),
            shadow_key,
            host_dev: st.st_dev,
            host_ino: st.st_ino,
            host_root_fd,
            shadow_root,
        })
    }
}

/// 单个发布事务日志崩溃恢复后的结算信息。
pub struct RecoveredPublish {
    pub reason: String,
    pub effects: Vec<EffectEntry>,
}

/// 恢复单条遗留的 `PublishJournal`（支持任意崩溃点重入，且校验每个宿主根的身份）。
pub fn recover_journal<F>(
    state_dir: &Path,
    invocation_id: &str,
    journal_path: &Path,
    journal: PublishJournal,
    recovery_roots: &[RecoveryRoot],
    mut persist_journal: F,
) -> anyhow::Result<RecoveredPublish>
where
    F: FnMut(&PublishJournal) -> anyhow::Result<()>,
{
    match journal {
        PublishJournal::Undo {
            call_hash,
            roots,
            ops,
            paths,
            ..
        } => {
            let matched =
                resolve_recovery_roots(invocation_id, journal_path, &roots, recovery_roots)?;
            let call_dir = state_dir.join("publish").join(&call_hash);
            let stage_dir = call_dir.join("stage");
            let pre_dir = call_dir.join("pre");
            let stage_fd = open_syncable_dir_fd(&stage_dir).with_context(|| {
                format!(
                    "恢复调用 `{invocation_id}`（日志 {}）失败：缺少暂存目录 {}",
                    journal_path.display(),
                    stage_dir.display()
                )
            })?;
            let pre_fd = open_syncable_dir_fd(&pre_dir).with_context(|| {
                format!(
                    "恢复调用 `{invocation_id}`（日志 {}）失败：缺少备份目录 {}",
                    journal_path.display(),
                    pre_dir.display()
                )
            })?;

            let host_fds: Vec<BorrowedFd<'_>> =
                matched.iter().map(|r| r.host_root_fd.as_fd()).collect();
            let mut touched_dirs = BTreeSet::new();

            let rb = rollback_ops(
                &ops,
                &paths,
                &host_fds,
                stage_fd.as_fd(),
                pre_fd.as_fd(),
                &mut touched_dirs,
            )
            .with_context(|| {
                format!(
                    "恢复调用 `{invocation_id}`（日志 {}）逆序撤回失败",
                    journal_path.display()
                )
            })?;

            sync_host_and_publish_dirs(&host_fds, &touched_dirs, stage_fd.as_fd(), pre_fd.as_fd())
                .with_context(|| {
                    format!(
                        "恢复调用 `{invocation_id}`（日志 {}）撤回后刷盘失败",
                        journal_path.display()
                    )
                })?;

            if !rb.has_host_stays {
                let finish = PublishJournal::Finish {
                    call_hash: call_hash.clone(),
                    roots,
                    paths,
                    outcome: PublishFinish::RolledBack,
                };
                persist_journal(&finish)?;
                cleanup_publish_dir(state_dir, &call_hash)?;
                Ok(RecoveredPublish {
                    reason:
                        "节点在调用结算前重启，发布已撤回，草稿保留；可用 status() 查看待定改动"
                            .to_owned(),
                    effects: Vec::new(),
                })
            } else {
                let dispositions = rb.dispositions;
                let finish = PublishJournal::Finish {
                    call_hash: call_hash.clone(),
                    roots: roots.clone(),
                    paths: paths.clone(),
                    outcome: PublishFinish::Indeterminate {
                        dispositions: dispositions.clone(),
                    },
                };
                persist_journal(&finish)?;

                let base_stage_fd = open_optional_base_stage_dir(state_dir, &call_hash)?;
                advance_recovery_bases_partial(
                    base_stage_fd.as_ref().map(AsFd::as_fd),
                    &matched,
                    &paths,
                    &dispositions,
                )?;
                cleanup_publish_dir(state_dir, &call_hash)?;

                let kept_set: BTreeSet<(usize, &str)> = dispositions
                    .iter()
                    .filter(|d| d.disposition.stays_on_host())
                    .map(|d| (d.root_idx, d.rel.as_str()))
                    .collect();
                let effects = build_publish_effects(&roots, &paths, Some(&kept_set), None);
                let reason = format_indeterminate_reason(
                    "节点在调用结算前重启，部分保留宿主修改：\n",
                    &roots,
                    &dispositions,
                );
                Ok(RecoveredPublish { reason, effects })
            }
        }
        PublishJournal::Finish {
            call_hash,
            roots,
            paths,
            outcome,
        } => {
            // C4：分派 Finish 的三种结果之前统一核验全部宿主根身份，不符时拒绝启动且不清理事务。
            let matched =
                resolve_recovery_roots(invocation_id, journal_path, &roots, recovery_roots)?;
            match outcome {
                PublishFinish::RolledBack => {
                    cleanup_publish_dir(state_dir, &call_hash)?;
                    Ok(RecoveredPublish {
                        reason:
                            "节点在调用结算前重启，发布已撤回，草稿保留；可用 status() 查看待定改动"
                                .to_owned(),
                        effects: Vec::new(),
                    })
                }
                PublishFinish::Committed => {
                    let base_stage_fd = open_optional_base_stage_dir(state_dir, &call_hash)?;
                    advance_recovery_bases_committed(
                        base_stage_fd.as_ref().map(AsFd::as_fd),
                        &matched,
                        &paths,
                    )?;
                    cleanup_publish_dir(state_dir, &call_hash)?;
                    let effects = build_publish_effects(&roots, &paths, None, None);
                    Ok(RecoveredPublish {
                        reason: "节点在调用结算前重启，发布已完成".to_owned(),
                        effects,
                    })
                }
                PublishFinish::Indeterminate { dispositions } => {
                    let base_stage_fd = open_optional_base_stage_dir(state_dir, &call_hash)?;
                    advance_recovery_bases_partial(
                        base_stage_fd.as_ref().map(AsFd::as_fd),
                        &matched,
                        &paths,
                        &dispositions,
                    )?;
                    cleanup_publish_dir(state_dir, &call_hash)?;
                    let kept_set: BTreeSet<(usize, &str)> = dispositions
                        .iter()
                        .filter(|d| d.disposition.stays_on_host())
                        .map(|d| (d.root_idx, d.rel.as_str()))
                        .collect();
                    let effects = build_publish_effects(&roots, &paths, Some(&kept_set), None);
                    let reason = format_indeterminate_reason(
                        "节点在调用结算前重启，部分保留宿主修改：\n",
                        &roots,
                        &dispositions,
                    );
                    Ok(RecoveredPublish { reason, effects })
                }
            }
        }
    }
}

fn open_optional_base_stage_dir(
    state_dir: &Path,
    call_hash: &str,
) -> anyhow::Result<Option<OwnedFd>> {
    let base_stage_dir = state_dir.join("publish").join(call_hash).join("base");
    match open_syncable_dir_fd(&base_stage_dir) {
        Ok(fd) => Ok(Some(fd)),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(err) => {
            Err(err).with_context(|| format!("打开基准暂存目录失败：{}", base_stage_dir.display()))
        }
    }
}

fn resolve_recovery_roots<'a>(
    invocation_id: &str,
    journal_path: &Path,
    journal_roots: &[JournalRoot],
    available: &'a [RecoveryRoot],
) -> anyhow::Result<Vec<&'a RecoveryRoot>> {
    let mut matched = Vec::with_capacity(journal_roots.len());
    for jr in journal_roots {
        let Some(found) = available.iter().find(|r| {
            r.shadow_key == jr.shadow_key && r.host_dev == jr.host_dev && r.host_ino == jr.host_ino
        }) else {
            anyhow::bail!(
                "恢复发布事务失败（调用 `{invocation_id}`，日志 {}）：当前配置中找不到与日志匹配的宿主工作目录（句柄 `{}`，shadow_key={}，dev={}，ino={}）",
                journal_path.display(),
                jr.handle,
                jr.shadow_key,
                jr.host_dev,
                jr.host_ino
            );
        };
        matched.push(found);
    }
    Ok(matched)
}

fn advance_recovery_bases_committed(
    base_stage_fd: Option<BorrowedFd<'_>>,
    matched: &[&RecoveryRoot],
    paths: &[BatchPath],
) -> anyhow::Result<()> {
    let mut any_staged_base = false;
    for (root_idx, root) in matched.iter().enumerate() {
        let root_paths: Vec<&BatchPath> =
            paths.iter().filter(|bp| bp.root_idx == root_idx).collect();
        if root_paths.is_empty() {
            continue;
        }
        if root_paths.iter().any(|bp| bp.staged_base.is_some()) {
            any_staged_base = true;
        }
        advance_recovery_single_base(base_stage_fd, root, &root_paths)?;
    }
    sync_base_stage_once(base_stage_fd, any_staged_base)?;
    Ok(())
}

fn advance_recovery_bases_partial(
    base_stage_fd: Option<BorrowedFd<'_>>,
    matched: &[&RecoveryRoot],
    paths: &[BatchPath],
    dispositions: &[PathDispositionEntry],
) -> anyhow::Result<()> {
    let mut any_staged_base = false;
    for (root_idx, root) in matched.iter().enumerate() {
        let root_paths = filter_partial_base_paths(root_idx, paths, dispositions);
        if root_paths.is_empty() {
            continue;
        }
        if root_paths.iter().any(|bp| bp.staged_base.is_some()) {
            any_staged_base = true;
        }
        advance_recovery_single_base(base_stage_fd, root, &root_paths)?;
    }
    sync_base_stage_once(base_stage_fd, any_staged_base)?;
    Ok(())
}

fn advance_recovery_single_base(
    base_stage_fd: Option<BorrowedFd<'_>>,
    root: &RecoveryRoot,
    root_paths: &[&BatchPath],
) -> anyhow::Result<()> {
    let base_root = root.shadow_root.join("base");
    let tmp_root = root.shadow_root.join("tmp");
    let base_fd = open_root_fd(&base_root)
        .with_context(|| format!("恢复推进基准时打开 base 目录失败：{}", base_root.display()))?;
    let mut widened = WidenedLog::new(&root.shadow_root, &tmp_root).with_context(|| {
        format!(
            "恢复推进基准时初始化 widened 失败：{}",
            root.shadow_root.display()
        )
    })?;

    let _ = advance_base_on_disk_with_widened(
        base_stage_fd,
        base_fd.as_fd(),
        &mut widened,
        root_paths,
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
    use tempfile::TempDir;

    /// C8：阶段 5 `ChmodDir` 已落盘目录 `d` 后，必须把 `(root_idx, "d")` 从 `touched_dirs` 移除，
    /// 避免批尾 `sync_host_and_publish_dirs` 对同一目录重复落盘。
    #[test]
    fn phase5_chmod_dir_is_not_synced_twice_at_batch_end() {
        let dir = TempDir::new().unwrap();
        let host = dir.path().join("host");
        let stage = dir.path().join("stage");
        let pre = dir.path().join("pre");
        fs::create_dir_all(host.join("sub")).unwrap();
        fs::create_dir_all(&stage).unwrap();
        fs::create_dir_all(&pre).unwrap();

        let host_fd = open_root_fd(&host).unwrap();
        let stage_fd = open_syncable_dir_fd(&stage).unwrap();
        let pre_fd = open_syncable_dir_fd(&pre).unwrap();

        let sub_meta = fs::symlink_metadata(host.join("sub")).unwrap();
        let sub_id = FileId {
            dev: sub_meta.dev(),
            ino: sub_meta.ino(),
        };

        let mut touched_dirs: BTreeSet<(usize, PathBuf)> = BTreeSet::new();
        // 模拟阶段 4 在 sub 内创建文件后已把 (0, "sub") 记入待刷盘目录集合。
        touched_dirs.insert((0, PathBuf::from("sub")));

        let op = JournalOp::ChmodDir {
            op_id: 9,
            root_idx: 0,
            rel: "sub".to_owned(),
            before_mode: 0o755,
            after_mode: 0o555,
            original_mode: None,
            target_id: sub_id,
        };
        apply_op_with_idx(
            0,
            &op,
            host_fd.as_fd(),
            stage_fd.as_fd(),
            pre_fd.as_fd(),
            &mut touched_dirs,
        )
        .unwrap();

        let _ = fs::set_permissions(host.join("sub"), fs::Permissions::from_mode(0o700));
        assert!(
            !touched_dirs.contains(&(0, PathBuf::from("sub"))),
            "阶段 5 已经 fchmod+fsync 落盘的目录 sub 不应继续留在 touched_dirs 供批尾重复落盘：{touched_dirs:?}"
        );
    }

    /// C3 + A + C8：基准推进遇到 `0444`（无执行位）祖先目录 `p` 及其下的 `0555` 子目录 `p/sub`、
    /// 预暂存文件 `p/sub/f.txt` 与待删只读目录 `ro_del/c.txt` 时，必须由深到浅精确设权并刷盘而不吞 `EACCES`，
    /// 且缺少基准暂存对象时返回错误而非回退读取 `tree/`。
    #[test]
    fn advance_base_strict_sync_and_0444_parent_mode_restore() {
        let dir = TempDir::new().unwrap();
        let shadow_root = dir.path().join("shadow");
        let base = shadow_root.join("base");
        let tmp = shadow_root.join("tmp");
        let base_stage = dir.path().join("pub_base");
        fs::create_dir_all(base.join("p/sub")).unwrap();
        fs::create_dir_all(base.join("ro_del")).unwrap();
        fs::write(base.join("ro_del/c.txt"), b"del\n").unwrap();
        fs::create_dir_all(&tmp).unwrap();
        fs::create_dir_all(&base_stage).unwrap();

        fs::set_permissions(base.join("ro_del"), fs::Permissions::from_mode(0o555)).unwrap();
        fs::set_permissions(base.join("p"), fs::Permissions::from_mode(0o444)).unwrap();

        fs::write(base_stage.join("1"), b"from-stage\n").unwrap();
        fs::set_permissions(base_stage.join("1"), fs::Permissions::from_mode(0o644)).unwrap();
        let st1 = fs::symlink_metadata(base_stage.join("1")).unwrap();
        let sb1 = StagedBase {
            slot: 1,
            id: FileId {
                dev: st1.dev(),
                ino: st1.ino(),
            },
        };

        let base_fd = open_root_fd(&base).unwrap();
        let base_stage_fd = open_syncable_dir_fd(&base_stage).unwrap();
        let mut widened = WidenedLog::new(&shadow_root, &tmp).unwrap();

        let paths = [
            BatchPath {
                root_idx: 0,
                rel: "p/sub".to_owned(),
                before: Some(ObjectState::Dir { mode: 0o755 }),
                after: Some(ObjectState::Dir { mode: 0o555 }),
                staged_base: None,
            },
            BatchPath {
                root_idx: 0,
                rel: "p/sub/f.txt".to_owned(),
                before: None,
                after: Some(ObjectState::File {
                    mode: 0o644,
                    size: 11,
                    sha256: [0u8; 32],
                }),
                staged_base: Some(sb1),
            },
            BatchPath {
                root_idx: 0,
                rel: "ro_del/c.txt".to_owned(),
                before: Some(ObjectState::File {
                    mode: 0o644,
                    size: 4,
                    sha256: [0u8; 32],
                }),
                after: None,
                staged_base: None,
            },
            BatchPath {
                root_idx: 0,
                rel: "ro_del".to_owned(),
                before: Some(ObjectState::Dir { mode: 0o555 }),
                after: None,
                staged_base: None,
            },
        ];
        let refs: Vec<&BatchPath> = paths.iter().collect();
        advance_base_on_disk_with_widened(
            Some(base_stage_fd.as_fd()),
            base_fd.as_fd(),
            &mut widened,
            &refs,
        )
        .unwrap();

        assert!(!base.join("ro_del").exists(), "只读待删目录应已干净删除");
        assert!(
            widened.base_active_modes().is_empty(),
            "Pass 3 完成后 widened 中的 Base 记录应清空"
        );
        assert_eq!(
            fs::symlink_metadata(base.join("p")).unwrap().mode() & 0o7777,
            0o444,
            "0444 祖先目录应精确恢复为 0444"
        );
        fs::set_permissions(base.join("p"), fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(
            fs::symlink_metadata(base.join("p/sub")).unwrap().mode() & 0o7777,
            0o555,
            "0444 祖先下的子目录必须精确设为 0555（不得先锁父目录吞掉 EACCES）"
        );
        let installed_st = fs::symlink_metadata(base.join("p/sub/f.txt")).unwrap();
        assert_eq!(
            (installed_st.dev(), installed_st.ino()),
            (sb1.id.dev, sb1.id.ino)
        );
        assert_eq!(
            fs::read_to_string(base.join("p/sub/f.txt")).unwrap(),
            "from-stage\n"
        );

        // 模拟清理 publish/<hash>/base 之后、写 Settled 之前崩溃的重入：
        // base_stage_fd == None 但 base/p/sub/f.txt 已经是 sb1.id -> 幂等通过；
        // 若目标身份不匹配（或缺失）则仍须明确报错。
        fs::set_permissions(base.join("p"), fs::Permissions::from_mode(0o444)).unwrap();
        advance_base_on_disk_with_widened(None, base_fd.as_fd(), &mut widened, &refs).unwrap();
        assert_eq!(
            fs::symlink_metadata(base.join("p")).unwrap().mode() & 0o7777,
            0o444
        );
        fs::set_permissions(base.join("p"), fs::Permissions::from_mode(0o755)).unwrap();

        // 缺少预暂存对象时必须报错，绝不静默跳过。
        let missing_bp = BatchPath {
            root_idx: 0,
            rel: "missing.txt".to_owned(),
            before: None,
            after: Some(ObjectState::File {
                mode: 0o644,
                size: 1,
                sha256: [0u8; 32],
            }),
            staged_base: Some(StagedBase {
                slot: 99,
                id: FileId { dev: 1, ino: 2 },
            }),
        };
        let err = advance_base_on_disk_with_widened(
            Some(base_stage_fd.as_fd()),
            base_fd.as_fd(),
            &mut widened,
            &[&missing_bp],
        )
        .unwrap_err();
        assert!(
            format!("{err:#}").contains("基准暂存槽"),
            "缺失基准暂存槽应明确报错：{err:#}"
        );

        let _ = fs::set_permissions(base.join("p/sub"), fs::Permissions::from_mode(0o700));
        let _ = fs::set_permissions(base.join("p"), fs::Permissions::from_mode(0o700));
    }

    /// B1：实时部分撤回仅传入已执行的前缀操作（不包含阶段 5 `ChmodDir`），
    /// 若新建只读目录 `d`（最终权限 `0555`，阶段 3 临时权限 `0755`）因留在宿主的子项 `d/x` 被保留并标为“已发布”，
    /// 必须依据 `BatchPath.after` 把宿主目录 `d` 恢复到最终权限 `0555` 并落盘。
    #[test]
    fn live_partial_rollback_prefix_restores_readonly_dir_mode() {
        let dir = TempDir::new().unwrap();
        let host = dir.path().join("host");
        let stage = dir.path().join("stage");
        let pre = dir.path().join("pre");
        fs::create_dir_all(host.join("d")).unwrap();
        fs::create_dir_all(&stage).unwrap();
        fs::create_dir_all(&pre).unwrap();

        fs::set_permissions(host.join("d"), fs::Permissions::from_mode(0o755)).unwrap();
        fs::write(host.join("d/x"), b"host-edited-x\n").unwrap();
        fs::set_permissions(host.join("d/x"), fs::Permissions::from_mode(0o644)).unwrap();

        let dir_meta = fs::symlink_metadata(host.join("d")).unwrap();
        let dir_id = FileId {
            dev: dir_meta.dev(),
            ino: dir_meta.ino(),
        };
        let file_meta = fs::symlink_metadata(host.join("d/x")).unwrap();
        let file_id = FileId {
            dev: file_meta.dev(),
            ino: file_meta.ino(),
        };

        let host_fd = open_root_fd(&host).unwrap();
        let stage_fd = open_syncable_dir_fd(&stage).unwrap();
        let pre_fd = open_syncable_dir_fd(&pre).unwrap();

        let st_x = ObjectState::File {
            mode: 0o644,
            size: 6,
            sha256: [1u8; 32],
        };
        // 仅传入阶段 3 + 阶段 4 前缀（阶段 5 ChmodDir 尚未执行，不在切片中）。
        let prefix_ops = vec![
            JournalOp::Create {
                op_id: 1,
                root_idx: 0,
                rel: "d".to_owned(),
                staged_id: dir_id,
                after: ObjectState::Dir { mode: 0o755 },
            },
            JournalOp::Create {
                op_id: 2,
                root_idx: 0,
                rel: "d/x".to_owned(),
                staged_id: file_id,
                after: st_x.clone(),
            },
        ];
        let paths = vec![
            BatchPath {
                root_idx: 0,
                rel: "d".to_owned(),
                before: None,
                after: Some(ObjectState::Dir { mode: 0o555 }),
                staged_base: None,
            },
            BatchPath {
                root_idx: 0,
                rel: "d/x".to_owned(),
                before: None,
                after: Some(st_x),
                staged_base: None,
            },
        ];

        let mut touched_dirs = BTreeSet::new();
        let rb = rollback_ops(
            &prefix_ops,
            &paths,
            &[host_fd.as_fd()],
            stage_fd.as_fd(),
            pre_fd.as_fd(),
            &mut touched_dirs,
        )
        .unwrap();

        let d_mode = fs::symlink_metadata(host.join("d")).unwrap().mode() & 0o7777;
        let _ = fs::set_permissions(host.join("d"), fs::Permissions::from_mode(0o700));
        assert!(rb.has_host_stays);
        assert_eq!(rb.dispositions[0].disposition, PathDisposition::Published);
        assert_eq!(
            rb.dispositions[1].disposition,
            PathDisposition::KeptHostEdit
        );
        assert_eq!(
            d_mode, 0o555,
            "实时部分撤回保留只读目录 d（已发布）时，必须依据 BatchPath.after 恢复 0555，实际为 {d_mode:o}"
        );
    }

    /// B2：基准暂存源目录 `publish/<hash>/base` 必须在整批全部根推进完后统一落盘一次，
    /// 重放（本轮无改名）时若该目录仍存在也同样落盘一次，跨多根时不得逐根重复落盘。
    #[test]
    fn base_stage_dir_synced_once_across_roots_and_on_replay() {
        let dir = TempDir::new().unwrap();
        let state = dir.path().join("state");
        let host1 = dir.path().join("host1");
        let host2 = dir.path().join("host2");
        fs::create_dir_all(&host1).unwrap();
        fs::create_dir_all(&host2).unwrap();
        fs::create_dir_all(&state).unwrap();

        let r1 = RecoveryRoot::open(&state, "w1", &host1, &[], &[]).unwrap();
        let r2 = RecoveryRoot::open(&state, "w2", &host2, &[], &[]).unwrap();
        for r in [&r1, &r2] {
            fs::create_dir_all(r.shadow_root.join("base/d")).unwrap();
            fs::create_dir_all(r.shadow_root.join("tree")).unwrap();
            fs::create_dir_all(r.shadow_root.join("tmp")).unwrap();
        }

        let base_stage = dir.path().join("pub_base");
        fs::create_dir_all(&base_stage).unwrap();

        // 子用例 1（跨根只落盘一次）：
        // 以 O_PATH 打开 base_stage（支持 statat/renameat，但 fsync 返回 EBADF）。
        // 若按根落盘，根 0 结束后即触发 EBADF，根 1 的文件尚未移动；
        // 若在整批各根推进完后统一落盘一次，则两根文件都已就位后才在末尾触发一次 EBADF。
        fs::write(base_stage.join("1"), b"root1\n").unwrap();
        fs::set_permissions(base_stage.join("1"), fs::Permissions::from_mode(0o644)).unwrap();
        let st1 = fs::symlink_metadata(base_stage.join("1")).unwrap();
        let sb1 = StagedBase {
            slot: 1,
            id: FileId {
                dev: st1.dev(),
                ino: st1.ino(),
            },
        };
        fs::write(base_stage.join("2"), b"root2\n").unwrap();
        fs::set_permissions(base_stage.join("2"), fs::Permissions::from_mode(0o644)).unwrap();
        let st2 = fs::symlink_metadata(base_stage.join("2")).unwrap();
        let sb2 = StagedBase {
            slot: 2,
            id: FileId {
                dev: st2.dev(),
                ino: st2.ino(),
            },
        };

        let paths = vec![
            BatchPath {
                root_idx: 0,
                rel: "d/x".to_owned(),
                before: None,
                after: Some(ObjectState::File {
                    mode: 0o644,
                    size: 6,
                    sha256: [0u8; 32],
                }),
                staged_base: Some(sb1),
            },
            BatchPath {
                root_idx: 1,
                rel: "d/y".to_owned(),
                before: None,
                after: Some(ObjectState::File {
                    mode: 0o644,
                    size: 6,
                    sha256: [0u8; 32],
                }),
                staged_base: Some(sb2),
            },
        ];

        let o_path_stage = openat(
            rustix::fs::CWD,
            &base_stage,
            OFlags::PATH | OFlags::DIRECTORY | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .unwrap();

        let err_multi =
            advance_recovery_bases_committed(Some(o_path_stage.as_fd()), &[&r1, &r2], &paths)
                .expect_err("O_PATH 基准暂存目录应在批尾落盘时失败");
        assert!(
            format!("{err_multi:#}").contains("刷盘基准暂存目录失败"),
            "错误应来自刷盘基准暂存目录：{err_multi:#}"
        );
        assert!(
            r1.shadow_root.join("base/d/x").is_file() && r2.shadow_root.join("base/d/y").is_file(),
            "跨根推进不得在根 0 结束后就落盘源目录：必须先推进完全部根（含根 1）再统一落盘一次"
        );

        // 子用例 2（重放时无新改名，但源目录仍在，仍必须对源目录落盘一次）：
        let err_replay =
            advance_recovery_bases_committed(Some(o_path_stage.as_fd()), &[&r1, &r2], &paths)
                .expect_err("重放时若源目录仍在，即使本轮无新改名也必须对其落盘一次");
        assert!(
            format!("{err_replay:#}").contains("刷盘基准暂存目录失败"),
            "重放错误应来自刷盘基准暂存目录：{err_replay:#}"
        );

        // 用正常目录 fd 重放必须顺利成功。
        let real_stage = open_syncable_dir_fd(&base_stage).unwrap();
        advance_recovery_bases_committed(Some(real_stage.as_fd()), &[&r1, &r2], &paths).unwrap();
    }

    /// B3：`Undo` 重入时，新建项 `d/x` 已在前一次撤回中移入 `pre/1` 并核对通过；
    /// 恢复确认已撤回的分支必须把宿主上级目录 `d` 记入 `touched_dirs`，保证批尾将其落盘。
    #[test]
    fn create_rollback_reentry_with_pre_entry_marks_host_parent_touched() {
        let dir = TempDir::new().unwrap();
        let host = dir.path().join("host");
        let stage = dir.path().join("stage");
        let pre = dir.path().join("pre");
        fs::create_dir_all(host.join("d")).unwrap();
        fs::create_dir_all(&stage).unwrap();
        fs::create_dir_all(&pre).unwrap();

        fs::write(pre.join("1"), b"created-x\n").unwrap();
        fs::set_permissions(pre.join("1"), fs::Permissions::from_mode(0o644)).unwrap();
        let pre_meta = fs::symlink_metadata(pre.join("1")).unwrap();
        let staged_id = FileId {
            dev: pre_meta.dev(),
            ino: pre_meta.ino(),
        };
        let mut h = Sha256::new();
        h.update(b"created-x\n");
        let sha: [u8; 32] = h.finalize().into();

        let st_x = ObjectState::File {
            mode: 0o644,
            size: 10,
            sha256: sha,
        };
        let ops = vec![JournalOp::Create {
            op_id: 1,
            root_idx: 0,
            rel: "d/x".to_owned(),
            staged_id,
            after: st_x.clone(),
        }];
        let paths = vec![BatchPath {
            root_idx: 0,
            rel: "d/x".to_owned(),
            before: None,
            after: Some(st_x),
            staged_base: None,
        }];

        let host_fd = open_root_fd(&host).unwrap();
        let stage_fd = open_syncable_dir_fd(&stage).unwrap();
        let pre_fd = open_syncable_dir_fd(&pre).unwrap();
        let mut touched_dirs = BTreeSet::new();

        let rb = rollback_ops(
            &ops,
            &paths,
            &[host_fd.as_fd()],
            stage_fd.as_fd(),
            pre_fd.as_fd(),
            &mut touched_dirs,
        )
        .unwrap();
        assert!(!rb.has_host_stays);
        assert_eq!(rb.dispositions[0].disposition, PathDisposition::RolledBack);
        assert!(
            touched_dirs.contains(&(0, PathBuf::from("d"))),
            "pre/1 核对通过确认 Create 已撤回时，必须把宿主上级目录 d 记入 touched_dirs：{touched_dirs:?}"
        );
    }

    /// 第 4 轮项 1：宿主在撤回期间把目录 `d` 从临时权限 `0755` 改为 `0111` 后，
    /// `restore_published_dir_mode_if_unchanged` 收到 `EACCES` 时必须用 `verify_object_at` 复核身份与临时权限：
    /// - 身份或权限已变 -> 返回 `Ok(false)`（交调用方记为“保留宿主修改”），不得升级为致命错误；
    /// - 身份与临时权限仍匹配 -> 传播原始 `EACCES` 错误。
    #[test]
    fn restore_published_dir_mode_handles_eacces_after_host_chmod_0111() {
        let dir = TempDir::new().unwrap();
        let host = dir.path().join("host");
        fs::create_dir_all(host.join("d")).unwrap();
        fs::create_dir_all(host.join("d_same")).unwrap();
        fs::set_permissions(host.join("d"), fs::Permissions::from_mode(0o755)).unwrap();
        fs::set_permissions(host.join("d_same"), fs::Permissions::from_mode(0o111)).unwrap();

        let d_meta = fs::symlink_metadata(host.join("d")).unwrap();
        let d_id = FileId {
            dev: d_meta.dev(),
            ino: d_meta.ino(),
        };
        let same_meta = fs::symlink_metadata(host.join("d_same")).unwrap();
        let same_id = FileId {
            dev: same_meta.dev(),
            ino: same_meta.ino(),
        };

        // 模拟宿主在撤回枚举子项期间把 d 从 0755 改为 0111（去掉读位，导致 O_RDONLY 返回 EACCES）。
        fs::set_permissions(host.join("d"), fs::Permissions::from_mode(0o111)).unwrap();

        let host_fd = open_root_fd(&host).unwrap();
        let res_changed = restore_published_dir_mode_if_unchanged(
            host_fd.as_fd(),
            OsStr::new("d"),
            d_id,
            0o755,
            0o555,
        );
        let d_mode_after = fs::symlink_metadata(host.join("d")).unwrap().mode() & 0o7777;

        let res_same = restore_published_dir_mode_if_unchanged(
            host_fd.as_fd(),
            OsStr::new("d_same"),
            same_id,
            0o111,
            0o555,
        );

        let _ = fs::set_permissions(host.join("d"), fs::Permissions::from_mode(0o700));
        let _ = fs::set_permissions(host.join("d_same"), fs::Permissions::from_mode(0o700));

        assert!(
            matches!(res_changed, Ok(false)),
            "宿主把 d 改为 0111 后 openat 返回 EACCES 必须识别权限已变并返回 Ok(false)，实际：{res_changed:?}"
        );
        assert_eq!(d_mode_after, 0o111, "宿主修改后的 0111 权限必须保留");

        let err = res_same.expect_err("身份与临时权限仍匹配时的 EACCES 必须向上传播");
        assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);
    }

    #[test]
    fn classify_dir_children_handles_eacces_after_host_chmod_0111() {
        let dir = TempDir::new().unwrap();
        let host = dir.path().join("host");
        fs::create_dir_all(host.join("d")).unwrap();
        fs::create_dir_all(host.join("d_same")).unwrap();
        fs::set_permissions(host.join("d"), fs::Permissions::from_mode(0o755)).unwrap();
        fs::set_permissions(host.join("d_same"), fs::Permissions::from_mode(0o111)).unwrap();

        let id_of = |name: &str| {
            let meta = fs::symlink_metadata(host.join(name)).unwrap();
            FileId {
                dev: meta.dev(),
                ino: meta.ino(),
            }
        };
        let d_id = id_of("d");
        let same_id = id_of("d_same");

        // 撤回已复核过 d（临时权限 0755），枚举子项之前宿主把它改为 0111。
        fs::set_permissions(host.join("d"), fs::Permissions::from_mode(0o111)).unwrap();

        let host_fd = open_root_fd(&host).unwrap();
        let stayed = BTreeSet::new();
        let res_changed = classify_dir_children(
            host_fd.as_fd(),
            OsStr::new("d"),
            &ObjectState::Dir { mode: 0o755 },
            d_id,
            0,
            Path::new("d"),
            &stayed,
        );
        let res_same = classify_dir_children(
            host_fd.as_fd(),
            OsStr::new("d_same"),
            &ObjectState::Dir { mode: 0o111 },
            same_id,
            0,
            Path::new("d_same"),
            &stayed,
        );

        let _ = fs::set_permissions(host.join("d"), fs::Permissions::from_mode(0o700));
        let _ = fs::set_permissions(host.join("d_same"), fs::Permissions::from_mode(0o700));

        assert!(
            matches!(res_changed, Ok(DirChildrenState::HasForeignChildren)),
            "宿主改权后枚举得到 EACCES，必须按宿主改动处理，实际：{res_changed:?}"
        );
        let err = res_same.expect_err("身份与临时权限仍匹配时的 EACCES 必须向上传播");
        assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);
    }
}
