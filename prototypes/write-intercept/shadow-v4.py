#!/usr/bin/env python3
"""持久 reflink 影子研究原型。固定三类顶层排除；不是通用安全沙箱。"""
import ctypes
import fcntl
import hashlib
import json
import os
from pathlib import Path
import shutil
import signal
import stat
import subprocess
import sys
import time

HERE = Path(__file__).resolve().parent
EXCLUDED = {'target', '.git', 'hidden'}
FICLONE = 0x40049409
ABSENT = {'type': 'Absent'}
libc = ctypes.CDLL(None, use_errno=True)


def dump(path, value):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(value, ensure_ascii=False, indent=2) + '\n')


def barrier(path):
    """整批 syncfs，范围为整个 XFS；包含同文件系统其他脏数据。"""
    fd = os.open(path, os.O_RDONLY | os.O_DIRECTORY)
    try:
        if libc.syncfs(fd):
            raise OSError(ctypes.get_errno(), '批量落盘失败')
    finally:
        os.close(fd)


def save(path, value):
    tmp = path.with_suffix('.tmp')
    dump(tmp, value)
    with tmp.open('rb') as f:
        os.fsync(f.fileno())
    os.replace(tmp, path)
    fd = os.open(path.parent, os.O_RDONLY | os.O_DIRECTORY)
    os.fsync(fd)
    os.close(fd)


def point(name):
    """仅测试启用的真实 SIGKILL 注入点。"""
    if os.environ.get('S_KILL_AT') == name:
        os.kill(os.getpid(), signal.SIGKILL)


def sha(path):
    h = hashlib.sha256()
    with path.open('rb') as f:
        for b in iter(lambda: f.read(1024 * 1024), b''):
            h.update(b)
    return h.hexdigest()


def inspect(path, old=None):
    try:
        s = path.lstat()
    except (FileNotFoundError, NotADirectoryError):
        return dict(ABSENT)
    kind = ('file' if stat.S_ISREG(s.st_mode) else 'directory' if stat.S_ISDIR(s.st_mode)
            else 'symlink' if stat.S_ISLNK(s.st_mode) else 'fifo' if stat.S_ISFIFO(s.st_mode) else 'special')
    e = dict(type=kind, mode=stat.S_IMODE(s.st_mode), dev=s.st_dev, ino=s.st_ino,
             nlink=s.st_nlink, size=s.st_size, mtime=s.st_mtime_ns, ctime=s.st_ctime_ns,
             atime=s.st_atime_ns)
    if kind == 'file':
        e['sha256'] = old['sha256'] if old and signature(e) == signature(old) else sha(path)
    if kind == 'symlink':
        e['link'] = os.readlink(path)
    return e


def signature(e):
    return tuple(e.get(k) for k in ('type', 'mode', 'dev', 'ino', 'size', 'mtime', 'ctime', 'nlink'))


def scan(root, cache=None):
    cache = cache or {}
    out = {}
    def walk(d, prefix):
        with os.scandir(d) as entries:
            for ent in entries:
                p = prefix + ent.name
                if not prefix and ent.name in EXCLUDED:
                    continue
                e = inspect(Path(ent.path), cache.get(p))
                if e['type'] == 'Absent':
                    raise RuntimeError('扫描期间目录项消失：' + p)
                out[p] = e
                if e['type'] == 'directory':
                    walk(ent.path, p + '/')
    walk(root, '')
    groups = {}
    for p, e in out.items():
        if e['type'] == 'file':
            groups.setdefault((e['dev'], e['ino']), []).append(p)
    for paths in groups.values():
        for p in paths:
            out[p]['links'] = sorted(paths)
    return out


def semantic(e):
    return {k: e[k] for k in ('type', 'mode', 'sha256', 'link', 'links') if k in e}


def changes(before, after):
    return sorted(p for p in before.keys() | after.keys()
                  if semantic(before.get(p, ABSENT)) != semantic(after.get(p, ABSENT)))


def remove(p):
    if p.is_symlink() or not p.is_dir():
        p.unlink(missing_ok=True)
    else:
        shutil.rmtree(p)


def metadata(p, e):
    if e['type'] != 'symlink':
        p.chmod(e['mode'])
    os.utime(p, ns=(e['atime'], e['mtime']), follow_symlinks=False)


def clone(src, dst):
    with src.open('rb') as a, dst.open('wb') as b:
        fcntl.ioctl(b.fileno(), FICLONE, a.fileno())


