#!/usr/bin/env python3
"""共享 Git 撤销与 1 万/10 万文件索引往返，包含私有索引的语义反例。"""
import json,os,runpy,shutil,subprocess,sys,time
from pathlib import Path
HERE=Path(__file__).resolve().parent
v=runpy.run_path(str(HERE/'shadow-v5.py'));Shadow,dump=v['Shadow'],v['dump'];W=HERE/'.scratch';R=HERE/'results/v5'
BASE=['git','-c','user.name=bench','-c','user.email=bench@example.invalid','-c','commit.gpgsign=false','-c','gc.auto=0']
ENV=dict(os.environ,GIT_CONFIG_NOSYSTEM='1',GIT_CONFIG_GLOBAL='/dev/null')
def host(root,args,env=None,trace=None):
    if trace:trace.unlink(missing_ok=True)
    start=time.monotonic();p=subprocess.run([*BASE,*args],cwd=root,env=dict(ENV,**(env or {}),**({'GIT_TRACE2_EVENT':str(trace)} if trace else {})),capture_output=True,text=True)
    return dict(returncode=p.returncode,stdout=p.stdout,stderr=p.stderr,wall_s=time.monotonic()-start,trace=trace_stats(trace))
def trace_stats(p):
    data=[json.loads(x)for x in p.read_text().splitlines()] if p and p.exists() else []
    return [x for x in data if x.get('event')=='data' and ('refresh/' in x.get('key','')or'preload/'in x.get('key',''))]
def inside(o,args,env=None):
    trace=o.real/'target/trace-inside-v5.jsonl';trace.unlink(missing_ok=True);out=o.execute([*BASE,*args],env=dict(ENV,**(env or {}),GIT_TRACE2_EVENT=str(trace)));out['trace']=trace_stats(trace);return out

def new(name,n=10):
    root=W/(name+'-v5');state=W/'states-v5'/name;shutil.rmtree(root,ignore_errors=True);shutil.rmtree(state,ignore_errors=True);root.mkdir(parents=True);(root/'target').mkdir()
    host(root,['init','--quiet','-b','main'])
    for i in range(n):
        p=root/f'd{i%100:03}'/f'f{i:06}';p.parent.mkdir(exist_ok=True);p.write_text(f'file {i}\n'*16)
    (root/'.git/info/exclude').write_text('/target/\n');host(root,['add','.']);out=host(root,['commit','--quiet','-m','bench: initial'])
    if out['returncode']:raise RuntimeError(out)
    return root,state

def discard():
    rows=[]
    for op in ['checkout','commit','stash','packed-refs']:
        for interference in ['none','after-end-index','during-act-refresh','during-act-stage']:
            for rep in range(3):
                root,state=new(f'git-discard-{op}-{interference}-{rep}');p='d000/f000000'
                host(root,['checkout','--quiet','-b','other']);(root/p).write_text('other commit');host(root,['add',p]);host(root,['commit','--quiet','-m','bench: other']);host(root,['checkout','--quiet','main']);host(root,['pack-refs','--all','--prune'])
                if op=='stash':(root/p).write_text('dirty before act')
                before_status=host(root,['status','--porcelain','-uno'])['stdout'];before_head=host(root,['rev-parse','HEAD'])['stdout'];before_index=(root/'.git/index').read_bytes()
                o=Shadow(root,state);begin=o.begin()
                if op=='checkout':out=inside(o,['checkout','--quiet','other'])
                elif op=='commit':(o.shadow/p).write_text('sandbox commit');inside(o,['add',p]);out=inside(o,['commit','--quiet','-m','bench: sandbox'])
                elif op=='stash':out=inside(o,['stash','push','--quiet','-m','bench: stash'])
                else:inside(o,['branch','sandbox-new']);out=inside(o,['pack-refs','--all','--prune'])
                interference_out=None;external_index=None
                if interference=='during-act-refresh':interference_out=host(root,['status','--porcelain','-uno']);external_index=(root/'.git/index').read_bytes()
                if interference=='during-act-stage':(root/'host-new').write_text('host staged');interference_out=host(root,['add','host-new']);external_index=(root/'.git/index').read_bytes()
                fin=o.finish();end_index=(root/'.git/index').read_bytes()
                if interference=='after-end-index':interference_out=host(root,['status','--porcelain','-uno']);external_index=(root/'.git/index').read_bytes();
                # status 可能未改变字节；再用合法 Git cacheinfo 保证结束后索引修改。
                if interference=='after-end-index' and external_index==end_index:
                    host(root,['update-index','--assume-unchanged',p]);external_index=(root/'.git/index').read_bytes()
                result=o.discard();preserved=(root/'.git/index').read_bytes()==external_index if external_index else None;restored_index=(root/'.git/index').read_bytes()==before_index
                final=host(root,['status','--porcelain','-uno']);head=host(root,['rev-parse','HEAD'])['stdout'];staged=host(root,['diff','--cached','--name-only'])['stdout']
                row=dict(operation=op,interference=interference,rep=rep,begin=begin,execution=out,finish=fin,result=result,head_restored=head==before_head,status_before=before_status,status_after=final['stdout'],status_consistent=before_status==final['stdout'],index_restored=restored_index,external_index_preserved=preserved,host_staging_preserved='host-new' in staged,interference_result=interference_out,stash=host(root,['stash','list'])['stdout'])
                rows.append(row);dump(R/'git-discard-v5.json',rows);o.close()
                print('git-discard',op,interference,rep,result['outcome'],row['status_consistent'],flush=True)

