#!/usr/bin/env python3
"""20,000 次 open/close 与含 1,000 文件目录改名；在构建基准结束后顺序运行。"""
from pathlib import Path
import runpy
v=runpy.run_path(str(Path(__file__).with_name('experiment-v2.py')))
globals().update({k:v[k] for k in ['SCRATCH','RESULTS','PROBE','setup','run','dump']})

def main():
    rows=[]
    for rep in range(3):
        for mode in ['none','ns','B2','B2v2']:
            for case in ['micro','rename-large']:
                name=f'{case}-{mode}-{rep}';root=setup(name,case)
                out=run(mode,root,name,[PROBE,case,20000] if case=='micro' else [PROBE,case],limit=120)
                rows.append(out);dump(RESULTS/'timings.json',rows);print(name,out['stats'],flush=True)
        name=f'rename-large-B2v2-batch-{rep}';root=setup(name,'rename-large')
        out=run('B2v2',root,name,[PROBE,'rename-large'],limit=120,env={'WI_BATCH':'1'})
        rows.append(out);dump(RESULTS/'timings.json',rows);print(name,out['stats'],flush=True)

if __name__=='__main__':main()