def reconcile(root, desired, paths, source, *, anchors=None, inplace=None, checkpoints=False):
    """按路径物化净变化；目录移动目前展开，日志格式仍用 B3 对象前态。"""
    paths = set(paths)
    anchors = anchors or {}
    inplace = inplace or set()
    # 先删除不存在项或类型改变项；目录只在其后代已删除之后 rmdir。
    for p in sorted(paths, key=lambda p: (-p.count('/'), p)):
        now = inspect(root/p)
        want = desired.get(p, ABSENT)
        if now['type'] != 'Absent' and (want['type'] == 'Absent' or now['type'] != want['type']):
            if now['type'] == 'directory':
                (root/p).rmdir()
            else:
                (root/p).unlink()
    applied = 0
    for p in sorted(paths, key=lambda p: (p.count('/'), p)):
        e = desired.get(p, ABSENT)
        if e['type'] == 'Absent':
            continue
        dst = root/p
        dst.parent.mkdir(parents=True, exist_ok=True)
        if e['type'] == 'directory':
            dst.mkdir(exist_ok=True)
            dst.chmod(e['mode'] | 0o700)
        elif e['type'] == 'file':
            aliases = e.get('links', [p])
            # 组首优先选未变路径，然后选排序首项。
            leader = next((q for q in aliases if q not in paths), min(aliases, key=lambda q: (q.count('/'), q)))
            anchor = anchors.get(p)
            if anchor:
                if not dst.exists() or dst.stat().st_ino != anchor.stat().st_ino:
                    remove(dst)
                    os.link(anchor, dst)
            elif p != leader:
                remove(dst)
                os.link(root/leader, dst)
            if p == leader or anchor:
                src = source(p)
                if anchor or p in inplace:
                    dst.chmod(e['mode'] | 0o200)
                    with dst.open('wb') as b, src.open('rb') as a:
                        # 原 inode 写入；注入点检验中途截断后恢复。
                        b.write(a.read(1));b.flush()
                        if checkpoints:
                            point('inplace-partial')
                        shutil.copyfileobj(a, b)
                else:
                    tmp = dst.parent / ('.s-v4-' + dst.name)
                    clone(src, tmp)
                    metadata(tmp, e)
                    os.replace(tmp, dst)
        elif e['type'] == 'symlink':
            remove(dst)
            dst.symlink_to(e['link'])
        elif e['type'] == 'fifo':
            remove(dst)
            os.mkfifo(dst, e['mode'])
        else:
            raise RuntimeError('未支持的对象类型：' + p)
        applied += 1
        if checkpoints:
            point('apply-' + str(applied))
    # 最后恢复目录权限及时间，避免子项物化覆盖目录 mtime。
    for p in sorted(paths, key=lambda p: (-p.count('/'), p)):
        e = desired.get(p, ABSENT)
        if e['type'] != 'Absent':
            metadata(root/p, e)


