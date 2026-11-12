"""离线回放正式脚本，逐条复核判定和完整运行网格。"""
from collections import Counter
import hashlib
import json
from pathlib import Path
import shutil
import tempfile
from check import evaluate, files_at
from driver import ROOT, MODELS, classify, run_act, verify_freeze
from report import read_runs


def main():
    freeze=verify_freeze()
    runs=read_runs('formal')
    expected={(model,mode,task.name,repeat) for model in MODELS for mode in ['A','B'] for task in (ROOT/'tasks').iterdir() if task.is_dir() for repeat in [1,2,3]}
    actual=[(r['summary']['alias'],r['summary']['mode'],r['summary']['task'],r['summary']['repeat']) for r in runs]
    assert len(actual)==len(set(actual)) and set(actual)==expected, '正式运行网格缺失或重复'
    mismatches=[]; value_differences=[]; call_count=0
    for r in runs:
        summary=r['summary']; task=ROOT/'tasks'/summary['task']
        assert 0<=len(r['calls'])<=4, 'act 次数超过上限'
        with tempfile.TemporaryDirectory(prefix='act-replay-') as td:
            temp=Path(td); fixture=temp/'repo'; shutil.copytree(task/'fixture',fixture)
            calls=[]
            for i,c in enumerate(r['calls'],1):
                assert c['call']==i, 'act 编号不连续'
                call_count+=1
                if c['outcome']['outcome']=='tool_arguments_error': outcome=c['outcome']
                else: outcome=run_act(fixture,task,c['script'],summary['mode'],temp)
                if outcome.get('value')!=c['outcome'].get('value'):
                    value_differences.append(dict(run_id=summary['run_id'],call=i,script=c['script'],recorded=c['outcome'].get('value'),replayed=outcome.get('value')))
                for key in ['parsed','ran','outcome','line','events','changes']:
                    if outcome.get(key)!=c['outcome'].get(key): mismatches.append([summary['run_id'],i,key])
                calls.append(dict(script=c['script'],outcome=outcome))
                if evaluate(task,files_at(fixture),calls)!=c['check']: mismatches.append([summary['run_id'],i,'check'])
                if classify(c['script'],outcome,summary['mode'])!=c['error_category']: mismatches.append([summary['run_id'],i,'category'])
            files=files_at(fixture)
            hashes={p:hashlib.sha256(c.encode()).hexdigest() for p,c in files.items()}
            if hashes!=summary['final_hashes']: mismatches.append([summary['run_id'],'final_hashes'])
            check=evaluate(task,files,calls,summary['final'])
            if summary['status']=='complete' and (summary['passed']!=check['passed'] or summary['reasons']!=check['reasons']): mismatches.append([summary['run_id'],'summary'])
    (ROOT/'evidence/replay-value-differences.json').write_text(json.dumps(value_differences,ensure_ascii=False,indent=2)+'\n')
    assert not mismatches, json.dumps(mismatches,ensure_ascii=False)
    result=dict(runs=len(runs),calls=call_count,grid_complete=True,frozen_files=len(freeze['files']),replay_state_or_check_mismatches=0,replay_value_differences=len(value_differences),status_counts=dict(Counter(r['summary']['status'] for r in runs)))
    (ROOT/'evidence/validation.json').write_text(json.dumps(result,ensure_ascii=False,indent=2)+'\n')
    print(json.dumps(result,ensure_ascii=False))


if __name__=='__main__': main()
