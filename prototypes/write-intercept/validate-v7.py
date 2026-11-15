#!/usr/bin/env python3
"""汇总断言、报告相对链接、来源摘要与交付清单；不改验收样本。"""
import hashlib,json,os,re,subprocess
from pathlib import Path
P=Path(__file__).resolve().parent;R=P/'results/v7';checks=[]
def read(n):return json.loads((R/(n+'-v7.json')).read_text())
def check(name,ok,detail=None):checks.append(dict(name=name,pass_=bool(ok),detail=detail))
for n,total,key in [('coverage',126,'pass'),('randomized',200,'pass'),('synchronization',45,'exact'),('continuous-mmap',20,'exact'),('crashes',27,'pass'),('git-crashes',18,'pass'),('git-matrix',96,'status_consistent'),('git-extra',18,'status_consistent'),('isolation',7,'pass_'),('rename-links-policy',4,'pass_'),('exchange-crashes',3,'pass_')]:
 rows=read(n);check(n,len(rows)==total and all(r.get(key)for r in rows),dict(passed=sum(bool(r.get(key))for r in rows),total=len(rows)))
x=read('concurrency');ok=len(x)==69
for r in x:
 k=r['case'];expected='Committed'if k=='different'else'Indeterminate'if k=='rollback-conflict'else'Conflict'
 ok &= r['result']['outcome']==expected
 if k=='different':ok &= r['a']=='agent-a' and r['b']=='agent-b' and r['other']=='host-other'
 elif k=='rollback-conflict':ok &= r['a']=='host-during-rollback' and r['b']=='external-window-v7'
 else:ok &= r['a'].startswith('a:BEFORE') and r['b']==('host-b'if k.startswith('same-')else'external-window-v7')
check('concurrency',ok,len(x))
check('self-writes',read('self-writes')['pass_'],read('self-writes'))
check('scan-fairness',read('scan-fairness')['exact'],read('scan-fairness'))
check('host-git-sync',all(r['exact'] and r['head_equal']for r in read('host-git-sync')))
for n in ['git-matrix','git-extra']:check(n+'-stage',all(r['host_stage_preserved']for r in read(n)))
check('git-protected',all(r['config_preserved']and r['hooks_preserved']for r in read('git-extra')))
check('aio',len(read('aio'))==2 and all(r['full_content_complete_after_reap']and r['landed_at_reap']==r['samples']and not r['inotify_after_reap_ms'] for r in read('aio')))
check('builds',len(read('builds'))==9 and all(r['returncode']==0 for r in read('builds')))
check('cache',len(read('private-cache'))==3 and all(r['returncode']==0 and r['source_absent_before'] for r in read('private-cache')))
check('seed',len(read('target-seed'))==3 and all(r['returncode']==0 for r in read('target-seed')))
check('warm-index',len(read('git-scale-warm'))==6 and all(r['begin']['git']['index_rebuilt']==False for r in read('git-scale-warm')))
check('final-fixed-count',len(read('fixed-final'))==27)
check('initial-count',len(read('initial'))==9)
missing=[]
for rel in re.findall(r'\]\(([^)]+)\)',(P/'REPORT-v7.md').read_text()):
 if '://' not in rel and not rel.startswith('#') and not (P/rel.split('#')[0]).exists():missing.append(rel)
check('report-links',not missing,missing)
root=P.parent.parent
p=subprocess.run(['git','--no-optional-locks','diff','--no-ext-diff','--no-textconv','--exit-code','origin/exp/write-intercept','--','prototypes/write-intercept'],cwd=root,capture_output=True,text=True)
check('baseline-unchanged',p.returncode==0)
check('adr-unchanged',not subprocess.check_output(['git','--no-optional-locks','diff','--name-only','HEAD','--','docs/adr'],cwd=root,text=True).strip())
(R/'validation-v7.json').write_text(json.dumps(dict(checks=checks,passed=sum(c['pass_']for c in checks),total=len(checks)),ensure_ascii=False,indent=2)+'\n')
sources=[p for p in P.iterdir() if p.is_file() and ('-v7' in p.name or p.name=='REPORT-v7.md')]+list((P/'shadow-v7').rglob('*'))
sources=[p for p in sources if p.is_file()]
hashes={str(p.relative_to(root)):hashlib.sha256(p.read_bytes()).hexdigest()for p in sources}
for p in [P/'.scratch/rust-target-v7/release/shadow-v7',P/'.scratch/namespace-v7']:
 hashes[str(p.relative_to(root))]=hashlib.sha256(p.read_bytes()).hexdigest()
(R/'source-hashes-v7.json').write_text(json.dumps(hashes,ensure_ascii=False,indent=2)+'\n')
files=sorted(set(sources+list(R.rglob('*'))+[R/'files-v7.txt']));files=[p for p in files if p.is_file() or p==R/'files-v7.txt']
(R/'files-v7.txt').write_text('\n'.join(str(p)for p in files)+'\n')
print(json.dumps(dict(passed=sum(c['pass_']for c in checks),total=len(checks),failures=[c for c in checks if not c['pass_']]),ensure_ascii=False))
