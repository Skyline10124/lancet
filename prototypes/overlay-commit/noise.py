#!/usr/bin/env python3
"""外部写入仅作用于实验克隆；频率由冻结记录校准。"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import random
import subprocess
import time
from ovl_act import BASE, command, save


def main():
    p = argparse.ArgumentParser()
    p.add_argument('--repo',type=Path,required=True)
    p.add_argument('--config',type=Path,required=True)
    p.add_argument('--stop',type=Path,required=True)
    p.add_argument('--output',type=Path,required=True)
    args=p.parse_args()
    repo=args.repo.resolve()
    assert repo == BASE/'repo'
    config=json.loads(args.config.read_text())
    run_seed=config['seed'] ^ int(hashlib.sha256(args.output.parent.name.encode()).hexdigest()[:16],16)
    rng=random.Random(run_seed)
    events=[]
    started=time.monotonic()
    due={k:rng.uniform(0,v) for k,v in config['intervals_s'].items() if v}
    phases=dict(due)
    due['git_status']=0.0
    intervals=dict(config['intervals_s'],git_status=config['git_status_interval_s'])
    tracked=subprocess.check_output(['git','-C',str(repo),'ls-files','-z']).decode().split('\0')[:-1]
    cargo=None
    if config['cargo_check_once']:
        log=args.output.with_suffix('.cargo.log')
        log_handle=log.open('w')
        cargo=subprocess.Popen(['cargo','check','--message-format=json','-q'],cwd=repo,stdout=log_handle,stderr=subprocess.STDOUT)
    seq=0
    def git(*argv, env=None):
        r=command(['git','-C',repo,*argv],env=env)
        events.append(dict(t=time.time(),type=kind,result=r))
        return r
    def lock_cycle(path, payload=b''):
        path.parent.mkdir(parents=True,exist_ok=True)
        try:
            with path.open('xb') as f:
                f.write(payload)
            path.unlink()
            events.append(dict(t=time.time(),type=kind,path=str(path.relative_to(repo)),operation='create-close-delete'))
        except FileExistsError as exc:
            events.append(dict(t=time.time(),type=kind,error=str(exc)))
    while not args.stop.exists():
        now=time.monotonic()-started
        for kind in list(due):
            if now<due[kind]:
                continue
            due[kind]+=intervals[kind]
            seq+=1
            if kind=='git_status':
                git('status','--porcelain')
            elif kind=='source_touch':
                path=repo/rng.choice(tracked)
                os.utime(path,None)
                events.append(dict(t=time.time(),type=kind,path=str(path.relative_to(repo))))
            elif kind=='index_refresh':
                git('update-index','--refresh')
            elif kind=='fetch_head':
                path=repo/'.git/FETCH_HEAD'
                data=path.read_bytes() if path.exists() else b''
                path.write_bytes(data)
                events.append(dict(t=time.time(),type=kind,path='.git/FETCH_HEAD',bytes=len(data)))
            elif kind=='ephemeral_dirs':
                paths=[repo/'.codex',repo/'.aws']
                for path in paths:
                    path.mkdir(exist_ok=False)
                t=time.time()
                time.sleep(config['ephemeral_lifetime_s'])
                for path in paths:
                    path.rmdir()
                events.append(dict(t=t,end=time.time(),type=kind,paths=['.codex','.aws']))
            elif kind in ('config_lock','packed_refs_lock','remote_head_lock'):
                path={'config_lock':'.git/config.lock','packed_refs_lock':'.git/packed-refs.lock',
                      'remote_head_lock':'.git/refs/remotes/origin/HEAD.lock'}[kind]
                lock_cycle(repo/path)
            elif kind=='worktree_index':
                # 工作树事件映射到独立索引，避免制造无效 .git/worktrees 管理目录。
                env=dict(os.environ,GIT_INDEX_FILE=str(repo/'.git/experiment-worktree-index'))
                git('read-tree','HEAD',env=env)
            elif kind=='checkpoint':
                env=dict(os.environ,GIT_INDEX_FILE=str(repo/'.git/t3-checkpoint-index-experiment'))
                git('add','-A',env=env)
                tree=git('write-tree',env=env)
                if tree['code']==0:
                    git('update-ref','refs/t3/experiment',tree['stdout'].strip())
        time.sleep(0.02)
    cargo_record=None
    if cargo:
        code=cargo.wait(timeout=300)
        log_handle.close()
        cargo_record=dict(command=['cargo','check','--message-format=json','-q'],code=code,output=log.read_text())
    save(args.output,dict(started_ns=time.time_ns()-int((time.monotonic()-started)*1e9),duration_s=time.monotonic()-started,
                          config=config,run_seed=run_seed,initial_phases_s=phases,events=events,cargo=cargo_record))


if __name__=='__main__':
    main()
