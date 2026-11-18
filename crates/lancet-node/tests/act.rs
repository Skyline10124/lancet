// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use lancet_node::act::{ActError, ActRuntime, Diagnostic, Payload, Phase};

const OOM_SCRIPT: &str = r#"
chunk = "x" * 1048576
chunks = []
for i in range(200):
    chunks.append(chunk + str(i))
return len(chunks)
"#;

const BUSY_SCRIPT: &str = r#"
acc = 0
for i in range(100000):
    for j in range(100000):
        acc = (acc + i + j) % 97
return acc
"#;

fn run_ok(rt: &ActRuntime, script: &str) -> Payload {
    rt.run(
        script,
        Instant::now() + Duration::from_secs(5),
        Arc::new(AtomicBool::new(false)),
    )
    .unwrap_or_else(|err| panic!("脚本预期成功，却失败：{err:?}\n脚本：\n{script}"))
}

fn run_script_err(rt: &ActRuntime, script: &str) -> Diagnostic {
    match rt.run(
        script,
        Instant::now() + Duration::from_secs(5),
        Arc::new(AtomicBool::new(false)),
    ) {
        Err(ActError::Script(diag)) => diag,
        other => panic!("脚本预期返回 Script 诊断，实际为：{other:?}\n脚本：\n{script}"),
    }
}

#[test]
fn top_level_return_and_print_are_per_call() {
    let rt = ActRuntime::load().expect("加载 act 运行时失败");

    // 顶层分支中的 return。
    let branched = run_ok(&rt, "if True:\n    return 42\nreturn 0\n");
    assert_eq!(branched.value_json, "42");
    assert_eq!(branched.printed, "");

    // 不写 return、空脚本与只有注释的脚本都返回 null。
    for script in ["x = 1 + 2\n", "", "# 仅注释\n   # 第二行注释\n"] {
        let payload = run_ok(&rt, script);
        assert_eq!(payload.value_json, "null", "脚本：{script:?}");
        assert_eq!(payload.printed, "", "脚本：{script:?}");
    }

    // 多行字符串内部的行不得被包裹缩进改变。
    let multiline = "s = \"\"\"第一行\n第二行\n    第三行\"\"\"\nreturn s\n";
    let multiline_payload = run_ok(&rt, multiline);
    assert_eq!(
        multiline_payload.value_json,
        "\"第一行\\n第二行\\n    第三行\""
    );

    // 两次 print 中文文本，printed 为两行；下一次调用 printed 为空。
    let printed_payload = run_ok(
        &rt,
        "print(\"第一行中文\")\nprint(\"第二行\", \"补充\")\nreturn 1\n",
    );
    assert_eq!(printed_payload.value_json, "1");
    assert_eq!(printed_payload.printed, "第一行中文\n第二行 补充\n");

    let next_payload = run_ok(&rt, "return 7\n");
    assert_eq!(next_payload.value_json, "7");
    assert_eq!(
        next_payload.printed, "",
        "上一次调用的 print 缓冲不得泄漏到下一次调用"
    );
}

#[test]
fn check_finishes_before_first_statement() {
    let rt = ActRuntime::load().expect("加载 act 运行时失败");

    // 对照：去掉静态错误后，首句的 fail("执行哨兵") 在 eval 阶段触发。
    let sentinel = run_script_err(&rt, "fail(\"执行哨兵\")\n");
    assert_eq!(sentinel.phase, Phase::Eval);
    assert_eq!(sentinel.line, Some(1));
    assert!(
        sentinel.message.contains("执行哨兵"),
        "对照应触发执行哨兵，实际：{}",
        sentinel.message
    );

    let cases = [
        ("未定义名称", "fail(\"执行哨兵\")\nx = missing_ident\n", 2),
        (
            "已知函数错误参数名",
            "fail(\"执行哨兵\")\nx = sorted([2, 1], revers = True)\n",
            2,
        ),
        ("错误参数类型", "fail(\"执行哨兵\")\nx = sorted(123)\n", 2),
        (
            "未知成员属性",
            "fail(\"执行哨兵\")\nx = struct(found = 1).foud\n",
            2,
        ),
    ];

    for (name, script, expected_line) in cases {
        let diag = run_script_err(&rt, script);
        assert_eq!(
            diag.phase,
            Phase::Check,
            "用例 `{name}` 应在 check 阶段拦截，实际诊断：{diag:?}"
        );
        assert_eq!(
            diag.line,
            Some(expected_line),
            "用例 `{name}` 行号应指向第 {expected_line} 行，实际诊断：{diag:?}"
        );
        assert!(
            !diag.message.contains("执行哨兵"),
            "用例 `{name}` 在 check 阶段失败前不得执行首句：{diag:?}"
        );
    }
}

