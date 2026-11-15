#!/usr/bin/env python3
"""三配置重测；另两配置引用第六轮；无并行重型验证，所有 Cargo 缓存在本轮 .scratch。"""
import json,os,runpy,shutil,subprocess,sys,time
from pathlib import Path
HERE=Path(__file__).resolve().parent;W=HERE/'.scratch';R=HERE/'results/v7';WORK=W/'lattice'
v5=runpy.run_path(str(HERE/'benchmarks-v5.py'));v7=runpy.run_path(str(HERE/'shadow-v7.py'))
Shadow=v7['Shadow'];dump=v7['dump'];run=v5['run'];run.__globals__['RESULTS']=R;git=v5['git'];C=v5['C'];TC=v5['TC'];environment=v5['environment'];MODES=['none','ns','S3'];nodes={}
def close():
    for o in nodes.values():o.close()
    nodes.clear()
def get(mode,root):
    key=(mode,str(root))
    if key not in nodes:nodes[key]=(Shadow if mode=='S3' else v5['s2']['Shadow'])(root,W/'states-v7'/('bench-'+mode+'-'+root.name))
    return nodes[key]
def measured(mode,root,name,command,env=None):
    # 不让另一配置的常驻监听污染基准。
    for key in list(nodes):
        if key[0]!=mode:nodes.pop(key).close()
    if mode in ['none','ns','B3']:
        out=run(mode,root,name+'-v7',command,limit=1200,env=env)
        return dict(name=name,mode=mode,returncode=out['returncode'],wall_s=out['stats']['wall_s'],process_wall_s=out['process_wall_s'],stdout=out['stdout'],stderr=out['stderr'],stats=out['stats'])
    o=get(mode,root);start=time.monotonic();a=o.begin();out=o.execute(command,env=env);b=o.finish(audit=False);c=o.commit() if out['returncode']==0 else o.discard()
    return dict(name=name,mode=mode,wall_s=time.monotonic()-start,**out,begin=a,finish=b,settlement=c)
def full():
    rows=[];git('checkout','--quiet','--detach',C['original'])
    for mode in MODES:
        if mode=='S3':
            o=get(mode,WORK);warm=o.execute([TC/'cargo','metadata','--offline','--format-version','1'],environment());dump(R/'build-cache-warm-v7.json',warm)
            if warm['returncode']:raise RuntimeError(warm['stderr'])
        for rep in range(3):
            target=get(mode,WORK).shadow/'target' if mode=='S3' else WORK/'target'
            shutil.rmtree(target,ignore_errors=True);(target/'tmp').mkdir(parents=True)
            row=measured(mode,WORK,f'full-{mode}-{rep}',[TC/'cargo','build','--offline','--workspace'],environment());row['rep']=rep;rows.append(row);dump(R/'builds-v7.json',rows)
            print('build',mode,rep,row['returncode'],row['wall_s'],flush=True)
            if row['returncode']:raise RuntimeError(row['stderr'][-2000:])
        close()
def checkout():
    rows=[]
    for mode in MODES:
        for scenario in ['A','B']:
            git('checkout','--quiet','--detach',C['a']);warm=measured(mode,WORK,f'warm-{mode}-{scenario}',['git','checkout','--quiet','--detach',C['b']],environment())
            if warm.get('settlement',{}).get('outcome','Committed') not in ['Committed','Unchanged']:raise RuntimeError(warm)
            for rep in range(3):
                if scenario=='A':git('checkout','--quiet','--detach',C['a'])
                for target in ['b'] if scenario=='A' else ['a','b']:
                    row=measured(mode,WORK,f'checkout-{scenario}-{mode}-{target}-{rep}',['git','checkout','--quiet','--detach',C[target]],environment());row.update(rep=rep,scenario=scenario,target=target,head=git('rev-parse','HEAD'),status=git('--no-optional-locks','status','--porcelain','-uno'));rows.append(row);dump(R/'checkouts-v7.json',rows)
                    print('checkout',scenario,mode,target,rep,row['wall_s'],row.get('settlement',{}).get('outcome'),flush=True)
                    if row['returncode'] or row['head']!=C[target] or row['status']:raise RuntimeError(row)
        close()
    git('checkout','--quiet','--detach',C['original'])
def fresh(name):
    root=W/(name+'-v7');state=W/'states-v7'/name;shutil.rmtree(root,ignore_errors=True);shutil.rmtree(state,ignore_errors=True);root.mkdir(parents=True);return root,state

