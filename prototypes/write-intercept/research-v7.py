#!/usr/bin/env python3
"""第七轮：全内容 oracle、持久 mmap、随机负载、事务冲突与 SIGKILL。"""
import ctypes,json,mmap,os,random,runpy,shutil,signal,subprocess,sys,time
from pathlib import Path
HERE=Path(__file__).resolve().parent
v=runpy.run_path(str(HERE/'verify-v7.py'));Shadow,setup,oracle,difference,dump=[v[k]for k in ['Shadow','setup','oracle','difference','dump']];W=v['W'];R=v['R'];BIN=W/'rust-target-v7/release/shadow-v7'
old=runpy.run_path(str(HERE/'research-v5.py'));random_changes=old['random_changes']
def fresh(name):return setup('research-'+name)
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
                        ops=random_changes(root,rng,batch);sync=o.call('sync');begin=o.begin();diff=difference(oracle(root),oracle(o.shadow));fin=o.finish();result=o.commit();row=dict(seed=seed,side=side,batch=batch,ops=ops,sync=sync,begin=begin,finish=fin,result=result,exact=not diff,real_shadow_difference=diff)
                    row['pass']=row['exact'] and fin['audit']['exact'] and not row['real_shadow_difference'] and result['outcome'] in ['Committed','Unchanged','discarded','Conflict'];rows.append(row);dump(R/'randomized-v7.json',rows)
                    if not row['pass']:print('random failure',seed,side,batch,flush=True);break
            except Exception as e:rows.append(dict(seed=seed,side=side,error=repr(e),pass_=False));dump(R/'randomized-v7.json',rows)
            finally:o.close()
    print('randomized',sum(x.get('pass',False)for x in rows),len(rows),flush=True)

def synchronize():
    rows=[]
    root,state=fresh('long-mmap');(root/'a').write_bytes(b'A'*4096);o=Shadow(root,state)
    f=open(root/'a','r+b');mm=mmap.mmap(f.fileno(),0);f.close()
    try:
        for i in range(30):
            # 同一长寿命映射，两次写之间不 msync；连续 act 保持映射打开。
            mm[0:1]=bytes([66+i%20]);mm[1:2]=bytes([70+i%20]);t=time.monotonic();deadline=t+5
            while (o.shadow/'a').read_bytes()!=(root/'a').read_bytes() and time.monotonic()<deadline:time.sleep(.002)
            delay=time.monotonic()-t;sync=o.call('stats');o.begin();fin=o.finish();o.commit()
            rows.append(dict(case='long-lived-mmap',rep=i,delay_s=delay,exact=not difference(oracle(root),oracle(o.shadow)),stats=sync,finish=fin))
    finally:mm.close();o.close()
    for case in ['outside-hardlink','editor-rename','tmpfile-linkat','same-tick','fake-mtime']:
        for rep in range(3):
            root,state=fresh(f'sync-{case}-{rep}');alias=W/f'outside-alias-{rep}-v7';alias.unlink(missing_ok=True)
            if case=='outside-hardlink':os.link(root/'a',alias)
            o=Shadow(root,state);before=(root/'a').stat();old_data=(root/'a').read_bytes();stamp=before.st_mtime_ns
            if case=='outside-hardlink':alias.write_bytes(b'alias-write')
            elif case=='editor-rename':(root/'new-save').write_bytes(b'editor');(root/'new-save').replace(root/'a')
            elif case=='tmpfile-linkat':subprocess.run([str(W/'probe-v3'),'tmpfile'],cwd=root,capture_output=True,check=True)
            elif case=='same-tick':
                (root/'a').write_bytes(b'X'*len(old_data));mid=(root/'a').stat().st_ctime_ns;(root/'a').write_bytes(b'Y'*len(old_data));last=(root/'a').stat().st_ctime_ns
            else:(root/'a').write_bytes(b'Z'*len(old_data));os.utime(root/'a',ns=(stamp,stamp))
            t=time.monotonic();deadline=t+5
            while difference(oracle(root),oracle(o.shadow)) and time.monotonic()<deadline:time.sleep(.002)
            rows.append(dict(case=case,rep=rep,exact=not difference(oracle(root),oracle(o.shadow)),delay_s=time.monotonic()-t,initial=o.initial,same_tick=(mid==last) if case=='same-tick' else None,stats=o.call('stats')));o.close();alias.unlink(missing_ok=True)
    dump(R/'synchronization-v7.json',rows);print('sync',sum(x['exact']for x in rows),len(rows),flush=True)

def concurrency():
    rows=[]
    for kind in ['same-inplace','same-rename','different','late-conflict','rollback-conflict','after-check']:
        for rep in range(3 if kind in ['late-conflict','rollback-conflict','after-check'] else 20):
            root,state=fresh(f'concurrent-{kind}-{rep}');o=Shadow(root,state);o.begin();(o.shadow/'a').write_text('agent-a');(o.shadow/'b').write_text('agent-b');o.finish()
            if kind=='same-inplace':(root/'b').write_text('host-b')
            if kind=='same-rename':(root/'tmp').write_text('host-b');(root/'tmp').replace(root/'b')
            if kind=='different':(root/'other').write_text('host-other')
            if kind in ['late-conflict','rollback-conflict']:o.call('env',values={'S3_INJECT_AT':'before-check','S3_INJECT_PATH':'b'})
            if kind=='after-check':o.call('env',values={'S3_INJECT_AT':'after-check','S3_INJECT_PATH':'b'})
            # 双注入由测试钩子支持，既制造后项冲突又在回滚前改写已换入项。
            if kind=='rollback-conflict':o.call('env',values={'S3_ROLLBACK_WRITE':'a'})
            out=o.commit();row=dict(case=kind,rep=rep,result=out,a=(root/'a').read_text(),b=(root/'b').read_text(),other=(root/'other').read_text() if (root/'other').exists()else None);rows.append(row);o.close()
    dump(R/'concurrency-v7.json',rows);print('concurrency',len(rows),flush=True)

