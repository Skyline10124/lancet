// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use lancet_node::act::{ActError, ActHost, ActRuntime, DispatchArgument, Phase};
use lancet_node::dispatch::{ComponentArtifact, DirSlot, Dispatcher};
use lancet_node::read_dir::ReadDir;
use lancet_node::shadow::ShadowDir;
use lancet_node::tools::embedded_artifacts;
use lancet_protocol::{EffectEntry, EffectKind, OutcomeKind, Residue, StepSummary};
use lancet_tool_decl::{EffectItem, ToolDecl};
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
    let dispatcher =
        unsafe { Dispatcher::new(rt.engine(), &artifacts, vec![DirSlot::ReadOnly(read_dir)]) }
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
) -> (Result<String, ActError>, Vec<StepSummary>, Vec<EffectEntry>)
where
    F: FnMut(&str, Instant) -> Instant + Send + 'static,
{
    let steps = Arc::new(Mutex::new(Vec::new()));
    let effects = Arc::new(Mutex::new(Vec::new()));
    let steps_bg = Arc::clone(&steps);
    let effects_bg = Arc::clone(&effects);
    let dispatcher_bg = dispatcher.clone();
    let cancel_bg = Arc::clone(&cancelled);

    struct TestActHost<F> {
        dispatcher: Dispatcher,
        deadline: Instant,
        cancelled: Arc<AtomicBool>,
        steps: Arc<Mutex<Vec<StepSummary>>>,
        effects: Arc<Mutex<Vec<EffectEntry>>>,
        on_dispatch: F,
    }

    impl<F> ActHost for TestActHost<F>
    where
        F: FnMut(&str, Instant) -> Instant + Send + 'static,
    {
        fn call(
            &mut self,
            tool_id: &str,
            args: &[DispatchArgument],
        ) -> wasmtime::Result<Result<String, String>> {
            let call_deadline = (self.on_dispatch)(tool_id, self.deadline);
            self.dispatcher.call(
                tool_id,
                args,
                call_deadline,
                Arc::clone(&self.cancelled),
                &self.steps,
                &self.effects,
            )
        }

        fn discard(&mut self) -> wasmtime::Result<Result<(), String>> {
            self.dispatcher.discard(
                self.deadline,
                Arc::clone(&self.cancelled),
                &self.steps,
                &self.effects,
            )
        }
    }

    let host = TestActHost {
        dispatcher: dispatcher_bg,
        deadline,
        cancelled: cancel_bg,
        steps: steps_bg,
        effects: effects_bg,
        on_dispatch,
    };

    let res = rt.run(script, dispatcher.context(), host, deadline, cancelled);
    let recorded_steps = std::mem::take(&mut *steps.lock().expect("步骤互斥锁不应中毒"));
    let recorded_effects = std::mem::take(&mut *effects.lock().expect("effect 互斥锁不应中毒"));
    (res.map(|p| p.value_json), recorded_steps, recorded_effects)
}

