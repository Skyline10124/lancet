#!/usr/bin/env python3
"""独立 oracle、随机负载、遗漏反例、提交窗口与真实 SIGKILL。"""
import ctypes,json,mmap,os,random,runpy,shutil,signal,subprocess,sys,time
from pathlib import Path
HERE=Path(__file__).resolve().parent
v=runpy.run_path(str(HERE/'verify-v5.py'));Shadow,setup,oracle,difference,dump=[v[k]for k in ['Shadow','setup','oracle','difference','dump']];W=v['W'];R=v['R'];BIN=W/'rust-target-v5/release/shadow-v5'
def fresh(name):return setup('research-'+name)
def random_changes(root,rng,batch):
    operations=[]
    for j in range(20):
        op=rng.choice(['write','atomic','link','unlink','chmod','mmap','directory'])
        p=root/f'f{rng.randrange(30)}'
        if op=='write':p.write_bytes(f'{batch}-{j}'.encode()*30)
        elif op=='atomic':
            tmp=root/'tmp-save';tmp.write_bytes(f'atomic-{batch}-{j}'.encode());tmp.replace(p)
        elif op=='link':
            p.unlink(missing_ok=True);src=root/f'f{rng.randrange(30)}'
            if src.exists() and src!=p:os.link(src,p)
        elif op=='unlink':p.unlink(missing_ok=True)
        elif op=='chmod':
            if p.exists():p.chmod(rng.choice([0o600,0o644,0o755]))
        elif op=='mmap':
            if p.exists() and p.stat().st_size:
                with p.open('r+b') as f:
                    with mmap.mmap(f.fileno(),0) as mm:mm[0:1]=bytes([65+rng.randrange(26)]);mm.flush()
        else:
            a=root/'tree';b=root/'moved'
            if a.exists():a.rename(b)
            elif b.exists():b.rename(a)
            else:
                (a/'new'/'nested').mkdir(parents=True);(a/'new/nested/x').write_text('new')
        operations.append([op,p.name])
    return operations

def randomized():
    rows=[]
    for seed in range(5):
        for side in ['shadow','external']:
            root,state=fresh(f'random-{seed}-{side}');rng=random.Random(seed);o=Shadow(root,state)
            try:
                for batch in range(20):
                    if side=='shadow':
                        begin=o.begin();before=oracle(o.shadow);ops=random_changes(o.shadow,rng,batch);after=oracle(o.shadow);fin=o.finish();expected=difference(before,after)
                        result=o.commit() if batch%2 else o.discard()
                        row=dict(seed=seed,side=side,batch=batch,ops=ops,begin=begin,finish=fin,result=result,expected=expected,exact=fin['epsilon']['paths']==expected,real_shadow_difference=difference(oracle(root),oracle(o.shadow)))
                    else:
                        ops=random_changes(root,rng,batch);begin=o.begin();diff=difference(oracle(root),oracle(o.shadow));fin=o.finish();result=o.commit();row=dict(seed=seed,side=side,batch=batch,ops=ops,begin=begin,finish=fin,result=result,exact=not diff,real_shadow_difference=diff)
                    row['pass']=row['exact'] and fin['audit']['exact'] and not row['real_shadow_difference'] and result['outcome'] in ['Committed','Unchanged','discarded'];rows.append(row);dump(R/'randomized-v5.json',rows)
                    if not row['pass']:raise RuntimeError(row)
            finally:o.close()
    print('randomized',len(rows),flush=True)

