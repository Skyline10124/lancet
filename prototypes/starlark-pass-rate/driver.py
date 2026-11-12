"""OpenAI 兼容接口实验驱动；v2：每任务最多 8 act；MiniMax 串行，deepseek 并发不超过 4。"""
import argparse
import concurrent.futures
import hashlib
import json
import os
from pathlib import Path
import random
import re
import shutil
import subprocess
import tempfile
import time
import urllib.error
import urllib.request
from check import evaluate, files_at
from metrics import act_metrics

ROOT = Path(__file__).resolve().parent
MODELS = {
    'deepseek': dict(base='https://api.deepseek.com',model='deepseek-flash',key_file='~/.config/llm-keys/deepseek.key'),
    'minimax': dict(base='https://api.minimaxi.com/v1',model='MiniMax-M3.1-Flash',key_file='~/.config/llm-keys/minimax-cn.key'),
}
GROUPS = ['base', 'overview', 'composed']
MAX_ACT = 8
V2 = ROOT / 'results/v2'
CATEGORIES = ['Python 习惯','接口误用','唯一匹配失败','命令未预置','超出上限','其他']


def classify(script, outcome, mode):
    if outcome['ran']:
        return None
    error=outcome.get('error') or ''
    pattern=r'\bwhile\s|\btry\s*:|\bexcept\b|\bclass\s|\bimport\s|\bf[\"\x27]|\braise\s|\bwith\s|\byield\b|\bglobal\s|\bassert\s|\bset\s*\('
    if re.search(pattern,script) or (mode=='B' and re.search(r'^return\b',script,re.M)):
        return 'Python 习惯'
    if 'EditMatchError' in error:
        return '唯一匹配失败'
    if '模拟环境未提供该命令' in error:
        return '命令未预置'
    if 'OutputLimitExceeded' in error:
        return '超出上限'
    if re.search(r'not found|has no attribute|not supported on type|Missing parameter|Unexpected parameter|Parameter|argument|expected.*type|not callable|does not have.*attribute',error,re.I):
        return '接口误用'
    return '其他'


def frozen_files():
    paths = [ROOT/'check.py']
    for folder in ['prompts','tasks']:
        paths += [p for p in (ROOT/folder).rglob('*') if p.is_file()]
    return {str(p.relative_to(ROOT)):hashlib.sha256(p.read_bytes()).hexdigest() for p in sorted(paths)}


def verify_freeze():
    manifest=json.loads((V2/'evidence/freeze.json').read_text())
    if frozen_files()!=manifest['files']:
        raise RuntimeError('正式输入与冻结清单不符')
    for name, digest in manifest.get('harness_files', {}).items():
        if hashlib.sha256((ROOT/name).read_bytes()).hexdigest()!=digest:
            raise RuntimeError('测试台与冻结清单不符: '+name)
    if hashlib.sha256((ROOT/'target/debug/act-sim').read_bytes()).hexdigest()!=manifest['binary_sha256']:
        raise RuntimeError('二进制与冻结清单不符')
    return manifest


def call_api(config,key,messages,tool,record):
    body=dict(model=config['model'],messages=messages,tools=[tool])
    request=urllib.request.Request(config['base'].rstrip('/')+'/chat/completions',data=json.dumps(body).encode(),headers={'Authorization':'Bearer '+key,'Content-Type':'application/json'})
    attempts=10 if config['model']==MODELS['minimax']['model'] else 5
    for attempt in range(1,attempts+1):
        start=time.time()
        try:
            with urllib.request.urlopen(request,timeout=180) as response:
                data=json.loads(response.read())
            if not data.get('choices'):
                raise ValueError('API 响应没有 choices: '+json.dumps(data,ensure_ascii=False))
            record(dict(type='api_response',attempt=attempt,elapsed_s=round(time.time()-start,3),response=data))
            return data
        except urllib.error.HTTPError as error:
            detail=error.read().decode(errors='replace').replace(key,'[REDACTED]')
            record(dict(type='api_retry',attempt=attempt,status=error.code,error=detail))
            if error.code!=429 and not 500<=error.code<600:
                raise RuntimeError(f'HTTP {error.code}: {detail}') from None
        except (urllib.error.URLError,TimeoutError,ValueError) as error:
            record(dict(type='api_retry',attempt=attempt,status=None,error=str(error).replace(key,'[REDACTED]')))
        if attempt<attempts:
            time.sleep(min(120,2**(attempt-1)))
    raise RuntimeError(f'API 请求最多 {attempts} 次尝试后仍失败')


