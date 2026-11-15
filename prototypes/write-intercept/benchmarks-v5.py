#!/usr/bin/env python3
"""五配置顺序基准。S/B3 直接复用前轮实现，S2 为常驻 Rust 节点。"""
import json,os,runpy,shutil,subprocess,sys,time
from pathlib import Path
HERE=Path(__file__).resolve().parent
v4=runpy.run_path(str(HERE/'benchmarks-v4.py'));s2=runpy.run_path(str(HERE/'shadow-v5.py'))
W=HERE/'.scratch';R=HERE/'results/v5';WORK=W/'lattice';dump=v4['dump'];git=v4['git'];environment=v4['environment'];TC=v4['TC'];C=v4['COMMITS'];run=v4['run'];run.__globals__['RESULTS']=R
MODES=['none','ns','B3','S','S2'];nodes={}
def get(mode,root,state=None):
    key=(mode,str(root),str(state))
    if key not in nodes:nodes[key]=(s2['Shadow'] if mode=='S2' else v4['Shadow'])(root,state or W/'states-v5'/f'bench-{mode}-{root.name}')
    return nodes[key]
def close():
    for o in nodes.values():o.close()
    nodes.clear()
def measured(mode,root,name,command,env=None,state=None):
    if mode!='S2':
        for key in [key for key in nodes if key[0]=='S2']:
            nodes.pop(key).close()
    if mode not in ['S','S2']:
        out=run(mode,root,name+'-v5',command,limit=1200,env=env)
        return dict(name=name,mode=mode,returncode=out['returncode'],wall_s=out['stats']['wall_s'],stats=out['stats'],stdout=out['stdout'],stderr=out['stderr'],process_wall_s=out['process_wall_s'])
    o=get(mode,root,state);start=time.monotonic();a=o.begin();out=o.execute(command,env=env);b=o.finish(audit=False) if mode=='S2' else o.finish();c=o.commit() if out['returncode']==0 else o.discard();wall=time.monotonic()-start
    if c['outcome'] not in ['Committed','Unchanged']:raise RuntimeError(c)
    return dict(name=name,mode=mode,wall_s=wall,**out,**a,scan_s=b['scan_s'],changed=len(b['epsilon']['paths']),settlement=c)
def full():
    rows=[];git('checkout','--quiet','--detach',C['original'])
    for mode in ['S']:
        o=get(mode,WORK)
        if mode=='S':o.begin();o.finish();o.commit()
    for rep in range(3):
        for mode in MODES[rep%len(MODES):]+MODES[:rep%len(MODES)]:
            shutil.rmtree(WORK/'target',ignore_errors=True);(WORK/'target/tmp').mkdir(parents=True)
            out=measured(mode,WORK,f'full-{mode}-{rep}',[TC/'cargo','build','--offline','--workspace'],environment());out['rep']=rep
            rows.append(out);dump(R/'builds-v5.json',rows);print(out['name'],out['returncode'],out['wall_s'],flush=True)
            if out['returncode']:raise RuntimeError(out['stderr'])
def s2_remeasure():
    global MODES
    old=MODES;MODES=['S2'];previous=json.loads((R/'builds-v5.json').read_text());full();new=json.loads((R/'builds-v5.json').read_text());dump(R/'builds-before-fix-v5.json',previous);dump(R/'builds-v5.json',[x for x in previous if x['mode']!='S2']+new)
    previous=json.loads((R/'checkouts-v5.json').read_text());dump(R/'checkouts-before-fix-v5.json',previous);MODES=old;checkout();MODES=['S2']
    previous=json.loads((R/'micro-v5.json').read_text());micro();new=json.loads((R/'micro-v5.json').read_text());dump(R/'micro-before-fix-v5.json',previous);dump(R/'micro-v5.json',[x for x in previous if x['mode']!='S2']+new);MODES=old