fn run_with_dispatcher(
    rt: &ActRuntime,
    dispatcher: &Dispatcher,
    script: &str,
    deadline: Instant,
    cancelled: Arc<AtomicBool>,
) -> (Result<String, ActError>, Vec<StepSummary>, Vec<EffectEntry>) {
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
    let (res, steps, _) = run_with_dispatcher(
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
        let (bad_res, bad_steps, _) = run_with_dispatcher(
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
    let (res, steps, _) = run_with_dispatcher(
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
    let (next_res, next_steps, _) = run_with_dispatcher(
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
    let (grow_res, grow_steps, _) = run_with_dispatcher(
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
    let (deadline_res, deadline_steps, _) = run_with_hook(
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
    let (cancel_res, cancel_steps, _) = std::thread::scope(|scope| {
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
    let (ok_res, ok_steps, _) = run_with_dispatcher(
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

#[test]
fn writable_slot_narrows_to_read_view() {
    let root = tempfile::Builder::new()
        .prefix("ln-disp-")
        .tempdir_in("/tmp")
        .expect("在 /tmp 创建临时目录失败");
    let host = root.path().join("host");
    let state = root.path().join("state");
    std::fs::create_dir_all(host.join("sub")).expect("创建宿主目录失败");
    std::fs::write(host.join("a.txt"), "base line\n").expect("写入 a.txt 失败");
    std::fs::write(host.join("sub/b.txt"), "hello\n").expect("写入 sub/b.txt 失败");

    let shadow = ShadowDir::open(&state, "rw", &host, &[], &[]).expect("建立影子失败");
    shadow
        .write("a.txt", "draft line\n")
        .expect("写入草稿 a.txt 失败");
    shadow
        .write("c.txt", "added draft\n")
        .expect("写入草稿 c.txt 失败");
    // 宿主同时修改 a.txt 并同步，使 a.txt 带 conflict=true。
    std::fs::write(host.join("a.txt"), "host line\n").expect("写入宿主 a.txt 失败");
    shadow.sync_from_host(None, None).expect("同步宿主失败");

    let rt = ActRuntime::load().expect("加载 act 运行时失败");
    let mut artifacts = embedded_artifacts();
    let probe_decls: Vec<ToolDecl> =
        serde_json::from_str(PROBE_DECLS_JSON).expect("解析 tool-probe.decl.json 失败");
    artifacts.push(ComponentArtifact {
        name: "tool-probe",
        decls: probe_decls,
        cwasm: PROBE_CWASM,
    });
    // SAFETY: 预编译产物均由同一构建与引擎配置生成。
    let dispatcher =
        unsafe { Dispatcher::new(rt.engine(), &artifacts, vec![DirSlot::Shadow(shadow)]) }
            .expect("构造分发器失败");

    let script = r#"
content = rw.read("a.txt")
entries = rw.list(".")
hits = rw.search("draft")
st = rw.status()
df = rw.diff()
return {"content": content, "entries": entries, "hits": hits, "status": st, "diff": df}
"#;
    let (res, steps, effects) = run_with_dispatcher(
        &rt,
        &dispatcher,
        script,
        Instant::now() + Duration::from_secs(5),
        Arc::new(AtomicBool::new(false)),
    );
    let val: serde_json::Value =
        serde_json::from_str(&res.expect("可写槽位调用只读工具应成功")).expect("解析 JSON 失败");
    assert_eq!(val["content"], "draft line\n");
    let entry_names: Vec<&str> = val["entries"]
        .as_array()
        .expect("entries 应为数组")
        .iter()
        .map(|e| e["name"].as_str().unwrap())
        .collect();
    assert_eq!(entry_names, vec!["a.txt", "c.txt", "sub"]);
    assert_eq!(
        val["hits"].as_array().expect("hits 应为数组").len(),
        2,
        "search 应看到草稿 a.txt 与 c.txt：{}",
        val["hits"]
    );
    assert_eq!(
        val["status"],
        serde_json::json!([
            {"path": "a.txt", "action": "modified", "conflict": true},
            {"path": "c.txt", "action": "added", "conflict": false},
        ])
    );
    let diffs = val["diff"].as_array().expect("diff 应为数组");
    assert_eq!(diffs.len(), 2);
    assert_eq!(diffs[0]["path"], "a.txt");
    assert_eq!(diffs[0]["conflict"], true);
    let patch_a = diffs[0]["patch"].as_str().expect("a.txt 应有文本 patch");
    assert!(
        patch_a.contains("-base line") && patch_a.contains("+draft line"),
        "diff 应为基准到影子：{patch_a}"
    );
    assert_eq!(
        steps.iter().map(|s| s.tool.as_str()).collect::<Vec<_>>(),
        vec!["fs.read", "fs.list", "fs.search", "fs.status", "fs.diff"]
    );
    assert!(steps.iter().all(|s| s.outcome == OutcomeKind::Succeeded));
    assert!(effects.is_empty(), "只读工具调用可写槽位不应记录任何 ε");

    // 验证 ResourceTable 实际压入的是 read-dir 视图：misuse(borrow<read-dir>) 尝试按 write-dir 解释句柄必陷入。
    let (misuse_res, misuse_steps, misuse_effects) = run_with_dispatcher(
        &rt,
        &dispatcher,
        "rw.misuse()",
        Instant::now() + Duration::from_secs(5),
        Arc::new(AtomicBool::new(false)),
    );
    match misuse_res {
        Err(ActError::Script(diag)) => {
            assert!(
                diag.message.contains("陷入"),
                "可写槽位收窄为 read-dir 后，misuse 应因资源类型不匹配陷入，实际：{}",
                diag.message
            );
        }
        other => panic!("misuse 应因收窄而陷入，实际：{other:?}"),
    }
    assert_eq!(
        misuse_steps,
        vec![StepSummary {
            tool: "probe.misuse".to_owned(),
            outcome: OutcomeKind::Failed,
        }]
    );
    assert!(misuse_effects.is_empty());
}

#[test]
fn write_receipt_survives_tool_failure() {
    let root = tempfile::Builder::new()
        .prefix("ln-disp-fail-")
        .tempdir_in("/tmp")
        .expect("在 /tmp 创建临时目录失败");
    let host = root.path().join("host");
    let state = root.path().join("state");
    std::fs::create_dir(&host).expect("创建宿主目录失败");

    let shadow = ShadowDir::open(&state, "rw", &host, &[], &[]).expect("建立影子失败");
    let rt = ActRuntime::load().expect("加载 act 运行时失败");
    let mut artifacts = embedded_artifacts();
    let probe_decls: Vec<ToolDecl> =
        serde_json::from_str(PROBE_DECLS_JSON).expect("解析 tool-probe.decl.json 失败");
    artifacts.push(ComponentArtifact {
        name: "tool-probe",
        decls: probe_decls,
        cwasm: PROBE_CWASM,
    });
    // SAFETY: 预编译产物均由同一构建与引擎配置生成。
    let dispatcher = unsafe {
        Dispatcher::new(
            rt.engine(),
            &artifacts,
            vec![DirSlot::Shadow(shadow.clone())],
        )
    }
    .expect("构造分发器失败");

    let script =
        "rw.write_then_fail(\"probe.txt\", \"landed\")\nrw.write(\"after.txt\", \"nope\")\n";
    let (res, steps, effects) = run_with_dispatcher(
        &rt,
        &dispatcher,
        script,
        Instant::now() + Duration::from_secs(5),
        Arc::new(AtomicBool::new(false)),
    );
    match res {
        Err(ActError::Script(diag)) => {
            assert_eq!(diag.phase, Phase::Eval);
            assert_eq!(diag.line, Some(1));
            assert!(diag.message.contains("故意报错"), "实际：{}", diag.message);
        }
        other => panic!("write_then_fail 应返回 Script 报错，实际：{other:?}"),
    }
    assert_eq!(
        steps,
        vec![StepSummary {
            tool: "probe.write_then_fail".to_owned(),
            outcome: OutcomeKind::Failed,
        }],
        "脚本应在失败处中止，不得执行第二行"
    );
    assert_eq!(effects.len(), 1, "已发生的写入必须留在回执 ε 中");
    assert_eq!(effects[0].handle(), "rw");
    assert_eq!(effects[0].kind(), EffectKind::Write);
    assert_eq!(effects[0].changes().len(), 1);
    assert_eq!(effects[0].changes()[0].target, "probe.txt");
    assert_eq!(effects[0].changes()[0].action, "write");
    assert_eq!(effects[0].changes()[0].residue, Residue::None);

    assert_eq!(
        shadow.read("probe.txt").expect("读取影子失败").as_deref(),
        Some("landed")
    );
    assert_eq!(shadow.read("after.txt").expect("读取影子失败"), None);
    assert!(!host.join("probe.txt").exists(), "宿主工作目录不应被写入");
}

#[test]
fn effect_outside_template_fails_act() {
    let root = tempfile::Builder::new()
        .prefix("ln-disp-tpl-")
        .tempdir_in("/tmp")
        .expect("在 /tmp 创建临时目录失败");
    let host1 = root.path().join("host1");
    let host2 = root.path().join("host2");
    let state = root.path().join("state");
    std::fs::create_dir(&host1).expect("创建 host1 失败");
    std::fs::create_dir(&host2).expect("创建 host2 失败");

    let w1 = ShadowDir::open(&state, "w1", &host1, &[], &[]).expect("建立 w1 影子失败");
    let w2 = ShadowDir::open(&state, "w2", &host2, &[], &[]).expect("建立 w2 影子失败");
    let rt = ActRuntime::load().expect("加载 act 运行时失败");

    // 场景 1：真实 fs-write 组件，在声明中删去允许的 Write 模板项。
    {
        let mut artifacts = embedded_artifacts();
        let fs_write = artifacts
            .iter_mut()
            .find(|a| a.name == "fs-write")
            .expect("应包含 fs-write 组件");
        fs_write.decls[0].templates.effects.clear();
        // SAFETY: 预编译产物均由同一构建与引擎配置生成。
        let dispatcher =
            unsafe { Dispatcher::new(rt.engine(), &artifacts, vec![DirSlot::Shadow(w1.clone())]) }
                .expect("构造分发器失败");

        let (res, steps, effects) = run_with_dispatcher(
            &rt,
            &dispatcher,
            "w1.write(\"a.txt\", \"draft\")\nw1.write(\"b.txt\", \"never\")\n",
            Instant::now() + Duration::from_secs(5),
            Arc::new(AtomicBool::new(false)),
        );
        match res {
            Err(ActError::Component(err)) => {
                let msg = format!("{err:#}");
                assert!(
                    msg.contains("effect 越出模板"),
                    "越出模板应以 InternalError 风格组件陷入并写明原因，实际：{msg}"
                );
            }
            other => panic!("越出模板应返回 ActError::Component，实际：{other:?}"),
        }
        assert_eq!(
            steps,
            vec![StepSummary {
                tool: "fs.write".to_owned(),
                outcome: OutcomeKind::Failed,
            }],
            "越出模板后步骤应记 Failed，且后续语句不得执行"
        );
        assert_eq!(effects.len(), 1);
        assert_eq!(effects[0].handle(), "w1");
        assert_eq!(effects[0].changes()[0].target, "a.txt");
        assert_eq!(w1.read("a.txt").unwrap().as_deref(), Some("draft"));
        assert_eq!(w1.read("b.txt").unwrap(), None);
    }

    // 场景 2：真实 tool-probe 的 write-first(dst, other, ...)，模板改为只允许另一个句柄 other。
    {
        let mut artifacts = embedded_artifacts();
        let mut probe_decls: Vec<ToolDecl> =
            serde_json::from_str(PROBE_DECLS_JSON).expect("解析 tool-probe.decl.json 失败");
        let wf = probe_decls
            .iter_mut()
            .find(|d| d.script_name == "probe.write_first")
            .expect("tool-probe 应包含 probe.write_first");
        wf.templates.effects = vec![EffectItem::Effect(1, lancet_tool_decl::EffectKind::Write)];
        artifacts.push(ComponentArtifact {
            name: "tool-probe",
            decls: probe_decls,
            cwasm: PROBE_CWASM,
        });
        // SAFETY: 预编译产物均由同一构建与引擎配置生成。
        let dispatcher = unsafe {
            Dispatcher::new(
                rt.engine(),
                &artifacts,
                vec![DirSlot::Shadow(w1.clone()), DirSlot::Shadow(w2.clone())],
            )
        }
        .expect("构造双可写目录分发器失败");

        let (res, steps, effects) = run_with_dispatcher(
            &rt,
            &dispatcher,
            "w1.write_first(w2, \"x.txt\", \"cross\")\nw2.write(\"after.txt\", \"never\")\n",
            Instant::now() + Duration::from_secs(5),
            Arc::new(AtomicBool::new(false)),
        );
        match res {
            Err(ActError::Component(err)) => {
                let msg = format!("{err:#}");
                assert!(
                    msg.contains("effect 越出模板"),
                    "写入未授权句柄应报 effect 越出模板，实际：{msg}"
                );
            }
            other => panic!("越出模板应返回 ActError::Component，实际：{other:?}"),
        }
        assert_eq!(
            steps,
            vec![StepSummary {
                tool: "probe.write_first".to_owned(),
                outcome: OutcomeKind::Failed,
            }]
        );
        assert_eq!(effects.len(), 1);
        assert_eq!(effects[0].handle(), "w1");
        assert_eq!(effects[0].changes()[0].target, "x.txt");
        assert_eq!(w1.read("x.txt").unwrap().as_deref(), Some("cross"));
        assert_eq!(w2.read("after.txt").unwrap(), None);
    }
}

#[test]
fn discard_partial_effects_survive_failure_in_receipt() {
    use rustix::fs::{CWD, FileType, Mode, mknodat};
    let root = tempfile::Builder::new()
        .prefix("ln-disp-disc-")
        .tempdir_in("/tmp")
        .expect("在 /tmp 创建临时目录失败");
    let host = root.path().join("host");
    let state = root.path().join("state");
    std::fs::create_dir(&host).expect("创建 host 失败");
    std::fs::write(host.join("a"), "base-a").unwrap();
    std::fs::write(host.join("z"), "base-z").unwrap();

    let shadow = ShadowDir::open(&state, "w", &host, &[], &[]).expect("建立影子失败");
    // a 换型为空目录，z 换型为内含未跟踪 FIFO 的目录，令 discard 删除 a 后在 z 报错。
    std::fs::remove_file(shadow.tree_path().join("a")).unwrap();
    std::fs::create_dir(shadow.tree_path().join("a")).unwrap();
    std::fs::remove_file(shadow.tree_path().join("z")).unwrap();
    std::fs::create_dir(shadow.tree_path().join("z")).unwrap();
    mknodat(
        CWD,
        shadow.tree_path().join("z/fifo"),
        FileType::Fifo,
        Mode::from_raw_mode(0o600),
        0,
    )
    .unwrap();

    let rt = ActRuntime::load().expect("加载 act 运行时失败");
    let artifacts = embedded_artifacts();
    // SAFETY: 预编译产物均由同一构建与引擎配置生成。
    let dispatcher =
        unsafe { Dispatcher::new(rt.engine(), &artifacts, vec![DirSlot::Shadow(shadow)]) }
            .expect("构造分发器失败");

    let (res, steps, effects) = run_with_dispatcher(
        &rt,
        &dispatcher,
        "discard()\nreturn 1\n",
        Instant::now() + Duration::from_secs(5),
        Arc::new(AtomicBool::new(false)),
    );
    assert!(
        matches!(res, Err(ActError::Script(_))),
        "discard 部分失败应报脚本错误，实际：{res:?}"
    );
    assert_eq!(
        steps,
        vec![StepSummary {
            tool: "discard".to_owned(),
            outcome: OutcomeKind::Failed,
        }]
    );
    assert_eq!(effects.len(), 1, "已落地的部分 discard 变更应写入回执 ε");
    assert_eq!(effects[0].handle(), "w");
    assert_eq!(effects[0].kind(), EffectKind::Write);
    assert_eq!(effects[0].changes().len(), 1);
    assert_eq!(effects[0].changes()[0].target, "a");
    assert_eq!(effects[0].changes()[0].action, "discard");
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
    let effects = Mutex::new(Vec::new());
    let cancelled = Arc::new(AtomicBool::new(false));
    for (label, tool_id, args) in cases {
        let deadline = Instant::now() + Duration::from_secs(30);
        for _ in 0..10 {
            let res = dispatcher
                .call(
                    tool_id,
                    &args,
                    deadline,
                    Arc::clone(&cancelled),
                    &steps,
                    &effects,
                )
                .unwrap_or_else(|e| panic!("预热工具 `{label}` 失败：{e:#}"));
            res.unwrap_or_else(|e| panic!("预热工具 `{label}` 返回错误：{e}"));
        }

        let mut samples = Vec::with_capacity(1_000);
        for _ in 0..1_000 {
            steps.lock().expect("步骤互斥锁不应中毒").clear();
            effects.lock().expect("effect 互斥锁不应中毒").clear();
            let t0 = Instant::now();
            let res = dispatcher
                .call(
                    tool_id,
                    &args,
                    deadline,
                    Arc::clone(&cancelled),
                    &steps,
                    &effects,
                )
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
