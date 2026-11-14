#!/usr/bin/env python3
"""校验最终结果矩阵；失败证据独立保存，不计入成功样本。"""
import json
from pathlib import Path

R=Path(__file__).resolve().parent/'results/v4'
def read(n):return json.loads((R/n).read_text())
checks={}
def check(name,condition):checks[name]=bool(condition)
a=read('coverage-v4.json');check('coverage_126',len(a)==126 and all(x['pass'] and x['namespace_equivalent'] and x['epsilon_exact'] for x in a))
check('tmpfiles_executed',all(x['execution']['returncode']==0 for x in a if x['case'] in ['tmpfile','tmpfile-empty']))
check('directory_rename_identified',all(x['epsilon']['epsilon']['directory_renames'] for x in a if x['case']=='rename-dir'))
a=read('boundaries-v4.json');check('cross_category_12_exdev',len(a['rows'])==17 and a['cross_all_exdev'] and a['hidden_all_denied'])
a=read('concurrent-v4.json');check('external_12',len(a)==12 and all(x['external_preserved'] and x['external_not_in_epsilon'] and x['epsilon']['epsilon']['paths']==['a'] for x in a))
check('same_file_commit_conflict',all(x['result']['outcome']=='Conflict' for x in a if x['same'] and x['commit']))
a=read('crashes-v4.json');check('crashes_55',len(a)==55 and all(x['pass'] for x in a));check('crash_ledger',all(x['ledger_phases']==(['complete'] if x['stage']=='after-complete' else [] if x['stage']=='before-log' else ['rolled-back']) for x in a))
a=read('restart-v4.json');check('restart_5',len(a)==5 and all(x.get('restart_code')==0 and x['killed'] and not x['difference'] and x['next_outcome']=='Unchanged' for x in a))
a=read('git-v4.json');check('git_48',len(a)==48 and all(x['status_expected'] and x['inside']['returncode']==0 and x['host']['returncode']==0 for x in a))
a=read('git-discard-v4.json');check('git_discard_counterexamples',len(a)==2 and all(not x['head_preserved'] and x['changed_lines']>0 for x in a))
a=read('git-minimal-only-risk-v4.json');check('minimal_independent_control',a['bytes_changed'] and a['minimal']['stdout']=='' and 'f0000.txt' in a['default_independent']['stdout'])
a=read('incremental-v4.json');check('incremental_one_crate',len(a['compiled3'])==1 and 'lattice-cli' in a['compiled3'][0] and a['compiled_host']==[] and a['act3']['returncode']==0)
for name in ['builds-v4.json','checkouts-v4.json']:
    a=read(name);check(name,len(a)==12 and all(sum(x['mode']==m and x['returncode']==0 for x in a)==3 for m in ['none','ns','B3','S']))
a=read('micro-v4.json');check('micro_24',len(a)==24 and all(x['returncode']==0 and x['observation']['iterations']==20000 and x['observation']['failed']==0 for x in a))
check('sync_27',len(read('sync-v4.json'))==27)
a=read('settlement-v4.json');check('settlement_9',len(a)==9 and all(x['outcome']=='Committed' and x['epsilon_count']==x['changed'] for x in a))
a=read('initial-v4.json');check('initial_9',len(a)==9 and all(sum(x['files']==n for x in a)==3 for n in [3000,10000,100000]))
a=read('baseline-integrity-v4.json');check('baseline_unmodified',a['checked']==2092 and a['mismatch']==[])
a=read('ledger-compat-v4.json');check('ledger_object_format',a['returncode']==0 and a['difference']==[])
check('commit_window_counterexample',read('commit-window-v4.json')['external_preserved'] is False)
a=read('supplement-v4.json');check('supplement_semantics',all(not x.get('difference') for x in a));check('preexisting_alias_counterexample',next(x for x in a if x['case']=='preexisting-cross-category-alias')['real_regular_changed'])
result=dict(passed=all(checks.values()),checks=checks)
(R/'audit-v4.json').write_text(json.dumps(result,ensure_ascii=False,indent=2)+'\n')
print(json.dumps(result,ensure_ascii=False,indent=2))
raise SystemExit(0 if result['passed'] else 1)
