#!/usr/bin/env python3
"""结束实验后恢复克隆并删除 upper/work；只在 ~/exp/overlay 操作。"""
import os
from pathlib import Path
import shutil
import stat
import sys
import time
from ovl_act import BASE, command, git_state, save
from suite import reset


def accessible_directories(path):
    # overlay 创建 mode=000 的 work/work；目录属于当前用户，只在卸载后恢复遍历权限。
    info=path.lstat()
    if stat.S_ISDIR(info.st_mode):
        os.chmod(path, stat.S_IMODE(info.st_mode) | 0o700)
        for child in path.iterdir():
            if child.is_dir() and not child.is_symlink():
                accessible_directories(child)


def main():
    active=[]
    mounts=[]
    for p in Path('/proc').iterdir():
        if not p.name.isdigit():
            continue
        try:
            if p.stat().st_uid!=os.getuid():
                continue
            args=(p/'cmdline').read_bytes().decode(errors='replace').split('\0')
            if any(a.endswith(('/ovl_act.py','/noise.py','/suite.py')) for a in args):
                active.append(dict(pid=p.name,args=args))
            mounts.extend(dict(pid=p.name,line=l) for l in (p/'mountinfo').read_text().splitlines()
                          if ' - overlay ' in l and str(BASE/'state') in l)
        except (FileNotFoundError,ProcessLookupError):
            continue
    if active or mounts:
        raise RuntimeError('仍有实验进程或挂载：'+str(dict(active=active,mounts=mounts)))
    record=dict(utc_ns=time.time_ns(),active_processes=active,overlay_mounts=mounts,
                free_before_bytes=shutil.disk_usage(BASE).free,
                before_git=git_state(BASE/'repo'),deleted=[],failures=[])
    reset(target=True)
    for state in sorted((BASE/'state').iterdir()):
        for name in ('upper','work'):
            p=state/name
            if p.exists():
                if p.is_symlink() or not p.resolve().is_relative_to(BASE/'state'):
                    raise ValueError('清理路径不在实验状态目录内：'+str(p))
                accessible_directories(p)
                for attempt in (1,2):
                    try:
                        shutil.rmtree(p)
                        record['deleted'].append(str(p))
                        break
                    except OSError as exc:
                        record['failures'].append(dict(path=str(p),attempt=attempt,error=repr(exc)))
    record['remaining_upper_work']=[str(s/n) for s in (BASE/'state').iterdir()
                                    for n in ('upper','work') if (s/n).exists()]
    record['after_git']=git_state(BASE/'repo')
    record['free_after_bytes']=shutil.disk_usage(BASE).free
    record['retained']=[str(BASE/'repo'),str(BASE/'state')+'/*/preimage',str(BASE/'state')+'/*/*.json',
                        str(BASE/'baseline'),str(BASE/'scripts'),str(Path(__file__).parent/'results')]
    record['recorder_status']=command(['systemctl','--user','show','lattice-write-recorder',
                                      '-p','ActiveState','-p','SubState','-p','MainPID','-p','ActiveEnterTimestamp'])
    save(Path(__file__).parent/'results/cleanup.json',record)
    print(dict(deleted=len(record['deleted']),remaining=record['remaining_upper_work'],
               final_status=record['after_git']['status']['stdout'],fsck_code=record['after_git']['fsck']['code']))
    return int(bool(record['remaining_upper_work']))


if __name__=='__main__':
    sys.exit(main())