#[test]
fn parse_errors_map_lines_and_offer_repairs() {
    let rt = ActRuntime::load().expect("加载 act 运行时失败");

    let repair_cases = [
        (
            "无括号生成器",
            "x = 1\ny = any(i > 0 for i in [1, 2])\n",
            2,
            "[x for x in xs]",
        ),
        (
            "import 语句",
            "x = 1\nimport json\n",
            2,
            "直接使用已提供的全局名称",
        ),
        (
            "is None",
            "x = None\nif x is None:\n    return 1\n",
            2,
            "== None",
        ),
        (
            "花括号集合推导",
            "x = 1\ns = {i for i in [1, 2]}\n",
            2,
            "set([x for x in xs])",
        ),
    ];

    for (name, script, expected_line, expected_hint) in repair_cases {
        let diag = run_script_err(&rt, script);
        assert_eq!(
            diag.phase,
            Phase::Parse,
            "用例 `{name}` 应为 parse 阶段，实际：{diag:?}"
        );
        assert_eq!(
            diag.line,
            Some(expected_line),
            "用例 `{name}` 行号错误，实际：{diag:?}"
        );
        assert!(
            diag.message.contains(expected_hint),
            "用例 `{name}` 诊断应包含提示 `{expected_hint}`，实际：\n{}",
            diag.message
        );
    }

    // 禁用 load 与 f-string（含未配对花括号，验证词法器遇到错误即停止）。
    for (name, script) in [
        ("load 语句", "load(\"a.star\", \"x\")\n"),
        ("f-string", "name = \"world\"\ns = f\"hello {name}\"\n"),
        ("未配对花括号 f-string", "s = f\"bad } brace\"\n"),
    ] {
        let diag = run_script_err(&rt, script);
        assert_eq!(diag.phase, Phase::Parse, "{name} 应报 parse 错误：{diag:?}");
    }

    // 末行续行不得把后续调用吞并进表达式而静默返回；行号与消息位置须一致。
    for (name, script, expected_line) in [
        ("末行续行", "print(\"ran\")\nx = 1 + \\\n", 2),
        ("无换行末行续行", "print(\"ran\")\nx = 1 + \\", 2),
        ("两行未闭合列表以换行结束", "x = [\n    1,\n", 2),
    ] {
        let diag = run_script_err(&rt, script);
        assert_eq!(diag.phase, Phase::Parse, "{name} 应报 parse 错误：{diag:?}");
        assert_eq!(
            diag.line,
            Some(expected_line),
            "{name} 行号应为 {expected_line}：{diag:?}"
        );
        assert!(
            diag.message.contains(&format!("act.star:{expected_line}:")),
            "{name} 的诊断消息位置应与 line={expected_line} 一致：\n{}",
            diag.message
        );
    }

    // 字符串或注释在同一出错行里出现 import 时，无关解析错误不得附带修复提示。
    let unrelated = run_script_err(&rt, "return \"import\" + # import\n");
    assert_eq!(unrelated.phase, Phase::Parse);
    assert!(
        !unrelated.message.contains("直接使用已提供的全局名称"),
        "字符串与注释中的 import 不应触发修复提示：\n{}",
        unrelated.message
    );
}

