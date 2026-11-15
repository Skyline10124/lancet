#!/usr/bin/env python3
"""私有 Git 的合并/丢弃矩阵与大索引成本。"""
import json,os,runpy,shutil,subprocess,sys,time
from pathlib import Path
HERE=Path(__file__).resolve().parent
v=runpy.run_path(str(HERE/'shadow-v7.py'));Shadow,dump=v['Shadow'],v['dump'];W=HERE/'.scratch';R=HERE/'results/v7'
BASE=['git','-c','user.name=bench','-c','user.email=bench@example.invalid','-c','commit.gpgsign=false','-c','gc.auto=0']
ENV=dict(os.environ,GIT_CONFIG_NOSYSTEM='1',GIT_CONFIG_GLOBAL='/dev/null',GIT_OPTIONAL_LOCKS='0')
def host(root,*args):
    p=subprocess.run([*BASE,*args],cwd=root,env=ENV,capture_output=True,text=True)
    if p.returncode:raise RuntimeError(p.stderr)
    return p.stdout

def inside(o,*args):
    p=o.execute([*BASE,*args],env=ENV)
    if p['returncode']:raise RuntimeError(p)
    return p

def new(name,n=10):
    root=W/(name+'-v7');state=W/'states-v7'/name;shutil.rmtree(root,ignore_errors=True);shutil.rmtree(state,ignore_errors=True);root.mkdir(parents=True)
    host(root,'init','--quiet','-b','main')
    for i in range(n):
        p=root/f'd{i%1000:03}'/f'f{i:06}';p.parent.mkdir(exist_ok=True);p.write_text(f'file {i}\n'*16)
    (root/'.git/info/exclude').write_text('/target/\n/hidden/\n');host(root,'add','.');host(root,'commit','--quiet','-m','bench: initial')
    return root,state

def statuses(s):return sorted(s.splitlines())
def matrix():
    rows=[];p='d000/f000000'
    for commit in [False,True]:
        for op in ['checkout','commit','stash','packed-refs']:
            for interference in ['none','after-end-index','during-act-refresh','during-act-stage']:
                for rep in range(3):
                    root,state=new(f'git-{commit}-{op}-{interference}-{rep}')
                    host(root,'checkout','--quiet','-b','other');(root/p).write_text('other');host(root,'add',p);host(root,'commit','--quiet','-m','bench: other');host(root,'checkout','--quiet','main');host(root,'pack-refs','--all','--prune')
                    if op=='stash':(root/p).write_text('dirty before act')
                    o=Shadow(root,state)
                    try:
                        a=o.begin()
                        if op=='checkout':out=inside(o,'checkout','--quiet','other')
                        elif op=='commit':(o.shadow/p).write_text('agent commit');inside(o,'add',p);out=inside(o,'commit','--quiet','-m','bench: agent')
                        elif op=='stash':out=inside(o,'stash','push','--quiet','-m','bench: stash')
                        else:inside(o,'branch','agent-new');out=inside(o,'pack-refs','--all','--prune')
                        if interference=='during-act-refresh':host(root,'status','--porcelain','-uno')
                        if interference=='during-act-stage':(root/'host-new').write_text('host staged');host(root,'add','host-new')
                        b=o.finish()
                        if interference=='after-end-index':host(root,'update-index','--assume-unchanged',p)
                        host_pre=host(root,'status','--porcelain','-uno');stage_pre=host(root,'diff','--cached','--name-only');index_pre=(root/'.git/index').read_bytes()
                        expected=inside(o,'status','--porcelain','-uno')['stdout'] if commit else host_pre
                        if commit and interference=='during-act-stage':expected+='A  host-new\n'
                        c=o.commit() if commit else o.discard();actual=host(root,'status','--porcelain','-uno');stage_post=host(root,'diff','--cached','--name-only')
                        rows.append(dict(commit=commit,operation=op,interference=interference,rep=rep,begin=a,finish=b,result=c,expected_status=expected,status=actual,status_consistent=statuses(actual)==statuses(expected),host_stage_preserved=set(stage_pre.splitlines())<=set(stage_post.splitlines()),index_bytes_preserved=index_pre==(root/'.git/index').read_bytes(),execution=out))
                    except Exception as e:rows.append(dict(commit=commit,operation=op,interference=interference,rep=rep,error=repr(e)))
                    finally:o.close()
                    dump(R/'git-matrix-v7.json',rows)
    print('git matrix',sum(x.get('status_consistent',False)and x.get('host_stage_preserved',False) for x in rows),len(rows),flush=True)

