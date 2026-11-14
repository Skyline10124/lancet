#!/usr/bin/env python3
"""确定性检查/执行/回复窗口与节点 SIGKILL 实验；命中率不是自然攻击概率。"""
import json
import mmap
import os
from pathlib import Path
import runpy
import signal
import struct
import time
v=runpy.run_path(str(Path(__file__).with_name('experiment-v2.py')))
globals().update({k:v[k] for k in ['SCRATCH','RESULTS','PROBE','setup','run','dump','view']})

def control_for(name,initial=1):
    p=SCRATCH/f'v2-{name}.control';p.write_bytes(struct.pack('i',initial)+bytes(4092));return p

def wait_control(m):
    end=time.monotonic()+12
    while struct.unpack_from('i',m)[0]!=2:
        if time.monotonic()>end:raise RuntimeError('竞态握手超时')
        time.sleep(.0001)

def parameters():
    rows=[]
    for rep in range(3):
        name=f'parameter-{rep}';root=setup(name,'race');control=control_for(name,0)
        out=run('B2v2',root,name,[PROBE,'race',200],limit=60,env={'WI_RACE_CONTROL':str(control)})
        rows.append(out);dump(RESULTS/'parameter-races.json',rows);print(name,out['observations'],flush=True)

def replacements():
    rows=[]
    cases=['open','open-inside','rename','renameat','renameat2','unlink','unlinkat','rmdir','link','linkat']
    for case in cases:
        patterns=['rename-old'] if case.startswith('open') else ['rename-old','remove-old']
        for pattern in patterns:
            for rep in range(20):
                name=f'replace-{case}-{pattern}-{rep}';root=setup(name,'race');ctl=control_for(name)
                if case=='rmdir':(root/'race-safe').unlink();(root/'race-safe').mkdir()
                before=view(root);trace=[]
                def writer(proc):
                    with ctl.open('r+b') as f,mmap.mmap(f.fileno(),4096) as m:
                        wait_control(m);trace.append(dict(phase='checkpoint',pid=proc.pid))
                        if pattern=='rename-old':(root/'race-safe').rename(root/'spare')
                        elif case=='rmdir':(root/'race-safe').rmdir()
                        else:(root/'race-safe').unlink()
                        if case=='rmdir':(root/'race-safe').mkdir()
                        elif case=='open':(root/'race-safe').symlink_to('race-evil')
                        else:(root/'race-safe').write_text('replacement')
                        trace.append(dict(phase='replaced',ino=(root/'race-safe').lstat().st_ino))
                        struct.pack_into('i',m,0,3)
                out=run('B2v2',root,name,[PROBE,'replace-'+case],env={'WI_RACE_CONTROL':str(ctl)},external=None if case=='open-inside' else writer)
                rows.append(dict(case=case,pattern=pattern,rep=rep,before=before,after=view(root),trace=trace,result=out))
                dump(RESULTS/'replacements.json',rows)
            print(case,pattern,sum(r['result']['stats'].get('indeterminate',0) for r in rows if r['case']==case and r['pattern']==pattern),flush=True)

def cancellations():
    rows=[]
    for case in ['cancel-open','cancel-unlink']:
        for rep in range(20):
            name=f'{case}-{rep}';root=setup(name,case);ctl=control_for(name)
            before=view(root)
            out=run('B2v2',root,name,[PROBE,case],env={'WI_RACE_CONTROL':str(ctl),'WI_RACE_PHASE':'1'})
            rows.append(dict(case=case,rep=rep,before=before,after=view(root),result=out));dump(RESULTS/'cancellations.json',rows)
        print(case,'done',flush=True)

def crashes():
    rows=[]
    for mode in ['B2','B2v2']:
        for rep in range(50):
            name=f'crash-{mode}-{rep}';root=setup(name,'crash');ctl=control_for(name);killed=[]
            def writer(proc):
                with ctl.open('r+b') as f,mmap.mmap(f.fileno(),4096) as m:
                    wait_control(m);killed.append(dict(pid=proc.pid,monotonic=time.monotonic()))
                    os.kill(proc.pid,signal.SIGKILL)
            before=view(root)
            out=run(mode,root,name,[PROBE,'crash-old'],env={'WI_RACE_CONTROL':str(ctl)},external=writer)
            rows.append(dict(mode=mode,rep=rep,killed=killed,before=before,after=view(root),result=out));dump(RESULTS/'crashes.json',rows)
        print('crash',mode,'done',flush=True)

if __name__=='__main__':
    import sys
    tasks={'parameters':parameters,'replacements':replacements,'cancellations':cancellations,'crashes':crashes}
    for task in sys.argv[1:] or tasks:tasks[task]()
