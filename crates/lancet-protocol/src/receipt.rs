// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 完整回执与单帧上限截断、`residue` 约束与 16 KiB 回执摘要裁剪。

use std::fmt;

use serde::{Deserialize, Deserializer, Serialize};

use crate::digest::CatalogDigest;
use crate::environment::EffectKind;
use crate::frame::{Frame, MAX_FRAME_BYTES, json_len_within};
use crate::message::{Labels, OutcomeKind, ReceiptLookup, Response, ToolError};

/// `receipt_summary` 紧凑 JSON 序列化的最大字节数（16 KiB）。
pub const MAX_RECEIPT_SUMMARY_BYTES: usize = 16 * 1024;

/// 变更项的残余（没有逆或逆所需材料未被保留的部分）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum Residue {
    None,
    NotRetained,
    Regenerable,
    InternalState,
    External,
}

/// 依据协议表核对 `residue` 是否允许出现在指定的 `EffectKind` 下。
pub const fn is_residue_allowed(kind: EffectKind, residue: Residue) -> bool {
    match kind {
        EffectKind::Write => matches!(
            residue,
            Residue::None | Residue::NotRetained | Residue::Regenerable | Residue::InternalState
        ),
        EffectKind::Exec => matches!(residue, Residue::InternalState | Residue::External),
        EffectKind::Emit => matches!(residue, Residue::External),
    }
}

/// 完整回执中的最终结局（不递归包含 `receipt_summary`）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum ReceiptOutcome {
    Succeeded { content: serde_json::Value },
    Failed { error: ToolError },
    Cancelled,
    Deadline,
    Indeterminate { reason: String },
}

impl ReceiptOutcome {
    /// 由最终结局推导 [`OutcomeKind`]。
    pub const fn kind(&self) -> OutcomeKind {
        match self {
            Self::Succeeded { .. } => OutcomeKind::Succeeded,
            Self::Failed { .. } => OutcomeKind::Failed,
            Self::Cancelled => OutcomeKind::Cancelled,
            Self::Deadline => OutcomeKind::Deadline,
            Self::Indeterminate { .. } => OutcomeKind::Indeterminate,
        }
    }
}

/// 子步骤摘要。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StepSummary {
    pub tool: String,
    pub outcome: OutcomeKind,
}

/// 单个 effect 条目中的变更项。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Change {
    pub target: String,
    pub action: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diff: Option<String>,
    #[serde(default)]
    pub diff_truncated: bool,
    pub residue: Residue,
}

/// 一次调用针对单个句柄及 effect 种类的变更记录。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EffectEntry {
    handle: String,
    kind: EffectKind,
    changes: Vec<Change>,
}

/// 构造或解码 [`EffectEntry`] 时的校验错误。
#[derive(Debug, PartialEq, Eq)]
pub enum EffectEntryError {
    InvalidResidue { kind: EffectKind, residue: Residue },
    MissingExternalDescription,
}

impl fmt::Display for EffectEntryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidResidue { kind, residue } => {
                write!(f, "effect 种类 {kind:?} 不允许携带 residue {residue:?}")
            }
            Self::MissingExternalDescription => {
                write!(f, "External residue 必须提供非空的 target 与 action 描述")
            }
        }
    }
}

impl std::error::Error for EffectEntryError {}

impl EffectEntry {
    /// 构造并校验 effect 条目与各变更项的 `residue` 合法范围。
    pub fn new(
        handle: impl Into<String>,
        kind: EffectKind,
        changes: Vec<Change>,
    ) -> Result<Self, EffectEntryError> {
        for change in &changes {
            if !is_residue_allowed(kind, change.residue) {
                return Err(EffectEntryError::InvalidResidue {
                    kind,
                    residue: change.residue,
                });
            }
            if change.residue == Residue::External
                && (change.target.is_empty() || change.action.is_empty())
            {
                return Err(EffectEntryError::MissingExternalDescription);
            }
        }
        Ok(Self {
            handle: handle.into(),
            kind,
            changes,
        })
    }

    pub fn handle(&self) -> &str {
        &self.handle
    }

    pub fn kind(&self) -> EffectKind {
        self.kind
    }

    pub fn changes(&self) -> &[Change] {
        &self.changes
    }
}

#[derive(Deserialize)]
struct RawEffectEntry {
    handle: String,
    kind: EffectKind,
    changes: Vec<Change>,
}

impl<'de> Deserialize<'de> for EffectEntry {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = RawEffectEntry::deserialize(deserializer)?;
        Self::new(raw.handle, raw.kind, raw.changes).map_err(serde::de::Error::custom)
    }
}

/// 节点持久化保存的完整调用回执（不记 `seam` 跨越）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Receipt {
    pub invocation_id: String,
    pub tool: String,
    pub catalog_digest: CatalogDigest,
    pub labels: Labels,
    pub outcome: ReceiptOutcome,
    pub steps: Vec<StepSummary>,
    pub effects: Vec<EffectEntry>,
    /// 为 `true` 时表示回执已按单帧上限截断，被截去的内容不能经 `receipt` 取回。
    pub truncated: bool,
}

