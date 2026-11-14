#!/usr/bin/env python3
"""顺序运行四配置；B3 驱动不改源码，仅把输出目录定向到 v4。"""
import json
import os
from pathlib import Path
import runpy
import shutil
import subprocess
import sys
import time

HERE=Path(__file__).resolve().parent
sys.path.insert(0,str(HERE))
v3=runpy.run_path(str(HERE/'benchmarks-v3.py'))
s=runpy.run_path(str(HERE/'shadow-v4.py'))
Shadow,dump=s['Shadow'],s['dump']
W=HERE/'.scratch';R=HERE/'results/v4';WORK=W/'lattice'
run=v3['run'];run.__globals__['RESULTS']=R
MODES=['none','ns','B3','S']
environment=v3['environment'];TC=v3['TC'];git=v3['git']
COMMITS=json.loads((HERE/'results/v3/checkout-commits.json').read_text())


def measured(mode,root,name,command,env=None,state=None):
    if mode!='S':
        out=run(mode,root,name+'-v4',command,limit=1200,env=env)
        return dict(name=name,mode=mode,returncode=out['returncode'],wall_s=out['stats']['wall_s'],
                    stats=out['stats'],stdout=out['stdout'],stderr=out['stderr'],process_wall_s=out['process_wall_s'])
    o=Shadow(root,state or W/'states-v4'/('bench-'+root.name))
    start=time.monotonic()
    a=o.begin();out=o.execute(command,env=env);b=o.finish();c=o.commit() if out['returncode']==0 else o.discard()
    wall=time.monotonic()-start
    o.close()
    return dict(name=name,mode=mode,wall_s=wall,**out,**a,scan_s=b['scan_s'],changed=len(b['epsilon']['paths']),settlement=c)


def full():
    rows=[]
    git('checkout','--quiet','--detach',COMMITS['original'])
    # 持久影子首次建立单列，正常 act 计时不重复计算初始化。
    o=Shadow(WORK,W/'states-v4/bench-lattice');init=o.begin();o.close();dump(R/'lattice-initial-v4.json',init)
    for rep in range(3):
        for mode in MODES[rep:]+MODES[:rep]:
            shutil.rmtree(WORK/'target',ignore_errors=True);(WORK/'target/tmp').mkdir(parents=True)
            out=measured(mode,WORK,f'full-{mode}-{rep}',[TC/'cargo','build','--offline','--workspace'],environment())
            rows.append(out);dump(R/'builds-v4.json',rows)
            print(out['name'],out['returncode'],out['wall_s'],out['stderr'][-400:],flush=True)
            if out['returncode']:raise RuntimeError('构建失败，禁止计为有效样本')


def checkout():
    rows=[]
    for rep in range(3):
        for mode in MODES[rep:]+MODES[:rep]:
            git('checkout','--quiet','--detach',COMMITS['a'])
            out=measured(mode,WORK,f'checkout-{mode}-{rep}',['git','checkout','--quiet','--detach',COMMITS['b']],environment())
            out['head']=git('rev-parse','HEAD');out['status']=git('status','--porcelain','--untracked-files=no')
            out['expected_head']=COMMITS['b']
            rows.append(out);dump(R/'checkouts-v4.json',rows)
            print(out['name'],out['returncode'],out['wall_s'],out.get('settlement'),flush=True)
            if out['returncode'] or out['head']!=COMMITS['b'] or out['status']:raise RuntimeError('检出不完整')
    git('checkout','--quiet','--detach',COMMITS['original'])


def micro():
    root=W/'micro-v4';root.mkdir(exist_ok=True);(root/'a').write_text('before')
    for d in ['target','.git']:(root/d).mkdir(exist_ok=True)
    rows=[]
    for rep in range(3):
        for path in ['/dev/null','a']:
            for mode in MODES:
                out=measured(mode,root,f'micro-{mode}-{Path(path).name}-{rep}',[W/'probe-v3','micro-path',path,'20000'])
                obs=json.loads(out['stdout']);out.update(observation=obs,path=path,rep=rep)
                rows.append(out);dump(R/'micro-v4.json',rows)
                print(out['name'],obs,flush=True)
                if out['returncode'] or obs['failed']:raise RuntimeError('微基准调用失败')


