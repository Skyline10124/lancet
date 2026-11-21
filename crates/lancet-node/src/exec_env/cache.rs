// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 私有包缓存的一次性 `FICLONE` 播种与复用。

use std::ffi::{OsStr, OsString};
use std::fmt::Write as _;
use std::fs::{self, DirBuilder};
use std::io;
use std::os::fd::{AsFd, AsRawFd as _, BorrowedFd, OwnedFd};
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::DirBuilderExt as _;
use std::path::{Component, Path, PathBuf};

use anyhow::Context as _;
use rustix::fs::{
    AtFlags, CWD, Dir, FileType, Mode, OFlags, ResolveFlags, chmodat, fchmod, fstat, mkdirat, open,
    openat, openat2, readlinkat, symlinkat, unlinkat,
};
use sha2::{Digest as _, Sha256};

/// Linux `FICLONE` ioctl 号（`_IOW(0x94, 9, int)`）。
const FICLONE: libc::c_ulong = 0x4004_9409;

/// 在状态目录独占锁保护下播种或复用单个私有缓存副本，返回副本目录的 `O_PATH` fd。
///
/// 尚未播种的缓存直接以 `FICLONE` 播种到同级临时目录，复制全部完成后再按分量逐层打开、
/// 自底向上统一还原目录权限并原子改名就位；已就位的副本直接复用。
pub fn seed_or_open_cache(
    cache_src: &Path,
    dest_path: &Path,
    env_caches_root: &Path,
    env_name: &str,
) -> anyhow::Result<OwnedFd> {
    match DirBuilder::new().mode(0o700).create(env_caches_root) {
        Ok(()) => {}
        Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {}
        Err(err) => {
            return Err(err).with_context(|| {
                format!(
                    "执行环境 `{env_name}` 无法创建私有缓存根目录 {}",
                    env_caches_root.display()
                )
            });
        }
    }

    let caches_root_fd = openat2(
        CWD,
        env_caches_root,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
        ResolveFlags::NO_SYMLINKS,
    )
    .map_err(io::Error::from)
    .with_context(|| {
        format!(
            "执行环境 `{env_name}` 打开私有缓存根目录 {} 失败",
            env_caches_root.display()
        )
    })?;

    let key = cache_key(dest_path);
    cleanup_stale_tmp_dirs(caches_root_fd.as_fd(), env_caches_root, &key, env_name)?;

    let final_dir = env_caches_root.join(&key);
    if final_dir.is_dir() {
        return open_dir_path_fd(&final_dir).with_context(|| {
            format!(
                "执行环境 `{env_name}` 打开已有私有缓存副本 {} 失败",
                final_dir.display()
            )
        });
    }

    let tmp_name = format!("{key}.tmp-{}", std::process::id());
    let tmp_dir = env_caches_root.join(&tmp_name);
    if tmp_dir.exists() {
        remove_dir_tree_at(caches_root_fd.as_fd(), OsStr::new(&tmp_name)).with_context(|| {
            format!(
                "执行环境 `{env_name}` 清理旧缓存临时目录 {} 失败",
                tmp_dir.display()
            )
        })?;
    }

    DirBuilder::new()
        .mode(0o700)
        .create(&tmp_dir)
        .with_context(|| {
            format!(
                "执行环境 `{env_name}` 创建缓存临时目录 {} 失败",
                tmp_dir.display()
            )
        })?;

    let mut dir_modes: Vec<(PathBuf, Mode)> = Vec::new();
    let copy_res = (|| -> anyhow::Result<OwnedFd> {
        let src_fd = openat2(
            CWD,
            cache_src,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
            ResolveFlags::NO_SYMLINKS,
        )
        .with_context(|| format!("打开缓存来源目录 {} 失败", cache_src.display()))?;

        let dst_fd = openat2(
            caches_root_fd.as_fd(),
            &tmp_name,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
            ResolveFlags::BENEATH | ResolveFlags::NO_SYMLINKS,
        )
        .with_context(|| format!("打开缓存临时目录 {} 失败", tmp_dir.display()))?;

        if is_cargo_registry(dest_path, cache_src) {
            seed_cargo_registry(src_fd.as_fd(), dst_fd.as_fd(), &mut dir_modes)?;
        } else {
            copy_tree_ficlone(
                src_fd.as_fd(),
                dst_fd.as_fd(),
                Path::new(""),
                &mut dir_modes,
            )?;
        }
        Ok(dst_fd)
    })();

    let tmp_root_fd = match copy_res {
        Ok(fd) => fd,
        Err(err) => {
            return Err(rollback_and_attach_context(
                err,
                caches_root_fd.as_fd(),
                OsStr::new(&tmp_name),
                &tmp_dir,
                env_name,
                cache_src,
                dest_path,
                &final_dir,
            ));
        }
    };

    // 整棵树复制完成后，按分量逐层打开、自底向上统一还原目录权限，避免超长相对路径触发 ENAMETOOLONG。
    if let Err(err) = apply_dir_modes(tmp_root_fd.as_fd(), &dir_modes) {
        drop(tmp_root_fd);
        return Err(rollback_and_attach_context(
            err,
            caches_root_fd.as_fd(),
            OsStr::new(&tmp_name),
            &tmp_dir,
            env_name,
            cache_src,
            dest_path,
            &final_dir,
        ));
    }
    drop(tmp_root_fd);

    if let Err(err) = fs::rename(&tmp_dir, &final_dir) {
        let rename_err = anyhow::Error::from(err).context(format!(
            "原子替换私有缓存 {} -> {} 失败",
            tmp_dir.display(),
            final_dir.display()
        ));
        return Err(rollback_and_attach_context(
            rename_err,
            caches_root_fd.as_fd(),
            OsStr::new(&tmp_name),
            &tmp_dir,
            env_name,
            cache_src,
            dest_path,
            &final_dir,
        ));
    }

    open_dir_path_fd(&final_dir).with_context(|| {
        format!(
            "执行环境 `{env_name}` 打开已就位私有缓存 {} 失败",
            final_dir.display()
        )
    })
}

