#!/usr/bin/env python3
"""第五轮外置 oracle：复用第三轮探针，不以原型自身 epsilon 为判据。"""
import concurrent.futures
import errno
import json
import os
from pathlib import Path
import runpy
import shutil
import subprocess
import sys
import time

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
v3 = runpy.run_path(str(HERE/'experiment-v3.py'))
s = runpy.run_path(str(HERE/'shadow-v5.py'))
Shadow, dump = s['Shadow'], s['dump']
R = HERE/'results/v5'
W = HERE/'.scratch'


def oracle(root):
    raw = v3['view'](root)
    raw = {p:e for p,e in raw.items() if p.split('/')[0] not in s['EXCLUDED']}
    groups = {}
    for p,e in raw.items():
        if e['type'] == 'file':
            groups.setdefault((e['dev'],e['ino']), []).append(p)
    result = {p:v3['semantic'](e) for p,e in raw.items()}
    for paths in groups.values():
        for p in paths:
            result[p]['links'] = sorted(paths)
    return result


def difference(a,b):
    return sorted(p for p in a.keys() | b.keys() if a.get(p) != b.get(p))


def setup(name, case='open'):
    root = v3['setup']('v5-'+name, case)
    state = W/'states-v5'/name
    shutil.rmtree(state, ignore_errors=True)
    return root, state


def coverage():
    cases = '''open open-rdwr open-append open-trunc-readonly open-readonly open-create open-excl-fail open-idle openat cwd openat2 openat2-beneath creat tmpfile tmpfile-empty opath proc-cwd proc-root symlink-alias dangling-symlink umask hardlink-normal unlink unlinkat rmdir unlinkat-dir rename renameat rename-exchange rename-noreplace rename-dir truncate link linkat symlink symlinkat unlink-symlink mkdir mkdirat chmod fchmod fchmodat fchmodat2 mknod mknodat ftruncate fallocate mmap nofollow excl-symlink cloexec not-cloexec fork vfork thread thread-exec'''.split()
    cases += ['unlink-hardlink', 'rename-hardlink', 'anchor-unlink-all', 'anchor-overwrite', 'rename-edit', 'type-file-dir', 'type-dir-file']
    rows = []
    for case in cases:
        for committed in [False, True]:
            name = f'coverage-{case}-{committed}'
            root,state = setup(name, 'hardlink-normal' if case in ['unlink-hardlink','rename-hardlink','anchor-unlink-all','anchor-overwrite'] else 'rename-dir' if case=='rename-edit' else case)
            before = oracle(root)
            obj = Shadow(root,state)
            try:
                sync = obj.begin()
                sb = oracle(obj.shadow)
                command = [W/'probe-v3',case]
                if case == 'type-file-dir': command = ['sh','-c','rm a && mkdir a && echo child > a/x']
                if case == 'type-dir-file': command = ['sh','-c','rmdir empty && echo file > empty']
                baseline_root,baseline_state = setup('ns-'+name, 'hardlink-normal' if case in ['unlink-hardlink','rename-hardlink','anchor-unlink-all','anchor-overwrite'] else 'rename-dir' if case=='rename-edit' else case)
                baseline_state.mkdir(parents=True,exist_ok=True)
                baseline=subprocess.run([str(W/'intercept-v3'),'ns',str(baseline_root),str(baseline_state),'20','--',*map(str,command)],capture_output=True,text=True)
                out = obj.execute(command)
                after = oracle(obj.shadow)
                finished = obj.finish()
                row = dict(case=case, commit=committed, execution=out, sync=sync,
                           epsilon=finished, expected_paths=difference(sb,after),
                           precommit_real_difference=difference(before,oracle(root)))
                row['namespace_equivalent'] = not difference(oracle(baseline_root),after) and baseline.returncode == out['returncode']
                row['namespace_code'] = baseline.returncode
                row['epsilon_exact'] = row['expected_paths'] == finished['epsilon']['paths']
                if committed:
                    result = obj.commit()
                    row['commit_result'] = result
                    row['commit_difference'] = difference(after,oracle(root))
                    if result.get('transaction'):
                        # 新进程消费持久账本，不复用 Shadow 的内存。
                        proc = subprocess.run(['python3',str(HERE/'shadow-v5.py'),'recover',result['transaction'],'--revert'],capture_output=True,text=True)
                        row['recovery'] = dict(code=proc.returncode, stdout=proc.stdout, stderr=proc.stderr)
                    row['restored_difference'] = difference(before,oracle(root))
                else:
                    row['discard_result'] = obj.discard()
                    row['restored_difference'] = difference(before,oracle(root))
                    row['shadow_restored_difference'] = difference(sb,oracle(obj.shadow))
                row['pass'] = (out['returncode'] != 125 and row['namespace_equivalent'] and row['epsilon_exact'] and not row['precommit_real_difference'] and
                               not row.get('commit_difference') and not row['restored_difference'] and
                               not row.get('shadow_restored_difference') and row.get('recovery',{}).get('code',0)==0)
            except Exception as ex:
                row = dict(case=case,commit=committed,error=repr(ex),pass_=False)
            finally:
                obj.close()
            rows.append(row)
            dump(R/'coverage-v5.json',rows)
            print(case,committed,row.get('pass',False),row.get('error',''),flush=True)
    return rows