def micro():
    rows=[];root,state=fresh('micro');(root/'a').write_text('before');(root/'target').mkdir();(root/'.git').mkdir()
    for mode in MODES:
        for path in ['/dev/null','a']:
            for rep in range(3):
                row=measured(mode,root,f'micro-{mode}-{Path(path).name}-{rep}',[W/'probe-v3','micro-path',path,'20000']);row.update(path=path,rep=rep,observation=json.loads(row['stdout']));rows.append(row);dump(R/'micro-v7.json',rows)
        close()
    print('micro done',flush=True)
def scales():
    def saved(name):
        p=R/(name+'-v7.json')
        return json.loads(p.read_text()) if os.environ.get('S3_RESUME')=='1' and p.exists() else []
    initial=saved('initial');fixed=saved('fixed');scans=saved('scan')
    for n in [3000,10000,100000]:
        for rep in range(3):
            complete=any(x['files']==n and x['rep']==rep for x in initial) and sum(x['files']==n and x['rep']==rep for x in scans)==3 and sum(x['files']==n and x['rep']==rep and x['mode']=='S3' for x in fixed)==3
            if complete:continue
            initial[:]=[x for x in initial if not (x['files']==n and x['rep']==rep)]
            scans[:]=[x for x in scans if not (x['files']==n and x['rep']==rep)]
            fixed[:]=[x for x in fixed if not (x['files']==n and x['rep']==rep and x['mode']=='S3')]
            root,state=fresh(f'scale-{n}-{rep}')
            for i in range(n):
                p=root/f'd{i%1000:03}'/f'f{i:06}';p.parent.mkdir(exist_ok=True);p.write_text(f'before-{i}\n'*32)
            t=time.monotonic();o=Shadow(root,state);initial.append(dict(files=n,rep=rep,process_init_s=time.monotonic()-t,**o.initial));dump(R/'initial-v7.json',initial)
            for nth in [8]:
                o.call('env',values={'S3_THREADS':str(nth)})
                for k in range(3):
                    out=o.call('sync');scans.append(dict(files=n,rep=rep,threads=nth,runrep=k,**out));dump(R/'scan-v7.json',scans)
            for k in range(3):
                t=time.monotonic();a=o.begin();b=o.finish(audit=False);c=o.commit();fixed.append(dict(mode='S3',files=n,rep=rep,runrep=k,wall_s=time.monotonic()-t,begin=a,finish=b,commit=c));dump(R/'fixed-v7.json',fixed)
            o.close()
            if rep==0:
                for mode in ['none','ns']:
                    for k in range(3):
                        row=measured(mode,root,f'fixed-{n}-{mode}-{k}',['true']);row.update(files=n,rep=k);fixed.append(row);dump(R/'fixed-v7.json',fixed)
                    close()
            shutil.rmtree(root);shutil.rmtree(state);print('scale',n,rep,flush=True)

def merges():
    rows=[];root,state=fresh('merge')
    for i in range(4000):(root/f'f{i:04}').write_text(f'before-{i}\n'*32)
    for mode in ['S3']:
        o=get(mode,root)
        for n in [1,100,4000]:
            for rep in range(3):
                a=o.begin()
                for i in range(n):(o.shadow/f'f{i:04}').write_text(f'after-{mode}-{n}-{rep}-{i}\n'*32)
                b=o.finish(audit=False);c=o.commit();rows.append(dict(mode=mode,files=n,rep=rep,begin=a,finish=b,commit=c));dump(R/'merges-v7.json',rows)
                print('merge',mode,n,rep,c['outcome'],c.get('commit_s'),flush=True)
        close()
def renames():
    rows=[]
    for n in [1000,10000]:
        root,state=fresh(f'rename-{n}');(root/'dir').mkdir()
        for i in range(n):(root/'dir'/f'f{i}').write_text(str(i))
        for mode in MODES:
            for rep in range(3):
                if (root/'moved').exists():(root/'moved').rename(root/'dir')
                if mode in ['S2','S3']:
                    o=get(mode,root);o.begin();o.finish(audit=False);o.commit()
                row=measured(mode,root,f'rename-{n}-{mode}-{rep}',['mv','dir','moved']);row.update(files=n,rep=rep);rows.append(row);dump(R/'renames-v7.json',rows)
                print('rename',n,mode,rep,row['wall_s'],flush=True)
            close()
if __name__=='__main__':
    try:
        for task in sys.argv[1:] or ['full','checkout','micro','scales','merges','renames']:globals()[task]()
    finally:close()

    if 'full' in sys.argv[1:]:
        subprocess.run([sys.executable,str(HERE/'incremental-v7.py')],check=True)
