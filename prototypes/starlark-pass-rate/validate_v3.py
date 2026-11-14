"""逐条离线回放、重算候选态与提交指标，并审计旧证据不变及密钥隔离。"""
from collections import Counter
import argparse
import hashlib
import json
from pathlib import Path
import shutil
import subprocess
import tempfile
from check import evaluate, files_at
from driver import ROOT, V3, TEMP, MAX_ACT, MODELS, classify, run_act, verify_freeze
from budget import read_key
from report_v3 import read_runs, aggregate
from metrics import act_metrics


def main():
    parser=argparse.ArgumentParser()
    parser.add_argument('--phase',choices=['pilot','formal'],default='formal')
    args=parser.parse_args()
    freeze=verify_freeze()
    for name,digest in freeze['pilot_files'].items():
        assert hashlib.sha256((ROOT/name).read_bytes()).hexdigest()==digest,'冻结后试跑记录变化'
    runs=read_runs(args.phase)
    ids=['read-02','probe-02','edit-01','batch-01','test-01','rollback-01'] if args.phase=='pilot' else [p.name for p in (ROOT/'tasks').iterdir() if p.is_dir()]
    expected={(a,t,r) for a in MODELS for t in ids for r in ([1] if args.phase=='pilot' else [1,2,3])}
    actual=[(r['summary']['alias'],r['summary']['task'],r['summary']['repeat']) for r in runs]
    assert len(actual)==len(set(actual)) and set(actual)==expected,'运行网格不完整或有重复'
    intervals={a:[] for a in MODELS}
    for r in runs:
        intervals[r['summary']['alias']].extend([(r['rows'][0]['time'],1),(r['summary']['time'],-1)])
    peaks={}
    for alias,points in intervals.items():
        live=maximum=0
        for _,delta in sorted(points):
            live+=delta;maximum=max(maximum,live)
        peaks[alias]=maximum
        assert maximum<=(1 if alias=='minimax' else 4),'模型并发超过上限'
    mismatches=[]; differences=[]; total=0
    for run in runs:
        summary=run['summary']; task=ROOT/'tasks'/summary['task']
        spec=json.loads((task/'spec.json').read_text())
        responses=[r for r in run['rows'] if r['type']=='api_response']
        totals={}
        for row in responses:
            response=row['response']
            assert row['actual_model']==response.get('model'),'响应模型未如实记录'
            if summary['alias']=='deepseek':
                assert row['actual_provider']==response.get('provider') and row['actual_provider'],'OpenRouter 未记录实际提供方'
            for name in ['prompt_tokens','completion_tokens','total_tokens']:
                if name in (response.get('usage') or {}):totals[name]=totals.get(name,0)+response['usage'][name]
        assert summary['usage']==totals,'token 汇总不符'
        assert summary['budget_exhausted']==(len(run['calls'])==MAX_ACT),'预算标志不符'
        assert summary['act_count']==len(run['calls'])<=MAX_ACT,'调用数不符'
        with tempfile.TemporaryDirectory(dir=TEMP,prefix='replay-') as td:
            temp=Path(td);fixture=temp/'repo';shutil.copytree(task/'fixture',fixture)
            calls=[]
            for i,c in enumerate(run['calls'],1):
                total+=1;before=files_at(fixture)
                assert c['call']==i,'act 编号不连续'
                assert c['usage']==(responses[c['response_index']-1]['response'].get('usage') or {}),'act token 关联不符'
                assert c['metrics']==act_metrics(c['script'],c['outcome']),'接口指标不符'
                outcome=c['outcome'] if c['outcome']['outcome']=='tool_arguments_error' else run_act(fixture,task,c['script'],summary['mode'],temp)
                calls.append(dict(script=c['script'],outcome=outcome))
                for k in ['parsed','ran','outcome','line','events','changes','commit_called','candidate_changes','revert_notice','receipt_summary']:
                    if outcome.get(k)!=c['outcome'].get(k):mismatches.append([summary['run_id'],i,k])
                if outcome.get('value')!=c['outcome'].get('value'):
                    differences.append(dict(run_id=summary['run_id'],call=i,script=c['script'],recorded=c['outcome'].get('value'),replayed=outcome.get('value')))
                candidate=dict(before)
                for path,content in outcome.get('candidate_changes',{}).items():
                    if content is None:candidate.pop(path,None)
                    else:candidate[path]=content
                checks=dict(check=evaluate(task,files_at(fixture),calls),candidate_check=evaluate(task,candidate,calls),candidate_files_correct=candidate==spec['expected'])
                for k,v in checks.items():
                    if v!=c[k]:mismatches.append([summary['run_id'],i,k])
                assert classify(c['script'],c['outcome'],summary['mode'])==c['error_category'],'错误分类不符'
                message=next(r['message'] for r in run['rows'] if r['type']=='tool_message' and r['call']==i)
                if c['outcome']['ran']:
                    assert message['content']==c['outcome']['content'],'可见内容不符'
                    assert len(message['content'].encode())<=65536,'交回内容超过上限'
                if c['outcome'].get('revert_notice'):
                    assert message['content'].endswith('\n'+c['outcome']['revert_notice']),'撤销提示未在末尾'
            files=files_at(fixture)
            hashes={p:hashlib.sha256(content.encode()).hexdigest() for p,content in files.items()}
            if hashes!=summary['final_hashes']:mismatches.append([summary['run_id'],'final_hashes'])
            result=evaluate(task,files,calls,summary['final'])
            if summary['status']=='complete' and (result['passed']!=summary['passed'] or result['reasons']!=summary['reasons']):mismatches.append([summary['run_id'],'summary'])
    # 历史报告、结果、判定和全部任务必须与用户指定的 v2 起点逐字节一致。
    old=['REPORT.md','REPORT-v2.md','results/formal','results/pilot','results/v2','evidence','tasks','check.py','command-normalization.json']
    git=['git','--no-optional-locks','--no-pager','diff','--no-ext-diff','--no-textconv','--exit-code']
    subprocess.run(git+['origin/exp/act-v2','--']+[str(ROOT/p) for p in old],check=True,capture_output=True)
    subprocess.run(git+['HEAD','--','docs'],check=True,capture_output=True,cwd=ROOT.parents[1])
    keys=[read_key('~/.config/llm-keys/'+name) for name in ['openrouter.key','minimax-cn.key']]
    for path in V3.rglob('*'):
        if path.is_file():
            content=path.read_bytes()
            assert not any(k.encode() in content for k in keys),'结果中发现密钥（不输出内容）'
    evidence=V3/'evidence'
    (evidence/f'{args.phase}-replay-mismatches.json').write_text(json.dumps(mismatches,ensure_ascii=False,indent=2)+'\n')
    (evidence/f'{args.phase}-replay-value-differences.json').write_text(json.dumps(differences,ensure_ascii=False,indent=2)+'\n')
    result=dict(phase=args.phase,runs=len(runs),acts=total,grid_complete=True,peaks=peaks,states_and_checks_mismatched=len(mismatches),value_differences=len(differences),old_evidence_and_tasks_unchanged=True,docs_unchanged=True,no_keys=True,statuses=dict(Counter(r['summary']['status'] for r in runs)))
    (evidence/f'{args.phase}-validation.json').write_text(json.dumps(result,ensure_ascii=False,indent=2)+'\n')
    print(json.dumps(result,ensure_ascii=False))
    assert not mismatches,'回放不一致，见 evidence 中清单'


if __name__=='__main__':main()
