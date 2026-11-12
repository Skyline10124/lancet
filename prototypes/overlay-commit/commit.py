"""上层分类、整批冲突预检与逐文件提交；不实现崩溃恢复。"""
import os
from pathlib import Path
import shutil
import stat
import subprocess
import tempfile
import time


def fs_time(directory):
    fd, name = tempfile.mkstemp(prefix='t0-', dir=directory)
    try:
        return os.fstat(fd).st_ctime_ns
    finally:
        os.close(fd)
        os.unlink(name)


def metadata(path):
    try:
        s = path.lstat()
        return dict(size=s.st_size, mtime_ns=s.st_mtime_ns, ctime_ns=s.st_ctime_ns,
                    inode=s.st_ino, mode=s.st_mode)
    except FileNotFoundError:
        return None


def is_target(path):
    return path == 'target' or path.startswith('target/')


def walk(root):
    """不跟随符号链接；将目录本身与子项分别记录。"""
    if not root.is_dir() or root.is_symlink():
        return
    for parent, dirs, files in os.walk(root, followlinks=False):
        for name in sorted(dirs + files):
            yield Path(parent) / name


def snapshot(repo, include_target):
    result = {}
    for parent, dirs, files in os.walk(repo, followlinks=False):
        if Path(parent) == repo and not include_target:
            dirs[:] = [d for d in dirs if d != 'target']
        for name in sorted(dirs + files):
            p = Path(parent) / name
            rel = str(p.relative_to(repo))
            if include_target or not is_target(rel):
                result[rel] = metadata(p)
    return result


def attrs(path):
    return {name: os.getxattr(path, name, follow_symlinks=False)
            for name in os.listxattr(path, follow_symlinks=False)
            if name.startswith('user.overlay.')}


def same(a, b):
    sa, sb = metadata(a), metadata(b)
    if sa is None or sb is None or sa['mode'] != sb['mode']:
        return False
    if a.is_symlink():
        return os.readlink(a) == os.readlink(b)
    if not a.is_file() or sa['size'] != sb['size']:
        return False
    with a.open('rb') as x, b.open('rb') as y:
        while True:
            ax, by = x.read(1024 * 1024), y.read(1024 * 1024)
            if ax != by:
                return False
            if not ax:
                return True


def classify(repo, upper):
    out = dict(operations=[], timestamp_only=[], redirect=[], opaque=[], entries=[])
    blocked = []
    for p in walk(upper):
        rel = str(p.relative_to(upper))
        s, xa = p.lstat(), attrs(p)
        out['entries'].append(dict(path=rel, mode=s.st_mode, size=s.st_size,
                                   xattrs={k: v.hex() for k, v in xa.items()}))
        if any(rel.startswith(x + '/') for x in blocked):
            continue
        if 'user.overlay.redirect' in xa:
            out['redirect'].append(rel)
            blocked.append(rel)
            continue
        dst = repo / rel
        if stat.S_ISCHR(s.st_mode) and s.st_rdev == os.makedev(0, 0):
            out['operations'].append(dict(path=rel, kind='delete'))
        elif stat.S_ISDIR(s.st_mode):
            # opaque=x 只是内核遍历优化，只有 y 才隐藏下层目录。
            if xa.get('user.overlay.opaque') == b'y':
                out['opaque'].append(rel)
                if dst.is_dir() and not dst.is_symlink():
                    for old in sorted(dst.iterdir()):
                        if not os.path.lexists(p / old.name):
                            out['operations'].append(dict(path=str(old.relative_to(repo)), kind='delete'))
                out['operations'].append(dict(path=rel, kind='directory', opaque=True))
            elif not dst.is_dir() or dst.is_symlink() or stat.S_IMODE(s.st_mode) != stat.S_IMODE(dst.stat().st_mode):
                out['operations'].append(dict(path=rel, kind='directory', opaque=False))
        elif stat.S_ISREG(s.st_mode) or stat.S_ISLNK(s.st_mode):
            if same(p, dst):
                out['timestamp_only'].append(rel)
            else:
                out['operations'].append(dict(path=rel, kind='replace'))
        else:
            raise ValueError(f'不支持的上层条目：{rel}，mode={s.st_mode}')
    return out


def remove(path):
    if path.is_dir() and not path.is_symlink():
        shutil.rmtree(path)
    elif os.path.lexists(path):
        path.unlink()


def strip_attrs(path):
    for name in attrs(path):
        os.removexattr(path, name, follow_symlinks=False)


