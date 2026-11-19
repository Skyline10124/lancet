// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use lancet_node::act::{ActError, ActRuntime, DispatchArgument, Phase};
use lancet_node::dispatch::{ComponentArtifact, Dispatcher};
use lancet_node::read_dir::ReadDir;
use lancet_node::tools::embedded_artifacts;
use lancet_protocol::{OutcomeKind, StepSummary};
use lancet_tool_decl::ToolDecl;
use tempfile::TempDir;

const PROBE_DECLS_JSON: &str = include_str!(concat!(env!("OUT_DIR"), "/tool-probe.decl.json"));
const PROBE_CWASM: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/tool-probe.cwasm"));

fn setup_harness() -> (TempDir, ActRuntime, Dispatcher) {
    let dir = TempDir::new().expect("创建临时目录失败");
    let read_dir = ReadDir::open("ro", dir.path(), &[]).expect("打开只读目录资源失败");
    let rt = ActRuntime::load().expect("加载 act 运行时失败");

    let mut artifacts = embedded_artifacts();
    let probe_decls: Vec<ToolDecl> =
        serde_json::from_str(PROBE_DECLS_JSON).expect("解析 tool-probe.decl.json 失败");
    artifacts.push(ComponentArtifact {
        name: "tool-probe",
        decls: probe_decls,
        cwasm: PROBE_CWASM,
    });

    // SAFETY: `embedded_artifacts()` 与 `PROBE_CWASM` 均由同一构建以相同 `engine_config()` 预编译。
    let dispatcher = unsafe { Dispatcher::new(rt.engine(), &artifacts, vec![read_dir]) }
        .expect("构造分发器失败");
    (dir, rt, dispatcher)
}

fn run_with_hook<F>(
    rt: &ActRuntime,
    dispatcher: &Dispatcher,
    script: &str,
    deadline: Instant,
    cancelled: Arc<AtomicBool>,
    on_dispatch: F,
) -> (Result<String, ActError>, Vec<StepSummary>)
where
    F: FnMut(&str, Instant) -> Instant + Send + 'static,
{
    let steps = Arc::new(Mutex::new(Vec::new()));
    let steps_bg = Arc::clone(&steps);
    let dispatcher_bg = dispatcher.clone();
    let cancel_bg = Arc::clone(&cancelled);
    let mut on_dispatch = on_dispatch;

    let res = rt.run(
        script,
        dispatcher.context(),
        move |tool_id, args| {
            let call_deadline = on_dispatch(tool_id, deadline);
            dispatcher_bg.call(
                tool_id,
                args,
                call_deadline,
                Arc::clone(&cancel_bg),
                &steps_bg,
            )
        },
        deadline,
        cancelled,
    );
    let recorded = std::mem::take(&mut *steps.lock().expect("步骤互斥锁不应中毒"));
    (res.map(|p| p.value_json), recorded)
}

fn run_with_dispatcher(
    rt: &ActRuntime,
    dispatcher: &Dispatcher,
    script: &str,
    deadline: Instant,
    cancelled: Arc<AtomicBool>,
) -> (Result<String, ActError>, Vec<StepSummary>) {
    run_with_hook(rt, dispatcher, script, deadline, cancelled, |_, d| d)
}

#[test]
fn dispatch_checks_wit_numeric_ranges() {
    let (_dir, rt, dispatcher) = setup_harness();
    let f32_max = f32::MAX;
    let f32_max_str = format!("{:?}", f32_max as f64);

    // 边界合法值：u8 (0, 255), s32 (MIN, MAX), u64::MAX, f32::MAX, list<u8> ([0, 255])。
    let ok_script = format!(
        r#"
r1 = ro.echo(0, -2147483648, 18446744073709551615, {f32_max_str}, [0, 255])
r2 = ro.echo(255, 2147483647, 42, 0.0, [])
return [r1, r2]
"#
    );
    let (res, steps) = run_with_dispatcher(
        &rt,
        &dispatcher,
        &ok_script,
        Instant::now() + Duration::from_secs(5),
        Arc::new(AtomicBool::new(false)),
    );
    let json = res.expect("合法数值边界应成功通过");
    let vals: Vec<String> = serde_json::from_str(&json).expect("解析返回数组失败");
    assert_eq!(
        vals[0],
        format!("a=0,b=-2147483648,c=18446744073709551615,d={f32_max:?},e=[0, 255]")
    );
    assert_eq!(vals[1], "a=255,b=2147483647,c=42,d=0.0,e=[]");
    assert_eq!(steps.len(), 2);
    assert!(steps.iter().all(|s| s.outcome == OutcomeKind::Succeeded));

    // 越界值：分别核对参数名与报错。
    let bad_cases = [
        ("u8 下溢 -1", "ro.echo(-1, 0, 0, 0.0, [])", "a"),
        ("u8 上溢 256", "ro.echo(256, 0, 0, 0.0, [])", "a"),
        ("s32 下溢", "ro.echo(0, -2147483649, 0, 0.0, [])", "b"),
        ("s32 上溢", "ro.echo(0, 2147483648, 0, 0.0, [])", "b"),
        ("f32 上溢 1e39", "ro.echo(0, 0, 0, 1e39, [])", "d"),
        ("f32 正刚越界", "ro.echo(0, 0, 0, 3.4028235e38, [])", "d"),
        ("f32 负刚越界", "ro.echo(0, 0, 0, -3.4028235e38, [])", "d"),
        ("list<u8> 元素越界", "ro.echo(0, 0, 0, 0.0, [1, 256])", "e"),
    ];

    for (case_name, script, param_name) in bad_cases {
        let (bad_res, bad_steps) = run_with_dispatcher(
            &rt,
            &dispatcher,
            script,
            Instant::now() + Duration::from_secs(5),
            Arc::new(AtomicBool::new(false)),
        );
        match bad_res {
            Err(ActError::Script(diag)) => {
                assert_eq!(diag.phase, Phase::Eval, "`{case_name}`：{diag:?}");
                let expected = format!("参数 `{param_name}`");
                assert!(
                    diag.message.contains(&expected),
                    "`{case_name}` 报错应写明 `{expected}`，实际：{}",
                    diag.message
                );
            }
            other => panic!("`{case_name}` 预期脚本报错，实际：{other:?}"),
        }
        assert_eq!(bad_steps.len(), 1);
        assert_eq!(bad_steps[0].outcome, OutcomeKind::Failed);
    }
}

