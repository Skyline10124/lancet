#!/usr/bin/env python3
"""第二轮外置 oracle。仅本 worktree 的 .scratch 和 results/v2 可写。"""
import argparse
import concurrent.futures
import ctypes
import hashlib
import json
import mmap
import os
from pathlib import Path
import shutil
import signal
import stat
import struct
import subprocess
import time
from experiment import HERE, SCRATCH, dump, fixture, sha

RESULTS=HERE/'results/v2'
MODES=['none','ns','B2','B2v2']
BIN=SCRATCH/'intercept-v2'
PROBE=SCRATCH/'probe-v2'

def state_of(p):
    try: st=p.lstat()
    except FileNotFoundError:return {'type':'Absent'}
    r={'mode':stat.S_IMODE(st.st_mode),'dev':st.st_dev,'ino':st.st_ino,'nlink':st.st_nlink}
    if stat.S_ISREG(st.st_mode):r.update(type='file',sha256=sha(p))
    elif stat.S_ISDIR(st.st_mode):r['type']='directory'
    elif stat.S_ISLNK(st.st_mode):r.update(type='symlink',link=os.readlink(p))
    elif stat.S_ISFIFO(st.st_mode):r['type']='fifo'
    else:r['type']='special'
    return r

def view(root):return {str(p.relative_to(root)):state_of(p) for p in sorted(root.rglob('*'))}
def semantic(s):return {k:v for k,v in s.items() if k not in ('dev','ino','nlink')}
def before_entry(e,state):
    r={'type':e['before']}
    if e['before']!='Absent':r['mode']=stat.S_IMODE(e['mode'])
    if e['before']=='file':r['sha256']=sha(state/f"{e['index']}.clone")
    if e['before']=='symlink':r['link']=e['link']
    return r

def run(mode,root,name,command,*,limit=30,env=None,pass_fds=(),external=None):
    state=SCRATCH/'states-v2'/name
    shutil.rmtree(state,ignore_errors=True);state.mkdir(parents=True)
    cmd=[str(BIN),mode,str(root),str(state),str(limit),'--',*map(str,command)]
    start=time.monotonic();e=os.environ.copy();e.update(env or {})
    with concurrent.futures.ThreadPoolExecutor(max_workers=1) as pool:
        proc=subprocess.Popen(cmd,env=e,stdin=subprocess.PIPE,stdout=subprocess.PIPE,stderr=subprocess.PIPE,pass_fds=pass_fds)
        writer=pool.submit(external,proc) if external else None
        try: stdout,stderr=proc.communicate(b'',timeout=limit+15)
        except subprocess.TimeoutExpired:
            proc.kill();stdout,stderr=proc.communicate(timeout=10)
        if writer:writer.result()
    wall=time.monotonic()-start
    stats=json.loads((state/'stats.json').read_text()) if (state/'stats.json').exists() else {}
    entries=[json.loads(x) for x in (state/'ledger.jsonl').read_text().splitlines()] if (state/'ledger.jsonl').exists() else []
    t=time.monotonic();changed=[]
    for entry in entries:
        entry['relative']=str(Path(entry['path']).relative_to(root))
        entry['after']=state_of(Path(entry['path']))
        if 'mode' in entry:
            entry['prestate']=before_entry(entry,state)
            if semantic(entry['after'])!=entry['prestate']:changed.append(entry['relative'])
        if entry['before']=='file':entry['clone_sha256']=sha(state/f"{entry['index']}.clone")
    stats['digest_s']=time.monotonic()-t
    out=dict(name=name,mode=mode,command=cmd,environment=env or {},returncode=proc.returncode,process_wall_s=wall,
             stats=stats,ledger=entries,changes=changed,stdout=stdout.decode(errors='replace'),stderr=stderr.decode(errors='replace'))
    out['observations']=[json.loads(x) for x in out['stdout'].splitlines() if x.startswith('{')]
    dump(RESULTS/'runs'/f'{name}.json',out);return out

def setup(name,case):
    root=SCRATCH/'fixtures-v2'/name
    if root.exists():
        for d in [root,*root.rglob('*')]:
            if d.is_dir() and not d.is_symlink():d.chmod(0o700)
    fixture(root,case)
    (root/'dangling').symlink_to('new-target')
    (root/'target/a').write_text('target-before')
    if case=='git-hardlink':os.link(root/'a',root/'.git/alias')
    (root/'readonly-file').write_text('read-only');(root/'readonly-file').chmod(0o444)
    (root/'readonly').mkdir(mode=0o555)
    (root/'tmpmount').mkdir();(root/'jail').mkdir();(root/'jail/a').write_text('jail-before')
    return root