#[test]
fn pure_globals_filter_and_serialize() {
    let rt = ActRuntime::load().expect("加载 act 运行时失败");

    // 反思与元编程全局函数必须从环境中移除，且在 check 阶段报错。
    for (name, script) in [
        ("dir", "fail(\"执行哨兵\")\nx = dir(1)\n"),
        ("getattr", "fail(\"执行哨兵\")\nx = getattr({}, \"get\")\n"),
        ("hasattr", "fail(\"执行哨兵\")\nx = hasattr({}, \"get\")\n"),
        ("type", "fail(\"执行哨兵\")\nx = type(1)\n"),
        (
            "sum 错误元素类型",
            "fail(\"执行哨兵\")\nx = sum([\"a\", \"b\"])\n",
        ),
    ] {
        let diag = run_script_err(&rt, script);
        assert_eq!(
            diag.phase,
            Phase::Check,
            "`{name}` 应在 check 阶段被拦截，实际：{diag:?}"
        );
        assert!(!diag.message.contains("执行哨兵"));
    }

    // 扩展全局函数、sum、regex 与 text 组合求值与 JSON 序列化。
    let script = r##"
s = set([3, 1, 2, 1])
decoded = json.decode(json.encode({"k": [1, True, None]}))
st = struct(a = 1, b = "x")
raw = "l0\r\nl1\r\nl2 hit\r\nl3\r\nl4 hit\r\nl5\r\nl6"
tv = text(raw)
overlapped = tv.grep(r"hit$", before = 1, after = 1)
eol_match = text("one\r\ntwo\r\nthree\r\n").grep(r"^two$")
no_match = tv.grep(r"missing")
return {
    "set_len": len(s),
    "decoded": decoded,
    "mapped": list(map(lambda x: x * 2, [1, 2, 3])),
    "filtered": list(filter(lambda x: x > 1, [1, 2, 3])),
    "struct": st,
    "sum_empty": sum([]),
    "sum_ints": sum([1, 2, 3]),
    "sum_mixed": sum([1, 2.5]),
    "rx_match_yes": regex.match("foo", "foobar"),
    "rx_match_no": regex.match("bar", "foobar"),
    "rx_match_multiline": regex.match("(?m)^bar", "foo\nbar"),
    "rx_findall": regex.findall("[0-9]+", "a1 b22 c333"),
    "rx_replace": regex.replace("[0-9]+", "#", "a1 b22"),
    "overlapped": overlapped,
    "eol_match": eol_match,
    "no_match": no_match,
    "head": tv.head(2),
    "tail": tv.tail(2),
    "head0": tv.head(0),
    "tail_all": tv.tail(99),
    "lines": text("a\r\nb\nc\rd").splitlines(),
    "lines_keep": text("a\r\nb\nc\rd").splitlines(True),
    "cr_lf_head1": text("a\rskip\n\n").grep("^(a|)$").head(1),
    "cr_lf_lines": text("a\rskip\n\n").grep("^(a|)$").splitlines(),
}
"##;
    let payload = run_ok(&rt, script);
    let actual: serde_json::Value =
        serde_json::from_str(&payload.value_json).expect("返回载荷应为合法 JSON");
    assert_eq!(actual["set_len"], 3);
    assert_eq!(actual["decoded"], serde_json::json!({"k": [1, true, null]}));
    assert_eq!(actual["mapped"], serde_json::json!([2, 4, 6]));
    assert_eq!(actual["filtered"], serde_json::json!([2, 3]));
    assert_eq!(actual["struct"], serde_json::json!({"a": 1, "b": "x"}));
    assert_eq!(actual["sum_empty"], 0);
    assert_eq!(actual["sum_ints"], 6);
    assert_eq!(actual["sum_mixed"], 3.5);
    assert_eq!(actual["rx_match_yes"], true);
    assert_eq!(actual["rx_match_no"], false);
    assert_eq!(actual["rx_match_multiline"], false);
    assert_eq!(actual["rx_findall"], serde_json::json!(["1", "22", "333"]));
    assert_eq!(actual["rx_replace"], "a# b#");
    assert_eq!(
        actual["overlapped"],
        "l1\r\nl2 hit\r\nl3\r\nl4 hit\r\nl5\r\n"
    );
    assert_eq!(actual["eol_match"], "two\r\n");
    assert_eq!(actual["no_match"], "");
    assert_eq!(actual["head"], "l0\r\nl1\r\n");
    assert_eq!(actual["tail"], "l5\r\nl6");
    assert_eq!(actual["head0"], "");
    assert_eq!(
        actual["tail_all"],
        "l0\r\nl1\r\nl2 hit\r\nl3\r\nl4 hit\r\nl5\r\nl6"
    );
    assert_eq!(actual["lines"], serde_json::json!(["a", "b", "c", "d"]));
    assert_eq!(
        actual["lines_keep"],
        serde_json::json!(["a\r\n", "b\n", "c\r", "d"])
    );
    assert_eq!(actual["cr_lf_head1"], "a\r");
    assert_eq!(actual["cr_lf_lines"], serde_json::json!(["a", ""]));

    // 顶层直接返回 TextView 时也序列化为 JSON 字符串。
    let top_tv = run_ok(&rt, "return text(\"a\\nb\\n\").grep(\"b\")\n");
    assert_eq!(top_tv.value_json, "\"b\\n\"");
}