#[test]
fn tool_trap_fails_only_that_call() {
    let (dir, rt, dispatcher) = setup_harness();
    std::fs::write(dir.path().join("hello.txt"), "world").expect("写入测试文件失败");

    let script = "ok = ro.echo(1, 2, 3, 1.5, [7])\nro.misuse()\nreturn ok\n";
    let (res, steps) = run_with_dispatcher(
        &rt,
        &dispatcher,
        script,
        Instant::now() + Duration::from_secs(5),
        Arc::new(AtomicBool::new(false)),
    );
    match res {
        Err(ActError::Script(diag)) => {
            assert_eq!(diag.phase, Phase::Eval);
            assert_eq!(diag.line, Some(2));
            assert!(
                diag.message.contains("陷入"),
                "工具陷入报错应包含“陷入”，实际：{}",
                diag.message
            );
        }
        other => panic!("misuse 应导致 Script 错误，实际：{other:?}"),
    }
    assert_eq!(
        steps,
        vec![
            StepSummary {
                tool: "probe.echo".to_owned(),
                outcome: OutcomeKind::Succeeded,
            },
            StepSummary {
                tool: "probe.misuse".to_owned(),
                outcome: OutcomeKind::Failed,
            },
        ]
    );

    // 随后的正常 act 同一运行时与分发层应继续正常服务。
    let (next_res, next_steps) = run_with_dispatcher(
        &rt,
        &dispatcher,
        "return ro.read(\"hello.txt\")",
        Instant::now() + Duration::from_secs(5),
        Arc::new(AtomicBool::new(false)),
    );
    assert_eq!(next_res.expect("陷入后再次调用应成功"), "\"world\"");
    assert_eq!(
        next_steps,
        vec![StepSummary {
            tool: "fs.read".to_owned(),
            outcome: OutcomeKind::Succeeded,
        }]
    );
}

