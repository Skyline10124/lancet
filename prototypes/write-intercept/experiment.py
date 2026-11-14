#!/usr/bin/env python3
"""原型外置判定器；全树观测只用于测试 oracle，不属于被测节点算法。"""
import argparse
import concurrent.futures
import fcntl
import gzip
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import time

HERE = Path(__file__).resolve().parent
SCRATCH = HERE / '.scratch'
RESULTS = HERE / 'results'
MODES = ['none', 'ns', 'A', 'B1', 'B2']
CASES = '''open open-rdwr open-append open-trunc-readonly open-readonly open-create
open-excl-fail openat cwd openat2 openat2-beneath creat tmpfile tmpfile-empty opath
proc-cwd proc-root symlink-alias hardlink-alias hardlink-normal unlink unlinkat
rmdir unlinkat-dir rename renameat rename-exchange rename-noreplace rename-dir rename-large
truncate link linkat symlink symlinkat unlink-symlink mkdir mkdirat chmod fchmod fchmodat
chown lchown fchown fchownat utimensat setxattr lsetxattr fsetxattr mknod mknodat
ftruncate fallocate mmap inherited-ftruncate inherited-fallocate inherited-mmap
hidden-open hidden-rename hidden-link hidden-symlink hidden-mkdir regenerable git
fork vfork thread thread-exec orphan daemon deadline ptrace-traceme ptrace-attach
filter-allow filter-bypass io-uring nondumpable external-window'''.split()

def dump(path, data):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(data, ensure_ascii=False, indent=2) + '\n')

def sha(path):
    h = hashlib.sha256()
    with path.open('rb') as f:
        for block in iter(lambda: f.read(1024 * 1024), b''):
            h.update(block)
    return h.hexdigest()

def snapshot(root):
    out = {}
    for p in sorted(root.rglob('*')):
        s = p.lstat()
        if p.is_symlink():
            content = {'link': os.readlink(p)}
        elif p.is_file():
            content = {'sha256': sha(p)}
        else:
            content = {'kind': 'directory' if p.is_dir() else 'special'}
        out[str(p.relative_to(root))] = dict(content, mode=s.st_mode, uid=s.st_uid,
            gid=s.st_gid, mtime_ns=s.st_mtime_ns, ctime_ns=s.st_ctime_ns, nlink=s.st_nlink,
            xattrs={k: os.getxattr(p, k, follow_symlinks=False).hex()
                    for k in os.listxattr(p, follow_symlinks=False) if k.startswith('user.')})
    return out

def regular(p):
    return p.split('/')[0] not in ['target', '.git', 'hidden']

def fixture(root, case):
    shutil.rmtree(root, ignore_errors=True)
    root.mkdir(parents=True)
    for d in ['target', '.git', 'hidden', 'sub', 'empty']:
        (root/d).mkdir()
    for name in ['a', 'b', 'sub/a', 'outside', 'race-safe', 'race-evil']:
        (root/name).write_bytes((name + ':BEFORE\n').encode().ljust(4096, b'.'))
    (root/'alias').symlink_to('a')
    if case == 'hardlink-alias': os.link(root/'a', root/'target/alias')
    if case == 'hardlink-normal': os.link(root/'a', root/'hard')
    if case.startswith('rename-') and case in ['rename-dir', 'rename-large']:
        (root/'dir').mkdir()
        for i in range(1000 if case == 'rename-large' else 3):
            (root/'dir'/f'f{i:04}').write_text(f'before-{i}')

def run(mode, root, name, command, *, limit=60, env=None, pass_fds=(), external=None):
    state = SCRATCH/'states'/name
    shutil.rmtree(state, ignore_errors=True)
    state.mkdir(parents=True)
    cmd = [str(SCRATCH/'intercept'), mode, str(root), str(state), str(limit), '--', *map(str,command)]
    e = os.environ.copy()
    e.update(env or {})
    start = time.monotonic()
    with concurrent.futures.ThreadPoolExecutor(max_workers=1) as pool:
        writer = pool.submit(external) if external else None
        result = subprocess.run(cmd, env=e, capture_output=True, text=True, pass_fds=pass_fds,
                                timeout=limit+15)
        if writer: writer.result()
    wall = time.monotonic()-start
    stats = json.loads((state/'stats.json').read_text()) if (state/'stats.json').exists() else {}
    ledger = [json.loads(s) for s in (state/'ledger.jsonl').read_text().splitlines()]
    digest_start = time.monotonic()
    for entry in ledger:
        p = Path(entry['path'])
        entry['relative'] = str(p.relative_to(root))
        # 写后摘要只访问登记集；符号链接单独记录链接文字，不跟随树遍历。
        entry['after'] = ('symlink:'+os.readlink(p) if p.is_symlink()
                          else sha(p) if p.is_file() else 'Absent')
    stats['digest_s'] = time.monotonic()-digest_start
    for entry in ledger:
        if entry['before'] == 'file': entry['clone_sha256'] = sha(state/f"{entry['index']}.clone")
    out = dict(name=name, mode=mode, command=cmd, environment=env or {}, returncode=result.returncode,
               process_wall_s=wall, stats=stats, ledger=ledger, stdout=result.stdout, stderr=result.stderr)
    dump(RESULTS/'runs'/f'{name}.json', out)
    return out

