// 后台只持有派生扫描缓存；工作区索引仍由 Node 发布和更新。
type Patch = BTreeMap<String, Option<Entry>>;
struct Background {
    tx: mpsc::Sender<Patch>,
    rx: mpsc::Receiver<(BTreeSet<String>, Value)>,
}
impl Background {
    fn new(root: PathBuf, mut baseline: Entries) -> Self {
        let (tx, commands) = mpsc::channel::<Patch>();
        let (output, rx) = mpsc::sync_channel(1);
        thread::spawn(move || {
            let mut last = clock_ns();
            let mut cache = baseline.clone();
            loop {
                match commands.recv_timeout(std::time::Duration::from_millis(100)) {
                    Ok(delta) => {
                        for (p, e) in delta {
                            if let Some(e) = e {
                                baseline.insert(p.clone(), e.clone());
                                cache.insert(p, e);
                            } else {
                                baseline.remove(&p);
                                cache.remove(&p);
                            }
                        }
                        continue;
                    }
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    Err(_) => {}
                }
                let start = Instant::now();
                let stamp = clock_ns();
                match parallel_scan(&root, &cache, last, false, true) {
                    Ok((new, rearmed)) => {
                        let paths = new
                            .entries
                            .keys()
                            .chain(baseline.keys())
                            .filter(|p| {
                                !signature(new.entries.get(*p), baseline.get(*p))
                                    || !semantic(new.entries.get(*p), baseline.get(*p))
                            })
                            .cloned()
                            .collect();
                        cache = new.entries;
                        last = stamp;
                        let _ = output.try_send((
                            paths,
                            json!({"scan_s":start.elapsed().as_secs_f64(),"rearmed":rearmed}),
                        ));
                    }
                    Err(e) => {
                        let _ =
                            output.try_send((BTreeSet::new(), json!({"error":format!("{e:#}")})));
                    }
                }
            }
        });
        Self { tx, rx }
    }
    fn patch(&self, delta: Patch) {
        if !delta.is_empty() {
            let _ = self.tx.send(delta);
        }
    }
}
impl Node {
    fn publish_background(&mut self) -> Result<()> {
        let Ok((mut paths, info)) = self.scanner.rx.try_recv() else {
            return Ok(());
        };
        self.background = info;
        paths.retain(|p| {
            !self.real_tree.entries.get(p).is_some_and(|e| {
                e.owned
                    && fs::symlink_metadata(self.real.join(p))
                        .ok()
                        .is_some_and(|m| stat_equal(&m, e))
            })
        });
        if paths.is_empty() {
            return Ok(());
        }
        let delta = self.real_tree.update(
            &self.real,
            Events {
                paths,
                ..Events::default()
            },
        )?;
        let paths: BTreeSet<_> = delta.keys().cloned().collect();
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
                    paths: paths.clone(),
                    ..Events::default()
                },
            )?;
            self.sw.flush()?;
            // 物化与摘要核对，变化中的源稍后重试。
            for p in &paths {
                if !object_equal(
                    self.real_tree.entries.get(p),
                    self.shadow_tree.entries.get(p),
                ) {
                    bail!("同步过程中宿主变化：{p}")
                }
            }
        }
        self.scanner.patch(
            paths
                .iter()
                .map(|p| (p.clone(), self.real_tree.entries.get(p).cloned()))
                .collect(),
        );
        self.background["synced"] = json!(paths.len());
        Ok(())
    }
}
fn hidden(p: &str) -> bool {
    p == "hidden" || p.starts_with("hidden/")
}
fn internal(p: &str) -> bool {
    p.split('/').any(|part| part.starts_with(".s3-v7-"))
}
fn stat_equal(m: &fs::Metadata, e: &Entry) -> bool {
    m.dev() == e.dev
        && m.ino() == e.ino
        && m.mode() & 0o7777 == e.mode
        && m.nlink() == e.nlink
        && m.size() == e.size
        && (m.mtime(), m.mtime_nsec(), m.ctime(), m.ctime_nsec()) == (e.mt, e.mn, e.ct, e.cn)
}
fn stat_entry(p: &Path, known: &Entry) -> Result<Option<Entry>> {
    let m = match fs::symlink_metadata(p) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    if known.kind == "file"
        && (m.size() != known.size || (m.mtime(), m.mtime_nsec()) != (known.mt, known.mn))
    {
        return current(p);
    }
    let mut e = known.clone();
    e.owned = true;
    e.dev = m.dev();
    e.ino = m.ino();
    e.nlink = m.nlink();
    e.size = m.size();
    e.mt = m.mtime();
    e.mn = m.mtime_nsec();
    e.ct = m.ctime();
    e.cn = m.ctime_nsec();
    Ok(Some(e))
}
fn initialize_parallel(real: &Path, shadow: &Path, entries: &Entries) -> Result<()> {
    let mut dirs: Vec<_> = entries
        .iter()
        .filter(|(_, e)| e.kind == "directory")
        .collect();
    dirs.sort_by_key(|(p, _)| p.matches('/').count());
    for (p, _) in &dirs {
        fs::create_dir_all(shadow.join(p))?;
    }
    let jobs: Vec<_> = entries
        .iter()
        .filter(|(p, e)| e.kind != "directory" && (e.kind != "file" || e.links.first() == Some(p)))
        .collect();
    let next = std::sync::atomic::AtomicUsize::new(0);
    thread::scope(|scope| -> Result<()> {
        let mut workers = vec![];
        for _ in 0..threads() {
            let jobs = &jobs;
            let next = &next;
            workers.push(scope.spawn(move || -> Result<()> {
                loop {
                    let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    if i >= jobs.len() {
                        break;
                    }
                    let (p, e) = jobs[i];
                    put_object(&real.join(p), &shadow.join(p), e)?;
                    if e.kind == "file" {
                        for q in &e.links {
                            if q != p {
                                fs::hard_link(shadow.join(p), shadow.join(q))?;
                            }
                        }
                    }
                }
                Ok(())
            }));
        }
        for w in workers {
            w.join().unwrap()?;
        }
        Ok(())
    })?;
    for (p, e) in dirs.into_iter().rev() {
        meta(&shadow.join(p), e)?;
    }
    barrier(shadow)
}
