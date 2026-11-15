// 扫描分摊到一级子树；扁平目录由文件队列分摊。时间戳近似干净窗口保守取 10 ms。
fn clock_ns() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos() as i64
}
fn threads() -> usize {
    std::env::var("S3_THREADS").ok().and_then(|x| x.parse().ok()).unwrap_or(8).max(1)
}
fn parallel_scan(root: &Path, cache: &Entries, last: i64, full: bool) -> Result<(Tree, usize)> {
    let mut jobs = Vec::new();
    for d in fs::read_dir(root)? {
        let d=d?; let p=d.file_name().into_string().map_err(|_|anyhow::anyhow!("原型要求 UTF-8"))?;
        if !excluded(&p) { jobs.push(p); }
    }
    let next=std::sync::atomic::AtomicUsize::new(0);
    let results=thread::scope(|scope| {
        let mut handles=Vec::new();
        for _ in 0..threads() {
            let jobs=&jobs; let next=&next;
            handles.push(scope.spawn(move || -> Result<(Entries,usize)> {
                let mut out=Entries::new(); let mut rearmed=0;
                fn visit(root:&Path,p:&str,cache:&Entries,last:i64,full:bool,out:&mut Entries,rearmed:&mut usize)->Result<()> {
                    let old=cache.get(p);
                    let racy=old.is_some_and(|e| e.ct*1_000_000_000+e.cn >= last-10_000_000 || e.mt*1_000_000_000+e.mn >= last-10_000_000);
                    if let Some(e)=inspect(&root.join(p),if full {None}else{old})? {
                        let mut e=e;
                        // 先绑定源 inode，再克隆稳定快照并重新武装 mmap；摘要来自快照。
                        // 不在读完可变源之后才 fsync，避免哈希/写回之间的脏页写丢失信号。
                        if racy && e.kind=="file" {
                            use std::os::unix::fs::OpenOptionsExt;
                            let source=File::open(root.join(p))?;
                            if source.metadata()?.ino()!=e.ino {bail!("扫描时路径被替换：{p}")}
                            let mut copy=OpenOptions::new().read(true).write(true).custom_flags(libc::O_TMPFILE).mode(0o600).open(root)?;
                            if unsafe{libc::ioctl(copy.as_raw_fd(),0x40049409u64,source.as_raw_fd())}!=0{return Err(std::io::Error::last_os_error().into());}
                            let mut h=Sha256::new();let mut buf=[0u8;65536];
                            loop {let n=copy.read(&mut buf)?;if n==0{break;}h.update(&buf[..n]);}
                            e.hash=format!("{:x}",h.finalize());*rearmed+=1;
                        }
                        let dir=e.kind=="directory"; out.insert(p.into(),e);
                        if dir {
                            for d in fs::read_dir(root.join(p))? {
                                let d=d?; let q=format!("{p}/{}",d.file_name().to_string_lossy());
                                visit(root,&q,cache,last,full,out,rearmed)?;
                            }
                        }
                    }
                    Ok(())
                }
                loop { let i=next.fetch_add(1,std::sync::atomic::Ordering::Relaxed); if i>=jobs.len(){break;}
                    visit(root,&jobs[i],cache,last,full,&mut out,&mut rearmed)?;
                }
                Ok((out,rearmed))
            }));
        }
        handles.into_iter().map(|h|h.join().unwrap()).collect::<Result<Vec<_>>>()
    })?;
    let mut entries=Entries::new();let mut rearmed=0;
    for (e,n) in results {entries.extend(e); rearmed+=n;}
    Ok((Tree::new(entries),rearmed))
}
fn aliases(t:&Tree)->Vec<Value> {
    t.groups.values().filter_map(|g|{
        let e=&t.entries[g.first().unwrap()];
        (e.nlink>g.len() as u64).then(||json!({"paths":g,"nlink":e.nlink,"inside_names":g.len(),"outside_names":e.nlink-g.len() as u64}))
    }).collect()
}
fn object_equal(a:Option<&Entry>,b:Option<&Entry>)->bool {
    match (a,b) {
        (None,None)=>true,
        (Some(a),Some(b))=>a.kind==b.kind && a.mode==b.mode && a.hash==b.hash && a.link==b.link,
        _=>false
    }
}
