// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! API 工具分发层：κ 核对、组件实例化、数值转换、资源收窄、ε 记录与模板核对、锁、`discard()` 与 `commit()`。

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, TryLockError};
use std::time::{Duration, Instant};

use anyhow::Context as _;
use lancet_protocol::{Change, EffectEntry, EffectKind, OutcomeKind, StepSummary};
use lancet_tool_decl::{
    ActContext, CoEffectItem, EffectItem, HandleGrant, HandleKind, ToolDecl, WitType,
};
use serde_json::Value;
use wasmtime::component::{
    Component, ComponentExportIndex, HasSelf, InstancePre, Linker, Resource, ResourceAny,
    ResourceTable, Type, Val,
};
use wasmtime::{Engine, Store};

use crate::act::{DispatchArgument, Interrupt, MemoryBudget, check_epoch_interrupt};
use crate::read_dir::ReadDir;
use crate::shadow::{
    DiscardError, ShadowDir, ShadowInterrupt, ShadowReadView, ShadowSyncError, WriteEffect,
};

wasmtime::component::bindgen!({
    world: "tool-host",
    path: "wit/lancet-host",
    with: {
        "lancet:host/handles.read-dir": ReadDirResource,
        "lancet:host/handles.write-dir": WriteDirResource,
    },
    imports: {
        default: trappable,
    },
});

/// 单次 API 工具组件调用的线性内存上限（128 MiB）。
pub const TOOL_MEMORY_LIMIT_BYTES: usize = 128 * 1024 * 1024;

/// 单次 API 工具调用交回脚本的成功值紧凑 JSON 字节上限（8 MiB）。
pub const MAX_TOOL_RESULT_BYTES: usize = 8 * 1024 * 1024;

/// 分发层按 `[[dir]]` 配置顺序持有的目录资源槽位。
#[derive(Clone)]
pub enum DirSlot {
    ReadOnly(ReadDir),
    Shadow(ShadowDir),
}

impl DirSlot {
    pub fn name(&self) -> &str {
        match self {
            Self::ReadOnly(dir) => dir.name(),
            Self::Shadow(shadow) => shadow.name(),
        }
    }

    pub fn handle_kind(&self) -> HandleKind {
        match self {
            Self::ReadOnly(_) => HandleKind::ReadDir,
            Self::Shadow(_) => HandleKind::WriteDir,
        }
    }
}

/// 压入工具组件 `ResourceTable` 的 `read-dir` 资源；可写槽位收窄为只读视图。
pub enum ReadDirResource {
    Host(ReadDir),
    Shadow(ShadowReadView),
}

/// 压入工具组件 `ResourceTable` 的 `write-dir` 资源，仅在形参为 `borrow<write-dir>` 时注入。
pub struct WriteDirResource {
    shadow: ShadowDir,
}

/// 已按槽位顺序持有的全部可写目录 `act` 级互斥锁。
pub struct WritableLocks<'a> {
    _guards: Vec<MutexGuard<'a, ()>>,
}

/// 等锁期间收到的中断类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaitInterrupt {
    Deadline,
    Cancelled,
}

/// 持锁后宿主同步期间收到的中断或真实 I/O 错误。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SyncError {
    Deadline,
    Cancelled,
    Failed(String),
}

/// 分发层检测到的节点内部契约违反（以 wasmtime 陷入中止整个 `act`）。
#[derive(Debug)]
pub(crate) enum DispatchTrap {
    EffectOutOfTemplate {
        tool: String,
        handle: String,
        kind: EffectKind,
    },
}

impl fmt::Display for DispatchTrap {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EffectOutOfTemplate { tool, handle, kind } => {
                write!(
                    f,
                    "工具 `{tool}` 在句柄 `{handle}` 上产生的 {kind:?} effect 越出模板"
                )
            }
        }
    }
}

impl std::error::Error for DispatchTrap {}

/// 待加载的工具组件声明与预编译产物。
pub struct ComponentArtifact<'a> {
    pub name: &'a str,
    pub decls: Vec<ToolDecl>,
    pub cwasm: &'a [u8],
}

/// 持有预链接 API 工具组件、目录资源槽位及对应调用授权上下文的分发器。
#[derive(Clone)]
pub struct Dispatcher {
    inner: Arc<DispatcherInner>,
}

struct DispatcherInner {
    engine: Engine,
    components: Vec<InstancePre<ToolStoreState>>,
    tools: HashMap<String, PreparedTool>,
    dirs: Vec<DirSlot>,
    context: ActContext,
}

struct PreparedTool {
    decl: ToolDecl,
    component_idx: usize,
    func_index: ComponentExportIndex,
}

struct ToolStoreState {
    memory: MemoryBudget,
    deadline: Instant,
    cancelled: Arc<AtomicBool>,
    table: ResourceTable,
    call_effects: Vec<EffectEntry>,
}

impl ToolStoreState {
    fn record_write_effect(&mut self, handle: &str, effect: WriteEffect) {
        let entry = EffectEntry::new(
            handle,
            EffectKind::Write,
            vec![Change {
                target: effect.target,
                action: effect.action.to_owned(),
                diff: None,
                diff_truncated: false,
                residue: effect.residue,
            }],
        )
        .expect("Write 变更的 residue 已由影子层保证合法");
        self.call_effects.push(entry);
    }
}

impl lancet::host::handles::Host for ToolStoreState {}

