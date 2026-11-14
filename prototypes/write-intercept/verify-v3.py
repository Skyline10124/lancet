#!/usr/bin/env python3
"""核对实验语义和交付完整性，不把有效反例当成脚本失败。"""
import gzip
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
HERE=Path(__file__).resolve().parent;ROOT=HERE.parent.parent;R=HERE/'results/v3'
def load(name):
    p=R/(name+'.json')
    return json.loads(p.read_text()) if p.exists() else json.loads(gzip.decompress(p.with_suffix('.json.gz').read_bytes()))
def main():
    checks=[]
    def check(name,ok,detail=None):checks.append(dict(name=name,passed=bool(ok),detail=detail))
    rows=load('landlock-races');check('七类参数改写各 200，常规路径零变化',len(rows)==7 and all(r['result']['observations'][0]['denied']==200 and not r['regular_changes'] and r['result']['stats']['continued']>=200 for r in rows))
    rows=load('landlock-residual');check('硬链接残余与两类可写树跨类反例确实执行',rows[0]['result']['observations'][0]['succeeded']==200 and all(r['result']['observations'][0]['succeeded']==1 for r in rows[1:]))
    rows=load('internal-races');check('沙箱内 80 次有实际尝试且没有插入锁内',len(rows)==80 and all(r['completed_inside_lock']==0 and r['result']['returncode']==0 and any(o.get('attacker_attempted')==1 for o in r['result']['observations']) for r in rows))
    rows=load('directory-lock');check('目录改名阻断不同子目录的打开',len(rows)==20 and all(r['observations'][0]['rc']==0 and r['observations'][0]['inside_lock']==0 and r['observations'][0]['attempted']==1 for r in rows))
    rows=load('external-races');check('外部 100 次窗口均执行，检测后恢复无冲突 b',len(rows)==100 and all(r['trace'] for r in rows) and all(not r['recovery']['errors'] and r['recovery_check']['unconflicted_b'] and r['recovery_check']['conflicts_untouched'] for r in rows if r['recovery']))
    rows=load('anchors');check('12 次操作成功、有锚点，新节点恢复保住 inode 拓扑',len(rows)==12 and all(r['operation_ok'] and r['exact_inode_topology'] and not r['recovery']['errors'] and r['recovery']['recovery_pid']!=r['result']['node_pid'] for r in rows))
    rows=load('directories');check('两个规模各 5 次，结构记录恒为 1、clone 恒为 0，恢复精确',len(rows)==10 and all(r['result']['stats']['moves']==1 and r['result']['stats']['clones']==0 and r['result']['observations'][0]['rc']==0 and not r['exact_difference'] and not r['recovery']['errors'] for r in rows))
    rows=load('directory-order');edit=next(r for r in rows if r['case']=='rename-edit');cross=next(r for r in rows if r['case']=='rename-cross');exchange=next(r for r in rows if r['case']=='rename-exchange-dir')
    check('先恢复改名后的文件，再改回目录', [e['action'] for e in edit['recovery']['trace']]==['object','rename'] and not edit['recovery']['errors'])
    def normal(view):return {p:{k:v for k,v in state.items() if k not in ['dev','ino','nlink']} for p,state in view.items() if p.split('/')[0] not in ['target','.git']}
    check('目录三种恢复的实际内容和权限正确',all(normal(r['before'])==normal(r['after']) for r in rows))
    check('跨类目录逐个 clone、目录交换无 clone',cross['result']['stats']['clones']==3 and not cross['recovery']['errors'] and exchange['result']['stats']['moves']==1 and exchange['result']['stats']['clones']==0 and not exchange['recovery']['errors'])
    rows=load('grouped');violations=[];peak=0
    for r in rows:
        commits={e['upto']:e['complete'] for e in r['audit'] if e.get('kind')=='commit'}
        for e in r['audit']:
            if 'execute_after' in e and (e['durable_generation'] not in commits or e['execute_after']<commits[e['durable_generation']]):violations.append(e)
        events=sorted([(e['start'],1) for e in r['audit'] if e.get('kind')=='fsync']+[(e['end'],-1) for e in r['audit'] if e.get('kind')=='fsync']);n=0
        for t,d in events:n+=d;peak=max(peak,n)
    check('并发 fsync、写许可晚于提交完成',len(rows)==10 and not violations and peak>1 and all(r['result']['returncode']==0 and r['result']['stats']['max_batch']>1 for r in rows),dict(peak_fsync_overlap=peak,violations=violations))
    cov=load('coverage');groups={}
    for r in cov:groups.setdefault(r['case'],{})[r['mode']]=r['result']['observations'][0]
    mismatches=[c for c,g in groups.items() if (g['ns'].get('rc'),g['ns'].get('errno'))!=(g['B3'].get('rc'),g['B3'].get('errno'))]
    check('上下文已记录两项动态 mount 差异，其余与 ns 一致',set(mismatches)=={'tmpfs-perm','tmpfs-perm-nocap'},mismatches)
    check('连续 cwd/umask 和无效 dirfd 不破坏 worker',all(r['returncode']==0 and r['observations'][0].get('ok') for r in load('context-alternating')) and all(r['returncode']==0 and r['observations'][0]['wrong']==0 for r in load('invalid-dirfd')))
    visibility=load('worker-visibility')['evidence'];ns=visibility['namespaces'];check('worker 在目标 user/mount、宿主 pid，沙箱 proc 不可见',ns['worker']['user']==ns['init']['user'] and ns['worker']['mnt']==ns['init']['mnt'] and ns['worker']['pid']==ns['node']['pid'] and ns['worker']['pid']!=ns['init']['pid'] and visibility['worker_absent_in_sandbox_proc'])
    rows=load('crashes');check('50 次最终终止，实际返回的调用均 ENOSYS，变化项保守恢复',len(rows)==50 and all(not r['live_processes'] and r['returned_errno'] in [0,38] and not any(p.endswith('/a') and '/target/' not in p for p in r['recovery']['residue']) for r in rows) and any(r['returned_errno']==38 for r in rows))
    for name in ['builds','checkouts']:
        rows=load(name);check(name+' 四配置各三个成功样本',all(len([r for r in rows if r['mode']==m and r['returncode']==0])==3 for m in ['none','ns','B2v2','B3']))
    check('空与缺失 target 均完整构建成功',all(r['result']['returncode']==0 for r in load('missing-target')))
    check('微基准每组 20,000 次无失败',len(load('micro'))==15 and all(r['result']['returncode']==0 and r['result']['observations'][0]['iterations']==20000 and r['result']['observations'][0]['failed']==0 for r in load('micro')))
    # 逐字比较受保护文件，不依赖 staged/unstaged 视图。
    files=subprocess.check_output(['git','ls-tree','-r','--name-only','origin/exp/write-intercept','--','prototypes/write-intercept'],cwd=ROOT,text=True).splitlines();changed=[]
    for name in files:
        rel=Path(name).relative_to('prototypes/write-intercept')
        if str(rel) in ['Makefile','README.md']:continue
        old=subprocess.check_output(['git','show',f'origin/exp/write-intercept:{name}'],cwd=ROOT)
        if old!=(ROOT/name).read_bytes():changed.append(name)
    check('第一二轮源码、报告、results 均保持原样',not changed,changed)
    check('ADR 没有修改',not subprocess.check_output(['git','diff','HEAD','--','docs/adr'],cwd=ROOT))
    links=[]
    for p in [HERE/'README.md',HERE/'REPORT-v3.md']:
        for target in re.findall(r'\]\(([^)]+)\)',p.read_text()):
            if not target.startswith(('http:','https:','#')) and not (p.parent/target.split('#')[0]).exists():links.append([p.name,target])
    check('交付文档相对链接存在',not links,links)
    live=[]
    for p in Path('/proc').glob('[0-9]*'):
        try:
            exe=os.readlink(p/'exe')
            if exe.startswith(str(HERE/'.scratch')) and Path(exe).name.startswith(('intercept','probe')):live.append(int(p.name))
        except (OSError,PermissionError):pass
    check('没有本轮沙箱或 probe 残留',not live,live)
    sources=[p for p in HERE.iterdir() if p.name.endswith(('-v3.py','-v3.c')) or p.name in ['v3-node.h','v3-runtime.h','Makefile']]
    hashes={p.name:hashlib.sha256(p.read_bytes()).hexdigest() for p in sources};hashes['binary']=hashlib.sha256((HERE/'.scratch/intercept-v3').read_bytes()).hexdigest()
    result=dict(checks=checks,passed=all(c['passed'] for c in checks),hashes=hashes)
    (R/'verification.json').write_text(json.dumps(result,ensure_ascii=False,indent=2)+'\n')
    for c in checks:print('PASS' if c['passed'] else 'FAIL',c['name'],c['detail'] if not c['passed'] else '')
    return 0 if result['passed'] else 1
if __name__=='__main__':raise SystemExit(main())