def run_act(fixture,task,script,mode,temp):
    source=temp/'script.star'; source.write_text(script)
    command=[str(ROOT/'target/debug/act-sim'),'--fixture',str(fixture),'--script',str(source),'--mode',mode,'--canned',str(task/'canned.json')]
    proc=subprocess.run(command,capture_output=True,text=True,timeout=60,start_new_session=True)
    if proc.returncode:
        raise RuntimeError('act-sim 异常退出: '+proc.stderr)
    return json.loads(proc.stdout)


def tool_prompt(group, mode):
    suffix = '_composed' if group == 'composed' else ''
    return (ROOT/f'prompts/act_tool_{mode}{suffix}.md').read_text()


def user_prompt(task, group):
    goal = (task/'task.md').read_text()
    if group != 'overview':
        return goal
    paths = sorted(p.relative_to(task/'fixture').as_posix() for p in (task/'fixture').rglob('*') if p.is_file())
    listing = 'Repository files:\n' + '\n'.join(paths[:200])
    if len(paths) > 200:
        listing += f'\n[Truncated: showing 200 of {len(paths)} files.]'
    return listing + '\n\n' + goal


def run_one(job,phase,outdir):
    group,alias,mode,ident,repeat=job
    config=MODELS[alias]
    key=Path(config['key_file']).expanduser().read_text().strip().rstrip('~').strip()
    task=ROOT/'tasks'/ident
    if not key: raise RuntimeError('密钥文件为空')
    run_id=f'{group}-{alias}-{mode}-{ident}-r{repeat}'
    dest=outdir/(run_id+'.jsonl')
    if dest.exists():
        rows=[json.loads(x) for x in dest.read_text().splitlines()]
        if rows and rows[-1]['type']=='summary': return rows[-1]
        raise RuntimeError('存在未结束记录，禁止覆盖: '+str(dest))
    with dest.open('x') as log, tempfile.TemporaryDirectory(prefix='act-experiment-') as td:
        temp=Path(td); fixture=temp/'repo'; shutil.copytree(task/'fixture',fixture)
        def record(row):
            row.update(run_id=run_id,phase=phase,group=group,model=config['model'],alias=alias,mode=mode,task=ident,repeat=repeat)
            serialized=json.dumps(row,ensure_ascii=False)
            if key in serialized: raise RuntimeError('日志中发现密钥，拒绝写入')
            log.write(serialized+'\n'); log.flush()
        record(dict(type='start',time=time.time(),base_url=config['base'],category=json.loads((task/'spec.json').read_text())['category']))
        tool=dict(type='function',function=dict(name='act',description=tool_prompt(group,mode),parameters=dict(type='object',properties=dict(script=dict(type='string',description='Starlark source code.')),required=['script'])))
        messages=[dict(role='system',content=(ROOT/'prompts/system.md').read_text()),dict(role='user',content=user_prompt(task,group))]
        calls=[]; final=''; api_error=None; harness_error=None; response_index=0; total_usage={}
        try:
            while True:
                try:
                    data=call_api(config,key,messages,tool,record)
                    assistant=data['choices'][0]['message']
                    response_index+=1
                    usage=data.get('usage') or {}
                    for name in ('prompt_tokens','completion_tokens','total_tokens'):
                        if name in usage: total_usage[name]=total_usage.get(name,0)+usage[name]
                except RuntimeError as error:
                    api_error=str(error).replace(key,'[REDACTED]'); break
                # 保留 reasoning_content、tool_calls 及供应商返回的所有消息字段。
                messages.append(assistant)
                tool_calls=assistant.get('tool_calls') or []
                if not tool_calls:
                    final=assistant.get('content') or ''; break
                if len(calls)>=MAX_ACT:
                    record(dict(type='budget_stop',requested=len(tool_calls))); break
                for tc in tool_calls:
                    if len(calls)>=MAX_ACT:
                        messages.append(dict(role='tool',tool_call_id=tc['id'],content='act call limit reached (8).'))
                        continue
                    try:
                        args=json.loads(tc['function']['arguments'])
                        script=args['script']
                        if tc['function']['name']!='act' or not isinstance(script,str): raise ValueError('工具名或 script 类型错误')
                    except (KeyError,TypeError,ValueError) as error:
                        script=tc.get('function',{}).get('arguments','')
                        outcome=dict(parsed=False,ran=False,outcome='tool_arguments_error',error=str(error),value=None,events=[],changes=[])
                    else:
                        outcome=run_act(fixture,task,script,mode,temp)
                    row=dict(type='act',call=len(calls)+1,script=script,outcome=outcome,error_category=classify(script,outcome,mode),metrics=act_metrics(script,outcome),response_index=response_index,usage=usage)
                    calls.append(row)
                    row['check']=evaluate(task,files_at(fixture),calls)
                    record(row)
                    visible={k:outcome.get(k) for k in ['outcome','value','error','line']}
                    messages.append(dict(role='tool',tool_call_id=tc['id'],content=json.dumps(visible,ensure_ascii=False)))
        except Exception as error:
            harness_error=str(error).replace(key,'[REDACTED]')
        files=files_at(fixture)
        result=evaluate(task,files,calls,final)
        summary=dict(type='summary',time=time.time(),status='api_error' if api_error else 'harness_error' if harness_error else 'complete',api_error=api_error,harness_error=harness_error,act_count=len(calls),first_write_act=next((c['call'] for c in calls if c['metrics']['category_counts']['write']),None),budget_exhausted=len(calls)==MAX_ACT,usage=total_usage,passed=result['passed'] if not api_error and not harness_error else None,first_act_passed=bool(calls and (calls[0]['check']['passed'] or (len(calls)==1 and result['passed']))) if not api_error and not harness_error else None,reasons=result['reasons'],final=final,final_changes={p:c for p,c in files.items() if files_at(task/'fixture').get(p)!=c},deleted_files=sorted(set(files_at(task/'fixture'))-set(files)),final_hashes={p:hashlib.sha256(c.encode()).hexdigest() for p,c in files.items()})
        record(summary)
        return summary