def roundtrip():
    rows=[];semantics=[]
    for n in [10000,100000]:
        root,state=new(f'git-index-{n}',n);o=Shadow(root,state);begin=o.begin();o.finish(audit=False);o.commit()
        private=root/'.git/private-index-v5';subprocess.run(['cp','--reflink=always',str(root/'.git/index'),str(private)],check=True)
        for mode,env in [('shared',{}),('private',{'GIT_INDEX_FILE':str(private)})]:
            warm_inside=inside(o,['status','--porcelain','-uno'],env);warm_host=host(root,['status','--porcelain','-uno'])
            for rep in range(3):
                a=inside(o,['status','--porcelain','-uno'],env);b=host(root,['status','--porcelain','-uno'],trace=root/'target/trace-host-v5.jsonl');c=inside(o,['status','--porcelain','-uno'],env)
                rows.append(dict(files=n,mode=mode,rep=rep,inside=a,host=b,inside_after_host=c,index_bytes=private.stat().st_size));dump(R/'git-index-v5.json',rows)
                if any(x['returncode'] or x['stdout'] for x in [a,b,c]):raise RuntimeError('干净索引往返不一致')
            print('index',n,mode,flush=True)
        # 私有索引看不到宿主 staged 变更；不能把缓存优化视为无语义成本。
        (root/'d000/f000000').write_text('host staged change');host(root,['add','d000/f000000']);h=host(root,['diff','--cached','--name-only']);i=inside(o,['diff','--cached','--name-only'],{'GIT_INDEX_FILE':str(private)});semantics.append(dict(files=n,host=h,private=i,staging_equal=h['stdout']==i['stdout']))
        o.close();dump(R/'git-private-semantics-v5.json',semantics)
def recovery():
    rows=[]
    for stage in ['before-log','after-ready','after-complete']:
        for rep in range(3):
            root,state=new(f'git-crash-{stage}-{rep}');before=host(root,['rev-parse','HEAD'])['stdout'];o=Shadow(root,state);o.begin();(o.shadow/'d000/f000000').write_text('sandbox commit');inside(o,['add','.']);out=inside(o,['commit','--quiet','-m','bench: interrupted']);o.finish();o.call('env',values={'S2_KILL_AT':stage})
            try:o.commit()
            except RuntimeError:o.proc.wait()
            o.close();o=Shadow(root,state);rows.append(dict(stage=stage,rep=rep,restart=o.initial,head_restored=host(root,['rev-parse','HEAD'])['stdout']==before,status=host(root,['status','--porcelain','-uno']),execution=out));o.close();dump(R/'git-crashes-v5.json',rows)

def ref_conflicts():
    rows=[]
    for kind in ['HEAD','ref','packed-refs']:
        for rep in range(3):
            root,state=new(f'git-conflict-{kind}-{rep}');before=host(root,['rev-parse','HEAD'])['stdout'].strip();o=Shadow(root,state);o.begin();(o.shadow/'d000/f000000').write_text('sandbox commit');inside(o,['add','.']);inside(o,['commit','--quiet','-m','bench: end conflict']);o.finish()
            if kind=='HEAD':out=host(root,['symbolic-ref','HEAD','refs/heads/external'])
            elif kind=='ref':out=host(root,['update-ref','refs/heads/main',before])
            else:host(root,['branch','external']);out=host(root,['pack-refs','--all','--prune'])
            p=root/'.git'/({'HEAD':'HEAD','ref':'refs/heads/main','packed-refs':'packed-refs'}[kind]);external=p.read_bytes();result=o.discard();rows.append(dict(kind=kind,rep=rep,result=result,external_preserved=p.read_bytes()==external,host=out));o.close();dump(R/'git-ref-conflicts-v5.json',rows)

def minimal_risk():
    root,state=new('git-minimal-only');p=root/'d000/f000000';stamp=int(time.time())-60;os.utime(p,(stamp,stamp));host(root,['status','--porcelain','-uno']);saved=(root/'.git/index').read_bytes();old=p.read_bytes();time.sleep(1.1)
    p.write_bytes(b'X'+old[1:]);os.utime(p,(stamp,stamp));minimal=host(root,['status','--porcelain','-uno'],{'GIT_CONFIG_COUNT':'1','GIT_CONFIG_KEY_0':'core.checkStat','GIT_CONFIG_VALUE_0':'minimal'});(root/'.git/index').write_bytes(saved);normal=host(root,['status','--porcelain','-uno'])
    dump(R/'git-minimal-only-risk-v5.json',dict(minimal=minimal,default_independent=normal,bytes_changed=p.read_bytes()!=old))

def fixed_git():
    rows=[]
    for n in [10000,100000]:
        root=W/f'git-index-{n}-v5';state=W/'states-v5'/f'git-index-{n}';o=Shadow(root,state)
        for rep in range(3):
            start=time.monotonic();a=o.begin();b=o.finish(audit=False);c=o.commit();rows.append(dict(files=n,rep=rep,wall_s=time.monotonic()-start,begin=a,finish=b,commit=c,index_bytes=(root/'.git/index').stat().st_size));dump(R/'git-fixed-v5.json',rows)
        o.close()

if __name__=='__main__':
    for task in sys.argv[1:] or ['discard','roundtrip','recovery','ref_conflicts']:globals()[task]()
