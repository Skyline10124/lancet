// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! `act` Starlark 解释器组件运行时。

mod engine;

use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::Context as _;
use lancet_tool_decl::ActContext;
use wasmtime::component::{Component, HasSelf, Linker};
use wasmtime::{Engine, ResourceLimiter, Store, UpdateDeadline};

wasmtime::component::bindgen!({
    world: "act",
    path: "../lancet-act/wit",
    additional_derives: [PartialEq, Eq],
    imports: {
        "lancet:act/dispatch.call": trappable,
        "lancet:act/dispatch.discard": trappable,
    },
});

pub use exports::lancet::act::interpreter::{Diagnostic, Payload, Phase};
pub use lancet::act::dispatch::Argument as DispatchArgument;

/// 单次 `act` 解释器组件调用的线性内存上限（128 MiB）。
pub const ACT_MEMORY_LIMIT_BYTES: usize = 128 * 1024 * 1024;

/// 驱动 `wasmtime` epoch 中断检查的时钟周期（10 ms）。
pub const ACT_EPOCH_INTERVAL: Duration = Duration::from_millis(10);

/// 构建产物中嵌入的预编译 `act` 组件字节。
const PRECOMPILED_ACT: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/act.cwasm"));

/// `act` 解释器组件运行时，持有复用的 `wasmtime::Engine` 与预链接组件。
pub struct ActRuntime {
    engine: Engine,
    pre: ActPre<StoreState>,
}

/// `act` 脚本执行或组件边界失败的分类错误。
#[derive(Debug)]
pub enum ActError {
    /// 解释器组件交回的脚本诊断（解析、静态检查或求值错误）。
    Script(Diagnostic),
    /// 线性内存分配或增长超出 `ACT_MEMORY_LIMIT_BYTES`。
    MemoryLimit,
    /// 到达调用方指定的 `deadline`。
    Deadline,
    /// `cancelled` 标志被置位。
    Cancelled,
    /// 组件其余陷入、实例化或调用失败。
    Component(anyhow::Error),
}

#[derive(Debug)]
pub(crate) enum Interrupt {
    MemoryLimit,
    Cancelled,
    Deadline,
}

impl fmt::Display for Interrupt {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MemoryLimit => write!(f, "组件超出线性内存上限"),
            Self::Cancelled => f.write_str("组件已被取消"),
            Self::Deadline => f.write_str("组件到达截止时间"),
        }
    }
}

impl std::error::Error for Interrupt {}

pub(crate) struct MemoryBudget {
    allocated_bytes: usize,
    limit_bytes: usize,
}

impl MemoryBudget {
    pub(crate) fn new(limit_bytes: usize) -> Self {
        Self {
            allocated_bytes: 0,
            limit_bytes,
        }
    }
}

impl ResourceLimiter for MemoryBudget {
    fn memory_growing(
        &mut self,
        current: usize,
        desired: usize,
        _maximum: Option<usize>,
    ) -> wasmtime::Result<bool> {
        let delta = desired.saturating_sub(current);
        let total = self.allocated_bytes.saturating_add(delta);
        if total > self.limit_bytes {
            return Err(wasmtime::Error::new(Interrupt::MemoryLimit));
        }
        self.allocated_bytes = total;
        Ok(true)
    }

    fn table_growing(
        &mut self,
        _current: usize,
        _desired: usize,
        _maximum: Option<usize>,
    ) -> wasmtime::Result<bool> {
        Ok(true)
    }
}

pub(crate) fn check_epoch_interrupt(
    deadline: Instant,
    cancelled: &AtomicBool,
) -> wasmtime::Result<UpdateDeadline> {
    if cancelled.load(Ordering::Relaxed) {
        return Err(wasmtime::Error::new(Interrupt::Cancelled));
    }
    if Instant::now() >= deadline {
        return Err(wasmtime::Error::new(Interrupt::Deadline));
    }
    Ok(UpdateDeadline::Continue(1))
}

/// `act` 解释器向宿主发起的工具分发与 `discard()` 回调。
pub trait ActHost: Send {
    fn call(
        &mut self,
        tool_id: &str,
        args: &[DispatchArgument],
    ) -> wasmtime::Result<Result<String, String>>;

    fn discard(&mut self) -> wasmtime::Result<Result<(), String>> {
        Ok(Err("当前调用未配置 discard()".to_owned()))
    }
}

impl<F> ActHost for F
where
    F: FnMut(&str, &[DispatchArgument]) -> wasmtime::Result<Result<String, String>> + Send,
{
    fn call(
        &mut self,
        tool_id: &str,
        args: &[DispatchArgument],
    ) -> wasmtime::Result<Result<String, String>> {
        self(tool_id, args)
    }
}

struct StoreState {
    memory: MemoryBudget,
    deadline: Instant,
    cancelled: Arc<AtomicBool>,
    host: Box<dyn ActHost>,
}

impl lancet::act::dispatch::Host for StoreState {
    fn call(
        &mut self,
        tool_id: String,
        args: Vec<DispatchArgument>,
    ) -> wasmtime::Result<Result<String, String>> {
        self.host.call(&tool_id, &args)
    }

    fn discard(&mut self) -> wasmtime::Result<Result<(), String>> {
        self.host.discard()
    }
}