impl lancet::host::handles::HostReadDir for ToolStoreState {
    fn read(
        &mut self,
        self_: Resource<ReadDirResource>,
        path: String,
    ) -> wasmtime::Result<Result<Option<String>, String>> {
        let dir = self.table.get(&self_).map_err(wasmtime::Error::from)?;
        let res = match dir {
            ReadDirResource::Host(d) => d.read(&path),
            ReadDirResource::Shadow(s) => s.read(&path),
        };
        Ok(res)
    }

    fn entries(
        &mut self,
        self_: Resource<ReadDirResource>,
        path: String,
    ) -> wasmtime::Result<Result<Option<Vec<lancet::host::handles::Entry>>, String>> {
        let dir = self.table.get(&self_).map_err(wasmtime::Error::from)?;
        let raw = match dir {
            ReadDirResource::Host(d) => d.entries(&path),
            ReadDirResource::Shadow(s) => s.entries(&path),
        };
        Ok(raw.map(|opt| {
            opt.map(|entries| {
                entries
                    .into_iter()
                    .map(|entry| lancet::host::handles::Entry {
                        name: entry.name,
                        is_dir: entry.is_dir,
                        is_link: entry.is_link,
                    })
                    .collect()
            })
        }))
    }

    fn status(
        &mut self,
        self_: Resource<ReadDirResource>,
    ) -> wasmtime::Result<Result<Vec<lancet::host::handles::PendingChange>, String>> {
        let dir = self.table.get(&self_).map_err(wasmtime::Error::from)?;
        let res = match dir {
            ReadDirResource::Host(_) => Ok(Vec::new()),
            ReadDirResource::Shadow(s) => s.status().map(|list| {
                list.into_iter()
                    .map(|c| lancet::host::handles::PendingChange {
                        path: c.path,
                        action: c.action.as_str().to_owned(),
                        conflict: c.conflict,
                    })
                    .collect()
            }),
        };
        Ok(res)
    }

    fn diff(
        &mut self,
        self_: Resource<ReadDirResource>,
    ) -> wasmtime::Result<Result<Vec<lancet::host::handles::PathDiff>, String>> {
        let dir = self.table.get(&self_).map_err(wasmtime::Error::from)?;
        let res = match dir {
            ReadDirResource::Host(_) => Ok(Vec::new()),
            ReadDirResource::Shadow(s) => s.diff().map(|list| {
                list.into_iter()
                    .map(|d| lancet::host::handles::PathDiff {
                        path: d.path,
                        action: d.action.as_str().to_owned(),
                        conflict: d.conflict,
                        patch: d.patch,
                    })
                    .collect()
            }),
        };
        Ok(res)
    }

    fn drop(&mut self, rep: Resource<ReadDirResource>) -> wasmtime::Result<()> {
        self.table.delete(rep).map_err(wasmtime::Error::from)?;
        Ok(())
    }
}

impl lancet::host::handles::HostWriteDir for ToolStoreState {
    fn read(
        &mut self,
        self_: Resource<WriteDirResource>,
        path: String,
    ) -> wasmtime::Result<Result<Option<String>, String>> {
        let dir = self.table.get(&self_).map_err(wasmtime::Error::from)?;
        Ok(dir.shadow.read(&path))
    }

    fn write(
        &mut self,
        self_: Resource<WriteDirResource>,
        path: String,
        content: String,
    ) -> wasmtime::Result<Result<(), String>> {
        let shadow = self
            .table
            .get(&self_)
            .map_err(wasmtime::Error::from)?
            .shadow
            .clone();
        match shadow.write(&path, &content) {
            Ok(Some(effect)) => {
                self.record_write_effect(shadow.name(), effect);
                Ok(Ok(()))
            }
            Ok(None) => Ok(Ok(())),
            Err(msg) => Ok(Err(msg)),
        }
    }

    fn reset(
        &mut self,
        self_: Resource<WriteDirResource>,
        path: String,
    ) -> wasmtime::Result<Result<(), String>> {
        let shadow = self
            .table
            .get(&self_)
            .map_err(wasmtime::Error::from)?
            .shadow
            .clone();
        match shadow.reset(&path) {
            Ok(Some(effect)) => {
                self.record_write_effect(shadow.name(), effect);
                Ok(Ok(()))
            }
            Ok(None) => Ok(Ok(())),
            Err(msg) => Ok(Err(msg)),
        }
    }

    fn drop(&mut self, rep: Resource<WriteDirResource>) -> wasmtime::Result<()> {
        self.table.delete(rep).map_err(wasmtime::Error::from)?;
        Ok(())
    }
}

impl lancet::host::handles::HostExecEnv for ToolStoreState {
    fn drop(&mut self, rep: Resource<lancet::host::handles::ExecEnv>) -> wasmtime::Result<()> {
        self.table.delete(rep).map_err(wasmtime::Error::from)?;
        Ok(())
    }
}

impl lancet::host::handles::HostEgress for ToolStoreState {
    fn drop(&mut self, rep: Resource<lancet::host::handles::Egress>) -> wasmtime::Result<()> {
        self.table.delete(rep).map_err(wasmtime::Error::from)?;
        Ok(())
    }
}

