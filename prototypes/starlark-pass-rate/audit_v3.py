"""记录回放值差异、边界外路径候选和非标准命令；只审计，不改完成率。"""
from collections import Counter
import json
import re
from driver import V3
from report_v3 import read_runs
from replay_audit import normalized, external_args


def main():
    audits=[];external=[];writes=[];failures=[]
    for phase in ['pilot','formal']:
        runs=read_runs(phase)
        diffs=json.loads((V3/f'evidence/{phase}-replay-value-differences.json').read_text())
        by_id={r['summary']['run_id']:r for r in runs}
        for d in diffs:
            c=next(c for c in by_id[d['run_id']]['calls'] if c['call']==d['call'])
            equal=normalized(d['recorded'])==normalized(d['replayed'])
            outside=external_args(c)
            def temp_normalized(v):
                return re.sub(r'(?<![A-Za-z0-9])\.tmp[A-Za-z0-9]{6}(?![A-Za-z0-9])','.tmp<ID>',json.dumps(normalized(v),sort_keys=True,ensure_ascii=False))
            temp_equal=temp_normalized(d['recorded'])==temp_normalized(d['replayed'])
            audits.append(dict(phase=phase,run_id=d['run_id'],call=d['call'],kind='ls 元数据' if equal else '临时副本目录名与 ls 元数据' if temp_equal else '需逐项审查',metadata_normalized_equal=equal,temp_normalized_equal=temp_equal,absolute_or_parent_args=outside))
        for r in runs:
            if not r['summary']['passed']:
                failures.append(dict(phase=phase,run_id=r['summary']['run_id'],reasons=r['summary']['reasons'],final=r['summary']['final'],act_values=[c['outcome'].get('value') for c in r['calls'] if c['outcome']['ran']]))
            for c in r['calls']:
                for argv in external_args(c):
                    external.append(dict(phase=phase,run_id=r['summary']['run_id'],call=c['call'],argv=argv,outcome=c['outcome']['outcome']))
                for e in c['outcome'].get('events',[]):
                    commands=[e['args']['argv']] if e['op']=='run' else e['args']['argvs'] if e['op']=='pipe' else []
                    for argv in commands:
                        if argv and (argv[0]=='git' or argv[0]=='sed' and any(a.startswith('-i') or a=='--in-place' for a in argv[1:])):
                            writes.append(dict(phase=phase,run_id=r['summary']['run_id'],call=c['call'],argv=argv,outcome=c['outcome']['outcome']))
    for name,value in [('replay-difference-audit',audits),('absolute-argument-audit',external),('possible-command-write-audit',writes),('failed-run-evidence',failures)]:
        (V3/f'evidence/{name}.json').write_text(json.dumps(value,ensure_ascii=False,indent=2)+'\n')
    print(json.dumps(dict(differences=dict(Counter(a['kind'] for a in audits)),absolute_argument_candidates=len(external),possible_command_writes=len(writes),failed_runs=len(failures)),ensure_ascii=False))


if __name__=='__main__':main()
