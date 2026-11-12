"""离线回放正式脚本，逐条复核判定和完整运行网格。"""
from collections import Counter
import argparse
import hashlib
import json
import subprocess
from pathlib import Path
import shutil
import tempfile
from check import evaluate, files_at
from driver import ROOT, V2, GROUPS, MAX_ACT, MODELS, classify, run_act, verify_freeze
from report import read_runs
from metrics import act_metrics


def main():
    parser=argparse.ArgumentParser(); parser.add_argument('--partial',action='store_true'); args=parser.parse_args()
    suffix='-partial' if args.partial else ''
    freeze=verify_freeze()
    for name,digest in freeze['pilot_files'].items():
        assert hashlib.sha256((ROOT/name).read_bytes()).hexdigest()==digest, '冻结后试跑记录变化'
    changed=subprocess.check_output(['git','diff','--name-only',freeze['base_commit'],'--','docs','prototypes/starlark-pass-rate/REPORT.md','prototypes/starlark-pass-rate/results/formal','prototypes/starlark-pass-rate/results/pilot','prototypes/starlark-pass-rate/evidence'],text=True)
    assert not changed, 'v1 原始记录或 docs 被修改'
    runs=read_runs('formal',allow_incomplete=args.partial)
    expected={(group,model,mode,task.name,repeat) for group in GROUPS for model in MODELS for mode in ['A','B'] for task in (ROOT/'tasks').iterdir() if task.is_dir() for repeat in [1,2,3]}
    actual=[(r['summary']['group'],r['summary']['alias'],r['summary']['mode'],r['summary']['task'],r['summary']['repeat']) for r in runs]
    assert len(actual)==len(set(actual)), '正式运行网格重复'
    assert set(actual)<=expected if args.partial else set(actual)==expected, '正式运行网格不符'
    intervals={alias:[] for alias in MODELS}
    for run in runs:
        intervals[run['summary']['alias']].extend([(run['rows'][0]['time'],1),(run['summary']['time'],-1)])
    peak={}
    for alias,points in intervals.items():
        live=maximum=0
        for _,delta in sorted(points):
            live+=delta; maximum=max(maximum,live)
        peak[alias]=maximum
        assert maximum<=(1 if alias=='minimax' else 4), '模型并发超过上限'
    mismatches=[]; value_differences=[]; call_count=0
    for r in runs:
        summary=r['summary']; task=ROOT/'tasks'/summary['task']
        responses=[row['response'] for row in r['rows'] if row['type']=='api_response']
        totals={}
        for response in responses:
            for name in ('prompt_tokens','completion_tokens','total_tokens'):
                if name in (response.get('usage') or {}): totals[name]=totals.get(name,0)+response['usage'][name]
        assert summary['usage']==totals, '任务 token 汇总不符'
        assert summary['budget_exhausted']==(len(r['calls'])==MAX_ACT), '预算耗尽标记不符'
        first_write=next((c['call'] for c in r['calls'] if c['metrics']['category_counts']['write']),None)
        assert summary['first_write_act']==first_write, '首次写 act 标记不符'
        assert 0<=len(r['calls'])<=MAX_ACT, 'act 次数超过上限'
        with tempfile.TemporaryDirectory(prefix='act-replay-') as td:
            temp=Path(td); fixture=temp/'repo'; shutil.copytree(task/'fixture',fixture)
            calls=[]
            for i,c in enumerate(r['calls'],1):
                assert c['call']==i, 'act 编号不连续'
                call_count+=1
                assert c['usage']==(responses[c['response_index']-1].get('usage') or {}), 'act token 关联不符'
                assert c['metrics']==act_metrics(c['script'],c['outcome']), '调用指标重算不符'
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
            expected_first=bool(r['calls'] and (r['calls'][0]['check']['passed'] or (len(r['calls'])==1 and summary['passed']))) if summary['status']=='complete' else None
            assert summary['first_act_passed']==expected_first, '首次 act 判定不符'
            if hashes!=summary['final_hashes']: mismatches.append([summary['run_id'],'final_hashes'])
            check=evaluate(task,files,calls,summary['final'])
            if summary['status']=='complete' and (summary['passed']!=check['passed'] or summary['reasons']!=check['reasons']): mismatches.append([summary['run_id'],'summary'])
    (V2/f'evidence/replay-value-differences{suffix}.json').write_text(json.dumps(value_differences,ensure_ascii=False,indent=2)+'\n')
    (V2/f'evidence/replay-mismatches{suffix}.json').write_text(json.dumps(mismatches,ensure_ascii=False,indent=2)+'\n')
    assert not mismatches, json.dumps(mismatches,ensure_ascii=False)
    result=dict(runs=len(runs),calls=call_count,grid_complete=set(actual)==expected,frozen_files=len(freeze['files']),replay_state_or_check_mismatches=0,replay_value_differences=len(value_differences),metrics_and_usage_checked=True,peak_concurrent_runs=peak,v1_and_docs_unchanged=True,status_counts=dict(Counter(r['summary']['status'] for r in runs)))
    (V2/f'evidence/validation{suffix}.json').write_text(json.dumps(result,ensure_ascii=False,indent=2)+'\n')
    print(json.dumps(result,ensure_ascii=False))


if __name__=='__main__': main()
