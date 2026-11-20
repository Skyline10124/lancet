// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 持久调用记录账本：零重放、围栏、在途取消、发布临界区准入、重启恢复与回执到期。

use std::collections::{BTreeSet, HashMap};
use std::fs::{DirBuilder, File};
use std::io::{self, Write as _};
use std::os::unix::fs::DirBuilderExt as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use anyhow::Context as _;
use lancet_node::shadow::publish::{
    CallRecord, PublishJournal, RecoveryRoot, call_record_file_name, call_record_hash,
    cleanup_publish_dir, recover_journal,
};
use lancet_protocol::{
    CatalogDigest, Labels, Receipt, ReceiptLookup, ReceiptOutcome, RejectCode, ToolError,
};
use rustix::fs::{Mode, OFlags};

use crate::execute::RejectedCall;
use crate::state::StateDir;

const RESTART_INTERNAL_ERROR_MESSAGE: &str = "节点在调用结算前重启，未保存执行结果";
const RESTART_INDETERMINATE_REASON: &str =
    "节点在调用结算前重启，发布未开始，影子中的写入是否发生未知；可用 status() 查看待定改动";

#[derive(Clone)]
struct PendingMeta {
    invocation_id: String,
    tool: String,
    catalog_digest: CatalogDigest,
    labels: Labels,
    writes_possible: bool,
}

struct InFlightCall {
    conn_id: u64,
    cancelled: Arc<AtomicBool>,
    meta: PendingMeta,
}

/// 受理通过后交给执行线程的调用上下文。
pub struct AdmittedCall {
    pub invocation_id: String,
    pub tool: String,
    pub catalog_digest: CatalogDigest,
    pub labels: Labels,
    pub script: String,
    pub cancelled: Arc<AtomicBool>,
}

/// 发布临界区准入闸门结果（与 `cancel`/`cancel_connection` 串行化）。
pub enum CriticalSectionGate {
    Entered,
    Cancelled,
    Deadline,
}

/// 启动期在打开 `ShadowDir` 之前恢复所有带 `PublishJournal` 的遗留调用，并清理未进入临界区或已结算调用的残留发布目录。
pub fn recover_published_calls(
    state_dir: &StateDir,
    recovery_roots: &[RecoveryRoot],
) -> anyhow::Result<()> {
    let calls_dir = state_dir.path().join("calls");
    let mut active_publish_hashes = BTreeSet::new();

    if calls_dir.is_dir() {
        let calls_dir_file = File::from(
            rustix::fs::open(
                &calls_dir,
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
                Mode::empty(),
            )
            .with_context(|| format!("无法打开调用记录目录：{}", calls_dir.display()))?,
        );

        let mut json_names = Vec::new();
        for entry in std::fs::read_dir(&calls_dir)
            .with_context(|| format!("无法扫描调用记录目录：{}", calls_dir.display()))?
        {
            let entry = entry
                .with_context(|| format!("读取调用记录目录项失败：{}", calls_dir.display()))?;
            if let Some(name) = entry.file_name().to_str()
                && name.ends_with(".json")
            {
                json_names.push(name.to_owned());
            }
        }
        json_names.sort();

        for name_str in json_names {
            let path = calls_dir.join(&name_str);
            let Some(record) = read_record(&path)? else {
                continue;
            };
            let CallRecord::Pending {
                invocation_id,
                tool,
                catalog_digest,
                labels,
                writes_possible,
                publish: Some(journal),
            } = record
            else {
                continue;
            };

            let call_hash = journal.call_hash().to_owned();
            active_publish_hashes.insert(call_hash.clone());

            let inv_for_cb = invocation_id.clone();
            let tool_for_cb = tool.clone();
            let digest_for_cb = catalog_digest.clone();
            let labels_for_cb = labels.clone();
            let recovered = recover_journal(
                state_dir.path(),
                &invocation_id,
                &path,
                journal,
                recovery_roots,
                |updated_journal| {
                    let updated = CallRecord::Pending {
                        invocation_id: inv_for_cb.clone(),
                        tool: tool_for_cb.clone(),
                        catalog_digest: digest_for_cb.clone(),
                        labels: labels_for_cb.clone(),
                        writes_possible,
                        publish: Some(updated_journal.clone()),
                    };
                    write_record(&calls_dir, &calls_dir_file, &name_str, &updated)
                },
            )?;

            let receipt = Receipt {
                invocation_id,
                tool,
                catalog_digest,
                labels,
                outcome: ReceiptOutcome::Indeterminate {
                    reason: recovered.reason,
                },
                steps: Vec::new(),
                effects: recovered.effects,
                truncated: false,
            };
            let settled = CallRecord::Settled {
                settled_at_ms: now_unix_ms(),
                receipt,
            };
            write_record(&calls_dir, &calls_dir_file, &name_str, &settled)?;
            active_publish_hashes.remove(&call_hash);
        }
    }

    // 清理未进入临界区（仅残留暂存目录）或已结算调用遗留的 publish/<hash>/ 目录。
    let publish_root = state_dir.path().join("publish");
    if publish_root.is_dir() {
        for entry in std::fs::read_dir(&publish_root)
            .with_context(|| format!("无法扫描发布目录：{}", publish_root.display()))?
        {
            let entry =
                entry.with_context(|| format!("读取发布目录项失败：{}", publish_root.display()))?;
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            if !active_publish_hashes.contains(&name) {
                cleanup_publish_dir(state_dir.path(), &name)
                    .with_context(|| format!("清理残留发布事务目录 `{name}` 失败"))?;
            }
        }
    }

    Ok(())
}

