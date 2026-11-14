#!/usr/bin/env python3
"""第三轮确定性竞态、锁与崩溃恢复实验；数据只写 results/v3。"""
import json
import mmap
import os
from pathlib import Path
import runpy
import signal
import struct
import subprocess
import time
v=runpy.run_path(str(Path(__file__).with_name('experiment-v3.py')))
globals().update({k:v[k] for k in ['HERE','SCRATCH','RESULTS','PROBE','setup','run','dump','view','restore','state_of','semantic']})

def control_for(name,initial=1):
    p=SCRATCH/f'v3-{name}.control';p.write_bytes(struct.pack('i',initial)+bytes(4092));return p

def wait_control(m):
    end=time.monotonic()+12
    while struct.unpack_from('i',m)[0]!=2:
        if time.monotonic()>end:raise RuntimeError('竞态握手超时')
        time.sleep(.0001)

def landlock():
    rows=[]
    for kind in ['open','truncate','rename','unlink','link','symlink','mkdir']:
        name=f'landlock-{kind}';root=setup(name,'race');ctl=control_for(name,0)
        (root/'target/race-safe').write_text('allowed')
        before=view(root)
        out=run('B3',root,name,[PROBE,'ll-race',kind,200],limit=120,env={'WI_RACE_CONTROL':str(ctl)})
        after=view(root)
        regular_changes=[p for p in before.keys()|after.keys() if p.split('/')[0] not in ['target','.git'] and semantic(before.get(p,{'type':'Absent'}))!=semantic(after.get(p,{'type':'Absent'}))]
        rows.append(dict(kind=kind,result=out,regular_changes=regular_changes));dump(RESULTS/'landlock-races.json',rows);print(name,out['observations'],regular_changes,flush=True)
    residual=[]
    for kind,variant,n in [('open','hardlink',200),('link','writable-cross',1),('rename','writable-cross',1)]:
        name=f'landlock-{variant}-{kind}';root=setup(name,'race');ctl=control_for(name,0)
        (root/'target/race-safe').write_text('allowed');os.link(root/'race-evil',root/'target/preexisting-alias')
        before=view(root)
        out=run('B3',root,name,[PROBE,'ll-race',kind,n,variant],limit=120,env={'WI_RACE_CONTROL':str(ctl)})
        residual.append(dict(kind=kind,variant=variant,before=before,after=view(root),result=out));dump(RESULTS/'landlock-residual.json',residual);print(name,out['observations'],flush=True)

def internal():
    rows=[]
    for pattern in ['remove','move','target','aba']:
        for attack in ['sequence']:
            for rep in range(20):
                name=f'inside-{pattern}-{attack}-{rep}';root=setup(name,'race');ctl=control_for(name,0)
                (root/'replacement').write_text('replacement');(root/'dest').write_text('destination')
                if pattern=='aba':(root/'race-safe').unlink()
                out=run('B3',root,name,[PROBE,'inside-race',pattern,attack],env={'WI_RACE_CONTROL':str(ctl),'WI_INTERNAL_LOCK':'1','WI_RACE_PHASE':'2' if pattern=='aba' else '0'})
                rows.append(dict(pattern=pattern,attack=attack,rep=rep,result=out,completed_inside_lock=struct.unpack_from('i',ctl.read_bytes(),12)[0]));dump(RESULTS/'internal-races.json',rows)
            print('inside',pattern,attack,'done',flush=True)

def directory_lock():
    rows=[]
    for rep in range(20):
        name=f'global-lock-{rep}';root=setup(name,'race');ctl=control_for(name,0)
        (root/'race-safe-dir').mkdir();(root/'race-safe-dir/a').write_text('before')
        out=run('B3',root,name,[PROBE,'directory-lock'],env={'WI_RACE_CONTROL':str(ctl),'WI_INTERNAL_LOCK':'1'})
        rows.append(out);dump(RESULTS/'directory-lock.json',rows)

