#!/usr/bin/env python3
"""补充归属、重启、多 act、同步与已知边界的反例。"""
import json
import os
from pathlib import Path
import runpy
import shutil
import subprocess
import sys
import time

HERE=Path(__file__).resolve().parent
v=runpy.run_path(str(HERE/'verify-v4.py'))
s=v['s'];Shadow,setup,oracle,difference,dump=[v[k] for k in ['Shadow','setup','oracle','difference','dump']]
R,W=v['R'],v['W']


def edges():
    rows=[]
    # 外部修改、添加、删除、类型变化、目录改名、硬链接组变化后的下次 act。
    root,state=setup('sync-edges','rename-dir');o=Shadow(root,state);o.begin()
    (root/'a').write_text('external');(root/'b').unlink();(root/'new').write_text('new')
    os.rename(root/'dir',root/'renamed');(root/'empty').rmdir();(root/'empty').symlink_to('new');os.link(root/'new',root/'hard')
    sync=o.begin();rows.append(dict(case='external-sync',sync=sync,difference=difference(oracle(root),oracle(o.shadow))))
    o.close()
    # 初始 reflink 保留 mtime、mode、链接文字与常规硬链接组。
    root,state=setup('initial-metadata','hardlink-normal')
    stamp=1700000000123456789
    os.utime(root/'a',ns=(stamp,stamp));(root/'a').chmod(0o640)
    os.utime(root/'alias',ns=(stamp,stamp),follow_symlinks=False)
    o=Shadow(root,state);o.begin()
    mismatch=[]
    for p in ['a','hard','alias','sub','sub/a']:
        a=(root/p).lstat();b=(o.shadow/p).lstat()
        if a.st_mtime_ns!=b.st_mtime_ns or a.st_mode!=b.st_mode:mismatch.append(p)
    rows.append(dict(case='initial-metadata',difference=mismatch,hardlink_preserved=(o.shadow/'a').stat().st_ino==(o.shadow/'hard').stat().st_ino,
                     shadow_is_different_inode=(root/'a').stat().st_ino!=(o.shadow/'a').stat().st_ino))
    o.close()
    # 多 act，先提交 a 再提交 b；按相反次序在新进程求逆。
    root,state=setup('multi-act');before=oracle(root);txns=[]
    for p in ['a','b']:
        o=Shadow(root,state);o.begin();(o.shadow/p).write_text('changed-'+p);o.finish();txns.append(o.commit()['transaction']);o.close()
    reverts=[]
    for txn in reversed(txns):
        r=subprocess.run(['python3',str(HERE/'shadow-v4.py'),'recover',txn,'--revert'],capture_output=True,text=True)
        reverts.append(dict(code=r.returncode,stdout=r.stdout,stderr=r.stderr))
    rows.append(dict(case='multi-act-revert',reverts=reverts,difference=difference(before,oracle(root))))
    # 待 revert 的路径被外部改写，必须保持外部内容。
    root,state=setup('revert-conflict');o=Shadow(root,state);o.begin();(o.shadow/'a').write_text('sandbox');o.finish();txn=o.commit()['transaction'];o.close()
    (root/'a').write_text('external');r=s['recover'](txn,True)
    rows.append(dict(case='revert-conflict',result=r,external_preserved=(root/'a').read_text()=='external'))
    # 真实 regenerable 中预建跨类别硬链接，bind mount 不能拆掉已有 inode 关系。
    root,state=setup('preexisting-alias','hardlink-alias');o=Shadow(root,state);o.begin()
    out=o.execute([W/'probe-v3','hardlink-alias']);fin=o.finish()
    rows.append(dict(case='preexisting-cross-category-alias',execution=out,epsilon=fin,real_regular_changed=(root/'a').read_bytes().startswith(b'AFTER!')));o.close()
    # 纯扫描无法强制 ADR 的 xattr ENOTSUP：该原型明确缺少独立策略过滤器。
    root,state=setup('xattr');o=Shadow(root,state);o.begin();out=o.execute([W/'probe-v3','setxattr']);fin=o.finish()
    rows.append(dict(case='xattr-policy',execution=out,epsilon=fin,shadow_xattrs=os.listxattr(o.shadow/'a')));o.close()
    # 硬链接原 inode 在提交和求逆过程中保持相等。
    root,state=setup('hardlink-inode','hardlink-normal');ino=(root/'a').stat().st_ino;o=Shadow(root,state);o.begin();(o.shadow/'hard').write_text('sandbox');o.finish();txn=o.commit()['transaction'];after=(root/'a').stat().st_ino;o.close();r=s['recover'](txn,True)
    rows.append(dict(case='hardlink-inode',before=ino,after=after,reverted=(root/'a').stat().st_ino,hard=(root/'hard').stat().st_ino,result=r))
    dump(R/'supplement-v4.json',rows)
    print(json.dumps(rows,ensure_ascii=False,indent=2))