impl Dispatcher {
    /// 反序列化并预链接工具组件列表，按槽位顺序绑定节点授予的目录资源并构建唯一 `ActContext`。
    ///
    /// # Safety
    ///
    /// `components` 中每个条目的 `cwasm` 字节切片必须来自同一构建、并由与 `engine`
    /// 一致的 `wasmtime::Config` 预编译产生。
    pub unsafe fn new(
        engine: &Engine,
        components: &[ComponentArtifact<'_>],
        dirs: Vec<DirSlot>,
    ) -> anyhow::Result<Self> {
        let mut linker = Linker::<ToolStoreState>::new(engine);
        ToolHost::add_to_linker::<_, HasSelf<ToolStoreState>>(&mut linker, |state| state)
            .map_err(anyhow::Error::from)
            .context("注册宿主接口 lancet:host 失败")?;

        let mut prepared_components = Vec::with_capacity(components.len());
        let mut tools = HashMap::new();
        let mut all_decls = Vec::new();

        for artifact in components {
            // SAFETY: 预编译产物均在同一构建由与 `engine` 一致的 `engine_config()` 生成。
            let component = unsafe { Component::deserialize(engine, artifact.cwasm) }
                .map_err(anyhow::Error::from)
                .with_context(|| format!("反序列化预编译工具组件 `{}` 失败", artifact.name))?;
            let instance_pre = linker
                .instantiate_pre(&component)
                .map_err(anyhow::Error::from)
                .with_context(|| format!("预链接工具组件 `{}` 失败", artifact.name))?;

            let component_idx = prepared_components.len();
            prepared_components.push(instance_pre);

            for decl in &artifact.decls {
                let iface_index = component
                    .get_export_index(None, &decl.wit_interface)
                    .ok_or_else(|| {
                        anyhow::anyhow!(
                            "工具组件 `{}` 缺少导出接口 `{}`",
                            artifact.name,
                            decl.wit_interface
                        )
                    })?;
                let func_index = component
                    .get_export_index(Some(&iface_index), &decl.wit_function)
                    .ok_or_else(|| {
                        anyhow::anyhow!(
                            "工具组件 `{}` 接口 `{}` 缺少导出函数 `{}`",
                            artifact.name,
                            decl.wit_interface,
                            decl.wit_function
                        )
                    })?;
                tools.insert(
                    decl.wit_export(),
                    PreparedTool {
                        decl: decl.clone(),
                        component_idx,
                        func_index,
                    },
                );
                all_decls.push(decl.clone());
            }
        }

        let handles = dirs
            .iter()
            .enumerate()
            .map(|(slot, dir)| HandleGrant {
                name: dir.name().to_owned(),
                kind: dir.handle_kind(),
                slot: slot as u32,
            })
            .collect();
        let context = ActContext {
            tools: all_decls,
            handles,
        };

        Ok(Self {
            inner: Arc::new(DispatcherInner {
                engine: engine.clone(),
                components: prepared_components,
                tools,
                dirs,
                context,
            }),
        })
    }

    /// 返回与当前分发器授权句柄一致的 `act` 上下文。
    pub fn context(&self) -> &ActContext {
        &self.inner.context
    }

    /// 返回本次授权中是否包含至少一个可写目录句柄。
    pub fn has_writable_dirs(&self) -> bool {
        self.inner
            .dirs
            .iter()
            .any(|slot| matches!(slot, DirSlot::Shadow(_)))
    }

    /// 按 `[[dir]]` 槽位顺序返回全部可写目录影子句柄。
    pub fn writable_shadows(&self) -> Vec<&ShadowDir> {
        self.inner
            .dirs
            .iter()
            .filter_map(|slot| match slot {
                DirSlot::Shadow(shadow) => Some(shadow),
                DirSlot::ReadOnly(_) => None,
            })
            .collect()
    }

    /// 核对发布产生的 `EffectEntry` 均未越出已授予可写目录的 `Write` effect 模板。
    pub fn validate_publish_effects(&self, entries: &[EffectEntry]) -> Result<(), String> {
        let allowed = allowed_writable_effects(&self.inner.context.handles);
        for entry in entries {
            if !allowed.contains(&(entry.handle().to_owned(), entry.kind())) {
                return Err(format!(
                    "发布在句柄 `{}` 上产生的 {:?} effect 越出模板",
                    entry.handle(),
                    entry.kind()
                ));
            }
        }
        Ok(())
    }

    /// 按槽位顺序取得全部可写目录的互斥锁，等待期间响应取消与截止时间。
    pub fn acquire_writable_locks(
        &self,
        deadline: Instant,
        cancelled: &AtomicBool,
    ) -> Result<WritableLocks<'_>, WaitInterrupt> {
        let mut guards = Vec::new();
        for slot in &self.inner.dirs {
            let DirSlot::Shadow(shadow) = slot else {
                continue;
            };
            loop {
                if cancelled.load(Ordering::Acquire) {
                    return Err(WaitInterrupt::Cancelled);
                }
                let now = Instant::now();
                if now >= deadline {
                    return Err(WaitInterrupt::Deadline);
                }
                match shadow.try_lock_act() {
                    Ok(guard) => {
                        guards.push(guard);
                        break;
                    }
                    Err(TryLockError::WouldBlock) => {
                        let remaining = deadline.saturating_duration_since(now);
                        let sleep_for = remaining.min(Duration::from_millis(10));
                        if !sleep_for.is_zero() {
                            std::thread::sleep(sleep_for);
                        }
                        if cancelled.load(Ordering::Acquire) {
                            return Err(WaitInterrupt::Cancelled);
                        }
                        if Instant::now() >= deadline {
                            return Err(WaitInterrupt::Deadline);
                        }
                    }
                    Err(TryLockError::Poisoned(_)) => {
                        panic!("可写目录互斥锁不应中毒");
                    }
                }
            }
        }
        Ok(WritableLocks { _guards: guards })
    }

