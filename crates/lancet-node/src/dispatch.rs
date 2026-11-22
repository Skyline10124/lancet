// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! API 工具分发层：κ 核对、组件实例化、数值转换、资源收窄、ε 记录与模板核对。

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use anyhow::Context as _;
use lancet_protocol::{Change, EffectEntry, EffectKind, OutcomeKind, Residue, StepSummary};
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
use crate::exec_env::{ExecRunOutcome, HostExecEnv};
use crate::read_dir::HostDir;

wasmtime::component::bindgen!({
    world: "tool-host",
    path: "wit/lancet-host",
    with: {
        "lancet:host/handles.read-dir": ReadDirResource,
        "lancet:host/handles.write-dir": WriteDirResource,
        "lancet:host/handles.exec-env": ExecEnvResource,
    },
    imports: {
        default: trappable,
    },
});

/// 单次 API 工具组件调用的线性内存上限（128 MiB）。
pub const TOOL_MEMORY_LIMIT_BYTES: usize = 128 * 1024 * 1024;

/// 单次 API 工具调用交回脚本的成功值紧凑 JSON 字节上限（8 MiB）。
pub const MAX_TOOL_RESULT_BYTES: usize = 8 * 1024 * 1024;

/// 分发层按配置顺序持有的统一授权槽位（依次为目录与执行环境）。
#[derive(Clone)]
pub enum HandleSlot {
    ReadDir(HostDir),
    WriteDir(HostDir),
    ExecEnv(HostExecEnv),
}

impl HandleSlot {
    pub fn name(&self) -> &str {
        match self {
            Self::ReadDir(dir) | Self::WriteDir(dir) => dir.name(),
            Self::ExecEnv(env) => env.name(),
        }
    }

    pub fn handle_kind(&self) -> HandleKind {
        match self {
            Self::ReadDir(_) => HandleKind::ReadDir,
            Self::WriteDir(_) => HandleKind::WriteDir,
            Self::ExecEnv(_) => HandleKind::ExecEnv,
        }
    }
}

/// 压入工具组件 `ResourceTable` 的 `read-dir` 资源；可写槽位落到 `borrow<read-dir>` 时仍收窄为只读资源。
pub struct ReadDirResource(HostDir);

/// 压入工具组件 `ResourceTable` 的 `write-dir` 资源，仅在形参为 `borrow<write-dir>` 时注入。
pub struct WriteDirResource(HostDir);

/// 压入工具组件 `ResourceTable` 的 `exec-env` 资源。
pub struct ExecEnvResource(HostExecEnv);