def check_conflicts(repo, operations, baseline, t0, mode):
    paths = set()
    for op in operations:
        rel = op['path']
        if is_target(rel):
            continue
        paths.add(rel)
        if op['kind'] in ('delete', 'replace') and (repo / rel).is_dir():
            paths.update(str(p.relative_to(repo)) for p in walk(repo / rel))
        # C1 需要看到整目录删除前已被外部移走的后代。
        if op['kind'] == 'delete' and baseline is not None:
            paths.update(p for p in baseline if p.startswith(rel + '/'))
    conflicts = {'C1': [], 'C4': []}
    observations = {}
    for rel in sorted(paths):
        now = metadata(repo / rel)
        before = baseline.get(rel) if baseline is not None else None
        observations[rel] = dict(before=before, current=now)
        if mode in ('C1', 'both') and before != now:
            conflicts['C1'].append(rel)
        if mode in ('C4', 'both') and now is not None and now['ctime_ns'] > t0:
            conflicts['C4'].append(rel)
    return conflicts, observations


def commit(repo, upper, preimage, baseline, t0, mode, only_target=False, discard=False):
    start = time.monotonic()
    classification = classify(repo, upper)
    classified = time.monotonic()
    ops = classification['operations']
    if only_target:
        ops = [op for op in ops if is_target(op['path'])]
    conflicts, observations = check_conflicts(repo, ops, baseline, t0, mode)
    checked = time.monotonic()
    out = dict(classification=classification, conflicts=conflicts,
               checked_paths=sorted(observations), observations=observations,
               preimages=[], committed_paths=[], overlay_xattr_residual_count=0,
               timings=dict(classify_s=classified-start, conflict_s=checked-classified))
    decision = conflicts['C4' if mode == 'C4' else 'C1']
    if discard or decision:
        out['outcome'] = 'discarded' if discard else 'conflict'
        out['timings']['commit_s'] = 0
        out['xattr_scan'] = dict(command=[], code=0, stdout='', stderr='', scanned_paths=[])
        return out
    # 全部检查通过后才生成前态；不将 target/ 递归复制进前态。
    for rel in sorted(observations):
        p, dest = repo / rel, preimage / rel
        if not os.path.lexists(p):
            continue
        dest.parent.mkdir(parents=True, exist_ok=True)
        if p.is_symlink():
            dest.symlink_to(os.readlink(p))
        elif p.is_dir():
            dest.mkdir(exist_ok=True)
            os.chmod(dest, stat.S_IMODE(p.stat().st_mode))
        else:
            subprocess.run(['cp', '--reflink=always', '--preserve=mode,timestamps', '--', str(p), str(dest)], check=True, capture_output=True)
        out['preimages'].append(rel)
    backed = time.monotonic()
    # 删除先行，目录由浅至深建立，然后移动普通文件与链接。
    for op in sorted(ops, key=lambda x: (x['kind'] != 'delete', x['path'].count('/'), x['path'])):
        rel = op['path']
        src, dst = upper / rel, repo / rel
        if op['kind'] == 'delete':
            remove(dst)
        elif op['kind'] == 'directory':
            if os.path.lexists(dst) and (not dst.is_dir() or dst.is_symlink()):
                remove(dst)
            dst.mkdir(parents=True, exist_ok=True)
            os.chmod(dst, stat.S_IMODE(src.stat().st_mode))
            strip_attrs(src)
            strip_attrs(dst)
        else:
            dst.parent.mkdir(parents=True, exist_ok=True)
            strip_attrs(src)
            if dst.is_dir() and not dst.is_symlink():
                remove(dst)
            os.replace(src, dst)
        out['committed_paths'].append(rel)
    written = time.monotonic()
    scan = [str(repo / rel) for rel in out['committed_paths'] if os.path.lexists(repo / rel)]
    stdout, stderr, code = '', '', 0
    # 分批避免超过 ARG_MAX；递归扫描提交目录，去重残留路径/属性。
    for i in range(0, len(scan), 128):
        r = subprocess.run(['getfattr', '-h', '-d', '-m', '-', '-R', '--', *scan[i:i+128]], capture_output=True, text=True)
        stdout += r.stdout
        stderr += r.stderr
        code = max(code, r.returncode)
    residuals, current = set(), ''
    for line in stdout.splitlines():
        if line.startswith('# file: '):
            current = line[8:]
        elif line.startswith('user.overlay.'):
            residuals.add((current, line.split('=', 1)[0]))
    out['overlay_xattr_residual_count'] = len(residuals)
    out['xattr_scan'] = dict(command=['getfattr', '-h', '-d', '-m', '-', '-R'], code=code,
                             stdout=stdout, stderr=stderr, scanned_paths=scan)
    out['outcome'] = 'committed'
    out['timings'].update(preimage_s=backed-checked, write_s=written-backed,
                          commit_s=written-checked, xattr_scan_s=time.monotonic()-written)
    return out
