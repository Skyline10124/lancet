"""报告冻结判定器对 wc -l 返回排列的已知限制；不修改任何判定。"""
import json
import re
from pathlib import Path
from check import leaves
from driver import ROOT
from report import read_runs


def audit(runs):
    rows=[]
    for r in runs:
        summary=r['summary']
        if summary['task']!='read-03' or summary['status']!='complete': continue
        wc_evidence=[]
        for c in r['calls']:
            if not c['outcome']['ran']: continue
            for value in leaves(c['outcome']['value']):
                if not isinstance(value,str): continue
                counts={path:int(n) for n,path in re.findall(r'(?m)^\s*(\d+)\s+(data/[ab]\.txt|total)\s*$',value)}
                if counts=={'data/a.txt':3,'data/b.txt':4,'total':7}:
                    wc_evidence.append(dict(call=c['call'],value=value))
        reasons=summary['reasons']
        if wc_evidence and not summary['passed'] and reasons and all(x.startswith('行数未与文件对应: ') for x in reasons):
            rows.append(dict(run_id=summary['run_id'],frozen_passed=summary['passed'],reasons=reasons,wc_evidence=wc_evidence))
    return rows


if __name__=='__main__':
    result=audit(read_runs('formal', version=1))
    (ROOT/'evidence/checker-audit.json').write_text(json.dumps(result,ensure_ascii=False,indent=2)+'\n')
    print(json.dumps(dict(affected_runs=len(result)),ensure_ascii=False))