#[test]
fn tool_limits_interrupt_only_that_call() {
    let (dir, rt, dispatcher) = setup_harness();
    std::fs::write(dir.path().join("ok.txt"), "healthy").expect("写入测试文件失败");

    // 1. 内存上限：grow()
    let (grow_res, grow_steps) = run_with_dispatcher(
        &rt,
        &dispatcher,
        "ro.grow()",
        Instant::now() + Duration::from_secs(5),
        Arc::new(AtomicBool::new(false)),
    );
    match grow_res {
        Err(ActError::Script(diag)) => {
            assert!(
                diag.message.contains("134217728"),
                "内存超限报错应写明 134217728 字节，实际：{}",
                diag.message
            );
        }
        other => panic!("grow 应返回 Script 内存上限错误，实际：{other:?}"),
    }
    assert_eq!(
        grow_steps,
        vec![StepSummary {
            tool: "probe.grow".to_owned(),
            outcome: OutcomeKind::Failed,
        }]
    );

    // 2. 短截止时间：进入分发闭包后为 spin() 赋予 50 ms 截止时间。
    let (deadline_res, deadline_steps) = run_with_hook(
        &rt,
        &dispatcher,
        "ro.spin()",
        Instant::now() + Duration::from_secs(10),
        Arc::new(AtomicBool::new(false)),
        |_, _| Instant::now() + Duration::from_millis(50),
    );
    assert!(
        matches!(deadline_res, Err(ActError::Deadline)),
        "短截止下的 spin 应返回 ActError::Deadline，实际：{deadline_res:?}"
    );
    assert_eq!(
        deadline_steps,
        vec![StepSummary {
            tool: "probe.spin".to_owned(),
            outcome: OutcomeKind::Deadline,
        }]
    );

    // 3. 分发闭包发信号后再由测试线程置位取消标志：spin()
    let cancelled = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&cancelled);
    let (tx, rx) = std::sync::mpsc::channel();
    let (cancel_res, cancel_steps) = std::thread::scope(|scope| {
        scope.spawn(move || {
            rx.recv().expect("应收到分发开始信号");
            std::thread::sleep(Duration::from_millis(20));
            flag.store(true, Ordering::Release);
        });
        run_with_hook(
            &rt,
            &dispatcher,
            "ro.spin()",
            Instant::now() + Duration::from_secs(10),
            cancelled,
            move |_, d| {
                let _ = tx.send(());
                d
            },
        )
    });
    assert!(
        matches!(cancel_res, Err(ActError::Cancelled)),
        "被取消的 spin 应返回 ActError::Cancelled，实际：{cancel_res:?}"
    );
    assert_eq!(
        cancel_steps,
        vec![StepSummary {
            tool: "probe.spin".to_owned(),
            outcome: OutcomeKind::Cancelled,
        }]
    );

    // 4. 随后的正常调用成功。
    let (ok_res, ok_steps) = run_with_dispatcher(
        &rt,
        &dispatcher,
        "return ro.read(\"ok.txt\")",
        Instant::now() + Duration::from_secs(5),
        Arc::new(AtomicBool::new(false)),
    );
    assert_eq!(ok_res.expect("各类中断后的正常调用应成功"), "\"healthy\"");
    assert_eq!(
        ok_steps,
        vec![StepSummary {
            tool: "fs.read".to_owned(),
            outcome: OutcomeKind::Succeeded,
        }]
    );
}

fn percentile_us(samples: &[Duration], numer: usize, denom: usize) -> f64 {
    let mut sorted = samples.to_vec();
    sorted.sort_unstable();
    let idx = (sorted.len().saturating_sub(1) * numer) / denom;
    sorted[idx].as_secs_f64() * 1_000_000.0
}

#[test]
#[ignore = "单线程实测三个产品工具完整 Dispatcher::call 的开销；手动运行"]
fn measure_tool_call_overhead() {
    let (_dir, _rt, dispatcher) = setup_harness();
    let cases = [
        (
            "fs.read",
            "lancet:fs-read/fs@0.1.0#read",
            vec![
                DispatchArgument::HandleRef(0),
                DispatchArgument::ValueJson("\"absent\"".to_owned()),
                DispatchArgument::ValueJson("true".to_owned()),
            ],
        ),
        (
            "fs.list",
            "lancet:fs-list/fs@0.1.0#list",
            vec![
                DispatchArgument::HandleRef(0),
                DispatchArgument::ValueJson("\"absent\"".to_owned()),
                DispatchArgument::ValueJson("true".to_owned()),
            ],
        ),
        (
            "fs.search",
            "lancet:fs-search/fs@0.1.0#search",
            vec![
                DispatchArgument::HandleRef(0),
                DispatchArgument::ValueJson("\"x\"".to_owned()),
                DispatchArgument::ValueJson("null".to_owned()),
            ],
        ),
    ];

    let steps = Mutex::new(Vec::with_capacity(1));
    let cancelled = Arc::new(AtomicBool::new(false));
    for (label, tool_id, args) in cases {
        let deadline = Instant::now() + Duration::from_secs(30);
        for _ in 0..10 {
            let res = dispatcher
                .call(tool_id, &args, deadline, Arc::clone(&cancelled), &steps)
                .unwrap_or_else(|e| panic!("预热工具 `{label}` 失败：{e:#}"));
            res.unwrap_or_else(|e| panic!("预热工具 `{label}` 返回错误：{e}"));
        }

        let mut samples = Vec::with_capacity(1_000);
        for _ in 0..1_000 {
            steps.lock().expect("步骤互斥锁不应中毒").clear();
            let t0 = Instant::now();
            let res = dispatcher
                .call(tool_id, &args, deadline, Arc::clone(&cancelled), &steps)
                .unwrap_or_else(|e| panic!("采样工具 `{label}` 失败：{e:#}"));
            samples.push(t0.elapsed());
            res.unwrap_or_else(|e| panic!("采样工具 `{label}` 返回错误：{e}"));
        }

        println!(
            "{label} (Dispatcher::call on empty dir, 10 warmup + 1000 runs): p50={:.2}us p95={:.2}us",
            percentile_us(&samples, 50, 100),
            percentile_us(&samples, 95, 100),
        );
    }
}