def completeness():
    rows=[]
    for side in ['shadow','external']:
        root,state=fresh('mmap-'+side);o=Shadow(root,state);p=(o.shadow if side=='shadow' else root)/'a'
        f=p.open('r+b');mm=mmap.mmap(f.fileno(),0);f.close()
        # 跨 act 持有映射，先消费打开/关闭事件；映射写入没有 close_write 兜底。
        o.begin();o.finish();o.commit()
        if side=='shadow':o.begin()
        mm[0:1]=b'Z';mm.flush()
        if side=='external':sync=o.begin()
        fin=o.finish();rows.append(dict(case='persistent-mmap',side=side,finish=fin,sync=sync if side=='external' else None,real_shadow_difference=difference(oracle(root),oracle(o.shadow)),bytes=p.read_bytes().hex()))
        mm.close();o.discard();o.close()
    for side in ['shadow','external']:
        root,state=fresh('tmpfile-'+side);o=Shadow(root,state)
        if side=='shadow':o.begin();out=o.execute([W/'probe-v3','tmpfile'])
        else:out=subprocess.run([str(W/'probe-v3'),'tmpfile'],cwd=root,capture_output=True,text=True);out=dict(returncode=out.returncode,stdout=out.stdout,stderr=out.stderr);sync=o.begin()
        fin=o.finish();rows.append(dict(case='tmpfile-linkat',side=side,execution=out,finish=fin,real_shadow_difference=difference(oracle(root),oracle(o.shadow)) if side=='external' else None));o.discard();o.close()
    root,state=fresh('unwatched-hardlink');os.link(root/'a',root/'target/alias');o=Shadow(root,state);(root/'target/alias').write_text('outside-alias');sync=o.begin();fin=o.finish();rows.append(dict(case='external-unwatched-hardlink',sync=sync,finish=fin,difference=difference(oracle(root),oracle(o.shadow))));o.close()
    # 真正填满内核队列；SIGSTOP 限于本测试的节点。
    root,state=fresh('overflow');o=Shadow(root,state);o.begin();os.kill(o.proc.pid,signal.SIGSTOP)
    try:
        for i in range(20000):(o.shadow/f'overflow-{i}').write_text(str(i))
    finally:os.kill(o.proc.pid,signal.SIGCONT)
    fin=o.finish();rows.append(dict(case='kernel-overflow',finish=fin,exact=fin['audit']['exact'],overflow=fin['overflow']));o.discard();o.close()
    dump(R/'completeness-v5.json',rows);print('completeness',flush=True)

def windows():
    rows=[]
    for injection in ['before-expected','after-ready','after-check',None]:
        for rep in range(3):
            root,state=fresh(f'window-{injection}-{rep}');o=Shadow(root,state);o.begin();(o.shadow/'a').write_text('sandbox');o.finish()
            if injection:o.call('env',values={'S2_INJECT_AT':injection,'S2_INJECT_PATH':'a'})
            result=o.commit();rows.append(dict(injection=injection,rep=rep,result=result,external_preserved=(root/'a').read_text()=='external-window-v5',final=(root/'a').read_text(),temporary_files=[p.name for p in root.glob('.s2-v5-*')]));o.close()
    dump(R/'commit-window-v5.json',rows);print('windows',flush=True)

def crashes():
    rows=[]
    stages=['before-log','after-log','before-pre-sync','after-pre-sync','after-ready','apply-1','apply-4','before-post-sync','after-post-sync','after-complete','inplace-partial','after-rename']
    for stage in stages:
        for rep in range(5):
            root,state=setup(f'research-crash-{stage}-{rep}','rename-dir' if stage=='after-rename' else 'hardlink-normal');before=oracle(root);o=Shadow(root,state);o.begin()
            cmd='mv dir moved; echo edit > moved/x' if stage=='after-rename' else 'printf changed > a; printf changed > b; rm outside; mkdir newdir; printf new > newdir/new; ln -s a newlink'
            out=o.execute(['sh','-c',cmd]);desired=oracle(o.shadow);fin=o.finish();o.call('env',values={'S2_KILL_AT':stage})
            try:o.commit();killed=False
            except RuntimeError:o.proc.wait();killed=o.proc.returncode==-9
            o.close();recovered=Shadow(root,state);diff=difference(desired if stage=='after-complete' else before,oracle(root));a=recovered.begin();b=recovered.finish();c=recovered.commit();row=dict(stage=stage,rep=rep,killed=killed,difference=diff,shadow_difference=difference(oracle(root),oracle(recovered.shadow)),next_result=c,execution=out)
            row['ledger_phases']=[json.loads(p.read_text())['phase']for p in sorted((state/'transactions').glob('*/transaction.json'))];row['expected_phases']=['complete']if stage=='after-complete'else[]if stage=='before-log'else['rolled-back'];row['pass']=killed and not diff and not row['shadow_difference'] and c['outcome']=='Unchanged'and row['ledger_phases']==row['expected_phases'];rows.append(row);dump(R/'crashes-v5.json',rows);recovered.close()
            if not row['pass']:print('crash FAIL',stage,rep,row,flush=True)
    print('crashes',sum(x['pass']for x in rows),len(rows),flush=True)