def checkout():
    rows=[]
    for scenario in ['A','B']:
        for mode in MODES:
            git('checkout','--quiet','--detach',C['a'])
            if mode in ['S','S2']:
                o=get(mode,WORK);o.begin();o.finish(audit=False) if mode=='S2' else o.finish();o.commit()
            measured(mode,WORK,'warm-checkout',['git','checkout','--quiet','--detach',C['b']],environment())
            for rep in range(3):
                if scenario=='A':git('checkout','--quiet','--detach',C['a'])
                for target in (['b'] if scenario=='A' else ['a','b']):
                    out=measured(mode,WORK,f'checkout-{scenario}-{mode}-{target}-{rep}',['git','checkout','--quiet','--detach',C[target]],environment());out.update(rep=rep,scenario=scenario,target=target)
                    out['head']=git('rev-parse','HEAD');out['status']=subprocess.check_output(['git','--no-optional-locks','-C',str(WORK),'status','--porcelain','-uno'],text=True)
                    rows.append(out);dump(R/'checkouts-v5.json',rows);print(out['name'],out['wall_s'],flush=True)
                    if out['returncode'] or out['head']!=C[target] or out['status']:raise RuntimeError('检出结果不一致')
    git('checkout','--quiet','--detach',C['original'])
def micro():
    root=W/'micro-v5';root.mkdir(exist_ok=True);(root/'a').write_text('before');[(root/d).mkdir(exist_ok=True)for d in ['target','.git']];rows=[]
    for rep in range(3):
        for path in ['/dev/null','a']:
            for mode in MODES:
                out=measured(mode,root,f'micro-{mode}-{Path(path).name}-{rep}',[W/'probe-v3','micro-path',path,'20000']);out.update(observation=json.loads(out['stdout']),path=path,rep=rep);rows.append(out);dump(R/'micro-v5.json',rows)
                if out['returncode'] or out['observation']['failed']:raise RuntimeError(out)
    print('micro complete',flush=True)
def fresh(name):
    root=W/(name+'-v5');state=W/'states-v5'/name
    shutil.rmtree(root,ignore_errors=True);shutil.rmtree(state,ignore_errors=True);root.mkdir(parents=True);return root,state
def scales():
    initial=[];fixed=[];sync=[]
    for n in [3000,10000,100000]:
        for rep in range(3):
            root,state=fresh(f'initial-{n}-{rep}')
            for i in range(n):
                p=root/f'd{i%100:03}'/f'f{i:06}';p.parent.mkdir(exist_ok=True);p.write_text(f'before-{i}\n'*32)
            start=time.monotonic();o=s2['Shadow'](root,state);row=dict(files=n,rep=rep,process_init_s=time.monotonic()-start,**o.initial,stats=o.call('stats'))
            for key,flags,p in [('allocated',[],o.shadow),('apparent',['--apparent-size'],o.shadow),('state_allocated',[],state)]:row[key]=int(subprocess.check_output(['du','-s','-B1',*flags,str(p)]).split()[0])
            initial.append(row);dump(R/'initial-v5.json',initial)
            for runrep in range(3):
                index_before=(state/'index-v5.jsonl').stat().st_size;start=time.monotonic();a=o.begin();b=o.finish(audit=False);c=o.commit();fixed.append(dict(mode='S2',files=n,rep=rep,runrep=runrep,wall_s=time.monotonic()-start,begin=a,finish=b,commit=c,index_bytes_before=index_before,index_bytes_after=(state/'index-v5.jsonl').stat().st_size));dump(R/'fixed-v5.json',fixed)
            if rep==0:
                for runrep in range(3):
                    start=time.monotonic();a=o.begin(full=True);b=o.finish(audit=False,overflow=True);c=o.commit();fixed.append(dict(mode='S2-full',files=n,rep=runrep,wall_s=time.monotonic()-start,begin=a,finish=b,commit=c));dump(R/'fixed-v5.json',fixed)
                for mode in ['none','ns','B3','S']:
                    if mode=='S':so=get(mode,root);so.begin();so.finish();so.commit()
                    for runrep in range(3):
                        row=measured(mode,root,f'fixed-{n}-{mode}-{runrep}',['true']);row.update(files=n,rep=runrep);fixed.append(row);dump(R/'fixed-v5.json',fixed)
                for changed in [10,1000]:
                    for runrep in range(3):
                        for i in range(changed):(root/f'd{i%100:03}'/f'f{i:06}').write_text(f'after-{changed}-{runrep}-{i}\n'*32)
                        a=o.begin();b=o.finish(audit=True);c=o.commit();sync.append(dict(files=n,changed=changed,rep=runrep,begin=a,finish=b,commit=c));dump(R/'sync-v5.json',sync)
            o.close();close();print('initial/fixed',n,rep,row.get('init_s'),flush=True)
            shutil.rmtree(root);shutil.rmtree(state)
    rows=[];root,state=fresh('settlement')
    for i in range(4000):(root/f'f{i:04}').write_text(f'before-{i}\n'*32)
    for mode in ['S','S2']:
        o=get(mode,root,state.with_name('settlement-'+mode));o.begin();o.finish(audit=False) if mode=='S2' else o.finish();o.commit()
        for changed in [1,100,4000]:
            for rep in range(3):
                a=o.begin()
                for i in range(changed):(o.shadow/f'f{i:04}').write_text(f'after-{mode}-{changed}-{rep}-{i}\n'*32)
                b=o.finish(audit=False) if mode=='S2' else o.finish();c=o.commit();row=dict(mode=mode,changed=changed,rep=rep,begin=a,scan_s=b['scan_s'],epsilon_count=len(b['epsilon']['paths']),**c);rows.append(row);dump(R/'settlement-v5.json',rows)
                if c['outcome']!='Committed':raise RuntimeError(c)
    close()
