"""只从冻结输入与 JSONL 原始记录生成数据表和脚本附录。"""
from collections import Counter
import gzip
import hashlib
import json
from pathlib import Path
import re
import sys
import time
ROOT=Path(__file__).resolve().parent
sys.path.insert(0,str(ROOT))
from driver import CATEGORIES, MODELS, verify_freeze


def read_runs(phase):
    runs=[]
    for path in sorted((ROOT/'results'/phase).glob('*.jsonl*')):
        opener=gzip.open if path.suffix=='.gz' else open
        with opener(path,'rt') as f: rows=[json.loads(line) for line in f]
        if not rows or rows[-1]['type']!='summary':
            raise RuntimeError('未结束的运行: '+str(path))
        runs.append(dict(path=path,rows=rows,summary=rows[-1],calls=[r for r in rows if r['type']=='act']))
    return runs


def pct(n,d):
    return f'{n}/{d} ({100*n/d:.2f}%)' if d else f'{n}/{d} (—)'


def render(runs,pilot):
    frozen=verify_freeze()
    out=[]
    def heading(s): out.extend(['## '+s,''])
    def table(headers,rows):
        out.append('| '+' | '.join(headers)+' |')
        out.append('| '+' | '.join(['---']*len(headers))+' |')
        for row in rows: out.append('| '+' | '.join(str(x).replace('|','\\|').replace('\n','<br>') for x in row)+' |')
        out.append('')
    groups=[(alias,mode) for alias in MODELS for mode in ['A','B']]
    group=lambda a,m:[r for r in runs if r['summary']['alias']==a and r['summary']['mode']==m]
    eligible=lambda a,m:[r for r in group(a,m) if r['summary']['status']=='complete']
    heading('环境')
    table(['项目','记录'],[
        ['实验','ADR 0002 接受前实验 1'],['日期（UTC）',time.strftime('%Y-%m-%d',time.gmtime(frozen['time']))],
        ['运行范围','24 任务 × 2 模式 × 2 模型 × 3 次 = 288；试跑 8 次单列'],
        ['仓库基线',frozen['base_commit']],['系统',frozen['runtime']],['Rust',frozen['rustc']],['Python',frozen['python'].splitlines()[0]],
        ['解释器','starlark = 0.14.2；Dialect::Standard + enable_top_level_stmt=true；enable_f_strings=false；Globals standard + Json'],
        ['二进制 SHA256',frozen['binary_sha256']],['冻结清单','evidence/freeze.json（151 个文件；SHA256）'],
        ['请求设置','默认温度；未指定采样参数；并发 4；每任务最多 4 次 act；429/5xx 最多 5 次尝试，等待 1/2/4/8 秒'],
        ['运行顺序','Random(20261112) 对完整运行列表 shuffle'],
        ['返回上限','序列化 JSON 的 UTF-8 长度 ≤ 65536 bytes'],
    ])
    table(['文件','Git blob SHA1'],frozen['prompt_git_blobs'].items())
    token_evidence=[]
    for alias,mode in groups:
        first=[next((row for row in r['rows'] if row['type']=='api_response'),None) for r in group(alias,mode)]
        tokens=[r['response']['usage']['prompt_tokens'] for r in first if r and 'prompt_tokens' in r['response'].get('usage',{})]
        token_evidence.append(dict(model=MODELS[alias]['model'],mode=mode,samples=len(tokens),min_prompt_tokens=min(tokens) if tokens else None,max_prompt_tokens=max(tokens) if tokens else None))
    (ROOT/'evidence/prompt-tokens.json').write_text(json.dumps({'source':'首次 API 响应 usage.prompt_tokens；包括系统提示、任务和完整工具 schema','groups':token_evidence},ensure_ascii=False,indent=2)+'\n')
    table(['模型','模式','首次响应样本数','总输入 tokens 最小','总输入 tokens 最大'],[[r[k] for k in ['model','mode','samples','min_prompt_tokens','max_prompt_tokens']] for r in token_evidence])
    table(['项目','原始数字'],[['正式记录起始 UTC',time.strftime('%Y-%m-%d %H:%M:%S',time.gmtime(min(r['rows'][0]['time'] for r in runs)))],['正式记录结束 UTC',time.strftime('%Y-%m-%d %H:%M:%S',time.gmtime(max(r['summary']['time'] for r in runs)))],['api_retry 记录数',sum(row['type']=='api_retry' for r in runs for row in r['rows'])],['全部正式 act 调用数',sum(len(r['calls']) for r in runs)],['api_error 运行中的 act 调用数',sum(len(r['calls']) for r in runs if r['summary']['status']=='api_error')]])
    modelrows=[]
    for alias in MODELS:
        selected=[r for r in runs if r['summary']['alias']==alias]
        versions=sorted({row['response'].get('model','未返回') for r in selected for row in r['rows'] if row['type']=='api_response'})
        modelrows.append([MODELS[alias]['model'],MODELS[alias]['base'],', '.join(versions),len(selected),sum(r['summary']['status']=='api_error' for r in selected),sum(r['summary']['status']=='harness_error' for r in selected)])
    table(['请求模型','base URL','响应 model 字段','正式运行数','api_error','harness_error'],modelrows)
    table(['统计项','口径'],[
        ['正式通过率分母','只包含 status=complete 的正式运行；api_error 整次运行及其 act 从通过率、均值和错误统计排除'],
        ['表 1','解析通过 / act 调用数；运行通过 / act 调用数。discard 为运行通过；fail 为运行失败'],
        ['表 2','每个任务重复一次为一次运行；最终文件与指定事实均符合冻结 check.py 才完成；首次完成按第一次 act 后的判定（仅一次 act 且最终回复补齐 fail 汇报也计入）'],
        ['表 4','下一次 act 的运行通过 / 存在下一次 act 的失败调用；最后一次失败无下一次调用单列；不要求任务完成'],
        ['表 5','按任务运行计数；各列可重叠；discard 按执行事件，fail 按失败脚本与 fail 调用行的错误定位；正常提交错误按最终文件发生变化'],
        ['表 6','按任务运行计数；all=True 按执行事件（包括尝试）；唯一匹配报错按原始错误；最终完成按 check.py'],
        ['原始记录','results/formal 与 results/pilot；每次 API 响应、act 脚本/结局/判定及最终摘要；不含密钥'],
    ])
    heading('表 1：每次 act 调用（按 模型 × 模式）')
    rows=[]
    for a,m in groups:
        calls=[c for r in eligible(a,m) for c in r['calls']]
        rows.append([MODELS[a]['model'],m,len(calls),pct(sum(c['outcome']['parsed'] for c in calls),len(calls)),pct(sum(c['outcome']['ran'] for c in calls),len(calls))])
    table(['模型','模式','调用数','解析通过率','运行通过率'],rows)
    heading('表 2：每个任务（按 模型 × 模式）')
    rows=[]; metrics=[]
    for a,m in groups:
        rs=eligible(a,m); n=len(rs); done=sum(r['summary']['passed'] for r in rs); first=sum(r['summary']['first_act_passed'] for r in rs); acts=sum(r['summary']['act_count'] for r in rs)
        rows.append([MODELS[a]['model'],m,pct(done,n),pct(first,n),f'{acts}/{n} = {acts/n:.4f}' if n else '—'])
        metrics.append(dict(model=MODELS[a]['model'],mode=m,n=n,completed=done,first=first,acts=acts))
    table(['模型','模式','任务完成率','第一次 act 即完成的比例','平均 act 次数'],rows)
    heading('表 3：错误分类（按 模型 × 模式）')
    rows=[]
    for a,m in groups:
        counts=Counter(c['error_category'] for r in eligible(a,m) for c in r['calls'] if not c['outcome']['ran'])
        rows.append([MODELS[a]['model'],m,*[counts[k] for k in CATEGORIES]])
    table(['模型','模式',*CATEGORIES],rows)
    heading('表 4：失败后下一次调用改对的比例（按错误分类）')
    rows=[]
    for category in CATEGORIES:
        n=passed=last=0
        for r in runs:
            if r['summary']['status']!='complete': continue
            for i,c in enumerate(r['calls']):
                if c['error_category']!=category: continue
                if i+1==len(r['calls']): last+=1
                else:
                    n+=1; passed+=r['calls'][i+1]['outcome']['ran']
        rows.append([category,passed,n,pct(passed,n),last])
    table(['错误分类','下次运行通过','存在下一次调用','比例','无下一次调用'],rows)
    heading('表 5：「失败就放弃」任务')
    rows=[]
    for a,m in groups:
        rs=[r for r in eligible(a,m) if r['summary']['task'].startswith('rollback-')]
        discard=fail=wrong=restored=untested=complete=0
        for r in rs:
            calls=r['calls']; events=[e for c in calls for e in c['outcome'].get('events',[])]
            discard+=any(e['op']=='discard' for e in events)
            failed=False
            for c in calls:
                line=c['outcome'].get('line'); lines=c['script'].splitlines()
                if not c['outcome']['ran'] and line and line<=len(lines) and re.search(r'\bfail\s*\(',lines[line-1]): failed=True
            fail+=failed
            changed=bool(r['summary']['final_changes'] or r['summary']['deleted_files'])
            wrong+=changed and any(c['outcome']['outcome']=='committed' and c['outcome'].get('changes') for c in calls)
            restored+=not changed and any(c['outcome']['outcome']=='committed' and c['outcome'].get('changes') for c in calls)
            untested+=not any(e['op']=='run' and e['args']['argv']==['cargo','test'] for e in events)
            complete+=r['summary']['passed']
        rows.append([MODELS[a]['model'],m,len(rs),discard,fail,wrong,restored,untested,complete])
    table(['模型','模式','运行数','用 discard','用 fail','正常返回（改动被错误提交）','先提交后恢复','未运行测试','最终完成'],rows)
    heading('表 6：批量替换任务')
    rows=[]
    for a,m in groups:
        rs=[r for r in eligible(a,m) if r['summary']['task'].startswith('batch-')]
        all_true=sum(any(e['op']=='edit' and e['args']['all'] for c in r['calls'] for e in c['outcome'].get('events',[])) for r in rs)
        unique=sum(any(c['error_category']=='唯一匹配失败' for c in r['calls']) for r in rs)
        rows.append([MODELS[a]['model'],m,len(rs),all_true,unique,sum(r['summary']['passed'] for r in rs)])
    table(['模型','模式','运行数','用 all=True','撞上唯一匹配报错','最终完成'],rows)
    heading('原始数字：冻结判定器限制')
    audit_path=ROOT/'evidence/checker-audit.json'
    audits=json.loads(audit_path.read_text()) if audit_path.exists() else []
    table(['冻结检查','已知限制','处理'],[['read-03 文件名与行数配对','只匹配文件名后紧接行数；wc -l 的反向排列及嵌套计数对象会造成假阴性','保持 check.py 和冻结统计；受影响原始记录单列']])
    table(['run_id','冻结判定','正确 wc 输出的 act','原始输出'],[[r['run_id'],int(r['frozen_passed']),','.join(str(e['call']) for e in r['wc_evidence']),r['wc_evidence'][0]['value']] for r in audits])
    heading('原始数字：分类任务与试跑')
    rows=[]
    for a,m in groups:
        for category in ['read','edit','batch','test','rollback','probe']:
            rs=[r for r in eligible(a,m) if r['summary']['task'].startswith(category+'-')]
            rows.append([MODELS[a]['model'],m,category,len(rs),sum(r['summary']['passed'] for r in rs),sum(r['summary']['first_act_passed'] for r in rs),sum(r['summary']['act_count'] for r in rs)])
    table(['模型','模式','类别','正式运行','完成','首次 act 完成','act 数'],rows)
    table(['试跑 run_id','状态','完成','act 数'],[[r['summary']['run_id'],r['summary']['status'],int(bool(r['summary']['passed'])),r['summary']['act_count']] for r in pilot])
    heading('提示词事实修正')
    extraction=json.loads((ROOT/'evidence/prompt-extraction.json').read_text())
    probes=[json.loads(l) for l in (ROOT/'evidence/language-probes.jsonl').read_text().splitlines()]
    table(['原文','修正','实测依据'],[[x['old'],x['new'],'evidence/language-probes.jsonl: '+x['probe']+'（A/B）'] for x in extraction['fixes']])
    table(['探针','A：解析 / 运行','B：解析 / 运行','结果或原始错误首行'],[[name,*[' / '.join(str(int(next(p for p in probes if p['probe']==name and p['mode']==mode)[k])) for k in ['parsed','ran']) for mode in ['A','B']],json.dumps(next(p for p in probes if p['probe']==name and p['mode']=='B')['value'],ensure_ascii=False) if next(p for p in probes if p['probe']==name and p['mode']=='B')['ran'] else next(p for p in probes if p['probe']==name and p['mode']=='B')['error'].splitlines()[0]] for name in dict.fromkeys(p['probe'] for p in probes)])
    heading('测试台与方案的偏差')
    table(['项','实际行为'],[
        ['句柄','按覆盖指令使用 repo.*；无 lancet 全局'],
        ['第 0 步','取消人工停顿；语义自动化测试、48 个任务参考用例与试跑后冻结'],
        ['返回值读取','A 保持指定 def/赋值包装；eval 完成后单独求值私有 __act_result__ 以读取返回值；B 的值保存在 Module.extra_value'],
        ['reset/status/diff 基线','按提示词草稿以本次 act 开始的文件状态为基线；方案接口表写“任务初始状态”'],
        ['text','保留自定义 text(s) 与链式方法；过滤结果以换行连接，不保留末尾换行'],
        ['命令输出','预置 canned.json 按 argv 精确匹配且不随文件变化；真实管道仅返回最后阶段的 code/stdout/stderr'],
        ['预置日志计数','canned.stdout 首行固定 running 4 tests，后续实际列出的用例为 2 或 3；任务判定使用明确的失败名和原因，不使用 running 数字'],
        ['提示词示例','按草稿，B 示例去掉 A 的“无结果提前 return”分支；除返回段和示例外 A/B 正文相同'],
        ['提示词顺序与长度','按用户指定草稿保留 Outcome→Returning→Repository→Commands→Text→语言差异→Example；长度以首次 API 响应 usage.prompt_tokens 核验，含 system/任务/工具 schema 的总输入小于 2500；见 evidence/prompt-tokens.json'],
        ['密钥配置','按覆盖指令从两个密钥文件运行时读取，strip 后 rstrip("~")；未使用 SUBJECT_API_KEY 环境变量；文件未修改'],
        ['终止条件','无工具调用的模型回复即结束；最多 4 次 act 后再取一条模型消息供最终汇报，若仍请求工具则停止；API 请求超时 180 秒，act 子进程超时 60 秒'],
        ['tool_calls','同一条 assistant 的多个工具调用顺序执行，计入同一任务的 4 次上限；超出部分仅回传预算耗尽'],
        ['任务判定','check.py 对文件做全文精确比较，对事实做固定词/数字检查；两个 JSON 任务额外核验结构、排序、数字及布尔；edit 类验证先前 act 读取时序；rollback 类核验测试时的候选文件'],
        ['试跑前后修复','修复 A 私有变量读取及堆栈行号换算；参考判定的 rollback 常量替换改为 = n;，避免把 u32 的 2 一并改掉；未改任务正文或提示词来适配模型'],
        ['范围','未实现 http.fetch、沙箱、overlay、内存窗口；仅在临时 fixture 副本执行命令'],
    ])
    heading('验证')
    verification=json.loads((ROOT/'evidence/validation.json').read_text()) if (ROOT/'evidence/validation.json').exists() else {}
    table(['命令','结果'],[
        ['cargo xtask docs-check','通过；evidence/docs-check.log（最终暂存后执行）'],
        ['cargo build --manifest-path prototypes/starlark-pass-rate/Cargo.toml -j 4','通过；evidence/build-final.log'],
        ['python3 -m unittest discover -s prototypes/starlark-pass-rate/tests','9 个测试通过；48 个参考用例；evidence/tests.log'],
        ['python3 prototypes/starlark-pass-rate/language_probe.py','70 条探针记录'],
        ['python3 prototypes/starlark-pass-rate/driver.py --phase pilot --workers 4',f'{len(pilot)} 条运行摘要'],
        ['python3 prototypes/starlark-pass-rate/driver.py --phase formal --workers 4',f'{len(runs)} 条运行摘要'],
        ['python3 prototypes/starlark-pass-rate/validate.py',json.dumps(verification,ensure_ascii=False)],
        ['返回值差异核验','7 处均仅为真实 ls 的时间戳及父目录链接计数；evidence/replay-value-differences.json 与 replay-difference-audit.json'],
        ['未覆盖','真实命令沙箱与真实 cargo test；正式模型的内部版本与服务端采样默认值无法锁定；未运行根工作区全量 CI（原型独立 workspace）'],
    ])
    heading('阻塞项')
    blockers=json.loads((ROOT/'evidence/blockers.json').read_text()) if (ROOT/'evidence/blockers.json').exists() else []
    apirows=[r for r in runs if r['summary']['status']!='complete']
    table(['命令 / run_id','原始报错'],[[b['command'],b['error']] for b in blockers]+[['driver.py --phase formal --workers 4 / '+r['summary']['run_id'],(next((row.get('error','') for row in reversed(r['rows']) if row['type']=='api_retry'), '')+' '+(r['summary']['api_error'] or r['summary']['harness_error'] or ''))] for r in apirows] or [['无','0']])
    heading('附录：每个错误分类各附 3 个脚本原文')
    for category in CATEGORIES:
        calls=[c for r in runs if r['summary']['status']=='complete' for c in r['calls'] if c['error_category']==category]
        out.extend(['### '+category,''])
        table(['正式错误调用总数','附录脚本数'],[[len(calls),min(3,len(calls))]])
        for c in calls[:3]:
            table(['run_id','act','原始错误'],[[c['run_id'],c['call'],c['outcome']['error']]])
            out.extend(['````python',c['script'],'````',''])
    (ROOT/'REPORT.md').write_text('\n'.join(out))
    (ROOT/'evidence/metrics.json').write_text(json.dumps(metrics,ensure_ascii=False,indent=2)+'\n')
    return metrics


if __name__=='__main__':
    print(json.dumps(render(read_runs('formal'),read_runs('pilot')),ensure_ascii=False,indent=2))