def scales():
    rows=[];initial=[]
    for n in [3000,10000,100000]:
        root=W/f'scale-{n}-v4';state=W/'states-v4'/f'scale-{n}'
        shutil.rmtree(root,ignore_errors=True);shutil.rmtree(state,ignore_errors=True);root.mkdir()
        for i in range(n):
            p=root/f'd{i%100:03}'/f'f{i:06}';p.parent.mkdir(exist_ok=True);p.write_text(f'before-{i}\n'*32)
        o=Shadow(root,state);start=time.monotonic();info=o.begin();info['init_total_s']=time.monotonic()-start
        info.update(files=n,entries=info['entries'])
        info['du_allocated_bytes']=int(subprocess.check_output(['du','-s','-B1',str(o.shadow)]).split()[0])
        info['du_apparent_bytes']=int(subprocess.check_output(['du','-s','--apparent-size','-B1',str(o.shadow)]).split()[0])
        info['state_allocated_bytes']=int(subprocess.check_output(['du','-s','-B1',str(state)]).split()[0])
        info['filefrag']=subprocess.run(['filefrag','-v',str(root/'d000/f000000'),str(o.shadow/'d000/f000000')],capture_output=True,text=True).stdout
        initial.append(info);dump(R/'initial-v4.json',initial)
        for changed in [0,10,1000]:
            for rep in range(3):
                for i in range(changed):(root/f'd{i%100:03}'/f'f{i:06}').write_text(f'after-{changed}-{rep}-{i}\n'*32)
                info=o.begin();rows.append(dict(files=n,changed=changed,rep=rep,**info));dump(R/'sync-v4.json',rows)
                print('sync',n,changed,rep,info,flush=True)
        o.close()
    rows=[];root=W/'end-scan-v4';state=W/'states-v4/end-scan'
    shutil.rmtree(root,ignore_errors=True);shutil.rmtree(state,ignore_errors=True);root.mkdir()
    for i in range(4000):(root/f'f{i:04}').write_text(f'before-{i}\n'*32)
    o=Shadow(root,state);o.begin()
    for changed in [1,100,4000]:
        for rep in range(3):
            sync=o.begin()
            for i in range(changed):(o.shadow/f'f{i:04}').write_text(f'after-{changed}-{rep}-{i}\n'*32)
            fin=o.finish();com=o.commit()
            row=dict(changed=changed,rep=rep,sync=sync,scan_s=fin['scan_s'],epsilon_count=len(fin['epsilon']['paths']),**com)
            rows.append(row);dump(R/'settlement-v4.json',rows);print('settlement',row,flush=True)
            if com['outcome']!='Committed':raise RuntimeError('提交失败')
    o.close()


def incremental():
    git('checkout','--quiet','--detach',COMMITS['original'])
    shutil.rmtree(WORK/'target',ignore_errors=True);(WORK/'target/tmp').mkdir(parents=True)
    state=W/'states-v4/bench-lattice'
    env=environment();env['CARGO_INCREMENTAL']='1'
    out1=measured('S',WORK,'incremental-act1',[TC/'cargo','build','--offline','--workspace','-v'],env,state)
    o=Shadow(WORK,state);sync=o.begin()
    p='crates/lattice-cli/src/main.rs'
    out2=o.execute(['python3','-c',f"from pathlib import Path;p=Path({p!r});p.write_text(p.read_text()+'\\n// v4 单 crate 增量验证\\n')"],env)
    fin=o.finish();com=o.commit();o.close()
    out3=measured('S',WORK,'incremental-act3',[TC/'cargo','build','--offline','--workspace','-v'],env,state)
    # 宿主也执行一次，检查真实路径与影子 mtime 能否共享 target。
    start=time.monotonic();host=subprocess.run([str(TC/'cargo'),'build','--offline','--workspace','-v'],cwd=WORK,env=dict(os.environ,**env),capture_output=True,text=True)
    host_result=dict(returncode=host.returncode,wall_s=time.monotonic()-start,stdout=host.stdout,stderr=host.stderr)
    dump(R/'incremental-v4.json',dict(act1=out1,act2=dict(execution=out2,sync=sync,finish=fin,commit=com),act3=out3,host=host_result,
                                    compiled3=[line.strip() for line in out3['stderr'].splitlines() if line.strip().startswith('Compiling ')],
                                    compiled_host=[line.strip() for line in host.stderr.splitlines() if line.strip().startswith('Compiling ')]))
    git('restore','--',p)
    print('incremental',out1['returncode'],out2['returncode'],out3['returncode'],flush=True)


if __name__=='__main__':
    for task in sys.argv[1:] or ['full','checkout','micro','scales','incremental']:globals()[task]()