    /// 持锁后逐个同步可写目录的宿主改动进影子与基准，并在路径间检查截止与取消。
    pub fn sync_writable_dirs(
        &self,
        deadline: Instant,
        cancelled: &AtomicBool,
    ) -> Result<(), SyncError> {
        for slot in &self.inner.dirs {
            let DirSlot::Shadow(shadow) = slot else {
                continue;
            };
            if let Err(err) = shadow.sync_from_host(Some(deadline), Some(cancelled)) {
                return Err(match err {
                    ShadowSyncError::Interrupted(ShadowInterrupt::Deadline) => SyncError::Deadline,
                    ShadowSyncError::Interrupted(ShadowInterrupt::Cancelled) => {
                        SyncError::Cancelled
                    }
                    ShadowSyncError::Failed(msg) => SyncError::Failed(msg),
                });
            }
        }
        Ok(())
    }

    /// 汇总全部可写目录中可见待定路径的总数（冲突只计一次，不含 `hidden` 与 `regenerable`）。
    pub fn visible_pending_count(&self) -> usize {
        self.inner
            .dirs
            .iter()
            .filter_map(|slot| match slot {
                DirSlot::Shadow(shadow) => Some(shadow.visible_pending_count()),
                DirSlot::ReadOnly(_) => None,
            })
            .sum()
    }

    /// 执行内置 `discard()`：把全部可写目录的常规待定路径恢复为基准并清除 `hidden` 项，保留种子。
    pub fn discard(
        &self,
        deadline: Instant,
        cancelled: Arc<AtomicBool>,
        steps: &Mutex<Vec<StepSummary>>,
        effects: &Mutex<Vec<EffectEntry>>,
    ) -> wasmtime::Result<Result<(), String>> {
        let allowed_effects = allowed_writable_effects(&self.inner.context.handles);
        let mut recorded = Vec::new();
        for slot in &self.inner.dirs {
            let DirSlot::Shadow(shadow) = slot else {
                continue;
            };
            match shadow.discard(Some(deadline), Some(&cancelled)) {
                Ok(list) => {
                    if let Some(entry) = build_write_entry(shadow.name(), list) {
                        recorded.push(entry);
                    }
                }
                Err(DiscardError::Interrupted { partial, interrupt }) => {
                    if let Some(entry) = build_write_entry(shadow.name(), partial) {
                        recorded.push(entry);
                    }
                    finalize_discard_effects(recorded, &allowed_effects, steps, effects)?;
                    return match interrupt {
                        ShadowInterrupt::Deadline => {
                            record_step(steps, "discard", OutcomeKind::Deadline);
                            Err(wasmtime::Error::new(Interrupt::Deadline))
                        }
                        ShadowInterrupt::Cancelled => {
                            record_step(steps, "discard", OutcomeKind::Cancelled);
                            Err(wasmtime::Error::new(Interrupt::Cancelled))
                        }
                    };
                }
                Err(DiscardError::Failed { partial, message }) => {
                    if let Some(entry) = build_write_entry(shadow.name(), partial) {
                        recorded.push(entry);
                    }
                    finalize_discard_effects(recorded, &allowed_effects, steps, effects)?;
                    record_step(steps, "discard", OutcomeKind::Failed);
                    return Ok(Err(message));
                }
            }
        }
        finalize_discard_effects(recorded, &allowed_effects, steps, effects)?;
        record_step(steps, "discard", OutcomeKind::Succeeded);
        Ok(Ok(()))
    }

    /// 执行内置 `commit()`：标记本次 `act` 在脚本正常返回且交回载荷不超限时发布最终待定改动。
    pub fn commit(&self, steps: &Mutex<Vec<StepSummary>>) -> wasmtime::Result<Result<(), String>> {
        record_step(steps, "commit", OutcomeKind::Succeeded);
        Ok(Ok(()))
    }