/// 分发层检测到的节点内部契约违反或宿主内部故障（以 wasmtime 陷入中止整个 `act` 并结算为 `InternalError`）。
#[derive(Debug)]
pub(crate) enum DispatchTrap {
    EffectOutOfTemplate {
        tool: String,
        handle: String,
        kind: EffectKind,
    },
    HostInternal {
        message: String,
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
            Self::HostInternal { message } => f.write_str(message),
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

/// 持有预链接 API 工具组件、统一授权槽位及对应调用授权上下文的分发器。
#[derive(Clone)]
pub struct Dispatcher {
    inner: Arc<DispatcherInner>,
}

struct DispatcherInner {
    engine: Engine,
    components: Vec<InstancePre<ToolStoreState>>,
    tools: HashMap<String, PreparedTool>,
    slots: Vec<HandleSlot>,
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
    fn record_write_effect(&mut self, handle: &str, target: String) {
        let entry = EffectEntry::new(
            handle,
            EffectKind::Write,
            vec![Change {
                target,
                action: "write".to_owned(),
                diff: None,
                diff_truncated: false,
                residue: Residue::NotRetained,
            }],
        )
        .expect("Write 变更使用 NotRetained 始终合法");
        self.call_effects.push(entry);
    }

    fn record_exec_effect(&mut self, handle: &str) {
        let entry = EffectEntry::new(handle, EffectKind::Exec, Vec::new())
            .expect("无变更项的 Exec 条目始终合法");
        self.call_effects.push(entry);
    }

    fn map_exec_outcome(
        &mut self,
        env_name: &str,
        any_started: bool,
        outcome: ExecRunOutcome,
    ) -> wasmtime::Result<Result<lancet::host::handles::ProcOutput, String>> {
        if any_started {
            self.record_exec_effect(env_name);
        }
        match outcome {
            ExecRunOutcome::Finished(out) => Ok(Ok(lancet::host::handles::ProcOutput {
                code: out.code,
                stdout: out.stdout,
                stderr: out.stderr,
            })),
            ExecRunOutcome::ToolError(msg) => Ok(Err(msg)),
            ExecRunOutcome::Cancelled => Err(wasmtime::Error::new(Interrupt::Cancelled)),
            ExecRunOutcome::Deadline => Err(wasmtime::Error::new(Interrupt::Deadline)),
            ExecRunOutcome::InternalError(message) => {
                Err(wasmtime::Error::new(DispatchTrap::HostInternal { message }))
            }
        }
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
        Ok(dir.0.read(&path))
    }

    fn entries(
        &mut self,
        self_: Resource<ReadDirResource>,
        path: String,
    ) -> wasmtime::Result<Result<Option<Vec<lancet::host::handles::Entry>>, String>> {
        let dir = self.table.get(&self_).map_err(wasmtime::Error::from)?;
        let raw = dir.0.entries(&path);
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
        Ok(dir.0.read(&path))
    }

    fn write(
        &mut self,
        self_: Resource<WriteDirResource>,
        path: String,
        content: String,
    ) -> wasmtime::Result<Result<(), String>> {
        let dir = self
            .table
            .get(&self_)
            .map_err(wasmtime::Error::from)?
            .0
            .clone();
        match dir.write_file(&path, &content) {
            Ok(res) => {
                if res.changed {
                    self.record_write_effect(dir.name(), res.real_rel);
                }
                Ok(Ok(()))
            }
            Err(msg) => Ok(Err(msg)),
        }
    }

    fn drop(&mut self, rep: Resource<WriteDirResource>) -> wasmtime::Result<()> {
        self.table.delete(rep).map_err(wasmtime::Error::from)?;
        Ok(())
    }
}

impl lancet::host::handles::HostExecEnv for ToolStoreState {
    fn run(
        &mut self,
        self_: Resource<ExecEnvResource>,
        argv: Vec<String>,
        cwd: Option<String>,
    ) -> wasmtime::Result<Result<lancet::host::handles::ProcOutput, String>> {
        let env = self
            .table
            .get(&self_)
            .map_err(wasmtime::Error::from)?
            .0
            .clone();
        let (any_started, outcome) = env.run(&argv, cwd.as_deref(), self.deadline, &self.cancelled);
        self.map_exec_outcome(env.name(), any_started, outcome)
    }

    fn pipe(
        &mut self,
        self_: Resource<ExecEnvResource>,
        stages: Vec<Vec<String>>,
        cwd: Option<String>,
    ) -> wasmtime::Result<Result<lancet::host::handles::ProcOutput, String>> {
        let env = self
            .table
            .get(&self_)
            .map_err(wasmtime::Error::from)?
            .0
            .clone();
        let (any_started, outcome) =
            env.pipe(&stages, cwd.as_deref(), self.deadline, &self.cancelled);
        self.map_exec_outcome(env.name(), any_started, outcome)
    }

    fn drop(&mut self, rep: Resource<ExecEnvResource>) -> wasmtime::Result<()> {
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
    /// 反序列化并预链接工具组件列表，按槽位顺序绑定节点授予的目录与执行环境资源并构建唯一 `ActContext`。
    ///
    /// # Safety
    ///
    /// `components` 中每个条目的 `cwasm` 字节切片必须来自同一构建、并由与 `engine`
    /// 一致的 `wasmtime::Config` 预编译产生。
    pub unsafe fn new(
        engine: &Engine,
        components: &[ComponentArtifact<'_>],
        slots: Vec<HandleSlot>,
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

        let handles = slots
            .iter()
            .enumerate()
            .map(|(slot_idx, item)| HandleGrant {
                name: item.name().to_owned(),
                kind: item.handle_kind(),
                slot: slot_idx as u32,
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
                slots,
                context,
            }),
        })
    }

    /// 返回与当前分发器授权句柄一致的 `act` 上下文。
    pub fn context(&self) -> &ActContext {
        &self.inner.context
    }

    /// 返回本次授权中是否包含可写目录或执行环境句柄（重启恢复时无法排除已产生 effect）。
    pub fn has_effectful_handles(&self) -> bool {
        self.inner
            .slots
            .iter()
            .any(|slot| matches!(slot, HandleSlot::WriteDir(_) | HandleSlot::ExecEnv(_)))
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

        if let Err(msg) = check_coeffects(
            &prepared.decl,
            args,
            &self.inner.context.handles,
            &self.inner.slots,
        ) {
            return record_tool_err(steps, step_name, msg);
        }

        let allowed_effects = instantiate_allowed_effects(
            &prepared.decl,
            args,
            &self.inner.context.handles,
            &self.inner.slots,
        );

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
                    let handle_slot = &self.inner.slots[*slot as usize];
                    let any = match kind {
                        HandleKind::ReadDir => {
                            let host_dir = match handle_slot {
                                HandleSlot::ReadDir(d) | HandleSlot::WriteDir(d) => d.clone(),
                                HandleSlot::ExecEnv(_) => {
                                    unreachable!("κ 已保证 ReadDir 形参只接收目录槽位");
                                }
                            };
                            let resource = store
                                .data_mut()
                                .table
                                .push(ReadDirResource(host_dir))
                                .map_err(|err| {
                                format!("注入只读目录句柄 `{}` 失败：{err}", param.name)
                            })?;
                            ResourceAny::try_from_resource(resource, &mut *store).map_err(
                                |err| format!("转换只读目录句柄 `{}` 失败：{err}", param.name),
                            )?
                        }
                        HandleKind::WriteDir => {
                            let HandleSlot::WriteDir(host_dir) = handle_slot else {
                                unreachable!("κ 已保证 WriteDir 形参只接收可写目录槽位");
                            };
                            let resource = store
                                .data_mut()
                                .table
                                .push(WriteDirResource(host_dir.clone()))
                                .map_err(|err| {
                                    format!("注入可写目录句柄 `{}` 失败：{err}", param.name)
                                })?;
                            ResourceAny::try_from_resource(resource, &mut *store).map_err(
                                |err| format!("转换可写目录句柄 `{}` 失败：{err}", param.name),
                            )?
                        }
                        HandleKind::ExecEnv => {
                            let HandleSlot::ExecEnv(env) = handle_slot else {
                                unreachable!("κ 已保证 ExecEnv 形参只接收执行环境槽位");
                            };
                            let resource = store
                                .data_mut()
                                .table
                                .push(ExecEnvResource(env.clone()))
                                .map_err(|err| {
                                    format!("注入执行环境句柄 `{}` 失败：{err}", param.name)
                                })?;
                            ResourceAny::try_from_resource(resource, &mut *store).map_err(
                                |err| format!("转换执行环境句柄 `{}` 失败：{err}", param.name),
                            )?
                        }
                        HandleKind::Egress => {
                            unreachable!("网络出口句柄随 #9 注入")
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

fn append_effects(effects: &Mutex<Vec<EffectEntry>>, entries: Vec<EffectEntry>) {
    let mut guard = effects.lock().expect("effect 互斥锁不应中毒");
    guard.extend(entries);
}

pub(crate) fn instantiate_allowed_effects(
    decl: &ToolDecl,
    args: &[DispatchArgument],
    grants: &[HandleGrant],
    slots: &[HandleSlot],
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
            EffectItem::WriteWritableMountsOf(idx) => {
                let DispatchArgument::HandleRef(slot) = args[idx] else {
                    continue;
                };
                if let Some(HandleSlot::ExecEnv(env)) = slots.get(slot as usize) {
                    for mount in env.mounts() {
                        if mount.writable {
                            allowed.insert((mount.dir.name().to_owned(), EffectKind::Write));
                        }
                    }
                }
            }
            EffectItem::EmitViaEgressOf(_) => {
                // 网络出口归 #9，本阶段展开为空。
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
    if err.downcast_ref::<DispatchTrap>().is_some() {
        record_step(steps, tool_id, OutcomeKind::Failed);
        return Err(err);
    }
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
    slots: &[HandleSlot],
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
            CoEffectItem::MountsOf(i) => {
                let param = &decl.params[i];
                let slot = match &args[i] {
                    DispatchArgument::HandleRef(slot) => *slot,
                    DispatchArgument::ValueJson(_) => {
                        return Err(format!(
                            "工具 `{}` 参数 `{}` 需要执行环境句柄实参",
                            decl.script_name, param.name
                        ));
                    }
                };
                let env_grant = grants
                    .iter()
                    .find(|g| g.slot == slot)
                    .ok_or_else(|| format!("执行环境槽位 {slot} 未授予给本次调用"))?;
                let Some(HandleSlot::ExecEnv(env)) = slots.get(slot as usize) else {
                    return Err(format!(
                        "执行环境 `{}`（槽位 {slot}）缺少执行环境槽位",
                        env_grant.name
                    ));
                };
                for mount in env.mounts() {
                    let mount_name = mount.dir.name();
                    let dir_grant = grants
                        .iter()
                        .find(|g| g.slot == mount.slot && g.name == mount_name)
                        .ok_or_else(|| {
                            format!(
                                "执行环境 `{}` 挂载的工作目录 `{}`（槽位 {}）未授予给本次调用",
                                env_grant.name, mount_name, mount.slot
                            )
                        })?;
                    let required = if mount.writable {
                        HandleKind::WriteDir
                    } else {
                        HandleKind::ReadDir
                    };
                    if !required.accepts(dir_grant.kind) {
                        return Err(format!(
                            "执行环境 `{}` 挂载的工作目录 `{}` 需要 `{required:?}` 权限，实际授予 `{:?}`",
                            env_grant.name, mount_name, dir_grant.kind
                        ));
                    }
                }
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use lancet_tool_decl::{
        CoEffectItem, EffectItem, HandleKind, Mode, ParamDecl, ReturnType, Templates, WitType,
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

    fn sample_exec_decl() -> ToolDecl {
        ToolDecl {
            script_name: "proc.run".to_owned(),
            wit_interface: "lancet:proc-run/proc@0.1.0".to_owned(),
            wit_function: "run".to_owned(),
            docs: None,
            params: vec![
                ParamDecl {
                    name: "env".to_owned(),
                    ty: WitType::Handle {
                        kind: HandleKind::ExecEnv,
                        borrow: true,
                    },
                },
                ParamDecl {
                    name: "argv".to_owned(),
                    ty: WitType::List(Box::new(WitType::String)),
                },
            ],
            result: ReturnType::Unit,
            records: Vec::new(),
            templates: Templates {
                coeffects: vec![CoEffectItem::Uses(0, None), CoEffectItem::MountsOf(0)],
                effects: vec![
                    EffectItem::Effect(0, lancet_tool_decl::EffectKind::Exec),
                    EffectItem::WriteWritableMountsOf(0),
                    EffectItem::EmitViaEgressOf(0),
                ],
            },
        }
    }

    #[test]
    fn check_coeffects_enforces_grants_and_modes() {
        let read_decl = sample_decl(Mode::Read, HandleKind::ReadDir);
        let write_decl = sample_decl(Mode::ReadWrite, HandleKind::WriteDir);
        let empty_slots: Vec<HandleSlot> = Vec::new();

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
                &empty_slots,
            )
            .is_err()
        );

        // 需要 ReadWrite 而只授予 ReadDir。
        assert!(
            check_coeffects(
                &write_decl,
                &[DispatchArgument::HandleRef(0), path_arg.clone()],
                std::slice::from_ref(&ro_grant),
                &empty_slots,
            )
            .is_err()
        );

        // 该传句柄处传了 JSON。
        assert!(
            check_coeffects(
                &read_decl,
                &[path_arg.clone(), path_arg.clone()],
                std::slice::from_ref(&ro_grant),
                &empty_slots,
            )
            .is_err()
        );

        // 句柄种类不匹配（ExecEnv 传给 ReadDir）。
        assert!(
            check_coeffects(
                &read_decl,
                &[DispatchArgument::HandleRef(2), path_arg.clone()],
                &[env_grant],
                &empty_slots,
            )
            .is_err()
        );

        // Read 接受 ReadDir 与 WriteDir；ReadWrite 接受 WriteDir。
        check_coeffects(
            &read_decl,
            &[DispatchArgument::HandleRef(0), path_arg.clone()],
            &[ro_grant],
            &empty_slots,
        )
        .expect("Read 应接受 ReadDir");
        check_coeffects(
            &read_decl,
            &[DispatchArgument::HandleRef(1), path_arg.clone()],
            std::slice::from_ref(&rw_grant),
            &empty_slots,
        )
        .expect("Read 应接受 WriteDir");
        check_coeffects(
            &write_decl,
            &[DispatchArgument::HandleRef(1), path_arg],
            &[rw_grant],
            &empty_slots,
        )
        .expect("ReadWrite 应接受 WriteDir");
    }

    #[test]
    fn exec_env_templates_expand_mounts_and_writable_effects() {
        let tmp = tempfile::tempdir().expect("创建临时目录失败");
        let ro_path = tmp.path().join("ro");
        let rw_path = tmp.path().join("rw");
        std::fs::create_dir(&ro_path).unwrap();
        std::fs::create_dir(&rw_path).unwrap();
        let ro_dir = HostDir::open("ro", &ro_path).unwrap();
        let rw_dir = HostDir::open("rw", &rw_path).unwrap();

        let env = HostExecEnv::for_unit_test(
            "dev",
            vec![
                crate::exec_env::EnvMount {
                    slot: 0,
                    dir: ro_dir.clone(),
                    writable: false,
                },
                crate::exec_env::EnvMount {
                    slot: 1,
                    dir: rw_dir.clone(),
                    writable: true,
                },
            ],
        );
        let slots = vec![
            HandleSlot::ReadDir(ro_dir),
            HandleSlot::WriteDir(rw_dir),
            HandleSlot::ExecEnv(env),
        ];

        let exec_decl = sample_exec_decl();
        let argv_arg = DispatchArgument::ValueJson("[\"true\"]".to_owned());
        let args = [DispatchArgument::HandleRef(2), argv_arg];

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
            name: "dev".to_owned(),
            kind: HandleKind::ExecEnv,
            slot: 2,
        };

        // 全部挂载目录与模式满足时，MountsOf 通过。
        let full_grants = [ro_grant.clone(), rw_grant.clone(), env_grant.clone()];
        check_coeffects(&exec_decl, &args, &full_grants, &slots)
            .expect("挂载目录均已按模式授予时 MountsOf 应通过");

        // 缺少挂载目录授权时拒绝。
        let missing_rw = [ro_grant.clone(), env_grant.clone()];
        assert!(
            check_coeffects(&exec_decl, &args, &missing_rw, &slots).is_err(),
            "缺少已挂载目录授权时 MountsOf 应失败"
        );

        // 可写挂载对应的槽位只有 ReadDir 授权时拒绝。
        let downgraded_rw = [
            ro_grant,
            HandleGrant {
                name: "rw".to_owned(),
                kind: HandleKind::ReadDir,
                slot: 1,
            },
            env_grant,
        ];
        assert!(
            check_coeffects(&exec_decl, &args, &downgraded_rw, &slots).is_err(),
            "可写挂载仅获只读授权时 MountsOf 应失败"
        );

        // WriteWritableMountsOf 只列出实际可写挂载；EmitViaEgressOf 展开为空。
        let allowed = instantiate_allowed_effects(&exec_decl, &args, &full_grants, &slots);
        assert_eq!(
            allowed,
            HashSet::from([
                ("dev".to_owned(), EffectKind::Exec),
                ("rw".to_owned(), EffectKind::Write),
            ])
        );
    }
}