def boundaries():
    rows=[]
    root,state=setup('boundaries');(root/'.git/a').write_text('git-before');obj=Shadow(root,state);obj.begin()
    code='''import os,json
rows=[]
for a,b in [('a','target/new'),('a','.git/new'),('target/a','.git/new'),('target/a','normal-new'),('.git/a','normal-new'),('.git/a','target/new')]:
 for op in ['link','rename']:
  try:getattr(os,op)(a,b);e=0
  except OSError as ex:e=ex.errno
  rows.append(dict(op=op,source=a,target=b,errno=e))
for op in ['open','mkdir','symlink','link','rename']:
 try:
  if op=='open':open('hidden/new','w').close()
  elif op=='mkdir':os.mkdir('hidden/new')
  elif op=='symlink':os.symlink('a','hidden/new')
  else:getattr(os,op)('a','hidden/new')
  e=0
 except OSError as ex:e=ex.errno
 rows.append(dict(op=op,target='hidden/new',errno=e))
print(json.dumps(rows))
'''
    out=obj.execute(['python3','-c',code]);rows=json.loads(out['stdout']);obj.finish();obj.discard();obj.close()
    dump(R/'boundaries-v5.json',dict(execution=out,rows=rows,cross_all_exdev=all(r['errno']==errno.EXDEV for r in rows[:12]),hidden_all_denied=all(r['errno'] in [errno.EROFS,errno.EACCES] for r in rows[12:])))


def external_writes():
    rows=[]
    for same in [False,True]:
        for committed in [False,True]:
            for rep in range(3):
                root,state=setup(f'external-{same}-{committed}-{rep}');obj=Shadow(root,state);obj.begin()
                before=oracle(root)
                code="from pathlib import Path;import time;Path('a').write_text('sandbox');Path('target/ready').touch();time.sleep(.2)"
                def external():
                    deadline=time.monotonic()+5
                    while not (root/'target/ready').exists():
                        if time.monotonic()>deadline:raise RuntimeError('未收到写入握手')
                        time.sleep(.001)
                    (root/('a' if same else 'outside')).write_text('external')
                with concurrent.futures.ThreadPoolExecutor() as pool:
                    fut=pool.submit(external)
                    out=obj.execute(['python3','-c',code]);fut.result()
                fin=obj.finish();result=obj.commit() if committed else obj.discard()
                rows.append(dict(same=same,commit=committed,rep=rep,execution=out,epsilon=fin,result=result,
                                 external_preserved=(root/('a' if same else 'outside')).read_text()=='external',
                                 external_not_in_epsilon='outside' not in fin['epsilon']['paths'],
                                 shadow_start_restored=not difference(before,oracle(obj.shadow)) if not committed else None))
                obj.close();dump(R/'concurrent-v5.json',rows)
    return rows


if __name__=='__main__':
    for task in sys.argv[1:] or ['coverage','boundaries','external_writes']:globals()[task]()
