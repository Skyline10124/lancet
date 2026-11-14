#!/usr/bin/env python3
"""非构建时段顺序测量微基准、结构化改名及并发持久化。"""
from pathlib import Path
import json
import os
import runpy
import subprocess
import concurrent.futures
import time
v=runpy.run_path(str(Path(__file__).with_name('experiment-v3.py')))
globals().update({k:v[k] for k in ['HERE','SCRATCH','RESULTS','PROBE','setup','run','dump','view','restore','semantic']})

def micro():
    rows=[]
    for rep in range(3):
        for mode,route in [('none','native'),('ns','namespace'),('B2v2','emulate'),('B3','continue'),('B3','emulate')]:
            name=f'micro-{mode}-{route}-{rep}';root=setup(name,'micro')
            env={'WI_FORCE_EMULATE':'1'} if mode=='B3' and route=='emulate' else {}
            out=run(mode,root,name,[PROBE,'micro-path','/dev/null',20000],limit=120,env=env)
            rows.append(dict(rep=rep,route=route,result=out));dump(RESULTS/'micro.json',rows)
            print(name,out['observations'],out['stats'],flush=True)

def directories():
    rows=[]
    for size in [1000,10000]:
        for rep in range(5):
            name=f'directory-{size}-{rep}';root=setup(name,'rename-dir')
            for p in (root/'dir').iterdir():p.unlink()
            for i in range(size):(root/'dir'/f'f{i:04}').write_text(f'before-{i}')
            before=view(root)
            flush_start=time.monotonic()
            def persist(path):
                fd=os.open(path,os.O_RDONLY);os.fsync(fd);os.close(fd)
            with concurrent.futures.ThreadPoolExecutor(max_workers=8) as pool:list(pool.map(persist,(root/'dir').iterdir()))
            persist(root/'dir');persist(root);flush_seconds=time.monotonic()-flush_start
            out=run('B3',root,name,[PROBE,'rename-large']);recovery=restore(out);after=view(root)
            diff=[p for p in before.keys()|after.keys() if before.get(p)!=after.get(p)]
            rows.append(dict(size=size,rep=rep,result=out,recovery=recovery,exact_difference=diff,fixture_fsync_s=flush_seconds));dump(RESULTS/'directories.json',rows)
            print(name,out['observations'],out['stats'],len(diff),flush=True)
    extras=[]
    for case in ['rename-edit','rename-cross','rename-exchange-dir']:
        name=f'directory-{case}';root=setup(name,'rename-dir');before=view(root)
        out=run('B3',root,name,[PROBE,case]);recovery=restore(out);after=view(root)
        extras.append(dict(case=case,before=before,after=after,result=out,recovery=recovery));dump(RESULTS/'directory-order.json',extras)
        print(case,out['observations'],out['stats'],recovery,flush=True)

def grouped():
    rows=[]
    for rep in range(10):
        name=f'grouped-{rep}';root=setup(name,'open')
        for i in range(8):(root/f'd{i}').mkdir();(root/f'd{i}/a').write_text('before')
        out=run('B3',root,name,[PROBE,'parallel-prestates',8],env={'WI_AUDIT':'1'})
        state=Path(out['command'][3]);audit=[json.loads(s) for s in (state/'durability.jsonl').read_text().splitlines()]
        rows.append(dict(rep=rep,result=out,audit=audit));dump(RESULTS/'grouped.json',rows);print(name,out['stats'],flush=True)
if __name__=='__main__':
    import sys
    for task in sys.argv[1:] or ['micro','directories','grouped']:globals()[task]()