#[allow(clippy::too_many_arguments)]
fn rollback_and_attach_context(
    err: anyhow::Error,
    caches_root_fd: BorrowedFd<'_>,
    tmp_name: &OsStr,
    tmp_dir: &Path,
    env_name: &str,
    cache_src: &Path,
    dest_path: &Path,
    final_dir: &Path,
) -> anyhow::Error {
    match remove_dir_tree_at(caches_root_fd, tmp_name) {
        Ok(()) => err.context(format!(
            "执行环境 `{env_name}` 播种私有缓存失败（来源 {}，目标 {}，挂载目的地 {}）",
            cache_src.display(),
            final_dir.display(),
            dest_path.display()
        )),
        Err(cleanup_err) => err.context(format!(
            "执行环境 `{env_name}` 播种私有缓存失败（来源 {}，目标 {}，挂载目的地 {}），且清理临时目录 {} 失败：{cleanup_err}",
            cache_src.display(),
            final_dir.display(),
            dest_path.display(),
            tmp_dir.display()
        )),
    }
}

/// 递归删除已打开父目录下的临时目录树：先在父目录上用 `fchmodat` 将子目录权限恢复为 `0700`
/// 再打开遍历（兼容 `0050`/`0000` 等无读权限的非空子目录与超过 `PATH_MAX` 的深层路径）。
fn remove_dir_tree_at(parent_fd: BorrowedFd<'_>, name: &OsStr) -> io::Result<()> {
    match chmodat(
        parent_fd,
        name,
        Mode::from_raw_mode(0o700),
        AtFlags::empty(),
    ) {
        Ok(()) => {}
        Err(rustix::io::Errno::NOENT) => return Ok(()),
        Err(err) => return Err(io::Error::from(err)),
    }

    let dir_fd = match openat2(
        parent_fd,
        name,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
        ResolveFlags::BENEATH | ResolveFlags::NO_SYMLINKS,
    ) {
        Ok(fd) => fd,
        Err(rustix::io::Errno::NOENT) => return Ok(()),
        Err(err) => return Err(io::Error::from(err)),
    };

    let entries = read_dir_entry_names(dir_fd.as_fd())?;
    for child in entries {
        let path_fd = match openat(
            dir_fd.as_fd(),
            &child,
            OFlags::PATH | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        ) {
            Ok(fd) => fd,
            Err(rustix::io::Errno::NOENT) => continue,
            Err(err) => return Err(io::Error::from(err)),
        };
        let st = fstat(&path_fd).map_err(io::Error::from)?;
        let ft = FileType::from_raw_mode(st.st_mode);
        drop(path_fd);

        if ft == FileType::Directory {
            remove_dir_tree_at(dir_fd.as_fd(), &child)?;
        } else {
            match unlinkat(dir_fd.as_fd(), &child, AtFlags::empty()) {
                Ok(()) | Err(rustix::io::Errno::NOENT) => {}
                Err(err) => return Err(io::Error::from(err)),
            }
        }
    }

    drop(dir_fd);
    match unlinkat(parent_fd, name, AtFlags::REMOVEDIR) {
        Ok(()) | Err(rustix::io::Errno::NOENT) => Ok(()),
        Err(err) => Err(io::Error::from(err)),
    }
}