def renames():
    rows=[]
    for n in [1000,10000]:
        root,state=fresh(f'rename-{n}');(root/'dir').mkdir();[(root/'dir'/f'f{i}').write_text(str(i))for i in range(n)]
        for mode in MODES:
            for rep in range(3):
                if (root/'moved').exists():(root/'moved').rename(root/'dir')
                if mode in ['S','S2']:
                    o=get(mode,root);o.begin();o.finish(audit=False) if mode=='S2' else o.finish();o.commit()
                out=measured(mode,root,f'rename-{n}-{mode}-{rep}',['mv','dir','moved']);out.update(files=n,rep=rep);rows.append(out);dump(R/'renames-v5.json',rows)
                print(out['name'],out['wall_s'],flush=True)
                if out['returncode']:raise RuntimeError(out)
        close()
def incremental():
    git('checkout','--quiet','--detach',C['original']);shutil.rmtree(WORK/'target',ignore_errors=True);(WORK/'target/tmp').mkdir(parents=True)
    env=environment();env['CARGO_INCREMENTAL']='1';state=W/'states-v5/incremental'
    out1=measured('S2',WORK,'incremental-act1',[TC/'cargo','build','--offline','--workspace','-v'],env,state)
    o=get('S2',WORK,state);a=o.begin();p='crates/lattice-cli/src/main.rs';out2=o.execute(['python3','-c',f"from pathlib import Path;p=Path({p!r});p.write_text(p.read_text()+'\\n// v5 单 crate 增量验证\\n')"],env);b=o.finish();c=o.commit()
    out3=measured('S2',WORK,'incremental-act3',[TC/'cargo','build','--offline','--workspace','-v'],env,state)
    start=time.monotonic();out=subprocess.run([str(TC/'cargo'),'build','--offline','--workspace','-v'],cwd=WORK,env=dict(os.environ,**env),capture_output=True,text=True);host=dict(returncode=out.returncode,stdout=out.stdout,stderr=out.stderr,wall_s=time.monotonic()-start)
    dump(R/'incremental-v5.json',dict(act1=out1,act2=dict(begin=a,execution=out2,finish=b,commit=c),act3=out3,host=host,compiled3=[l.strip()for l in out3['stderr'].splitlines()if l.strip().startswith('Compiling ')],compiled_host=[l.strip()for l in out.stderr.splitlines()if l.strip().startswith('Compiling ')]))
    git('restore','--',p);print('incremental',out1['returncode'],out2['returncode'],out3['returncode'],out.returncode,flush=True)

def baseline_initial():
    rows=[]
    for n in [3000,10000,100000]:
        for rep in range(3):
            root,state=fresh(f'initial-S-{n}-{rep}')
            for i in range(n):
                p=root/f'd{i%100:03}'/f'f{i:06}';p.parent.mkdir(exist_ok=True);p.write_text(f'before-{i}\n'*32)
            o=v4['Shadow'](root,state);start=time.monotonic();a=o.begin();row=dict(mode='S',files=n,rep=rep,init_s=time.monotonic()-start,**a)
            for key,flags,p in [('allocated',[],o.shadow),('apparent',['--apparent-size'],o.shadow),('state_allocated',[],state)]:row[key]=int(subprocess.check_output(['du','-s','-B1',*flags,str(p)]).split()[0])
            rows.append(row);dump(R/'initial-s-v5.json',rows);o.close();shutil.rmtree(root);shutil.rmtree(state);print('S initial',n,rep,row['init_s'],flush=True)

if __name__=='__main__':
    try:
        for task in sys.argv[1:] or ['full','checkout','micro','scales','renames']:globals()[task]()
    finally:close()