impl Receipt {
    /// 由最终结局推导 [`OutcomeKind`]。
    pub const fn outcome_kind(&self) -> OutcomeKind {
        self.outcome.kind()
    }

    /// 使 `receipt` 回复的帧负载不超过 [`MAX_FRAME_BYTES`]。
    ///
    /// 放得下时原样返回；超出时按与 [`ReceiptSummary::from_receipt`] 相同的顺序截断 `steps`
    /// 与 `effects`（先裁剪 `diff`，再裁剪条目），并置 `truncated = true`。调用身份、`labels`
    /// 与结局原样保留；它们本身超过上限时，返回的回执仍会被 [`crate::write_frame`] 拒绝编码。
    pub fn truncate_to_frame(self) -> Self {
        let max_len = max_receipt_len();
        if json_len_within(&self, max_len)
            .expect("回执序列化不应失败")
            .is_some()
        {
            return self;
        }

        let Self {
            invocation_id,
            tool,
            catalog_digest,
            labels,
            outcome,
            steps,
            effects,
            truncated: _,
        } = self;
        let mut fitted = Self {
            invocation_id,
            tool,
            catalog_digest,
            labels,
            outcome,
            steps: Vec::new(),
            effects: Vec::new(),
            // 与摘要相同，按较长的 false 预留标记空间。
            truncated: false,
        };
        let budget = max_len.saturating_sub(exact_json_len(&fitted));
        (fitted.steps, fitted.effects) = trim_steps_and_effects(&steps, &effects, budget);
        fitted.truncated = true;
        fitted
    }
}

/// 完整回执紧凑 JSON 的最大字节数：单帧上限减去 `receipt_result` 回复与帧的包装。
///
/// 回执原样嵌套在帧中，包装的字节数与回执内容无关；按最长的 `request_id` 计算，
/// 使任一请求编号下的帧都不超限。
fn max_receipt_len() -> usize {
    let probe = Receipt {
        invocation_id: String::new(),
        tool: String::new(),
        catalog_digest: CatalogDigest::from_sha256_bytes([0; 32]),
        labels: Labels::new(),
        outcome: ReceiptOutcome::Cancelled,
        steps: Vec::new(),
        effects: Vec::new(),
        truncated: false,
    };
    let receipt_len = exact_json_len(&probe);
    let frame_len = exact_json_len(&Frame {
        request_id: u64::MAX,
        message: Response::ReceiptResult {
            lookup: ReceiptLookup::Settled { receipt: probe },
        },
    });
    MAX_FRAME_BYTES - (frame_len - receipt_len)
}

/// 随 `execute_result` 返回的紧凑回执摘要，紧凑 JSON 序列化不超过 16 KiB。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReceiptSummary {
    steps: Vec<StepSummary>,
    effects: Vec<EffectEntry>,
    truncated: bool,
}

/// 解码 [`ReceiptSummary`] 时的校验错误。
#[derive(Debug)]
pub enum ReceiptSummaryError {
    TooLarge { max: usize },
    Serialize(serde_json::Error),
}

impl fmt::Display for ReceiptSummaryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooLarge { max } => {
                write!(f, "回执摘要紧凑 JSON 序列化超过上限 {max} 字节")
            }
            Self::Serialize(err) => write!(f, "回执摘要序列化失败：{err}"),
        }
    }
}

impl std::error::Error for ReceiptSummaryError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Serialize(err) => Some(err),
            Self::TooLarge { .. } => None,
        }
    }
}

#[derive(Serialize)]
struct ReceiptSummaryRef<'a> {
    steps: &'a [StepSummary],
    effects: &'a [EffectEntry],
    truncated: bool,
}

#[derive(Serialize)]
struct ChangeView<'a> {
    target: &'a str,
    action: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    diff: Option<&'a str>,
    diff_truncated: bool,
    residue: Residue,
}

#[derive(Serialize)]
struct EffectEntryHeaderView<'a> {
    handle: &'a str,
    kind: EffectKind,
    changes: &'a [Change],
}

fn exact_json_len<T: Serialize>(value: &T) -> usize {
    json_len_within(value, usize::MAX)
        .expect("回执结构序列化不应失败")
        .expect("usize::MAX 不会超限")
}

/// 统计列表项紧凑 JSON 总长度（含元素间逗号）；超过 `limit` 立即返回 `None`，避免扫描海量条目。
fn measure_steps_within(steps: &[StepSummary], limit: usize) -> Option<usize> {
    let mut total = 0usize;
    for (idx, step) in steps.iter().enumerate() {
        let sep = usize::from(idx > 0);
        let remaining = limit.checked_sub(total + sep)?;
        let item_len = json_len_within(step, remaining).ok()??;
        total += sep + item_len;
    }
    Some(total)
}