def crashes():
    rows=[]
    for stage in ['before-log','after-ready','apply-1','apply-4','before-post-sync','after-post-sync','after-complete','inplace-partial','after-rename']:
        for rep in range(3):
            root,state=setup(f'crash-{stage}-{rep}','rename-dir' if stage=='after-rename' else 'hardlink-normal' if stage=='inplace-partial' else 'open');before=oracle(root);o=Shadow(root,state);o.begin()
            cmd='mv dir moved; echo edit > moved/x' if stage=='after-rename' else 'printf changed > a; printf changed > b; rm outside; mkdir newdir; printf new > newdir/new; ln -s a newlink'
            out=o.execute(['sh','-c',cmd]);desired=oracle(o.shadow);o.finish();o.call('env',values={'S3_KILL_AT':stage})
            try:result=o.commit();killed=False
            except RuntimeError:o.proc.wait();killed=o.proc.returncode==-9
            o.close();recovered=Shadow(root,state);actual=oracle(root)
            row=dict(stage=stage,rep=rep,killed=killed,equals_before=not difference(before,actual),equals_after=not difference(desired,actual),before_diff=difference(before,actual),after_diff=difference(desired,actual),initial=recovered.initial,execution=out)
            row['policy_conflict']=not killed and stage=='inplace-partial' and result['outcome']=='Conflict'
            row['pass']=(killed and (row['equals_before']or row['equals_after'])) or (row['policy_conflict'] and row['equals_before']);rows.append(row);dump(R/'crashes-v7.json',rows);recovered.close()
    print('crashes',sum(x['pass']for x in rows),len(rows),flush=True)

def close_write():
    rows=[];libc=ctypes.CDLL(None,use_errno=True)
    for kind in ['normal-exit','mmap-fd-closed','unwatched-alias','readonly-truncate']:
        root,state=fresh('close-'+kind);outside=W/f'close-alias-{kind}-v7';outside.unlink(missing_ok=True)
        if kind=='unwatched-alias':os.link(root/'a',outside)
        fd=libc.inotify_init1(os.O_NONBLOCK|os.O_CLOEXEC);libc.inotify_add_watch(fd,os.fsencode(root),0x8)
        code="import os,mmap;f=os.open('a',os.O_RDWR);os.write(f,b'changed')"
        if kind=='mmap-fd-closed':code="import os,mmap;f=os.open('a',os.O_RDWR);m=mmap.mmap(f,0);os.close(f);m[0:1]=b'X'"
        if kind=='unwatched-alias':code=f"open({str(outside)!r},'w').write('alias')"
        if kind=='readonly-truncate':code="import os;os.close(os.open('a',os.O_RDONLY|os.O_TRUNC))"
        subprocess.run(['python3','-c',code],cwd=root,check=True)
        try:data=os.read(fd,65536)
        except BlockingIOError:data=b''
        os.close(fd);outside.unlink(missing_ok=True);rows.append(dict(case=kind,close_write_event=bool(data),event_bytes=len(data)))
    dump(R/'close-write-v7.json',rows)


def continuous_mmap():
    import threading
    root,state=fresh('continuous-mmap');(root/'a').write_bytes(b'0'*4096);o=Shadow(root,state)
    f=open(root/'a','r+b');mm=mmap.mmap(f.fileno(),0);f.close();run=threading.Event();quiet=threading.Event();stop=threading.Event();rows=[]
    def writer():
        counter=0
        while not stop.is_set():
            if run.is_set():
                quiet.clear();counter+=1;mm[:16]=f'{counter:016}'.encode();time.sleep(.0005)
            else:quiet.set();time.sleep(.0005)
    t=threading.Thread(target=writer);t.start()
    try:
        for rep in range(20):
            run.set();error=None
            try:
                o.begin();time.sleep(.025);o.finish(audit=True);o.commit()
            except Exception as e:error=repr(e)
            run.clear();quiet.wait(2);start=time.monotonic()
            while (o.shadow/'a').read_bytes()!=(root/'a').read_bytes() and time.monotonic()-start<2:time.sleep(.002)
            rows.append(dict(rep=rep,error=error,delay_s=time.monotonic()-start,exact=not difference(oracle(root),oracle(o.shadow))))
    finally:
        stop.set();t.join();mm.close();o.close()
    dump(R/'continuous-mmap-v7.json',rows);print('continuous mmap',sum(x['exact']for x in rows),len(rows),flush=True)

if __name__=='__main__':
    for task in sys.argv[1:] or ['synchronize','randomized','concurrency','crashes','close_write','continuous_mmap']:globals()[task]()