def external():
    rows=[]
    for case,pattern in [('unlink','remove'),('unlink','move'),('rename','move'),('rename','target'),('symlink','aba')]:
        for rep in range(20):
            name=f'external-{case}-{pattern}-{rep}';root=setup(name,'race');ctl=control_for(name)
            (root/'dest').write_text('destination')
            if pattern=='aba':(root/'race-safe').unlink()
            before=view(root);trace=[]
            def writer(proc):
                with ctl.open('r+b') as f,mmap.mmap(f.fileno(),4096) as m:
                    wait_control(m)
                    target=root/('dest' if pattern=='target' else 'race-safe')
                    if pattern=='remove':target.unlink()
                    else:target.rename(root/'spare')
                    if pattern=='aba':target.symlink_to('a')
                    else:target.write_text('replacement')
                    trace.append(state_of(target));struct.pack_into('i',m,0,3)
            out=run('B3',root,name,[PROBE,'replace-'+case],env={'WI_RACE_CONTROL':str(ctl),'WI_RACE_PHASE':'2' if pattern=='aba' else '0'},external=writer)
            after=view(root);recovery=restore(out) if out['stats'].get('indeterminate') else None
            restored_view=view(root) if recovery else None
            recovery_check=None if recovery is None else dict(unconflicted_b=restored_view['b']==before['b'],conflicts_untouched=all(restored_view.get(str(Path(p).relative_to(root)),{'type':'Absent'})==after.get(str(Path(p).relative_to(root)),{'type':'Absent'}) for p in recovery['conflicts']))
            rows.append(dict(case=case,pattern=pattern,rep=rep,before=before,after=after,trace=trace,result=out,recovery=recovery,recovery_check=recovery_check));dump(RESULTS/'external-races.json',rows)
        print('external',case,pattern,'done',flush=True)

def anchors():
    rows=[]
    for case in ['unlink-hardlink','rename-hardlink','anchor-unlink-all','anchor-overwrite']:
        for rep in range(3):
            name=f'anchor-{case}-{rep}';root=setup(name,'hardlink-normal');before=view(root)
            out=run('B3',root,name,[PROBE,case]);recovery=restore(out);after=view(root)
            paths=['a','hard','b'];equal=all(before[p]==after[p] for p in paths)
            operation_ok=out['returncode']==0 and out['stats'].get('emulated',0)>0 and out['stats'].get('anchors',0)>0 and all(x.get('rc',0)>=0 for x in out['observations'])
            rows.append(dict(case=case,rep=rep,before=before,after=after,result=out,recovery=recovery,exact_inode_topology=equal,operation_ok=operation_ok));dump(RESULTS/'anchors.json',rows)
            print(name,equal,recovery['errors'],flush=True)

def crashes():
    rows=[]
    for rep in range(50):
        name=f'crash-B3-{rep}';root=setup(name,'crash');ctl=control_for(name);killed=[]
        def writer(proc):
            with ctl.open('r+b') as f,mmap.mmap(f.fileno(),4096) as m:
                wait_control(m);killed.append(dict(pid=proc.pid,monotonic=time.monotonic()));os.kill(proc.pid,signal.SIGKILL)
        before=view(root)
        out=run('B3',root,name,[PROBE,'crash-v3'],env={'WI_RACE_CONTROL':str(ctl)},external=writer)
        state=Path(out['command'][3]);recovery=json.loads(subprocess.check_output(['python3',str(HERE/'recover-v3.py'),str(state),'--pending'],text=True))
        pids=json.loads((state/'pids.json').read_text());live=[]
        for key,pid in pids.items():
            try:
                stat=Path(f'/proc/{pid}/stat').read_text().split();
                if stat[2]!='Z':live.append(key)
            except FileNotFoundError:pass
        rows.append(dict(rep=rep,killed=killed,before=before,after=view(root),result=out,recovery=recovery,live_processes=live,returned_errno=struct.unpack_from('i',ctl.read_bytes(),16)[0],fd_write=struct.unpack_from('i',ctl.read_bytes(),20)[0],allowed_write=struct.unpack_from('i',ctl.read_bytes(),24)[0]));dump(RESULTS/'crashes.json',rows)
    print('crashes done',flush=True)
if __name__=='__main__':
    import sys
    tasks={'landlock':landlock,'internal':internal,'external':external,'anchors':anchors,'crashes':crashes,'directory_lock':directory_lock}
    for task in sys.argv[1:] or tasks:tasks[task]()
