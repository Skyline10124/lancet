use anyhow::{Context, bail};
use serde_json::{Value as Json, json};
use starlark::environment::GlobalsBuilder;
use starlark::eval::Evaluator;
use starlark::values::list::UnpackList;
use starlark::values::none::NoneType;
use starlark::values::structs::AllocStruct;
use starlark::values::{Heap, ProvidesStaticType, Value};
use starlark_derive::starlark_module;
use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};

#[derive(ProvidesStaticType)]
pub struct State {
    pub root: PathBuf,
    pub baseline: BTreeMap<String, Vec<u8>>,
    pub canned: Json,
    pub discarded: Cell<bool>,
    pub result_set: Cell<bool>,
    pub events: RefCell<Vec<Json>>,
}
fn state<'a>(eval: &'a Evaluator) -> &'a State {
    eval.extra.unwrap().downcast_ref::<State>().unwrap()
}
impl State {
    fn path(&self, path: &str) -> anyhow::Result<PathBuf> {
        let p = Path::new(path);
        if p.is_absolute() || p.components().any(|c| matches!(c, Component::ParentDir)) {
            bail!("路径必须位于仓库内: {path}");
        }
        Ok(self.root.join(p))
    }
    fn record(&self, op: &str, args: Json) {
        self.events
            .borrow_mut()
            .push(json!({"op": op, "args": args}));
    }
    pub fn changes(&self) -> anyhow::Result<Vec<(String, String)>> {
        let now = snapshot(&self.root)?;
        let keys: BTreeSet<_> = self.baseline.keys().chain(now.keys()).collect();
        Ok(keys
            .into_iter()
            .filter_map(|key| {
                let before = self.baseline.get(key);
                let after = now.get(key);
                if before == after {
                    None
                } else {
                    Some((
                        key.clone(),
                        if before.is_none() {
                            "added"
                        } else if after.is_none() {
                            "deleted"
                        } else {
                            "modified"
                        }
                        .to_owned(),
                    ))
                }
            })
            .collect())
    }
}
pub fn snapshot(root: &Path) -> anyhow::Result<BTreeMap<String, Vec<u8>>> {
    fn visit(root: &Path, dir: &Path, files: &mut BTreeMap<String, Vec<u8>>) -> anyhow::Result<()> {
        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            let ty = entry.file_type()?;
            if ty.is_dir() {
                visit(root, &entry.path(), files)?;
            } else if ty.is_file() {
                files.insert(
                    entry
                        .path()
                        .strip_prefix(root)?
                        .to_string_lossy()
                        .into_owned(),
                    fs::read(entry.path())?,
                );
            } else {
                bail!("原型不接受符号链接或特殊文件");
            }
        }
        Ok(())
    }
    let mut files = BTreeMap::new();
    visit(root, root, &mut files)?;
    Ok(files)
}
pub fn restore(root: &Path, files: &BTreeMap<String, Vec<u8>>) -> anyhow::Result<()> {
    for path in snapshot(root)?.keys() {
        if !files.contains_key(path) {
            fs::remove_file(root.join(path))?;
        }
    }
    for (path, content) in files {
        let path = root.join(path);
        fs::create_dir_all(path.parent().unwrap())?;
        fs::write(path, content)?;
    }
    Ok(())
}
#[starlark_module]
pub fn repo(builder: &mut GlobalsBuilder) {
    fn read<'v>(
        path: &str,
        #[starlark(default = false)] missing_ok: bool,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<Value<'v>> {
        let s = state(eval);
        s.record("read", json!({"path":path}));
        match fs::read_to_string(s.path(path)?) {
            Ok(v) => Ok(eval.heap().alloc(v)),
            Err(e) if missing_ok && e.kind() == std::io::ErrorKind::NotFound => {
                Ok(Value::new_none())
            }
            Err(e) => Err(e).with_context(|| format!("读取文件 {path}")),
        }
    }
    fn list(
        #[starlark(default = ".")] path: &str,
        eval: &mut Evaluator,
    ) -> anyhow::Result<Vec<String>> {
        let mut result = Vec::new();
        for e in fs::read_dir(state(eval).path(path)?)? {
            let e = e?;
            result.push(format!(
                "{}{}",
                e.file_name().to_string_lossy(),
                if e.file_type()?.is_dir() { "/" } else { "" }
            ));
        }
        result.sort();
        Ok(result)
    }
    fn search<'v>(
        pattern: &str,
        #[starlark(default = "**")] glob: &str,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<Value<'v>> {
        let s = state(eval);
        s.record("search", json!({"pattern":pattern,"glob":glob}));
        let re = regex::Regex::new(pattern)?;
        let matcher = globset::Glob::new(glob)?.compile_matcher();
        let mut hits = Vec::new();
        for (path, bytes) in snapshot(&s.root)? {
            if !matcher.is_match(&path) {
                continue;
            }
            let Ok(content) = String::from_utf8(bytes) else {
                continue;
            };
            for (i, line) in content.lines().enumerate() {
                if re.is_match(line) {
                    let heap = eval.heap();
                    hits.push(heap.alloc(AllocStruct([
                        ("path", heap.alloc(path.as_str())),
                        ("line", heap.alloc(i + 1)),
                        ("text", heap.alloc(line)),
                    ])));
                }
            }
        }
        Ok(eval.heap().alloc(hits))
    }
    fn write(path: &str, content: &str, eval: &mut Evaluator) -> anyhow::Result<NoneType> {
        let s = state(eval);
        s.record("write", json!({"path":path}));
        let p = s.path(path)?;
        fs::create_dir_all(p.parent().unwrap())?;
        fs::write(p, content)?;
        Ok(NoneType)
    }
    fn edit(
        path: &str,
        old: &str,
        new: &str,
        #[starlark(default = false)] all: bool,
        eval: &mut Evaluator,
    ) -> anyhow::Result<NoneType> {
        let s = state(eval);
        let p = s.path(path)?;
        let content = fs::read_to_string(&p)?;
        let count = content.matches(old).count();
        s.record(
            "edit",
            json!({"path":path,"old":old,"new":new,"all":all,"matches":count}),
        );
        if count == 0 || (!all && count != 1) {
            bail!(
                "EditMatchError: {path}: 匹配 {count} 次，要求{}",
                if all { "至少 1 次" } else { "恰好 1 次" }
            );
        }
        fs::write(p, content.replace(old, new))?;
        Ok(NoneType)
    }
    fn reset(path: &str, eval: &mut Evaluator) -> anyhow::Result<NoneType> {
        let s = state(eval);
        let p = s.path(path)?;
        s.record("reset", json!({"path":path}));
        if let Some(content) = s.baseline.get(path) {
            fs::create_dir_all(p.parent().unwrap())?;
            fs::write(p, content)?;
        } else if p.exists() {
            fs::remove_file(p)?;
        }
        Ok(NoneType)
    }
    fn status<'v>(eval: &mut Evaluator<'v, '_, '_>) -> anyhow::Result<Value<'v>> {
        Ok(eval.heap().alloc(
            state(eval)
                .changes()?
                .into_iter()
                .map(|(path, kind)| {
                    eval.heap()
                        .alloc(AllocStruct([("path", path), ("kind", kind)]))
                })
                .collect::<Vec<_>>(),
        ))
    }
    fn diff(eval: &mut Evaluator) -> anyhow::Result<String> {
        let s = state(eval);
        let now = snapshot(&s.root)?;
        let mut output = String::new();
        for (path, _) in s.changes()? {
            let before = String::from_utf8_lossy(
                s.baseline.get(&path).map(Vec::as_slice).unwrap_or_default(),
            );
            let after =
                String::from_utf8_lossy(now.get(&path).map(Vec::as_slice).unwrap_or_default());
            output.push_str(
                &similar::TextDiff::from_lines(&before, &after)
                    .unified_diff()
                    .header(&format!("a/{path}"), &format!("b/{path}"))
                    .to_string(),
            );
        }
        Ok(output)
    }
}
fn allowed(argv: &[String]) -> anyhow::Result<()> {
    if !argv.first().is_some_and(|x| {
        [
            "git", "grep", "rg", "ls", "cat", "head", "tail", "wc", "find", "sed",
        ]
        .contains(&x.as_str())
    }) {
        bail!("模拟环境未提供该命令: {argv:?}");
    }
    Ok(())
}
fn response<'v>(
    code: i32,
    stdout: String,
    stderr: String,
    check: bool,
    heap: Heap<'v>,
) -> anyhow::Result<Value<'v>> {
    if check && code != 0 {
        bail!("命令退出码 {code}\n{stdout}{stderr}");
    }
    Ok(heap.alloc(AllocStruct([
        ("code", heap.alloc(code)),
        ("stdout", heap.alloc(stdout)),
        ("stderr", heap.alloc(stderr)),
    ])))
}
#[starlark_module]
pub fn env(builder: &mut GlobalsBuilder) {
    fn run<'v>(
        argv: UnpackList<String>,
        #[starlark(default = true)] check: bool,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<Value<'v>> {
        let argv = argv.items;
        let s = state(eval);
        s.record("run", json!({"argv":argv,"check":check,"files":snapshot(&s.root)?.into_iter().map(|(p,c)| (p,String::from_utf8_lossy(&c).into_owned())).collect::<BTreeMap<_,_>>()}));
        if let Some(entries) = s.canned.as_array() {
            for entry in entries {
                if entry["argv"] == json!(argv) {
                    s.record("command_result", entry.clone());
                    return response(
                        entry["code"].as_i64().unwrap() as i32,
                        entry["stdout"].as_str().unwrap().to_owned(),
                        entry["stderr"].as_str().unwrap().to_owned(),
                        check,
                        eval.heap(),
                    );
                }
            }
        }
        allowed(&argv)?;
        let out = Command::new(&argv[0])
            .args(&argv[1..])
            .current_dir(&s.root)
            .stdin(Stdio::null())
            .output()?;
        response(
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
            check,
            eval.heap(),
        )
    }
    fn pipe<'v>(
        argvs: UnpackList<UnpackList<String>>,
        #[starlark(default = true)] check: bool,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<Value<'v>> {
        let argvs: Vec<Vec<String>> = argvs.items.into_iter().map(|x| x.items).collect();
        if argvs.is_empty() {
            bail!("管道不能为空");
        }
        for argv in &argvs {
            allowed(argv)?;
        }
        let s = state(eval);
        s.record("pipe", json!({"argvs":argvs,"check":check}));
        let mut children = Vec::new();
        let mut input = Stdio::null();
        for (i, argv) in argvs.iter().enumerate() {
            // 中间阶段 stderr 落临时文件，避免无人读取的 stderr 管道死锁。
            let errfile = tempfile::tempfile()?;
            let mut child = Command::new(&argv[0])
                .args(&argv[1..])
                .current_dir(&s.root)
                .stdin(input)
                .stdout(Stdio::piped())
                .stderr(if i + 1 == argvs.len() {
                    Stdio::piped()
                } else {
                    Stdio::from(errfile)
                })
                .spawn()?;
            if i + 1 == argvs.len() {
                let out = child.wait_with_output()?;
                for mut previous in children {
                    let _: std::process::ExitStatus = std::process::Child::wait(&mut previous)?;
                }
                return response(
                    out.status.code().unwrap_or(-1),
                    String::from_utf8_lossy(&out.stdout).into_owned(),
                    String::from_utf8_lossy(&out.stderr).into_owned(),
                    check,
                    eval.heap(),
                );
            }
            input = Stdio::from(child.stdout.take().unwrap());
            children.push(child);
        }
        unreachable!()
    }
}
#[starlark_module]
pub fn common(builder: &mut GlobalsBuilder) {
    fn text<'v>(s: &str, heap: Heap<'v>) -> anyhow::Result<Value<'v>> {
        Ok(heap.alloc(crate::text::Text(s.to_owned())))
    }
    fn discard<'v>(value: Value<'v>, eval: &mut Evaluator<'v, '_, '_>) -> anyhow::Result<NoneType> {
        state(eval).discarded.set(true);
        state(eval).record("discard", json!({}));
        eval.module().set_extra_value(value);
        bail!("__ACT_DISCARD__")
    }
}
#[starlark_module]
pub fn mode_b(builder: &mut GlobalsBuilder) {
    fn result<'v>(value: Value<'v>, eval: &mut Evaluator<'v, '_, '_>) -> anyhow::Result<NoneType> {
        if state(eval).result_set.replace(true) {
            bail!("result() 第二次调用");
        }
        eval.module().set_extra_value(value);
        Ok(NoneType)
    }
}
