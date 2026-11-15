include!("base-v7.rs");
include!("scan-v7.rs");
include!("transaction-v7.rs");
include!("background-v7.rs");
struct Node {
    real: PathBuf,
    state: PathBuf,
    shadow: PathBuf,
    real_tree: Tree,
    shadow_tree: Tree,
    rw: Watch,
    sw: Watch,
    _lock: File,
    active: bool,
    paths: BTreeSet<String>,
    before: Entries,
    last_scan: i64,
    shadow_scan: i64,
    background: Value,
    renames: Vec<Rename>,
    scanner: Background,
}
impl Node {
    fn git(&self, op: &str) -> Result<Value> {
        if !self.real.join(".git/HEAD").is_file() {
            return Ok(json!({"paths":[],"locks":[],"conflicts":[]}));
        }
        let helper = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .join("git-v7.py");
        let o = std::process::Command::new("python3")
            .arg(helper)
            .arg(op)
            .arg(&self.real)
            .arg(&self.state)
            .output()?;
        if !o.status.success() {
            bail!("Git 准备失败：{}", String::from_utf8_lossy(&o.stderr));
        }
        Ok(serde_json::from_slice(&o.stdout)?)
    }
    fn new(real: PathBuf, state: PathBuf) -> Result<(Self, Value)> {
        let start = Instant::now();
        fs::create_dir_all(&state)?;
        if state.starts_with(&real) {
            bail!("影子状态必须位于工作目录之外")
        }
        let canonical_state = state.canonicalize()?;
        for d in [
            "home",
            "tmp",
            "dev",
            "root",
            "shadow",
            "shadow/target",
            "shadow/.git",
        ] {
            fs::create_dir_all(state.join(d))?;
            if !state.join(d).canonicalize()?.starts_with(&canonical_state) {
                bail!("可写挂载来源不在状态目录：{d}")
            }
        }
        if let Ok(spec) = std::env::var("S3_WRITABLE_SOURCES") {
            let sources: Vec<PathBuf> = serde_json::from_str(&spec)?;
            let canonical = state.canonicalize()?;
            for source in sources {
                if !source.canonicalize()?.starts_with(&canonical) {
                    bail!("可写挂载来源不在状态目录：{}", source.display())
                }
            }
        }
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(state.join("lock"))?;
        if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            bail!("状态目录已有写者")
        }
        if real.metadata()?.dev() != state.metadata()?.dev() {
            bail!("状态与宿主须在同一文件系统")
        }
        fs::create_dir_all(state.join("transactions"))?;
        let mut recovery = vec![];
        for d in fs::read_dir(state.join("transactions"))? {
            let d = d?;
            if d.path().join("transaction-v7.json").exists() {
                recovery.push(recover_v7(&d.path(), false)?);
            } else if let Ok(bytes) = fs::read(d.path().join("staging-v7.json")) {
                let names: Vec<PathBuf> = serde_json::from_slice(&bytes)?;
                for name in names {
                    if fs::symlink_metadata(&name).is_ok() {
                        remove(&name)?;
                    }
                }
            }
        }
        let lock_manifest = state.join("git-locks-v7.json");
        if let Ok(bytes) = fs::read(&lock_manifest) {
            let m: Value = serde_json::from_slice(&bytes)?;
            if let (Some(token), Some(paths)) = (m["token"].as_str(), m["paths"].as_array()) {
                for path in paths {
                    let lock = real.join(path.as_str().unwrap());
                    if fs::read(&lock).ok().as_deref() == Some(token.as_bytes()) {
                        fs::remove_file(lock)?;
                    }
                }
            }
            fs::remove_file(lock_manifest)?;
        }
        let shadow = state.join("shadow");
        fs::create_dir_all(&shadow)?;
        for d in ["target", ".git/host-objects-v7"] {
            fs::create_dir_all(shadow.join(d))?;
        }
        fs::create_dir_all(state.join("pass-0"))?;
        let rw = Watch::new(real.clone())?;
        let sw = Watch::new(shadow.clone())?;
        let (real_tree, _) = parallel_scan(&real, &Entries::new(), 0, true, false)?;
        let (old, _) = parallel_scan(&shadow, &Entries::new(), 0, true, false)?;
        let paths = real_tree
            .entries
            .keys()
            .chain(old.entries.keys())
            .cloned()
            .collect();
        barrier(&real)?;
        if old.entries.is_empty() {
            initialize_parallel(&real, &shadow, &real_tree.entries)?;
        } else {
            materialize(
                &shadow,
                &real_tree.entries,
                &paths,
                &|p| real.join(p),
                &BTreeMap::new(),
                &BTreeSet::new(),
                |_| Ok(()),
                false,
            )?;
        }
        if real.join("target").is_dir() && fs::read_dir(shadow.join("target"))?.next().is_none() {
            let status = std::process::Command::new("cp")
                .args(["-a", "--reflink=always"])
                .arg(real.join("target/."))
                .arg(shadow.join("target"))
                .status()?;
            if !status.success() {
                bail!("regenerable 种子复制失败")
            }
        }
        let scanner = Background::new(real.clone(), real_tree.entries.clone());
        let (shadow_tree, _) = parallel_scan(&shadow, &Entries::new(), 0, true, false)?;
        let mut n = Self {
            real,
            state,
            shadow,
            real_tree,
            shadow_tree,
            rw,
            sw,
            _lock: lock,
            active: false,
            paths: BTreeSet::new(),
            before: Entries::new(),
            last_scan: clock_ns(),
            shadow_scan: clock_ns(),
            background: json!({}),
            renames: vec![],
            scanner,
        };
        let git = n.git("sync")?;
        n.rw.flush()?;
        n.sw.flush()?;
        let info = json!({"init_s":start.elapsed().as_secs_f64(),"entries":n.real_tree.entries.len(),"external_aliases":aliases(&n.real_tree),"recovery":recovery,"git":git,"pid":std::process::id()});
        n.background = json!({"scan_s":0.});
        Ok((n, info))
    }
    fn synchronize(&mut self, full: bool) -> Result<Value> {
        let start = Instant::now();
        let stamp = clock_ns();
        let (new, rearmed) = parallel_scan(
            &self.real,
            &self.real_tree.entries,
            self.last_scan,
            full,
            false,
        )?;
        let scan_s = start.elapsed().as_secs_f64();
        let mut paths: BTreeSet<_> = new
            .entries
            .keys()
            .chain(self.real_tree.entries.keys())
            .filter(|p| {
                !signature(new.entries.get(*p), self.real_tree.entries.get(*p))
                    || !semantic(new.entries.get(*p), self.real_tree.entries.get(*p))
            })
            .cloned()
            .collect();
        for p in paths.clone() {
            if let Some(e) = new.entries.get(&p) {
                paths.extend(e.links.iter().cloned());
            }
        }
        if !paths.is_empty() {
            barrier(&self.real)?;
            materialize(
                &self.shadow,
                &new.entries,
                &paths,
                &|p| self.real.join(p),
                &BTreeMap::new(),
                &BTreeSet::new(),
                |_| Ok(()),
                false,
            )?;
            // 克隆以后再读影子，核对实际同步字节，不能将不相干的宿主版本提升为基准。
            let events = Events {
                paths: paths.clone(),
                ..Events::default()
            };
            self.shadow_tree.update(&self.shadow, events)?;
            for p in &paths {
                if !object_equal(new.entries.get(p), self.shadow_tree.entries.get(p)) {
                    bail!("同步过程中宿主变化，稍后重试：{p}")
                }
            }
        }
        self.scanner.patch(
            new.entries
                .keys()
                .chain(self.real_tree.entries.keys())
                .map(|p| (p.clone(), new.entries.get(p).cloned()))
                .collect(),
        );
        self.real_tree = new;
        self.last_scan = stamp;
        self.sw.flush()?;
        Ok(
            json!({"scan_s":scan_s,"sync_s":start.elapsed().as_secs_f64(),"synced":paths.len(),"rearmed":rearmed,"external_aliases":aliases(&self.real_tree)}),
        )
    }
    fn begin(&mut self, full: bool) -> Result<Value> {
        if self.active {
            bail!("前次 act 尚未结算")
        }
        let start = Instant::now();
        self.publish_background()?;
        let mut ev = self.rw.flush()?;
        ev.paths.retain(|p| !hidden(p) && !internal(p));
        ev.recursive.retain(|p| !hidden(p) && !internal(p));
        let count = ev.count;
        let changes = if full || ev.overflow || ev.uncertain {
            self.synchronize(true)?
        } else if !ev.paths.is_empty() || !ev.recursive.is_empty() {
            // 事件候选作加速；基线始终来自本次实际物化的版本。
            // 合并已经写入索引；相同 stat 的自写事件不重新计算摘要。
            ev.paths.retain(|p| {
                !fs::symlink_metadata(self.real.join(p))
                    .ok()
                    .zip(self.real_tree.entries.get(p))
                    .is_some_and(|(m, e)| stat_equal(&m, e))
            });
            let delta = self.real_tree.update(&self.real, ev)?;
            self.scanner.patch(
                delta
                    .keys()
                    .map(|p| (p.clone(), self.real_tree.entries.get(p).cloned()))
                    .collect(),
            );
            let mut paths: BTreeSet<_> = delta.keys().cloned().collect();
            for p in paths.clone() {
                if let Some(e) = self.real_tree.entries.get(&p) {
                    paths.extend(e.links.iter().cloned());
                }
            }
            if !paths.is_empty() {
                barrier(&self.real)?;
                materialize(
                    &self.shadow,
                    &self.real_tree.entries,
                    &paths,
                    &|p| self.real.join(p),
                    &BTreeMap::new(),
                    &BTreeSet::new(),
                    |_| Ok(()),
                    false,
                )?;
                self.shadow_tree.update(
                    &self.shadow,
                    Events {
                        paths: paths.clone(),
                        ..Events::default()
                    },
                )?;
            }
            json!({"synced":paths.len()})
        } else {
            json!({"synced":0})
        };
        self.sw.flush()?;
        self.paths.clear();
        self.renames.clear();
        self.before.clear();
        self.shadow_scan = clock_ns();
        let git = self.git("sync")?;
        self.active = true;
        Ok(
            json!({"sync_s":start.elapsed().as_secs_f64(),"events":count,"sync":changes,"git":git,"background":self.background}),
        )
    }
    fn finish(&mut self, audit: bool, overflow: bool) -> Result<Value> {
        let start = Instant::now();
        let mut ev = self.sw.flush()?;
        ev.overflow |= overflow;
        let count = ev.count;
        let overflowed = ev.overflow;
        let fallback = ev.overflow || ev.uncertain;
        let baseline = if audit {
            Some(self.shadow_tree.entries.clone())
        } else {
            None
        };
        let delta = self.shadow_tree.update(&self.shadow, ev)?;
        self.paths = delta
            .iter()
            .filter(|(p, e)| !semantic(e.as_ref(), self.shadow_tree.entries.get(*p)))
            .map(|(p, _)| p.clone())
            .collect();
        self.before = delta
            .into_iter()
            .filter(|(p, _)| self.paths.contains(p))
            .filter_map(|(p, e)| e.map(|e| (p, e)))
            .collect();
        let tree = &self.shadow_tree;
        let mut dirs = HashMap::new();
        for (p, e) in &self.before {
            if e.kind == "directory" {
                dirs.insert(e.ino, p.clone());
            }
        }
        for p in &self.paths {
            if let Some(e) = tree.entries.get(p) {
                if e.kind == "directory" {
                    if let Some(old) = dirs.get(&e.ino) {
                        if old != p
                            && !tree.entries.contains_key(old)
                            && !self.real_tree.entries.contains_key(p)
                        {
                            self.renames.push(Rename {
                                from: old.clone(),
                                to: p.clone(),
                            });
                        }
                    }
                }
            }
        }
        self.renames.sort_by_key(|r| r.from.len());
        let mut kept: Vec<Rename> = vec![];
        for r in std::mem::take(&mut self.renames) {
            if !kept
                .iter()
                .any(|k| r.from.starts_with(&format!("{}/", k.from)))
            {
                kept.push(r);
            }
        }
        self.renames = kept;
        let mut out = json!({"scan_s":start.elapsed().as_secs_f64(),"events":count,"fallback":fallback,"overflow":overflowed,"epsilon":{"paths":self.paths,"directory_renames":self.renames}});
        if audit {
            let full = Tree::scan(&self.shadow, &Entries::new())?;
            let expected: BTreeSet<_> = full
                .entries
                .keys()
                .chain(baseline.as_ref().unwrap().keys())
                .filter(|p| !semantic(full.entries.get(*p), baseline.as_ref().unwrap().get(*p)))
                .cloned()
                .collect();
            out["audit"] = json!({"exact":expected==self.paths,"full_paths":expected});
        }
        Ok(out)
    }
    fn rebuild(&mut self) -> Result<f64> {
        let start = Instant::now();
        let mut paths = self.paths.clone();
        let hidden_paths: Vec<_> = paths.iter().filter(|p| hidden(p)).cloned().collect();
        for p in hidden_paths.iter().rev() {
            remove(&self.shadow.join(p))?;
        }
        paths.retain(|p| !hidden(p));
        self.shadow_tree
            .patch(&hidden_paths.into_iter().map(|p| (p, None)).collect());
        let changes = self.real_tree.update(
            &self.real,
            Events {
                paths: paths.clone(),
                ..Events::default()
            },
        )?;
        paths.extend(changes.into_keys());
        self.scanner.patch(
            paths
                .iter()
                .map(|p| (p.clone(), self.real_tree.entries.get(p).cloned()))
                .collect(),
        );
        if !paths.is_empty() {
            materialize(
                &self.shadow,
                &self.real_tree.entries,
                &paths,
                &|p| self.real.join(p),
                &BTreeMap::new(),
                &BTreeSet::new(),
                |_| Ok(()),
                false,
            )?;
            self.shadow_tree.update(
                &self.shadow,
                Events {
                    paths,
                    ..Events::default()
                },
            )?;
        }
        self.sw.flush()?;
        self.active = false;
        Ok(start.elapsed().as_secs_f64())
    }
    fn discard(&mut self) -> Result<Value> {
        let _ = fs::remove_file(self.state.join("git-stamp-v7.json"));
        let paths = self.paths.clone();
        let t = self.rebuild()?;
        Ok(json!({"outcome":"discarded","uncommitted_reverted":paths,"discard_s":t}))
    }
    fn unlock(&self, locks: &[String]) {
        for p in locks {
            let _ = fs::remove_file(self.real.join(p));
        }
    }
    fn commit(&mut self) -> Result<Value> {
        let start = Instant::now();
        let policy: Vec<_> = self
            .paths
            .iter()
            .filter(|p| {
                hidden(p)
                    || self
                        .real_tree
                        .entries
                        .get(*p)
                        .is_some_and(|e| e.kind == "file" && e.nlink > 1)
                    || fs::symlink_metadata(self.real.join(p))
                        .ok()
                        .is_some_and(|m| m.is_file() && m.nlink() > 1)
            })
            .cloned()
            .collect();
        if !policy.is_empty() {
            let rebuild = self.rebuild()?;
            return Ok(
                json!({"outcome":"Conflict","conflicts":policy,"reason":"hidden 或宿主多链接文件","rebuild_s":rebuild,"commit_s":start.elapsed().as_secs_f64()}),
            );
        }
        let git = self.git("prepare")?;
        let locks: Vec<String> = serde_json::from_value(git["locks"].clone())?;
        let mut conflicts: Vec<String> = serde_json::from_value(git["conflicts"].clone())?;
        for p in &self.paths {
            if !object_equal(
                self.real_tree.entries.get(p),
                current(&self.real.join(p))?.as_ref(),
            ) {
                conflicts.push(p.clone());
            }
        }
        if !conflicts.is_empty() {
            self.unlock(&locks);
            let rebuild = self.rebuild()?;
            return Ok(
                json!({"outcome":"Conflict","conflicts":conflicts,"git":git,"rebuild_s":rebuild,"commit_s":start.elapsed().as_secs_f64()}),
            );
        }
        let gitpaths: Vec<String> = serde_json::from_value(git["paths"].clone())?;
        if self.paths.is_empty() && gitpaths.is_empty() {
            self.unlock(&locks);
            self.active = false;
            return Ok(
                json!({"outcome":"Unchanged","git":git,"commit_s":start.elapsed().as_secs_f64()}),
            );
        }
        let txn = self.state.join("transactions").join(clock_ns().to_string());
        fs::create_dir_all(&txn)?;
        let mut ops = Vec::new();
        let mut workpaths = self.paths.clone();
        let mut renamed_before = Entries::new();
        for r in &self.renames {
            // 同类别移动的三方核对；子项另有编辑时仍逐项处理差异。
            let before = self.real_tree.entries.get(&r.from).cloned();
            let backup = txn.join(format!("rename-{}.json", ops.len()));
            let prefix = format!("{}/", r.from);
            let subtree: Entries = self
                .real_tree
                .entries
                .iter()
                .filter_map(|(p, e)| p.strip_prefix(&prefix).map(|q| (q.to_string(), e.clone())))
                .collect();
            save(&backup, &subtree)?;
            ops.push(Operation {
                path: r.to.clone(),
                source: r.from.clone(),
                kind: "rename".into(),
                before: before.clone(),
                after: before,
                slot: txn.join(format!("slot-{}", ops.len())),
                stage: txn.join(format!("slot-{}", ops.len())),
                backup,
            });
            let mut names = BTreeSet::new();
            for (p, e) in &self.real_tree.entries {
                if p == &r.from || p.starts_with(&format!("{}/", r.from)) {
                    let q = format!("{}{}", r.to, &p[r.from.len()..]);
                    renamed_before.insert(q.clone(), e.clone());
                    names.insert(q);
                    workpaths.remove(p);
                }
            }
            names.extend(
                self.shadow_tree
                    .entries
                    .keys()
                    .filter(|p| *p == &r.to || p.starts_with(&format!("{}/", r.to)))
                    .cloned(),
            );
            for p in names {
                workpaths.remove(&p);
                if !object_equal(renamed_before.get(&p), self.shadow_tree.entries.get(&p)) {
                    workpaths.insert(p);
                }
            }
        }
        let mut ordered: Vec<_> = workpaths.iter().cloned().collect();
        // 删除子项先于目录；其余父目录先于子项。
        ordered.sort_by_key(|p| {
            if self.shadow_tree.entries.contains_key(p) {
                (1, p.matches('/').count() as i64, p.clone())
            } else {
                (0, -(p.matches('/').count() as i64), p.clone())
            }
        });
        ordered.extend(gitpaths);
        barrier(&self.shadow)?;
        let mut stage_links: HashMap<(u64, u64), PathBuf> = HashMap::new();
        // 先持久记录宿主临时名，再创建；准备阶段崩溃只清理这些私有临时名。
        let stage_names: Vec<PathBuf> = ordered
            .iter()
            .enumerate()
            .map(|(i, p)| {
                self.real.join(p).with_file_name(format!(
                    ".s3-v7-{}-slot-{}",
                    txn.file_name().unwrap().to_string_lossy(),
                    i + ops.len()
                ))
            })
            .collect();
        save(&txn.join("staging-v7.json"), &stage_names)?;
        for (position, p) in ordered.into_iter().enumerate() {
            let is_git = p.starts_with(".git/");
            let before = if is_git {
                current(&self.real.join(&p))?
            } else {
                renamed_before
                    .get(&p)
                    .or(self.real_tree.entries.get(&p))
                    .cloned()
            };
            let src = if is_git {
                self.state.join("git-merge-v7").join(&p[5..])
            } else {
                self.shadow.join(&p)
            };
            let after = if is_git {
                current(&src)?
            } else {
                self.shadow_tree.entries.get(&p).cloned()
            };
            let kind = match (&before, &after) {
                (Some(b), Some(a)) if b.kind == "directory" && a.kind == "directory" => "mode",
                (Some(_), Some(_)) => "exchange",
                (None, Some(_)) => "create",
                (Some(_), None) => "delete",
                _ => continue,
            };
            let slot = txn.join(format!("slot-{}", ops.len()));
            let backup = txn.join(format!("backup-{}", ops.len()));
            let stage = if stage_names[position].parent().unwrap().is_dir()
                && matches!(kind, "exchange" | "create")
            {
                stage_names[position].clone()
            } else {
                slot.clone()
            };
            let mut after = after;
            if matches!(kind, "exchange" | "create") {
                let e = after.as_ref().unwrap();
                let key = (e.dev, e.ino);
                if let Some(prior) = stage_links.get(&key).filter(|_| e.kind == "file") {
                    fs::hard_link(prior, &stage)?;
                } else {
                    put_object(&src, &stage, e)?;
                    if e.kind == "file" {
                        stage_links.insert(key, stage.clone());
                    }
                }
                let mut a = stat_entry(&stage, e)?.unwrap();
                a.links = e.links.clone();
                a.hash = e.hash.clone();
                after = Some(a);
            }
            ops.push(Operation {
                path: p,
                source: String::new(),
                kind: kind.into(),
                before,
                after,
                slot,
                stage,
                backup,
            });
        }
        let mut j = Journal {
            phase: "ready".into(),
            real: self.real.clone(),
            ops,
            locks: locks.clone(),
        };
        point("before-log");
        barrier(&txn)?;
        save(&txn.join("transaction-v7.json"), &j)?;
        point("after-ready");
        let prepare_s = start.elapsed().as_secs_f64();
        let mut windows = vec![];
        let visible = Instant::now();
        let mut partial_visible_s = 0.;
        for (i, op) in j.ops.iter().enumerate() {
            let dst = self.real.join(&op.path);
            inject(&self.real, "before-check", &op.path);
            let check = Instant::now();
            let checkpath = if op.kind == "rename" {
                self.real.join(&op.source)
            } else {
                dst.clone()
            };
            let expected = op.before.as_ref();
            if fs::symlink_metadata(&checkpath).ok().is_some_and(|m| {
                m.is_file() && m.nlink() > 1 && expected.is_some_and(|e| e.nlink == 1)
            }) || !object_equal(expected, current(&checkpath)?.as_ref())
            {
                conflicts.push(op.path.clone());
                break;
            }
            if op.kind == "rename" {
                let old: Entries = serde_json::from_slice(&fs::read(&op.backup)?)?;
                let now = Tree::scan(&checkpath, &Entries::new())?;
                if dst.exists()
                    || old.len() != now.entries.len()
                    || old
                        .iter()
                        .any(|(p, e)| !object_equal(Some(e), now.entries.get(p)))
                {
                    conflicts.push(op.path.clone());
                    break;
                }
            }
            inject(&self.real, "after-check", &op.path);
            let result: Result<()> = match op.kind.as_str() {
                "exchange" => install_local(op, &dst, libc::RENAME_EXCHANGE),
                "create" => install_local(op, &dst, libc::RENAME_NOREPLACE),
                "delete" => exchange(&dst, &op.slot, libc::RENAME_NOREPLACE),
                "rename" => exchange(&checkpath, &dst, libc::RENAME_NOREPLACE),
                "mode" => meta(&dst, op.after.as_ref().unwrap()),
                _ => Ok(()),
            };
            windows.push(check.elapsed().as_nanos() as u64);
            partial_visible_s = visible.elapsed().as_secs_f64();
            if result.is_err() {
                conflicts.push(op.path.clone());
                break;
            }
            point(&format!("apply-{}", i + 1));
            if op.kind == "rename" {
                point("after-rename");
            }
            // 换出的 inode 是实际前态；事后核对能发现检查窗口中部分宿主写入。
            if (op.kind == "exchange" || op.kind == "delete")
                && !object_equal(expected, current(&op.slot)?.as_ref())
            {
                conflicts.push(op.path.clone());
                break;
            }
        }
        if !conflicts.is_empty() {
            if let Ok(p) = std::env::var("S3_ROLLBACK_WRITE") {
                fs::write(self.real.join(p), b"host-during-rollback")?;
            }
            let rollback = recover_v7(&txn, false)?;
            self.unlock(&locks);
            let rebuild = self.rebuild()?;
            return Ok(
                json!({"outcome":if rollback["outcome"]=="Indeterminate"{"Indeterminate"}else{"Conflict"},"conflicts":conflicts,"rollback":rollback,"git":git,"transaction":txn,"rebuild_s":rebuild,"commit_s":start.elapsed().as_secs_f64()}),
            );
        }
        point("before-post-sync");
        barrier(&self.real)?;
        point("after-post-sync");
        j.phase = "complete".into();
        save(&txn.join("transaction-v7.json"), &j)?;
        point("after-complete");
        self.unlock(&locks);
        let apply_s = start.elapsed().as_secs_f64() - prepare_s;
        let index_start = Instant::now();
        let mut delta = BTreeMap::new();
        for p in &self.paths {
            let entry = match self.shadow_tree.entries.get(p) {
                Some(e) => stat_entry(&self.real.join(p), e)?,
                None => None,
            };
            delta.insert(p.clone(), entry);
        }
        self.real_tree.patch(&delta);
        self.scanner.patch(delta);
        self.active = false;
        let rebuild_s = 0.;
        let index_s = index_start.elapsed().as_secs_f64();
        Ok(
            json!({"outcome":"Committed","transaction":txn,"git":git,"commit_s":start.elapsed().as_secs_f64(),"prepare_s":prepare_s,"apply_s":apply_s,"rebuild_s":rebuild_s,"index_s":index_s,"check_to_replace_ns":windows,"partial_visible_s":partial_visible_s,"rename_syscalls":self.renames.len(),"conflicts":[]}),
        )
    }
}
fn main() -> Result<()> {
    let args: Vec<_> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("verify-git-objects") {
        let root = Path::new(&args[2]);
        let repo = gix::open(root)?;
        let mut count = 0;
        for dir in fs::read_dir(root.join(".git/objects"))? {
            let dir = dir?;
            let name = dir.file_name().to_string_lossy().into_owned();
            if name.len() != 2 || !dir.file_type()?.is_dir() {
                continue;
            }
            for file in fs::read_dir(dir.path())? {
                let file = file?;
                let hex = format!("{name}{}", file.file_name().to_string_lossy());
                let oid = gix::hash::ObjectId::from_hex(hex.as_bytes())?;
                let object = repo.find_object(oid)?;
                let computed =
                    gix::objs::compute_hash(gix::hash::Kind::Sha1, object.kind, &object.data)?;
                if oid != computed {
                    bail!("Git 对象校验失败：{hex}")
                };
                count += 1;
            }
        }
        println!(
            "{}",
            json!({"objects_verified":count,"implementation":"gix 0.85"})
        );
        return Ok(());
    }

    if args.get(1).map(String::as_str) == Some("recover") {
        println!(
            "{}",
            recover_v7(Path::new(&args[2]), args.iter().any(|a| a == "--revert"))?
        );
        return Ok(());
    }
    let (mut n, info) = Node::new(PathBuf::from(&args[1]), PathBuf::from(&args[2]))?;
    println!("{info}");
    std::io::stdout().flush()?;
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        for line in std::io::stdin().lock().lines() {
            if tx.send(line).is_err() {
                break;
            }
        }
    });
    loop {
        let req = match rx.recv_timeout(std::time::Duration::from_millis(5)) {
            Ok(line) => serde_json::from_str::<Value>(&line?)?,
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Err(_) => {
                if !n.active {
                    if let Err(e) = n.publish_background() {
                        n.background = json!({"error":format!("{e:#}")});
                    }
                }
                continue;
            }
        };
        let result = match req["op"].as_str().unwrap_or("") {
            "begin" => n.begin(req["full"].as_bool().unwrap_or(false)),
            "finish" => n.finish(
                req["audit"].as_bool().unwrap_or(false),
                req["overflow"].as_bool().unwrap_or(false),
            ),
            "commit" => {
                SCAN_PAUSED.store(true, std::sync::atomic::Ordering::Release);
                let result = n.commit();
                SCAN_PAUSED.store(false, std::sync::atomic::Ordering::Release);
                result
            }
            "discard" => n.discard(),
            "sync" => n.synchronize(req["full"].as_bool().unwrap_or(false)),
            "stats" => Ok(
                json!({"scan_digests":SCAN_DIGESTS.load(std::sync::atomic::Ordering::Relaxed),"background":n.background,"entries":n.real_tree.entries.len(),"external_aliases":aliases(&n.real_tree)}),
            ),
            "env" => {
                if let Some(m) = req["values"].as_object() {
                    for (k, v) in m {
                        unsafe {
                            if let Some(s) = v.as_str() {
                                std::env::set_var(k, s)
                            } else {
                                std::env::remove_var(k)
                            }
                        }
                    }
                }
                Ok(json!({"ok":true}))
            }
            "stop" => break,
            _ => Err(anyhow::anyhow!("未知操作")),
        };
        match result {
            Ok(v) => println!("{v}"),
            Err(e) => println!("{}", json!({"error":format!("{e:#}")})),
        };
        std::io::stdout().flush()?;
    }
    Ok(())
}
