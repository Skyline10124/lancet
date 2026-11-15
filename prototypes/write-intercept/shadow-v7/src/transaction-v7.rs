#[derive(Serialize, Deserialize, Clone, Debug)]
struct Rename {
    from: String,
    to: String,
}
#[derive(Serialize, Deserialize, Clone, Debug)]
struct Operation {
    path: String,
    source: String,
    kind: String,
    before: Option<Entry>,
    after: Option<Entry>,
    slot: PathBuf,
    backup: PathBuf,
    stage: PathBuf,
}
#[derive(Serialize, Deserialize)]
struct Journal {
    phase: String,
    real: PathBuf,
    ops: Vec<Operation>,
    locks: Vec<String>,
}
fn exchange(a: &Path, b: &Path, flags: u32) -> Result<()> {
    let a = CString::new(a.as_os_str().as_bytes())?;
    let b = CString::new(b.as_os_str().as_bytes())?;
    if unsafe {
        libc::renameat2(
            libc::AT_FDCWD,
            a.as_ptr(),
            libc::AT_FDCWD,
            b.as_ptr(),
            flags,
        )
    } != 0
    {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(())
}
fn current(p: &Path) -> Result<Option<Entry>> {
    inspect(p, None)
}
fn put_object(src: &Path, dst: &Path, e: &Entry) -> Result<()> {
    match e.kind.as_str() {
        "file" => clone_file(src, dst)?,
        "directory" => fs::create_dir(dst)?,
        "symlink" => symlink(&e.link, dst)?,
        "fifo" => {
            let c = CString::new(dst.as_os_str().as_bytes())?;
            if unsafe { libc::mkfifo(c.as_ptr(), e.mode) } != 0 {
                bail!("mkfifo 失败")
            }
        }
        _ => bail!("不支持的对象类型 {}", e.kind),
    }
    meta(dst, e)
}
fn recover_v7(txn: &Path, revert: bool) -> Result<Value> {
    let p = txn.join("transaction-v7.json");
    let mut j: Journal = serde_json::from_slice(&fs::read(&p)?)?;
    if j.phase == "complete" && !revert {
        return Ok(json!({"outcome":"Committed"}));
    }
    if j.phase == "rolled-back" {
        return Ok(json!({"outcome":"Restored"}));
    }
    let mut restored = vec![];
    let mut conflicts = vec![];
    let mut irreversible = vec![];
    for op in j.ops.iter().rev() {
        if revert && op.path.starts_with(".git/") {
            irreversible.push(op.path.clone());
            continue;
        }
        let dst = j.real.join(&op.path);
        let mut temporary = local_slot(op, &dst);
        if fs::symlink_metadata(&temporary).is_err()
            && op.stage != op.slot
            && fs::symlink_metadata(&op.stage).is_ok()
        {
            temporary = op.stage.clone();
        }
        if temporary.exists() || fs::symlink_metadata(&temporary).is_ok() {
            if fs::symlink_metadata(&op.slot).is_ok() {
                bail!("事务两处同时持有暂存 inode")
            }
            fs::rename(&temporary, &op.slot)?;
        }
        let now = current(&dst)?;
        let slot = current(&op.slot)?;
        let post = op.after.as_ref();
        let pre = op.before.as_ref();
        if op.kind == "rename" {
            let src = j.real.join(&op.source);
            if src.exists() {
                continue;
            }
            if now
                .as_ref()
                .zip(pre)
                .is_some_and(|(a, b)| a.ino == b.ino && a.kind == b.kind)
            {
                // 目录及其后代都核对；外部新增或修改导致整条目录逆操作冲突。
                let expected: Entries = serde_json::from_slice(&fs::read(&op.backup)?)?;
                let actual = Tree::scan(&dst, &Entries::new())?;
                if expected.len() != actual.entries.len()
                    || expected
                        .iter()
                        .any(|(p, e)| !object_equal(Some(e), actual.entries.get(p)))
                {
                    conflicts.push(op.path.clone());
                    continue;
                }
                exchange(&dst, &src, libc::RENAME_NOREPLACE)?;
                restored.push(op.source.clone());
            } else {
                conflicts.push(op.path.clone());
            }
            continue;
        }
        let applied = match op.kind.as_str() {
            "exchange" => slot.as_ref().zip(pre).is_some_and(|(a, b)| a.ino == b.ino),
            "delete" => slot.is_some(),
            "create" => slot.is_none() && now.is_some(),
            "mode" => !object_equal(now.as_ref(), pre),
            _ => false,
        };
        if !applied {
            continue;
        }
        let mut owned = object_equal(now.as_ref(), post);
        if op.kind == "exchange" || op.kind == "create" {
            owned &= now.as_ref().zip(post).is_some_and(|(a, b)| a.ino == b.ino);
        }
        if !owned {
            conflicts.push(op.path.clone());
            continue;
        }
        inject(&j.real, "rollback", &op.path);
        if !object_equal(current(&dst)?.as_ref(), post) {
            conflicts.push(op.path.clone());
            continue;
        }
        match op.kind.as_str() {
            "exchange" => exchange(&dst, &op.slot, libc::RENAME_EXCHANGE)?,
            "delete" => exchange(&op.slot, &dst, libc::RENAME_NOREPLACE)?,
            "create" => exchange(&dst, &op.slot, libc::RENAME_NOREPLACE)?,
            "mode" => meta(&dst, pre.unwrap())?,
            _ => {}
        }
        restored.push(op.path.clone());
        point("rollback-step");
    }
    barrier(&j.real)?;
    j.phase = if conflicts.is_empty() {
        "rolled-back"
    } else {
        "indeterminate"
    }
    .into();
    save(&p, &j)?;
    for l in &j.locks {
        let _ = fs::remove_file(j.real.join(l));
    }
    Ok(
        json!({"outcome":if conflicts.is_empty(){"Restored"}else if revert{"Conflict"}else{"Indeterminate"},"restored":restored,"conflicts":conflicts,"irreversible":irreversible}),
    )
}

fn local_slot(op: &Operation, dst: &Path) -> PathBuf {
    dst.with_file_name(format!(
        ".s3-v7-{}-{}",
        op.slot
            .parent()
            .unwrap()
            .file_name()
            .unwrap()
            .to_string_lossy(),
        op.slot.file_name().unwrap().to_string_lossy()
    ))
}
fn install_local(op: &Operation, dst: &Path, flags: u32) -> Result<()> {
    let temporary = local_slot(op, dst);
    // 已持久化的提交版本移入目标目录；交换在同一目录，影子 inode 从不移动。
    if op.stage != temporary {
        exchange(&op.stage, &temporary, libc::RENAME_NOREPLACE)?;
    }
    let result = exchange(&temporary, dst, flags);
    point("after-exchange");
    if fs::symlink_metadata(&temporary).is_ok() {
        fs::rename(&temporary, &op.slot)?;
    }
    result
}