    /// 核对 κ、在隔离 `Store` 中运行工具组件、执行统一 ε 归集与模板核对、返回窗口与步骤摘要记录。
    #[allow(clippy::too_many_arguments)]
    pub fn call(
        &self,
        tool_id: &str,
        args: &[DispatchArgument],
        deadline: Instant,
        cancelled: Arc<AtomicBool>,
        steps: &Mutex<Vec<StepSummary>>,
        effects: &Mutex<Vec<EffectEntry>>,
    ) -> wasmtime::Result<Result<String, String>> {
        let Some(prepared) = self.inner.tools.get(tool_id) else {
            return record_tool_err(steps, tool_id, format!("未知的 API 工具：`{tool_id}`"));
        };
        let step_name = &prepared.decl.script_name;

        if let Err(msg) = check_coeffects(&prepared.decl, args, &self.inner.context.handles) {
            return record_tool_err(steps, step_name, msg);
        }

        let allowed_effects =
            instantiate_allowed_effects(&prepared.decl, args, &self.inner.context.handles);

        let mut store = new_tool_store(&self.inner.engine, deadline, cancelled);
        let body_outcome = (|| -> CallBodyOutcome {
            let pre = &self.inner.components[prepared.component_idx];
            let instance = match pre.instantiate(&mut store) {
                Ok(inst) => inst,
                Err(err) => return CallBodyOutcome::WasmtimeErr(err),
            };

            let func = instance
                .get_func(&mut store, prepared.func_index)
                .expect("导出函数索引来自同一组件，不会缺失");

            let wit_args = match self.build_wit_args(&prepared.decl, &func, &mut store, args) {
                Ok(vals) => vals,
                Err(msg) => return CallBodyOutcome::ToolErr(msg),
            };

            let result_count = func.ty(&store).results().len();
            let mut results = vec![Val::Bool(false); result_count];
            if let Err(err) = func.call(&mut store, &wit_args, &mut results) {
                return CallBodyOutcome::WasmtimeErr(err);
            }

            let json_val = match extract_result_json(&results) {
                Ok(Ok(val)) => val,
                Ok(Err(tool_err)) => return CallBodyOutcome::ToolErr(tool_err),
                Err(conv_err) => return CallBodyOutcome::ToolErr(conv_err),
            };

            let compact_json =
                serde_json::to_string(&json_val).expect("已校验的返回值 JSON 序列化不会失败");

            if compact_json.len() > MAX_TOOL_RESULT_BYTES {
                return CallBodyOutcome::ToolErr(format!(
                    "工具 `{step_name}` 的返回值有 {} 字节，超过单次返回窗口 8 MiB（{MAX_TOOL_RESULT_BYTES} 字节）；请改用 `proc.pipe` 过滤后再返回",
                    compact_json.len()
                ));
            }
            CallBodyOutcome::OkJson(compact_json)
        })();

        // 统一收尾：先把本次调用实际落地的变更归入 act 回执，再核对是否越出声明的 effect 模板。
        let call_effects = std::mem::take(&mut store.data_mut().call_effects);
        let mut violation: Option<(String, EffectKind)> = None;
        for entry in &call_effects {
            if !allowed_effects.contains(&(entry.handle().to_owned(), entry.kind())) {
                violation = Some((entry.handle().to_owned(), entry.kind()));
                break;
            }
        }
        if !call_effects.is_empty() {
            append_effects(effects, call_effects);
        }
        if let Some((handle, kind)) = violation {
            record_step(steps, step_name, OutcomeKind::Failed);
            return Err(wasmtime::Error::new(DispatchTrap::EffectOutOfTemplate {
                tool: step_name.clone(),
                handle,
                kind,
            }));
        }

        match body_outcome {
            CallBodyOutcome::OkJson(compact_json) => {
                record_step(steps, step_name, OutcomeKind::Succeeded);
                Ok(Ok(compact_json))
            }
            CallBodyOutcome::ToolErr(msg) => record_tool_err(steps, step_name, msg),
            CallBodyOutcome::WasmtimeErr(err) => handle_wasmtime_error(step_name, err, steps),
        }
    }

    fn build_wit_args(
        &self,
        decl: &ToolDecl,
        func: &wasmtime::component::Func,
        store: &mut Store<ToolStoreState>,
        args: &[DispatchArgument],
    ) -> Result<Vec<Val>, String> {
        let param_types: Vec<Type> = func.ty(&*store).params().map(|(_, ty)| ty).collect();
        let mut wit_args = Vec::with_capacity(decl.params.len());
        for ((param, wit_ty), arg) in decl.params.iter().zip(param_types.iter()).zip(args.iter()) {
            match (&param.ty, arg) {
                (WitType::Handle { kind, .. }, DispatchArgument::HandleRef(slot)) => {
                    let dir_slot = &self.inner.dirs[*slot as usize];
                    let any = match kind {
                        HandleKind::ReadDir => {
                            let res_val = match dir_slot {
                                DirSlot::ReadOnly(read_dir) => {
                                    ReadDirResource::Host(read_dir.clone())
                                }
                                DirSlot::Shadow(shadow) => {
                                    ReadDirResource::Shadow(shadow.read_view())
                                }
                            };
                            let resource = store.data_mut().table.push(res_val).map_err(|err| {
                                format!("注入只读目录句柄 `{}` 失败：{err}", param.name)
                            })?;
                            ResourceAny::try_from_resource(resource, &mut *store).map_err(
                                |err| format!("转换只读目录句柄 `{}` 失败：{err}", param.name),
                            )?
                        }
                        HandleKind::WriteDir => {
                            let DirSlot::Shadow(shadow) = dir_slot else {
                                unreachable!("κ 已保证 WriteDir 形参只接收可写目录槽位");
                            };
                            let resource = store
                                .data_mut()
                                .table
                                .push(WriteDirResource {
                                    shadow: shadow.clone(),
                                })
                                .map_err(|err| {
                                    format!("注入可写目录句柄 `{}` 失败：{err}", param.name)
                                })?;
                            ResourceAny::try_from_resource(resource, &mut *store).map_err(
                                |err| format!("转换可写目录句柄 `{}` 失败：{err}", param.name),
                            )?
                        }
                        HandleKind::ExecEnv | HandleKind::Egress => {
                            unreachable!("执行环境与网络出口句柄随 #8/#9 注入")
                        }
                    };
                    wit_args.push(Val::Resource(any));
                }
                (WitType::Handle { .. }, _) | (_, DispatchArgument::HandleRef(_)) => {
                    unreachable!("κ 已保证句柄形参拿到 HandleRef，解释器对数据形参只传 ValueJson");
                }
                (_, DispatchArgument::ValueJson(raw_json)) => {
                    let parsed: Value = serde_json::from_str(raw_json).map_err(|err| {
                        format!(
                            "工具 `{}` 参数 `{}` JSON 解析失败：{err}",
                            decl.script_name, param.name
                        )
                    })?;
                    wit_args.push(json_to_val(&param.name, wit_ty, &parsed)?);
                }
            }
        }
        Ok(wit_args)
    }
}

enum CallBodyOutcome {
    OkJson(String),
    ToolErr(String),
    WasmtimeErr(wasmtime::Error),
}

