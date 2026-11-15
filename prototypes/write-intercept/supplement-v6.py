#!/usr/bin/env python3
"""原型边界：外部别名、只读目录、Git 崩溃、revert 与连续 act 公平性。"""
import json,os,runpy,shutil,subprocess,sys,time
from pathlib import Path
HERE=Path(__file__).resolve().parent;v=runpy.run_path(str(HERE/'research-v6.py'));globals().update({k:v[k]for k in ['Shadow','setup','oracle','difference','dump','W','R','BIN']})
g=runpy.run_path(str(HERE/'verify-git-v6.py'))
def aliases():
    rows=[]
    for readonly in [False,True]:
        root,state=setup('alias-ro-'+str(readonly));cache=W/('writable-cache-'+str(readonly)+'-v6');shutil.rmtree(cache,ignore_errors=True);cache.mkdir();os.link(root/'a',cache/'alias')
        o=Shadow(root,state);detected=o.initial['external_aliases'];o.close()
        script='mount --bind "$1" "$1"; if [ "$3" = yes ]; then mount -o remount,bind,ro "$1"; fi; printf host-alias > "$2/alias"'
        out=subprocess.run(['unshare','--user','--map-root-user','--mount','--','sh','-c',script,'sh',str(root),str(cache),'yes'if readonly else'no'],capture_output=True,text=True)
        rows.append(dict(readonly=readonly,detected=detected,returncode=out.returncode,stderr=out.stderr,host_changed=(root/'a').read_text()=='host-alias'))
    dump(R/'external-aliases-v6.json',rows)

def git_crashes():
    rows=[]
    for stage in ['before-log','after-ready','apply-1','apply-2','before-post-sync','after-complete']:
        for rep in range(3):
            root,state=g['new'](f'git-crash-{stage}-{rep}');o=Shadow(root,state);before=oracle(root);head=g['host'](root,'rev-parse','HEAD');o.begin();(o.shadow/'d000/f000000').write_text('agent commit');g['inside'](o,'add','.');g['inside'](o,'commit','--quiet','-m','bench: crash');desired=oracle(o.shadow);newhead=g['inside'](o,'rev-parse','HEAD')['stdout'];o.finish();o.call('env',values={'S3_KILL_AT':stage})
            try:o.commit();killed=False
            except RuntimeError:o.proc.wait();killed=o.proc.returncode==-9
            o.close();o=Shadow(root,state);actual=oracle(root);h=g['host'](root,'rev-parse','HEAD');status=g['host'](root,'status','--porcelain','-uno');locks=[str(p.relative_to(root))for p in (root/'.git').rglob('*.lock')];row=dict(stage=stage,rep=rep,killed=killed,equals_before=actual==before and h==head,equals_after=actual==desired and h==newhead,status=status,locks=locks,recovery=o.initial['recovery']);row['pass']=killed and (row['equals_before']or row['equals_after'])and not locks and not status;rows.append(row);o.close();dump(R/'git-crashes-v6.json',rows)
    print('git crashes',sum(x['pass']for x in rows),len(rows),flush=True)

def reverts():
    rows=[]
    root,state=setup('revert-items');o=Shadow(root,state);old={p:(root/p).read_bytes()for p in ['a','b']};o.begin();(o.shadow/'a').write_text('agent-a');(o.shadow/'b').write_text('agent-b');o.finish();c=o.commit();o.close();(root/'a').write_text('host-a')
    proc=subprocess.run([str(BIN),'recover',c['transaction'],'--revert'],capture_output=True,text=True);rows.append(dict(case='per-item',result=json.loads(proc.stdout),a_preserved=(root/'a').read_text()=='host-a',b_restored=(root/'b').read_bytes()==old['b']))
    # 复用第五轮目录改名+硬链接的 4 种组合，入口与输出均重定向第六轮。
    oldmod=runpy.run_path(str(HERE/'research-v5.py'));f=oldmod['rename_links'];f.__globals__.update(Shadow=Shadow,setup=setup,oracle=oracle,difference=difference,dump=lambda p,v:dump(R/'rename-links-v6.json',v),BIN=BIN);f()
    dump(R/'revert-v6.json',rows)

def starvation():
    import mmap
    root,state=setup('scan-starvation');o=Shadow(root,state);f=open(root/'a','r+b');mm=mmap.mmap(f.fileno(),0);f.close();o.begin();o.finish();o.commit();mm[0:1]=b'X';start=time.monotonic();count=0
    while (o.shadow/'a').read_bytes()!=(root/'a').read_bytes() and time.monotonic()-start<2:
        o.begin();o.finish(audit=False);o.commit();count+=1
    row=dict(acts=count,delay_s=time.monotonic()-start,exact=not difference(oracle(root),oracle(o.shadow)));mm.close();o.close();dump(R/'scan-fairness-v6.json',row)


def model():
    root,state=g['new']('model');o=Shadow(root,state);o.begin()
    code="""import os,json
from pathlib import Path
Path('target/retained').write_text('agent cache')
try:
 Path('.git/host-objects-v6/should-not-exist').write_text('blocked');error=0
except OSError as e:error=e.errno
print(json.dumps(dict(object_write_errno=error,ordinary_ino=Path('d000/f000000').stat().st_ino,git_config_ino=Path('.git/config').stat().st_ino)))
"""
    out=o.execute(['python3','-c',code]);obs=json.loads(out['stdout']);o.finish();settlement=o.discard();row=dict(execution=out,observation=obs,discard=settlement,host_target_unchanged=not (root/'target/retained').exists(),host_object_unchanged=not (root/'.git/objects/should-not-exist').exists(),ordinary_isolated=obs['ordinary_ino']!=(root/'d000/f000000').stat().st_ino,git_private=obs['git_config_ino']!=(root/'.git/config').stat().st_ino);o.close();o=Shadow(root,state);row['cache_retained_across_restart']=(o.shadow/'target/retained').read_text()=='agent cache';o.close();dump(R/'workspace-model-v6.json',row)


def host_git_sync():
    rows=[]
    for rep in range(3):
        root,state=g['new']('host-git-sync-'+str(rep));g['host'](root,'checkout','--quiet','-b','other');(root/'d000/f000000').write_text('other');g['host'](root,'add','.');g['host'](root,'commit','--quiet','-m','bench: other');g['host'](root,'checkout','--quiet','main');o=Shadow(root,state)
        try:
            for branch in ['other','main']:
                g['host'](root,'checkout','--quiet',branch);sync=o.call('sync');begin=o.begin();equal=not difference(oracle(root),oracle(o.shadow));head_equal=g['host'](root,'rev-parse','HEAD')==g['inside'](o,'rev-parse','HEAD')['stdout'];o.finish();o.discard();rows.append(dict(rep=rep,branch=branch,sync=sync,begin=begin,exact=equal,head_equal=head_equal))
        finally:o.close()
    dump(R/'host-git-sync-v6.json',rows)

if __name__=='__main__':
    for task in sys.argv[1:] or ['aliases','git_crashes','reverts','starvation','model','host_git_sync']:globals()[task]()
