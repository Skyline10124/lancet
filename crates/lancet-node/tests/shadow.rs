// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use std::collections::BTreeMap;
use std::ffi::{CString, OsStr, c_int, c_long, c_void};
use std::fs;
use std::io::{Read as _, Write as _};
use std::os::fd::{AsFd as _, AsRawFd as _, OwnedFd};
use std::os::unix::ffi::{OsStrExt as _, OsStringExt as _};
use std::os::unix::fs::{FileTypeExt as _, MetadataExt as _, PermissionsExt as _, symlink};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use lancet_node::shadow::publish::{
    ConflictReason, PublishFinish, PublishJournal, PublishOutcome, PublishPrep, RecoveryRoot,
    call_record_hash, prepare_publish, recover_journal,
};
use lancet_node::shadow::{DiscardError, PendingAction, ShadowDir};
use rustix::fs::{
    AtFlags, CWD, FallocateFlags, Mode, OFlags, RenameFlags, ResolveFlags, Timespec, Timestamps,
    chmodat, fchmod, ftruncate, linkat, mkdirat, mknodat, openat, openat2, renameat, renameat_with,
    symlinkat, unlinkat, utimensat,
};
use rustix::io::{FdFlags, fcntl_getfd};
use sha2::{Digest as _, Sha256};
use tempfile::TempDir;

const PROT_READ: c_int = 0x1;
const PROT_WRITE: c_int = 0x2;
const MAP_SHARED: c_int = 0x01;
const MS_SYNC: c_int = 0x04;
const SYS_FCHMODAT2: c_long = 452;
const RLIMIT_NOFILE: c_int = 7;

#[repr(C)]
#[derive(Clone, Copy)]
struct RLimit {
    rlim_cur: u64,
    rlim_max: u64,
}