impl ActRuntime {
    /// 加载预编译 `act` 组件并启动 epoch 计时线程。
    pub fn load() -> anyhow::Result<Self> {
        let engine = Engine::new(&engine::engine_config())
            .map_err(anyhow::Error::from)
            .context("创建 wasmtime 运行时引擎失败")?;
        // SAFETY: `PRECOMPILED_ACT` 仅来自同一构建由相同 `engine_config()` 预编译的产物。
        let component = unsafe { Component::deserialize(&engine, PRECOMPILED_ACT) }
            .map_err(anyhow::Error::from)
            .context("反序列化预编译 act 组件失败")?;
        let mut linker = Linker::<StoreState>::new(&engine);
        lancet::act::dispatch::add_to_linker::<_, HasSelf<_>>(&mut linker, |state| state)
            .map_err(anyhow::Error::from)
            .context("注册 act dispatch 导入到链接器失败")?;
        let instance_pre = linker
            .instantiate_pre(&component)
            .map_err(anyhow::Error::from)
            .context("预链接 act 组件失败（组件只能导入 `lancet:act/dispatch`）")?;
        let pre = ActPre::new(instance_pre)
            .map_err(anyhow::Error::from)
            .context("解析 act 组件导出接口失败")?;

        let weak = engine.weak();
        std::thread::Builder::new()
            .name("act-epoch".to_owned())
            .spawn(move || {
                loop {
                    std::thread::sleep(ACT_EPOCH_INTERVAL);
                    let Some(engine) = weak.upgrade() else {
                        break;
                    };
                    engine.increment_epoch();
                }
            })
            .context("启动 act epoch 计时线程失败")?;

        Ok(Self { engine, pre })
    }

    /// 返回与解释器共用的 `wasmtime::Engine`（由同一 epoch 计时线程驱动）。
    pub fn engine(&self) -> &Engine {
        &self.engine
    }

    /// 在独立 `Store` 中执行一次 Starlark 脚本，施加内存上限、截止时间、取消控制与宿主分发。
    pub fn run(
        &self,
        script: &str,
        context: &ActContext,
        host: impl ActHost + 'static,
        deadline: Instant,
        cancelled: Arc<AtomicBool>,
    ) -> Result<Payload, ActError> {
        let context_json = serde_json::to_string(context)
            .map_err(anyhow::Error::from)
            .map_err(ActError::Component)?;
        let mut store = self.new_store(Box::new(host), deadline, cancelled);
        let call_res = (|| -> wasmtime::Result<Result<Payload, Diagnostic>> {
            let bindings = self.pre.instantiate(&mut store)?;
            bindings
                .lancet_act_interpreter()
                .call_run(&mut store, script, &context_json)
        })();
        match call_res {
            Ok(Ok(payload)) => Ok(payload),
            Ok(Err(diag)) => Err(ActError::Script(diag)),
            Err(err) => Err(classify_wasmtime_error(err)),
        }
    }

    /// 实例化组件一次并读取 `act` 内置全局环境的 `.pyi` 类型存根。
    pub fn builtin_stub(&self) -> anyhow::Result<String> {
        let mut store = self.new_store(
            Box::new(|_: &str, _: &[DispatchArgument]| {
                Ok(Err("builtin-stub 不支持子调用分发".to_owned()))
            }),
            Instant::now() + Duration::from_secs(5),
            Arc::new(AtomicBool::new(false)),
        );
        let bindings = self
            .pre
            .instantiate(&mut store)
            .map_err(anyhow::Error::from)
            .context("实例化 act 组件以读取 builtin-stub 失败")?;
        bindings
            .lancet_act_interpreter()
            .call_builtin_stub(&mut store)
            .map_err(anyhow::Error::from)
            .context("调用 act 组件 builtin-stub 失败")
    }

    /// 校验配置中的句柄名是否为合法 Starlark 标识符且不与内置全局或包装入口重名。
    pub fn check_handle_name(&self, name: &str) -> anyhow::Result<()> {
        let mut store = self.new_store(
            Box::new(|_: &str, _: &[DispatchArgument]| {
                Ok(Err("check-handle-name 不支持子调用分发".to_owned()))
            }),
            Instant::now() + Duration::from_secs(5),
            Arc::new(AtomicBool::new(false)),
        );
        let bindings = self
            .pre
            .instantiate(&mut store)
            .map_err(anyhow::Error::from)
            .context("实例化 act 组件以校验句柄名称失败")?;
        let res = bindings
            .lancet_act_interpreter()
            .call_check_handle_name(&mut store, name)
            .map_err(anyhow::Error::from)
            .context("调用 act 组件 check-handle-name 失败")?;
        res.map_err(anyhow::Error::msg)
    }

    fn new_store(
        &self,
        host: Box<dyn ActHost>,
        deadline: Instant,
        cancelled: Arc<AtomicBool>,
    ) -> Store<StoreState> {
        let mut store = Store::new(
            &self.engine,
            StoreState {
                memory: MemoryBudget::new(ACT_MEMORY_LIMIT_BYTES),
                deadline,
                cancelled,
                host,
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
}

fn classify_wasmtime_error(err: wasmtime::Error) -> ActError {
    match err.downcast_ref::<Interrupt>() {
        Some(Interrupt::MemoryLimit) => ActError::MemoryLimit,
        Some(Interrupt::Cancelled) => ActError::Cancelled,
        Some(Interrupt::Deadline) => ActError::Deadline,
        None => ActError::Component(anyhow::Error::from(err)),
    }
}