/// 节点持久调用记录账本；登记、围栏、查询、发布临界区准入与结算均由单把互斥锁串行化。
pub struct Ledger {
    state_dir: PathBuf,
    calls_dir: PathBuf,
    calls_dir_file: File,
    receipt_retention_ms: u64,
    in_flight: Mutex<HashMap<String, InFlightCall>>,
}

impl Ledger {
    /// 打开 `<state_dir>/calls` 并完成重启恢复：结算遗留的 `Pending`，迁移到期的 `Settled`。
    pub fn open(state_dir: &StateDir, receipt_retention_ms: u64) -> anyhow::Result<Self> {
        let calls_dir = state_dir.path().join("calls");
        let created = match DirBuilder::new().mode(0o700).create(&calls_dir) {
            Ok(()) => true,
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => false,
            Err(err) => {
                return Err(err)
                    .with_context(|| format!("无法创建调用记录目录：{}", calls_dir.display()));
            }
        };
        if created {
            state_dir.sync()?;
        }

        let calls_dir_file = File::from(
            rustix::fs::open(
                &calls_dir,
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
                Mode::empty(),
            )
            .with_context(|| format!("无法打开调用记录目录：{}", calls_dir.display()))?,
        );

        recover_calls(
            state_dir.path(),
            &calls_dir,
            &calls_dir_file,
            receipt_retention_ms,
        )?;

        Ok(Self {
            state_dir: state_dir.path().to_path_buf(),
            calls_dir,
            calls_dir_file,
            receipt_retention_ms,
            in_flight: Mutex::new(HashMap::new()),
        })
    }

    /// 返回节点状态目录根路径。
    pub fn state_dir(&self) -> &Path {
        &self.state_dir
    }

