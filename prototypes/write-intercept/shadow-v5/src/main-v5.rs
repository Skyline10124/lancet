//! S2 研究原型：固定分类、常驻双侧监听、增量索引及可恢复的合并。
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    ffi::CString,
    fs::{self, File, OpenOptions},
    io::{BufRead, Read, Write},
    os::{
        fd::AsRawFd,
        unix::{
            ffi::OsStrExt,
            fs::{MetadataExt, PermissionsExt, symlink},
        },
    },
    path::{Path, PathBuf},
    sync::mpsc,
    thread,
    time::Instant,
};
const EXCLUDED: [&str; 3] = [".git", "target", "hidden"];
type Entries = BTreeMap<String, Entry>;
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, Default)]
struct Entry {
    kind: String,
    mode: u32,
    dev: u64,
    ino: u64,
    nlink: u64,
    size: u64,
    mt: i64,
    mn: i64,
    ct: i64,
    cn: i64,
    hash: String,
    link: String,
    #[serde(default)]
    links: BTreeSet<String>,
}
impl Entry {
    fn signature(&self, b: &Self) -> bool {
        self.kind == b.kind
            && self.mode == b.mode
            && self.dev == b.dev
            && self.ino == b.ino
            && self.nlink == b.nlink
            && self.size == b.size
            && (self.mt, self.mn, self.ct, self.cn) == (b.mt, b.mn, b.ct, b.cn)
    }
    fn semantic(&self, b: &Self) -> bool {
        self.kind == b.kind
            && self.mode == b.mode
            && self.hash == b.hash
            && self.link == b.link
            && self.links == b.links
    }
}
fn inspect(p: &Path, old: Option<&Entry>) -> Result<Option<Entry>> {
    let m = match fs::symlink_metadata(p) {
        Ok(x) => x,
        Err(e) if matches!(e.raw_os_error(), Some(libc::ENOENT | libc::ENOTDIR)) => {
            return Ok(None);
        }
        Err(e) => return Err(e.into()),
    };
    let ft = m.file_type();
    let mut e = Entry {
        kind: if ft.is_file() {
            "file"
        } else if ft.is_dir() {
            "directory"
        } else if ft.is_symlink() {
            "symlink"
        } else if m.mode() & libc::S_IFMT == libc::S_IFIFO {
            "fifo"
        } else {
            "special"
        }
        .into(),
        mode: m.mode() & 0o7777,
        dev: m.dev(),
        ino: m.ino(),
        nlink: m.nlink(),
        size: m.size(),
        mt: m.mtime(),
        mn: m.mtime_nsec(),
        ct: m.ctime(),
        cn: m.ctime_nsec(),
        ..Entry::default()
    };
    if e.kind == "file" {
        e.hash = if let Some(b) = old.filter(|b| e.signature(b)) {
            b.hash.clone()
        } else {
            let mut f = File::open(p)?;
            let mut h = Sha256::new();
            let mut buf = [0; 65536];
            loop {
                let n = f.read(&mut buf)?;
                if n == 0 {
                    break;
                }
                h.update(&buf[..n]);
            }
            format!("{:x}", h.finalize())
        };
    }
    if e.kind == "symlink" {
        e.link = fs::read_link(p)?.to_string_lossy().into_owned();
    }
    Ok(Some(e))
}
fn semantic(a: Option<&Entry>, b: Option<&Entry>) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(a), Some(b)) => a.semantic(b),
        _ => false,
    }
}
fn signature(a: Option<&Entry>, b: Option<&Entry>) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(a), Some(b)) => a.signature(b),
        _ => false,
    }
}
fn excluded(p: &str) -> bool {
    EXCLUDED.contains(&p.split('/').next().unwrap_or(""))
}
fn descendants(m: &Entries, p: &str) -> Vec<String> {
    let prefix = format!("{p}/");
    m.range(prefix.clone()..)
        .take_while(|(q, _)| q.starts_with(&prefix))
        .map(|(q, _)| q.clone())
        .collect()
}
fn walk(root: &Path, p: &str, out: &mut Entries, cache: &Entries) -> Result<()> {
    for d in fs::read_dir(root.join(p))? {
        let d = d?;
        let name = d
            .file_name()
            .into_string()
            .map_err(|_| anyhow::anyhow!("原型只接受 UTF-8 路径"))?;
        let q = if p.is_empty() {
            name
        } else {
            format!("{p}/{name}")
        };
        if excluded(&q) {
            continue;
        }
        if let Some(e) = inspect(&root.join(&q), cache.get(&q))? {
            let dir = e.kind == "directory";
            out.insert(q.clone(), e);
            if dir {
                walk(root, &q, out, cache)?;
            }
        }
    }
    Ok(())
}
#[derive(Default)]
struct Tree {
    entries: Entries,
    groups: HashMap<(u64, u64), BTreeSet<String>>,
}
impl Tree {
    fn new(entries: Entries) -> Self {
        let mut t = Self {
            entries,
            ..Self::default()
        };
        for (p, e) in &t.entries {
            if e.kind == "file" {
                t.groups
                    .entry((e.dev, e.ino))
                    .or_default()
                    .insert(p.clone());
            }
        }
        for e in t.entries.values_mut() {
            if e.kind == "file" {
                e.links = t.groups[&(e.dev, e.ino)].clone();
            }
        }
        t
    }
    fn scan(root: &Path, cache: &Entries) -> Result<Self> {
        let mut m = Entries::new();
        walk(root, "", &mut m, cache)?;
        Ok(Self::new(m))
    }
    fn update(&mut self, root: &Path, events: Events) -> Result<BTreeMap<String, Option<Entry>>> {
        if events.overflow || events.uncertain {
            let new = Self::scan(root, &Entries::new())?;
            let mut old = BTreeMap::new();
            for p in self.entries.keys().chain(new.entries.keys()) {
                if self.entries.get(p) != new.entries.get(p) {
                    old.insert(p.clone(), self.entries.get(p).cloned());
                }
            }
            *self = new;
            return Ok(old);
        }
        let mut paths = events.paths;
        for p in events.recursive {
            paths.insert(p.clone());
            paths.extend(descendants(&self.entries, &p));
            if root.join(&p).is_dir() {
                let mut m = Entries::new();
                walk(root, &p, &mut m, &self.entries)?;
                paths.extend(m.into_keys());
            }
        }
        let mut examined = BTreeMap::new();
        for p in &paths {
            if let Some(e) = self.entries.get(p) {
                if let Some(g) = self.groups.get(&(e.dev, e.ino)) {
                    examined.extend(g.iter().map(|q| (q.clone(), ())));
                }
            }
            if let Ok(m) = fs::symlink_metadata(root.join(p)) {
                if let Some(g) = self.groups.get(&(m.dev(), m.ino())) {
                    examined.extend(g.iter().map(|q| (q.clone(), ())));
                }
            }
        }
        paths.extend(examined.into_keys());
        let old: BTreeMap<_, _> = paths
            .iter()
            .map(|p| (p.clone(), self.entries.get(p).cloned()))
            .collect();
        let mut keys = BTreeSet::new();
        for p in &paths {
            if let Some(e) = self.entries.remove(p) {
                let k = (e.dev, e.ino);
                keys.insert(k);
                if let Some(g) = self.groups.get_mut(&k) {
                    g.remove(p);
                }
            }
        }
        for p in &paths {
            if let Some(e) = inspect(&root.join(p), None)? {
                if e.kind == "file" {
                    let k = (e.dev, e.ino);
                    keys.insert(k);
                    self.groups.entry(k).or_default().insert(p.clone());
                }
                self.entries.insert(p.clone(), e);
            }
        }
        for k in keys {
            if let Some(g) = self.groups.get(&k) {
                for p in g {
                    if let Some(e) = self.entries.get_mut(p) {
                        e.links = g.clone();
                    }
                }
                if g.is_empty() {
                    self.groups.remove(&k);
                }
            }
        }
        Ok(old
            .into_iter()
            .filter(|(p, e)| e.as_ref() != self.entries.get(p))
            .collect())
    }
}
#[derive(Default)]
struct Events {
    paths: BTreeSet<String>,
    recursive: BTreeSet<String>,
    overflow: bool,
    uncertain: bool,
    created: BTreeSet<String>,
    count: usize,
    watches: usize,
}
enum WatchCmd {
    Flush(mpsc::Sender<Result<Events, String>>),
    Count(mpsc::Sender<usize>),
    Stop,
}
struct Watch {
    tx: mpsc::Sender<WatchCmd>,
    wake: i32,
    thread: Option<thread::JoinHandle<()>>,
}
impl Watch {
    fn new(root: PathBuf) -> Result<Self> {
        let fd = unsafe { libc::inotify_init1(libc::IN_NONBLOCK | libc::IN_CLOEXEC) };
        if fd < 0 {
            bail!("inotify 初始化失败：{}", std::io::Error::last_os_error())
        }
        let wake = unsafe { libc::eventfd(0, libc::EFD_CLOEXEC | libc::EFD_NONBLOCK) };
        if wake < 0 {
            bail!("eventfd 初始化失败")
        }
        let (tx, rx) = mpsc::channel();
        let (ready_tx, ready_rx) = mpsc::channel();
        let handle = thread::spawn(move || {
            let mut w = WatchWorker {
                root,
                fd,
                dirs: HashMap::new(),
                events: Events::default(),
                moves: HashMap::new(),
                created_moves: BTreeSet::new(),
            };
            if let Err(e) = w.add("") {
                let _ = ready_tx.send(Err(e.to_string()));
                unsafe {
                    libc::close(fd);
                }
                return;
            }
            let _ = ready_tx.send(Ok(()));
            loop {
                let mut fds = [
                    libc::pollfd {
                        fd,
                        events: libc::POLLIN,
                        revents: 0,
                    },
                    libc::pollfd {
                        fd: wake,
                        events: libc::POLLIN,
                        revents: 0,
                    },
                ];
                unsafe {
                    libc::poll(fds.as_mut_ptr(), 2, -1);
                }
                let res = w.drain();
                if let Err(e) = res {
                    w.events.overflow = true;
                    eprintln!("监听降级：{e}");
                }
                if fds[1].revents != 0 {
                    let mut n = 0u64;
                    unsafe {
                        libc::read(wake, (&mut n as *mut u64).cast(), 8);
                    }
                }
                while let Ok(c) = rx.try_recv() {
                    match c {
                        WatchCmd::Count(reply) => {
                            let _ = reply.send(w.dirs.len());
                        }
                        WatchCmd::Stop => {
                            unsafe {
                                libc::close(fd);
                            }
                            return;
                        }
                        WatchCmd::Flush(reply) => {
                            let r = w.drain();
                            w.events.watches = w.dirs.len();
                            w.events.uncertain |= !w.created_moves.is_empty();
                            w.created_moves.clear();
                            let ev = std::mem::take(&mut w.events);
                            let _ = reply.send(r.map(|_| ev).map_err(|e| e.to_string()));
                        }
                    }
                }
            }
        });
        ready_rx.recv()?.map_err(anyhow::Error::msg)?;
        Ok(Self {
            tx,
            wake,
            thread: Some(handle),
        })
    }
    fn count(&self) -> Result<usize> {
        let (tx, rx) = mpsc::channel();
        self.tx.send(WatchCmd::Count(tx))?;
        self.signal();
        Ok(rx.recv()?)
    }
    fn signal(&self) {
        let n = 1u64;
        unsafe {
            libc::write(self.wake, (&n as *const u64).cast(), 8);
        }
    }
    fn flush(&self) -> Result<Events> {
        let (tx, rx) = mpsc::channel();
        self.tx.send(WatchCmd::Flush(tx))?;
        self.signal();
        rx.recv()?.map_err(anyhow::Error::msg)
    }
}
impl Drop for Watch {
    fn drop(&mut self) {
        let _ = self.tx.send(WatchCmd::Stop);
        self.signal();
        if let Some(h) = self.thread.take() {
            let _ = h.join();
        }
        unsafe {
            libc::close(self.wake);
        }
    }
}
struct WatchWorker {
    root: PathBuf,
    fd: i32,
    dirs: HashMap<i32, String>,
    events: Events,
    moves: HashMap<u32, String>,
    created_moves: BTreeSet<u32>,
}
impl WatchWorker {
    fn add(&mut self, p: &str) -> Result<()> {
        let path = self.root.join(p);
        if !path.is_dir() {
            return Ok(());
        }
        let c = CString::new(path.as_os_str().as_bytes())?;
        let mask = libc::IN_MODIFY
            | libc::IN_CLOSE_WRITE
            | libc::IN_CREATE
            | libc::IN_DELETE
            | libc::IN_MOVED_FROM
            | libc::IN_MOVED_TO
            | libc::IN_ATTRIB
            | libc::IN_DELETE_SELF
            | libc::IN_MOVE_SELF
            | libc::IN_ONLYDIR
            | libc::IN_DONT_FOLLOW;
        let wd = unsafe { libc::inotify_add_watch(self.fd, c.as_ptr(), mask) };
        if wd < 0 {
            let e = std::io::Error::last_os_error();
            if e.kind() == std::io::ErrorKind::NotFound {
                return Ok(());
            }
            return Err(e.into());
        }
        self.dirs.insert(wd, p.into());
        for d in fs::read_dir(path)? {
            let d = d?;
            if !d.file_type()?.is_dir() {
                continue;
            }
            let q = if p.is_empty() {
                d.file_name().to_string_lossy().into_owned()
            } else {
                format!("{p}/{}", d.file_name().to_string_lossy())
            };
            if !excluded(&q) {
                self.add(&q)?;
            }
        }
        Ok(())
    }
    fn drain(&mut self) -> Result<()> {
        let mut buf = [0u8; 65536];
        loop {
            let n = unsafe { libc::read(self.fd, buf.as_mut_ptr().cast(), buf.len()) };
            if n < 0 {
                let e = std::io::Error::last_os_error();
                if e.kind() == std::io::ErrorKind::WouldBlock {
                    break;
                }
                return Err(e.into());
            }
            if n == 0 {
                break;
            }
            let mut off = 0;
            while off < n as usize {
                let e = unsafe {
                    std::ptr::read_unaligned(buf.as_ptr().add(off).cast::<libc::inotify_event>())
                };
                let start = off + std::mem::size_of::<libc::inotify_event>();
                let name = &buf[start..start + e.len as usize];
                let name =
                    String::from_utf8_lossy(name.split(|b| *b == 0).next().unwrap_or_default());
                off = start + e.len as usize;
                if e.mask & libc::IN_Q_OVERFLOW != 0 {
                    self.events.overflow = true;
                    continue;
                }
                if e.mask & libc::IN_IGNORED != 0 {
                    self.dirs.remove(&e.wd);
                    continue;
                }
                let Some(base) = self.dirs.get(&e.wd).cloned() else {
                    continue;
                };
                let p = if name.is_empty() {
                    base
                } else if base.is_empty() {
                    name.into_owned()
                } else {
                    format!("{base}/{name}")
                };
                if excluded(&p) {
                    continue;
                }
                self.events.count += 1;
                if e.mask & libc::IN_CREATE != 0 {
                    self.events.created.insert(p.clone());
                    if e.mask & libc::IN_ISDIR != 0 {
                        self.events.uncertain = true;
                    }
                }
                if e.mask & libc::IN_DELETE != 0 && self.events.created.remove(&p) {
                    self.events.uncertain = true;
                }
                if e.mask & libc::IN_MOVED_FROM != 0 && self.events.created.remove(&p) {
                    self.created_moves.insert(e.cookie);
                }
                if e.mask & libc::IN_MOVED_TO != 0 && self.created_moves.remove(&e.cookie) {
                    self.events.created.insert(p.clone());
                }

                if !p.is_empty() {
                    self.events.paths.insert(p.clone());
                }
                if e.mask & libc::IN_ISDIR != 0
                    || e.mask & (libc::IN_DELETE_SELF | libc::IN_MOVE_SELF) != 0
                {
                    if !p.is_empty() {
                        self.events.recursive.insert(p.clone());
                    }
                    if e.mask & libc::IN_MOVED_FROM != 0 {
                        self.moves.insert(e.cookie, p.clone());
                    }
                    if e.mask & libc::IN_MOVED_TO != 0 {
                        if let Some(old) = self.moves.remove(&e.cookie) {
                            self.events.created = self
                                .events
                                .created
                                .iter()
                                .map(|q| {
                                    if q.starts_with(&format!("{old}/")) {
                                        format!("{p}{}", &q[old.len()..])
                                    } else {
                                        q.clone()
                                    }
                                })
                                .collect();
                            for q in self.dirs.values_mut() {
                                if *q == old {
                                    *q = p.clone()
                                } else if q.starts_with(&format!("{old}/")) {
                                    *q = format!("{p}{}", &q[old.len()..]);
                                }
                            }
                        }
                    }
                    if e.mask & (libc::IN_CREATE | libc::IN_MOVED_TO) != 0 {
                        self.add(&p)?;
                    }
                }
            }
        }
        if self.events.overflow {
            self.add("")?;
        }
        Ok(())
    }
}
fn barrier(p: &Path) -> Result<()> {
    let f = File::open(p)?;
    if unsafe { libc::syncfs(f.as_raw_fd()) } != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(())
}
fn save(p: &Path, value: &impl Serialize) -> Result<()> {
    let tmp = p.with_extension("tmp-v5");
    let mut f = File::create(&tmp)?;
    let bytes = serde_json::to_vec(value)?;
    f.write_all(&bytes)?;
    f.sync_all()?;
    fs::rename(&tmp, p)?;
    File::open(p.parent().unwrap())?.sync_all()?;
    Ok(())
}
fn clone_file(src: &Path, dst: &Path) -> Result<()> {
    let a = File::open(src)?;
    let b = File::create(dst)?;
    if unsafe { libc::ioctl(b.as_raw_fd(), 0x40049409u64, a.as_raw_fd()) } != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(())
}
fn meta(p: &Path, e: &Entry) -> Result<()> {
    if e.kind != "symlink" {
        fs::set_permissions(p, fs::Permissions::from_mode(e.mode))?;
    }
    let c = CString::new(p.as_os_str().as_bytes())?;
    let ts = [
        libc::timespec {
            tv_sec: 0,
            tv_nsec: libc::UTIME_OMIT,
        },
        libc::timespec {
            tv_sec: e.mt,
            tv_nsec: e.mn,
        },
    ];
    if unsafe {
        libc::utimensat(
            libc::AT_FDCWD,
            c.as_ptr(),
            ts.as_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    } != 0
    {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(())
}
fn remove(p: &Path) -> Result<()> {
    match fs::symlink_metadata(p) {
        Ok(m) => {
            if m.is_dir() {
                fs::remove_dir(p)?
            } else {
                fs::remove_file(p)?
            }
        }
        Err(e) if matches!(e.raw_os_error(), Some(libc::ENOENT | libc::ENOTDIR)) => {}
        Err(e) => return Err(e.into()),
    }
    Ok(())
}
fn point(name: &str) {
    if std::env::var("S2_KILL_AT").ok().as_deref() == Some(name) {
        unsafe {
            libc::kill(libc::getpid(), libc::SIGKILL);
        }
    }
}
fn inject(root: &Path, name: &str, p: &str) {
    if std::env::var("S2_INJECT_AT").ok().as_deref() == Some(name) {
        let q = std::env::var("S2_INJECT_PATH").unwrap_or_else(|_| "a".into());
        if q == p {
            fs::write(root.join(p), b"external-window-v5").unwrap();
        }
    }
}
// 所有物化共用一个实现；guard 在每条实际破坏操作之前执行。
thread_local! { static WINDOWS: std::cell::RefCell<Vec<u64>> = const { std::cell::RefCell::new(Vec::new()) }; static CHECKED_AT: std::cell::Cell<Option<Instant>> = const { std::cell::Cell::new(None) }; }
fn materialize(
    root: &Path,
    desired: &Entries,
    paths: &BTreeSet<String>,
    source: &impl Fn(&str) -> PathBuf,
    anchors: &BTreeMap<String, PathBuf>,
    inplace: &BTreeSet<String>,
    mut guard: impl FnMut(&str) -> Result<()>,
    checkpoints: bool,
) -> Result<usize> {
    let mut ordered: Vec<_> = paths.iter().cloned().collect();
    ordered.sort_by_key(|p| (std::cmp::Reverse(p.matches('/').count()), p.clone()));
    let mut checked = BTreeSet::new();
    for p in &ordered {
        let now = inspect(&root.join(p), None)?;
        let want = desired.get(p);
        if now.is_some() && (want.is_none() || now.as_ref().unwrap().kind != want.unwrap().kind) {
            guard(p)?;
            checked.insert(p.clone());
            remove(&root.join(p))?;
        }
    }
    ordered.sort_by_key(|p| (p.matches('/').count(), p.clone()));
    let mut applied = 0;
    for p in &ordered {
        let Some(e) = desired.get(p) else { continue };
        let dst = root.join(p);
        fs::create_dir_all(dst.parent().unwrap())?;
        if e.kind == "file" {
            let leader = e
                .links
                .iter()
                .find(|q| !paths.contains(*q))
                .or_else(|| e.links.iter().min_by_key(|q| (q.matches('/').count(), *q)))
                .unwrap_or(p);
            if let Some(anchor) = anchors.get(p) {
                if !checked.contains(p) {
                    guard(p)?;
                }
                if fs::metadata(&dst).map(|m| m.ino()).ok()
                    != fs::metadata(anchor).map(|m| m.ino()).ok()
                {
                    remove(&dst)?;
                    fs::hard_link(anchor, &dst)?;
                }
                if p == leader {
                    fs::set_permissions(&dst, fs::Permissions::from_mode(e.mode | 0o200))?;
                    fs::copy(source(p), &dst)?;
                }
            } else if p != leader {
                if !checked.contains(p) {
                    guard(p)?;
                }
                remove(&dst)?;
                fs::hard_link(root.join(leader), &dst)?;
            } else if inplace.contains(p) {
                if !checked.contains(p) {
                    guard(p)?;
                }
                fs::set_permissions(&dst, fs::Permissions::from_mode(e.mode | 0o200))?;
                let mut a = File::open(source(p))?;
                let mut b = File::create(&dst)?;
                let mut byte = [0];
                let n = a.read(&mut byte)?;
                b.write_all(&byte[..n])?;
                if checkpoints {
                    point("inplace-partial");
                }
                std::io::copy(&mut a, &mut b)?;
            } else {
                let tmp = dst.with_file_name(format!(
                    ".s2-v5-{}",
                    dst.file_name().unwrap().to_string_lossy()
                ));
                clone_file(&source(p), &tmp)?;
                meta(&tmp, e)?;
                if !checked.contains(p) {
                    if let Err(e) = guard(p) {
                        fs::remove_file(&tmp).context("冲突临时文件清理失败")?;
                        return Err(e);
                    }
                }
                fs::rename(&tmp, &dst)?;
                CHECKED_AT.with(|t| {
                    if let Some(start) = t.take() {
                        WINDOWS.with(|v| v.borrow_mut().push(start.elapsed().as_nanos() as u64));
                    }
                });
            }
        } else {
            if !checked.contains(p) {
                guard(p)?;
            }
            match e.kind.as_str() {
                "directory" => {
                    fs::create_dir_all(&dst)?;
                    fs::set_permissions(&dst, fs::Permissions::from_mode(e.mode | 0o700))?;
                }
                "symlink" => {
                    remove(&dst)?;
                    symlink(&e.link, &dst)?;
                }
                "fifo" => {
                    remove(&dst)?;
                    let c = CString::new(dst.as_os_str().as_bytes())?;
                    if unsafe { libc::mkfifo(c.as_ptr(), e.mode) } != 0 {
                        return Err(std::io::Error::last_os_error().into());
                    }
                }
                _ => bail!("未支持的对象类型：{}", e.kind),
            }
        }
        applied += 1;
        if checkpoints {
            point(&format!("apply-{applied}"));
        }
    }
    for p in ordered.iter().rev() {
        if let Some(e) = desired.get(p) {
            meta(&root.join(p), e)?;
        }
    }
    Ok(applied)
}
#[derive(Serialize, Deserialize, Clone, Debug)]
struct Rename {
    from: String,
    to: String,
}
#[derive(Serialize, Deserialize)]
struct Transaction {
    phase: String,
    real: PathBuf,
    paths: BTreeSet<String>,
    before: Entries,
    renames: Vec<Rename>,
    post: Entries,
    #[serde(default)]
    anchors: BTreeMap<String, PathBuf>,
}
fn mapped(p: &str, renames: &[Rename], reverse: bool) -> String {
    for r in renames {
        let (a, b) = if reverse {
            (&r.to, &r.from)
        } else {
            (&r.from, &r.to)
        };
        if p == a {
            return b.clone();
        }
        if p.starts_with(&format!("{a}/")) {
            return format!("{b}{}", &p[a.len()..]);
        }
    }
    p.into()
}
fn map_entry(e: &Entry, renames: &[Rename]) -> Entry {
    let mut e = e.clone();
    e.links = e.links.iter().map(|p| mapped(p, renames, false)).collect();
    e
}
fn recover(txn: &Path, revert: bool) -> Result<Value> {
    let mut t: Transaction = serde_json::from_reader(File::open(txn.join("transaction.json"))?)?;
    if t.phase == "complete" && !revert {
        return Ok(json!({"outcome":"Committed"}));
    }
    if t.phase == "rolled-back" {
        return Ok(json!({"outcome":"Restored"}));
    }
    if t.phase == "complete" {
        let mut conflicts = vec![];
        for r in &t.renames {
            if t.real.join(&r.from).exists() {
                conflicts.push(r.from.clone());
            }
        }
        for (p, e) in &t.post {
            if !signature(Some(e), inspect(&t.real.join(p), Some(e))?.as_ref()) {
                conflicts.push(p.clone());
            }
        }
        for p in &t.paths {
            if !t.post.contains_key(p) && t.real.join(p).exists() {
                conflicts.push(p.clone());
            }
        }
        if !conflicts.is_empty() {
            return Ok(json!({"outcome":"Conflict","conflicts":conflicts}));
        }
    }
    if t.phase == "ready" || t.phase == "complete" {
        for r in t.renames.iter().rev() {
            if t.real.join(&r.to).exists() && !t.real.join(&r.from).exists() {
                fs::rename(t.real.join(&r.to), t.real.join(&r.from))?;
            }
        }
        for p in &t.paths {
            for d in [
                t.real.join(p),
                t.real.join(p).parent().unwrap().to_path_buf(),
            ] {
                if let Ok(m) = fs::symlink_metadata(&d) {
                    if m.is_dir() {
                        fs::set_permissions(&d, fs::Permissions::from_mode(m.mode() | 0o700))?;
                    }
                }
            }
        }
        materialize(
            &t.real,
            &t.before,
            &t.paths,
            &|p| txn.join("pre").join(p),
            &t.anchors,
            &BTreeSet::new(),
            |_| Ok(()),
            false,
        )?;
        barrier(&t.real)?;
    }
    for a in t.anchors.values() {
        if a.exists() {
            fs::remove_file(a)?;
        }
    }
    for p in &t.paths {
        let dst = t.real.join(p);
        let tmp = dst.with_file_name(format!(
            ".s2-v5-{}",
            dst.file_name().unwrap().to_string_lossy()
        ));
        let _ = fs::remove_file(tmp);
    }
    t.phase = "rolled-back".into();
    save(&txn.join("transaction.json"), &t)?;
    Ok(json!({"outcome":"Restored"}))
}
fn git_entries(root: &Path) -> Result<Entries> {
    let mut out = Entries::new();
    let git = root.join(".git");
    if !git.is_dir() {
        return Ok(out);
    }
    for p in ["HEAD", "index", "packed-refs"] {
        if let Some(e) = inspect(&git.join(p), None)? {
            out.insert(p.into(), e);
        }
    }
    fn descend(root: &Path, p: &str, out: &mut Entries) -> Result<()> {
        if !root.join(p).is_dir() {
            return Ok(());
        }
        for d in fs::read_dir(root.join(p))? {
            let d = d?;
            let q = format!("{p}/{}", d.file_name().to_string_lossy());
            if d.file_type()?.is_dir() {
                descend(root, &q, out)?
            } else if !q.ends_with(".lock") {
                if let Some(e) = inspect(&root.join(&q), None)? {
                    out.insert(q, e);
                }
            }
        }
        Ok(())
    }
    descend(&git, "refs", &mut out)?;
    descend(&git, "logs", &mut out)?;
    Ok(out)
}
struct Node {
    real: PathBuf,
    state: PathBuf,
    shadow: PathBuf,
    real_tree: Tree,
    shadow_tree: Tree,
    rw: Watch,
    sw: Watch,
    index: File,
    _lock: File,
    before_delta: BTreeMap<String, Option<Entry>>,
    paths: BTreeSet<String>,
    renames: Vec<Rename>,
    git_before: Entries,
    git_after: Entries,
    active: bool,
}
impl Node {
    fn new(real: PathBuf, state: PathBuf) -> Result<(Self, Value)> {
        let start = Instant::now();
        fs::create_dir_all(&state)?;
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(state.join("lock"))?;
        if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            bail!("已有节点持有该状态目录")
        }
        if real.metadata()?.dev() != state.metadata()?.dev() {
            bail!("状态目录与真实目录不在同一文件系统")
        }
        let txns = state.join("transactions");
        fs::create_dir_all(&txns)?;
        for d in fs::read_dir(&txns)? {
            let d = d?;
            if d.path().join("transaction.json").exists() {
                recover(&d.path(), false)?;
            }
        }
        let shadow = state.join("shadow");
        fs::create_dir_all(&shadow)?;
        for p in EXCLUDED {
            fs::create_dir_all(shadow.join(p))?;
        }
        for p in ["pass-0", "pass-1"] {
            fs::create_dir_all(state.join(p))?;
        }
        let rw = Watch::new(real.clone())?;
        let sw = Watch::new(shadow.clone())?;
        let real_tree = Tree::scan(&real, &Entries::new())?;
        let current = Tree::scan(&shadow, &Entries::new())?;
        let paths: BTreeSet<_> = real_tree
            .entries
            .keys()
            .chain(current.entries.keys())
            .filter(|p| !semantic(real_tree.entries.get(*p), current.entries.get(*p)))
            .cloned()
            .collect();
        barrier(&real)?;
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
        let shadow_tree = Tree::scan(&shadow, &Entries::new())?;
        // 启动全扫是恢复边界；索引日志在此压缩，act 期间只追加实际改动条目。
        let index = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(state.join("index-v5.jsonl"))?;
        let mut n = Self {
            real,
            state,
            shadow,
            real_tree,
            shadow_tree,
            rw,
            sw,
            index,
            _lock: lock,
            before_delta: BTreeMap::new(),
            paths: BTreeSet::new(),
            renames: vec![],
            git_before: Entries::new(),
            git_after: Entries::new(),
            active: false,
        };
        let paths = n.real_tree.entries.keys().cloned().collect();
        n.persist(&paths)?;
        let r = n.rw.flush()?;
        let s = n.sw.flush()?;
        let git_recovery = if state_git_active(&n.state) {
            "Indeterminate"
        } else {
            "None"
        };
        let info = json!({"git_recovery":git_recovery,"init_s":start.elapsed().as_secs_f64(),"entries":n.real_tree.entries.len(),"real_watches":r.watches,"shadow_watches":s.watches,"pid":std::process::id()});
        Ok((n, info))
    }
    fn persist(&mut self, paths: &BTreeSet<String>) -> Result<()> {
        if paths.is_empty() {
            return Ok(());
        }
        let mut buf = Vec::new();
        for p in paths {
            serde_json::to_writer(
                &mut buf,
                &json!({"path":p,"real":self.real_tree.entries.get(p),"shadow":self.shadow_tree.entries.get(p)}),
            )?;
            buf.push(b'\n');
        }
        self.index.write_all(&buf)?;
        self.index.sync_data()?;
        Ok(())
    }
    fn begin(&mut self, full: bool) -> Result<Value> {
        if self.active {
            bail!("已有 act 尚未结算")
        }
        let start = Instant::now();
        let mut ev = self.rw.flush()?;
        ev.overflow |= full;
        let count = ev.count;
        let overflow = ev.overflow;
        let uncertain = ev.uncertain;
        let watches = ev.watches;
        let changes = self.real_tree.update(&self.real, ev)?;
        let mut paths: BTreeSet<_> = changes.keys().cloned().collect();
        for p in changes.keys() {
            if let Some(e) = self.real_tree.entries.get(p) {
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
        }
        let ev = self.sw.flush()?;
        self.shadow_tree.update(&self.shadow, ev)?;
        self.persist(&paths)?;
        self.before_delta.clear();
        self.paths.clear();
        self.renames.clear();
        let git_start = Instant::now();
        self.git_before = git_entries(&self.real)?;
        let snap = self.state.join("git-before-v5");
        if snap.exists() {
            fs::remove_dir_all(&snap)?;
        }
        fs::create_dir_all(&snap)?;
        for p in self.git_before.keys() {
            let dst = snap.join(p);
            fs::create_dir_all(dst.parent().unwrap())?;
            clone_file(&self.real.join(".git").join(p), &dst)?;
        }
        // Git 前态也持久化；重启不盲目回滚缺失结束摘要的共享 Git 状态。
        if !self.git_before.is_empty() {
            save(
                &self.state.join("git-act-v5.json"),
                &json!({"phase":"active","before":self.git_before}),
            )?;
            barrier(&snap)?;
        }
        self.active = true;
        Ok(
            json!({"sync_s":start.elapsed().as_secs_f64(),"git_snapshot_s":git_start.elapsed().as_secs_f64(),"synced":paths.len(),"events":count,"overflow":overflow,"uncertain_history":uncertain,"watches":watches,"entries":self.real_tree.entries.len()}),
        )
    }
    fn finish(&mut self, audit: bool, force_overflow: bool) -> Result<Value> {
        let start = Instant::now();
        let mut ev = self.sw.flush()?;
        ev.overflow |= force_overflow;
        let count = ev.count;
        let overflow = ev.overflow;
        let uncertain = ev.uncertain;
        let watches = ev.watches;
        self.before_delta = self.shadow_tree.update(&self.shadow, ev)?;
        self.paths = self
            .before_delta
            .iter()
            .filter(|(p, b)| !semantic(b.as_ref(), self.shadow_tree.entries.get(*p)))
            .map(|(p, _)| p.clone())
            .collect();
        let old_dirs: HashMap<_, _> = self
            .before_delta
            .iter()
            .filter_map(|(p, b)| {
                b.as_ref()
                    .filter(|e| e.kind == "directory")
                    .map(|e| ((e.dev, e.ino), p.clone()))
            })
            .collect();
        let mut moves = vec![];
        for p in &self.paths {
            if let Some(e) = self.shadow_tree.entries.get(p) {
                if e.kind == "directory" {
                    if let Some(old) = old_dirs.get(&(e.dev, e.ino)) {
                        if old != p
                            && !self.shadow_tree.entries.contains_key(old)
                            && !self.real_tree.entries.contains_key(p)
                        {
                            moves.push(Rename {
                                from: old.clone(),
                                to: p.clone(),
                            });
                        }
                    }
                }
            }
        }
        moves.sort_by_key(|r| r.from.len());
        self.renames.clear();
        for r in moves {
            if !self
                .renames
                .iter()
                .any(|x| r.from.starts_with(&format!("{}/", x.from)))
            {
                self.renames.push(r);
            }
        }
        self.git_after = git_entries(&self.real)?;
        let scan_s = start.elapsed().as_secs_f64();
        let mut out = json!({"scan_s":scan_s,"events":count,"overflow":overflow,"uncertain_history":uncertain,"watches":watches,"epsilon":{"paths":self.paths,"directory_renames":self.renames}});
        if audit {
            let audit_start = Instant::now();
            let full = Tree::scan(&self.shadow, &Entries::new())?;
            let keys: BTreeSet<_> = full
                .entries
                .keys()
                .chain(self.shadow_tree.entries.keys())
                .chain(self.before_delta.keys())
                .cloned()
                .collect();
            let mut full_delta = vec![];
            let mut mismatch = vec![];
            for p in keys {
                let before = if let Some(b) = self.before_delta.get(&p) {
                    b.as_ref()
                } else {
                    self.shadow_tree.entries.get(&p)
                };
                if !semantic(before, full.entries.get(&p)) {
                    full_delta.push(p.clone());
                }
                if !semantic(self.shadow_tree.entries.get(&p), full.entries.get(&p)) {
                    mismatch.push(p);
                }
            }
            out["audit"] = json!({"exact":full_delta.iter().cloned().collect::<BTreeSet<_>>()==self.paths,"full_paths":full_delta,"missed_state":mismatch,"full_scan_s":audit_start.elapsed().as_secs_f64()});
        }
        Ok(out)
    }
    fn git_discard(&self) -> Result<Value> {
        let start = Instant::now();
        let now = git_entries(&self.real)?;
        let paths: BTreeSet<_> = self
            .git_before
            .keys()
            .chain(self.git_after.keys())
            .cloned()
            .collect();
        let paths: BTreeSet<_> = paths
            .into_iter()
            .filter(|p| !semantic(self.git_before.get(p), self.git_after.get(p)))
            .collect();
        let mut conflicts = vec![];
        let mut restored = vec![];
        let guarded_paths: BTreeSet<_> = self
            .git_before
            .keys()
            .chain(self.git_after.keys())
            .chain(now.keys())
            .cloned()
            .collect();
        // 先获取 Git 使用的 lockfile；冲突时整组保留，防止 HEAD 与 index 撕裂。
        let mut locks = vec![];
        for p in &guarded_paths {
            if !signature(self.git_after.get(p), now.get(p)) {
                conflicts.push(p.clone());
            }
            let lock = self.real.join(".git").join(format!("{p}.lock"));
            fs::create_dir_all(lock.parent().unwrap())?;
            match OpenOptions::new().write(true).create_new(true).open(&lock) {
                Ok(f) => {
                    locks.push((lock, f));
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    conflicts.push(p.clone())
                }
                Err(e) => return Err(e.into()),
            }
        }
        if conflicts.is_empty() {
            let again = git_entries(&self.real)?;
            for p in &guarded_paths {
                if !signature(self.git_after.get(p), again.get(p)) {
                    conflicts.push(p.clone());
                }
            }
        }
        if conflicts.is_empty() {
            for p in &paths {
                let dst = self.real.join(".git").join(p);
                if let Some(e) = self.git_before.get(p) {
                    let tmp = dst.with_extension("restore-v5");
                    clone_file(&self.state.join("git-before-v5").join(p), &tmp)?;
                    meta(&tmp, e)?;
                    fs::rename(tmp, &dst)?;
                } else {
                    remove(&dst)?;
                }
                restored.push(p.clone());
            }
            if !paths.is_empty() {
                barrier(&self.real)?;
            }
        }
        for (lock, _) in locks {
            fs::remove_file(lock)?;
        }
        Ok(
            json!({"conflicts":conflicts,"restored":restored,"git_restore_s":start.elapsed().as_secs_f64()}),
        )
    }
    fn discard(&mut self) -> Result<Value> {
        let start = Instant::now();
        let git = self.git_discard()?;
        let ev = self.rw.flush()?;
        let changed = self.real_tree.update(&self.real, ev)?;
        let mut paths = self.paths.clone();
        paths.extend(changed.keys().cloned());
        // 丢弃恢复最新真实字节；第四轮已明确这不是不可变历史影子的保证。
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
        self.shadow_tree.update(&self.shadow, self.sw.flush()?)?;
        self.persist(&paths)?;
        self.settle_git()?;
        self.active = false;
        Ok(
            json!({"outcome":if git["conflicts"].as_array().is_some_and(|x|!x.is_empty()){"Conflict"}else{"discarded"},"git":git,"discard_s":start.elapsed().as_secs_f64()}),
        )
    }
    fn settle_git(&self) -> Result<()> {
        if !self.git_before.is_empty() {
            save(
                &self.state.join("git-act-v5.json"),
                &json!({"phase":"settled"}),
            )?;
        }
        Ok(())
    }
    fn commit(&mut self) -> Result<Value> {
        WINDOWS.with(|v| v.borrow_mut().clear());
        let start = Instant::now();
        if self.paths.is_empty() {
            self.settle_git()?;
            self.active = false;
            return Ok(
                json!({"outcome":"Unchanged","commit_s":0.,"prepare_s":0.,"apply_s":0.,"conflicts":[]}),
            );
        }
        let mut conflicts = vec![];
        for p in &self.paths {
            if !signature(
                self.real_tree.entries.get(p),
                inspect(&self.real.join(p), self.real_tree.entries.get(p))?.as_ref(),
            ) {
                conflicts.push(p.clone());
            }
        }
        if !conflicts.is_empty() {
            return Ok(
                json!({"outcome":"Conflict","conflicts":conflicts,"commit_s":start.elapsed().as_secs_f64()}),
            );
        }
        let txn = self.state.join("transactions").join(format!(
            "{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_nanos()
        ));
        fs::create_dir_all(txn.join("pre"))?;
        let mut workpaths = self.paths.clone();
        for r in &self.renames {
            workpaths.retain(|p| {
                p != &r.from
                    && p != &r.to
                    && !p.starts_with(&format!("{}/", r.from))
                    && !p.starts_with(&format!("{}/", r.to))
            });
            let old = std::iter::once(r.from.clone())
                .chain(descendants(&self.real_tree.entries, &r.from));
            let new =
                std::iter::once(r.to.clone()).chain(descendants(&self.shadow_tree.entries, &r.to));
            let names: BTreeSet<_> = old
                .map(|p| mapped(&p, &self.renames, false))
                .chain(new)
                .collect();
            for p in names {
                let old = mapped(&p, &self.renames, true);
                let b = self
                    .real_tree
                    .entries
                    .get(&old)
                    .map(|e| map_entry(e, &self.renames));
                if !semantic(b.as_ref(), self.shadow_tree.entries.get(&p)) {
                    workpaths.insert(p);
                }
            }
        }
        let restore_paths: BTreeSet<_> = workpaths
            .iter()
            .map(|p| mapped(p, &self.renames, true))
            .collect();
        let before: Entries = restore_paths
            .iter()
            .filter_map(|p| {
                self.real_tree
                    .entries
                    .get(p)
                    .map(|e| (p.clone(), e.clone()))
            })
            .collect();
        let mut t = Transaction {
            phase: "preparing".into(),
            real: self.real.clone(),
            paths: restore_paths,
            before,
            renames: self.renames.clone(),
            post: Entries::new(),
            anchors: BTreeMap::new(),
        };
        point("before-log");
        save(&txn.join("transaction.json"), &t)?;
        point("after-log");
        // 源文件先批量落盘，避免每个 FICLONE 各自等待脏数据。
        barrier(&self.shadow)?;
        for (p, e) in &t.before {
            if e.kind == "file" {
                let dst = txn.join("pre").join(p);
                fs::create_dir_all(dst.parent().unwrap())?;
                clone_file(&self.real.join(p), &dst)?;
                if e.nlink > 1 {
                    let a = txn.join(format!("{}-{}.anchor", e.dev, e.ino));
                    if !a.exists() {
                        fs::hard_link(self.real.join(p), &a)?;
                    }
                    t.anchors.insert(p.clone(), a);
                }
            }
        }
        save(&txn.join("transaction.json"), &t)?;
        point("before-pre-sync");
        barrier(&txn)?;
        point("after-pre-sync");
        t.phase = "ready".into();
        save(&txn.join("transaction.json"), &t)?;
        point("after-ready");
        inject(&self.real, "before-expected", "a");
        let mut expected = BTreeMap::new();
        for p in &workpaths {
            let old = mapped(p, &self.renames, true);
            let base = self.real_tree.entries.get(&old).cloned();
            // 锚点改变 nlink/ctime；只补偿本事务创建的链接，内容仍必须等于 act 基线。
            let expected_entry = if t.anchors.contains_key(&old) {
                let now = inspect(&self.real.join(&old), None)?;
                if base.as_ref().zip(now.as_ref()).is_some_and(|(a, b)| {
                    let mut adjusted = a.clone();
                    adjusted.ct = b.ct;
                    adjusted.cn = b.cn;
                    adjusted.nlink += 1;
                    !adjusted.signature(b) || a.hash != b.hash
                }) {
                    return Ok(json!({"outcome":"Conflict","conflicts":[old]}));
                }
                now
            } else {
                base
            };
            expected.insert(p.clone(), expected_entry);
        }
        let anchor_expected: HashMap<_, _> = expected
            .values()
            .flatten()
            .filter(|e| {
                t.anchors.keys().any(|p| {
                    t.before
                        .get(p)
                        .is_some_and(|b| b.dev == e.dev && b.ino == e.ino)
                })
            })
            .map(|e| ((e.dev, e.ino), e.clone()))
            .collect();
        let prepare_s = start.elapsed().as_secs_f64();
        inject(&self.real, "after-ready", "a");
        let mut rename_count = 0;
        for r in &self.renames {
            let mut check = vec![r.from.clone()];
            check.extend(descendants(&self.real_tree.entries, &r.from));
            for p in check {
                let original = self.real_tree.entries.get(&p);
                let expected = original
                    .and_then(|e| anchor_expected.get(&(e.dev, e.ino)))
                    .or(original);
                if !signature(expected, inspect(&self.real.join(&p), expected)?.as_ref()) {
                    return Ok(
                        json!({"outcome":"Conflict","conflicts":[p],"transaction":txn,"commit_s":start.elapsed().as_secs_f64()}),
                    );
                }
            }
            if self.real.join(&r.to).exists() {
                bail!("改名目的路径被外部创建")
            }
            fs::rename(self.real.join(&r.from), self.real.join(&r.to))?;
            rename_count += 1;
            point("after-rename");
        }
        let mut guarded = BTreeSet::new();
        let mut touched = BTreeSet::new();
        let mut windows = vec![];
        let mut last_check: Option<Instant> = None;
        let inplace: BTreeSet<_> = workpaths
            .iter()
            .filter(|p| {
                let old = mapped(p, &self.renames, true);
                self.real_tree
                    .entries
                    .get(&old)
                    .is_some_and(|e| e.nlink > 1)
                    && self
                        .before_delta
                        .get(&old)
                        .and_then(|b| b.as_ref())
                        .zip(self.shadow_tree.entries.get(*p))
                        .is_some_and(|(b, e)| b.ino == e.ino)
            })
            .cloned()
            .collect();
        let real = &self.real;
        let applied = materialize(
            real,
            &self.shadow_tree.entries,
            &workpaths,
            &|p| self.shadow.join(p),
            &BTreeMap::new(),
            &inplace,
            |p| {
                if let Some(last) = last_check.take() {
                    windows.push(last.elapsed().as_nanos() as u64);
                }
                CHECKED_AT.with(|t| t.set(Some(Instant::now())));
                let old = expected.get(p).and_then(|e| e.as_ref());
                let key = old.map(|e| (e.dev, e.ino));
                if !key.is_some_and(|k| guarded.contains(&k)) {
                    let now = inspect(&real.join(p), old)?;
                    let equal = if let (Some(b), Some(a)) = (old, now.as_ref()) {
                        if b.kind == "directory" {
                            b.kind == a.kind && b.mode == a.mode && b.ino == a.ino
                        } else {
                            b.signature(a)
                        }
                    } else {
                        old.is_none() && now.is_none()
                    };
                    if !equal {
                        bail!("逐文件冲突：{p}")
                    }
                }
                inject(real, "after-check", p);
                if let Some(k) = key {
                    guarded.insert(k);
                }
                touched.insert(mapped(p, &self.renames, true));
                last_check = Some(Instant::now());
                Ok(())
            },
            true,
        );
        // syscall 窗口由单独探针准确测量；此值是含循环工作的上界。
        if let Some(last) = last_check {
            windows.push(last.elapsed().as_nanos() as u64);
        }
        if let Err(e) = applied {
            t.paths = touched;
            t.before.retain(|p, _| t.paths.contains(p));
            save(&txn.join("transaction.json"), &t)?;
            let _ = recover(&txn, false)?;
            return Ok(
                json!({"outcome":"Conflict","error":e.to_string(),"commit_s":start.elapsed().as_secs_f64(),"transaction":txn}),
            );
        }
        point("before-post-sync");
        barrier(real)?;
        point("after-post-sync");
        for p in &self.paths {
            if let Some(e) = inspect(&real.join(p), None)? {
                t.post.insert(p.clone(), e);
            }
        }
        t.phase = "complete".into();
        save(&txn.join("transaction.json"), &t)?;
        point("after-complete");
        // 仅更新本批路径，运行中其他真实改动仍留给下一次 begin。
        let ev = Events {
            paths: self.paths.clone(),
            ..Events::default()
        };
        self.real_tree.update(&self.real, ev)?;
        let paths = self.paths.clone();
        self.persist(&paths)?;
        self.settle_git()?;
        self.active = false;
        Ok(
            json!({"outcome":"Committed","transaction":txn,"commit_s":start.elapsed().as_secs_f64(),"prepare_s":prepare_s,"apply_s":start.elapsed().as_secs_f64()-prepare_s,"applied":applied.unwrap(),"rename_syscalls":rename_count,"check_to_rename_ns":WINDOWS.with(|v|v.borrow().clone()),"guard_interval_upper_ns":windows,"conflicts":[]}),
        )
    }
}
fn state_git_active(state: &Path) -> bool {
    fs::read(state.join("git-act-v5.json"))
        .ok()
        .and_then(|s| serde_json::from_slice::<Value>(&s).ok())
        .is_some_and(|v| v["phase"] == "active")
}
fn main() -> Result<()> {
    let args: Vec<_> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("recover") {
        let out = recover(Path::new(&args[2]), args.iter().any(|a| a == "--revert"))?;
        println!("{out}");
        return Ok(());
    }
    if args.len() != 3 {
        bail!("用法：shadow-v5 REAL STATE；或 recover TRANSACTION [--revert]")
    }
    let (n, init) =
        Node::new(PathBuf::from(&args[1]), PathBuf::from(&args[2])).context("节点启动失败")?;
    let mut n = n;
    println!("{init}");
    for line in std::io::stdin().lock().lines() {
        let line = line?;
        let req: Value = serde_json::from_str(&line)?;
        let result: Result<Value> = match req["op"].as_str().unwrap_or("") {
            "begin" => n.begin(req["full"].as_bool().unwrap_or(false)),
            "finish" => n.finish(
                req["audit"].as_bool().unwrap_or(false),
                req["overflow"].as_bool().unwrap_or(false),
            ),
            "commit" => n.commit(),
            "discard" => n.discard(),
            "env" => {
                if let Some(values) = req["values"].as_object() {
                    for (k, v) in values {
                        unsafe {
                            if let Some(v) = v.as_str() {
                                std::env::set_var(k, v)
                            } else {
                                std::env::remove_var(k)
                            }
                        }
                    }
                }
                Ok(json!({"ok":true}))
            }
            "stats" => Ok(
                json!({"real_watches":n.rw.count()?,"shadow_watches":n.sw.count()?,"entries":n.real_tree.entries.len(),"status":fs::read_to_string("/proc/self/status")?}),
            ),
            "stop" => break,
            _ => Err(anyhow::anyhow!("未知操作")),
        };
        match result {
            Ok(out) => println!("{out}"),
            Err(e) => println!("{}", json!({"error":format!("{e:#}")})),
        };
        std::io::stdout().flush()?;
    }
    Ok(())
}
