"""区分 ls 元数据变化、fixture 外读取和需要单独核对的返回值差异。"""
import argparse
import json
import re
from collections import Counter
from driver import V2
from metrics import source_calls


def normalized(value):
    if isinstance(value,dict): return {k:normalized(v) for k,v in value.items()}
    if isinstance(value,list): return [normalized(v) for v in value]
    if not isinstance(value,str): return value
    lines=[]
    for line in value.splitlines():
        if re.fullmatch(r'total \d+',line):
            lines.append('total <blocks>')
        elif re.match(r'^[bcdlps-][rwxstST-]{9}[.+]?\s',line):
            parts=line.split(maxsplit=8)
            if len(parts)==9:
                # 文件大小保留；目录大小、链接数和 mtime 是临时副本元数据。
                directory=parts[0].startswith('d')
                lines.append(' '.join([parts[0],'<links>' if directory else parts[1],*parts[2:4],'<dir-size>' if directory else parts[4],'<mtime>',parts[8]]))
            else: lines.append(line)
        else: lines.append(line)
    return '\n'.join(lines)


def external_args(call):
    commands=[]
    for event in call['outcome'].get('events',[]):
        argvs=[event['args']['argv']] if event['op']=='run' else event['args']['argvs'] if event['op']=='pipe' else []
        for argv in argvs:
            if any(arg.startswith('/') or '..' in arg.split('/') for arg in argv): commands.append(argv)
    return commands


def main():
    parser=argparse.ArgumentParser();parser.add_argument('--partial',action='store_true');args=parser.parse_args()
    suffix='-partial' if args.partial else ''
    differences=json.loads((V2/f'evidence/replay-value-differences{suffix}.json').read_text())
    audit=[]
    for difference in differences:
        rows=[json.loads(s) for s in (V2/'formal'/(difference['run_id']+'.jsonl')).read_text().splitlines()]
        call=next(r for r in rows if r['type']=='act' and r['call']==difference['call'])
        outside=external_args(call)
        metadata=normalized(difference['recorded'])==normalized(difference['replayed'])
        kind='ls 元数据' if metadata else '含 fixture 外命令的返回差异' if outside else '需单独核对'
        audit.append(dict(run_id=difference['run_id'],call=difference['call'],kind=kind,external_commands=outside,metadata_normalized_equal=metadata))
    result=dict(counts=dict(Counter(x['kind'] for x in audit)),entries=audit)
    (V2/f'evidence/replay-difference-audit{suffix}.json').write_text(json.dumps(result,ensure_ascii=False,indent=2)+'\n')
    external=[]; unrecorded=[]
    for path in sorted((V2/'formal').glob('*.jsonl')):
        rows=[json.loads(line) for line in path.read_text().splitlines()]
        if rows[-1]['type']!='summary': continue
        if rows[-1].get('status')=='harness_error':
            responses=[x for x in rows if x['type']=='api_response']
            acts=[x for x in rows if x['type']=='act']
            if responses:
                response=responses[-1]['response']
                recorded=sum(c['response_index']==len(responses) for c in acts)
                pending=(response['choices'][0]['message'].get('tool_calls') or [])[recorded:recorded+1]
                for tc in pending:
                    try: script=json.loads(tc['function']['arguments']).get('script','')
                    except (ValueError,KeyError): script=tc.get('function',{}).get('arguments','')
                    unrecorded.append(dict(run_id=rows[-1]['run_id'],response_index=len(responses),tool_call_id=tc['id'],script=script,usage=response.get('usage'),source_interfaces=source_calls(script),error=rows[-1]['harness_error'],runtime_events_available=False))
        for call in rows:
            if call['type']!='act': continue
            for argv in external_args(call):
                external.append(dict(run_id=call['run_id'],call=call['call'],argv=argv,act_outcome=call['outcome']['outcome']))
    (V2/f'evidence/external-command-audit{suffix}.json').write_text(json.dumps(external,ensure_ascii=False,indent=2)+'\n')
    (V2/f'evidence/unrecorded-tool-calls{suffix}.json').write_text(json.dumps(unrecorded,ensure_ascii=False,indent=2)+'\n')
    print(json.dumps(result['counts'],ensure_ascii=False))


if __name__=='__main__': main()