fn new_tool_store(
    engine: &Engine,
    deadline: Instant,
    cancelled: Arc<AtomicBool>,
) -> Store<ToolStoreState> {
    let mut store = Store::new(
        engine,
        ToolStoreState {
            memory: MemoryBudget::new(TOOL_MEMORY_LIMIT_BYTES),
            deadline,
            cancelled,
            table: ResourceTable::new(),
            call_effects: Vec::new(),
        },
    );
    store.limiter(|state| &mut state.memory);
    store.set_epoch_deadline(0);
    store.epoch_deadline_callback(|cx| {
        let state = cx.data();
        check_epoch_interrupt(state.deadline, &state.cancelled)
    });
    store
}

fn build_write_entry(handle: &str, list: Vec<WriteEffect>) -> Option<EffectEntry> {
    if list.is_empty() {
        return None;
    }
    let changes = list
        .into_iter()
        .map(|eff| Change {
            target: eff.target,
            action: eff.action.to_owned(),
            diff: None,
            diff_truncated: false,
            residue: eff.residue,
        })
        .collect();
    Some(
        EffectEntry::new(handle, EffectKind::Write, changes)
            .expect("Write 变更的 residue 已由影子层保证合法"),
    )
}

fn append_effects(effects: &Mutex<Vec<EffectEntry>>, entries: Vec<EffectEntry>) {
    let mut guard = effects.lock().expect("effect 互斥锁不应中毒");
    guard.extend(entries);
}

fn allowed_writable_effects(grants: &[HandleGrant]) -> HashSet<(String, EffectKind)> {
    grants
        .iter()
        .filter(|g| g.kind == HandleKind::WriteDir)
        .map(|g| (g.name.clone(), EffectKind::Write))
        .collect()
}

fn finalize_discard_effects(
    recorded: Vec<EffectEntry>,
    allowed_effects: &HashSet<(String, EffectKind)>,
    steps: &Mutex<Vec<StepSummary>>,
    effects: &Mutex<Vec<EffectEntry>>,
) -> wasmtime::Result<()> {
    let mut violation: Option<(String, EffectKind)> = None;
    for entry in &recorded {
        if !allowed_effects.contains(&(entry.handle().to_owned(), entry.kind())) {
            violation = Some((entry.handle().to_owned(), entry.kind()));
            break;
        }
    }
    if !recorded.is_empty() {
        append_effects(effects, recorded);
    }
    if let Some((handle, kind)) = violation {
        record_step(steps, "discard", OutcomeKind::Failed);
        return Err(wasmtime::Error::new(DispatchTrap::EffectOutOfTemplate {
            tool: "discard".to_owned(),
            handle,
            kind,
        }));
    }
    Ok(())
}

fn instantiate_allowed_effects(
    decl: &ToolDecl,
    args: &[DispatchArgument],
    grants: &[HandleGrant],
) -> HashSet<(String, EffectKind)> {
    let mut allowed = HashSet::new();
    for item in &decl.templates.effects {
        match *item {
            EffectItem::Effect(idx, kind) => {
                let DispatchArgument::HandleRef(slot) = args[idx] else {
                    continue;
                };
                let Some(grant) = grants.iter().find(|g| g.slot == slot) else {
                    continue;
                };
                let proto_kind = match kind {
                    lancet_tool_decl::EffectKind::Write => EffectKind::Write,
                    lancet_tool_decl::EffectKind::Exec => EffectKind::Exec,
                    lancet_tool_decl::EffectKind::Emit => EffectKind::Emit,
                };
                allowed.insert((grant.name.clone(), proto_kind));
            }
            EffectItem::WriteWritableMountsOf(_) | EffectItem::EmitViaEgressOf(_) => {
                unreachable!("执行环境与网络出口句柄随 #8/#9 注入")
            }
        }
    }
    allowed
}

fn record_step(steps: &Mutex<Vec<StepSummary>>, tool_id: &str, outcome: OutcomeKind) {
    let mut guard = steps.lock().expect("步骤互斥锁不应中毒");
    guard.push(StepSummary {
        tool: tool_id.to_owned(),
        outcome,
    });
}

fn record_tool_err(
    steps: &Mutex<Vec<StepSummary>>,
    tool_id: &str,
    msg: String,
) -> wasmtime::Result<Result<String, String>> {
    record_step(steps, tool_id, OutcomeKind::Failed);
    Ok(Err(msg))
}

fn handle_wasmtime_error(
    tool_id: &str,
    err: wasmtime::Error,
    steps: &Mutex<Vec<StepSummary>>,
) -> wasmtime::Result<Result<String, String>> {
    match err.downcast_ref::<Interrupt>() {
        Some(Interrupt::Deadline) => {
            record_step(steps, tool_id, OutcomeKind::Deadline);
            Err(wasmtime::Error::new(Interrupt::Deadline))
        }
        Some(Interrupt::Cancelled) => {
            record_step(steps, tool_id, OutcomeKind::Cancelled);
            Err(wasmtime::Error::new(Interrupt::Cancelled))
        }
        Some(Interrupt::MemoryLimit) => record_tool_err(
            steps,
            tool_id,
            format!("工具 `{tool_id}` 超出内存上限 {TOOL_MEMORY_LIMIT_BYTES} 字节"),
        ),
        None => record_tool_err(
            steps,
            tool_id,
            format!("工具 `{tool_id}` 陷入：{}", err.root_cause()),
        ),
    }
}