def supplements():
    rows=[]
    root,state=setup('v5-external-sync','rename-dir');o=Shadow(root,state)
    (root/'a').write_text('external');(root/'b').unlink();(root/'new').write_text('new');os.rename(root/'dir',root/'renamed');(root/'empty').rmdir();(root/'empty').symlink_to('new');os.link(root/'new',root/'hard')
    sync=o.begin();rows.append(dict(case='external-sync',sync=sync,difference=difference(oracle(root),oracle(o.shadow))));o.close()
    root,state=setup('v5-initial-metadata','hardlink-normal');stamp=1700000000123456789;os.utime(root/'a',ns=(stamp,stamp));(root/'a').chmod(0o640);os.utime(root/'alias',ns=(stamp,stamp),follow_symlinks=False);o=Shadow(root,state)
    rows.append(dict(case='initial-metadata',difference=[p for p in ['a','hard','alias','sub','sub/a'] if ((root/p).lstat().st_mode,(root/p).lstat().st_mtime_ns)!=((o.shadow/p).lstat().st_mode,(o.shadow/p).lstat().st_mtime_ns)]));o.close()
    root,state=fresh('multi-act');before=oracle(root);o=Shadow(root,state);txns=[]
    for p in ['a','b']:o.begin();(o.shadow/p).write_text('changed-'+p);o.finish();txns.append(o.commit()['transaction'])
    o.close();reverts=[]
    for txn in reversed(txns):
        out=subprocess.run([str(BIN),'recover',txn,'--revert'],capture_output=True,text=True);reverts.append(dict(code=out.returncode,stdout=out.stdout,stderr=out.stderr))
    rows.append(dict(case='multi-act-revert',reverts=reverts,difference=difference(before,oracle(root))))
    root,state=setup('v5-hardlink-inode','hardlink-normal');ino=(root/'a').stat().st_ino;o=Shadow(root,state);o.begin();(o.shadow/'hard').write_text('sandbox');o.finish();txn=o.commit()['transaction'];after=(root/'a').stat().st_ino;o.close();out=subprocess.run([str(BIN),'recover',txn,'--revert'],capture_output=True,text=True);rows.append(dict(case='hardlink-inode',before=ino,after=after,reverted=(root/'a').stat().st_ino,hard=(root/'hard').stat().st_ino,code=out.returncode))
    # revert 冲突、第四轮跨类别别名与 xattr 限制。
    root,state=fresh('revert-conflict');o=Shadow(root,state);o.begin();(o.shadow/'a').write_text('sandbox');o.finish();txn=o.commit()['transaction'];o.close();(root/'a').write_text('external');out=subprocess.run([str(BIN),'recover',txn,'--revert'],capture_output=True,text=True);rows.append(dict(case='revert-conflict',result=json.loads(out.stdout),external_preserved=(root/'a').read_text()=='external'))
    for case in ['hardlink-alias','setxattr']:
        root,state=setup('v5-'+case,case);o=Shadow(root,state);o.begin();out=o.execute([W/'probe-v3',case]);fin=o.finish();rows.append(dict(case=case,execution=out,finish=fin,real_changed=(root/'a').read_bytes().startswith(b'AFTER!'),shadow_xattrs=os.listxattr(o.shadow/'a')));o.close()
    dump(R/'supplement-v5.json',rows);print('supplements',flush=True)
def downtime():
    root,state=fresh('downtime-external');o=Shadow(root,state);o.begin();(o.shadow/'a').write_text('sandbox');o.finish();o.call('env',values={'S2_KILL_AT':'after-ready'})
    try:o.commit()
    except RuntimeError:o.proc.wait()
    o.close();(root/'a').write_text('external-during-downtime');o=Shadow(root,state)
    row=dict(case='external-during-downtime',external_preserved=(root/'a').read_text()=='external-during-downtime',final=(root/'a').read_text());o.close();dump(R/'downtime-v5.json',row)