    /// 按查重优先顺序核对传入校验结果，全部通过后持久登记 `Pending` 并记入在途表。
    #[allow(clippy::too_many_arguments)]
    pub fn admit_execute(
        &self,
        conn_id: u64,
        invocation_id: String,
        catalog_digest: CatalogDigest,
        tool: String,
        labels: Labels,
        writes_possible: bool,
        validation: Result<String, RejectedCall>,
    ) -> Result<AdmittedCall, RejectedCall> {
        let mut in_flight = self.lock_in_flight();
        let file_name = call_record_file_name(&invocation_id);
        let record_path = self.calls_dir.join(&file_name);

        let exists = record_path
            .try_exists()
            .with_context(|| format!("检查调用记录是否存在失败：{}", record_path.display()))
            .unwrap_or_else(|err| fatal_error(err));
        if exists {
            return Err(RejectedCall {
                code: RejectCode::DuplicateInvocation,
                message: format!("调用 `{invocation_id}` 已存在记录，拒绝重复执行"),
            });
        }

        let script = validation?;

        let meta = PendingMeta {
            invocation_id: invocation_id.clone(),
            tool: tool.clone(),
            catalog_digest: catalog_digest.clone(),
            labels: labels.clone(),
            writes_possible,
        };
        let pending = CallRecord::Pending {
            invocation_id: meta.invocation_id.clone(),
            tool: meta.tool.clone(),
            catalog_digest: meta.catalog_digest.clone(),
            labels: meta.labels.clone(),
            writes_possible: meta.writes_possible,
            publish: None,
        };
        write_record(&self.calls_dir, &self.calls_dir_file, &file_name, &pending)
            .unwrap_or_else(|err| fatal_error(err));

        let cancelled = Arc::new(AtomicBool::new(false));
        in_flight.insert(
            invocation_id.clone(),
            InFlightCall {
                conn_id,
                cancelled: Arc::clone(&cancelled),
                meta,
            },
        );

        Ok(AdmittedCall {
            invocation_id,
            tool,
            catalog_digest,
            labels,
            script,
            cancelled,
        })
    }

    /// 在 `in_flight` 锁下核对取消标志与截止时间（与 `cancel`/`cancel_connection` 串行化），
    /// 通过后将 `PublishJournal::Undo` 原子写入 `CallRecord::Pending` 并放锁进入发布临界区；
    /// 日志写入任何失败均按账本写入失败终止进程，不在进程内清理暂存或撤回。
    pub fn enter_publish_critical_section(
        &self,
        invocation_id: &str,
        deadline: Instant,
        undo_journal: &PublishJournal,
    ) -> CriticalSectionGate {
        let in_flight = self.lock_in_flight();
        let entry = in_flight
            .get(invocation_id)
            .unwrap_or_else(|| fatal_error(anyhow::anyhow!("调用 `{invocation_id}` 不在在途表中")));
        if entry.cancelled.load(Ordering::Acquire) {
            return CriticalSectionGate::Cancelled;
        }
        if Instant::now() >= deadline {
            return CriticalSectionGate::Deadline;
        }
        let record = CallRecord::Pending {
            invocation_id: entry.meta.invocation_id.clone(),
            tool: entry.meta.tool.clone(),
            catalog_digest: entry.meta.catalog_digest.clone(),
            labels: entry.meta.labels.clone(),
            writes_possible: entry.meta.writes_possible,
            publish: Some(undo_journal.clone()),
        };
        let file_name = call_record_file_name(invocation_id);
        write_record(&self.calls_dir, &self.calls_dir_file, &file_name, &record)
            .with_context(|| format!("写入调用 `{invocation_id}` 初始发布事务日志失败"))
            .unwrap_or_else(|err| fatal_error(err));
        CriticalSectionGate::Entered
    }

    /// 在发布临界区内更新 `CallRecord::Pending.publish`（写入 `rollback_reason` 或 `Finish`）。
    pub fn update_publish_journal(
        &self,
        invocation_id: &str,
        journal: &PublishJournal,
    ) -> anyhow::Result<()> {
        let in_flight = self.lock_in_flight();
        let entry = in_flight
            .get(invocation_id)
            .ok_or_else(|| anyhow::anyhow!("调用 `{invocation_id}` 不在在途表中"))?;
        let record = CallRecord::Pending {
            invocation_id: entry.meta.invocation_id.clone(),
            tool: entry.meta.tool.clone(),
            catalog_digest: entry.meta.catalog_digest.clone(),
            labels: entry.meta.labels.clone(),
            writes_possible: entry.meta.writes_possible,
            publish: Some(journal.clone()),
        };
        let file_name = call_record_file_name(invocation_id);
        write_record(&self.calls_dir, &self.calls_dir_file, &file_name, &record)
    }

