#!/usr/bin/env python3
"""诊断 checkout 同步与 clone 成本；额外测一次批量预刷盘的替代策略。"""
import json
from pathlib import Path
import runpy
import time

HERE=Path(__file__).resolve().parent
v=runpy.run_path(str(HERE/'benchmarks-v4.py'))
Shadow,git,WORK,W,R,C,env=[v[k] for k in ['Shadow','git','WORK','W','R','COMMITS','environment']]
s=v['s'];rows=[]
original=s['clone'];totals={}
def clone(a,b):
    t=time.monotonic()
    try:return original(a,b)
    finally:
        totals['clone_s']=totals.get('clone_s',0)+time.monotonic()-t
        totals['clone_n']=totals.get('clone_n',0)+1
# 函数共享实际 globals；只插入计时，不改克隆行为。
original.__globals__['clone']=clone
for rep in range(3):
    for preflush in [False,True]:
        git('checkout','-f','--quiet','--detach',C['a'])
        o=Shadow(WORK,W/'states-v4/bench-lattice');totals.clear();t=time.monotonic()
        if preflush:s['barrier'](WORK)
        flush_s=time.monotonic()-t
        sync=o.begin();sync_profile=dict(totals);totals.clear()
        executed=o.execute(['git','checkout','--quiet','--detach',C['b']],env=env())
        fin=o.finish();com=o.commit();wall=time.monotonic()-t
        row=dict(preflush=preflush,rep=rep,flush_s=flush_s,sync=sync,sync_profile=sync_profile,execution=executed,scan_s=fin['scan_s'],commit=com,commit_profile=dict(totals),total_s=wall)
        rows.append(row);s['dump'](R/'profile-v4.json',rows);o.close();print(preflush,rep,flush_s,sync,sync_profile,com,wall,flush=True)
git('checkout','-f','--quiet','--detach',C['original'])