fn read_dir_entry_names(dir_fd: BorrowedFd<'_>) -> io::Result<Vec<OsString>> {
    let dup_fd = openat(
        dir_fd,
        c".",
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(io::Error::from)?;
    let mut dir = Dir::new(dup_fd).map_err(io::Error::from)?;
    let mut names = Vec::new();
    while let Some(next) = dir.read() {
        let entry = next.map_err(io::Error::from)?;
        let raw = entry.file_name().to_bytes();
        if raw == b"." || raw == b".." {
            continue;
        }
        names.push(OsStr::from_bytes(raw).to_os_string());
    }
    Ok(names)
}

fn apply_dir_modes(
    tmp_root_fd: BorrowedFd<'_>,
    dir_modes: &[(PathBuf, Mode)],
) -> anyhow::Result<()> {
    for (rel, mode) in dir_modes.iter().rev() {
        let fd = open_dir_by_components(tmp_root_fd, rel)
            .with_context(|| format!("打开待还原权限的私有缓存目录 `{}` 失败", rel.display()))?;
        fchmod(&fd, *mode)
            .map_err(io::Error::from)
            .with_context(|| format!("还原私有缓存目录 `{}` 权限失败", rel.display()))?;
    }
    Ok(())
}

/// 按单个路径分量逐层打开目标目录（末级用 `O_RDONLY` 以供 `fchmod`），避免深层路径触发 `ENAMETOOLONG`。
fn open_dir_by_components(root_fd: BorrowedFd<'_>, rel: &Path) -> io::Result<OwnedFd> {
    let comps: Vec<&OsStr> = rel
        .components()
        .map(|comp| match comp {
            Component::Normal(part) => Ok(part),
            _ => Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "目录相对路径包含非法分量",
            )),
        })
        .collect::<io::Result<Vec<_>>>()?;

    let Some((last, parents)) = comps.split_last() else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "目录相对路径为空",
        ));
    };

    let mut cur: Option<OwnedFd> = None;
    for part in parents {
        let base = cur.as_ref().map_or(root_fd, AsFd::as_fd);
        let next = openat2(
            base,
            *part,
            OFlags::PATH | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
            ResolveFlags::BENEATH | ResolveFlags::NO_SYMLINKS,
        )
        .map_err(io::Error::from)?;
        cur = Some(next);
    }

    let parent_fd = cur.as_ref().map_or(root_fd, AsFd::as_fd);
    openat2(
        parent_fd,
        *last,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
        ResolveFlags::BENEATH | ResolveFlags::NO_SYMLINKS,
    )
    .map_err(io::Error::from)
}