def coverage():
    rows = []
    for mode in ['A','B1','B2']:
        for case in CASES:
            name=f'{mode}-{case}';root=SCRATCH/'fixtures'/name;fixture(root,case)
            before=snapshot(root); fd=None;command=[SCRATCH/'probe',case]
            if case.startswith('inherited'):
                fd=os.open(root/'a',os.O_RDWR);command.append(fd)
            def writer():
                time.sleep(.1);(root/'outside').write_text('external-editor')
            out=run(mode,root,name,command,limit=2 if case=='deadline' else 20,
                    pass_fds=(fd,) if fd is not None else (),
                    external=writer if case=='external-window' else None)
            if fd is not None:os.close(fd)
            after=snapshot(root)
            changed=[];metadata=[]
            for p in sorted(before.keys() | after.keys()):
                b=before.get(p,{});a=after.get(p,{})
                if {k:b.get(k) for k in ['sha256','link','kind']} != {k:a.get(k) for k in ['sha256','link','kind']}:
                    changed.append(p)
                elif b!=a:metadata.append(p)
            entries={e['relative']:e for e in out['ledger']}
            missing=[p for p in changed if regular(p) and p!='outside' and p not in entries]
            prestate_bad=[p for p,e in entries.items()
                         if (e['before']=='Absent' and p in before)
                         or (e['before']=='file' and e.get('clone_sha256')!=before.get(p,{}).get('sha256'))]
            row=dict(name=name,mode=mode,case=case,stats=out['stats'],returncode=out['returncode'],
                     observations=[json.loads(s) for s in out['stdout'].splitlines() if s.startswith('{')],
                     changed=changed,metadata=metadata,missing=missing,prestate_bad=prestate_bad,
                     registered=list(entries),external_registered='outside' in entries)
            rows.append(row)
            with gzip.open(RESULTS/'runs'/f'{name}-oracle.json.gz','wt') as f:
                json.dump(dict(before=before,after=after),f)
            print(name, 'rc',out['returncode'],'missing',len(missing),'bad',len(prestate_bad), flush=True)
    dump(RESULTS/'coverage.json',rows)

def races():
    rows=[]
    for mode in ['A','B1','B2']:
        for rep in range(3):
            name=f'race-{mode}-{rep}';root=SCRATCH/'fixtures'/name;fixture(root,'race')
            control=SCRATCH/f'{name}.control';control.write_bytes(bytes(4096))
            out=run(mode,root,name,[SCRATCH/'probe','race',200],limit=30,
                    env={'WI_RACE_CONTROL':str(control)})
            entries={e['relative']:e for e in out['ledger']}
            rows.append(dict(mode=mode,rep=rep,stats=out['stats'],returncode=out['returncode'],
                evil_changed=(root/'race-evil').read_bytes()==b'X',
                safe_changed=(root/'race-safe').read_bytes()==b'X',
                evil_registered='race-evil' in entries,registered=list(entries),
                observations=[json.loads(s) for s in out['stdout'].splitlines() if s.startswith('{')]))
            print(rows[-1],flush=True)
    dump(RESULTS/'races.json',rows)

def failures():
    rows=[]
    for mode in ['A','B1','B2']:
        name=f'failure-{mode}';root=SCRATCH/'fixtures'/name;fixture(root,'hold')
        env={'WI_CRASH_AFTER':'1'} if mode=='A' else {'WI_CLOSE_AFTER':'1'}
        out=run(mode,root,name,[SCRATCH/'probe','hold'],limit=10,env=env)
        rows.append(out)
    dump(RESULTS/'failures.json',rows)

def environment():
    commands=['uname -a','id','getenforce','getsebool deny_ptrace',
              'cat /proc/sys/kernel/yama/ptrace_scope /proc/sys/kernel/seccomp/actions_avail',
              'df -T .','rustc +1.96.0 --version','gcc --version','git --version','nproc',
              'git --no-optional-locks --no-pager rev-parse HEAD',
              'git --no-optional-locks --no-pager -C /home/gcp-vegas/Dev/lattice rev-parse HEAD']
    data={s:subprocess.run(s,shell=True,capture_output=True,text=True).stdout for s in commands}
    a=SCRATCH/'ficlone-source';b=SCRATCH/'ficlone-dest';a.write_bytes(b'BEFORE')
    with a.open('rb') as src,b.open('wb') as dst:fcntl.ioctl(dst,0x40049409,src.fileno())
    a.write_bytes(b'AFTER!')
    data['ficlone']={'source_device':a.stat().st_dev,'checkpoint_device':b.stat().st_dev,
                     'checkpoint_unchanged':b.read_bytes()==b'BEFORE'}
    dump(RESULTS/'environment.json',data)

def micro():
    rows=[]
    for rep in range(3):
        for mode in MODES:
            name=f'micro-{mode}-{rep}';root=SCRATCH/'fixtures'/name;fixture(root,'micro')
            out=run(mode,root,name,[SCRATCH/'probe','micro',20000])
            rows.append(out)
            print(name,out['stats'],flush=True)
    dump(RESULTS/'micro.json',rows)

if __name__=='__main__':
    parser=argparse.ArgumentParser();parser.add_argument('phase',choices=['environment','coverage','races','failures','micro','all'])
    args=parser.parse_args();SCRATCH.mkdir(exist_ok=True);RESULTS.mkdir(exist_ok=True)
    for phase in (['environment','coverage','races','failures','micro'] if args.phase=='all' else [args.phase]):
        globals()[phase]()
