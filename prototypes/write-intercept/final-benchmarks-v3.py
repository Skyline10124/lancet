#!/usr/bin/env python3
"""优化后的 B3 与 namespace 成对重测；保留先前全部样本。"""
from pathlib import Path
import json
import gzip
import runpy
import shutil
v=runpy.run_path(str(Path(__file__).with_name('benchmarks-v3.py')))
R=v['RESULTS'];dump=v['dump']
# 相同代码的 none/B2v2 基线保留；ns 与 B3 成对重跑，控制共享主机的阶段噪声。
if not (R/'builds-before-optimization.json').exists():
    shutil.copyfile(R/'builds.json',R/'builds-before-optimization.json')
    old=json.loads((R/'builds.json').read_text());dump(R/'builds.json',[r for r in old if r['mode'] not in ['ns','B3']])
v['full'].__globals__['MODES']=['ns','B3'];v['full']()
if not (R/'checkouts-before-optimization.json').exists() and not (R/'checkouts-before-optimization.json.gz').exists():shutil.copyfile(R/'checkouts.json',R/'checkouts-before-optimization.json')
archive=R/'checkouts-before-optimization.json'
old=json.loads(archive.read_text()) if archive.exists() else json.loads(gzip.decompress(archive.with_suffix('.json.gz').read_bytes()));rows=[r for r in old if r['mode']!='B3']
commits=v['prepare_checkout']()
for rep in range(3):
    v['git']('checkout','--quiet','--detach',commits['a'])
    out=v['run']('B3',v['WORK'],f'checkout-B3-{rep}',['git','checkout','--quiet','--detach',commits['b']],limit=180,env=v['environment']())
    out['changed_files']=commits['files'];out['git_result']=v['git']('rev-parse','HEAD')
    rows.append(out);dump(R/'checkouts.json',rows);print(out['name'],out['returncode'],out['stats'],flush=True)
v['git']('checkout','--quiet','--detach',commits['original'])