class Shadow:
    def __init__(self, real, state):
        self.real, self.state = Path(real).resolve(), Path(state).resolve()
        self.shadow = self.state/'shadow'
        self.state.mkdir(parents=True, exist_ok=True)
        self.shadow.mkdir(exist_ok=True)
        for i in range(2):
            (self.state/f'pass-{i}').mkdir(exist_ok=True)
        # 基准只使用这三个顶层目录；真实缺失的类别不预建宿主目录。
        for p in EXCLUDED:
            (self.shadow/p).mkdir(exist_ok=True)
        self.lock = (self.state/'lock').open('w')
        fcntl.flock(self.lock, fcntl.LOCK_EX)
        self.index = json.loads((self.state/'index.json').read_text()) if (self.state/'index.json').exists() else {'real': {}, 'shadow': {}}
        if self.real.stat().st_dev != self.state.stat().st_dev:
            raise RuntimeError('状态目录与工作目录必须在同一文件系统')

    def close(self):
        self.lock.close()

    def begin(self):
        start = time.monotonic()
        before = scan(self.real, self.index['real'])
        old = self.index['real']
        paths = {p for p in before.keys() | old.keys() if signature(before.get(p, ABSENT)) != signature(old.get(p, ABSENT))}
        # 同一 inode 的链接组一起同步，避免复制拆散别名。
        for p in list(paths):
            paths.update(before.get(p, {}).get('links', []))
        reconcile(self.shadow, before, paths, lambda p: self.real/p)
        shadow = scan(self.shadow, self.index['shadow'])
        if changes(before, shadow):
            raise RuntimeError('同步后的内容或拓扑不一致')
        self.before, self.base_shadow = before, shadow
        self.index = {'real': before, 'shadow': shadow}
        save(self.state/'index.json', self.index)
        save(self.state/'begin.json', {'real': before, 'shadow': shadow})
        self.sync_s = time.monotonic() - start
        self.synced = len(paths)
        return {'sync_s': self.sync_s, 'synced': self.synced, 'entries': len(before)}

    def execute(self, command, env=None, timeout=1200):
        start = time.monotonic()
        cmd = [str(HERE/'.scratch/namespace-v4'), str(self.real), str(self.state), '--', *map(str, command)]
        proc = subprocess.Popen(cmd, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                env=dict(os.environ, **(env or {})), start_new_session=True)
        try:
            out, err = proc.communicate(timeout=timeout)
        except subprocess.TimeoutExpired:
            os.killpg(proc.pid, signal.SIGKILL)
            out, err = proc.communicate()
        return dict(returncode=proc.returncode, stdout=out.decode(errors='replace'),
                    stderr=err.decode(errors='replace'), exec_s=time.monotonic()-start, command=cmd)

    def finish(self):
        start = time.monotonic()
        self.after = scan(self.shadow, self.base_shadow)
        self.paths = changes(self.base_shadow, self.after)
        old_dirs = {(e['dev'], e['ino']): p for p, e in self.base_shadow.items() if e['type'] == 'directory'}
        renames = []
        for p, e in self.after.items():
            old = old_dirs.get((e['dev'], e['ino'])) if e['type'] == 'directory' else None
            if old and old != p:
                renames.append({'from': old, 'to': p, 'ino': e['ino']})
        self.delta = dict(paths=self.paths, directory_renames=renames,
                          entries=[dict(path=p, before=semantic(self.base_shadow.get(p, ABSENT)),
                                        after=semantic(self.after.get(p, ABSENT))) for p in self.paths])
        self.scan_s = time.monotonic()-start
        return dict(scan_s=self.scan_s, epsilon=self.delta)

    def discard(self):
        # 外部写可能改变真实目录；从真实目录重同步只保证下次 act 一致，
        # 无法恢复已被外部写覆盖的历史字节。报告明确该替代语义。
        start = time.monotonic()
        current = scan(self.real)
        dirty = set(changes(current, self.after))
        reconcile(self.shadow, current, dirty, lambda p: self.real/p)
        self.index = {'real': current, 'shadow': scan(self.shadow)}
        save(self.state/'index.json', self.index)
        return dict(discard_s=time.monotonic()-start, outcome='discarded')

    def commit(self):
        start = time.monotonic()
        if not self.paths:
            return dict(outcome='Unchanged', commit_s=0.0, prepare_s=0.0, apply_s=0.0, conflicts=[])
        current = scan(self.real, self.before)
        conflicts = []
        for p in self.paths:
            if signature(current.get(p, ABSENT)) != signature(self.before.get(p, ABSENT)):
                # 目录 mtime 的变化可能来自无关的同级外部新增；涉及目录项本身仍保守拒绝。
                conflicts.append(p)
        if conflicts:
            return dict(outcome='Conflict', conflicts=conflicts, commit_s=time.monotonic()-start)
        # 与 B3 相同的对象记录、index.clone、dev-ino.anchor 和 poststate.json。
        txn = self.state/'transactions'/str(time.time_ns())
        txn.mkdir(parents=True)
        events = []
        for i, p in enumerate(self.paths):
            b = self.before.get(p, ABSENT)
            e = dict(index=i, path=str(self.real/p), before=b['type'], relative=p)
            e.update({k: b[k] for k in ('mode', 'dev', 'ino', 'nlink', 'link') if k in b})
            events.append(e)
        point('before-log')
        (txn/'ledger.jsonl').write_text(''.join(json.dumps(e)+'\n' for e in events))
        save(txn/'transaction.json', dict(phase='preparing', real=str(self.real), paths=self.paths,
                                         before={p:self.before.get(p, ABSENT) for p in self.paths}))
        point('after-log')
        anchors = {}
        for e in events:
            p = e['relative'];b = self.before.get(p, ABSENT)
            if b['type'] == 'file':
                clone(self.real/p, txn/f"{e['index']}.clone")
                if b['nlink'] > 1:
                    a = txn/f"{b['dev']}-{b['ino']}.anchor"
                    if not a.exists():
                        os.link(self.real/p, a)
                    anchors[p] = a
        point('before-pre-sync')
        barrier(txn)
        point('after-pre-sync')
        record = json.loads((txn/'transaction.json').read_text())
        record['phase'] = 'ready'
        save(txn/'transaction.json', record)
        point('after-ready')
        prep_end = time.monotonic()
        inplace = {p for p in self.paths if self.before.get(p, {}).get('type') == 'file'
                   and self.before[p]['nlink'] > 1
                   and self.after.get(p, {}).get('type') == 'file'
                   and self.after[p]['ino'] == self.base_shadow[p]['ino']}
        reconcile(self.real, self.after, self.paths, lambda p: self.shadow/p,
                  inplace=inplace, checkpoints=True)
        point('before-post-sync')
        barrier(self.real)
        point('after-post-sync')
        post = {str(self.real/p): inspect(self.real/p) for p in self.paths}
        save(txn/'poststate.json', post)
        record['phase'] = 'complete'
        save(txn/'transaction.json', record)
        point('after-complete')
        # 索引更新必须保留 act 期间未涉及项的旧签名，让下次同步发现外部改动。
        for p in self.paths:
            if p in self.after:
                self.index['real'][p] = inspect(self.real/p)
            else:
                self.index['real'].pop(p, None)
        self.index['shadow'] = self.after
        save(self.state/'index.json', self.index)
        return dict(outcome='Committed', transaction=str(txn), commit_s=time.monotonic()-start,
                    prepare_s=prep_end-start, apply_s=time.monotonic()-prep_end, conflicts=[])


