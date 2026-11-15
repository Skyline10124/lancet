#!/usr/bin/env python3
"""共享 .git 的索引往返、提交与未提交反例；trace2 记录内容重比对计数。"""
import json
import os
from pathlib import Path
import runpy
import shutil
import subprocess
import sys
import time

HERE=Path(__file__).resolve().parent
v=runpy.run_path(str(HERE/'benchmarks-v5.py'));v['Shadow']=v['s2']['Shadow']
Shadow,dump=v['Shadow'],v['dump'];W,R=v['W'],v['R'];C=v['C']
ROOT=W/'git-regression-v5'
BASE=['git','-c','user.name=bench','-c','user.email=bench@example.invalid','-c','commit.gpgsign=false']
ENV=dict(os.environ,GIT_CONFIG_NOSYSTEM='1',GIT_CONFIG_GLOBAL='/dev/null')


def host(args,env=None):
    trace=ROOT/'target/trace-host.jsonl';trace.unlink(missing_ok=True)
    start=time.monotonic()
    p=subprocess.run([*BASE,*args],cwd=ROOT,env=dict(ENV,**(env or {}),GIT_TRACE2_EVENT=str(trace)),capture_output=True,text=True)
    return dict(returncode=p.returncode,stdout=p.stdout,stderr=p.stderr,wall_s=time.monotonic()-start,trace=trace_stats(trace))


def trace_stats(p):
    data=[json.loads(x) for x in p.read_text().splitlines()] if p.exists() else []
    return [x for x in data if x.get('event')=='data' and ('refresh/' in x.get('key','') or 'preload/' in x.get('key',''))]


def inside(o,args,env):
    trace=ROOT/'target/trace-inside.jsonl';trace.unlink(missing_ok=True)
    result=o.execute([*BASE,*args],env=dict(env,GIT_TRACE2_EVENT=str(trace)))
    result['trace']=trace_stats(trace)
    return result


def main():
    if not ROOT.exists():subprocess.run(['cp','-a','--reflink=always',str(W/'lattice'),str(ROOT)],check=True)
    shutil.rmtree(ROOT/'target',ignore_errors=True);(ROOT/'target').mkdir()
    rows=[]
    configs={
        'default':{},
        'minimal-inside':{'GIT_CONFIG_COUNT':'1','GIT_CONFIG_KEY_0':'core.checkStat','GIT_CONFIG_VALUE_0':'minimal'},
        'minimal-noctime-inside':{'GIT_CONFIG_COUNT':'2','GIT_CONFIG_KEY_0':'core.checkStat','GIT_CONFIG_VALUE_0':'minimal','GIT_CONFIG_KEY_1':'core.trustctime','GIT_CONFIG_VALUE_1':'false'},
        'minimal-noctime-both':{'GIT_CONFIG_COUNT':'2','GIT_CONFIG_KEY_0':'core.checkStat','GIT_CONFIG_VALUE_0':'minimal','GIT_CONFIG_KEY_1':'core.trustctime','GIT_CONFIG_VALUE_1':'false'},
    }
    for config,extra in configs.items():
        env=dict(GIT_CONFIG_NOSYSTEM='1',GIT_CONFIG_GLOBAL='/dev/null',**extra)
        hostenv=extra if config.endswith('-both') else {}
        for operation in ['status','add','commit','checkout']:
            for rep in range(3):
                host(['checkout','-f','--quiet','--detach',C['a']])
                warm=host(['status','--porcelain','-uno'],hostenv)
                state=W/'states-v5'/f'git-{config}-{operation}-{rep}';shutil.rmtree(state,ignore_errors=True)
                o=Shadow(ROOT,state);sync=o.begin()
                if operation in ['add','commit']:
                    out=o.execute(['python3','-c',"from pathlib import Path;p=Path('wi-checkout-data/d00/f0000.txt');p.write_text(p.read_text()+'v4-git\\n')"],env)
                    if out['returncode']:raise RuntimeError(out)
                if operation=='status': cmd=['status','--porcelain','-uno']
                elif operation=='add':cmd=['add','wi-checkout-data/d00/f0000.txt']
                elif operation=='commit':
                    inside(o,['add','wi-checkout-data/d00/f0000.txt'],env);cmd=['commit','--quiet','-m','bench: v4 shared git']
                else:cmd=['checkout','--quiet','--detach',C['b']]
                executed=inside(o,cmd,env);fin=o.finish();com=o.commit()
                real=host(['status','--porcelain','-uno'],hostenv)
                again=inside(o,['status','--porcelain','-uno'],env)
                row=dict(config=config,operation=operation,rep=rep,sync=sync,inside=executed,commit=com,host=real,inside_after_host=again,
                         expected_status='M  wi-checkout-data/d00/f0000.txt\n' if operation=='add' else '')
                row['status_expected']=real['stdout']==row['expected_status']
                rows.append(row);dump(R/'git-regression-v5.json',rows);o.close()
                print(config,operation,rep,real['wall_s'],real['trace'],flush=True)
    counter=[]
    for operation in ['checkout','commit']:
        host(['checkout','-f','--quiet','--detach',C['a']]);before_head=host(['rev-parse','HEAD'])['stdout']
        state=W/'states-v5'/f'git-discard-{operation}';shutil.rmtree(state,ignore_errors=True)
        o=Shadow(ROOT,state);o.begin()
        if operation=='checkout':executed=inside(o,['checkout','--quiet','--detach',C['b']],{})
        else:
            (o.shadow/'wi-checkout-data/d00/f0000.txt').write_text('new-content\n')
            inside(o,['add','wi-checkout-data/d00/f0000.txt'],{})
            executed=inside(o,['commit','--quiet','-m','bench: discarded act'],{})
        fin=o.finish();discard=o.discard();after_head=host(['rev-parse','HEAD'])['stdout'];real=host(['status','--porcelain','-uno'])
        counter.append(dict(operation=operation,execution=executed,epsilon_count=len(fin['epsilon']['paths']),discard=discard,
                            head_before=before_head,head_after=after_head,head_preserved=before_head==after_head,host=real,
                            changed_lines=len(real['stdout'].splitlines())))
        o.close();dump(R/'git-regression-discard-v5.json',counter)
    # 同秒等长写且保留 mtime：展示 minimal + trustctime=false 的漏检代价。
    host(['checkout','-f','--quiet','--detach',C['a']]);p=ROOT/'wi-checkout-data/d00/f0000.txt'
    old_time=int(time.time())-60;os.utime(p,(old_time,old_time))
    relaxed=configs['minimal-noctime-both'];host(['status','--porcelain','-uno'],relaxed)
    old=p.read_bytes();p.write_bytes(b'X'+old[1:]);os.utime(p,(old_time,old_time))
    missed=host(['status','--porcelain','-uno'],relaxed);normal=host(['status','--porcelain','-uno'])
    dump(R/'git-minimal-risk-v5.json',dict(relaxed=missed,default=normal,bytes_changed=p.read_bytes()!=old))
    host(['checkout','-f','--quiet','--detach',C['original']])


if __name__=='__main__':main()