fn strip_change_diff(change: &Change) -> Change {
    Change {
        target: change.target.clone(),
        action: change.action.clone(),
        diff: None,
        diff_truncated: change.diff_truncated || change.diff.is_some(),
        residue: change.residue,
    }
}

fn change_view_no_diff(change: &Change) -> ChangeView<'_> {
    ChangeView {
        target: &change.target,
        action: &change.action,
        diff: None,
        diff_truncated: change.diff_truncated || change.diff.is_some(),
        residue: change.residue,
    }
}

fn measure_effects_no_diff_within(effects: &[EffectEntry], limit: usize) -> Option<usize> {
    let mut total = 0usize;
    for (e_idx, entry) in effects.iter().enumerate() {
        let e_sep = usize::from(e_idx > 0);
        let header = EffectEntryHeaderView {
            handle: &entry.handle,
            kind: entry.kind,
            changes: &[],
        };
        let remaining = limit.checked_sub(total + e_sep)?;
        let mut entry_len = json_len_within(&header, remaining).ok()??;
        for (c_idx, change) in entry.changes.iter().enumerate() {
            let c_sep = usize::from(c_idx > 0);
            let c_rem = limit.checked_sub(total + e_sep + entry_len + c_sep)?;
            let c_len = json_len_within(&change_view_no_diff(change), c_rem).ok()??;
            entry_len += c_sep + c_len;
        }
        total += e_sep + entry_len;
    }
    Some(total)
}

fn take_steps_within(steps: &[StepSummary], limit: usize) -> (Vec<StepSummary>, usize, bool) {
    let mut kept = Vec::new();
    let mut used = 0usize;
    for (idx, step) in steps.iter().enumerate() {
        let sep = usize::from(idx > 0);
        let Some(remaining) = limit.checked_sub(used + sep) else {
            return (kept, used, true);
        };
        match json_len_within(step, remaining).expect("步骤序列化不应失败") {
            Some(item_len) => {
                kept.push(step.clone());
                used += sep + item_len;
            }
            None => return (kept, used, true),
        }
    }
    (kept, used, false)
}

fn take_effects_no_diff_within(
    effects: &[EffectEntry],
    limit: usize,
) -> (Vec<EffectEntry>, usize, bool) {
    let mut kept = Vec::new();
    let mut used = 0usize;
    for (e_idx, entry) in effects.iter().enumerate() {
        let e_sep = usize::from(e_idx > 0);
        let Some(remaining) = limit.checked_sub(used + e_sep) else {
            return (kept, used, true);
        };
        let header = EffectEntryHeaderView {
            handle: &entry.handle,
            kind: entry.kind,
            changes: &[],
        };
        let Some(header_len) = json_len_within(&header, remaining).expect("effect 序列化不应失败")
        else {
            return (kept, used, true);
        };

        if entry.changes.is_empty() {
            kept.push(EffectEntry {
                handle: entry.handle.clone(),
                kind: entry.kind,
                changes: Vec::new(),
            });
            used += e_sep + header_len;
            continue;
        }

        let mut entry_len = header_len;
        let mut kept_changes = Vec::new();
        let mut change_dropped = false;
        for (c_idx, change) in entry.changes.iter().enumerate() {
            let c_sep = usize::from(c_idx > 0);
            let Some(c_rem) = limit.checked_sub(used + e_sep + entry_len + c_sep) else {
                change_dropped = true;
                break;
            };
            let view = change_view_no_diff(change);
            match json_len_within(&view, c_rem).expect("变更项序列化不应失败") {
                Some(c_len) => {
                    kept_changes.push(strip_change_diff(change));
                    entry_len += c_sep + c_len;
                }
                None => {
                    change_dropped = true;
                    break;
                }
            }
        }

        if kept_changes.is_empty() {
            return (kept, used, true);
        }
        kept.push(EffectEntry {
            handle: entry.handle.clone(),
            kind: entry.kind,
            changes: kept_changes,
        });
        used += e_sep + entry_len;
        if change_dropped {
            return (kept, used, true);
        }
    }
    (kept, used, false)
}

/// 返回单个字符在 `serde_json` 紧凑 JSON 字符串字面量中所占的 UTF-8 字节数。
const fn json_escaped_char_len(ch: char) -> usize {
    match ch {
        '"' | '\\' | '\x08' | '\x0c' | '\n' | '\r' | '\t' => 2,
        c if (c as u32) < 0x20 => 6,
        c => c.len_utf8(),
    }
}