def restore(out):
    """最小求逆：先核对登记集后态，保住仍存在的原 inode；不扫描工作树。"""
    entries=out['ledger'];state=Path(out['command'][3]);conflicts=[];fds={};sources={}
    for e in entries:
        p=Path(e['path'])
        if state_of(p)!=e['after']:conflicts.append(e['relative'])
        if e['after']['type']=='file':
            key=(e['after']['dev'],e['after']['ino']);sources[key]=p
            if key not in fds:fds[key]=os.open(p,os.O_RDONLY)
    if conflicts:
        for fd in fds.values():os.close(fd)
        return {'conflicts':conflicts,'errors':[]}
    errors=[];temporary_modes={}
    # 还原器是宿主拥有者；暂时增加自身写权限，最后还原目录与文件原权限。
    for e in entries:
        p=Path(e['path'])
        for d in [p.parent,p]:
            if d.exists() and not d.is_symlink() and d.stat().st_uid==os.getuid():
                old=stat.S_IMODE(d.stat().st_mode)
                if not old&stat.S_IWUSR:
                    temporary_modes[d]=old;d.chmod(old|stat.S_IWUSR)
    try:
        # 最深路径先清理。所有源 inode 的稳定 fd 已先取得，目录 rename 不丢内容。
        for e in sorted(entries,key=lambda e:len(Path(e['path']).parts),reverse=True):
            p=Path(e['path']);a=e['after'];b=e['before']
            keep=(b=='file' and a['type']=='file' and (e['dev'],e['ino'])==(a['dev'],a['ino'])) or b==a['type']=='directory'
            if a['type']!='Absent' and not keep:
                try:p.rmdir() if a['type']=='directory' else p.unlink()
                except OSError as ex:errors.append([e['relative'],str(ex)])
        restored={}
        for e in sorted(entries,key=lambda e:len(Path(e['path']).parts)):
            p=Path(e['path']);b=e['before'];key=(e['dev'],e['ino']);mode=stat.S_IMODE(e['mode'])
            try:
                if b=='Absent':continue
                if b=='directory':p.mkdir(exist_ok=True);p.chmod(mode)
                elif b=='symlink':p.symlink_to(e['link'])
                elif b=='fifo':os.mkfifo(p,mode)
                elif b=='file':
                    if not p.exists() and key in restored:os.link(restored[key],p)
                    if not p.exists() and key in sources and sources[key].exists() and (sources[key].stat().st_dev,sources[key].stat().st_ino)==key:os.link(sources[key],p)
                    if not p.exists() and key in fds and os.fstat(fds[key]).st_nlink>0:
                        # rename 的目的端已在登记集中；保留的 fd 可连接仍有别名的原 inode。
                        libc=ctypes.CDLL(None,use_errno=True)
                        rc=libc.linkat(-100,os.fsencode(f'/proc/self/fd/{fds[key]}'),-100,os.fsencode(p),0x400)
                        if rc:raise OSError(ctypes.get_errno(),os.strerror(ctypes.get_errno()))
                    # 已存在的硬链接用原 inode 写回。不可 relink 的零链接 inode 会重建并报告 inode 差异。
                    with p.open('wb') as f,(state/f"{e['index']}.clone").open('rb') as src:shutil.copyfileobj(src,f)
                    p.chmod(mode);restored[key]=p
            except OSError as ex:errors.append([e['relative'],str(ex)])
    finally:
        for fd in fds.values():os.close(fd)
        for p,mode in temporary_modes.items():
            if p.exists():p.chmod(mode)
    return dict(conflicts=conflicts,errors=errors)

CASES='''mkdir-trailing symlink-proc-text open open-rdwr open-append open-trunc-readonly open-readonly open-create open-excl-fail open-idle openat cwd openat2 openat2-beneath creat tmpfile tmpfile-empty opath proc-cwd proc-root symlink-alias dangling-symlink umask hardlink-alias hardlink-normal git-hardlink unlink unlinkat rmdir unlinkat-dir rename renameat rename-exchange rename-noreplace rename-dir truncate link linkat symlink symlinkat unlink-symlink mkdir mkdirat chmod fchmod fchmodat fchmodat2 setxattr lsetxattr fsetxattr removexattr lremovexattr fremovexattr mknod mknodat ftruncate fallocate mmap inherited-ftruncate inherited-fallocate inherited-mmap hidden-open hidden-rename hidden-link hidden-symlink hidden-mkdir hidden-creat hidden-openat hidden-openat2 hidden-mkdirat hidden-mknod hidden-mknodat hidden-symlinkat hidden-linkat hidden-renameat hidden-renameat2 cross-target cross-git cross-target-git within-target regenerable git nofollow excl-symlink cloexec not-cloexec resolve-beneath resolve-beneath-escape resolve-in-root resolve-in-root-create resolve-nosym resolve-nomagic resolve-noxdev resolve-noxdev-ok resolve-nosym-ok resolve-nomagic-ok resolve-dangling-nosym perm-file perm-file-nocap perm-dir perm-dir-nocap tmpfs-perm tmpfs-perm-nocap chroot fork vfork thread thread-exec orphan daemon io-uring nondumpable'''.split()

def coverage(cases=None):
    rows=[]
    for case in cases or CASES:
        for mode in ['ns','B2v2']:
            name=f'coverage-{mode}-{case}';root=setup(name,case);before=view(root)
            command=[PROBE,case];fd=None
            if case.startswith('inherited'):fd=os.open(root/'a',os.O_RDWR);command.append(fd)
            out=run(mode,root,name,command,pass_fds=(fd,) if fd is not None else ())
            if fd is not None:os.close(fd)
            after=view(root);restoration=None
            if mode=='B2v2' and not out['stats'].get('indeterminate'):
                restoration=restore(out);restored=view(root)
                expected={k:v for k,v in before.items() if k.split('/')[0] not in ['target','.git','hidden']}
                actual={k:v for k,v in restored.items() if k.split('/')[0] not in ['target','.git','hidden']}
                restoration['semantic_difference']=[p for p in sorted(expected.keys()|actual.keys()) if semantic(expected.get(p,{'type':'Absent'}))!=semantic(actual.get(p,{'type':'Absent'}))]
                restoration['hardlink_inodes_preserved']=all(before[p]['ino']==restored.get(p,{}).get('ino') for p in before if before[p]['type']=='file' and before[p]['nlink']>1)
            row=dict(case=case,mode=mode,result=out,before=before,after=after,restoration=restoration)
            rows.append(row);dump(RESULTS/'coverage.json',rows)
            print(name,out['observations'],out['stderr'][:150],restoration,flush=True)

if __name__=='__main__':
    p=argparse.ArgumentParser();p.add_argument('suite',choices=['coverage']);p.add_argument('cases',nargs='*');args=p.parse_args()
    if args.suite=='coverage':coverage(args.cases)