fn extract_result_json(results: &[Val]) -> Result<Result<Value, String>, String> {
    if results.is_empty() {
        return Ok(Ok(Value::Null));
    }
    match &results[0] {
        Val::Result(res) => match res {
            Ok(Some(ok_val)) => Ok(Ok(val_to_json(ok_val)?)),
            Ok(None) => Ok(Ok(Value::Null)),
            Err(Some(err_val)) => {
                let Val::String(msg) = &**err_val else {
                    unreachable!("声明加载已校验 result 错误类型必为 string");
                };
                Ok(Err(msg.clone()))
            }
            Err(None) => unreachable!("声明加载已校验 result 错误类型必为 string"),
        },
        other => Ok(Ok(val_to_json(other)?)),
    }
}

fn json_to_val(param_name: &str, ty: &Type, value: &Value) -> Result<Val, String> {
    match ty {
        Type::Bool => value
            .as_bool()
            .map(Val::Bool)
            .ok_or_else(|| format!("参数 `{param_name}` 需要 bool")),
        Type::String => value
            .as_str()
            .map(|s| Val::String(s.to_owned()))
            .ok_or_else(|| format!("参数 `{param_name}` 需要 string")),
        Type::S8 => parse_signed(param_name, value, i8::MIN as i64, i8::MAX as i64)
            .map(|v| Val::S8(v as i8)),
        Type::S16 => parse_signed(param_name, value, i16::MIN as i64, i16::MAX as i64)
            .map(|v| Val::S16(v as i16)),
        Type::S32 => parse_signed(param_name, value, i32::MIN as i64, i32::MAX as i64)
            .map(|v| Val::S32(v as i32)),
        Type::S64 => parse_signed(param_name, value, i64::MIN, i64::MAX).map(Val::S64),
        Type::U8 => parse_unsigned(param_name, value, u8::MAX as u64).map(|v| Val::U8(v as u8)),
        Type::U16 => parse_unsigned(param_name, value, u16::MAX as u64).map(|v| Val::U16(v as u16)),
        Type::U32 => parse_unsigned(param_name, value, u32::MAX as u64).map(|v| Val::U32(v as u32)),
        Type::U64 => parse_unsigned(param_name, value, u64::MAX).map(Val::U64),
        Type::Float32 => {
            let v64 = value
                .as_f64()
                .filter(|v| v.is_finite())
                .ok_or_else(|| format!("参数 `{param_name}` 需要有限浮点数"))?;
            if v64.abs() > f64::from(f32::MAX) {
                return Err(format!("参数 `{param_name}` 的值 {v64} 超出 float32 范围"));
            }
            Ok(Val::Float32(v64 as f32))
        }
        Type::Float64 => value
            .as_f64()
            .filter(|v| v.is_finite())
            .map(Val::Float64)
            .ok_or_else(|| format!("参数 `{param_name}` 需要有限 float64")),
        Type::Option(opt_ty) => match value {
            Value::Null => Ok(Val::Option(None)),
            other => Ok(Val::Option(Some(Box::new(json_to_val(
                param_name,
                &opt_ty.ty(),
                other,
            )?)))),
        },
        Type::List(list_ty) => {
            let items = value
                .as_array()
                .ok_or_else(|| format!("参数 `{param_name}` 需要数组"))?;
            let elem_ty = list_ty.ty();
            let converted = items
                .iter()
                .map(|item| json_to_val(param_name, &elem_ty, item))
                .collect::<Result<Vec<_>, _>>()?;
            Ok(Val::List(converted))
        }
        _ => unreachable!("声明加载已校验参数 `{param_name}` 为支持的 WIT 数据类型"),
    }
}

fn parse_signed(param_name: &str, value: &Value, min: i64, max: i64) -> Result<i64, String> {
    let Some(num) = value.as_i64() else {
        return Err(format!(
            "参数 `{param_name}` 的值超出有符号整数范围 [{min}, {max}]"
        ));
    };
    if num < min || num > max {
        return Err(format!(
            "参数 `{param_name}` 的值 {num} 超出范围 [{min}, {max}]"
        ));
    }
    Ok(num)
}

fn parse_unsigned(param_name: &str, value: &Value, max: u64) -> Result<u64, String> {
    let Some(num) = value.as_u64() else {
        return Err(format!(
            "参数 `{param_name}` 的值超出无符号整数范围 [0, {max}]"
        ));
    };
    if num > max {
        return Err(format!(
            "参数 `{param_name}` 的值 {num} 超出范围 [0, {max}]"
        ));
    }
    Ok(num)
}

fn val_to_json(val: &Val) -> Result<Value, String> {
    match val {
        Val::Bool(b) => Ok(Value::Bool(*b)),
        Val::S8(n) => Ok(Value::from(*n)),
        Val::U8(n) => Ok(Value::from(*n)),
        Val::S16(n) => Ok(Value::from(*n)),
        Val::U16(n) => Ok(Value::from(*n)),
        Val::S32(n) => Ok(Value::from(*n)),
        Val::U32(n) => Ok(Value::from(*n)),
        Val::S64(n) => Ok(Value::from(*n)),
        Val::U64(n) => Ok(Value::from(*n)),
        Val::Float32(f) => serde_json::Number::from_f64(f64::from(*f))
            .map(Value::Number)
            .ok_or_else(|| format!("float32 返回值 {f} 无法表示为有限 JSON 数字")),
        Val::Float64(f) => serde_json::Number::from_f64(*f)
            .map(Value::Number)
            .ok_or_else(|| format!("float64 返回值 {f} 无法表示为有限 JSON 数字")),
        Val::String(s) => Ok(Value::String(s.clone())),
        Val::Option(None) => Ok(Value::Null),
        Val::Option(Some(inner)) => val_to_json(inner),
        Val::List(items) => items
            .iter()
            .map(val_to_json)
            .collect::<Result<Vec<_>, _>>()
            .map(Value::Array),
        Val::Record(fields) => {
            let mut map = serde_json::Map::with_capacity(fields.len());
            for (name, field_val) in fields {
                map.insert(name.clone(), val_to_json(field_val)?);
            }
            Ok(Value::Object(map))
        }
        other => unreachable!("声明加载已校验 WIT 返回值为支持的数据类型：{other:?}"),
    }
}

