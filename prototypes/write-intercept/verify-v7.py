#!/usr/bin/env python3
"""第七轮外置 oracle：复用第三轮探针，不以原型自身 epsilon 为判据。"""
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
s = runpy.run_path(str(HERE/'shadow-v7.py'))
Shadow, dump = s['Shadow'], s['dump']
R = HERE/'results/v7'
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
    root = v3['setup']('v7-'+name, case)
    state = W/'states-v7'/name
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
                    policy_conflict = result['outcome']=='Conflict' and any((root/p).is_file() and (root/p).stat().st_nlink>1 for p in row['expected_paths'])
                    row['expected_policy_conflict'] = policy_conflict
                    row['commit_difference'] = difference(before if policy_conflict else after,oracle(root))
                    if result.get('transaction'):
                        # 新进程消费持久账本，不复用 Shadow 的内存。
                        proc = subprocess.run(['python3',str(HERE/'shadow-v7.py'),'recover',result['transaction'],'--revert'],capture_output=True,text=True)
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
            dump(R/'coverage-v7.json',rows)
            print(case,committed,row.get('pass',False),row.get('error',''),flush=True)
    return rows


if __name__=='__main__':
    coverage()