    /// 查询调用记录；未知 id 持久登记围栏后返回 `NeverExecuted`，到期回执迁移为 `Expired`。
    pub fn query_receipt(&self, invocation_id: &str) -> ReceiptLookup {
        let _in_flight = self.lock_in_flight();
        let file_name = call_record_file_name(invocation_id);
        let path = self.calls_dir.join(&file_name);
        let record = read_record(&path).unwrap_or_else(|err| fatal_error(err));

        match record {
            None => {
                write_record(
                    &self.calls_dir,
                    &self.calls_dir_file,
                    &file_name,
                    &CallRecord::Fenced,
                )
                .unwrap_or_else(|err| fatal_error(err));
                ReceiptLookup::NeverExecuted
            }
            Some(CallRecord::Fenced) => ReceiptLookup::NeverExecuted,
            Some(CallRecord::Pending { .. }) => ReceiptLookup::Pending,
            Some(CallRecord::Settled {
                settled_at_ms,
                receipt,
            }) => {
                let now_ms = now_unix_ms();
                if is_expired(now_ms, settled_at_ms, self.receipt_retention_ms) {
                    let outcome = receipt.outcome_kind();
                    write_record(
                        &self.calls_dir,
                        &self.calls_dir_file,
                        &file_name,
                        &CallRecord::Expired { outcome },
                    )
                    .unwrap_or_else(|err| fatal_error(err));
                    ReceiptLookup::Expired { outcome }
                } else {
                    ReceiptLookup::Settled { receipt }
                }
            }
            Some(CallRecord::Expired { outcome }) => ReceiptLookup::Expired { outcome },
        }
    }

    /// 置位指定在途调用的取消标志；未知或已结束 id 不改动记录。
    pub fn cancel(&self, invocation_id: &str) {
        let in_flight = self.lock_in_flight();
        if let Some(entry) = in_flight.get(invocation_id) {
            entry.cancelled.store(true, Ordering::Release);
        }
    }

    /// 连接断开时置位该连接所有在途调用的取消标志。
    pub fn cancel_connection(&self, conn_id: u64) {
        let in_flight = self.lock_in_flight();
        for entry in in_flight.values() {
            if entry.conn_id == conn_id {
                entry.cancelled.store(true, Ordering::Release);
            }
        }
    }

    /// 从在途表移除调用并持久写入 `Settled` 回执。
    pub fn settle(&self, invocation_id: &str, receipt: &Receipt) {
        let mut in_flight = self.lock_in_flight();
        in_flight.remove(invocation_id);

        let file_name = call_record_file_name(invocation_id);
        let record = CallRecord::Settled {
            settled_at_ms: now_unix_ms(),
            receipt: receipt.clone(),
        };
        write_record(&self.calls_dir, &self.calls_dir_file, &file_name, &record)
            .unwrap_or_else(|err| fatal_error(err));
    }

    fn lock_in_flight(&self) -> MutexGuard<'_, HashMap<String, InFlightCall>> {
        self.in_flight.lock().expect("调用记录账本互斥锁不应中毒")
    }
}