#[test]
fn evaluation_errors_keep_the_script_location() {
    let rt = ActRuntime::load().expect("加载 act 运行时失败");

    let div_zero = run_script_err(&rt, "x = 1\ny = 1 // 0\n");
    assert_eq!(div_zero.phase, Phase::Eval);
    assert_eq!(div_zero.line, Some(2));
    assert!(
        !div_zero.message.contains("lancet_act_entry"),
        "诊断不得暴露合成入口函数：\n{}",
        div_zero.message
    );

    let in_helper = run_script_err(&rt, "def helper():\n    return 1 // 0\nx = helper()\n");
    assert_eq!(in_helper.phase, Phase::Eval);
    assert_eq!(in_helper.line, Some(2));
    assert!(
        in_helper.message.contains("helper"),
        "用户辅助函数栈帧应保留：\n{}",
        in_helper.message
    );
    assert!(
        in_helper.message.contains("act.star:3"),
        "调用点应映射回第 3 行：\n{}",
        in_helper.message
    );
    assert!(!in_helper.message.contains("lancet_act_entry"));

    let attr_missing = run_script_err(
        &rt,
        "def f(x):\n    return x.foud\nreturn f(struct(found = 1))\n",
    );
    assert_eq!(attr_missing.phase, Phase::Eval);
    assert_eq!(attr_missing.line, Some(2));
    assert!(
        attr_missing.message.contains("act.star:3"),
        "动态属性失败调用点应映射回第 3 行：\n{}",
        attr_missing.message
    );

    let bad_regex = run_script_err(&rt, "x = 1\ny = regex.match(\"[\", \"abc\")\n");
    assert_eq!(bad_regex.phase, Phase::Eval);
    assert_eq!(bad_regex.line, Some(2));

    let bad_grep = run_script_err(&rt, "t = text(\"a\")\ny = t.grep(\"[\")\n");
    assert_eq!(bad_grep.phase, Phase::Eval);
    assert_eq!(bad_grep.line, Some(2));

    let unserializable = run_script_err(&rt, "def f():\n    pass\nreturn f\n");
    assert_eq!(unserializable.phase, Phase::Eval);
    assert_eq!(unserializable.line, None);
}