unsafe extern "C" {
    fn mmap(
        addr: *mut c_void,
        length: usize,
        prot: c_int,
        flags: c_int,
        fd: c_int,
        offset: i64,
    ) -> *mut c_void;
    fn munmap(addr: *mut c_void, length: usize) -> c_int;
    fn msync(addr: *mut c_void, length: usize, flags: c_int) -> c_int;
    fn umask(mask: u32) -> u32;
    fn fork() -> c_int;
    fn vfork() -> c_int;
    fn waitpid(pid: c_int, status: *mut c_int, options: c_int) -> c_int;
    fn _exit(status: c_int) -> !;
    fn execv(path: *const i8, argv: *const *const i8) -> c_int;
    fn syscall(num: c_long, ...) -> c_long;
    fn getrlimit(resource: c_int, rlim: *mut RLimit) -> c_int;
    fn setrlimit(resource: c_int, rlim: *const RLimit) -> c_int;
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum OracleEntry {
    File { mode: u32, sha256: [u8; 32] },
    Dir { mode: u32 },
    Symlink { target: Vec<u8> },
}

/// 按决定 1 口径独立遍历目录树：只统计常规文件、目录与符号链接（排除特殊文件、保留 `.git`
/// 以及调用方指定的排除顶层名），按路径独立记录存在性、类型、内容摘要/链接目标与权限位，不比较硬链接关系。
fn oracle_scan(root: &Path, skip_top: &[&str]) -> BTreeMap<String, OracleEntry> {
    let mut out = BTreeMap::new();
    oracle_walk(root, "", skip_top, &mut out);
    out
}

fn oracle_walk(
    root: &Path,
    rel_dir: &str,
    skip_top: &[&str],
    out: &mut BTreeMap<String, OracleEntry>,
) {
    let abs_dir = if rel_dir.is_empty() {
        root.to_path_buf()
    } else {
        root.join(rel_dir)
    };
    let Ok(rd) = fs::read_dir(&abs_dir) else {
        return;
    };
    for entry in rd.flatten() {
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        let rel = if rel_dir.is_empty() {
            if name == ".git" || skip_top.contains(&name.as_str()) {
                continue;
            }
            name
        } else {
            format!("{rel_dir}/{name}")
        };
        let abs = entry.path();
        let Ok(m) = fs::symlink_metadata(&abs) else {
            continue;
        };
        let ft = m.file_type();
        if ft.is_fifo() || ft.is_socket() || ft.is_block_device() || ft.is_char_device() {
            continue;
        }
        if ft.is_symlink() {
            let target = fs::read_link(&abs)
                .expect("读取符号链接目标失败")
                .into_os_string()
                .into_vec();
            out.insert(rel, OracleEntry::Symlink { target });
        } else if ft.is_dir() {
            let mode = m.mode() & 0o7777;
            out.insert(rel.clone(), OracleEntry::Dir { mode });
            oracle_walk(root, &rel, skip_top, out);
        } else if ft.is_file() {
            let mode = m.mode() & 0o7777;
            let bytes = fs::read(&abs).expect("独立对照读取文件失败");
            let sha256: [u8; 32] = Sha256::digest(&bytes).into();
            out.insert(rel, OracleEntry::File { mode, sha256 });
        }
    }
}

fn oracle_diff(
    base: &BTreeMap<String, OracleEntry>,
    tree: &BTreeMap<String, OracleEntry>,
) -> Vec<(String, PendingAction)> {
    let mut keys: Vec<&String> = base.keys().chain(tree.keys()).collect();
    keys.sort();
    keys.dedup();
    let mut diff = Vec::new();
    for k in keys {
        match (base.get(k), tree.get(k)) {
            (None, Some(_)) => diff.push((k.clone(), PendingAction::Added)),
            (Some(_), None) => diff.push((k.clone(), PendingAction::Deleted)),
            (Some(a), Some(b)) if a != b => diff.push((k.clone(), PendingAction::Modified)),
            _ => {}
        }
    }
    diff
}

fn status_pairs(shadow: &ShadowDir) -> Vec<(String, PendingAction)> {
    shadow
        .status()
        .expect("读取影子 status 失败")
        .into_iter()
        .map(|c| (c.path, c.action))
        .collect()
}

fn tempdir_xfs(prefix: &str) -> TempDir {
    tempfile::Builder::new()
        .prefix(prefix)
        .tempdir_in("/tmp")
        .expect("在 /tmp 创建 XFS 临时目录失败")
}

// =============================================================================
// 1. shadow_reopens_pending_without_reseeding
// =============================================================================

#[test]
fn shadow_reopens_pending_without_reseeding() {
    let root = tempdir_xfs("ln-sh-reopen-");
    let host = root.path().join("host");
    let state = root.path().join("state");
    fs::create_dir_all(host.join("seed")).unwrap();
    fs::create_dir_all(host.join("sub")).unwrap();

    fs::write(host.join("a.txt"), "base-a\n").unwrap();
    fs::set_permissions(host.join("a.txt"), fs::Permissions::from_mode(0o640)).unwrap();
    fs::write(host.join("sub/b.txt"), "base-b\n").unwrap();
    symlink("a.txt", host.join("link_a")).unwrap();
    fs::write(host.join(".env"), "SECRET=1").unwrap();
    fs::write(host.join("seed/cache.txt"), "seed-orig").unwrap();
    fs::write(host.join("seed/secret.txt"), "seed-hidden").unwrap();

    let hidden = vec![".env".to_owned(), "seed/secret.txt".to_owned()];
    let regenerable = vec!["seed/".to_owned()];

    let key1 = {
        let shadow =
            ShadowDir::open(&state, "ws", &host, &hidden, &regenerable).expect("首次建立影子失败");
        assert!(!shadow.tree_path().join(".env").exists());
        assert!(!shadow.tree_path().join("seed/secret.txt").exists());
        assert!(shadow.tree_path().join("seed/cache.txt").exists());
        assert!(!shadow.base_path().join("seed").exists());
        assert_eq!(
            fs::symlink_metadata(shadow.tree_path().join("a.txt"))
                .unwrap()
                .permissions()
                .mode()
                & 0o7777,
            0o640
        );
        assert_eq!(
            fs::read_link(shadow.tree_path().join("link_a")).unwrap(),
            PathBuf::from("a.txt")
        );

        // 模拟两次 act：各自同步并写入草稿与种子。
        shadow.sync_from_host(None, None).unwrap();
        shadow.write("a.txt", "draft-1\n").unwrap();
        shadow.write("seed/cache.txt", "seed-v1").unwrap();

        shadow.sync_from_host(None, None).unwrap();
        shadow.write("new.txt", "draft-2\n").unwrap();
        shadow.write("seed/extra.txt", "seed-added").unwrap();

        shadow.shadow_key().to_owned()
    };

    // 重启复用（故意换用不同句柄名 ws_renamed，验证句柄名不影响绑定键）。
    let reopened = ShadowDir::open(&state, "ws_renamed", &host, &hidden, &regenerable)
        .expect("重启复用影子失败");
    assert_eq!(
        reopened.shadow_key(),
        key1,
        "仅改变句柄名不应改变影子绑定键"
    );
    assert_eq!(
        status_pairs(&reopened),
        vec![
            ("a.txt".to_owned(), PendingAction::Modified),
            ("new.txt".to_owned(), PendingAction::Added),
        ]
    );
    assert_eq!(
        reopened.read("a.txt").unwrap().as_deref(),
        Some("draft-1\n")
    );
    assert_eq!(
        reopened.read("new.txt").unwrap().as_deref(),
        Some("draft-2\n")
    );
    assert_eq!(
        fs::read_to_string(reopened.base_path().join("a.txt")).unwrap(),
        "base-a\n",
        "重启后基准仍应保持 base-a"
    );
    assert_eq!(
        reopened.read("seed/cache.txt").unwrap().as_deref(),
        Some("seed-v1"),
        "重启不应重播种子覆盖已有修改"
    );
    assert_eq!(
        reopened.read("seed/extra.txt").unwrap().as_deref(),
        Some("seed-added")
    );
    assert!(!reopened.tree_path().join("seed/secret.txt").exists());
    // 宿主完全未变。
    assert_eq!(fs::read_to_string(host.join("a.txt")).unwrap(), "base-a\n");
    assert_eq!(
        fs::read_to_string(host.join("seed/cache.txt")).unwrap(),
        "seed-orig"
    );
    assert!(!host.join("new.txt").exists());
    drop(reopened);

    // 改变 hidden 规则 -> 换用新影子，旧影子目录保留。
    let mut hidden2 = hidden.clone();
    hidden2.push("*.key".to_owned());
    let s_new_hidden = ShadowDir::open(&state, "ws", &host, &hidden2, &regenerable)
        .expect("变更 hidden 规则后建新影子失败");
    assert_ne!(s_new_hidden.shadow_key(), key1);
    assert!(s_new_hidden.status().unwrap().is_empty());
    assert_eq!(
        s_new_hidden.read("a.txt").unwrap().as_deref(),
        Some("base-a\n")
    );
    assert!(
        state.join("shadows").join(&key1).is_dir(),
        "旧影子目录应保留不清理"
    );
    drop(s_new_hidden);

    // 改变 regenerable 规则 -> 换用新影子。
    let mut regen2 = regenerable.clone();
    regen2.push("dist/".to_owned());
    let s_new_regen = ShadowDir::open(&state, "ws", &host, &hidden, &regen2)
        .expect("变更 regenerable 规则后建新影子失败");
    assert_ne!(s_new_regen.shadow_key(), key1);
    drop(s_new_regen);

    // 改变宿主根路径 -> 换用新影子。
    let host2 = root.path().join("host2");
    fs::create_dir(&host2).unwrap();
    let s_new_path = ShadowDir::open(&state, "ws", &host2, &hidden, &regenerable)
        .expect("变更宿主路径后建新影子失败");
    assert_ne!(s_new_path.shadow_key(), key1);
}

// =============================================================================
// 2. shadow_status_matches_inherited_cases（实验 3 全部 63 个继承输入）
// =============================================================================

/// 实验 3 `verify-v7.py` 中的全部 63 个继承用例。
const INHERITED_CASES: &[&str] = &[
    "open",
    "open-rdwr",
    "open-append",
    "open-trunc-readonly",
    "open-readonly",
    "open-create",
    "open-excl-fail",
    "open-idle",
    "openat",
    "cwd",
    "openat2",
    "openat2-beneath",
    "creat",
    "tmpfile",
    "tmpfile-empty",
    "opath",
    "proc-cwd",
    "proc-root",
    "symlink-alias",
    "dangling-symlink",
    "umask",
    "hardlink-normal",
    "unlink",
    "unlinkat",
    "rmdir",
    "unlinkat-dir",
    "rename",
    "renameat",
    "rename-exchange",
    "rename-noreplace",
    "rename-dir",
    "truncate",
    "link",
    "linkat",
    "symlink",
    "symlinkat",
    "unlink-symlink",
    "mkdir",
    "mkdirat",
    "chmod",
    "fchmod",
    "fchmodat",
    "fchmodat2",
    "mknod",
    "mknodat",
    "ftruncate",
    "fallocate",
    "mmap",
    "nofollow",
    "excl-symlink",
    "cloexec",
    "not-cloexec",
    "fork",
    "vfork",
    "thread",
    "thread-exec",
    "unlink-hardlink",
    "rename-hardlink",
    "anchor-unlink-all",
    "anchor-overwrite",
    "rename-edit",
    "type-file-dir",
    "type-dir-file",
];

/// 按决定 1 与方案 A 与实验 3 v7 对照结论存在明确预期差异的 8 个用例：
/// (用例名, 实验 3 v7 原对照路径, 现行合同口径预期路径, 差异原因)。
const EXPECTED_V7_DIFFERENCES: &[(&str, &[&str], &[&str], &str)] = &[
    (
        "hardlink-normal",
        &["a", "hard"],
        &["hard"],
        "建影子按路径独立克隆、不重建硬链接组；写入 hard 仅改变 hard 本身",
    ),
    (
        "link",
        &["a", "new"],
        &["new"],
        "决定 1：各路径按内容/存在性/类型/权限独立比较，不比较硬链接关系；a 本身未变",
    ),
    (
        "linkat",
        &["a", "new"],
        &["new"],
        "决定 1：不比较硬链接关系；a 本身未变，仅 new 新增",
    ),
    (
        "mknod",
        &["fifo"],
        &[],
        "决定 1：FIFO/套接字/设备等特殊文件不计入待定改动，也不发布",
    ),
    (
        "mknodat",
        &["fifo"],
        &[],
        "决定 1：FIFO/套接字/设备等特殊文件不计入待定改动，也不发布",
    ),
    (
        "unlink-hardlink",
        &["a", "hard"],
        &["hard"],
        "决定 1 + 方案 A：不比较硬链接关系且初始化不重建硬链接组；仅 hard 被删除",
    ),
    (
        "rename-hardlink",
        &["a", "hard", "new"],
        &["hard", "new"],
        "决定 1 + 方案 A：不比较硬链接关系；a 未变，仅 hard 删除与 new 新增",
    ),
    (
        "anchor-overwrite",
        &["a", "b", "hard"],
        &["b", "hard"],
        "决定 1 + 方案 A：不比较硬链接关系；a 未变，仅 b 删除与 hard 被原 b 覆盖",
    ),
];

fn setup_inherited_fixture(root: &Path, case: &str) {
    let fixture_kind = match case {
        "unlink-hardlink" | "rename-hardlink" | "anchor-unlink-all" | "anchor-overwrite" => {
            "hardlink-normal"
        }
        "rename-edit" => "rename-dir",
        other => other,
    };
    for d in ["target", ".git", "hidden", "sub", "empty"] {
        fs::create_dir_all(root.join(d)).unwrap();
    }
    for name in ["a", "b", "sub/a", "outside", "race-safe", "race-evil"] {
        let mut buf = format!("{name}:BEFORE\n").into_bytes();
        buf.resize(4096, b'.');
        fs::write(root.join(name), buf).unwrap();
    }
    symlink("a", root.join("alias")).unwrap();
    symlink("new-target", root.join("dangling")).unwrap();
    fs::write(root.join("target/a"), "target-before").unwrap();
    fs::write(root.join("readonly-file"), "read-only").unwrap();
    fs::set_permissions(
        root.join("readonly-file"),
        fs::Permissions::from_mode(0o444),
    )
    .unwrap();
    fs::create_dir(root.join("readonly")).unwrap();
    fs::set_permissions(root.join("readonly"), fs::Permissions::from_mode(0o555)).unwrap();

    if fixture_kind == "hardlink-normal" {
        fs::hard_link(root.join("a"), root.join("hard")).unwrap();
    }
    if fixture_kind == "rename-dir" {
        fs::create_dir(root.join("dir")).unwrap();
        for i in 0..3 {
            fs::write(root.join(format!("dir/f{i:04}")), format!("before-{i}")).unwrap();
        }
    }
}

fn write_after_fd(fd: OwnedFd) {
    let mut f = fs::File::from(fd);
    f.write_all(b"AFTER!").expect("写入 AFTER! 失败");
}

fn run_inherited_case_in_cwd(case: &str) {
    let rw_mode = Mode::from_raw_mode(0o644);
    match case {
        "child-write" => {
            let fd = openat(
                CWD,
                "child",
                OFlags::WRONLY | OFlags::CREATE | OFlags::TRUNC,
                rw_mode,
            )
            .expect("child-write openat 失败");
            write_after_fd(fd);
        }
        "open" => {
            let fd = openat(CWD, "a", OFlags::WRONLY, Mode::empty()).expect("open 失败");
            write_after_fd(fd);
        }
        "open-rdwr" => {
            let fd = openat(CWD, "a", OFlags::RDWR, Mode::empty()).expect("open-rdwr 失败");
            write_after_fd(fd);
        }
        "open-append" => {
            let fd = openat(CWD, "a", OFlags::WRONLY | OFlags::APPEND, Mode::empty())
                .expect("open-append 失败");
            write_after_fd(fd);
        }
        "open-trunc-readonly" => {
            let fd = openat(CWD, "a", OFlags::RDONLY | OFlags::TRUNC, Mode::empty())
                .expect("open-trunc-readonly 失败");
            drop(fd);
        }
        "open-readonly" => {
            let fd = openat(CWD, "a", OFlags::RDONLY, Mode::empty()).expect("open-readonly 失败");
            drop(fd);
        }
        "open-create" => {
            let fd = openat(
                CWD,
                "new",
                OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL,
                rw_mode,
            )
            .expect("open-create 失败");
            write_after_fd(fd);
        }
        "open-excl-fail" => {
            let res = openat(
                CWD,
                "a",
                OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL,
                rw_mode,
            );
            assert!(res.is_err(), "open-excl-fail 对已存在文件应返回 EEXIST");
        }
        "open-idle" => {
            let fd = openat(CWD, "a", OFlags::WRONLY, Mode::empty()).expect("open-idle 失败");
            drop(fd);
        }
        "openat" => {
            let d = openat(
                CWD,
                "sub",
                OFlags::RDONLY | OFlags::DIRECTORY,
                Mode::empty(),
            )
            .expect("openat 打开目录失败");
            let fd = openat(&d, "a", OFlags::WRONLY, Mode::empty()).expect("openat 打开文件失败");
            write_after_fd(fd);
        }
        "cwd" => {
            std::env::set_current_dir("sub").expect("cwd chdir 失败");
            let fd = openat(CWD, "a", OFlags::WRONLY, Mode::empty()).expect("cwd openat 失败");
            write_after_fd(fd);
        }
        "openat2" | "openat2-beneath" => {
            let resolve = if case == "openat2-beneath" {
                ResolveFlags::BENEATH
            } else {
                ResolveFlags::empty()
            };
            let fd = openat2(CWD, "a", OFlags::WRONLY, Mode::empty(), resolve)
                .unwrap_or_else(|e| panic!("{case} 失败：{e}"));
            write_after_fd(fd);
        }
        "creat" => {
            let fd = openat(
                CWD,
                "a",
                OFlags::WRONLY | OFlags::CREATE | OFlags::TRUNC,
                rw_mode,
            )
            .expect("creat 失败");
            write_after_fd(fd);
        }
        "tmpfile" => {
            let fd = openat(CWD, ".", OFlags::TMPFILE | OFlags::RDWR, rw_mode)
                .expect("tmpfile openat 失败");
            let raw = fd.as_raw_fd();
            assert_eq!(
                rustix::io::write(&fd, b"AFTER!").expect("tmpfile write 失败"),
                6
            );
            let proc_path = format!("/proc/self/fd/{raw}");
            linkat(CWD, &proc_path, CWD, "new", AtFlags::SYMLINK_FOLLOW)
                .expect("tmpfile linkat 失败");
        }
        "tmpfile-empty" => {
            let fd = openat(CWD, ".", OFlags::TMPFILE | OFlags::RDWR, rw_mode)
                .expect("tmpfile-empty openat 失败");
            assert_eq!(
                rustix::io::write(&fd, b"AFTER!").expect("tmpfile-empty write 失败"),
                6
            );
            linkat(&fd, "", CWD, "new", AtFlags::EMPTY_PATH).expect("tmpfile-empty linkat 失败");
        }
        "opath" => {
            let fd = openat(CWD, "a", OFlags::PATH, Mode::empty()).expect("opath openat 失败");
            let proc_path = format!("/proc/self/fd/{}", fd.as_raw_fd());
            let wfd =
                openat(CWD, &proc_path, OFlags::WRONLY, Mode::empty()).expect("opath reopen 失败");
            write_after_fd(wfd);
        }
        "proc-cwd" => {
            let fd = openat(CWD, "/proc/self/cwd/a", OFlags::WRONLY, Mode::empty())
                .expect("proc-cwd openat 失败");
            write_after_fd(fd);
        }
        "proc-root" => {
            let cwd = std::env::current_dir().expect("获取 cwd 失败");
            let path = format!("/proc/self/root{}/a", cwd.display());
            let fd =
                openat(CWD, &path, OFlags::WRONLY, Mode::empty()).expect("proc-root openat 失败");
            write_after_fd(fd);
        }
        "symlink-alias" => {
            let fd = openat(CWD, "alias", OFlags::WRONLY, Mode::empty())
                .expect("symlink-alias openat 失败");
            write_after_fd(fd);
        }
        "dangling-symlink" => {
            let fd = openat(CWD, "dangling", OFlags::WRONLY | OFlags::CREATE, rw_mode)
                .expect("dangling-symlink openat 失败");
            write_after_fd(fd);
        }
        "umask" => {
            // SAFETY: 当前位于独立子进程中，调整本进程 umask 不影响测试主进程。
            unsafe {
                umask(0o077);
            }
            let fd = openat(
                CWD,
                "new",
                OFlags::WRONLY | OFlags::CREATE,
                Mode::from_raw_mode(0o666),
            )
            .expect("umask openat 失败");
            write_after_fd(fd);
        }
        "hardlink-normal" => {
            let fd = openat(CWD, "hard", OFlags::WRONLY, Mode::empty())
                .expect("hardlink-normal openat 失败");
            write_after_fd(fd);
        }
        "unlink" | "unlinkat" => {
            unlinkat(CWD, "a", AtFlags::empty()).expect("unlink 失败");
        }
        "rmdir" | "unlinkat-dir" => {
            unlinkat(CWD, "empty", AtFlags::REMOVEDIR).expect("rmdir 失败");
        }
        "rename" | "renameat" => {
            renameat(CWD, "a", CWD, "b").expect("rename 失败");
        }
        "rename-exchange" => {
            renameat_with(CWD, "a", CWD, "b", RenameFlags::EXCHANGE).expect("rename-exchange 失败");
        }
        "rename-noreplace" => {
            let res = renameat_with(CWD, "a", CWD, "b", RenameFlags::NOREPLACE);
            assert!(res.is_err(), "rename-noreplace 覆盖已存在目标应返回 EEXIST");
        }
        "rename-dir" => {
            renameat(CWD, "dir", CWD, "moved").expect("rename-dir 失败");
        }
        "truncate" => {
            let f = fs::OpenOptions::new().write(true).open("a").unwrap();
            f.set_len(2).expect("truncate set_len 失败");
        }
        "link" | "linkat" => {
            linkat(CWD, "a", CWD, "new", AtFlags::empty()).expect("link 失败");
        }
        "symlink" | "symlinkat" => {
            symlinkat("a", CWD, "new").expect("symlink 失败");
        }
        "unlink-symlink" => {
            unlinkat(CWD, "alias", AtFlags::empty()).expect("unlink-symlink 失败");
        }
        "mkdir" | "mkdirat" => {
            mkdirat(CWD, "newdir", Mode::from_raw_mode(0o755)).expect("mkdir 失败");
        }
        "chmod" => {
            fs::set_permissions("a", fs::Permissions::from_mode(0o600)).expect("chmod 失败");
        }
        "fchmod" => {
            let fd = openat(CWD, "a", OFlags::RDONLY, Mode::empty()).expect("fchmod open 失败");
            fchmod(&fd, Mode::from_raw_mode(0o600)).expect("fchmod 失败");
        }
        "fchmodat" => {
            chmodat(CWD, "a", Mode::from_raw_mode(0o600), AtFlags::empty()).expect("fchmodat 失败");
        }
        "fchmodat2" => {
            let p = CString::new("a").unwrap();
            // SAFETY: 标准 Linux SYS_fchmodat2(AT_FDCWD, "a", 0601, 0) 调用。
            let rc = unsafe { syscall(SYS_FCHMODAT2, -100i32, p.as_ptr(), 0o601u32, 0u32) };
            assert_eq!(
                rc,
                0,
                "fchmodat2 应成功，实际返回 {rc} ({:?})",
                std::io::Error::last_os_error()
            );
        }
        "mknod" | "mknodat" => {
            mknodat(
                CWD,
                "fifo",
                rustix::fs::FileType::Fifo,
                Mode::from_raw_mode(0o600),
                0,
            )
            .expect("mknod 失败");
        }
        "ftruncate" => {
            let fd = openat(CWD, "a", OFlags::RDWR, Mode::empty()).expect("ftruncate open 失败");
            ftruncate(&fd, 2).expect("ftruncate 失败");
        }
        "fallocate" => {
            let fd = openat(CWD, "a", OFlags::RDWR, Mode::empty()).expect("fallocate open 失败");
            rustix::fs::fallocate(&fd, FallocateFlags::ZERO_RANGE, 0, 4096)
                .expect("fallocate 失败");
        }
        "mmap" => {
            let fd = openat(CWD, "a", OFlags::RDWR, Mode::empty()).expect("mmap open 失败");
            // SAFETY: 映射长度为 4096（文件大小为 4096），写 6 字节后 msync 并 munmap。
            unsafe {
                let ptr = mmap(
                    std::ptr::null_mut(),
                    4096,
                    PROT_READ | PROT_WRITE,
                    MAP_SHARED,
                    fd.as_raw_fd(),
                    0,
                );
                assert_ne!(ptr, usize::MAX as *mut c_void, "mmap 失败");
                std::ptr::copy_nonoverlapping(b"AFTER!".as_ptr(), ptr.cast::<u8>(), 6);
                assert_eq!(msync(ptr, 4096, MS_SYNC), 0, "msync 失败");
                assert_eq!(munmap(ptr, 4096), 0, "munmap 失败");
            }
        }
        "nofollow" => {
            let res = openat(
                CWD,
                "alias",
                OFlags::WRONLY | OFlags::NOFOLLOW,
                Mode::empty(),
            );
            assert!(res.is_err(), "nofollow 打开符号链接应返回 ELOOP");
        }
        "excl-symlink" => {
            let res = openat(
                CWD,
                "alias",
                OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL,
                Mode::from_raw_mode(0o666),
            );
            assert!(res.is_err(), "excl-symlink 对已存在符号链接应返回 EEXIST");
        }
        "cloexec" => {
            let fd = openat(CWD, "a", OFlags::WRONLY | OFlags::CLOEXEC, Mode::empty())
                .expect("cloexec openat 失败");
            let flags = fcntl_getfd(&fd).expect("cloexec fcntl_getfd 失败");
            assert!(flags.contains(FdFlags::CLOEXEC));
        }
        "not-cloexec" => {
            let fd =
                openat(CWD, "a", OFlags::WRONLY, Mode::empty()).expect("not-cloexec openat 失败");
            let flags = fcntl_getfd(&fd).expect("not-cloexec fcntl_getfd 失败");
            assert!(!flags.contains(FdFlags::CLOEXEC));
        }
        "fork" | "vfork" => {
            let exe = CString::new(std::env::current_exe().unwrap().to_str().unwrap()).unwrap();
            let a1 = CString::new("--exact").unwrap();
            let a2 = CString::new("inherited_writer_helper").unwrap();
            let a3 = CString::new("--ignored").unwrap();
            let argv = [
                exe.as_ptr(),
                a1.as_ptr(),
                a2.as_ptr(),
                a3.as_ptr(),
                std::ptr::null(),
            ];
            // SAFETY: 子进程已提前准备好参数与环境变量，vfork 后立即 execv 或 _exit。
            unsafe {
                std::env::set_var("LANCET_INHERITED_CASE", "child-write");
                let pid = if case == "vfork" { vfork() } else { fork() };
                assert!(pid >= 0, "{case} 失败");
                if pid == 0 {
                    execv(exe.as_ptr(), argv.as_ptr());
                    _exit(99);
                }
                let mut status: c_int = 0;
                assert_eq!(waitpid(pid, &mut status, 0), pid, "{case} waitpid 失败");
                assert_eq!(status, 0, "{case} 子进程应正常退出，实际 status={status}");
            }
        }
        "thread" => {
            std::thread::spawn(|| {
                let fd = openat(
                    CWD,
                    "child",
                    OFlags::WRONLY | OFlags::CREATE | OFlags::TRUNC,
                    Mode::from_raw_mode(0o644),
                )
                .expect("thread openat 失败");
                write_after_fd(fd);
            })
            .join()
            .unwrap();
        }
        "thread-exec" => {
            let exe = CString::new(std::env::current_exe().unwrap().to_str().unwrap()).unwrap();
            let a1 = CString::new("--exact").unwrap();
            let a2 = CString::new("inherited_writer_helper").unwrap();
            let a3 = CString::new("--ignored").unwrap();
            // SAFETY: 独立子进程中的线程执行 execv 替换该子进程。
            unsafe {
                std::env::set_var("LANCET_INHERITED_CASE", "child-write");
            }
            std::thread::spawn(move || {
                let argv = [
                    exe.as_ptr(),
                    a1.as_ptr(),
                    a2.as_ptr(),
                    a3.as_ptr(),
                    std::ptr::null(),
                ];
                // SAFETY: 仅替换当前独立测试子进程镜像。
                unsafe {
                    execv(exe.as_ptr(), argv.as_ptr());
                    _exit(97);
                }
            })
            .join()
            .expect("thread-exec 线程失败");
        }
        "unlink-hardlink" => {
            unlinkat(CWD, "hard", AtFlags::empty()).expect("unlink-hardlink 失败");
        }
        "rename-hardlink" => {
            renameat(CWD, "hard", CWD, "new").expect("rename-hardlink 失败");
        }
        "anchor-unlink-all" => {
            unlinkat(CWD, "hard", AtFlags::empty()).expect("anchor-unlink-all unlink hard 失败");
            unlinkat(CWD, "a", AtFlags::empty()).expect("anchor-unlink-all unlink a 失败");
        }
        "anchor-overwrite" => {
            renameat(CWD, "b", CWD, "hard").expect("anchor-overwrite 失败");
        }
        "rename-edit" => {
            renameat(CWD, "dir", CWD, "moved").expect("rename-edit rename 失败");
            let fd = openat(
                CWD,
                "moved/f0000",
                OFlags::WRONLY | OFlags::TRUNC,
                Mode::empty(),
            )
            .expect("rename-edit open 失败");
            write_after_fd(fd);
        }
        "type-file-dir" => {
            fs::remove_file("a").unwrap();
            fs::create_dir("a").unwrap();
            fs::write("a/x", "child\n").unwrap();
        }
        "type-dir-file" => {
            fs::remove_dir("empty").unwrap();
            fs::write("empty", "file\n").unwrap();
        }
        other => panic!("未知继承用例：{other}"),
    }
}

#[test]
#[ignore = "shadow_status_matches_inherited_cases 内部调用的独立写者子进程入口"]
fn inherited_writer_helper() {
    let Some(case) = std::env::var_os("LANCET_INHERITED_CASE") else {
        return;
    };
    run_inherited_case_in_cwd(&case.to_string_lossy());
}

#[test]
fn shadow_status_matches_inherited_cases() {
    assert_eq!(
        INHERITED_CASES.len(),
        63,
        "应包含实验 3 的全部 63 个继承用例"
    );
    let exe = std::env::current_exe().expect("获取测试可执行文件路径失败");
    let hidden = vec!["hidden/".to_owned()];
    let regenerable = vec!["target/".to_owned()];
    let skip_top = ["target", "hidden"];

    for &case in INHERITED_CASES {
        let root = tempdir_xfs("ln-sh-inh-");
        let host = root.path().join("host");
        let state = root.path().join("state");
        setup_inherited_fixture(&host, case);
        let host_before = oracle_scan(&host, &skip_top);

        let shadow = ShadowDir::open(&state, "w", &host, &hidden, &regenerable)
            .unwrap_or_else(|e| panic!("用例 `{case}` 建立影子失败：{e:#}"));

        // 写者子进程直接在影子 tree/ 中执行该用例，退出后确保所有 fd/映射均已关闭。
        let out = Command::new(&exe)
            .current_dir(shadow.tree_path())
            .env("LANCET_INHERITED_CASE", case)
            .args(["--exact", "inherited_writer_helper", "--ignored"])
            .output()
            .unwrap_or_else(|e| panic!("用例 `{case}` 启动写者子进程失败：{e}"));
        assert!(
            out.status.success(),
            "用例 `{case}` 写者子进程异常退出：{:?}\nstderr:\n{}",
            out.status,
            String::from_utf8_lossy(&out.stderr)
        );

        let actual = status_pairs(&shadow);
        let base_oracle = oracle_scan(shadow.base_path(), &skip_top);
        let tree_oracle = oracle_scan(shadow.tree_path(), &skip_top);
        let expected = oracle_diff(&base_oracle, &tree_oracle);
        assert_eq!(
            actual, expected,
            "用例 `{case}` 增量 status 与独立遍历基准/影子对照不一致"
        );

        // 核对已知的 8 项与实验 3 v7 对照的预期差异。
        let actual_paths: Vec<&str> = actual.iter().map(|(p, _)| p.as_str()).collect();
        if let Some((_, _v7_paths, contract_paths, reason)) = EXPECTED_V7_DIFFERENCES
            .iter()
            .find(|(c, _, _, _)| *c == case)
        {
            assert_eq!(
                actual_paths.as_slice(),
                *contract_paths,
                "预期差异用例 `{case}`（原因：{reason}）路径集合不符"
            );
        }

        // 宿主全程未被写入。
        let host_after = oracle_scan(&host, &skip_top);
        assert!(
            oracle_diff(&host_before, &host_after).is_empty(),
            "用例 `{case}` 不得改动宿主工作目录"
        );

        // discard 后常规路径应完全恢复为基准。
        shadow
            .discard(None, None)
            .unwrap_or_else(|e| panic!("用例 `{case}` discard 失败：{e:?}"));
        assert!(
            shadow.status().unwrap().is_empty(),
            "用例 `{case}` discard 后 status 应为空"
        );
        let tree_after_discard = oracle_scan(shadow.tree_path(), &skip_top);
        assert!(
            oracle_diff(&base_oracle, &tree_after_discard).is_empty(),
            "用例 `{case}` discard 后 tree 应与 base 一致"
        );

        // 放宽只读夹具目录权限以便 TempDir 清理。
        let _ = fs::set_permissions(host.join("readonly"), fs::Permissions::from_mode(0o700));
        let _ = fs::set_permissions(
            shadow.tree_path().join("readonly"),
            fs::Permissions::from_mode(0o700),
        );
        let _ = fs::set_permissions(
            shadow.base_path().join("readonly"),
            fs::Permissions::from_mode(0o700),
        );
    }

    // 补充核查方案 C：在影子内新建硬链接后经新路径写入，同 inode 的两个路径都应被核实为待定。
    let root = tempdir_xfs("ln-sh-hl-");
    let host = root.path().join("host");
    let state = root.path().join("state");
    fs::create_dir(&host).unwrap();
    fs::write(host.join("a"), "orig").unwrap();
    let shadow = ShadowDir::open(&state, "w", &host, &[], &[]).unwrap();
    fs::hard_link(shadow.tree_path().join("a"), shadow.tree_path().join("hl")).unwrap();
    assert_eq!(
        status_pairs(&shadow),
        vec![("hl".to_owned(), PendingAction::Added)]
    );
    fs::write(shadow.tree_path().join("hl"), "modified-via-hl").unwrap();
    assert_eq!(
        status_pairs(&shadow),
        vec![
            ("a".to_owned(), PendingAction::Modified),
            ("hl".to_owned(), PendingAction::Added),
        ],
        "影子内硬链接写入应通过 (dev, ino) 索引同时核实兄弟路径 `a`"
    );
}

// =============================================================================
// 3. shadow_random_batches_match_full_contents（重放实验 3 原始 200 批记录）
// =============================================================================

const RANDOMIZED_V7_FIXTURE: &str = include_str!("fixtures/randomized-v7.json");

#[derive(serde::Deserialize)]
struct RandomizedBatch {
    seed: u64,
    side: String,
    batch: usize,
    ops: Vec<(String, String)>,
}

fn replay_random_batch(root: &Path, seed: u64, batch: usize, ops: &[(String, String)]) {
    for (j, (op, name)) in ops.iter().enumerate() {
        let p = root.join(name);
        match op.as_str() {
            "write" => {
                let body = format!("{batch}-{j}").repeat(30);
                fs::write(&p, body).unwrap_or_else(|e| panic!("write {name} 失败：{e}"));
            }
            "atomic" => {
                let tmp = root.join("tmp-save");
                fs::write(&tmp, format!("atomic-{batch}-{j}"))
                    .unwrap_or_else(|e| panic!("atomic write tmp-save 失败：{e}"));
                fs::rename(&tmp, &p).unwrap_or_else(|e| panic!("atomic rename {name} 失败：{e}"));
            }
            "link" => {
                let _ = fs::remove_file(&p);
                let start = ((seed as usize) * 13 + batch * 7 + j) % 30;
                for off in 0..30 {
                    let cand = root.join(format!("f{}", (start + off) % 30));
                    if cand != p && cand.is_file() {
                        fs::hard_link(&cand, &p)
                            .unwrap_or_else(|e| panic!("hard_link -> {name} 失败：{e}"));
                        break;
                    }
                }
            }
            "unlink" => match fs::remove_file(&p) {
                Ok(()) => {}
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
                Err(err) => panic!("unlink {name} 失败：{err}"),
            },
            "chmod" => {
                if p.exists() {
                    let modes = [0o600, 0o644, 0o755];
                    let mode = modes[((seed as usize) + batch + j) % 3];
                    fs::set_permissions(&p, fs::Permissions::from_mode(mode))
                        .unwrap_or_else(|e| panic!("chmod {name} 失败：{e}"));
                }
            }
            "mmap" => {
                if let Ok(m) = fs::symlink_metadata(&p)
                    && m.is_file()
                    && m.len() > 0
                {
                    let fd = openat(CWD, &p, OFlags::RDWR, Mode::empty())
                        .unwrap_or_else(|e| panic!("mmap open {name} 失败：{e}"));
                    let len = m.len() as usize;
                    let byte = b'A' + (((seed as usize) + batch * 7 + j) % 26) as u8;
                    // SAFETY: 映射范围严格等于文件当前长度，写单字节后 msync 并 munmap。
                    unsafe {
                        let ptr = mmap(
                            std::ptr::null_mut(),
                            len,
                            PROT_READ | PROT_WRITE,
                            MAP_SHARED,
                            fd.as_raw_fd(),
                            0,
                        );
                        assert_ne!(ptr, usize::MAX as *mut c_void, "mmap {name} 失败");
                        *ptr.cast::<u8>() = byte;
                        assert_eq!(msync(ptr, len, MS_SYNC), 0, "msync {name} 失败");
                        assert_eq!(munmap(ptr, len), 0, "munmap {name} 失败");
                    }
                }
            }
            "directory" => {
                let a = root.join("tree");
                let b = root.join("moved");
                if a.exists() {
                    fs::rename(&a, &b).expect("rename tree -> moved 失败");
                } else if b.exists() {
                    fs::rename(&b, &a).expect("rename moved -> tree 失败");
                } else {
                    fs::create_dir_all(a.join("new/nested")).expect("创建 tree/new/nested 失败");
                    fs::write(a.join("new/nested/x"), "new").expect("写入 tree/new/nested/x 失败");
                }
            }
            other => panic!("未知随机操作类型：{other}"),
        }
    }
}

#[test]
fn shadow_random_batches_match_full_contents() {
    let all_batches: Vec<RandomizedBatch> =
        serde_json::from_str(RANDOMIZED_V7_FIXTURE).expect("解析 randomized-v7.json 失败");
    assert_eq!(all_batches.len(), 200, "原始随机记录应包含 200 批");
    assert_eq!(
        all_batches[0].ops[0],
        ("directory".to_owned(), "f12".to_owned())
    );

    for seed in 0..5u64 {
        // 1. 影子侧：按原始记录重放 20 批，每批与全内容对照一致；偶数批 discard 后归零，奇数批经产品发布接口发布。
        {
            let root = tempdir_xfs("ln-sh-rand-s-");
            let host = root.path().join("host");
            let state = root.path().join("state");
            fs::create_dir(&host).unwrap();
            fs::write(host.join("a"), "seed-init\n").unwrap();

            let shadow = ShadowDir::open(&state, "w", &host, &[], &[]).unwrap();
            let shadow_batches: Vec<&RandomizedBatch> = all_batches
                .iter()
                .filter(|b| b.seed == seed && b.side == "shadow")
                .collect();
            assert_eq!(shadow_batches.len(), 20);
            for rec in shadow_batches {
                let host_before_batch = oracle_scan(&host, &[]);
                replay_random_batch(shadow.tree_path(), seed, rec.batch, &rec.ops);
                let actual = status_pairs(&shadow);
                let base_o = oracle_scan(shadow.base_path(), &[]);
                let tree_o = oracle_scan(shadow.tree_path(), &[]);
                let expected = oracle_diff(&base_o, &tree_o);
                assert_eq!(
                    actual, expected,
                    "影子侧随机批 seed={seed} batch={} 与全内容对照不一致",
                    rec.batch
                );

                if rec.batch % 2 == 0 {
                    shadow.discard(None, None).expect("随机批 discard 失败");
                    assert!(
                        shadow.status().unwrap().is_empty(),
                        "seed={seed} batch={} discard 后应无待定",
                        rec.batch
                    );
                    let tree_after = oracle_scan(shadow.tree_path(), &[]);
                    assert!(
                        oracle_diff(&base_o, &tree_after).is_empty(),
                        "seed={seed} batch={} discard 后 tree 应等于 base",
                        rec.batch
                    );
                    assert!(
                        oracle_diff(&host_before_batch, &oracle_scan(&host, &[])).is_empty(),
                        "seed={seed} batch={} discard 不得改动宿主",
                        rec.batch
                    );
                } else {
                    let call_hash = call_record_hash(&format!("rand-pub-{seed}-{}", rec.batch));
                    let prep = prepare_publish(&state, &call_hash, &[&shadow], None, None)
                        .unwrap_or_else(|e| {
                            panic!(
                                "seed={seed} batch={} prepare_publish 失败：{e:?}",
                                rec.batch
                            )
                        });
                    match prep {
                        PublishPrep::Empty => {
                            assert!(
                                expected.is_empty(),
                                "seed={seed} batch={} 非空待定不应返回 Empty",
                                rec.batch
                            );
                            assert!(
                                oracle_diff(&host_before_batch, &oracle_scan(&host, &[]))
                                    .is_empty()
                            );
                        }
                        PublishPrep::Ready(prepared) => {
                            let out =
                                prepared
                                    .execute(&[&shadow], |_| Ok(()))
                                    .unwrap_or_else(|e| {
                                        panic!("seed={seed} batch={} execute 失败：{e}", rec.batch)
                                    });
                            assert!(
                                matches!(out, PublishOutcome::Committed { .. }),
                                "seed={seed} batch={} 应成功发布",
                                rec.batch
                            );
                            assert!(
                                shadow.status().unwrap().is_empty(),
                                "seed={seed} batch={} 发布后 status 应为空",
                                rec.batch
                            );
                            let host_after = oracle_scan(&host, &[]);
                            let base_after = oracle_scan(shadow.base_path(), &[]);
                            let tree_after = oracle_scan(shadow.tree_path(), &[]);
                            assert!(
                                oracle_diff(&host_after, &base_after).is_empty()
                                    && oracle_diff(&host_after, &tree_after).is_empty(),
                                "seed={seed} batch={} 发布后 host/base/tree 必须三方完全一致",
                                rec.batch
                            );
                            shadow.sync_from_host(None, None).unwrap();
                            assert!(
                                shadow.status().unwrap().is_empty(),
                                "seed={seed} batch={} 发布后再次 sync 不得产生假待定或假冲突",
                                rec.batch
                            );
                        }
                        PublishPrep::Conflicts(conflicts) => {
                            panic!(
                                "seed={seed} batch={} 宿主未改且无 hidden/外部硬链接，不应冲突：{conflicts:?}",
                                rec.batch
                            );
                        }
                    }
                }
            }
        }

        // 2. 宿主侧：按原始记录重放 20 批，每批同步后 tree 与 base 均与宿主全内容一致。
        // 注：宿主侧不维护 inode 索引，经硬链接改写的兄弟路径由后台 1 秒扫描按 stat 发现；
        // 若单次同步尚未覆盖，等待后台扫描产出候选后再同步即应收敛。
        {
            let root = tempdir_xfs("ln-sh-rand-h-");
            let host = root.path().join("host");
            let state = root.path().join("state");
            fs::create_dir(&host).unwrap();
            fs::write(host.join("a"), "seed-init\n").unwrap();

            let shadow = ShadowDir::open(&state, "w", &host, &[], &[]).unwrap();
            let ext_batches: Vec<&RandomizedBatch> = all_batches
                .iter()
                .filter(|b| b.seed == seed && b.side == "external")
                .collect();
            assert_eq!(ext_batches.len(), 20);
            for rec in ext_batches {
                replay_random_batch(&host, seed, rec.batch, &rec.ops);
                wait_host_sync_converged(
                    &host,
                    &shadow,
                    &format!("random-host seed={seed} batch={}", rec.batch),
                );
            }
        }
    }
}

// =============================================================================
// 4. host_sync_converges_after_unnotified_writes（45 个宿主同步观测）
// =============================================================================

fn wait_host_sync_converged(host: &Path, shadow: &ShadowDir, label: &str) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        shadow.sync_from_host(None, None).unwrap();
        let host_o = oracle_scan(host, &[]);
        let tree_o = oracle_scan(shadow.tree_path(), &[]);
        let base_o = oracle_scan(shadow.base_path(), &[]);
        if oracle_diff(&host_o, &tree_o).is_empty()
            && oracle_diff(&host_o, &base_o).is_empty()
            && shadow.status().unwrap().is_empty()
        {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "宿主同步观测 `{label}` 在 5s 内未收敛；host vs tree = {:?}, host vs base = {:?}, status = {:?}",
            oracle_diff(&host_o, &tree_o),
            oracle_diff(&host_o, &base_o),
            shadow.status().unwrap()
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn host_sync_converges_after_unnotified_writes() {
    // 1. 30 次长期 mmap 写入（关闭 fd、不 msync、保持映射全程打开）。
    {
        let root = tempdir_xfs("ln-sh-mmap-");
        let host = root.path().join("host");
        let state = root.path().join("state");
        fs::create_dir(&host).unwrap();
        fs::write(host.join("a"), vec![b'A'; 4096]).unwrap();

        let shadow = ShadowDir::open(&state, "w", &host, &[], &[]).unwrap();
        let fd = openat(CWD, host.join("a"), OFlags::RDWR, Mode::empty()).unwrap();
        // SAFETY: 映射长度为 4096 与文件等长，并在循环结束后 munmap。
        let ptr = unsafe {
            mmap(
                std::ptr::null_mut(),
                4096,
                PROT_READ | PROT_WRITE,
                MAP_SHARED,
                fd.as_raw_fd(),
                0,
            )
        };
        assert_ne!(ptr, usize::MAX as *mut c_void, "长期 mmap 失败");
        drop(fd);

        for rep in 0..30usize {
            // SAFETY: 写入已映射页的前 2 字节，无 msync。
            unsafe {
                *ptr.cast::<u8>() = 66 + (rep % 20) as u8;
                *ptr.cast::<u8>().add(1) = 70 + (rep % 20) as u8;
            }
            wait_host_sync_converged(&host, &shadow, &format!("long-lived-mmap#{rep}"));
        }

        // SAFETY: 匹配前述 mmap 映射参数。
        unsafe {
            munmap(ptr, 4096);
        }
    }

    // 2. 五类隐蔽写入各 3 次（共 15 次，合计 45 次观测）。
    let cases = [
        "outside-hardlink",
        "editor-rename",
        "tmpfile-linkat",
        "same-tick",
        "fake-mtime",
    ];
    for case in cases {
        for rep in 0..3usize {
            let root = tempdir_xfs("ln-sh-sync-");
            let host = root.path().join("host");
            let state = root.path().join("state");
            fs::create_dir(&host).unwrap();
            let orig = vec![b'a'; 4096];
            fs::write(host.join("a"), &orig).unwrap();

            let outside_alias = root.path().join(format!("outside-alias-{rep}"));
            if case == "outside-hardlink" {
                fs::hard_link(host.join("a"), &outside_alias).unwrap();
            }

            let shadow = ShadowDir::open(&state, "w", &host, &[], &[]).unwrap();
            let before_meta = fs::symlink_metadata(host.join("a")).unwrap();

            match case {
                "outside-hardlink" => {
                    fs::write(&outside_alias, b"alias-write").unwrap();
                }
                "editor-rename" => {
                    fs::write(host.join("new-save"), b"editor").unwrap();
                    fs::rename(host.join("new-save"), host.join("a")).unwrap();
                }
                "tmpfile-linkat" => {
                    let fd = openat(
                        CWD,
                        &host,
                        OFlags::TMPFILE | OFlags::RDWR,
                        Mode::from_raw_mode(0o644),
                    )
                    .unwrap();
                    let _ = rustix::io::write(&fd, b"AFTER!");
                    let proc_path = format!("/proc/self/fd/{}", fd.as_raw_fd());
                    linkat(
                        CWD,
                        &proc_path,
                        CWD,
                        host.join("new"),
                        AtFlags::SYMLINK_FOLLOW,
                    )
                    .unwrap();
                }
                "same-tick" => {
                    fs::write(host.join("a"), vec![b'X'; orig.len()]).unwrap();
                    fs::write(host.join("a"), vec![b'Y'; orig.len()]).unwrap();
                }
                "fake-mtime" => {
                    fs::write(host.join("a"), vec![b'Z'; orig.len()]).unwrap();
                    let ts = Timespec {
                        tv_sec: before_meta.mtime(),
                        tv_nsec: before_meta.mtime_nsec(),
                    };
                    utimensat(
                        CWD,
                        host.join("a"),
                        &Timestamps {
                            last_access: ts,
                            last_modification: ts,
                        },
                        AtFlags::empty(),
                    )
                    .unwrap();
                }
                _ => unreachable!(),
            }

            wait_host_sync_converged(&host, &shadow, &format!("{case}#{rep}"));
        }
    }
}

// =============================================================================
// 5. shadow_queue_overflow_matches_full_scan
// =============================================================================

#[test]
fn shadow_queue_overflow_matches_full_scan() {
    let max_queued: usize = fs::read_to_string("/proc/sys/fs/inotify/max_queued_events")
        .expect("读取 max_queued_events 失败")
        .trim()
        .parse()
        .expect("解析 max_queued_events 失败");

    let root = tempdir_xfs("ln-sh-ovf-");
    let host = root.path().join("host");
    let state = root.path().join("state");
    fs::create_dir(&host).unwrap();
    fs::write(host.join("keep.txt"), "base").unwrap();
    fs::write(host.join("del_me.txt"), "to-delete").unwrap();
    fs::write(host.join("mod_me.txt"), "before").unwrap();

    let shadow = ShadowDir::open(&state, "w", &host, &[], &[]).unwrap();

    // 在两次排空之间制造超过 max_queued_events 的真实事件，触发内核 IN_Q_OVERFLOW。
    let storm_path = shadow.tree_path().join("storm.txt");
    let iterations = max_queued + 2_000;
    for i in 0..iterations {
        let mut f = fs::File::create(&storm_path).unwrap();
        let _ = f.write_all(&[(b'a' + (i % 26) as u8)]);
    }
    fs::remove_file(&storm_path).unwrap();
    fs::remove_file(shadow.tree_path().join("del_me.txt")).unwrap();
    fs::write(shadow.tree_path().join("mod_me.txt"), "after-overflow").unwrap();
    fs::write(shadow.tree_path().join("added.txt"), "new-after-overflow").unwrap();

    // 溢出后应自动退回全量核对并重建监听，与独立遍历结果逐项一致。
    let actual = status_pairs(&shadow);
    let base_o = oracle_scan(shadow.base_path(), &[]);
    let tree_o = oracle_scan(shadow.tree_path(), &[]);
    let expected = oracle_diff(&base_o, &tree_o);
    assert_eq!(
        actual, expected,
        "触发真实 IN_Q_OVERFLOW 后 status 必须与全量对照一致"
    );
    assert_eq!(
        actual,
        vec![
            ("added.txt".to_owned(), PendingAction::Added),
            ("del_me.txt".to_owned(), PendingAction::Deleted),
            ("mod_me.txt".to_owned(), PendingAction::Modified),
        ]
    );

    // 确认监听已恢复：新建子目录并写入，再继续修改与新增，均能准确跟踪。
    let new_dir = shadow.tree_path().join("recovered_dir/nested");
    fs::create_dir_all(&new_dir).unwrap();
    fs::write(new_dir.join("first.txt"), "v1").unwrap();
    assert_eq!(
        status_pairs(&shadow),
        oracle_diff(&base_o, &oracle_scan(shadow.tree_path(), &[])),
        "溢出恢复后新建子目录并写入应被准确发现"
    );

    fs::write(new_dir.join("first.txt"), "v2").unwrap();
    fs::write(new_dir.join("second.txt"), "inc").unwrap();
    assert_eq!(
        status_pairs(&shadow),
        oracle_diff(&base_o, &oracle_scan(shadow.tree_path(), &[])),
        "溢出恢复后的子目录应已安装监听并继续增量跟踪后续写入"
    );
}

// =============================================================================
// 6. 第 2 轮审查补充复现测试（A1–A8、B1–B8）
// =============================================================================

#[test]
fn shadow_reconcile_failure_preserves_draft_on_sync() {
    // A1：同一批事件中，影子 a 改为 000 权限，z 写草稿，宿主也改写 z；
    // 核对在 a 处失败时不得丢弃 z 的事件，也不得让宿主覆盖草稿 z。
    let root = tempdir_xfs("ln-sh-a1-");
    let host = root.path().join("host");
    let state = root.path().join("state");
    fs::create_dir(&host).unwrap();
    fs::write(host.join("a"), "orig-a").unwrap();
    fs::write(host.join("z"), "orig-z").unwrap();

    let shadow = ShadowDir::open(&state, "w", &host, &[], &[]).unwrap();
    fs::set_permissions(
        shadow.tree_path().join("a"),
        fs::Permissions::from_mode(0o000),
    )
    .unwrap();
    fs::write(shadow.tree_path().join("z"), "draft-z").unwrap();
    fs::write(host.join("z"), "host-z").unwrap();

    let res = shadow.sync_from_host(None, None);
    assert!(
        res.is_err(),
        "影子核对失败时 sync_from_host 应中止并返回错误"
    );
    assert_eq!(
        fs::read_to_string(shadow.tree_path().join("z")).unwrap(),
        "draft-z",
        "影子核对失败时宿主版本不得覆盖草稿 z"
    );

    // 恢复 a 的可读权限后，再次同步应保留草稿 z 并标为冲突。
    fs::set_permissions(
        shadow.tree_path().join("a"),
        fs::Permissions::from_mode(0o644),
    )
    .unwrap();
    shadow
        .sync_from_host(None, None)
        .expect("恢复权限后同步应成功");
    assert_eq!(
        fs::read_to_string(shadow.tree_path().join("z")).unwrap(),
        "draft-z"
    );
    let st = shadow.status().unwrap();
    assert_eq!(st.len(), 1, "status={st:?}");
    assert_eq!(st[0].path, "z");
    assert_eq!(st[0].action, PendingAction::Modified);
    assert!(st[0].conflict, "z 应标记冲突：{st:?}");
}

#[test]
fn host_unreadable_file_is_not_treated_as_deleted() {
    // A2：已同步宿主文件改为 000 权限后读取返回 EACCES，不得删除 tree/ 与 base/ 中的该文件。
    let root = tempdir_xfs("ln-sh-a2-");
    let host = root.path().join("host");
    let state = root.path().join("state");
    fs::create_dir(&host).unwrap();
    fs::write(host.join("a"), "keep-me").unwrap();

    let shadow = ShadowDir::open(&state, "w", &host, &[], &[]).unwrap();
    fs::set_permissions(host.join("a"), fs::Permissions::from_mode(0o000)).unwrap();

    let res = shadow.sync_from_host(None, None);
    assert!(res.is_err(), "宿主文件不可读时同步应报错，而非静默成功");
    assert_eq!(
        fs::read_to_string(shadow.tree_path().join("a")).unwrap(),
        "keep-me",
        "tree/a 不得被当成已删除而移除"
    );
    assert_eq!(
        fs::read_to_string(shadow.base_path().join("a")).unwrap(),
        "keep-me",
        "base/a 不得被当成已删除而移除"
    );

    fs::set_permissions(host.join("a"), fs::Permissions::from_mode(0o644)).unwrap();
    shadow
        .sync_from_host(None, None)
        .expect("恢复权限后同步应成功");
    assert!(shadow.status().unwrap().is_empty());
}

#[test]
fn reset_rejects_collateral_before_deleting_draft() {
    // A3：基准有 d/x，影子把目录 d 换成含草稿的普通文件，宿主删除 d；
    // reset("d") 修改前应同时检查影子、基准与待定后代，拒绝且保留 d 的草稿字节。
    let root = tempdir_xfs("ln-sh-a3-");
    let host = root.path().join("host");
    let state = root.path().join("state");
    fs::create_dir_all(host.join("d")).unwrap();
    fs::write(host.join("d/x"), "base-x").unwrap();

    let shadow = ShadowDir::open(&state, "w", &host, &[], &[]).unwrap();
    fs::remove_dir_all(shadow.tree_path().join("d")).unwrap();
    fs::write(shadow.tree_path().join("d"), "precious-draft").unwrap();
    fs::remove_dir_all(host.join("d")).unwrap();

    shadow.sync_from_host(None, None).unwrap();
    let err = shadow
        .reset("d")
        .expect_err("存在待定后代 d/x 时 reset(\"d\") 应拒绝");
    assert!(err.contains('d'), "错误信息应包含调用路径 `d`：{err}");
    assert!(
        !err.contains("d/x"),
        "连带影响报错只应回显调用者给出的路径：{err}"
    );
    assert_eq!(
        fs::read_to_string(shadow.tree_path().join("d")).unwrap(),
        "precious-draft",
        "reset 被拒绝时不得先删掉草稿 d"
    );
    assert!(
        shadow.base_path().join("d/x").is_file(),
        "reset 被拒绝时不得破坏基准 base/d/x"
    );
}

#[test]
fn dir_replaced_with_symlink_syncs_without_following_link() {
    // A4：宿主原有 a/f、b/f，将目录 a 换成指向 b 的符号链接后，同步不得沿链接保留旧 a/f。
    let root = tempdir_xfs("ln-sh-a4-");
    let host = root.path().join("host");
    let state = root.path().join("state");
    fs::create_dir_all(host.join("a")).unwrap();
    fs::create_dir_all(host.join("b")).unwrap();
    fs::write(host.join("a/f"), "from-a").unwrap();
    fs::write(host.join("b/f"), "from-b").unwrap();

    let shadow = ShadowDir::open(&state, "w", &host, &[], &[]).unwrap();
    fs::remove_dir_all(host.join("a")).unwrap();
    symlink("b", host.join("a")).unwrap();

    shadow.sync_from_host(None, None).unwrap();
    assert!(
        fs::symlink_metadata(shadow.tree_path().join("a"))
            .unwrap()
            .file_type()
            .is_symlink(),
        "tree/a 应已同步为符号链接"
    );
    assert_eq!(
        fs::read_link(shadow.tree_path().join("a")).unwrap(),
        PathBuf::from("b")
    );
    assert!(
        fs::symlink_metadata(shadow.base_path().join("a"))
            .unwrap()
            .file_type()
            .is_symlink(),
        "base/a 应已同步为符号链接"
    );
    assert_eq!(
        fs::read_link(shadow.base_path().join("a")).unwrap(),
        PathBuf::from("b")
    );
    assert!(shadow.status().unwrap().is_empty());
}

#[test]
fn short_lived_hardlink_write_is_detected() {
    // A5：两次排空之间在影子内为 f 建短命硬链接、经别名写入并删除别名，status() 不得漏报。
    let root = tempdir_xfs("ln-sh-a5-");
    let host = root.path().join("host");
    let state = root.path().join("state");
    fs::create_dir(&host).unwrap();
    fs::write(host.join("f"), "before").unwrap();

    let shadow = ShadowDir::open(&state, "w", &host, &[], &[]).unwrap();
    let alias = shadow.tree_path().join("ephemeral_alias");
    fs::hard_link(shadow.tree_path().join("f"), &alias).unwrap();
    fs::write(&alias, "after!").unwrap();
    fs::remove_file(&alias).unwrap();

    assert_eq!(
        status_pairs(&shadow),
        vec![("f".to_owned(), PendingAction::Modified)],
        "短命硬链接修改 f 后 status() 必须检出 f 为 Modified"
    );
}

#[test]
fn dir_move_out_reuse_and_rename_exchange_keep_watches() {
    // A6 复现一：同一批事件中先将 d 移出影子再新建同名 d，后续写入 d/new.txt 必须被发现。
    {
        let root = tempdir_xfs("ln-sh-a6-reuse-");
        let host = root.path().join("host");
        let state = root.path().join("state");
        fs::create_dir_all(host.join("d")).unwrap();
        fs::write(host.join("d/old.txt"), "old").unwrap();

        let shadow = ShadowDir::open(&state, "w", &host, &[], &[]).unwrap();
        let outside = root.path().join("moved_out_d");
        fs::rename(shadow.tree_path().join("d"), &outside).unwrap();
        fs::create_dir(shadow.tree_path().join("d")).unwrap();
        let _ = shadow.status().unwrap();

        fs::write(shadow.tree_path().join("d/new.txt"), "fresh").unwrap();
        let st = status_pairs(&shadow);
        assert!(
            st.contains(&("d/new.txt".to_owned(), PendingAction::Added)),
            "移出并复用同名目录 d 后，写入 d/new.txt 必须被监听发现，实际：{st:?}"
        );
    }

    // A6 复现二：用 RENAME_EXCHANGE 交换两个已监听目录后，分别写入两侧都必须被发现。
    {
        let root = tempdir_xfs("ln-sh-a6-exch-");
        let host = root.path().join("host");
        let state = root.path().join("state");
        fs::create_dir_all(host.join("d1")).unwrap();
        fs::create_dir_all(host.join("d2")).unwrap();
        fs::write(host.join("d1/a.txt"), "a").unwrap();
        fs::write(host.join("d2/b.txt"), "b").unwrap();

        let shadow = ShadowDir::open(&state, "w", &host, &[], &[]).unwrap();
        renameat_with(
            CWD,
            shadow.tree_path().join("d1"),
            CWD,
            shadow.tree_path().join("d2"),
            RenameFlags::EXCHANGE,
        )
        .expect("RENAME_EXCHANGE 失败");
        let _ = shadow.status().unwrap();

        fs::write(shadow.tree_path().join("d1/new1.txt"), "1").unwrap();
        fs::write(shadow.tree_path().join("d2/new2.txt"), "2").unwrap();
        let st = status_pairs(&shadow);
        assert!(
            st.contains(&("d1/new1.txt".to_owned(), PendingAction::Added))
                && st.contains(&("d2/new2.txt".to_owned(), PendingAction::Added)),
            "RENAME_EXCHANGE 后两侧目录均应保持有效监听，实际：{st:?}"
        );
    }
}

#[test]
fn crash_recovery_cleans_tmp_and_restores_widened_modes() {
    // A7：影子同步在 0555 目录中建临时文件并放宽权限后崩溃，重启前清空 tmp/ 并按 widened 恢复权限，
    // status() 不得残留假草稿。
    let root = tempdir_xfs("ln-sh-a7-");
    let host = root.path().join("host");
    let state = root.path().join("state");
    fs::create_dir_all(host.join("ro")).unwrap();
    fs::write(host.join("ro/a.txt"), "v1").unwrap();
    fs::set_permissions(host.join("ro"), fs::Permissions::from_mode(0o555)).unwrap();

    let shadow = ShadowDir::open(&state, "w", &host, &[], &[]).unwrap();
    let shadow_root = state.join("shadows").join(shadow.shadow_key());
    let tmp_dir = shadow_root.join("tmp");
    let widened_file = shadow_root.join("widened");

    // 1. 正常在 0555 目录中同步一个文件：不留 tmp 文件，也不留 widened 记录，目录权限仍为 0555。
    fs::set_permissions(host.join("ro"), fs::Permissions::from_mode(0o755)).unwrap();
    fs::write(host.join("ro/b.txt"), "v2").unwrap();
    fs::set_permissions(host.join("ro"), fs::Permissions::from_mode(0o555)).unwrap();
    shadow.sync_from_host(None, None).unwrap();
    assert!(tmp_dir.is_dir(), "shadows/<key>/tmp/ 目录应已建立");
    assert!(
        fs::read_dir(&tmp_dir).unwrap().next().is_none(),
        "正常同步后 tmp/ 应为空"
    );
    assert!(
        !widened_file.exists(),
        "正常同步恢复权限后 shadows/<key>/widened 不应残留"
    );
    assert_eq!(
        fs::symlink_metadata(shadow.tree_path().join("ro"))
            .unwrap()
            .permissions()
            .mode()
            & 0o7777,
        0o555
    );
    drop(shadow);

    // 2. 模拟在放宽 tree/ro 并向 tmp/ 写入临时文件后发生 SIGKILL。
    fs::write(tmp_dir.join(".lancet-s-stale"), "partial").unwrap();
    fs::set_permissions(
        shadow_root.join("tree/ro"),
        fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    // 编码格式：`<tree|base> <octal_mode> <hex_rel>\n`（"ro" -> "726f"）。
    fs::write(&widened_file, "tree 0555 726f\n").unwrap();

    let reopened = ShadowDir::open(&state, "w", &host, &[], &[]).unwrap();
    assert!(
        fs::read_dir(&tmp_dir).unwrap().next().is_none(),
        "重启后应清空 shadows/<key>/tmp/"
    );
    assert!(
        !widened_file.exists(),
        "重启恢复放宽记录后应删除 shadows/<key>/widened"
    );
    assert_eq!(
        fs::symlink_metadata(reopened.tree_path().join("ro"))
            .unwrap()
            .permissions()
            .mode()
            & 0o7777,
        0o555,
        "重启后 tree/ro 应恢复为 0555"
    );
    assert!(
        reopened.status().unwrap().is_empty(),
        "崩溃恢复后 status() 不应列出假草稿：{:?}",
        reopened.status().unwrap()
    );

    let _ = fs::set_permissions(host.join("ro"), fs::Permissions::from_mode(0o700));
    let _ = fs::set_permissions(
        reopened.tree_path().join("ro"),
        fs::Permissions::from_mode(0o700),
    );
    let _ = fs::set_permissions(
        reopened.base_path().join("ro"),
        fs::Permissions::from_mode(0o700),
    );
}

#[test]
fn write_failure_rolls_back_created_dirs() {
    // A8 复现一：write("created/" + "x"*256, "x") 失败时应回滚已新建的空目录 created。
    let root = tempdir_xfs("ln-sh-a8-wr-");
    let host = root.path().join("host");
    let state = root.path().join("state");
    fs::create_dir(&host).unwrap();

    let shadow = ShadowDir::open(&state, "w", &host, &[], &[]).unwrap();
    let bad_path = format!("created/{}", "x".repeat(256));
    assert!(
        shadow.write(&bad_path, "x").is_err(),
        "超长文件名应写入失败"
    );
    assert!(
        !shadow.tree_path().join("created").exists(),
        "失败的 write 应回滚本次新建的空目录 `created`"
    );
    assert!(shadow.status().unwrap().is_empty());
}

#[test]
fn discard_records_partial_effects_and_clears_added_dir_with_regenerable() {
    // B7：空根下 write("new/build/seed", "x") 后，discard 应连同新增目录下的 regenerable 一并删除。
    {
        let root = tempdir_xfs("ln-sh-b7-");
        let host = root.path().join("host");
        let state = root.path().join("state");
        fs::create_dir(&host).unwrap();

        let shadow = ShadowDir::open(&state, "w", &host, &[], &["build/".to_owned()]).unwrap();
        shadow.write("new/build/seed", "x").unwrap();
        assert!(shadow.tree_path().join("new/build/seed").is_file());

        let effects = shadow
            .discard(None, None)
            .expect("discard 应能删除含 regenerable 的新增常规目录");
        assert!(
            !shadow.tree_path().join("new").exists(),
            "discard 后新增目录 new 应被彻底删除"
        );
        assert!(shadow.status().unwrap().is_empty());
        assert!(effects.iter().any(|e| e.target == "new"));
    }

    // A8 复现二：discard 删掉换型的 a 后在后续路径报错时，partial 必须包含已落地的 a。
    {
        let root = tempdir_xfs("ln-sh-a8-disc-");
        let host = root.path().join("host");
        let state = root.path().join("state");
        fs::create_dir(&host).unwrap();
        fs::write(host.join("a"), "base-a").unwrap();
        fs::write(host.join("z"), "base-z").unwrap();

        let shadow = ShadowDir::open(&state, "w", &host, &[], &[]).unwrap();
        // a 换型为目录；z 换型为内含未跟踪 FIFO 的目录，令 Pass 1 删掉 a 后在 z 处报 ENOTEMPTY。
        fs::remove_file(shadow.tree_path().join("a")).unwrap();
        fs::create_dir(shadow.tree_path().join("a")).unwrap();
        fs::remove_file(shadow.tree_path().join("z")).unwrap();
        fs::create_dir(shadow.tree_path().join("z")).unwrap();
        mknodat(
            CWD,
            shadow.tree_path().join("z/fifo"),
            rustix::fs::FileType::Fifo,
            Mode::from_raw_mode(0o600),
            0,
        )
        .unwrap();

        match shadow.discard(None, None) {
            Err(DiscardError::Failed { partial, .. }) => {
                let targets: Vec<&str> = partial.iter().map(|e| e.target.as_str()).collect();
                assert_eq!(
                    targets,
                    vec!["a"],
                    "Pass 1 已删掉换型的 a，报错交回的 partial 必须记录 a：{partial:?}"
                );
            }
            other => panic!("期望 DiscardError::Failed 并携带 partial=[a]，实际：{other:?}"),
        }
    }
}

#[test]
fn host_only_file_under_pending_dir_is_deferred() {
    // B1：空基准下影子新建 d/mine，宿主另建 d/other；同步不得把 d/other 混进待定集合。
    let root = tempdir_xfs("ln-sh-b1-");
    let host = root.path().join("host");
    let state = root.path().join("state");
    fs::create_dir(&host).unwrap();

    let shadow = ShadowDir::open(&state, "w", &host, &[], &[]).unwrap();
    shadow.write("d/mine", "mine").unwrap();
    fs::create_dir(host.join("d")).unwrap();
    fs::write(host.join("d/other"), "other").unwrap();

    shadow.sync_from_host(None, None).unwrap();
    assert!(
        !shadow.tree_path().join("d/other").exists(),
        "宿主独有的 d/other 在基准缺少 d 时不得写入影子"
    );
    let paths: Vec<String> = shadow
        .status()
        .unwrap()
        .into_iter()
        .map(|c| c.path)
        .collect();
    assert_eq!(
        paths,
        vec!["d".to_owned(), "d/mine".to_owned()],
        "d/other 不得混入待定集合"
    );

    // discard 清除待定目录 d 后，下次同步应接纳宿主的 d 与 d/other。
    shadow.discard(None, None).unwrap();
    shadow.sync_from_host(None, None).unwrap();
    assert_eq!(
        fs::read_to_string(shadow.tree_path().join("d/other")).unwrap(),
        "other"
    );
    assert_eq!(
        fs::read_to_string(shadow.base_path().join("d/other")).unwrap(),
        "other"
    );
    assert!(shadow.status().unwrap().is_empty());
}

#[test]
fn conflict_recomputes_for_all_pending_paths_without_host_events() {
    // B2：待定路径即使未产生宿主 inotify 事件（如经树外硬链接修改/恢复），每次同步都应重算冲突。
    let root = tempdir_xfs("ln-sh-b2-");
    let host = root.path().join("host");
    let state = root.path().join("state");
    fs::create_dir(&host).unwrap();
    fs::write(host.join("a"), "base-a").unwrap();
    let outside = root.path().join("outside-alias");
    fs::hard_link(host.join("a"), &outside).unwrap();

    let shadow = ShadowDir::open(&state, "w", &host, &[], &[]).unwrap();
    shadow.write("a", "draft-a").unwrap();

    // 经树外硬链接改写宿主 a，不等后台 1s 扫描直接同步，应立即标出冲突。
    fs::write(&outside, "host-b").unwrap();
    shadow.sync_from_host(None, None).unwrap();
    let st1 = shadow.status().unwrap();
    assert_eq!(st1.len(), 1);
    assert!(st1[0].conflict, "未发 host inotify 时也应重算冲突：{st1:?}");

    // 经树外硬链接恢复为基准后直接同步，应立即清除冲突。
    fs::write(&outside, "base-a").unwrap();
    shadow.sync_from_host(None, None).unwrap();
    let st2 = shadow.status().unwrap();
    assert_eq!(st2.len(), 1);
    assert!(!st2[0].conflict, "宿主恢复为基准后应立即清除冲突：{st2:?}");
}

#[test]
fn mmap_writes_survive_shadow_reopen() {
    // B3：跨节点重启持有的长期 MAP_SHARED 映射，重启重建宿主观测须经 FICLONE 重新武装写保护。
    let root = tempdir_xfs("ln-sh-b3-");
    let host = root.path().join("host");
    let state = root.path().join("state");
    fs::create_dir(&host).unwrap();
    fs::write(host.join("a"), vec![b'A'; 4096]).unwrap();

    let shadow1 = ShadowDir::open(&state, "w", &host, &[], &[]).unwrap();
    let fd = openat(CWD, host.join("a"), OFlags::RDWR, Mode::empty()).unwrap();
    // SAFETY: 映射与文件等长，测试结束前 munmap。
    let ptr = unsafe {
        mmap(
            std::ptr::null_mut(),
            4096,
            PROT_READ | PROT_WRITE,
            MAP_SHARED,
            fd.as_raw_fd(),
            0,
        )
    };
    assert_ne!(ptr, usize::MAX as *mut c_void);
    drop(fd);

    // 重启前写一次并等待同步，再以同字节写入把已映射页重新变脏（使内容仍等于基准，重启时不会触发同步安装）。
    unsafe {
        *ptr.cast::<u8>() = b'B';
    }
    wait_host_sync_converged(&host, &shadow1, "before-reopen-mmap");
    unsafe {
        *ptr.cast::<u8>() = b'B';
    }
    drop(shadow1);

    // 越过 2 秒 racy 窗口后再重启影子，使 reopen 后的后台扫描不能靠 racy 窗口兜底，
    // 必须靠 reopen 经 FICLONE 重新武装写保护。
    std::thread::sleep(Duration::from_millis(2_200));
    let shadow2 = ShadowDir::open(&state, "w", &host, &[], &[]).unwrap();

    // 重启后再经同一长期映射写入，不 msync，后台扫描必须能发现并同步。
    unsafe {
        *ptr.cast::<u8>() = b'D';
    }
    wait_host_sync_converged(&host, &shadow2, "after-reopen-mmap");

    unsafe {
        assert_eq!(munmap(ptr, 4096), 0);
    }
}

#[test]
fn interrupted_sync_preserves_unfinished_additions() {
    // B4：同步在 Pass 1 中途因截止中断时，已遍历过（深度更深）但要到 Pass 2 才安装的新增项不得丢失。
    let root = tempdir_xfs("ln-sh-b4-");
    let host = root.path().join("host");
    let state = root.path().join("state");
    fs::create_dir_all(host.join("deep")).unwrap();
    for i in 0..400 {
        fs::write(host.join(format!("del_{i:03}.txt")), "old").unwrap();
    }

    let shadow = ShadowDir::open(&state, "w", &host, &[], &[]).unwrap();
    // 宿主同时删除 400 个浅层文件（depth 0），并在更深目录（depth 1）新增文件：
    // Pass 1 按深度降序先遍历到 deep/add_*.txt（跳过留待 Pass 2），随后在删除 del_* 期间触发中断。
    for i in 0..400 {
        fs::remove_file(host.join(format!("del_{i:03}.txt"))).unwrap();
    }
    for i in 0..20 {
        fs::write(
            host.join(format!("deep/add_{i:02}.txt")),
            format!("add-{i}"),
        )
        .unwrap();
    }

    // 用短预算令中断落在 Pass 1/2 过程中；反复推进直至被中断过且剩余待同步。
    let mut saw_interrupt = false;
    for budget_us in [500u64, 1_500, 3_000, 6_000, 10_000] {
        let dl = Instant::now() + Duration::from_micros(budget_us);
        if shadow.sync_from_host(Some(dl), None).is_err() {
            saw_interrupt = true;
            // 若已删掉至少一个浅层文件，说明中断已发生在 Pass 1 中途（已越过深层新增项）。
            let any_deleted =
                (0..400).any(|i| !shadow.tree_path().join(format!("del_{i:03}.txt")).exists());
            if any_deleted {
                break;
            }
        } else {
            break;
        }
    }
    assert!(saw_interrupt, "短预算同步应至少发生一次中断");

    // 取消预算限制后再次同步（不等后台 1s 扫描，直接依赖保留的候选），所有新增项必须完整落地。
    shadow.sync_from_host(None, None).unwrap();
    for i in 0..400 {
        assert!(!shadow.tree_path().join(format!("del_{i:03}.txt")).exists());
    }
    for i in 0..20 {
        assert_eq!(
            fs::read_to_string(shadow.tree_path().join(format!("deep/add_{i:02}.txt"))).unwrap(),
            format!("add-{i}"),
            "Pass 1 中断后深层新增项 deep/add_{i:02}.txt 不得丢失"
        );
    }
    assert!(shadow.status().unwrap().is_empty());
}

#[test]
fn dir_only_hidden_rule_handles_files_and_type_swaps() {
    // B5 & B6：规则 `cache/` 仅匹配目录，可见普通文件 `cache` 允许 write/reset，
    // 换型为隐藏目录时宿主同步与影子 discard 均须正确处理旧可见版本。
    let root = tempdir_xfs("ln-sh-b56-");
    let host = root.path().join("host");
    let state = root.path().join("state");
    fs::create_dir(&host).unwrap();
    fs::write(host.join("cache"), "visible-file").unwrap();
    fs::create_dir_all(host.join("box")).unwrap();

    let hidden = vec!["cache/".to_owned(), "onlydir/".to_owned()];
    let shadow = ShadowDir::open(&state, "w", &host, &hidden, &[]).unwrap();

    // B5a：类型已知为普通文件 `cache` 时，write 与 reset 均应允许。
    shadow
        .write("cache", "draft-file")
        .expect("普通文件 cache 应可写入");
    shadow.reset("cache").expect("普通文件 cache 应可 reset");
    assert_eq!(
        shadow.read("cache").unwrap().as_deref(),
        Some("visible-file")
    );

    // B5b：末项缺失且命中目录规则 `onlydir/` 时，write 与 reset 均按文件与目录各判一次而拒绝。
    assert!(
        shadow.write("onlydir", "x").is_err(),
        "缺失的 onlydir 应被目录 hidden 规则拒绝"
    );
    assert!(
        shadow.reset("onlydir").is_err(),
        "缺失的 onlydir 执行 reset 也应被 hidden 规则拒绝"
    );

    // B5c：连带影响的报错只回显调用者给出的路径，不得泄漏隐藏子项名。
    fs::create_dir_all(shadow.tree_path().join("box/cache")).unwrap();
    fs::write(shadow.tree_path().join("box/cache/secret"), "s").unwrap();
    // 先使 box 本身成为待定（修改权限触发 pending），宿主删除 box，同步后 box 冲突且下有隐藏待定子项。
    fs::set_permissions(
        shadow.tree_path().join("box"),
        fs::Permissions::from_mode(0o700),
    )
    .unwrap();
    fs::remove_dir_all(host.join("box")).unwrap();
    shadow.sync_from_host(None, None).unwrap();
    let err = shadow
        .reset("box")
        .expect_err("box 下有隐藏待定子项时 reset(\"box\") 应报连带影响");
    assert!(err.contains("box"), "{err}");
    assert!(
        !err.contains("cache") && !err.contains("secret"),
        "连带影响报错不得泄漏隐藏子项名：{err}"
    );
    shadow.discard(None, None).unwrap();
    shadow.sync_from_host(None, None).unwrap();

    // B6a：影子侧把可见普通文件 cache 换成隐藏目录 cache/，discard 应清除隐藏目录并恢复基准文件。
    fs::remove_file(shadow.tree_path().join("cache")).unwrap();
    fs::create_dir(shadow.tree_path().join("cache")).unwrap();
    fs::write(shadow.tree_path().join("cache/inner"), "secret").unwrap();
    shadow
        .discard(None, None)
        .expect("discard 应能恢复被隐藏目录替换的基准文件");
    assert_eq!(
        fs::read_to_string(shadow.tree_path().join("cache")).unwrap(),
        "visible-file",
        "discard 清除隐藏目录后应恢复基准中的可见文件 cache"
    );
    assert!(
        shadow.status().unwrap().is_empty(),
        "discard 后 status() 应为空：{:?}",
        shadow.status().unwrap()
    );

    // B6b：宿主侧把可见普通文件 cache 换成隐藏目录 cache/，同步后影子不得继续读到旧文件。
    fs::remove_file(host.join("cache")).unwrap();
    fs::create_dir(host.join("cache")).unwrap();
    fs::write(host.join("cache/h"), "host-secret").unwrap();
    shadow.sync_from_host(None, None).unwrap();
    assert!(
        !shadow.tree_path().join("cache").exists(),
        "宿主将可见文件换成隐藏目录后，影子中的旧文件应移除"
    );
    assert!(
        !shadow.base_path().join("cache").exists(),
        "宿主将可见文件换成隐藏目录后，基准中的旧文件应移除"
    );
    assert!(shadow.status().unwrap().is_empty());
}

#[test]
fn non_utf8_filename_is_cloned_and_rejected_by_entries() {
    // B8：非 UTF-8 可见文件名建影与同步均应保留原字节名，entries("") 在转 WIT 字符串时报编码错误。
    let root = tempdir_xfs("ln-sh-b8-");
    let host = root.path().join("host");
    let state = root.path().join("state");
    fs::create_dir(&host).unwrap();
    let bad_name = OsStr::from_bytes(b"bad_\xff.txt");
    fs::write(host.join(bad_name), "raw-bytes").unwrap();

    let shadow = ShadowDir::open(&state, "w", &host, &[], &[]).unwrap();
    assert!(
        shadow.tree_path().join(bad_name).is_file(),
        "建影时应复制非 UTF-8 可见文件到 tree/"
    );
    assert!(
        shadow.base_path().join(bad_name).is_file(),
        "建影时应复制非 UTF-8 可见文件到 base/"
    );
    let err = shadow
        .entries("")
        .expect_err("可见子项名称不是有效 UTF-8 时 entries 应报错");
    assert!(err.contains("UTF-8"), "实际报错：{err}");

    // 宿主新增另一个非 UTF-8 文件也应同步到 tree/ 与 base/。
    let bad_name2 = OsStr::from_bytes(b"sync_\xfe.txt");
    fs::write(host.join(bad_name2), "synced").unwrap();
    shadow.sync_from_host(None, None).unwrap();
    assert_eq!(
        fs::read_to_string(shadow.tree_path().join(bad_name2)).unwrap(),
        "synced"
    );
    assert_eq!(
        fs::read_to_string(shadow.base_path().join(bad_name2)).unwrap(),
        "synced"
    );
}

#[test]
fn recreated_hardlink_alias_write_is_detected() {
    // 第 3 轮 A1：同批事件内删除已有独立文件 alias、用硬链接重建为 a 的别名、经 alias 写入草稿再删除 alias，
    // status() 必须检出 a 的修改，且后续宿主修改 a 同步时必须标为冲突并保留草稿。
    let root = tempdir_xfs("ln-sh-r3a1-");
    let host = root.path().join("host");
    let state = root.path().join("state");
    fs::create_dir(&host).unwrap();
    fs::write(host.join("a"), "base-a").unwrap();
    fs::write(host.join("alias"), "base-alias").unwrap();

    let shadow = ShadowDir::open(&state, "w", &host, &[], &[]).unwrap();
    let tree_a = shadow.tree_path().join("a");
    let tree_alias = shadow.tree_path().join("alias");

    fs::remove_file(&tree_alias).unwrap();
    fs::hard_link(&tree_a, &tree_alias).unwrap();
    fs::write(&tree_alias, "draft").unwrap();
    fs::remove_file(&tree_alias).unwrap();

    assert_eq!(
        status_pairs(&shadow),
        vec![
            ("a".to_owned(), PendingAction::Modified),
            ("alias".to_owned(), PendingAction::Deleted),
        ],
        "重建同名硬链接写入并删除后，status() 不得漏报 a 的修改"
    );

    fs::write(host.join("a"), "host").unwrap();
    shadow.sync_from_host(None, None).unwrap();
    let st = shadow.status().unwrap();
    let a_change = st
        .iter()
        .find(|c| c.path == "a")
        .expect("同步后 a 仍应位于待定集合");
    assert_eq!(a_change.action, PendingAction::Modified);
    assert!(a_change.conflict, "宿主改写 a 后同步应将 a 标为冲突");
    assert_eq!(
        fs::read_to_string(&tree_a).unwrap(),
        "draft",
        "宿主同步不得覆盖 a 的草稿"
    );
}

#[test]
fn non_utf8_real_path_write_and_reset_fail_before_mutation() {
    // 第 3 轮 A2：经符号链接解析出的真实路径不是有效 UTF-8 时，write 与 reset 必须在修改磁盘之前报错，
    // 影子、基准与待定集合均保持不变；regenerable 路径记规则名，不要求真实路径为 UTF-8。
    let root = tempdir_xfs("ln-sh-r3a2-");
    let host = root.path().join("host");
    let state = root.path().join("state");
    let bad_dir = OsStr::from_bytes(b"d\xff");
    let regen_bad_dir = Path::new("regen").join(bad_dir);
    fs::create_dir_all(host.join(bad_dir)).unwrap();
    fs::write(host.join(bad_dir).join("x"), "base").unwrap();
    symlink(bad_dir, host.join("alias")).unwrap();
    fs::create_dir_all(host.join(&regen_bad_dir)).unwrap();
    symlink(&regen_bad_dir, host.join("rlink")).unwrap();

    let shadow = ShadowDir::open(&state, "w", &host, &[], &["regen/".to_owned()]).unwrap();
    let tree_x = shadow.tree_path().join(bad_dir).join("x");
    let base_x = shadow.base_path().join(bad_dir).join("x");

    // 1. 写入已有文件：解析后真实路径非 UTF-8，必须在落盘前报错。
    let write_err = shadow
        .write("alias/x", "draft")
        .expect_err("真实路径非 UTF-8 时 write 应报错");
    assert!(write_err.contains("UTF-8"), "实际报错：{write_err}");
    assert_eq!(
        fs::read_to_string(&tree_x).unwrap(),
        "base",
        "write 报错后不得已经修改影子文件"
    );
    assert_eq!(fs::read_to_string(&base_x).unwrap(), "base");
    assert_eq!(shadow.visible_pending_count(), 0);
    assert!(shadow.status().unwrap().is_empty());

    // 2. 写入缺失文件并需创建上级目录：必须在建目录前报错。
    let write_new_err = shadow
        .write("alias/sub/y", "draft")
        .expect_err("真实路径非 UTF-8 且需建目录时 write 应报错");
    assert!(write_new_err.contains("UTF-8"), "实际报错：{write_new_err}");
    assert!(
        !shadow.tree_path().join(bad_dir).join("sub").exists(),
        "write 报错前不得创建上级目录"
    );
    assert_eq!(shadow.visible_pending_count(), 0);

    // 3. 手动把 d\xff/x 改写为待定草稿后执行 reset("alias/x")：必须在恢复文件前报错，且待定集合不变。
    fs::write(&tree_x, "manual-draft").unwrap();
    assert_eq!(shadow.visible_pending_count(), 1);
    let reset_err = shadow
        .reset("alias/x")
        .expect_err("真实路径非 UTF-8 时 reset 应报错");
    assert!(reset_err.contains("UTF-8"), "实际报错：{reset_err}");
    assert_eq!(
        fs::read_to_string(&tree_x).unwrap(),
        "manual-draft",
        "reset 报错后不得已经改写影子文件"
    );
    assert_eq!(fs::read_to_string(&base_x).unwrap(), "base");
    assert_eq!(
        shadow.visible_pending_count(),
        1,
        "reset 报错后待定集合应保持不变"
    );

    // 4. regenerable 目标记的是规则名，真实路径包含非 UTF-8 字节仍允许写入。
    let regen_eff = shadow
        .write("rlink/seed", "seed-data")
        .expect("regenerable 目标记录规则名，不要求真实路径为 UTF-8")
        .expect("新写种子文件应产生 WriteEffect");
    assert_eq!(regen_eff.target, "regen/");
    assert_eq!(
        fs::read_to_string(shadow.tree_path().join(&regen_bad_dir).join("seed")).unwrap(),
        "seed-data"
    );
}

#[test]
fn widened_recovery_skips_non_directory_entries() {
    // 第 3 轮 B1：影子中 p 仍是 0644 普通文件而 widened 记有 p -> 0555（换型为目录前崩溃的磁盘状态），
    // 重启恢复时不得对非目录对象执行 chmod 0555，status() 应为空。
    let root = tempdir_xfs("ln-sh-r3b1-");
    let host = root.path().join("host");
    let state = root.path().join("state");
    fs::create_dir(&host).unwrap();
    fs::write(host.join("p"), "file-body").unwrap();
    fs::set_permissions(host.join("p"), fs::Permissions::from_mode(0o644)).unwrap();

    let shadow = ShadowDir::open(&state, "w", &host, &[], &[]).unwrap();
    let shadow_root = state.join("shadows").join(shadow.shadow_key());
    let widened_file = shadow_root.join("widened");
    drop(shadow);

    // "p" -> hex "70"
    fs::write(&widened_file, "tree 0555 70\n").unwrap();

    let reopened = ShadowDir::open(&state, "w", &host, &[], &[]).unwrap();
    assert!(!widened_file.exists(), "恢复后应删除 widened 文件");
    assert_eq!(
        fs::symlink_metadata(reopened.tree_path().join("p"))
            .unwrap()
            .permissions()
            .mode()
            & 0o7777,
        0o644,
        "widened 恢复不得修改普通文件 p 的权限"
    );
    assert!(
        reopened.status().unwrap().is_empty(),
        "重启后 status() 应为空：{:?}",
        reopened.status().unwrap()
    );
}

#[test]
#[ignore = "write_emfile_opening_new_parent_rolls_back_dirs 内部调用的子进程入口"]
fn emfile_rollback_helper() {
    if std::env::var_os("LANCET_EMFILE_HELPER").is_none() {
        return;
    }
    let root = tempdir_xfs("ln-sh-r3b2-");
    let host = root.path().join("host");
    let state = root.path().join("state");
    fs::create_dir(&host).unwrap();

    let shadow = ShadowDir::open(&state, "w", &host, &[], &[]).unwrap();

    let mut orig = RLimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: orig 为有效的栈上 RLimit 结构体指针。
    assert_eq!(unsafe { getrlimit(RLIMIT_NOFILE, &mut orig) }, 0);

    let max_fd = fs::read_dir("/proc/self/fd")
        .unwrap()
        .flatten()
        .filter_map(|e| e.file_name().to_str().and_then(|s| s.parse::<u64>().ok()))
        .max()
        .unwrap_or(16);
    let tight = RLimit {
        rlim_cur: (max_fd + 4).min(orig.rlim_max),
        rlim_max: orig.rlim_max,
    };
    // SAFETY: tight 指向合法 RLimit，仅收紧当前子进程的软上限。
    assert_eq!(unsafe { setrlimit(RLIMIT_NOFILE, &tight) }, 0);

    // 占满全部剩余 fd 槽位，再精确释放 1 个：
    // write("new/deep/file", "x") 建好 new 并打开 new（占掉该槽位），再建好 new/deep 后打开它必然报 EMFILE。
    let mut fillers: Vec<OwnedFd> = Vec::new();
    loop {
        match openat(
            CWD,
            "/dev/null",
            OFlags::RDONLY | OFlags::CLOEXEC,
            Mode::empty(),
        ) {
            Ok(fd) => fillers.push(fd),
            Err(rustix::io::Errno::MFILE) => break,
            Err(err) => panic!("填充 fd 时遇到非 EMFILE 错误：{err}"),
        }
    }
    assert!(
        fillers.pop().is_some(),
        "应至少留出 1 个可释放的 fd 槽位用于打开首层目录"
    );

    let write_res = shadow.write("new/deep/file", "x");

    drop(fillers);
    // SAFETY: 恢复原始软上限以继续检查状态与清理。
    assert_eq!(unsafe { setrlimit(RLIMIT_NOFILE, &orig) }, 0);

    let err = write_res.expect_err("fd 耗尽时打开新建上级目录应报错");
    assert!(
        !shadow.tree_path().join("new/deep").exists(),
        "EMFILE 报错后应回滚新建目录 new/deep（报错：{err}）"
    );
    assert!(
        !shadow.tree_path().join("new").exists(),
        "EMFILE 报错后应回滚新建目录 new（报错：{err}）"
    );
    assert!(
        shadow.status().unwrap().is_empty(),
        "EMFILE 报错后 status() 应为空：{:?}",
        shadow.status().unwrap()
    );
}

#[test]
fn write_emfile_opening_new_parent_rolls_back_dirs() {
    // 第 3 轮 B2：子进程在精确 RLIMIT_NOFILE 限制下执行 write("new/deep/file", "x")，
    // 建好 new/deep 后 openat2 报 EMFILE，回滚必须先释放目录 fd 并删除 new/deep 与 new。
    let exe = std::env::current_exe().expect("获取测试可执行文件路径失败");
    let out = Command::new(&exe)
        .env("LANCET_EMFILE_HELPER", "1")
        .args([
            "--exact",
            "emfile_rollback_helper",
            "--ignored",
            "--test-threads=1",
        ])
        .output()
        .expect("启动 EMFILE 子进程测试失败");
    assert!(
        out.status.success(),
        "EMFILE 回滚子进程失败：{:?}\nstdout:\n{}\nstderr:\n{}",
        out.status,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn interrupted_recursive_dir_sync_retries_completely() {
    // 第 3 轮 B3：基准已有空目录 d，宿主用一次改名把含 2,000 个文件的目录换到 d；
    // 对一组递增预算执行“带预算同步 + 立即无预算重试”，每次重试后影子与基准均须与宿主全内容一致。
    let budgets_us = [50u64, 150, 400, 1_000, 2_500, 6_000];
    let mut saw_interrupt = false;

    for budget_us in budgets_us {
        let root = tempdir_xfs("ln-sh-r3b3-");
        let host = root.path().join("host");
        let state = root.path().join("state");
        fs::create_dir_all(host.join("d")).unwrap();

        let shadow = ShadowDir::open(&state, "w", &host, &[], &[]).unwrap();

        let staged = root.path().join("staged_d");
        fs::create_dir(&staged).unwrap();
        for i in 0..2_000 {
            fs::write(staged.join(format!("f_{i:04}.txt")), format!("v{i}")).unwrap();
        }
        // POSIX rename 原子覆盖空目录 d，发出目录的 MOVED_TO 递归事件。
        fs::rename(&staged, host.join("d")).unwrap();

        let dl = Instant::now() + Duration::from_micros(budget_us);
        if shadow.sync_from_host(Some(dl), None).is_err() {
            saw_interrupt = true;
        }

        // 立即无预算重试，不等待后台扫描。
        shadow
            .sync_from_host(None, None)
            .unwrap_or_else(|e| panic!("budget={budget_us}us 无预算重试失败：{e:?}"));

        let host_oracle = oracle_scan(&host, &[]);
        let tree_oracle = oracle_scan(shadow.tree_path(), &[]);
        let base_oracle = oracle_scan(shadow.base_path(), &[]);
        assert_eq!(
            tree_oracle,
            host_oracle,
            "budget={budget_us}us 重试后影子目录树与宿主不一致（影子条目数={}，宿主条目数={}）",
            tree_oracle.len(),
            host_oracle.len()
        );
        assert_eq!(
            base_oracle,
            host_oracle,
            "budget={budget_us}us 重试后基准目录树与宿主不一致（基准条目数={}，宿主条目数={}）",
            base_oracle.len(),
            host_oracle.len()
        );
        assert!(
            shadow.status().unwrap().is_empty(),
            "budget={budget_us}us 重试后 status() 应为空"
        );
    }

    assert!(
        saw_interrupt,
        "递增预算序列中应至少有一次在同步期间触发中断"
    );
}

fn read_thread_rchar() -> u64 {
    let text = fs::read_to_string("/proc/thread-self/io").expect("读取 /proc/thread-self/io 失败");
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("rchar:") {
            return rest.trim().parse::<u64>().expect("解析 rchar 数值失败");
        }
    }
    panic!("/proc/thread-self/io 缺少 rchar 字段：\n{text}");
}

#[test]
fn stable_conflict_sync_does_not_reread_file_content() {
    // 第 3 轮 B4：8 MiB 文件形成冲突并过 racy 窗后，无变化的同步不得重复读取文件内容；
    // 冲突保持不变，宿主改回基准后冲突清除。
    let root = tempdir_xfs("ln-sh-r3b4-");
    let host = root.path().join("host");
    let state = root.path().join("state");
    fs::create_dir(&host).unwrap();

    const FILE_BYTES: usize = 8 * 1024 * 1024;
    let base_bytes = vec![b'A'; FILE_BYTES];
    let host_bytes = vec![b'B'; FILE_BYTES];
    fs::write(host.join("big.bin"), &base_bytes).unwrap();

    let shadow = ShadowDir::open(&state, "w", &host, &[], &[]).unwrap();
    shadow.write("big.bin", "draft").unwrap();
    fs::write(host.join("big.bin"), &host_bytes).unwrap();

    // 首次同步标出冲突。
    shadow.sync_from_host(None, None).unwrap();
    let st0 = shadow.status().unwrap();
    assert_eq!(st0.len(), 1);
    assert!(st0[0].conflict, "首次同步后 big.bin 应标为冲突");

    // 越过 2 秒 racy 时间窗，执行一次同步以完成 racy 复核并排空早期后台候选。
    std::thread::sleep(Duration::from_millis(2_200));
    shadow.sync_from_host(None, None).unwrap();

    // 稳定冲突后的无变化同步：rchar 增量应远小于 8 MiB。
    let r0 = read_thread_rchar();
    shadow.sync_from_host(None, None).unwrap();
    let delta1 = read_thread_rchar().saturating_sub(r0);
    assert!(
        delta1 < 256 * 1024,
        "稳定冲突后的无变化同步读取了 {delta1} 字节，远超预期（文件为 {FILE_BYTES} 字节）"
    );
    let st1 = shadow.status().unwrap();
    assert_eq!(st1.len(), 1);
    assert!(st1[0].conflict, "稳定冲突应继续保留");

    // 再经过一轮后台扫描间隔后无变化同步，仍不得重读 8 MiB 内容。
    std::thread::sleep(Duration::from_millis(1_200));
    let r1 = read_thread_rchar();
    shadow.sync_from_host(None, None).unwrap();
    let delta2 = read_thread_rchar().saturating_sub(r1);
    assert!(
        delta2 < 256 * 1024,
        "后台扫描一轮后稳定冲突同步读取了 {delta2} 字节，不应重复读文件内容"
    );
    assert!(shadow.status().unwrap()[0].conflict);

    // 宿主改回基准内容后，同步最多读一次文件内容，且应清除冲突。
    fs::write(host.join("big.bin"), &base_bytes).unwrap();
    let r2 = read_thread_rchar();
    shadow.sync_from_host(None, None).unwrap();
    let delta_revert = read_thread_rchar().saturating_sub(r2);
    assert!(
        delta_revert < (FILE_BYTES as u64) + 512 * 1024,
        "有新事件的冲突路径在同一次同步中最多读一次内容，实际读取了 {delta_revert} 字节"
    );
    let st2 = shadow.status().unwrap();
    assert_eq!(st2.len(), 1);
    assert!(!st2[0].conflict, "宿主改回基准后冲突应清除");
}

#[test]
fn background_scanner_reemits_reappearing_host_version() {
    // 第 4 轮 A1：基准为 A、影子为 C；经宿主树外硬链接写 B，等后台扫描后同步得到冲突；
    // 在下一轮扫描前经同一硬链接写回 A 并同步，再分别用 discard()、reset、写回 A 解除待定，
    // 最后再次经硬链接写成 B：后台扫描必须重新接纳全部三条路径的 B。
    let root = tempdir_xfs("ln-sh-r4a1-");
    let host = root.path().join("host");
    let state = root.path().join("state");
    let outside = root.path().join("outside");
    fs::create_dir(&host).unwrap();
    fs::create_dir(&outside).unwrap();

    let names = ["f_discard", "f_reset", "f_write"];
    for name in names {
        fs::write(host.join(name), "A").unwrap();
        fs::hard_link(host.join(name), outside.join(name)).unwrap();
    }
    // 哨兵文件同样经树外硬链接改写（无 inotify），用来确认后台扫描已完整跑过一轮。
    fs::write(host.join("probe"), "0").unwrap();
    fs::hard_link(host.join("probe"), outside.join("probe")).unwrap();

    let shadow = ShadowDir::open(&state, "w", &host, &[], &[]).unwrap();
    for name in names {
        shadow.write(name, "C").unwrap();
    }

    // 1. 经宿主树外硬链接把三条路径写成 B，同时改写哨兵为 "1"，等待后台扫描完成并同步。
    for name in names {
        fs::write(outside.join(name), "B").unwrap();
    }
    fs::write(outside.join("probe"), "1").unwrap();

    let deadline1 = Instant::now() + Duration::from_secs(5);
    loop {
        shadow.sync_from_host(None, None).unwrap();
        if fs::read_to_string(shadow.tree_path().join("probe")).unwrap() == "1" {
            break;
        }
        assert!(Instant::now() < deadline1, "等待首轮后台扫描接纳哨兵超时");
        std::thread::sleep(Duration::from_millis(50));
    }

    let st_conflict = shadow.status().unwrap();
    assert_eq!(st_conflict.len(), 3);
    assert!(
        st_conflict.iter().all(|c| c.conflict),
        "首轮后台扫描同步后三条路径均应标为冲突：{st_conflict:?}"
    );

    // 2. 紧接着（在下一轮 1s 扫描之前）经同一树外硬链接写回 A 并同步，解除冲突。
    for name in names {
        fs::write(outside.join(name), "A").unwrap();
    }
    shadow.sync_from_host(None, None).unwrap();
    let st_cleared = shadow.status().unwrap();
    assert_eq!(st_cleared.len(), 3);
    assert!(
        st_cleared.iter().all(|c| !c.conflict),
        "写回 A 并同步后三条路径冲突应全部清除：{st_cleared:?}"
    );

    // 3. 分别用 reset、把影子写回 A、discard() 三种方式解除待定。
    assert!(shadow.reset("f_reset").unwrap().is_some());
    assert!(shadow.write("f_write", "A").unwrap().is_some());
    let discarded = shadow.discard(None, None).unwrap();
    assert!(
        !discarded.is_empty(),
        "discard() 应清除剩余的待定路径 f_discard"
    );
    assert!(
        shadow.status().unwrap().is_empty(),
        "三种方式解除待定后 status() 应为空"
    );

    // 4. 再次经树外硬链接把三条路径写成 B，同时把哨兵写成 "2"，等后台扫描接纳。
    for name in names {
        fs::write(outside.join(name), "B").unwrap();
    }
    fs::write(outside.join("probe"), "2").unwrap();

    let deadline2 = Instant::now() + Duration::from_secs(5);
    loop {
        shadow.sync_from_host(None, None).unwrap();
        if fs::read_to_string(shadow.tree_path().join("probe")).unwrap() == "2" {
            break;
        }
        assert!(Instant::now() < deadline2, "等待第二轮后台扫描接纳哨兵超时");
        std::thread::sleep(Duration::from_millis(50));
    }

    // 再多给一轮扫描机会，避免边界时序影响结论。
    fs::write(outside.join("probe"), "3").unwrap();
    let deadline3 = Instant::now() + Duration::from_secs(5);
    loop {
        shadow.sync_from_host(None, None).unwrap();
        if fs::read_to_string(shadow.tree_path().join("probe")).unwrap() == "3" {
            break;
        }
        assert!(Instant::now() < deadline3, "等待第三轮后台扫描接纳哨兵超时");
        std::thread::sleep(Duration::from_millis(50));
    }

    for name in names {
        assert_eq!(
            fs::read_to_string(shadow.tree_path().join(name)).unwrap(),
            "B",
            "路径 {name} 的影子应重新接纳再次出现的宿主版本 B"
        );
        assert_eq!(
            fs::read_to_string(shadow.base_path().join(name)).unwrap(),
            "B",
            "路径 {name} 的基准应重新接纳再次出现的宿主版本 B"
        );
    }
    assert!(shadow.status().unwrap().is_empty());
}

#[test]
fn overlong_path_write_is_rejected_before_creating_dirs() {
    // 第 4 轮 A3：超长路径写入（分量 > 255 字节，或影子绝对路径 > 4095 字节）必须在首次 mkdirat
    // 之前报错，不留新目录，报错不泄露影子绝对路径，且 status() 与重新打开均正常；已有文件同理。
    let root = tempdir_xfs("ln-sh-r4a3-");
    let host = root.path().join("host");
    let state = root.path().join("state");
    fs::create_dir(&host).unwrap();

    let shadow = ShadowDir::open(&state, "w", &host, &[], &[]).unwrap();
    let canonical_tree = fs::canonicalize(shadow.tree_path()).unwrap();
    let tree_abs_str = canonical_tree.to_string_lossy().into_owned();

    // 1. leader 的复现输入：18 级 250 字节缺失目录 + 256 字节叶名。
    let mut parts: Vec<String> = (0..18).map(|_| "a".repeat(250)).collect();
    parts.push("b".repeat(256));
    let leader_path = parts.join("/");
    let err1 = shadow
        .write(&leader_path, "x")
        .expect_err("超长分量与超长路径写入必须报错");
    assert!(
        !err1.contains(&tree_abs_str),
        "报错不得泄露影子绝对路径：{err1}"
    );
    assert!(
        fs::read_dir(shadow.tree_path()).unwrap().next().is_none(),
        "报错后影子 tree/ 不得留下任何新建目录"
    );
    assert!(shadow.status().unwrap().is_empty());

    drop(shadow);
    let shadow = ShadowDir::open(&state, "w", &host, &[], &[])
        .expect("超长路径写入失败后重新打开影子必须成功");
    assert!(shadow.status().unwrap().is_empty());

    // 2. 各分量均合法（<= 255 字节），仅影子中的绝对路径总长超过 4095 字节（精确落在 4096）。
    let root_len = canonical_tree.as_os_str().as_bytes().len();
    let seg = "d".repeat(200);
    let mut valid_parts: Vec<String> = Vec::new();
    let mut rel_len = 0usize;
    while 4096 - (root_len + 1 + rel_len) > 255 {
        valid_parts.push(seg.clone());
        rel_len += 201;
    }
    let remaining_for_4096 = 4096 - (root_len + 1 + rel_len);
    assert!(
        (1..=255).contains(&remaining_for_4096),
        "叶文件名长度应落在 1..=255，实际为 {remaining_for_4096}"
    );
    valid_parts.push("f".repeat(remaining_for_4096));
    let over_4095_rel = valid_parts.join("/");
    assert_eq!(root_len + 1 + over_4095_rel.len(), 4096);
    assert!(over_4095_rel.len() <= 4095);

    let err2 = shadow
        .write(&over_4095_rel, "x")
        .expect_err("总长 4096 字节的缺失路径写入必须报错");
    assert!(
        !err2.contains(&tree_abs_str),
        "报错不得泄露影子绝对路径：{err2}"
    );
    assert!(
        fs::read_dir(shadow.tree_path()).unwrap().next().is_none(),
        "总长超限报错后影子 tree/ 不得留下任何新建目录"
    );
    assert!(shadow.status().unwrap().is_empty());

    drop(shadow);
    let reopened = ShadowDir::open(&state, "w", &host, &[], &[])
        .expect("总长超限写入失败后重新打开影子必须成功");
    assert!(reopened.status().unwrap().is_empty());

    // 边界值 4095 字节（含 NUL 正好 4096）应允许写入，且 status() 与重新打开均正常。
    let mut boundary_parts = valid_parts;
    boundary_parts.pop();
    boundary_parts.push("f".repeat(remaining_for_4096 - 1));
    let exact_4095_rel = boundary_parts.join("/");
    assert_eq!(root_len + 1 + exact_4095_rel.len(), 4095);
    assert!(
        reopened.write(&exact_4095_rel, "ok").unwrap().is_some(),
        "恰好 4095 字节的合法路径应允许写入"
    );
    assert!(!reopened.status().unwrap().is_empty());
    drop(reopened);
    let reopened_after_4095 = ShadowDir::open(&state, "w", &host, &[], &[])
        .expect("写入 4095 字节边界路径后重新打开影子必须成功");
    assert!(!reopened_after_4095.status().unwrap().is_empty());

    // 3. 已有文件的总长超限写入：相对路径 <= 4095 可被 openat2 打开，但影子绝对路径 4096 > 4095 仍须报错。
    let root2 = tempdir_xfs("ln-sh-r4a3-ex-");
    let host2 = root2.path().join("host");
    let state2 = root2.path().join("state");
    fs::create_dir(&host2).unwrap();
    let shadow2 = ShadowDir::open(&state2, "w", &host2, &[], &[]).unwrap();
    let tree2_abs = fs::canonicalize(shadow2.tree_path()).unwrap();
    let tree2_str = tree2_abs.to_string_lossy().into_owned();
    let root2_len = tree2_abs.as_os_str().as_bytes().len();

    let mut ex_parts: Vec<String> = Vec::new();
    let mut ex_rel_len = 0usize;
    while 4096 - (root2_len + 1 + ex_rel_len) > 255 {
        ex_parts.push(seg.clone());
        ex_rel_len += 201;
    }
    let ex_leaf_len = 4096 - (root2_len + 1 + ex_rel_len);
    assert!((1..=255).contains(&ex_leaf_len));
    let ex_leaf = "f".repeat(ex_leaf_len);
    ex_parts.push(ex_leaf.clone());
    let ex_rel = ex_parts.join("/");
    assert_eq!(root2_len + 1 + ex_rel.len(), 4096);

    // 逐级 mkdirat + openat 在 tree/ 内直接建出该超长已有文件。
    let mut dir_fd = openat(
        CWD,
        shadow2.tree_path(),
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .unwrap();
    for dir_name in &ex_parts[..ex_parts.len() - 1] {
        mkdirat(
            dir_fd.as_fd(),
            dir_name.as_str(),
            Mode::from_raw_mode(0o755),
        )
        .unwrap();
        dir_fd = openat(
            dir_fd.as_fd(),
            dir_name.as_str(),
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .unwrap();
    }
    {
        let fd = openat(
            dir_fd.as_fd(),
            ex_leaf.as_str(),
            OFlags::WRONLY | OFlags::CREATE | OFlags::TRUNC | OFlags::CLOEXEC,
            Mode::from_raw_mode(0o644),
        )
        .unwrap();
        let mut f = fs::File::from(fd);
        f.write_all(b"old").unwrap();
    }

    let err3 = shadow2
        .write(&ex_rel, "new")
        .expect_err("总长 4096 字节的已有文件写入必须报错");
    assert!(
        !err3.contains(&tree2_str),
        "已有文件超长报错不得泄露影子绝对路径：{err3}"
    );
    let mut body = String::new();
    let rd_fd = openat(
        dir_fd.as_fd(),
        ex_leaf.as_str(),
        OFlags::RDONLY | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .unwrap();
    fs::File::from(rd_fd).read_to_string(&mut body).unwrap();
    assert_eq!(body, "old", "超长已有文件报错后原内容不得被修改");
}

#[test]
fn seed_write_rejects_non_utf8_created_regular_parent() {
    // 第 4 轮 B1：alias 指向宿主已有目录 dir\xff，regenerable = ["build/"]；
    // write("alias/new/build/x", "x") 会顺带新建常规上级目录 dir\xff/new（非 UTF-8），
    // 必须在创建任何目录前报错，影子保持不变。
    let root = tempdir_xfs("ln-sh-r4b1-");
    let host = root.path().join("host");
    let state = root.path().join("state");
    let bad_dir = OsStr::from_bytes(b"dir\xff");
    fs::create_dir_all(host.join(bad_dir)).unwrap();
    symlink(bad_dir, host.join("alias")).unwrap();

    let shadow = ShadowDir::open(&state, "w", &host, &[], &["build/".to_owned()]).unwrap();
    let err = shadow
        .write("alias/new/build/x", "x")
        .expect_err("写种子时需新建非 UTF-8 常规上级目录必须报错");
    assert!(err.contains("UTF-8"), "实际报错：{err}");
    assert!(
        !shadow.tree_path().join(bad_dir).join("new").exists(),
        "报错前不得创建常规上级目录 dir\\xff/new"
    );
    assert!(
        shadow.status().unwrap().is_empty(),
        "报错后影子待定集合应保持为空"
    );
}

#[test]
fn reset_on_non_pending_non_utf8_seed_returns_none() {
    // 第 4 轮 B2：alias 指向 regenerable 下的 build/d\xff；write("alias/seed", "S") 成功后
    // status() 为空，对同一路径调用 reset 必须返回 Ok(None) 而不报 UTF-8 错误。
    let root = tempdir_xfs("ln-sh-r4b2-");
    let host = root.path().join("host");
    let state = root.path().join("state");
    let regen_bad_dir = Path::new("build").join(OsStr::from_bytes(b"d\xff"));
    fs::create_dir_all(host.join(&regen_bad_dir)).unwrap();
    symlink(&regen_bad_dir, host.join("alias")).unwrap();

    let shadow = ShadowDir::open(&state, "w", &host, &[], &["build/".to_owned()]).unwrap();
    let eff = shadow
        .write("alias/seed", "S")
        .expect("regenerable 种子写入应成功")
        .expect("新写种子文件应产生 WriteEffect");
    assert_eq!(eff.target, "build/");
    assert!(shadow.status().unwrap().is_empty());

    let reset_res = shadow
        .reset("alias/seed")
        .expect("没有待定改动的路径调用 reset 不应报错");
    assert!(reset_res.is_none(), "没有待定改动时 reset 应返回 Ok(None)");
    assert_eq!(
        fs::read_to_string(shadow.tree_path().join(&regen_bad_dir).join("seed")).unwrap(),
        "S",
        "reset 返回 None 后种子文件应保持不变"
    );
    assert!(shadow.status().unwrap().is_empty());
}

// =============================================================================
// 7. Slice 4b 发布事务库层测试
// =============================================================================

#[test]
fn publish_path_kinds() {
    let root = tempdir_xfs("ln-sh-pub-kinds-");
    let host = root.path().join("host");
    let state = root.path().join("state");
    fs::create_dir_all(host.join(".git")).unwrap();
    fs::create_dir_all(host.join("cache")).unwrap();
    fs::create_dir_all(host.join("del_dir/sub")).unwrap();
    fs::create_dir_all(host.join("ro_dir")).unwrap();
    fs::create_dir_all(host.join("chmod_dir")).unwrap();
    fs::create_dir_all(host.join("widen_dir")).unwrap();
    fs::create_dir_all(host.join("swap_d2f")).unwrap();
    fs::create_dir_all(host.join("rename_dir/sub")).unwrap();

    fs::write(host.join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
    fs::write(host.join("cache/seed.txt"), "host-seed\n").unwrap();
    fs::write(host.join("keep.txt"), "keep\n").unwrap();
    fs::write(host.join("mod_content.txt"), "before-mod\n").unwrap();
    fs::set_permissions(
        host.join("mod_content.txt"),
        fs::Permissions::from_mode(0o640),
    )
    .unwrap();
    fs::write(host.join("chmod_file.txt"), "same-bytes\n").unwrap();
    fs::set_permissions(
        host.join("chmod_file.txt"),
        fs::Permissions::from_mode(0o644),
    )
    .unwrap();
    fs::write(host.join("del_file.txt"), "bye\n").unwrap();
    fs::write(host.join("del_dir/sub/f.txt"), "deep-del\n").unwrap();
    fs::write(host.join("ro_dir/inside.txt"), "ro-before\n").unwrap();
    fs::set_permissions(host.join("ro_dir"), fs::Permissions::from_mode(0o555)).unwrap();
    fs::set_permissions(host.join("widen_dir"), fs::Permissions::from_mode(0o555)).unwrap();
    fs::write(host.join("swap_d2f/old.txt"), "d2f\n").unwrap();
    fs::write(host.join("swap_f2d"), "f2d\n").unwrap();
    fs::write(host.join("rename_dir/a.txt"), "ra\n").unwrap();
    fs::write(host.join("rename_dir/sub/b.txt"), "rb\n").unwrap();
    symlink("keep.txt", host.join("link_dangling")).unwrap();

    let shadow = ShadowDir::open(&state, "w", &host, &[], &["cache/".to_owned()]).unwrap();
    let tree = shadow.tree_path();

    // 各类改动全部放入同一批次：
    fs::write(tree.join("mod_content.txt"), "after-mod\n").unwrap();
    fs::set_permissions(
        tree.join("chmod_file.txt"),
        fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    fs::remove_file(tree.join("del_file.txt")).unwrap();
    fs::remove_dir_all(tree.join("del_dir")).unwrap();

    // 只读祖先目录 ro_dir 内部修改（阶段 1 放宽 + 阶段 5 恢复）。
    fs::set_permissions(tree.join("ro_dir"), fs::Permissions::from_mode(0o755)).unwrap();
    fs::write(tree.join("ro_dir/inside.txt"), "ro-after\n").unwrap();
    fs::set_permissions(tree.join("ro_dir"), fs::Permissions::from_mode(0o555)).unwrap();

    // 目录仅改权限（收紧与放宽）。
    fs::set_permissions(tree.join("chmod_dir"), fs::Permissions::from_mode(0o555)).unwrap();
    fs::set_permissions(tree.join("widen_dir"), fs::Permissions::from_mode(0o755)).unwrap();

    // 目录 <-> 文件互换。
    fs::remove_dir_all(tree.join("swap_d2f")).unwrap();
    fs::write(tree.join("swap_d2f"), "now-a-file\n").unwrap();
    fs::remove_file(tree.join("swap_f2d")).unwrap();
    fs::create_dir(tree.join("swap_f2d")).unwrap();
    fs::write(tree.join("swap_f2d/child.txt"), "now-a-dir\n").unwrap();

    // 目录改名（逐路径删除 + 创建）。
    fs::rename(tree.join("rename_dir"), tree.join("renamed_dir")).unwrap();

    // 新建只读目录及子文件。
    fs::create_dir_all(tree.join("new_ro/deep")).unwrap();
    fs::write(tree.join("new_ro/deep/added.txt"), "added\n").unwrap();
    fs::set_permissions(tree.join("new_ro/deep"), fs::Permissions::from_mode(0o555)).unwrap();
    fs::set_permissions(tree.join("new_ro"), fs::Permissions::from_mode(0o555)).unwrap();

    // 符号链接改为悬空符号链接。
    fs::remove_file(tree.join("link_dangling")).unwrap();
    symlink("nonexistent_target", tree.join("link_dangling")).unwrap();

    // 影子内新建 FIFO 与修改 regenerable：两者均不得发布到宿主。
    mknodat(
        CWD,
        tree.join("shadow_fifo"),
        rustix::fs::FileType::Fifo,
        Mode::from_raw_mode(0o600),
        0,
    )
    .unwrap();
    fs::write(tree.join("cache/seed.txt"), "shadow-seed-edited\n").unwrap();

    let ino_mod_before = fs::symlink_metadata(tree.join("mod_content.txt"))
        .unwrap()
        .ino();
    let ino_add_before = fs::symlink_metadata(tree.join("new_ro/deep/added.txt"))
        .unwrap()
        .ino();

    let call_hash = call_record_hash("pub-kinds-1");
    let prep = prepare_publish(&state, &call_hash, &[&shadow], None, None).unwrap();
    let PublishPrep::Ready(prepared) = prep else {
        panic!("预期 Ready");
    };
    let out = prepared.execute(&[&shadow], |_| Ok(())).unwrap();
    assert!(matches!(out, PublishOutcome::Committed { .. }));
    assert!(shadow.status().unwrap().is_empty());

    // 独立全内容对照：host、base、tree 三方完全一致（排除 .git 与 cache）。
    let skip = ["cache"];
    let host_o = oracle_scan(&host, &skip);
    let base_o = oracle_scan(shadow.base_path(), &skip);
    let tree_o = oracle_scan(shadow.tree_path(), &skip);
    assert!(
        oracle_diff(&host_o, &base_o).is_empty(),
        "host vs base 差异：{:?}",
        oracle_diff(&host_o, &base_o)
    );
    assert!(
        oracle_diff(&host_o, &tree_o).is_empty(),
        "host vs tree 差异：{:?}",
        oracle_diff(&host_o, &tree_o)
    );

    // 核对关键权限与悬空符号链接。
    assert_eq!(
        fs::symlink_metadata(host.join("ro_dir")).unwrap().mode() & 0o7777,
        0o555
    );
    assert_eq!(
        fs::symlink_metadata(host.join("chmod_dir")).unwrap().mode() & 0o7777,
        0o555
    );
    assert_eq!(
        fs::symlink_metadata(host.join("widen_dir")).unwrap().mode() & 0o7777,
        0o755
    );
    assert_eq!(
        fs::symlink_metadata(host.join("new_ro")).unwrap().mode() & 0o7777,
        0o555
    );
    assert_eq!(
        fs::symlink_metadata(host.join("new_ro/deep"))
            .unwrap()
            .mode()
            & 0o7777,
        0o555
    );
    assert_eq!(
        fs::read_link(host.join("link_dangling")).unwrap(),
        PathBuf::from("nonexistent_target")
    );

    // 核对影子常规文件 inode 保持不变、FIFO/.git/regenerable 均未污染宿主。
    assert_eq!(
        fs::symlink_metadata(tree.join("mod_content.txt"))
            .unwrap()
            .ino(),
        ino_mod_before,
        "发布不得改变影子常规文件 inode"
    );
    assert_eq!(
        fs::symlink_metadata(tree.join("new_ro/deep/added.txt"))
            .unwrap()
            .ino(),
        ino_add_before,
        "发布不得改变影子新增常规文件 inode"
    );
    assert!(!host.join("shadow_fifo").exists());
    assert_eq!(
        fs::read_to_string(host.join(".git/HEAD")).unwrap(),
        "ref: refs/heads/main\n"
    );
    assert_eq!(
        fs::read_to_string(host.join("cache/seed.txt")).unwrap(),
        "host-seed\n"
    );

    // 子用例 2：宿主存在同名 FIFO，影子新增常规文件 -> 报 HostChanged 冲突，FIFO 完好。
    mknodat(
        CWD,
        host.join("host_fifo"),
        rustix::fs::FileType::Fifo,
        Mode::from_raw_mode(0o600),
        0,
    )
    .unwrap();
    shadow.write("host_fifo", "regular\n").unwrap();
    let prep_fifo = prepare_publish(
        &state,
        &call_record_hash("pub-kinds-fifo"),
        &[&shadow],
        None,
        None,
    )
    .unwrap();
    match prep_fifo {
        PublishPrep::Conflicts(c) => {
            assert_eq!(c.len(), 1);
            assert_eq!(c[0].rel, "host_fifo");
            assert_eq!(c[0].reason, ConflictReason::HostChanged);
        }
        _ => panic!("宿主同名 FIFO 应报冲突"),
    }
    assert!(
        fs::symlink_metadata(host.join("host_fifo"))
            .unwrap()
            .file_type()
            .is_fifo()
    );
    shadow.discard(None, None).unwrap();

    // 子用例 3：待删目录在宿主中多出批次外子项（含 FIFO 与普通文件）-> 报 HostChanged 冲突，宿主不动。
    fs::create_dir(host.join("extra_dir")).unwrap();
    fs::write(host.join("extra_dir/tracked.txt"), "t\n").unwrap();
    shadow.sync_from_host(None, None).unwrap();
    fs::remove_dir_all(tree.join("extra_dir")).unwrap();
    mknodat(
        CWD,
        host.join("extra_dir/untracked_fifo"),
        rustix::fs::FileType::Fifo,
        Mode::from_raw_mode(0o600),
        0,
    )
    .unwrap();
    let prep_extra = prepare_publish(
        &state,
        &call_record_hash("pub-kinds-extra"),
        &[&shadow],
        None,
        None,
    )
    .unwrap();
    match prep_extra {
        PublishPrep::Conflicts(c) => {
            assert!(
                c.iter()
                    .any(|item| item.rel == "extra_dir"
                        && item.reason == ConflictReason::HostChanged),
                "含未删子项的待删目录应报 HostChanged：{c:?}"
            );
        }
        _ => panic!("含批次外子项的待删目录应报冲突"),
    }
    assert!(host.join("extra_dir/tracked.txt").exists());
    assert!(host.join("extra_dir/untracked_fifo").exists());

    // 放宽只读目录权限以便 TempDir 回收。
    for base_dir in [host.as_path(), shadow.tree_path(), shadow.base_path()] {
        for rel in ["ro_dir", "chmod_dir", "new_ro", "new_ro/deep"] {
            let _ = fs::set_permissions(base_dir.join(rel), fs::Permissions::from_mode(0o700));
        }
    }
}

#[test]
fn publish_advances_base_without_hiding_external_events() {
    let root = tempdir_xfs("ln-sh-pub-events-");
    let host = root.path().join("host");
    let state = root.path().join("state");
    let outside = root.path().join("outside");
    fs::create_dir(&host).unwrap();
    fs::create_dir(&outside).unwrap();

    fs::write(host.join("clean.txt"), "c-0\n").unwrap();
    fs::write(host.join("bg_file.txt"), "bg-0\n").unwrap();
    fs::hard_link(host.join("bg_file.txt"), outside.join("bg_alias")).unwrap();
    fs::write(host.join("mod_file.txt"), "m-0\n").unwrap();
    fs::write(host.join("del_file.txt"), "d-0\n").unwrap();
    fs::write(host.join("revert_target.txt"), "rev-0\n").unwrap();

    let shadow = ShadowDir::open(&state, "w", &host, &[], &[]).unwrap();

    // 1. 干净发布后重复同步，不产生待定改动或伪冲突。
    shadow.write("clean.txt", "c-pub\n").unwrap();
    let prep0 = prepare_publish(
        &state,
        &call_record_hash("pub-events-clean"),
        &[&shadow],
        None,
        None,
    )
    .unwrap();
    let PublishPrep::Ready(prepared0) = prep0 else {
        panic!("预期 Ready");
    };
    assert!(matches!(
        prepared0.execute(&[&shadow], |_| Ok(())).unwrap(),
        PublishOutcome::Committed { .. }
    ));
    shadow.sync_from_host(None, None).unwrap();
    shadow.sync_from_host(None, None).unwrap();
    assert!(
        shadow.status().unwrap().is_empty(),
        "干净发布后重复 sync 不应产生假待定或假冲突"
    );

    // 2. 在发布前通过树外硬链接修改 bg_file（无 inotify），并等待超过一轮后台扫描周期，
    //    确保 bg_file 在交换发生前就已进入后台扫描器的就绪候选集合。
    fs::write(outside.join("bg_alias"), "bg-ext\n").unwrap();
    std::thread::sleep(Duration::from_millis(1_500));

    // 准备发布批次：修改 mod_file、revert_target，新增 add_file 与 post_dir/init.txt，删除 del_file。
    shadow.write("mod_file.txt", "m-pub\n").unwrap();
    shadow.write("revert_target.txt", "rev-pub\n").unwrap();
    shadow.write("add_file.txt", "a-pub\n").unwrap();
    shadow.write("post_dir/init.txt", "p-pub\n").unwrap();
    fs::remove_file(shadow.tree_path().join("del_file.txt")).unwrap();

    let prep = prepare_publish(
        &state,
        &call_record_hash("pub-events-1"),
        &[&shadow],
        None,
        None,
    )
    .unwrap();
    let PublishPrep::Ready(prepared) = prep else {
        panic!("预期 Ready");
    };
    let out = prepared.execute(&[&shadow], |_| Ok(())).unwrap();
    assert!(matches!(out, PublishOutcome::Committed { .. }));
    assert!(shadow.status().unwrap().is_empty());

    // 3. 在不先调用 sync_from_host 的前提下，立即写入外部宿主修改，
    //    使节点自写事件、发布前已就绪的后台候选与紧随其后的外部修改落在同一次待处理窗口里。
    fs::write(host.join("mod_file.txt"), "m-host-after\n").unwrap();
    fs::write(host.join("add_file.txt"), "a-host-after\n").unwrap();
    fs::write(host.join("post_dir/ext.txt"), "p-host-after\n").unwrap();
    fs::write(host.join("del_file.txt"), "d-host-reborn\n").unwrap();

    // 仅调用一次 sync_from_host，必须立即接纳全部真实宿主版本（不靠后续多轮扫描补救）。
    shadow.sync_from_host(None, None).unwrap();
    assert_eq!(
        shadow.read("bg_file.txt").unwrap().as_deref(),
        Some("bg-ext\n"),
        "发布前已就绪的后台扫描候选必须在首次同步被接纳"
    );
    assert_eq!(
        shadow.read("mod_file.txt").unwrap().as_deref(),
        Some("m-host-after\n"),
        "同窗口内紧随发布的修改必须在首次同步被接纳"
    );
    assert_eq!(
        shadow.read("add_file.txt").unwrap().as_deref(),
        Some("a-host-after\n"),
        "同窗口内对新增文件的后续修改必须在首次同步被接纳"
    );
    assert_eq!(
        shadow.read("post_dir/ext.txt").unwrap().as_deref(),
        Some("p-host-after\n"),
        "同窗口内在新发布目录中创建的外部文件必须在首次同步被接纳"
    );
    assert_eq!(
        shadow.read("del_file.txt").unwrap().as_deref(),
        Some("d-host-reborn\n"),
        "同窗口内对刚删除文件的重建必须在首次同步被接纳"
    );
    assert!(shadow.status().unwrap().is_empty());

    // 4. 核对后台扫描器的基准补丁（patch_baseline）：把刚发布的 revert_target 经树外硬链接改回发布前内容 rev-0，
    //    若扫描器未收到基准补丁则会误以为与旧基准 rev-0 一致而漏报；收到补丁后会立即检出并接纳 rev-0。
    fs::hard_link(host.join("revert_target.txt"), outside.join("revert_alias")).unwrap();
    fs::write(outside.join("revert_alias"), "rev-0\n").unwrap();
    let dl = Instant::now() + Duration::from_secs(5);
    loop {
        shadow.sync_from_host(None, None).unwrap();
        if shadow.read("revert_target.txt").unwrap().as_deref() == Some("rev-0\n") {
            break;
        }
        assert!(
            Instant::now() < dl,
            "后台扫描器应通过 patch_baseline 检出回退到发布前旧值的树外写入"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(shadow.status().unwrap().is_empty());
}

/// A 节：基准后态对象必须在临界区外预准备到 `state_dir/publish/<call_hash>/base/<slot>`，
/// `Finish(Committed)` 崩溃恢复推进 `base/` 时只从预暂存对象改名就位，绝不读取 `tree/`。
#[test]
fn finish_recovery_uses_prestaged_base_without_reading_tree() {
    let root = tempdir_xfs("ln-sh-pub-prestage-base-");
    let host = root.path().join("host");
    let state = root.path().join("state");
    fs::create_dir(&host).unwrap();
    fs::write(host.join("mod.txt"), "old-mod\n").unwrap();
    fs::write(host.join("del.txt"), "old-del\n").unwrap();

    let shadow = ShadowDir::open(&state, "w", &host, &[], &[]).unwrap();
    let tree = shadow.tree_path().to_path_buf();
    let base = shadow.base_path().to_path_buf();

    fs::write(tree.join("mod.txt"), "new-mod\n").unwrap();
    fs::write(tree.join("add.txt"), "new-add\n").unwrap();
    symlink("target-link", tree.join("sym")).unwrap();
    fs::remove_file(tree.join("del.txt")).unwrap();
    fs::create_dir(tree.join("sub")).unwrap();
    fs::write(tree.join("sub/in.txt"), "in-sub\n").unwrap();
    fs::set_permissions(tree.join("sub"), fs::Permissions::from_mode(0o555)).unwrap();

    let call_hash = call_record_hash("prestage-base-recovery");
    let prep = prepare_publish(&state, &call_hash, &[&shadow], None, None).unwrap();
    let PublishPrep::Ready(prepared) = prep else {
        panic!("预期 Ready");
    };

    let PublishJournal::Undo { roots, paths, .. } = prepared.undo_journal().clone() else {
        panic!("预期 Undo 日志");
    };
    let base_stage_dir = state.join("publish").join(&call_hash).join("base");
    assert!(
        base_stage_dir.is_dir(),
        "prepare_publish 必须在临界区外建立 publish/<hash>/base 目录"
    );

    let mut expected_ids = BTreeMap::new();
    for bp in &paths {
        match &bp.after {
            Some(
                lancet_node::shadow::publish::ObjectState::File { .. }
                | lancet_node::shadow::publish::ObjectState::Symlink { .. },
            ) => {
                let sb = bp
                    .staged_base
                    .unwrap_or_else(|| panic!("路径 {} 缺少 staged_base", bp.rel));
                let slot_path = base_stage_dir.join(sb.slot.to_string());
                let st = fs::symlink_metadata(&slot_path).expect("基准暂存槽应已落盘");
                assert_eq!((st.dev(), st.ino()), (sb.id.dev, sb.id.ino));
                expected_ids.insert(bp.rel.clone(), sb.id);
            }
            _ => {
                assert!(bp.staged_base.is_none());
            }
        }
    }
    assert_eq!(expected_ids.len(), 4);

    // 关闭活影子，模拟宿主已提交后在 Finish(Committed) 处崩溃；
    // 故意篡改并删除 tree/ 中的全部文件，确认恢复过程完全不依赖 tree/。
    drop(prepared);
    drop(shadow);

    fs::set_permissions(tree.join("sub"), fs::Permissions::from_mode(0o700)).unwrap();
    fs::write(tree.join("mod.txt"), "CORRUPTED-TREE\n").unwrap();
    fs::remove_file(tree.join("add.txt")).unwrap();
    fs::remove_file(tree.join("sym")).unwrap();
    fs::remove_file(tree.join("sub/in.txt")).unwrap();

    fs::write(host.join("mod.txt"), "new-mod\n").unwrap();
    fs::write(host.join("add.txt"), "new-add\n").unwrap();
    symlink("target-link", host.join("sym")).unwrap();
    fs::remove_file(host.join("del.txt")).unwrap();
    fs::create_dir(host.join("sub")).unwrap();
    fs::write(host.join("sub/in.txt"), "in-sub\n").unwrap();
    fs::set_permissions(host.join("sub"), fs::Permissions::from_mode(0o555)).unwrap();

    let rec_root = RecoveryRoot::open(&state, "w", &host, &[], &[]).unwrap();
    let finish = PublishJournal::Finish {
        call_hash: call_hash.clone(),
        roots,
        paths,
        outcome: PublishFinish::Committed,
    };
    let recovered = recover_journal(
        &state,
        "prestage-base-recovery",
        &state.join("calls/fake.json"),
        finish,
        &[rec_root],
        |_| Ok(()),
    )
    .expect("Finish(Committed) 崩溃恢复必须只使用 publish/<hash>/base 而成功");
    assert!(!state.join("publish").join(&call_hash).exists());
    assert_eq!(recovered.effects.len(), 1);

    // 验证 base/ 安装的就是预暂存的对象 inode 与发布内容，完全未受 tree/ 篡改影响。
    assert_eq!(
        fs::read_to_string(base.join("mod.txt")).unwrap(),
        "new-mod\n"
    );
    assert_eq!(
        fs::read_to_string(base.join("add.txt")).unwrap(),
        "new-add\n"
    );
    assert_eq!(
        fs::read_link(base.join("sym")).unwrap(),
        PathBuf::from("target-link")
    );
    assert!(!base.join("del.txt").exists());
    assert_eq!(
        fs::symlink_metadata(base.join("sub")).unwrap().mode() & 0o7777,
        0o555
    );
    assert_eq!(
        fs::read_to_string(base.join("sub/in.txt")).unwrap(),
        "in-sub\n"
    );

    for (rel, id) in expected_ids {
        let st = fs::symlink_metadata(base.join(&rel)).unwrap();
        assert_eq!(
            (st.dev(), st.ino()),
            (id.dev, id.ino),
            "base/{rel} 必须是预暂存对象改名就位，而非从 tree/ 复制"
        );
    }

    let _ = fs::set_permissions(host.join("sub"), fs::Permissions::from_mode(0o700));
    let _ = fs::set_permissions(base.join("sub"), fs::Permissions::from_mode(0o700));
}

/// 第 3 轮 B1：实时部分撤回（阶段 5 尚未执行）中，新建只读目录 `d`（目标权限 `0555`）因宿主修改了子文件 `d/x`
/// 而被标为“已发布”保留在宿主上时，宿主 `d` 与基准 `d` 的权限必须同为 `0555`，不得在宿主上残留临时写权限 `0755`。
#[test]
fn live_partial_rollback_restores_readonly_published_dir_mode() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    let pad_count = 300usize;
    let max_attempts = 10usize;
    let mut hit = false;

    for attempt in 1..=max_attempts {
        let root = tempdir_xfs("ln-sh-pub-b1-live-ro-");
        let host = root.path().join("host");
        let state = root.path().join("state");
        fs::create_dir(&host).unwrap();
        for i in 0..pad_count {
            fs::write(host.join(format!("m{i:04}.txt")), format!("old-m-{i}\n")).unwrap();
        }
        fs::write(host.join("z.txt"), "old-z\n").unwrap();

        let shadow = ShadowDir::open(&state, "w", &host, &[], &[]).unwrap();
        let tree = shadow.tree_path().to_path_buf();
        let base = shadow.base_path().to_path_buf();

        // 在影子中新建只读目录 d（0555）及子文件 d/x，修改中间填充文件与末项 z.txt。
        fs::create_dir(tree.join("d")).unwrap();
        fs::write(tree.join("d/x"), "pub-x\n").unwrap();
        fs::set_permissions(tree.join("d"), fs::Permissions::from_mode(0o555)).unwrap();
        for i in 0..pad_count {
            fs::write(tree.join(format!("m{i:04}.txt")), format!("new-m-{i}\n")).unwrap();
        }
        fs::write(tree.join("z.txt"), "new-z\n").unwrap();

        let call_hash = call_record_hash(&format!("live-ro-partial-{attempt}"));
        let prep = prepare_publish(&state, &call_hash, &[&shadow], None, None).unwrap();
        let PublishPrep::Ready(prepared) = prep else {
            panic!("预期 Ready");
        };

        let z_meta_before = fs::symlink_metadata(host.join("z.txt")).unwrap();
        let z_before_id = (z_meta_before.dev(), z_meta_before.ino());
        let writer_staged = root.path().join(format!("writer-live-z-{attempt}.tmp"));
        fs::write(&writer_staged, b"host-late-z\n").unwrap();
        let writer_staged_bg = writer_staged.clone();

        let stop = Arc::new(AtomicBool::new(false));
        let stop_bg = Arc::clone(&stop);
        let host_bg = host.clone();
        let writer = std::thread::spawn(move || {
            let dx = host_bg.join("d/x");
            let z = host_bg.join("z.txt");
            while !stop_bg.load(Ordering::Relaxed) {
                if fs::read_to_string(&dx).ok().as_deref() == Some("pub-x\n") {
                    // 原地改写已发布的 d/x（破坏其可撤回性），再用 RENAME_EXCHANGE 原子替换 z.txt
                    // 并核对被换出的对象是否为交换前旧 inode（第 4 轮项 2）。
                    fs::write(&dx, "host-edited-dx\n").unwrap();
                    renameat_with(CWD, &writer_staged_bg, CWD, &z, RenameFlags::EXCHANGE).unwrap();
                    let swapped = fs::symlink_metadata(&writer_staged_bg).unwrap();
                    let swapped_old_z = (swapped.dev(), swapped.ino()) == z_before_id;
                    return Some(swapped_old_z);
                }
            }
            None
        });

        let out = prepared.execute(&[&shadow], |_| Ok(())).unwrap();
        stop.store(true, Ordering::Relaxed);
        let evidence = writer.join().unwrap();
        let _ = fs::remove_file(&writer_staged);

        let host_d_mode = fs::symlink_metadata(host.join("d"))
            .ok()
            .map(|m| m.mode() & 0o7777);
        let base_d_mode = fs::symlink_metadata(base.join("d"))
            .ok()
            .map(|m| m.mode() & 0o7777);
        let _ = fs::set_permissions(host.join("d"), fs::Permissions::from_mode(0o700));
        let _ = fs::set_permissions(tree.join("d"), fs::Permissions::from_mode(0o700));
        let _ = fs::set_permissions(base.join("d"), fs::Permissions::from_mode(0o700));

        match out {
            PublishOutcome::Indeterminate { reason, .. } => {
                assert_eq!(
                    evidence,
                    Some(true),
                    "部分撤回必须由写者在交换前改写末项触发，实际证据：{evidence:?}\n{reason}"
                );
                assert!(
                    reason.contains("w \"d\"：已发布"),
                    "目录 d 应标为已发布，实际：\n{reason}"
                );
                assert!(
                    reason.contains("w \"d/x\"：保留宿主修改"),
                    "子文件 d/x 应标为保留宿主修改，实际：\n{reason}"
                );
                assert_eq!(base_d_mode, Some(0o555), "基准目录 d 权限应为 0555");
                assert_eq!(
                    host_d_mode,
                    Some(0o555),
                    "实时部分撤回保留只读目录 d（已发布）时，宿主 d 权限也必须为 0555，不得残留 0755"
                );
                hit = true;
                break;
            }
            PublishOutcome::Committed { .. } => {
                assert!(
                    evidence != Some(true),
                    "写者已确证在交换前修改 z.txt 旧 inode，不得提交成功"
                );
            }
            PublishOutcome::RolledBackConflict { conflicts } => {
                panic!("第 {attempt} 轮出现意外整批撤回：{conflicts:?}");
            }
            PublishOutcome::RolledBackIoError { message } => {
                panic!("第 {attempt} 轮出现意外 I/O 错误：{message}");
            }
        }
    }
    assert!(hit, "在限定轮次内未能触发实时部分撤回只读目录窗口");
}
