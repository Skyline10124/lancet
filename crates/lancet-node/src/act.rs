// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! `act` Starlark 解释器组件运行时。

mod engine;

use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::Context as _;
use wasmtime::component::{Component, Linker};
use wasmtime::{Engine, ResourceLimiter, Store, UpdateDeadline};

wasmtime::component::bindgen!({
    world: "act",
    path: "../lancet-act/wit",
    additional_derives: [PartialEq, Eq],
});

pub use exports::lancet::act::interpreter::{Diagnostic, Payload, Phase};

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
enum Interrupt {
    MemoryLimit,
    Cancelled,
    Deadline,
}

impl fmt::Display for Interrupt {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MemoryLimit => write!(
                f,
                "解释器组件超出线性内存上限（{ACT_MEMORY_LIMIT_BYTES} 字节）"
            ),
            Self::Cancelled => f.write_str("解释器组件已被取消"),
            Self::Deadline => f.write_str("解释器组件到达截止时间"),
        }
    }
}

impl std::error::Error for Interrupt {}

struct MemoryBudget {
    allocated_bytes: usize,
}

impl MemoryBudget {
    fn new() -> Self {
        Self { allocated_bytes: 0 }
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
        if total > ACT_MEMORY_LIMIT_BYTES {
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

struct StoreState {
    memory: MemoryBudget,
    deadline: Instant,
    cancelled: Arc<AtomicBool>,
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
        let linker = Linker::<StoreState>::new(&engine);
        let instance_pre = linker
            .instantiate_pre(&component)
            .map_err(anyhow::Error::from)
            .context("预链接 act 组件失败（组件不得有任何未满足的导入）")?;
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

    /// 在独立 `Store` 中执行一次 Starlark 脚本，施加内存上限、截止时间与取消控制。
    pub fn run(
        &self,
        script: &str,
        deadline: Instant,
        cancelled: Arc<AtomicBool>,
    ) -> Result<Payload, ActError> {
        let mut store = self.new_store(deadline, cancelled);
        let call_res = (|| -> wasmtime::Result<Result<Payload, Diagnostic>> {
            let bindings = self.pre.instantiate(&mut store)?;
            bindings
                .lancet_act_interpreter()
                .call_run(&mut store, script)
        })();
        match call_res {
            Ok(Ok(payload)) => Ok(payload),
            Ok(Err(diag)) => Err(ActError::Script(diag)),
            Err(err) => Err(classify_wasmtime_error(err)),
        }
    }

    fn new_store(&self, deadline: Instant, cancelled: Arc<AtomicBool>) -> Store<StoreState> {
        let mut store = Store::new(
            &self.engine,
            StoreState {
                memory: MemoryBudget::new(),
                deadline,
                cancelled,
            },
        );
        store.limiter(|state| &mut state.memory);
        store.set_epoch_deadline(1);
        store.epoch_deadline_callback(|cx| {
            let state = cx.data();
            if state.cancelled.load(Ordering::Relaxed) {
                return Err(wasmtime::Error::new(Interrupt::Cancelled));
            }
            if Instant::now() >= state.deadline {
                return Err(wasmtime::Error::new(Interrupt::Deadline));
            }
            Ok(UpdateDeadline::Continue(1))
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