def ledger_compat():
    # 在明确写出的 v3 兼容后态上调用原版恢复器，核对格式兼容。
    root,state=setup('ledger-compat');before=oracle(root);o=Shadow(root,state);o.begin();(o.shadow/'a').write_text('sandbox');o.finish();txn=Path(o.commit()['transaction']);o.close()
    original=json.loads((txn/'poststate.json').read_text())
    compatible={p:{k:e[k] for k in ['type','mode','dev','ino','nlink','sha256','link'] if k in e} for p,e in original.items()}
    dump(txn/'poststate.json',compatible)
    result=subprocess.run(['python3',str(HERE/'recover-v3.py'),str(txn)],capture_output=True,text=True)
    dump(R/'ledger-compat-v4.json',dict(command=result.args,returncode=result.returncode,stdout=result.stdout,stderr=result.stderr,
                                      poststate_metadata_projection_required=True,difference=difference(before,oracle(root))))


def restart_cases():
    rows=[]
    for stage in ['before-log','after-ready','apply-1','after-complete','inplace-partial']:
        root,state=setup('restart-'+stage,'hardlink-normal');o=Shadow(root,state);o.begin()
        (o.shadow/'a').write_text('changed');(o.shadow/'new').write_text('new');o.close()
        killed=subprocess.run(['python3',str(HERE/'shadow-v4.py'),'commit',str(root),str(state)],env=dict(os.environ,S_KILL_AT=stage),capture_output=True,text=True)
        r=subprocess.run(['python3',str(HERE/'recover-v4.py'),str(root),str(state)],capture_output=True,text=True)
        o=Shadow(root,state)
        try:
            sync=o.begin();o.finish();com=o.commit()
            rows.append(dict(stage=stage,killed=killed.returncode==-9,restart_code=r.returncode,stdout=r.stdout,stderr=r.stderr,
                             sync=sync,next_outcome=com['outcome'],difference=difference(oracle(root),oracle(o.shadow))))
        except Exception as ex:rows.append(dict(stage=stage,error=repr(ex)))
        finally:o.close()
    dump(R/'restart-v4.json',rows)


def commit_window():
    root,state=setup('commit-window');o=Shadow(root,state);o.begin();(o.shadow/'a').write_text('sandbox');o.finish()
    original=Shadow.commit.__globals__['reconcile']
    def hooked(*args,**kwargs):
        if kwargs.get('checkpoints'):(root/'a').write_text('external-after-check')
        return original(*args,**kwargs)
    Shadow.commit.__globals__['reconcile']=hooked
    try:result=o.commit()
    finally:Shadow.commit.__globals__['reconcile']=original;o.close()
    dump(R/'commit-window-v4.json',dict(result=result,final=(root/'a').read_text(),external_preserved=(root/'a').read_text()=='external-after-check',
                                      injected_at='三方比对与前态落盘之后、应用之前'))


def git_minimal_only_risk():
    g=runpy.run_path(str(HERE/'git-v4.py'));host=g['host'];root=g['ROOT'];c=g['C']
    host(['checkout','-f','--quiet','--detach',c['a']])
    p=root/'wi-checkout-data/d00/f0000.txt';old=p.read_bytes();stamp=int(time.time())-60
    os.utime(p,(stamp,stamp));host(['status','--porcelain','-uno'])
    saved_index=(root/'.git/index').read_bytes()
    time.sleep(1.1)  # 跨秒，使默认 ctime 检查也有明确的独立信号。
    p.write_bytes(b'X'+old[1:]);os.utime(p,(stamp,stamp))
    minimal=host(['status','--porcelain','-uno'],{'GIT_CONFIG_COUNT':'1','GIT_CONFIG_KEY_0':'core.checkStat','GIT_CONFIG_VALUE_0':'minimal'})
    default_after=host(['status','--porcelain','-uno'])
    (root/'.git/index').write_bytes(saved_index)
    default=host(['status','--porcelain','-uno'])
    dump(R/'git-minimal-only-risk-v4.json',dict(minimal=minimal,default_after_minimal=default_after,default_independent=default,bytes_changed=p.read_bytes()!=old))
    host(['checkout','-f','--quiet','--detach',c['original']])


def initial_repeats():
    rows=json.loads((R/'initial-v4.json').read_text())
    for row in rows:row['rep']=0
    for n in [3000,10000,100000]:
        for rep in [1,2]:
            root=W/f'initial-source-{n}-{rep}-v4'
            shutil.rmtree(root,ignore_errors=True);root.mkdir()
            for i in range(n):
                p=root/f'd{i%100:03}'/f'f{i:06}';p.parent.mkdir(exist_ok=True);p.write_text(f'before-{i}\n'*32)
            state=W/'states-v4'/f'initial-{n}-{rep}';shutil.rmtree(state,ignore_errors=True)
            o=Shadow(root,state);start=time.monotonic();info=o.begin();info['init_total_s']=time.monotonic()-start
            info.update(files=n,rep=rep)
            for key,flags,p in [('du_allocated_bytes',[],o.shadow),('du_apparent_bytes',['--apparent-size'],o.shadow),('state_allocated_bytes',[],state)]:
                info[key]=int(subprocess.check_output(['du','-s','-B1',*flags,str(p)]).split()[0])
            rows.append(info);dump(R/'initial-v4.json',rows);o.close();print(n,rep,info['init_total_s'],flush=True)
            shutil.rmtree(state);shutil.rmtree(root)


if __name__=='__main__':
    for task in sys.argv[1:] or ['edges','ledger_compat','restart_cases','commit_window','git_minimal_only_risk']:globals()[task]()
