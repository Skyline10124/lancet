#!/usr/bin/env python3
"""第二轮外置 oracle。仅本 worktree 的 .scratch 和 results/v3 可写。"""
import argparse
import gzip
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

RESULTS=HERE/'results/v3'
MODES=['none','ns','B2','B2v2']
BIN=SCRATCH/'intercept-v3'
PROBE=SCRATCH/'probe-v3'

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
    state=SCRATCH/'states-v3'/name
    shutil.rmtree(state,ignore_errors=True);state.mkdir(parents=True)
    cmd=[str(SCRATCH/'intercept-v2' if mode=='B2v2' else BIN),mode,str(root),str(state),str(limit),'--',*map(str,command)]
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
    moves=[e for e in entries if e.get("record")=="rename"]
    entries=[e for e in entries if e.get("record")!="rename"]
    t=time.monotonic();changed=[]
    for entry in entries:
        entry['relative']=str(Path(entry['path']).relative_to(root))
        entry['after']=state_of(Path(entry['path']))
        if 'mode' in entry:
            entry['prestate']=before_entry(entry,state)
            if semantic(entry['after'])!=entry['prestate']:changed.append(entry['relative'])
        if entry['before']=='file':entry['clone_sha256']=sha(state/f"{entry['index']}.clone")
    stats['digest_s']=time.monotonic()-t
    out=dict(name=name,mode=mode,command=cmd,environment=env or {},returncode=proc.returncode,node_pid=proc.pid,process_wall_s=wall,
             stats=stats,ledger=entries,moves=moves,changes=changed,stdout=stdout.decode(errors='replace'),stderr=stderr.decode(errors='replace'))
    out['observations']=[json.loads(x) for x in out['stdout'].splitlines() if x.startswith('{')]
    if len(entries)>1000:
        dest=RESULTS/'runs'/f'{name}.json.gz';dest.parent.mkdir(parents=True,exist_ok=True)
        with gzip.open(dest,'wt') as f:json.dump(out,f,ensure_ascii=False)
    else:dump(RESULTS/'runs'/f'{name}.json',out)
    return out

def setup(name,case):
    root=SCRATCH/'fixtures-v3'/name
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
    state=Path(out['command'][3])
    post={e['path']:e['after'] for e in out['ledger']}
    for m in out['moves']:
        post[m['to']]=state_of(Path(m['to']))
        if m.get('exchange'):post[m['from']]=state_of(Path(m['from']))
    dump(state/'poststate.json',post)
    with (state/'poststate.json').open('rb') as f:os.fsync(f.fileno())
    result=subprocess.run(['python3',str(HERE/'recover-v3.py'),str(state)],capture_output=True,text=True,check=True)
    return json.loads(result.stdout)

CASES='''mkdir-trailing symlink-proc-text open open-rdwr open-append open-trunc-readonly open-readonly open-create open-excl-fail open-idle openat cwd openat2 openat2-beneath creat tmpfile tmpfile-empty opath proc-cwd proc-root symlink-alias dangling-symlink umask hardlink-alias hardlink-normal git-hardlink unlink unlinkat rmdir unlinkat-dir rename renameat rename-exchange rename-noreplace rename-dir truncate link linkat symlink symlinkat unlink-symlink mkdir mkdirat chmod fchmod fchmodat fchmodat2 setxattr lsetxattr fsetxattr removexattr lremovexattr fremovexattr mknod mknodat ftruncate fallocate mmap inherited-ftruncate inherited-fallocate inherited-mmap hidden-open hidden-rename hidden-link hidden-symlink hidden-mkdir hidden-creat hidden-openat hidden-openat2 hidden-mkdirat hidden-mknod hidden-mknodat hidden-symlinkat hidden-linkat hidden-renameat hidden-renameat2 cross-target cross-git cross-target-git within-target regenerable git nofollow excl-symlink cloexec not-cloexec resolve-beneath resolve-beneath-escape resolve-in-root resolve-in-root-create resolve-nosym resolve-nomagic resolve-noxdev resolve-noxdev-ok resolve-nosym-ok resolve-nomagic-ok resolve-dangling-nosym perm-file perm-file-nocap perm-dir perm-dir-nocap tmpfs-perm tmpfs-perm-nocap chroot fork vfork thread thread-exec orphan daemon io-uring nondumpable'''.split()

def coverage(cases=None):
    rows=[]
    for case in cases or CASES:
        for mode in ['ns','B3']:
            name=f'coverage-{mode}-{case}';root=setup(name,case);before=view(root)
            command=[PROBE,case];fd=None
            if case.startswith('inherited'):fd=os.open(root/'a',os.O_RDWR);command.append(fd)
            out=run(mode,root,name,command,pass_fds=(fd,) if fd is not None else ())
            if fd is not None:os.close(fd)
            after=view(root);restoration=None
            if mode=='B3' and not out['stats'].get('indeterminate'):
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