def extra():
    rows=[];p='d000/f000000';q='d001/f000001'
    for case in ['host-commit','host-stage-other','host-stage-same','hooks-config','stage-only','host-stage-before']:
        for rep in range(3):
            root,state=new('git-extra-'+case+str(rep))
            if case=='host-stage-before':(root/q).write_text('host staged');host(root,'add',q)
            o=Shadow(root,state)
            try:
                o.begin();(o.shadow/p).write_text('agent');inside(o,'add',p)
                if case!='stage-only':inside(o,'commit','--quiet','-m','bench: agent')
                config=(root/'.git/config').read_bytes();hooks={str(x.relative_to(root/'.git/hooks')):x.read_bytes()for x in (root/'.git/hooks').iterdir() if x.is_file()}
                if case=='host-commit':(root/q).write_text('host commit');host(root,'add',q);host(root,'commit','--quiet','-m','bench: host')
                if case.startswith('host-stage') and case!='host-stage-before':
                    target=p if case.endswith('same') else q;(root/target).write_text('host staged');host(root,'add',target)
                if case=='hooks-config':
                    inside(o,'config','core.fsmonitor','evil-command');(o.shadow/'.git/hooks/pre-commit').write_text('#!/bin/sh\necho unsafe\n')
                host_before=host(root,'status','--porcelain','-uno');stage=host(root,'ls-files','--stage');o.finish();c=o.commit();status=host(root,'status','--porcelain','-uno')
                expected=host_before if case in ['host-commit','host-stage-same'] else 'M  '+q+'\n' if case in ['host-stage-other','host-stage-before'] else ' M '+p+'\n' if case=='stage-only' else ''
                now_stage=host(root,'ls-files','--stage');preserved=all(line in now_stage.splitlines() for line in stage.splitlines() if '\t'+(p if case=='host-stage-same'else q) in line)
                rows.append(dict(case=case,rep=rep,result=c,status=status,expected_status=expected,status_consistent=status==expected,host_stage_preserved=preserved,config_preserved=(root/'.git/config').read_bytes()==config,hooks_preserved=hooks=={str(x.relative_to(root/'.git/hooks')):x.read_bytes()for x in (root/'.git/hooks').iterdir() if x.is_file()}))
            except Exception as e:rows.append(dict(case=case,rep=rep,error=repr(e)))
            finally:o.close()
            dump(R/'git-extra-v7.json',rows)
    print('git extra',sum(x.get('status_consistent',False)and x.get('host_stage_preserved',False)for x in rows),len(rows),flush=True)

def scale():
    rows=[];warm=[]
    for n in [10000,100000]:
        root,state=new(f'git-scale-{n}',n);o=Shadow(root,state)
        try:
            for k in range(3):
                start=time.monotonic();a=o.begin();warm.append(dict(files=n,rep=k,rpc_s=time.monotonic()-start,begin=a));o.finish(audit=False);o.commit();dump(R/'git-scale-warm-v7.json',warm)
            for rep in range(3):
                a=o.begin();(o.shadow/'d000/f000000').write_text('agent-'+str(rep));inside(o,'add','d000/f000000');inside(o,'commit','--quiet','-m','bench: scale');b=o.finish(audit=False);c=o.commit()
                status=host(root,'status','--porcelain','-uno');rows.append(dict(files=n,rep=rep,begin=a,finish=b,commit=c,status=status,index_bytes=(root/'.git/index').stat().st_size));dump(R/'git-scale-v7.json',rows)
                print('git scale',n,rep,c['outcome'],flush=True)
        finally:o.close()
        shutil.rmtree(root);shutil.rmtree(state)

if __name__=='__main__':
    for task in sys.argv[1:] or ['matrix','extra','scale']:globals()[task]()