fn recover_calls(
    state_dir: &Path,
    calls_dir: &Path,
    calls_dir_file: &File,
    receipt_retention_ms: u64,
) -> anyhow::Result<()> {
    let entries = std::fs::read_dir(calls_dir)
        .with_context(|| format!("无法扫描调用记录目录：{}", calls_dir.display()))?;

    for entry in entries {
        let entry =
            entry.with_context(|| format!("读取调用记录目录项失败：{}", calls_dir.display()))?;
        let path = entry.path();
        let file_name = entry.file_name();
        let Some(name_str) = file_name.to_str() else {
            continue;
        };
        if !name_str.ends_with(".json") {
            continue;
        }

        let Some(record) = read_record(&path)? else {
            continue;
        };
        match record {
            CallRecord::Pending {
                invocation_id,
                tool,
                catalog_digest,
                labels,
                writes_possible,
                publish: None,
            } => {
                let _ = cleanup_publish_dir(state_dir, &call_record_hash(&invocation_id));
                let outcome = if writes_possible {
                    ReceiptOutcome::Indeterminate {
                        reason: RESTART_INDETERMINATE_REASON.to_owned(),
                    }
                } else {
                    ReceiptOutcome::Failed {
                        error: ToolError::new("InternalError", RESTART_INTERNAL_ERROR_MESSAGE),
                    }
                };
                let receipt = Receipt {
                    invocation_id,
                    tool,
                    catalog_digest,
                    labels,
                    outcome,
                    steps: Vec::new(),
                    effects: Vec::new(),
                    truncated: false,
                };
                let settled = CallRecord::Settled {
                    settled_at_ms: now_unix_ms(),
                    receipt,
                };
                write_record(calls_dir, calls_dir_file, name_str, &settled)?;
            }
            CallRecord::Pending {
                invocation_id,
                publish: Some(_),
                ..
            } => {
                anyhow::bail!(
                    "调用 `{invocation_id}` 仍有未恢复的发布事务日志（{}），不应跳过前置恢复",
                    path.display()
                );
            }
            CallRecord::Settled {
                settled_at_ms,
                receipt,
            } => {
                if is_expired(now_unix_ms(), settled_at_ms, receipt_retention_ms) {
                    let expired = CallRecord::Expired {
                        outcome: receipt.outcome_kind(),
                    };
                    write_record(calls_dir, calls_dir_file, name_str, &expired)?;
                }
            }
            CallRecord::Fenced | CallRecord::Expired { .. } => {}
        }
    }

    Ok(())
}

fn read_record(path: &Path) -> anyhow::Result<Option<CallRecord>> {
    match std::fs::read(path) {
        Ok(bytes) => {
            let record = serde_json::from_slice(&bytes)
                .with_context(|| format!("解析调用记录失败：{}", path.display()))?;
            Ok(Some(record))
        }
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err).with_context(|| format!("读取调用记录失败：{}", path.display())),
    }
}

fn write_record(
    calls_dir: &Path,
    calls_dir_file: &File,
    file_name: &str,
    record: &CallRecord,
) -> anyhow::Result<()> {
    let path = calls_dir.join(file_name);
    let tmp_path = calls_dir.join(format!("{file_name}.tmp"));
    let bytes = serde_json::to_vec(record).context("序列化调用记录失败")?;
    {
        let mut tmp_file = File::create(&tmp_path)
            .with_context(|| format!("无法创建临时调用记录文件：{}", tmp_path.display()))?;
        tmp_file
            .write_all(&bytes)
            .with_context(|| format!("无法写入临时调用记录文件：{}", tmp_path.display()))?;
        tmp_file
            .sync_all()
            .with_context(|| format!("无法刷盘临时调用记录文件：{}", tmp_path.display()))?;
    }
    std::fs::rename(&tmp_path, &path)
        .with_context(|| format!("无法将 {} 重命名为 {}", tmp_path.display(), path.display()))?;
    calls_dir_file
        .sync_all()
        .with_context(|| format!("无法刷盘调用记录目录：{}", calls_dir.display()))?;
    Ok(())
}

fn now_unix_ms() -> u64 {
    u64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis(),
    )
    .unwrap_or(u64::MAX)
}

const fn is_expired(now_ms: u64, settled_at_ms: u64, receipt_retention_ms: u64) -> bool {
    now_ms.saturating_sub(settled_at_ms) >= receipt_retention_ms
}

/// 遇到无法继续安全服务的致命错误时输出中文原因并终止进程，待重启后由恢复流程结算。
pub fn fatal_error(err: anyhow::Error) -> ! {
    eprintln!("错误：{err:#}");
    std::process::exit(1);
}
