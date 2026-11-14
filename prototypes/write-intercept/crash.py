#!/usr/bin/env python3
"""检查点持久化后，SIGKILL 真正的节点进程；观测未放行调用和已持有 fd。"""
import mmap
import os
from pathlib import Path
import signal
import struct
import time
from experiment import SCRATCH, RESULTS, dump, fixture, run, snapshot

def main():
    rows=[]
    for mode in ['A','B1','B2']:
        name=f'crash-{mode}';root=SCRATCH/'fixtures'/name;fixture(root,'crash')
        control=SCRATCH/f'{name}.control';control.write_bytes(struct.pack('i',1)+bytes(4092))
        killed=[]
        def writer():
            with control.open('r+b') as f,mmap.mmap(f.fileno(),4096) as m:
                end=time.monotonic()+10
                while struct.unpack_from('i',m)[0]!=2:
                    if time.monotonic()>end:raise RuntimeError('检查点握手超时')
                    time.sleep(.0001)
                matching=[]
                for p in Path('/proc').glob('[0-9]*'):
                    try:
                        args=(p/'cmdline').read_bytes().split(b'\0')
                        if len(args)>3 and args[0]==os.fsencode(SCRATCH/'intercept') and args[1]==mode.encode() and args[2]==os.fsencode(root):
                            matching.append(int(p.name))
                    except (OSError,ProcessLookupError):pass
                # 启动垫片继承 argv；最早出生的是宿主节点，其他是它的后代。
                pid=min(matching);killed.append(dict(node_pid=pid,processes=matching))
                os.kill(pid,signal.SIGKILL)
        fd=os.open(root/'b',os.O_RDWR);before=snapshot(root)
        out=run(mode,root,name,[SCRATCH/'probe','hold-existing-fd',fd],limit=10,
                env={'WI_RACE_CONTROL':str(control)},pass_fds=(fd,),external=writer)
        os.close(fd)
        rows.append(dict(mode=mode,killed=killed,before=before,after=snapshot(root),result=out))
        print(mode,out['returncode'],out['stdout'],flush=True)
    dump(RESULTS/'crash.json',rows)

if __name__=='__main__':main()