/// 尽可能把原 `diff` 完整或按 UTF-8 字符边界安全裁剪后填回已保留的 `effects` 中。
fn fill_diffs_within_budget(
    orig_effects: &[EffectEntry],
    kept_effects: &mut [EffectEntry],
    mut remaining: usize,
) {
    for (orig_entry, kept_entry) in orig_effects.iter().zip(kept_effects.iter_mut()) {
        for (orig_change, kept_change) in
            orig_entry.changes.iter().zip(kept_entry.changes.iter_mut())
        {
            let Some(orig_diff) = &orig_change.diff else {
                continue;
            };

            let without_diff_len = exact_json_len(kept_change);
            let full_view = ChangeView {
                target: &orig_change.target,
                action: &orig_change.action,
                diff: Some(orig_diff),
                diff_truncated: orig_change.diff_truncated,
                residue: orig_change.residue,
            };
            if let Some(full_len) =
                json_len_within(&full_view, without_diff_len + remaining).expect("序列化不应失败")
            {
                kept_change.diff = Some(orig_diff.clone());
                kept_change.diff_truncated = orig_change.diff_truncated;
                remaining = remaining + without_diff_len - full_len;
                continue;
            }

            let empty_diff_view = ChangeView {
                target: &orig_change.target,
                action: &orig_change.action,
                diff: Some(""),
                diff_truncated: true,
                residue: orig_change.residue,
            };
            let empty_diff_len = exact_json_len(&empty_diff_view);
            let overhead = empty_diff_len.saturating_sub(without_diff_len);
            if remaining <= overhead {
                return;
            }

            let max_escaped_bytes = remaining - overhead;
            let mut used_bytes = 0usize;
            let mut prefix_end = 0usize;
            for (idx, ch) in orig_diff.char_indices() {
                let cost = json_escaped_char_len(ch);
                if used_bytes + cost > max_escaped_bytes {
                    break;
                }
                used_bytes += cost;
                prefix_end = idx + ch.len_utf8();
            }
            if prefix_end == 0 {
                return;
            }

            // 以 serde_json 实际输出校验一遍，确保多字节与转义计算零偏差。
            while prefix_end > 0 {
                let candidate_view = ChangeView {
                    target: &orig_change.target,
                    action: &orig_change.action,
                    diff: Some(&orig_diff[..prefix_end]),
                    diff_truncated: true,
                    residue: orig_change.residue,
                };
                if json_len_within(&candidate_view, without_diff_len + remaining)
                    .expect("序列化不应失败")
                    .is_some()
                {
                    kept_change.diff = Some(orig_diff[..prefix_end].to_owned());
                    kept_change.diff_truncated = true;
                    break;
                }
                let prev_char_start = orig_diff[..prefix_end]
                    .char_indices()
                    .next_back()
                    .map_or(0, |(i, _)| i);
                prefix_end = prev_char_start;
            }
            return;
        }
    }
}

/// 把 `steps` 与 `effects` 裁进 `budget` 字节（两个列表各项的紧凑 JSON 及项间逗号合计）。
///
/// 条目不带 `diff` 能放下时保留全部条目，再按顺序填回 `diff`：放得下的完整保留，第一个放不下的
/// 截短，其后的去掉。条目本身也放不下时去掉全部 `diff`，优先完整保留较小的一侧，其余空间分给
/// 另一侧，各自按顺序保留前缀条目。去掉或截短的 `diff` 置 `diff_truncated = true`。
fn trim_steps_and_effects(
    steps: &[StepSummary],
    effects: &[EffectEntry],
    budget: usize,
) -> (Vec<StepSummary>, Vec<EffectEntry>) {
    let steps_fit = measure_steps_within(steps, budget);
    let effects_fit = measure_effects_no_diff_within(effects, budget);

    if let (Some(sb), Some(eb)) = (steps_fit, effects_fit)
        && sb + eb <= budget
    {
        let mut kept_effects: Vec<EffectEntry> = effects
            .iter()
            .map(|entry| EffectEntry {
                handle: entry.handle.clone(),
                kind: entry.kind,
                changes: entry.changes.iter().map(strip_change_diff).collect(),
            })
            .collect();
        fill_diffs_within_budget(effects, &mut kept_effects, budget - sb - eb);
        return (steps.to_vec(), kept_effects);
    }

    // 条目本身也超出预算：优先保证较小一侧完整保留，其余空间分配给另一侧。
    let steps_budget = match (steps_fit, effects_fit) {
        (Some(sb), _) if sb <= budget / 2 => sb,
        (_, Some(eb)) if eb <= budget / 2 => budget - eb,
        _ => budget / 2,
    };
    let (kept_steps, used_steps, _) = take_steps_within(steps, steps_budget);
    let (kept_effects, _, _) = take_effects_no_diff_within(effects, budget - used_steps);
    (kept_steps, kept_effects)
}

impl ReceiptSummary {
    /// 从完整 [`Receipt`] 构造紧凑摘要，优先裁剪 `diff` 再裁剪摘要条目，确保紧凑 JSON 不超 16 KiB。
    pub fn from_receipt(receipt: &Receipt) -> Self {
        let lossless = ReceiptSummaryRef {
            steps: &receipt.steps,
            effects: &receipt.effects,
            truncated: false,
        };
        if json_len_within(&lossless, MAX_RECEIPT_SUMMARY_BYTES)
            .expect("回执摘要序列化不应失败")
            .is_some()
        {
            return Self {
                steps: receipt.steps.clone(),
                effects: receipt.effects.clone(),
                truncated: false,
            };
        }

        // 按较长的 false 预留标记空间，避免仅靠改为 true 缩短一字节而谎报内容已裁剪。
        let skeleton_len = exact_json_len(&ReceiptSummaryRef {
            steps: &[],
            effects: &[],
            truncated: false,
        });
        let (steps, effects) = trim_steps_and_effects(
            &receipt.steps,
            &receipt.effects,
            MAX_RECEIPT_SUMMARY_BYTES - skeleton_len,
        );
        Self {
            steps,
            effects,
            truncated: true,
        }
    }