/// Cargo registry 只播种 `cache/` 与 `index/`，`src/` 留给 Cargo 在私有目录里自行解压。
fn seed_cargo_registry(
    src_root: BorrowedFd<'_>,
    dst_root: BorrowedFd<'_>,
    dir_modes: &mut Vec<(PathBuf, Mode)>,
) -> anyhow::Result<()> {
    for sub in ["cache", "index"] {
        let sub_src = match openat(
            src_root,
            sub,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        ) {
            Ok(fd) => fd,
            Err(rustix::io::Errno::NOENT) => continue,
            Err(rustix::io::Errno::NOTDIR | rustix::io::Errno::LOOP) => continue,
            Err(err) => {
                return Err(io::Error::from(err))
                    .with_context(|| format!("打开 Cargo registry 子目录 `{sub}` 失败"));
            }
        };

        let st = fstat(&sub_src)
            .map_err(io::Error::from)
            .with_context(|| format!("读取 Cargo registry 子目录 `{sub}` 属性失败"))?;
        let orig_mode = Mode::from_raw_mode(st.st_mode & 0o7777);
        mkdirat(dst_root, sub, Mode::from_raw_mode(0o700))
            .map_err(io::Error::from)
            .with_context(|| format!("创建私有 Cargo registry 子目录 `{sub}` 失败"))?;
        let sub_dst = openat(
            dst_root,
            sub,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(io::Error::from)
        .with_context(|| format!("打开私有 Cargo registry 子目录 `{sub}` 失败"))?;

        let sub_rel = PathBuf::from(sub);
        dir_modes.push((sub_rel.clone(), orig_mode));
        copy_tree_ficlone(sub_src.as_fd(), sub_dst.as_fd(), &sub_rel, dir_modes)?;
    }
    Ok(())
}

fn copy_tree_ficlone(
    src_dir_fd: BorrowedFd<'_>,
    dst_dir_fd: BorrowedFd<'_>,
    rel_prefix: &Path,
    dir_modes: &mut Vec<(PathBuf, Mode)>,
) -> anyhow::Result<()> {
    let dup_fd = openat(
        src_dir_fd,
        c".",
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(io::Error::from)
    .context("打开来源目录遍历描述符失败")?;
    let mut dir = Dir::new(dup_fd)
        .map_err(io::Error::from)
        .context("初始化来源目录迭代器失败")?;

    while let Some(next) = dir.read() {
        let entry = next.map_err(io::Error::from).context("遍历缓存目录失败")?;
        let name_bytes = entry.file_name().to_bytes();
        if name_bytes == b"." || name_bytes == b".." {
            continue;
        }
        let name = OsStr::from_bytes(name_bytes);

        let child_path_fd = openat(
            src_dir_fd,
            name,
            OFlags::PATH | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(io::Error::from)
        .with_context(|| format!("打开缓存条目 `{}` 失败", name.to_string_lossy()))?;
        let st = fstat(&child_path_fd)
            .map_err(io::Error::from)
            .with_context(|| format!("读取缓存条目 `{}` 属性失败", name.to_string_lossy()))?;
        let ft = FileType::from_raw_mode(st.st_mode);
        let orig_mode = Mode::from_raw_mode(st.st_mode & 0o7777);

        match ft {
            FileType::Directory => {
                drop(child_path_fd);
                let sub_src = openat(
                    src_dir_fd,
                    name,
                    OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                    Mode::empty(),
                )
                .map_err(io::Error::from)
                .with_context(|| format!("打开缓存子目录 `{}` 失败", name.to_string_lossy()))?;

                mkdirat(dst_dir_fd, name, Mode::from_raw_mode(0o700))
                    .map_err(io::Error::from)
                    .with_context(|| {
                        format!("创建私有缓存子目录 `{}` 失败", name.to_string_lossy())
                    })?;
                let sub_dst = openat(
                    dst_dir_fd,
                    name,
                    OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                    Mode::empty(),
                )
                .map_err(io::Error::from)
                .with_context(|| format!("打开私有缓存子目录 `{}` 失败", name.to_string_lossy()))?;

                let child_rel = rel_prefix.join(name);
                dir_modes.push((child_rel.clone(), orig_mode));
                copy_tree_ficlone(sub_src.as_fd(), sub_dst.as_fd(), &child_rel, dir_modes)?;
            }
            FileType::RegularFile => {
                let proc_path = format!("/proc/self/fd/{}", child_path_fd.as_raw_fd());
                let src_file = open(proc_path, OFlags::RDONLY | OFlags::CLOEXEC, Mode::empty())
                    .map_err(io::Error::from)
                    .with_context(|| {
                        format!("打开缓存普通文件 `{}` 失败", name.to_string_lossy())
                    })?;
                drop(child_path_fd);

                let dst_file = openat(
                    dst_dir_fd,
                    name,
                    OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::CLOEXEC,
                    Mode::from_raw_mode(0o600),
                )
                .map_err(io::Error::from)
                .with_context(|| format!("创建私有缓存文件 `{}` 失败", name.to_string_lossy()))?;

                ioctl_ficlone(dst_file.as_raw_fd(), src_file.as_raw_fd()).with_context(|| {
                    format!("FICLONE 克隆缓存文件 `{}` 失败", name.to_string_lossy())
                })?;
                fchmod(&dst_file, orig_mode)
                    .map_err(io::Error::from)
                    .with_context(|| {
                        format!("还原私有缓存文件 `{}` 权限失败", name.to_string_lossy())
                    })?;
            }
            FileType::Symlink => {
                let target = readlinkat(&child_path_fd, c"", Vec::new())
                    .map_err(io::Error::from)
                    .with_context(|| {
                        format!("读取缓存符号链接 `{}` 失败", name.to_string_lossy())
                    })?;
                drop(child_path_fd);
                symlinkat(OsStr::from_bytes(target.as_bytes()), dst_dir_fd, name)
                    .map_err(io::Error::from)
                    .with_context(|| {
                        format!("重建缓存符号链接 `{}` 失败", name.to_string_lossy())
                    })?;
            }
            _ => {
                drop(child_path_fd);
            }
        }
    }

    Ok(())
}

fn is_cargo_registry(dest_path: &Path, src_path: &Path) -> bool {
    ends_with_cargo_registry(dest_path) || ends_with_cargo_registry(src_path)
}

fn ends_with_cargo_registry(path: &Path) -> bool {
    let comps: Vec<&OsStr> = path
        .components()
        .filter_map(|c| match c {
            Component::Normal(s) => Some(s),
            _ => None,
        })
        .collect();
    if comps.len() < 2 {
        return false;
    }
    let last = comps[comps.len() - 1];
    let prev = comps[comps.len() - 2];
    last == OsStr::new("registry") && (prev == OsStr::new(".cargo") || prev == OsStr::new("cargo"))
}

fn cache_key(dest_path: &Path) -> String {
    let digest = Sha256::digest(dest_path.as_os_str().as_bytes());
    let mut out = String::with_capacity(64);
    for byte in digest {
        let _ = write!(&mut out, "{byte:02x}");
    }
    out
}

fn cleanup_stale_tmp_dirs(
    caches_root_fd: BorrowedFd<'_>,
    env_caches_root: &Path,
    key: &str,
    env_name: &str,
) -> anyhow::Result<()> {
    let prefix = format!("{key}.tmp-");
    let entries = read_dir_entry_names(caches_root_fd)
        .with_context(|| format!("扫描缓存目录 {} 失败", env_caches_root.display()))?;
    for name in entries {
        if name.to_str().is_some_and(|s| s.starts_with(&prefix)) {
            let path = env_caches_root.join(&name);
            remove_dir_tree_at(caches_root_fd, &name).with_context(|| {
                format!(
                    "执行环境 `{env_name}` 清理遗留缓存临时目录 {} 失败",
                    path.display()
                )
            })?;
        }
    }
    Ok(())
}

fn open_dir_path_fd(path: &Path) -> io::Result<OwnedFd> {
    openat2(
        CWD,
        path,
        OFlags::PATH | OFlags::DIRECTORY | OFlags::CLOEXEC,
        Mode::empty(),
        ResolveFlags::NO_SYMLINKS,
    )
    .map_err(io::Error::from)
}

fn ioctl_ficlone(dst_fd: libc::c_int, src_fd: libc::c_int) -> io::Result<()> {
    // SAFETY: Linux FICLONE ioctl 将 src_fd 指向的文件数据块写时复制克隆到 dst_fd。
    let rc = unsafe { libc::ioctl(dst_fd, FICLONE, src_fd as libc::c_int) };
    if rc == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;
    use std::os::unix::fs::PermissionsExt as _;

    #[test]
    fn cleans_stale_tmp_dir_with_0050_nonempty_subdir() {
        let tmp = tempfile::Builder::new()
            .prefix("ln-cache-0050-")
            .tempdir_in("/var/tmp")
            .expect("在 /var/tmp 创建临时目录失败");
        let src = tmp.path().join("src");
        let caches_root = tmp.path().join("caches");
        fs::create_dir_all(&src).unwrap();
        fs::create_dir_all(&caches_root).unwrap();
        fs::write(src.join("seed.bin"), b"seed-data").unwrap();

        let dest_path = Path::new("/fake/home/.cache/demo");
        let key = cache_key(dest_path);
        let stale = caches_root.join(format!("{key}.tmp-99999"));
        let locked = stale.join("locked");
        let locked_nested = locked.join("nested");
        fs::create_dir_all(&locked_nested).unwrap();
        fs::write(locked_nested.join("inside.txt"), b"cannot-read-dir").unwrap();
        fs::set_permissions(&locked_nested, fs::Permissions::from_mode(0o050)).unwrap();
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o050)).unwrap();

        let fd = seed_or_open_cache(&src, dest_path, &caches_root, "dev")
            .expect("带 0050 非空子目录的遗留临时树应可清理并完成播种");
        drop(fd);

        assert!(!stale.exists(), "遗留临时目录应已删除");
        let final_dir = caches_root.join(&key);
        assert_eq!(fs::read(final_dir.join("seed.bin")).unwrap(), b"seed-data");
    }

    #[test]
    fn seeds_deep_path_exceeding_path_max_and_restores_modes() {
        let tmp = tempfile::Builder::new()
            .prefix("ln-cache-deep-")
            .tempdir_in("/var/tmp")
            .expect("在 /var/tmp 创建临时目录失败");
        let src = tmp.path().join("src");
        let caches_root = tmp.path().join("caches");
        fs::create_dir_all(&src).unwrap();
        fs::create_dir_all(&caches_root).unwrap();

        let root_fd = openat2(
            CWD,
            &src,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
            ResolveFlags::NO_SYMLINKS,
        )
        .unwrap();

        // 构造 18 层嵌套目录，每层名 240 字节，相对路径总长 > 4,300 字节（超过 PATH_MAX = 4,096）。
        let mut cur = root_fd;
        let mut rel = PathBuf::new();
        let mut level_fds = Vec::with_capacity(18);
        for level in 0..18usize {
            let seg = format!("d{level:02}_{}", "x".repeat(236));
            mkdirat(cur.as_fd(), &seg, Mode::from_raw_mode(0o700)).unwrap();
            let next = openat2(
                cur.as_fd(),
                &seg,
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
                ResolveFlags::BENEATH | ResolveFlags::NO_SYMLINKS,
            )
            .unwrap();
            rel.push(&seg);
            level_fds.push(cur);
            cur = next;
        }
        assert!(
            rel.as_os_str().len() > 4096,
            "深层相对路径长度应超过 PATH_MAX：{}",
            rel.as_os_str().len()
        );

        let leaf_file = openat(
            cur.as_fd(),
            "leaf.txt",
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::CLOEXEC,
            Mode::from_raw_mode(0o644),
        )
        .unwrap();
        let mut f = fs::File::from(leaf_file);
        f.write_all(b"deep-ok").unwrap();
        drop(f);

        // 最深层目录设为只读 0555，验证按分量逐层设置权限且保留自底向上顺序。
        fchmod(&cur, Mode::from_raw_mode(0o555)).unwrap();
        drop(cur);
        drop(level_fds);

        let dest_path = Path::new("/fake/home/.cache/deep");
        let seeded_fd = seed_or_open_cache(&src, dest_path, &caches_root, "dev")
            .expect("18 层超 PATH_MAX 深层路径应能完成播种并还原目录权限");

        // 逐分量验证第 18 层目录权限与文件内容。
        let deep_dir_fd = open_dir_by_components(seeded_fd.as_fd(), &rel).unwrap();
        let deep_st = fstat(&deep_dir_fd).unwrap();
        assert_eq!(deep_st.st_mode & 0o777, 0o555);
        let cloned_leaf = openat(
            deep_dir_fd.as_fd(),
            "leaf.txt",
            OFlags::RDONLY | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .unwrap();
        let mut content = String::new();
        io::Read::read_to_string(&mut fs::File::from(cloned_leaf), &mut content).unwrap();
        assert_eq!(content, "deep-ok");
        drop(deep_dir_fd);
        drop(seeded_fd);

        // 清理超深目录树，避免 TempDir::drop 遇到 PATH_MAX / 0555 失败。
        let tmp_fd = openat2(
            CWD,
            tmp.path(),
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
            ResolveFlags::NO_SYMLINKS,
        )
        .unwrap();
        remove_dir_tree_at(tmp_fd.as_fd(), OsStr::new("src")).unwrap();
        remove_dir_tree_at(tmp_fd.as_fd(), OsStr::new("caches")).unwrap();
    }
}
