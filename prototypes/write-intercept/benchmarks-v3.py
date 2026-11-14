#!/usr/bin/env python3
"""四配置顺序基准；只在自己的 reflink 副本生成工作负载提交。"""
import json
import os
from pathlib import Path
import runpy
import shutil
import subprocess
v=runpy.run_path(str(Path(__file__).with_name('experiment-v3.py')))
run,SCRATCH,RESULTS,dump=[v[k] for k in ['run','SCRATCH','RESULTS','dump']]
WORK=SCRATCH/'lattice';CACHE=SCRATCH/'cargo-home'
TC=Path.home()/'.rustup/toolchains/1.96.0-x86_64-unknown-linux-gnu/bin'
MODES=['none','ns','B2v2','B3']

def environment():
    (SCRATCH/'home-v3').mkdir(exist_ok=True)
    return dict(CARGO_HOME=str(CACHE),RUSTC=str(TC/'rustc'),RUSTDOC=str(TC/'rustdoc'),
        RUSTUP_HOME=str(Path.home()/'.rustup'),HOME=str(SCRATCH/'home-v3'),CARGO_BUILD_JOBS='4',
        CARGO_PROFILE_DEV_DEBUG='0',CARGO_PROFILE_TEST_DEBUG='0',CARGO_INCREMENTAL='0',
        GIT_CONFIG_NOSYSTEM='1',GIT_CONFIG_GLOBAL='/dev/null',TMPDIR=str(WORK/'target/tmp'),
        LC_ALL='C',PATH=str(TC)+':'+os.environ['PATH'])

def git(*args):
    return subprocess.check_output(['git','-C',str(WORK),'-c','user.name=bench','-c','user.email=bench@example.invalid','-c','commit.gpgsign=false',*args],text=True).strip()

def prepare_checkout():
    meta=RESULTS/'checkout-commits.json'
    if meta.exists():return json.loads(meta.read_text())
    original=git('rev-parse','HEAD');tracked=len(git('ls-files').splitlines())
    git('checkout','-b','wi-v3-checkout')
    for i in range(4000):
        path=WORK/'wi-checkout-data'/f'd{i%64:02}'/f'f{i:04}.txt';path.parent.mkdir(parents=True,exist_ok=True);path.write_text(f'before-{i}\n'*32)
    git('add','--','wi-checkout-data');git('commit','-m','bench: checkout prestate A');a=git('rev-parse','HEAD')
    for i in range(4000):(WORK/'wi-checkout-data'/f'd{i%64:02}'/f'f{i:04}.txt').write_text(f'after-{i}\n'*32)
    git('add','--','wi-checkout-data');git('commit','-m','bench: checkout prestate B');b=git('rev-parse','HEAD')
    changes=git('diff','--name-only',a,b).splitlines()
    data=dict(original=original,tracked_original=tracked,a=a,b=b,files=len(changes),paths=changes,synthetic=True,
        reason='副本当前只有 485 个跟踪文件；构造两个只改变 4000 个常规文本文件的提交，保留 LATTICE 真实仓库与构建源。')
    dump(meta,data);return data

def full():
    path=RESULTS/'builds.json';rows=json.loads(path.read_text()) if path.exists() else [];done={r['name'] for r in rows if r['returncode']==0}
    for rep in range(3):
        for mode in MODES[rep:]+MODES[:rep]:
            name=f'full-{mode}-{rep}'
            if name in done:continue
            shutil.rmtree(WORK/'target',ignore_errors=True);(WORK/'target/tmp').mkdir(parents=True)
            out=run(mode,WORK,name,[TC/'cargo','build','--offline','--workspace'],limit=1200,env=environment())
            rows.append(out);dump(path,rows);print(name,out['returncode'],out['stats'],out['stderr'][-1000:],flush=True)
            if out['returncode']:return

def checkout():
    commits=prepare_checkout();rows=[]
    for rep in range(3):
        for mode in MODES[rep:]+MODES[:rep]:
            git('checkout','--quiet','--detach',commits['a'])
            name=f'checkout-{mode}-{rep}'
            out=run(mode,WORK,name,['git','checkout','--quiet','--detach',commits['b']],limit=180,env=environment())
            out['changed_files']=commits['files'];out['git_result']=git('rev-parse','HEAD')
            rows.append(out);dump(RESULTS/'checkouts.json',rows);print(name,out['returncode'],out['stats'],flush=True)
    git('checkout','--quiet','--detach',commits['original'])

def missing_target():
    rows=[]
    for condition in ['empty','absent']:
        shutil.rmtree(WORK/'target',ignore_errors=True)
        if condition=='empty':(WORK/'target').mkdir()
        env=environment();tmp=SCRATCH/'tmp-v3';tmp.mkdir(exist_ok=True);env['TMPDIR']=str(tmp)
        out=run('B3',WORK,f'build-target-{condition}',[TC/'cargo','build','--offline','--workspace'],limit=1200,env=env)
        rows.append(dict(condition=condition,result=out));dump(RESULTS/'missing-target.json',rows);print(condition,out['returncode'],out['stats'],out['stderr'][-1000:],flush=True)
if __name__=='__main__':
    import sys
    for task in sys.argv[1:] or ['full','checkout','missing_target']:globals()[task]()
