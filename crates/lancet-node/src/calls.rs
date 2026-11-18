// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 持久调用记录账本：零重放、围栏、在途取消、重启恢复与回执到期。

use std::collections::HashMap;
use std::fmt::Write as _;
use std::fs::{DirBuilder, File};
use std::io::{self, Write as _};
use std::os::unix::fs::DirBuilderExt as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Context as _;
use lancet_protocol::{
    CatalogDigest, Labels, OutcomeKind, Receipt, ReceiptLookup, ReceiptOutcome, RejectCode,
    ToolError,
};
use rustix::fs::{Mode, OFlags};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use crate::execute::RejectedCall;
use crate::state::StateDir;

const RESTART_INTERNAL_ERROR_MESSAGE: &str = "节点在调用结算前重启，未保存执行结果";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "state")]
enum CallRecord {
    Fenced,
    Pending {
        invocation_id: String,
        tool: String,
        catalog_digest: CatalogDigest,
        labels: Labels,
    },
    Settled {
        settled_at_ms: u64,
        receipt: Receipt,
    },
    Expired {
        outcome: OutcomeKind,
    },
}

struct InFlightCall {
    conn_id: u64,
    cancelled: Arc<AtomicBool>,
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

/// 节点持久调用记录账本；登记、围栏、查询与结算均由单把互斥锁串行化。
pub struct Ledger {
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

        recover_calls(&calls_dir, &calls_dir_file, receipt_retention_ms)?;

        Ok(Self {
            calls_dir,
            calls_dir_file,
            receipt_retention_ms,
            in_flight: Mutex::new(HashMap::new()),
        })
    }

    /// 按查重优先顺序核对传入校验结果，全部通过后持久登记 `Pending` 并记入在途表。
    pub fn admit_execute(
        &self,
        conn_id: u64,
        invocation_id: String,
        catalog_digest: CatalogDigest,
        tool: String,
        labels: Labels,
        validation: Result<String, RejectedCall>,
    ) -> Result<AdmittedCall, RejectedCall> {
        let mut in_flight = self.lock_in_flight();
        let file_name = record_file_name(&invocation_id);
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

        let pending = CallRecord::Pending {
            invocation_id: invocation_id.clone(),
            tool: tool.clone(),
            catalog_digest: catalog_digest.clone(),
            labels: labels.clone(),
        };
        write_record(&self.calls_dir, &self.calls_dir_file, &file_name, &pending)
            .unwrap_or_else(|err| fatal_error(err));

        let cancelled = Arc::new(AtomicBool::new(false));
        in_flight.insert(
            invocation_id.clone(),
            InFlightCall {
                conn_id,
                cancelled: Arc::clone(&cancelled),
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

    /// 查询调用记录；未知 id 持久登记围栏后返回 `NeverExecuted`，到期回执迁移为 `Expired`。
    pub fn query_receipt(&self, invocation_id: &str) -> ReceiptLookup {
        let _in_flight = self.lock_in_flight();
        let file_name = record_file_name(invocation_id);
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

        let file_name = record_file_name(invocation_id);
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
            } => {
                let receipt = Receipt {
                    invocation_id,
                    tool,
                    catalog_digest,
                    labels,
                    outcome: ReceiptOutcome::Failed {
                        error: ToolError::new("InternalError", RESTART_INTERNAL_ERROR_MESSAGE),
                    },
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

fn record_file_name(invocation_id: &str) -> String {
    let digest = Sha256::digest(invocation_id.as_bytes());
    let mut name = String::with_capacity(64 + 5);
    for byte in digest {
        let _ = write!(&mut name, "{byte:02x}");
    }
    name.push_str(".json");
    name
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