    pub fn steps(&self) -> &[StepSummary] {
        &self.steps
    }

    pub fn effects(&self) -> &[EffectEntry] {
        &self.effects
    }

    /// 为 `true` 时表示摘要已裁剪，完整内容须以同一 `invocation_id` 调用 `receipt` 获取。
    pub fn is_truncated(&self) -> bool {
        self.truncated
    }
}

#[derive(Deserialize)]
struct RawReceiptSummary {
    steps: Vec<StepSummary>,
    effects: Vec<EffectEntry>,
    truncated: bool,
}

impl<'de> Deserialize<'de> for ReceiptSummary {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = RawReceiptSummary::deserialize(deserializer)?;
        let summary_ref = ReceiptSummaryRef {
            steps: &raw.steps,
            effects: &raw.effects,
            truncated: raw.truncated,
        };
        let within = json_len_within(&summary_ref, MAX_RECEIPT_SUMMARY_BYTES)
            .map_err(serde::de::Error::custom)?;
        if within.is_none() {
            return Err(serde::de::Error::custom(ReceiptSummaryError::TooLarge {
                max: MAX_RECEIPT_SUMMARY_BYTES,
            }));
        }
        Ok(Self {
            steps: raw.steps,
            effects: raw.effects,
            truncated: raw.truncated,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::digest::CatalogDigest;
    use crate::frame::{FrameError, write_frame};
    use std::collections::BTreeMap;

    fn sample_receipt(steps: Vec<StepSummary>, effects: Vec<EffectEntry>) -> Receipt {
        Receipt {
            invocation_id: "inv-1".to_owned(),
            tool: "act".to_owned(),
            catalog_digest: CatalogDigest::from_sha256_bytes([0x22; 32]),
            labels: BTreeMap::from([("turn".to_owned(), serde_json::json!(1))]),
            outcome: ReceiptOutcome::Succeeded {
                content: serde_json::json!("ok"),
            },
            steps,
            effects,
            truncated: false,
        }
    }

    fn write_entry(changes: Vec<Change>) -> EffectEntry {
        EffectEntry::new("ws", EffectKind::Write, changes).unwrap()
    }

    /// 按最长的 `request_id` 编码 `receipt` 回复，返回帧负载字节数；超过单帧上限时返回编码错误。
    fn receipt_result_payload_len(receipt: &Receipt) -> Result<usize, FrameError> {
        let frame = Frame {
            request_id: u64::MAX,
            message: Response::ReceiptResult {
                lookup: ReceiptLookup::Settled {
                    receipt: receipt.clone(),
                },
            },
        };
        let mut encoded = Vec::new();
        write_frame(&mut encoded, &frame)?;
        Ok(encoded.len() - 4)
    }

    /// 去掉 `steps`、`effects` 与 `truncated` 后的回执，用于核对截断不触及其余字段。
    fn without_lists(receipt: &Receipt) -> Receipt {
        Receipt {
            steps: Vec::new(),
            effects: Vec::new(),
            truncated: false,
            ..receipt.clone()
        }
    }

    #[test]
    fn enforces_residue_combinations_and_external_description() {
        let valid = [
            (EffectKind::Write, Residue::None),
            (EffectKind::Write, Residue::NotRetained),
            (EffectKind::Write, Residue::Regenerable),
            (EffectKind::Write, Residue::InternalState),
            (EffectKind::Exec, Residue::InternalState),
            (EffectKind::Exec, Residue::External),
            (EffectKind::Emit, Residue::External),
        ];
        for (kind, residue) in valid {
            let entry = EffectEntry::new(
                "h",
                kind,
                vec![Change {
                    target: "api.example.com".to_owned(),
                    action: "POST /submit".to_owned(),
                    diff: None,
                    diff_truncated: false,
                    residue,
                }],
            );
            assert!(entry.is_ok(), "合法组合 ({kind:?}, {residue:?}) 应当通过");
        }

        let invalid = [
            (EffectKind::Write, Residue::External),
            (EffectKind::Exec, Residue::None),
            (EffectKind::Exec, Residue::NotRetained),
            (EffectKind::Exec, Residue::Regenerable),
            (EffectKind::Emit, Residue::None),
            (EffectKind::Emit, Residue::NotRetained),
            (EffectKind::Emit, Residue::Regenerable),
            (EffectKind::Emit, Residue::InternalState),
        ];
        for (kind, residue) in invalid {
            assert!(
                EffectEntry::new(
                    "h",
                    kind,
                    vec![Change {
                        target: "t".to_owned(),
                        action: "a".to_owned(),
                        diff: None,
                        diff_truncated: false,
                        residue,
                    }],
                )
                .is_err(),
                "非法组合 ({kind:?}, {residue:?}) 必须被拒绝"
            );

            let raw_json = serde_json::json!({
                "handle": "h",
                "kind": kind,
                "changes": [{
                    "target": "t",
                    "action": "a",
                    "diff_truncated": false,
                    "residue": residue
                }]
            })
            .to_string();
            assert!(
                serde_json::from_str::<EffectEntry>(&raw_json).is_err(),
                "直接反序列化非法组合 ({kind:?}, {residue:?}) 必须被拒绝"
            );
        }

        assert!(
            EffectEntry::new(
                "egress",
                EffectKind::Emit,
                vec![Change {
                    target: "".to_owned(),
                    action: "GET /".to_owned(),
                    diff: None,
                    diff_truncated: false,
                    residue: Residue::External,
                }],
            )
            .is_err(),
            "External 缺少 target 描述必须被拒绝"
        );
    }

    #[test]
    fn receipt_summary_lossless_when_small_and_truncates_large_diff_preserving_original() {
        let small_entry = EffectEntry::new(
            "ws",
            EffectKind::Write,
            vec![Change {
                target: "src/lib.rs".to_owned(),
                action: "edit".to_owned(),
                diff: Some("+println!(\"你好\");".to_owned()),
                diff_truncated: false,
                residue: Residue::None,
            }],
        )
        .unwrap();
        let small = sample_receipt(
            vec![StepSummary {
                tool: "fs.edit".to_owned(),
                outcome: OutcomeKind::Succeeded,
            }],
            vec![small_entry],
        );
        let small_summary = ReceiptSummary::from_receipt(&small);
        assert!(
            !small_summary.is_truncated(),
            "小摘要应无损且 truncated=false"
        );
        assert_eq!(small_summary.steps(), small.steps.as_slice());
        assert_eq!(small_summary.effects(), small.effects.as_slice());

        // 构造带转义字符与多字节中文的超大 diff（> 16 KiB）。
        let huge_diff = "\"中文\\测试\n".repeat(4000);
        let large = sample_receipt(
            vec![StepSummary {
                tool: "fs.edit".to_owned(),
                outcome: OutcomeKind::Succeeded,
            }],
            vec![
                EffectEntry::new(
                    "ws",
                    EffectKind::Write,
                    vec![Change {
                        target: "src/big.rs".to_owned(),
                        action: "write".to_owned(),
                        diff: Some(huge_diff),
                        diff_truncated: false,
                        residue: Residue::None,
                    }],
                )
                .unwrap(),
            ],
        );
        let original_clone = large.clone();
        let summary = ReceiptSummary::from_receipt(&large);
        let serialized = serde_json::to_vec(&summary).unwrap();
        assert!(
            serialized.len() <= MAX_RECEIPT_SUMMARY_BYTES,
            "摘要字节数 {} 不得超过 {}",
            serialized.len(),
            MAX_RECEIPT_SUMMARY_BYTES
        );
        assert!(summary.is_truncated(), "裁剪后必须标明 truncated=true");
        assert_eq!(summary.steps().len(), 1, "优先裁剪 diff 时应保留子步骤条目");
        assert_eq!(
            summary.effects().len(),
            1,
            "优先裁剪 diff 时应保留 effect 条目"
        );
        assert!(
            summary.effects()[0].changes()[0].diff.is_some(),
            "剩余预算应尽可能保留安全的 UTF-8 diff 前缀"
        );
        assert!(
            summary.effects()[0].changes()[0].diff_truncated,
            "被裁剪 diff 的变更项应标记 diff_truncated=true"
        );
        assert_eq!(large, original_clone, "构造摘要不得修改原完整回执");
    }

    #[test]
    fn receipt_summary_exact_16384_boundary_many_entries_and_deserialize_limit() {
        // 1) 大量条目（10,000 条）裁剪后不超过 16384 字节。
        let many_steps: Vec<StepSummary> = (0..10_000)
            .map(|i| StepSummary {
                tool: format!("fs.read#{i}"),
                outcome: OutcomeKind::Succeeded,
            })
            .collect();
        let many_effects: Vec<EffectEntry> = (0..2000)
            .map(|i| {
                EffectEntry::new(
                    "ws",
                    EffectKind::Write,
                    vec![Change {
                        target: format!("file_{i}.rs"),
                        action: "write".to_owned(),
                        diff: Some("x".repeat(64)),
                        diff_truncated: false,
                        residue: Residue::None,
                    }],
                )
                .unwrap()
            })
            .collect();
        let huge_receipt = sample_receipt(many_steps, many_effects);
        let summary = ReceiptSummary::from_receipt(&huge_receipt);
        let bytes = serde_json::to_vec(&summary).unwrap();
        assert!(bytes.len() <= MAX_RECEIPT_SUMMARY_BYTES);
        assert!(summary.is_truncated());
        assert!(!summary.steps().is_empty(), "应保留部分步骤条目");
        assert!(!summary.effects().is_empty(), "应保留部分 effect 条目");

        // 2) Deserialize 必须拒绝超过 16384 字节的摘要，不静默丢弃。
        let over_json = serde_json::json!({
            "steps": [{ "tool": "a".repeat(MAX_RECEIPT_SUMMARY_BYTES), "outcome": "Succeeded" }],
            "effects": [],
            "truncated": true
        })
        .to_string();
        assert!(
            serde_json::from_str::<ReceiptSummary>(&over_json).is_err(),
            "直接反序列化超过 16384 字节的 ReceiptSummary 必须失败"
        );

        // 3) 恰好 16384 字节无损通过，多 1 字节触发裁剪，且直接反序列化 16385 字节失败。
        let probe = serde_json::json!({
            "steps": [{ "tool": "", "outcome": "Succeeded" }],
            "effects": [],
            "truncated": false
        })
        .to_string();
        let exact_tool_len = MAX_RECEIPT_SUMMARY_BYTES - probe.len();
        let exact_receipt = sample_receipt(
            vec![StepSummary {
                tool: "t".repeat(exact_tool_len),
                outcome: OutcomeKind::Succeeded,
            }],
            vec![],
        );
        let exact_summary = ReceiptSummary::from_receipt(&exact_receipt);
        let exact_bytes = serde_json::to_vec(&exact_summary).unwrap();
        assert_eq!(exact_bytes.len(), MAX_RECEIPT_SUMMARY_BYTES);
        assert!(!exact_summary.is_truncated(), "恰好 16384 字节应无损保留");
        let parsed: ReceiptSummary = serde_json::from_slice(&exact_bytes)
            .expect("恰好 16384 字节的 ReceiptSummary 反序列化应当成功");
        assert_eq!(parsed, exact_summary);

        let over_one_receipt = sample_receipt(
            vec![StepSummary {
                tool: "t".repeat(exact_tool_len + 1),
                outcome: OutcomeKind::Succeeded,
            }],
            vec![],
        );
        let over_one_summary = ReceiptSummary::from_receipt(&over_one_receipt);
        assert!(over_one_summary.is_truncated(), "超过 1 字节必须触发裁剪");
        assert!(
            over_one_summary.steps().len() < over_one_receipt.steps.len(),
            "truncated 必须表示内容已裁剪，不能只靠布尔值编码缩短 1 字节"
        );
        assert!(
            serde_json::to_vec(&over_one_summary).unwrap().len() <= MAX_RECEIPT_SUMMARY_BYTES,
            "裁剪后必须满足上限"
        );

        // 4) 通过解码 execute_result 验证超限摘要不会在消息边界被静默放行或截断。
        let oversized_execute_result = serde_json::json!({
            "type": "execute_result",
            "outcome": {
                "kind": "Succeeded",
                "content": null,
                "receipt_summary": {
                    "steps": [{ "tool": "x".repeat(MAX_RECEIPT_SUMMARY_BYTES), "outcome": "Succeeded" }],
                    "effects": [],
                    "truncated": false
                }
            }
        })
        .to_string();
        assert!(
            serde_json::from_str::<crate::message::Response>(&oversized_execute_result).is_err(),
            "execute_result 中超限的 receipt_summary 必须在反序列化时拒绝"
        );

        // 5) 边界裁剪多字节 UTF-8 + 转义 diff 时不切碎字符且总长严格不超过 16384。
        let multibyte_diff = "中\"文\n😀".repeat(1500);
        let mb_receipt = sample_receipt(
            vec![],
            vec![
                EffectEntry::new(
                    "ws",
                    EffectKind::Write,
                    vec![Change {
                        target: "src/utf8.rs".to_owned(),
                        action: "edit".to_owned(),
                        diff: Some(multibyte_diff),
                        diff_truncated: false,
                        residue: Residue::None,
                    }],
                )
                .unwrap(),
            ],
        );
        let mb_summary = ReceiptSummary::from_receipt(&mb_receipt);
        let mb_bytes = serde_json::to_vec(&mb_summary).unwrap();
        assert!(mb_bytes.len() <= MAX_RECEIPT_SUMMARY_BYTES);
        assert!(mb_summary.is_truncated());
        let mb_decoded: ReceiptSummary =
            serde_json::from_slice(&mb_bytes).expect("裁剪后的多字节摘要应可正常反序列化");
        assert_eq!(mb_decoded, mb_summary);
    }

    #[test]
    fn receipt_truncate_to_frame_keeps_fitting_receipt_and_cuts_diffs_before_entries() {
        let small = sample_receipt(
            vec![StepSummary {
                tool: "fs.edit".to_owned(),
                outcome: OutcomeKind::Succeeded,
            }],
            vec![write_entry(vec![Change {
                target: "src/lib.rs".to_owned(),
                action: "edit".to_owned(),
                diff: Some("+1\n".to_owned()),
                diff_truncated: false,
                residue: Residue::NotRetained,
            }])],
        );
        assert_eq!(
            small.clone().truncate_to_frame(),
            small,
            "放得进单帧的回执应原样返回"
        );

        // 三个变更项各带 3 MiB diff，合计超过 8 MiB 单帧上限；不带 diff 的条目远小于上限。
        let diff = "x".repeat(3 * 1024 * 1024);
        let changes = (0..3)
            .map(|i| Change {
                target: format!("src/gen_{i}.rs"),
                action: "write".to_owned(),
                diff: Some(diff.clone()),
                diff_truncated: false,
                residue: Residue::NotRetained,
            })
            .collect();
        let large = sample_receipt(
            vec![StepSummary {
                tool: "fs.write".to_owned(),
                outcome: OutcomeKind::Succeeded,
            }],
            vec![write_entry(changes)],
        );
        assert!(
            matches!(
                receipt_result_payload_len(&large),
                Err(FrameError::FrameTooLarge { .. })
            ),
            "未截断的回执应超过单帧上限"
        );

        let fitted = large.clone().truncate_to_frame();
        let payload_len = receipt_result_payload_len(&fitted).expect("截断后的回执应能编码成一帧");
        assert!(payload_len <= MAX_FRAME_BYTES);
        assert!(fitted.truncated, "截断后必须标明 truncated=true");
        assert!(
            without_lists(&fitted) == without_lists(&large),
            "调用身份、labels 与结局不截断"
        );
        assert_eq!(
            fitted.steps, large.steps,
            "只裁 diff 就能放下时保留全部子步骤"
        );
        let kept = fitted.effects[0].changes();
        let original = large.effects[0].changes();
        assert_eq!(kept.len(), 3, "只裁 diff 就能放下时保留全部变更项");
        assert!(
            kept[0] == original[0] && kept[1] == original[1],
            "预算内的 diff 按顺序完整保留"
        );
        let cut = kept[2]
            .diff
            .as_deref()
            .expect("剩余预算应保留第三项 diff 的前缀");
        assert!(cut.len() < diff.len() && diff.starts_with(cut));
        assert!(
            kept[2].diff_truncated,
            "被裁剪 diff 的变更项应标记 diff_truncated=true"
        );
    }

    #[test]
    fn receipt_truncate_to_frame_exact_frame_boundary_and_entries_beyond_frame() {
        // 1) 帧负载（按最长的 request_id 计）恰好 8 MiB 时原样返回，多 1 字节即截断。
        let probe = sample_receipt(
            vec![StepSummary {
                tool: String::new(),
                outcome: OutcomeKind::Succeeded,
            }],
            vec![],
        );
        let exact_tool_len = MAX_FRAME_BYTES - receipt_result_payload_len(&probe).unwrap();
        let exact = sample_receipt(
            vec![StepSummary {
                tool: "t".repeat(exact_tool_len),
                outcome: OutcomeKind::Succeeded,
            }],
            vec![],
        );
        assert_eq!(receipt_result_payload_len(&exact).unwrap(), MAX_FRAME_BYTES);
        assert!(
            exact.clone().truncate_to_frame() == exact,
            "恰好 8 MiB 的回执应原样返回"
        );

        let over_one = sample_receipt(
            vec![StepSummary {
                tool: "t".repeat(exact_tool_len + 1),
                outcome: OutcomeKind::Succeeded,
            }],
            vec![],
        );
        assert!(receipt_result_payload_len(&over_one).is_err());
        let fitted = over_one.truncate_to_frame();
        assert!(fitted.truncated, "超过 1 字节必须截断");
        assert!(fitted.steps.is_empty(), "放不下的子步骤整条截去");
        assert!(receipt_result_payload_len(&fitted).unwrap() <= MAX_FRAME_BYTES);

        // 2) 不带 diff 的条目本身超过单帧上限时，按顺序保留前缀条目。
        let changes = (0..120_000)
            .map(|i| Change {
                target: format!("src/file_{i:06}.rs"),
                action: "write".to_owned(),
                diff: None,
                diff_truncated: false,
                residue: Residue::NotRetained,
            })
            .collect();
        let many = sample_receipt(
            vec![StepSummary {
                tool: "fs.write".to_owned(),
                outcome: OutcomeKind::Succeeded,
            }],
            vec![write_entry(changes)],
        );
        assert!(receipt_result_payload_len(&many).is_err());
        let fitted = many.clone().truncate_to_frame();
        assert!(fitted.truncated);
        assert!(receipt_result_payload_len(&fitted).unwrap() <= MAX_FRAME_BYTES);
        assert_eq!(fitted.steps, many.steps, "较小的一侧完整保留");
        let kept = fitted.effects[0].changes();
        let all = many.effects[0].changes();
        assert!(
            !kept.is_empty() && kept.len() < all.len(),
            "应保留部分变更项，保留 {} 项",
            kept.len()
        );
        assert!(kept == &all[..kept.len()], "按顺序保留前缀条目");
    }
}