/// 核对本次调用实参及授予句柄是否满足工具声明中的 co-effect 模板 κ。
pub(crate) fn check_coeffects(
    decl: &ToolDecl,
    args: &[DispatchArgument],
    grants: &[HandleGrant],
) -> Result<(), String> {
    for item in &decl.templates.coeffects {
        match *item {
            CoEffectItem::Uses(i, _) => {
                let param = &decl.params[i];
                let WitType::Handle { kind, .. } = param.ty else {
                    unreachable!("CoEffectItem::Uses 只能引用句柄形参");
                };
                let slot = match &args[i] {
                    DispatchArgument::HandleRef(slot) => *slot,
                    DispatchArgument::ValueJson(_) => {
                        return Err(format!(
                            "工具 `{}` 参数 `{}` 需要句柄实参",
                            decl.script_name, param.name
                        ));
                    }
                };
                let grant = grants
                    .iter()
                    .find(|g| g.slot == slot)
                    .ok_or_else(|| format!("句柄槽位 {slot} 未授予给本次调用"))?;
                // 模式由形参种类决定，种类接受即模式满足：`ReadDir` 形参对应 `Read`，`WriteDir` 形参对应 `ReadWrite`。
                if !kind.accepts(grant.kind) {
                    return Err(format!(
                        "工具 `{}` 参数 `{}` 需要 `{kind:?}` 句柄，实际传入 `{:?}`",
                        decl.script_name, param.name, grant.kind
                    ));
                }
            }
            CoEffectItem::MountsOf(_) => unreachable!("执行环境句柄随 #8 注入"),
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use lancet_tool_decl::{
        CoEffectItem, HandleKind, Mode, ParamDecl, ReturnType, Templates, WitType,
    };

    fn sample_decl(mode: Mode, recv_kind: HandleKind) -> ToolDecl {
        ToolDecl {
            script_name: "fs.op".to_owned(),
            wit_interface: "lancet:test/fs@0.1.0".to_owned(),
            wit_function: "op".to_owned(),
            docs: None,
            params: vec![
                ParamDecl {
                    name: "dir".to_owned(),
                    ty: WitType::Handle {
                        kind: recv_kind,
                        borrow: true,
                    },
                },
                ParamDecl {
                    name: "path".to_owned(),
                    ty: WitType::String,
                },
            ],
            result: ReturnType::Unit,
            records: Vec::new(),
            templates: Templates {
                coeffects: vec![CoEffectItem::Uses(0, Some(mode))],
                effects: Vec::new(),
            },
        }
    }

    #[test]
    fn check_coeffects_enforces_grants_and_modes() {
        let read_decl = sample_decl(Mode::Read, HandleKind::ReadDir);
        let write_decl = sample_decl(Mode::ReadWrite, HandleKind::WriteDir);

        let ro_grant = HandleGrant {
            name: "ro".to_owned(),
            kind: HandleKind::ReadDir,
            slot: 0,
        };
        let rw_grant = HandleGrant {
            name: "rw".to_owned(),
            kind: HandleKind::WriteDir,
            slot: 1,
        };
        let env_grant = HandleGrant {
            name: "env".to_owned(),
            kind: HandleKind::ExecEnv,
            slot: 2,
        };

        let path_arg = DispatchArgument::ValueJson("\"a.txt\"".to_owned());

        // 槽位未授予。
        assert!(
            check_coeffects(
                &read_decl,
                &[DispatchArgument::HandleRef(9), path_arg.clone()],
                std::slice::from_ref(&ro_grant),
            )
            .is_err()
        );

        // 需要 ReadWrite 而只授予 ReadDir。
        assert!(
            check_coeffects(
                &write_decl,
                &[DispatchArgument::HandleRef(0), path_arg.clone()],
                std::slice::from_ref(&ro_grant),
            )
            .is_err()
        );

        // 该传句柄处传了 JSON。
        assert!(
            check_coeffects(
                &read_decl,
                &[path_arg.clone(), path_arg.clone()],
                std::slice::from_ref(&ro_grant),
            )
            .is_err()
        );

        // 句柄种类不匹配（ExecEnv 传给 ReadDir）。
        assert!(
            check_coeffects(
                &read_decl,
                &[DispatchArgument::HandleRef(2), path_arg.clone()],
                &[env_grant],
            )
            .is_err()
        );

        // Read 接受 ReadDir 与 WriteDir；ReadWrite 接受 WriteDir。
        check_coeffects(
            &read_decl,
            &[DispatchArgument::HandleRef(0), path_arg.clone()],
            &[ro_grant],
        )
        .expect("Read 应接受 ReadDir");
        check_coeffects(
            &read_decl,
            &[DispatchArgument::HandleRef(1), path_arg.clone()],
            std::slice::from_ref(&rw_grant),
        )
        .expect("Read 应接受 WriteDir");
        check_coeffects(
            &write_decl,
            &[DispatchArgument::HandleRef(1), path_arg],
            &[rw_grant],
        )
        .expect("ReadWrite 应接受 WriteDir");
    }
}