#[test]
fn memory_limit_terminates_only_its_store() {
    let rt = Arc::new(ActRuntime::load().expect("加载 act 运行时失败"));

    let (oom_res, concurrent_res) = std::thread::scope(|scope| {
        let rt_oom = Arc::clone(&rt);
        let handle_oom = scope.spawn(move || {
            rt_oom.run(
                OOM_SCRIPT,
                Instant::now() + Duration::from_secs(10),
                Arc::new(AtomicBool::new(false)),
            )
        });

        let rt_ok = Arc::clone(&rt);
        let handle_ok = scope.spawn(move || {
            rt_ok.run(
                "return sum(range(100))\n",
                Instant::now() + Duration::from_secs(10),
                Arc::new(AtomicBool::new(false)),
            )
        });

        let oom_res = handle_oom.join().expect("等待 OOM Store 线程失败");
        let concurrent_res = handle_ok.join().expect("等待正常 Store 线程失败");
        (oom_res, concurrent_res)
    });

    assert!(
        matches!(oom_res, Err(ActError::MemoryLimit)),
        "超出线性内存上限应返回 ActError::MemoryLimit，实际：{oom_res:?}"
    );
    let concurrent_payload = concurrent_res.expect("并发独立 Store 应不受 OOM Store 影响");
    assert_eq!(concurrent_payload.value_json, "4950");

    // 内存超限后，后续新 Store 仍可正常执行。
    let healthy = run_ok(&rt, "return sum(range(10))\n");
    assert_eq!(healthy.value_json, "45");
}

#[test]
fn deadline_interrupts_busy_store() {
    let rt = ActRuntime::load().expect("加载 act 运行时失败");

    let started = Instant::now();
    let res = rt.run(
        BUSY_SCRIPT,
        started + Duration::from_millis(50),
        Arc::new(AtomicBool::new(false)),
    );
    let elapsed = started.elapsed();

    assert!(
        matches!(res, Err(ActError::Deadline)),
        "超时忙循环应返回 ActError::Deadline，实际：{res:?}"
    );
    assert!(
        elapsed < Duration::from_secs(3),
        "超时中断耗时过长：{elapsed:?}"
    );

    let healthy = run_ok(&rt, "return 42\n");
    assert_eq!(healthy.value_json, "42");
}

#[test]
fn cancel_interrupts_only_selected_store() {
    let rt = Arc::new(ActRuntime::load().expect("加载 act 运行时失败"));
    let finite_script = r#"
acc = 0
for i in range(4000):
    for j in range(1000):
        acc = (acc + i * 3 + j) % 97
return acc
"#;
    let mut expected_acc = 0i64;
    for i in 0..4000i64 {
        for j in 0..1000i64 {
            expected_acc = (expected_acc + i * 3 + j) % 97;
        }
    }

    let cancel_a = Arc::new(AtomicBool::new(false));
    let cancel_b = Arc::new(AtomicBool::new(false));

    let ((res_a, elapsed_a, finished_a), (res_b, finished_b)) = std::thread::scope(|scope| {
        let rt_a = Arc::clone(&rt);
        let flag_a = Arc::clone(&cancel_a);
        let handle_a = scope.spawn(move || {
            let started = Instant::now();
            let res = rt_a.run(BUSY_SCRIPT, started + Duration::from_secs(10), flag_a);
            (res, started.elapsed(), Instant::now())
        });

        let rt_b = Arc::clone(&rt);
        let flag_b = Arc::clone(&cancel_b);
        let handle_b = scope.spawn(move || {
            let res = rt_b.run(
                finite_script,
                Instant::now() + Duration::from_secs(10),
                flag_b,
            );
            (res, Instant::now())
        });

        std::thread::sleep(Duration::from_millis(30));
        cancel_a.store(true, Ordering::Relaxed);

        let a = handle_a.join().expect("等待受控 Store 线程失败");
        let b = handle_b.join().expect("等待独立 Store 线程失败");
        (a, b)
    });

    assert!(
        matches!(res_a, Err(ActError::Cancelled)),
        "被取消的 Store 应返回 ActError::Cancelled，实际：{res_a:?}"
    );
    assert!(
        elapsed_a < Duration::from_secs(3),
        "取消中断耗时过长：{elapsed_a:?}"
    );
    assert!(
        finished_b > finished_a,
        "独立 Store B 应在 A 收到 Cancelled 之后才结束（A={finished_a:?}, B={finished_b:?}）"
    );
    let payload_b = res_b.expect("未被取消的并发 Store 应正常完成");
    assert_eq!(payload_b.value_json, expected_acc.to_string());
}