def main():
    parser=argparse.ArgumentParser(); parser.add_argument('--phase',choices=['pilot','formal'],required=True); parser.add_argument('--workers',type=int,default=4)
    args=parser.parse_args()
    if not 1<=args.workers<=4: parser.error('并发必须为 1 到 4')
    if args.phase=='formal': verify_freeze()
    ids=['read-02','rollback-01'] if args.phase=='pilot' else sorted(p.name for p in (ROOT/'tasks').iterdir() if p.is_dir())
    repeats=1 if args.phase=='pilot' else 3
    jobs=[(group,alias,mode,ident,r) for group in GROUPS for alias in MODELS for mode in ['A','B'] for ident in ids for r in range(1,repeats+1)]
    random.Random(20261112).shuffle(jobs)
    outdir=V2/args.phase; outdir.mkdir(exist_ok=True,parents=True)
    with concurrent.futures.ThreadPoolExecutor(max_workers=args.workers) as deepseek_pool, concurrent.futures.ThreadPoolExecutor(max_workers=1) as minimax_pool:
        futures=[(minimax_pool if job[1]=='minimax' else deepseek_pool).submit(run_one,job,args.phase,outdir) for job in jobs]
        for n,f in enumerate(concurrent.futures.as_completed(futures),1):
            row=f.result(); print(json.dumps(dict(done=n,total=len(jobs),run_id=row['run_id'],status=row['status'],passed=row['passed'],acts=row['act_count']),ensure_ascii=False),flush=True)
    if args.phase=='formal': verify_freeze()


if __name__=='__main__':
    main()