def recover(txn, revert=False):
    txn = Path(txn)
    record = json.loads((txn/'transaction.json').read_text())
    root = Path(record['real'])
    phase = record['phase']
    if phase == 'complete' and not revert:
        return dict(outcome='Committed', phase=phase)
    if phase == 'rolled-back':
        return dict(outcome='Restored', phase=phase)
    events = [json.loads(s) for s in (txn/'ledger.jsonl').read_text().splitlines()]
    if phase == 'complete':
        post = json.loads((txn/'poststate.json').read_text())
        conflicts = [p for p, e in post.items() if signature(inspect(Path(p))) != signature(e)]
        if conflicts:
            return dict(outcome='Conflict', conflicts=conflicts)
    if phase in ('ready', 'complete'):
        entries = {e['relative']: e for e in events}
        anchors = {}
        for p, e in entries.items():
            if e.get('nlink', 0) > 1:
                a = txn/f"{e['dev']}-{e['ino']}.anchor"
                if a.exists():
                    anchors[p] = a
        # 暂时放宽所有者写权限，以恢复只读目录内的旧项。
        modes = {}
        for p in record['paths']:
            for d in (root/p, (root/p).parent):
                if d.is_dir() and not d.is_symlink():
                    modes[d] = stat.S_IMODE(d.stat().st_mode)
                    d.chmod(modes[d] | 0o700)
        reconcile(root, record['before'], record['paths'],
                  lambda p: txn/f"{entries[p]['index']}.clone", anchors=anchors)
        for p, mode in modes.items():
            rel = str(p.relative_to(root))
            if p.is_dir() and rel not in record['paths']:
                p.chmod(mode)
        barrier(root)
    for a in txn.glob('*.anchor'):
        a.unlink()
    # 原子替换前的临时文件在 kill 窗口可能残留。
    for p in record['paths']:
        try:
            (root/p).with_name('.s-v4-'+Path(p).name).unlink(missing_ok=True)
        except NotADirectoryError:
            pass
    record['phase'] = 'rolled-back'
    save(txn/'transaction.json', record)
    return dict(outcome='Restored', phase=phase)


if __name__ == '__main__':
    if sys.argv[1] == 'recover':
        print(json.dumps(recover(sys.argv[2], '--revert' in sys.argv)))
    elif sys.argv[1] == 'commit':
        s = Shadow(sys.argv[2], sys.argv[3])
        b = json.loads((s.state/'begin.json').read_text())
        s.before, s.base_shadow = b['real'], b['shadow']
        s.finish()
        print(json.dumps(s.commit()))
        s.close()
