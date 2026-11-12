#!/usr/bin/env python3
"""验证实验记录的完整性、口径和可复算性；不要求实验结果符合预期。"""
import argparse
from collections import Counter
import hashlib
import json
from pathlib import Path
import sys
import time

from report import read
from ovl_act import save

HERE=Path(__file__).resolve().parent
RESULTS=HERE/'results'


def main():
    parser=argparse.ArgumentParser()
    parser.add_argument("--allow-reruns",action="store_true")
    args=parser.parse_args()
    start=time.monotonic()
    errors=[]
    def check(condition,message):
        if not condition:
            errors.append(message)
    exclusions=read(RESULTS/'excluded-runs.json') if (RESULTS/'excluded-runs.json').exists() else {}
    rows=[]
    for p in RESULTS.glob('S*.json*'):
        d=read(p)
        if isinstance(d,dict) and 'outcome' in d and 'scenario' in d and d['run_id'] not in exclusions:
            rows.append(d)
    keys=Counter((r['scenario'],r.get('variant'),r['git_policy']) for r in rows)
    expected={}
    for s in ('S1','S3','S4'):
        expected[(s,'default','P1')]=3
    for p in ('P1','P2','P3'):
        expected[('S2','noise',p)]=3
        for noise in ('quiet','noise'):
            for finish in ('commit','discard'):
                expected[('S6',noise+'-'+finish,p)]=3
    for mode in ('discard','keep-regenerable'):
        expected[('S5',mode,'P1')]=3
    for cmd in ('mv','git-mv','os-rename'):
        expected[('S7',cmd,'P1')]=3
    expected[('S8','opaque','P1')]=3
    for relation in ('different','same'):
        for actor in ('A','B'):
            expected[('S9',relation+'-'+actor,'P1')]=3
    matrix_ok = dict(keys)==expected
    if args.allow_reruns:
        matrix_ok = set(keys)==set(expected) and all(n>=3 and n%3==0 for n in keys.values())
    check(matrix_ok, '运行矩阵不完整：'+str(dict(keys)))
    ids={r['run_id'] for r in rows}
    for r in rows:
        name=r['run_id']
        for field in ('start_utc','end_utc','total_s','child','conflicts','classification','snapshot',
                      'upper_counts','after_git','expected','observed','assertions','matches_expectation','confusion'):
            check(field in r,name+' 缺字段 '+field)
        check(r.get('outcome')!='blocked',name+' 基础设施阻塞')
        check(r['free_bytes']>=15*1024**3,name+' 磁盘不足')
        check(r['check']=='both',name+' 检查方案不匹配')
        check(r['matches_expectation']==all(r['assertions'].values()),name+' 判定不一致')
        check(r['after_git']['fsck']['code']==0,name+' Git fsck 非零')
        check(not any(p=='target' or p.startswith('target/') for p in r['checked_paths']),name+' 检查了 target')
        check(not any(p=='target' or p.startswith('target/') for p in r['preimages']),name+' 为 target 留了前态')
        expected_truth=['README.md'] if r['scenario']=='S3' or (r['scenario']=='S9' and r['variant']=='same-B') else []
        check(r.get('truth_conflict_paths',[])==expected_truth,name+' 真值不匹配')
        for c in ('C1','C4'):
            paths=r['conflicts'][c]
            truth=set(expected_truth)
            confusion=dict(false_positive=sorted(set(paths)-truth),false_negative=sorted(truth-set(paths)))
            check(confusion==r['confusion'][c],name+' 混淆计数不一致 '+c)
            observed=r['observations']
            recomputed=[]
            for p,v in observed.items():
                if c=='C1' and v['before']!=v['current']:
                    recomputed.append(p)
                if c=='C4' and v['current'] is not None and v['current']['ctime_ns']>r['t0_ns']:
                    recomputed.append(p)
            check(sorted(recomputed)==paths,name+' 检查判定无法复算 '+c)
        if r['outcome']=='conflict':
            check(not r['committed_paths'] and not r['preimages'],name+' 冲突时存在提交或前态')
        if r['committed_paths']:
            check(r['overlay_xattr_residual_count']==0,name+' 扩展属性残留')
            check(r['xattr_scan']['code']==0,name+' getfattr 非零')
        if r['scenario']=='S5':
            check('cargo build -p net-guard' in r['script_text'] and 'cargo test -p net-guard' in r['script_text'],name+' crate 参数')
            check(r['next_build']['command']==['cargo','build','-p','net-guard'],name+' 重建命令')
            check('tests::static_base_url_byte_limit_is_inclusive ... FAILED' in r['child']['script']['stdout'],name+' 未触发指定失败测试')
        if r['scenario']=='S9':
            check(set(r['pair'])<=ids,name+' S9 配对不完整')
        if 'noise' in r:
            n=r['noise']
            check(n['process_code']==0,name+' 噪声进程非零')
            check(n.get('cargo',{}).get('code')==0,name+' 外部 cargo check 非零')
            check(n['duration_s']>=n['config']['duration_s'],name+' 噪声窗口不足')
            check(n['config']['initial_phase']=='seeded-uniform',name+' 相位规则')
    for cp in RESULTS.glob('calibration-*/calibration.json'):
        cal=read(cp)
        for name,meta in cal['sources'].items():
            content=(cp.parent/name).read_bytes()
            check(len(content)==meta['bytes'] and hashlib.sha256(content).hexdigest()==meta['sha256'],'校准输入摘要不匹配 '+name)
        for k,n in cal['observed_operations'].items():
            check(cal['intervals_s'][k]==(cal['covered_seconds']/n if n else None),'校准频率不一致 '+k)
    summary=dict(runs=len(rows),variants=len(keys),excluded=len(exclusions),errors=errors,
                 outcomes=dict(Counter(r['outcome'] for r in rows)),
                 expectation_true=sum(r['matches_expectation'] is True for r in rows),
                 confusion={c:{k:sum(len(r['confusion'][c][k]) for r in rows) for k in ('false_positive','false_negative')} for c in ('C1','C4')})
    save(RESULTS/'data-validation.json',dict(command=['python3','prototypes/overlay-commit/verify_results.py']+(['--allow-reruns'] if args.allow_reruns else []),
                                          code=int(bool(errors)),seconds=time.monotonic()-start,allow_reruns=args.allow_reruns,summary=summary))
    print(json.dumps(summary,ensure_ascii=False,indent=2))
    return int(bool(errors))


if __name__=='__main__':
    sys.exit(main())