def rename_links():
    rows=[]
    for edit in [False,True]:
        for outside in [False,True]:
            root,state=setup(f'v5-rename-links-{edit}-{outside}','rename-dir');(root/'dir/one').write_text('original');os.link(root/'dir/one',root/'dir/two')
            if outside:os.link(root/'dir/one',root/'outside-hard')
            before=oracle(root);ino=(root/'dir/one').stat().st_ino;o=Shadow(root,state);o.begin();(o.shadow/'dir').rename(o.shadow/'moved')
            if edit:(o.shadow/'moved/one').write_text('changed')
            desired=oracle(o.shadow);fin=o.finish();result=o.commit();row=dict(edit=edit,outside=outside,finish=fin,result=result,difference=difference(desired,oracle(root)),inode_preserved=(root/'moved/one').exists()and(root/'moved/one').stat().st_ino==ino)
            o.close()
            if result.get('outcome')=='Committed':
                p=subprocess.run([str(BIN),'recover',result['transaction'],'--revert'],capture_output=True,text=True);row.update(recovery=dict(code=p.returncode,stdout=p.stdout,stderr=p.stderr),restored_difference=difference(before,oracle(root)))
            row['pass']=result.get('outcome')=='Committed'and not row['difference']and row['inode_preserved']and not row.get('restored_difference');rows.append(row);dump(R/'rename-links-v5.json',rows)
    print('rename-links',sum(x['pass']for x in rows),len(rows),flush=True)

def index_recovery():
    root,state=fresh('index-recovery');o=Shadow(root,state);index=state/'index-v5.jsonl';initial=index.stat().st_size;lines=len(index.read_text().splitlines());o.begin();(o.shadow/'a').write_text('incremental');o.finish();result=o.commit();after=index.stat().st_size;added=len(index.read_text().splitlines())-lines;o.close()
    with index.open('ab')as f:f.write(b'{"torn":');f.flush();os.fsync(f.fileno())
    o=Shadow(root,state);difference_after=difference(oracle(root),oracle(o.shadow));valid=all(isinstance(json.loads(line),dict)for line in index.read_text().splitlines());o.begin();o.finish();next_result=o.commit();o.close()
    dump(R/'index-recovery-v5.json',dict(initial_bytes=initial,added_bytes=after-initial,added_lines=added,result=result,torn_tail_rebuilt=valid,difference=difference_after,next_result=next_result))

def ledger_compat():
    root,state=fresh('ledger-compat');before=oracle(root);o=Shadow(root,state);o.begin();(o.shadow/'a').write_text('sandbox');o.finish();txn=Path(o.commit()['transaction']);o.close()
    record=json.loads((txn/'transaction.json').read_text());b=record['before']['a'];event=dict(index=0,path=str(root/'a'),before=b['kind'],**{k:b[k]for k in ['mode','dev','ino','nlink']})
    (txn/'ledger.jsonl').write_text(json.dumps(event)+'\n');os.link(txn/'pre/a',txn/'0.clone');old=runpy.run_path(str(HERE/'recover-v3.py'));dump(txn/'poststate.json',{str(root/'a'):old['state_of'](root/'a')})
    proc=subprocess.run(['python3',str(HERE/'recover-v3.py'),str(txn)],capture_output=True,text=True);dump(R/'ledger-compat-v5.json',dict(returncode=proc.returncode,stdout=proc.stdout,stderr=proc.stderr,difference=difference(before,oracle(root)),projection_required=['ledger.jsonl','poststate.json','index.clone']))

def density():
    rows=[]
    for n in [1000,10000]:
        root,state=fresh(f'density-{n}')
        for i in range(n):
            p=root/f'd{i:06}';p.mkdir();(p/'f').write_text('data')
        o=Shadow(root,state);rows.append(dict(directories=n,initial=o.initial,stats=o.call('stats')));o.close()
    dump(R/'watch-density-v5.json',rows)

if __name__=='__main__':
    for task in sys.argv[1:] or ['randomized','completeness','windows','crashes','supplements']:globals()[task]()
