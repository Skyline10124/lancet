// S3 研究原型；基础对象模型、目录监听与影子物化源自第五轮。
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
const EXCLUDED: [&str; 2] = [".git", "target"];
type Entries = BTreeMap<String, Entry>;
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, Default)]
struct Entry {
    #[serde(default)]
    owned: bool,
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
#[derive(Default, Clone)]
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
    fn patch(&mut self, delta: &BTreeMap<String, Option<Entry>>) {
        for (p, e) in delta {
            if let Some(old) = self.entries.remove(p) {
                let key = (old.dev, old.ino);
                if let Some(g) = self.groups.get_mut(&key) {
                    g.remove(p);
                    if g.is_empty() {
                        self.groups.remove(&key);
                    }
                }
            }
            if let Some(e) = e {
                if e.kind == "file" {
                    self.groups
                        .entry((e.dev, e.ino))
                        .or_default()
                        .insert(p.clone());
                }
                self.entries.insert(p.clone(), e.clone());
            }
        }
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
            .filter(|(p, e)| {
                !signature(e.as_ref(), self.entries.get(p))
                    || !semantic(e.as_ref(), self.entries.get(p))
            })
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
                if excluded(&p) || internal(&p) {
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
    let tmp = p.with_extension("tmp-v7");
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
    if std::env::var("S3_KILL_AT").ok().as_deref() == Some(name) {
        unsafe {
            libc::kill(libc::getpid(), libc::SIGKILL);
        }
    }
}
fn inject(root: &Path, name: &str, p: &str) {
    if std::env::var("S3_INJECT_AT").ok().as_deref() == Some(name) {
        let q = std::env::var("S3_INJECT_PATH").unwrap_or_else(|_| "a".into());
        if q == p {
            fs::write(root.join(p), b"external-window-v7").unwrap();
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
                    ".s2-v7-{}",
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
