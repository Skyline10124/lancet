#!/usr/bin/env python3
"""额外边界：代执行上下文、文件系统竞态、监听 fd 关闭后的旧 fd。"""
import json
import mmap
import os
import struct
import time
from experiment import SCRATCH, RESULTS, dump, fixture, run, snapshot

def main():
    rows=[]
    for mode in ['A','B1','B2']:
        for case in ['dangling-symlink','umask','fs-race','hold-existing-fd']:
            name=f'extra-{mode}-{case}';root=SCRATCH/'fixtures'/name;fixture(root,case)
            (root/'dangling').symlink_to('not-yet')
            command=[SCRATCH/'probe',case];env={};fd=None;writer=None
            if case=='fs-race':
                control=SCRATCH/f'{name}.control';control.write_bytes(struct.pack('i',1)+bytes(4092))
                env={'WI_RACE_CONTROL':str(control)}
                def writer():
                    with control.open('r+b') as f, mmap.mmap(f.fileno(),4096) as m:
                        deadline=time.monotonic()+10
                        while struct.unpack_from('i',m)[0]!=2:
                            if time.monotonic()>deadline:raise RuntimeError('未收到路径替换握手')
                            time.sleep(.0001)
                        (root/'race-safe').rename(root/'race-old')
                        (root/'race-safe').symlink_to('race-evil')
                        struct.pack_into('i',m,0,3)
            if case=='hold-existing-fd':
                fd=os.open(root/'b',os.O_RDWR);command.append(fd)
                env={'WI_CRASH_AFTER':'1'} if mode=='A' else {'WI_CLOSE_AFTER':'1'}
            before=snapshot(root)
            out=run(mode,root,name,command,env=env,external=writer,
                    pass_fds=(fd,) if fd is not None else ())
            if fd is not None:os.close(fd)
            after=snapshot(root)
            rows.append(dict(name=name,mode=mode,case=case,before=before,after=after,
                             stats=out['stats'],ledger=out['ledger'],stdout=out['stdout'],returncode=out['returncode']))
            print(name,out['stdout'].strip(),flush=True)
    dump(RESULTS/'supplement.json',rows)

if __name__=='__main__':main()