fn percentile_ms(samples: &[Duration], numer: usize, denom: usize) -> f64 {
    let mut sorted: Vec<Duration> = samples.to_vec();
    sorted.sort_unstable();
    let idx = (sorted.len().saturating_sub(1) * numer) / denom;
    sorted[idx].as_secs_f64() * 1_000.0
}

#[test]
#[ignore = "单线程实测每次调用开销与中断延迟；手动运行"]
fn measure_component_runtime() {
    let rt = ActRuntime::load().expect("加载 act 运行时失败");

    // 1. 每次调用开销（新建 Store、实例化并运行 `return 1`）：10 次预热 + 100 次串行实测。
    for _ in 0..10 {
        let payload = run_ok(&rt, "return 1");
        assert_eq!(payload.value_json, "1");
    }
    let mut call_samples = Vec::with_capacity(100);
    for _ in 0..100 {
        let t0 = Instant::now();
        let payload = rt
            .run(
                "return 1",
                Instant::now() + Duration::from_secs(5),
                Arc::new(AtomicBool::new(false)),
            )
            .expect("测量调用失败");
        call_samples.push(t0.elapsed());
        assert_eq!(payload.value_json, "1");
    }

    // 2. Deadline 超出 50 ms 预算的中断延迟（30 次样本）。
    let mut deadline_samples = Vec::with_capacity(30);
    for _ in 0..30 {
        let deadline = Instant::now() + Duration::from_millis(50);
        let res = rt.run(BUSY_SCRIPT, deadline, Arc::new(AtomicBool::new(false)));
        let finished = Instant::now();
        assert!(matches!(res, Err(ActError::Deadline)));
        deadline_samples.push(finished.saturating_duration_since(deadline));
    }

    // 3. Cancel 从置位到返回的中断延迟（30 次样本）。
    let mut cancel_samples = Vec::with_capacity(30);
    for _ in 0..30 {
        let cancelled = Arc::new(AtomicBool::new(false));
        let cancelled_at = Arc::new(OnceLock::new());
        let flag = Arc::clone(&cancelled);
        let stamp = Arc::clone(&cancelled_at);
        let finished = std::thread::scope(|scope| {
            scope.spawn(move || {
                std::thread::sleep(Duration::from_millis(20));
                let _ = stamp.set(Instant::now());
                flag.store(true, Ordering::Release);
            });
            let res = rt.run(
                BUSY_SCRIPT,
                Instant::now() + Duration::from_secs(10),
                Arc::clone(&cancelled),
            );
            let done = Instant::now();
            assert!(matches!(res, Err(ActError::Cancelled)));
            done
        });
        let sent = *cancelled_at.get().expect("取消时间戳应已记录");
        cancel_samples.push(finished.saturating_duration_since(sent));
    }

    println!(
        "per_call_overhead (new Store + instantiate + `return 1`, 10 warmup + 100 runs): p50={:.3}ms p95={:.3}ms max={:.3}ms",
        percentile_ms(&call_samples, 50, 100),
        percentile_ms(&call_samples, 95, 100),
        call_samples.iter().max().expect("非空样本").as_secs_f64() * 1_000.0
    );
    println!(
        "deadline_latency (overshoot past 50ms budget, 30 runs): p50={:.3}ms p95={:.3}ms max={:.3}ms",
        percentile_ms(&deadline_samples, 50, 100),
        percentile_ms(&deadline_samples, 95, 100),
        deadline_samples
            .iter()
            .max()
            .expect("非空样本")
            .as_secs_f64()
            * 1_000.0
    );
    println!(
        "cancel_latency (30 runs): p50={:.3}ms p95={:.3}ms max={:.3}ms",
        percentile_ms(&cancel_samples, 50, 100),
        percentile_ms(&cancel_samples, 95, 100),
        cancel_samples.iter().max().expect("非空样本").as_secs_f64() * 1_000.0
    );
}
