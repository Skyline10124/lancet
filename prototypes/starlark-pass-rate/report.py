"""只从 v2 冻结输入与原始 JSONL 生成数据表和脚本原文附录。"""
from collections import Counter
import gzip
import json
import time
from driver import ROOT, V2, CATEGORIES, GROUPS, MODELS, verify_freeze
from commands import RULES, normalize


def read_runs(phase, version=2, allow_incomplete=False):
    runs=[]
    root=V2 if version==2 else ROOT/'results'
    for path in sorted((root/phase).glob('*.jsonl*')):
        opener=gzip.open if path.suffix=='.gz' else open
        with opener(path,'rt') as f:
            rows=[json.loads(line) for line in f]
        if not rows or rows[-1]['type']!='summary':
            if allow_incomplete: continue
            raise RuntimeError('未结束的运行: '+str(path))
        runs.append(dict(path=path,rows=rows,summary=rows[-1],calls=[r for r in rows if r['type']=='act']))
    return runs


def pct(n,d):
    return f'{n}/{d} ({100*n/d:.2f}%)' if d else f'{n}/{d} (—)'


def mean(n,d):
    return f'{n}/{d} = {n/d:.4f}' if d else f'{n}/{d} (—)'


def render(runs,pilot):
    frozen=verify_freeze()
    out=[]
    def heading(s): out.extend(['## '+s,''])
    def table(headers,rows):
        out.append('| '+' | '.join(headers)+' |')
        out.append('| '+' | '.join(['---']*len(headers))+' |')
        for row in rows:
            out.append('| '+' | '.join(str(x).replace('|','\\|').replace('\n','<br>') for x in row)+' |')
        out.append('')
    cells=[(g,a,m) for g in GROUPS for a in MODELS for m in 'AB']
    def selected(g,a,m,eligible=True):
        return [r for r in runs if (r['summary']['group'],r['summary']['alias'],r['summary']['mode'])==(g,a,m)
                and (not eligible or r['summary']['status']=='complete')]
    def label(g,a,m): return [g,MODELS[a]['model'],m]
    heading('环境')
    table(['项目','记录'],[
        ['实验','ADR 0002 接受前实验 1 · v2'],
        ['日期（UTC）',time.strftime('%Y-%m-%d',time.gmtime(frozen['time']))],
        ['规模','3 组 × 2 模式 × 2 模型 × 24 任务 × 3 次 = 864；试跑 24 次单列'],
        ['仓库基线',frozen['base_commit']],['系统',frozen['runtime']],['Rust',frozen['rustc']],['Python',frozen['python']],
        ['解释器','starlark = 0.14.2；Dialect::Standard + enable_top_level_stmt=true；enable_f_strings=false；Globals standard + Json'],
        ['二进制 SHA256',frozen['binary_sha256']],
        ['冻结清单',f"results/v2/evidence/freeze.json；输入 {len(frozen['files'])} 文件；测试台 {len(frozen['harness_files'])} 文件"],
        ['冻结 UTC',time.strftime('%Y-%m-%d %H:%M:%S',time.gmtime(frozen['time']))],
        ['请求设置','默认温度，未指定采样参数；MiniMax 并发 1，deepseek 并发 4；请求超时 180 秒，act 子进程超时 60 秒'],
        ['重试','MiniMax 429/5xx/URLError/超时/响应格式错误最多 10 次，等待 1/2/4/8/16/32/64/120/120 秒；deepseek 最多 5 次，等待 1/2/4/8 秒'],
        ['预算','最多 8 次 act；耗尽后再取一条消息供最终汇报，若仍请求工具则停止；无工具调用的回复即结束'],
        ['运行顺序','Random(20261112) 对完整列表 shuffle；每模型独立线程池按其子序列调度'],
        ['返回上限','序列化 JSON 的 UTF-8 长度 ≤ 65536 bytes'],
        ['恢复执行','Codex 用量上限中断后恢复；原驱动继续运行，已核验试跑/冻结；删除不完整记录 0，因中断重跑 0；见 results/v2/evidence/resume-audit.json'],
        ['原始记录','results/v2/formal 与 results/v2/pilot；脚本、API 响应、usage、调用事件、判定、最终文件哈希'],
    ])
    table(['文件','Git blob SHA1'],frozen['prompt_git_blobs'].items())
    table(['组','提示变化'],[
        ['base','修正后的共同基线'],
        ['overview','只在 user 消息开头追加 Repository files: 与排序后的相对文件路径；超过 200 个时显示前 200 个并注明总数'],
        ['composed','仅替换工具说明末尾示例；11 行内读→edit→测试→discard/返回摘要；A/B 仅最后返回写法不同'],
    ])
    modelrows=[]
    for a in MODELS:
        rs=[r for r in runs if r['summary']['alias']==a]
        versions=sorted({x['response'].get('model','未返回') for r in rs for x in r['rows'] if x['type']=='api_response'})
        modelrows.append([MODELS[a]['model'],MODELS[a]['base'],', '.join(versions),len(rs),
                          pct(sum(r['summary']['status']=='api_error' for r in rs),len(rs)),
                          sum(r['summary']['status']=='harness_error' for r in rs)])
    table(['请求模型','base URL','响应 model','运行数','api_error','harness_error'],modelrows)
    table(['组','模型','模式','运行数','complete 分母','api_error','harness_error'],[
        [*label(g,a,m),len(selected(g,a,m,False)),len(selected(g,a,m)),
         *[sum(r['summary']['status']==status for r in selected(g,a,m,False)) for status in ['api_error','harness_error']]]
        for g,a,m in cells])
    times=[r['rows'][0]['time'] for r in runs]
    table(['项目','原始数字'],[
        ['正式记录起始 UTC',time.strftime('%Y-%m-%d %H:%M:%S',time.gmtime(min(times)))],
        ['正式记录结束 UTC',time.strftime('%Y-%m-%d %H:%M:%S',time.gmtime(max(r['summary']['time'] for r in runs)))],
        ['api_retry 记录',sum(row['type']=='api_retry' for r in runs for row in r['rows'])],
        ['有 JSON 回执的正式 act',sum(len(r['calls']) for r in runs)],
        ['api_error 运行中的 act',sum(len(r['calls']) for r in runs if r['summary']['status']=='api_error')],
    ])
    token_rows=[]
    for g,a,m in cells:
        first=[next((x for x in r['rows'] if x['type']=='api_response'),None) for r in selected(g,a,m,False)]
        tokens=[x['response']['usage']['prompt_tokens'] for x in first if x and 'prompt_tokens' in (x['response'].get('usage') or {})]
        token_rows.append([*label(g,a,m),len(tokens),min(tokens) if tokens else '—',max(tokens) if tokens else '—'])
    table(['组','模型','模式','首次响应样本数','总输入 tokens 最小','总输入 tokens 最大'],token_rows)
    table(['统计项','口径'],[
        ['分母','只计 status=complete；api_error 与 harness_error 的整次运行及其 act 排除并单列；重复一次任务为一次运行'],
        ['表 1','解析通过 / act；运行通过 / act；discard 为运行通过，fail 为运行失败'],
        ['表 2','沿用 v1 冻结判定；首次完成按第一次 act 后判定（仅一次 act 且最终回复补齐 fail 汇报也计入）'],
        ['edit 类时序','沿用 v1：必须在较早一次 act 读取目标后再修改；同一次 act 读写即使文件正确也未满足此判定'],
        ['表 4','下一次 act 运行通过 / 存在下一次 act 的失败调用；不要求任务完成；无下一次调用另列'],
        ['表 5–6','每列按任务运行计数，列可重叠；discard/all=True 按宿主事件，fail 按原生错误行定位'],
        ['接口计数','metrics.interfaces/category_counts 为宿主事件进入次数；read/list/search/status/diff=读，write/edit/reset=写，run/pipe=执行，discard/fail 各列；参数绑定前报错没有宿主事件'],
        ['脚本调用点','source_interfaces/source_category_counts 为排除字符串和注释的词法调用点；未执行分支也保留；与实际执行次数分列'],
        ['只读 act','至少一次读接口事件，且无写、执行、discard、fail 事件；env.run(cat/wc 等)归执行；分母为全部有效 act'],
        ['单接口 act','实际事件只涉及一种接口，可多次调用；另记 single_call 表示仅一次接口调用；纯计算/解析失败无事件时两者均否'],
        ['首次写 act','第一次进入 write/edit/reset 所在 act；只对发生写接口事件的任务求均值，无写为 null、不记 0'],
        ['用尽预算','实际执行 8 次 act 即记 true，包括第 8 次后正常完成'],
        ['异常分类限制','沿用 v1 外层异常处理：ConnectionResetError 未进入请求重试，会记为 harness_error；原始错误单列且排除该次分母'],
        ['token','每 act 关联产生其脚本的 API 响应完整 usage；同响应多工具可重复关联；任务总 token 按 API 响应去重，含最终回复/预算终止响应，不估算失败请求 token'],
        ['print','print 不在全局环境；按脚本词法调用点计数（包括导致解析后名称解析失败的调用点），不是动态循环次数；字符串/注释不计'],
    ])
    heading('表 1：每次 act 调用')
    rows=[]
    for g,a,m in cells:
        calls=[c for r in selected(g,a,m) for c in r['calls']]
        rows.append([*label(g,a,m),len(calls),pct(sum(c['outcome']['parsed'] for c in calls),len(calls)),pct(sum(c['outcome']['ran'] for c in calls),len(calls))])
    table(['组','模型','模式','调用数','解析通过率','运行通过率'],rows)
    heading('表 2：每个任务')
    rows=[]; metrics=[]
    for g,a,m in cells:
        rs=selected(g,a,m); n=len(rs); done=sum(r['summary']['passed'] for r in rs); first=sum(r['summary']['first_act_passed'] for r in rs); acts=sum(len(r['calls']) for r in rs)
        readonly=sum(c['metrics']['read_only'] for r in rs for c in r['calls'])
        rows.append([*label(g,a,m),pct(done,n),pct(first,n),mean(acts,n)])
        metrics.append(dict(group=g,model=MODELS[a]['model'],mode=m,n=n,completed=done,first=first,acts=acts,read_only=readonly))
    table(['组','模型','模式','任务完成率','第一次 act 即完成的比例','平均 act 次数'],rows)
    heading('表 3：错误分类')
    rows=[]
    for g,a,m in cells:
        counts=Counter(c['error_category'] for r in selected(g,a,m) for c in r['calls'] if not c['outcome']['ran'])
        rows.append([*label(g,a,m),*[counts[k] for k in CATEGORIES]])
    table(['组','模型','模式',*CATEGORIES],rows)
    heading('表 4：失败后下一次调用改对的比例')
    rows=[]
    for g,a,m in cells:
        for category in CATEGORIES:
            n=passed=last=0
            for r in selected(g,a,m):
                for i,c in enumerate(r['calls']):
                    if c['error_category']!=category: continue
                    if i+1==len(r['calls']): last+=1
                    else:
                        n+=1; passed+=r['calls'][i+1]['outcome']['ran']
            rows.append([*label(g,a,m),category,passed,n,pct(passed,n),last])
    table(['组','模型','模式','错误分类','下次运行通过','存在下一次调用','比例','无下一次调用'],rows)
    heading('表 5：「失败就放弃」任务')
    rows=[]
    for g,a,m in cells:
        rs=[r for r in selected(g,a,m) if r['summary']['task'].startswith('rollback-')]
        discard=fail=wrong=restored=untested=complete=0
        for r in rs:
            calls=r['calls']; events=[e for c in calls for e in c['outcome'].get('events',[])]
            discard+=any(e['op']=='discard' for e in events)
            fail+=any(c['metrics']['category_counts']['fail'] for c in calls)
            changed=bool(r['summary']['final_changes'] or r['summary']['deleted_files'])
            committed=any(c['outcome']['outcome']=='committed' and c['outcome'].get('changes') for c in calls)
            wrong+=bool(changed and committed); restored+=bool(not changed and committed)
            untested+=not any(e['op']=='run' and normalize(e['args']['argv'])==['cargo','test'] for e in events)
            complete+=r['summary']['passed']
        rows.append([*label(g,a,m),len(rs),discard,fail,wrong,restored,untested,complete])
    table(['组','模型','模式','运行数','用 discard','用 fail','正常返回（改动被错误提交）','先提交后恢复','未运行测试','最终完成'],rows)
    heading('表 6：批量替换任务')
    rows=[]
    for g,a,m in cells:
        rs=[r for r in selected(g,a,m) if r['summary']['task'].startswith('batch-')]
        all_true=sum(any(e['op']=='edit' and e['args']['all'] for c in r['calls'] for e in c['outcome'].get('events',[])) for r in rs)
        unique=sum(any(c['error_category']=='唯一匹配失败' for c in r['calls']) for r in rs)
        rows.append([*label(g,a,m),len(rs),all_true,unique,sum(r['summary']['passed'] for r in rs)])
    table(['组','模型','模式','运行数','用 all=True','撞上唯一匹配报错','最终完成'],rows)
    heading('表 7：探索行为')
    rows=[]
    for g,a,m in cells:
        rs=selected(g,a,m); calls=[c for r in rs for c in r['calls']]
        writes=[r['summary']['first_write_act'] for r in rs if r['summary']['first_write_act'] is not None]
        tokens=[r['summary']['usage']['total_tokens'] for r in rs if 'total_tokens' in r['summary']['usage']]
        rows.append([*label(g,a,m),pct(sum(c['metrics']['read_only'] for c in calls),len(calls)),
                     pct(sum(c['metrics']['single_interface'] for c in calls),len(calls)),mean(sum(writes),len(writes)),
                     pct(sum(r['summary']['budget_exhausted'] for r in rs),len(rs)),mean(len(calls),len(rs)),mean(sum(tokens),len(tokens))])
    table(['组','模型','模式','只读 act 占比','单接口 act 占比','首次写操作 act 平均序号','用尽预算任务占比','平均 act 次数','平均总 token'],rows)
    heading('表 8：print')
    rows=[]
    for g,a,m in cells:
        rs=selected(g,a,m)
        rows.append([*label(g,a,m),sum(c['metrics']['print_calls']>0 for r in rs for c in r['calls']),sum(any(c['metrics']['print_calls'] for c in r['calls']) for r in rs),sum(c['metrics']['print_calls'] for r in rs for c in r['calls'])])
    table(['组','模型','模式','调用过 print 的 act 数','调用过 print 的任务数','print 调用点总数'],rows)
    heading('原始数字：分类任务与试跑')
    table(['组','模型','模式','类别','正式运行','完成','首次 act 完成','act 数'],[
        [*label(g,a,m),category,len(rs),sum(r['summary']['passed'] for r in rs),sum(r['summary']['first_act_passed'] for r in rs),sum(len(r['calls']) for r in rs)]
        for g,a,m in cells for category in ['read','edit','batch','test','rollback','probe']
        for rs in [[r for r in selected(g,a,m) if r['summary']['task'].startswith(category+'-')]]])
    table(['试跑 run_id','状态','完成','act 数'],[[r['summary']['run_id'],r['summary']['status'],int(bool(r['summary']['passed'])),len(r['calls'])] for r in pilot])
    heading('测试台修正')
    table(['编号','实际改法','证据'],[
        [1,'system.md 删除引导分步句，未替换为其他建议','prompts/system.md'],
        [2,'预算 4→8；末尾公开最多 8 次；耗尽后行为保持 v1','driver.py；tests/test_v2.py'],
        [3,'8 份 canned 输出首行按实际用例计数（test 类 3、rollback 类 2）；宿主和判定器共享命令归一化表','command-normalization.json；commands.py；act-sim/src/commands.rs'],
        [4,'read-03 同时接受文件名→计数、wc 行数→文件名、嵌套计数对象和记录列表；错误配对/13 代替 3 不通过','6 条历史假阴性回归 + 嵌套/反例；tests/test_v2.py'],
        [5,'MiniMax 独立单线程池，最多 10 次请求；429 指数退避封顶 120 秒；deepseek 独立 4 线程池','driver.py；重试回归；results/v2/formal'],
    ])
    table(['命令归一化项','显式规则'],[
        ['命令前缀',json.dumps(RULES['prefix'])],
        ['cargo 层忽略选项',', '.join(RULES['cargo_ignored_flags'])],
        ['保留并排序的 scope 选项',', '.join(RULES['cargo_scope_flags'])],
        ['cargo 层测试名过滤',f"至多 {RULES['cargo_filter_limit']} 个不以 - 开头的位置参数，忽略过滤值"],
        ['-- 后忽略选项',', '.join(RULES['test_ignored_flags'])],
        ['-- 后调度参数',', '.join(RULES['test_positive_integer_options'])+'；正整数，支持 --key N 与 --key=N'],
        ['-- 后测试名过滤',f"至多 {RULES['test_filter_limit']} 个不以 - 开头的位置参数，忽略过滤值"],
        ['输出',RULES['description']],
        ['表外命令','保留原 argv；先精确预置匹配，否则沿用 v1 白名单；不执行真实 cargo'],
    ])
    audits=json.loads((V2/'evidence/v1-command-audit.json').read_text())
    table(['v1 run_id','act','原 argv','归一化 argv','命中同任务预置'],[
        [r['run_id'],r['call'],json.dumps(r['argv']),json.dumps(r['normalized']),int(r['matches_canned'])] for r in audits])
    heading('与 v1 的差异')
    table(['测试台项','v1','v2'],[
        ['分步引导','system 中要求先读、后次 act 再改','删除该句'],
        ['预算','4，未公开','8，公开'],
        ['canned 测试数','固定 4','与列出的 2/3 用例一致'],
        ['命令匹配','完整 argv','显式选项表归一化；scope 保留'],
        ['read-03 判定','只匹配文件名后紧接数字','补充 wc 反向排列与嵌套计数'],
        ['MiniMax 并发/重试','共享 4 线程；5 次','独立 1 线程；10 次，上限 120 秒'],
        ['提示因素','单组','base/overview/composed；A/B'],
        ['调用观测','部分 repo 事件','补齐 list/status/diff；pipe 在白名单检查前记录；另记词法调用点、分类、只读/单接口、print、usage'],
        ['任务观测','act_count 与通过判定','增加首次写 act、预算耗尽、汇总 usage'],
        ['冻结','prompts/tasks/check.py','同样输入冻结，另记录并核验运行驱动、归一化、指标代码、Rust 源码和二进制哈希'],
        ['数据位置','results/formal、results/pilot、REPORT.md','results/v2/formal、results/v2/pilot、REPORT-v2.md'],
    ])
    heading('验证')
    verification=json.loads((V2/'evidence/validation.json').read_text())
    table(['命令','结果'],[
        ['cargo build --manifest-path prototypes/starlark-pass-rate/Cargo.toml -j 4',(V2/'evidence/build.log').read_text().strip()],
        ['python3 -m unittest discover -s prototypes/starlark-pass-rate/tests',(V2/'evidence/tests.log').read_text().strip()],
        ['python3 prototypes/starlark-pass-rate/language_probe.py','70 条探针记录；results/v2/evidence/language-probes.jsonl'],
        ['python3 prototypes/starlark-pass-rate/driver.py --phase pilot --workers 4',f'{len(pilot)} 条摘要，试跑不计正式分母'],
        ['python3 prototypes/starlark-pass-rate/driver.py --phase formal --workers 4',f'{len(runs)} 条摘要'],
        ['python3 prototypes/starlark-pass-rate/validate.py',json.dumps(verification,ensure_ascii=False)],
        ['cargo xtask docs-check',(V2/'evidence/docs-check.log').read_text().strip() if (V2/'evidence/docs-check.log').exists() else '未执行'],
        ['未覆盖','真实 cargo、沙箱、overlay；模型内部版本及服务端默认采样参数；根 workspace 全量 CI'],
    ])
    diff_audit=json.loads((V2/'evidence/replay-difference-audit.json').read_text())
    table(['返回值差异分类','数量'],diff_audit['counts'].items())
    external=json.loads((V2/'evidence/external-command-audit.json').read_text())
    table(['命令访问范围','原始数字与限制'],[['含绝对/上级路径参数的命令尝试',len(external)],['执行环境','沿用 v1：只设置 fixture 副本为 cwd，未隔离文件系统；部分只读命令枚举 fixture 外目录。原始 argv 与 act 结局见 results/v2/evidence/external-command-audit.json；具体外部文件访问范围未验证']])
    heading('阻塞项')
    blockers=json.loads((V2/'evidence/blockers.json').read_text()) if (V2/'evidence/blockers.json').exists() else []
    rows=[[b['command'],b['error']] for b in blockers]
    for r in runs+pilot:
        s=r['summary']
        if s['status']!='complete':
            raw=next((x.get('error','') for x in reversed(r['rows']) if x['type']=='api_retry'),'')
            rows.append(['driver.py --phase '+s['phase']+' --workers 4 / '+s['run_id'],raw+' '+(s['api_error'] or s['harness_error'] or '')])
    mm=[r for r in runs if r['summary']['alias']=='minimax']; errors=sum(r['summary']['status']=='api_error' for r in mm)
    if mm and errors/len(mm)>.05:
        rows.append(['MiniMax api_error 目标 ≤5%',pct(errors,len(mm))+'；原始错误见本表逐条记录'])
    table(['命令 / run_id','原始报错'],rows or [['无','0']])
    missing_path=V2/'evidence/unrecorded-tool-calls.json'
    missing=json.loads(missing_path.read_text()) if missing_path.exists() else []
    heading('原始数字：未收到 act 回执的调用')
    table(['run_id','API 响应序号','调用点','usage','原始报错'],[
        [x['run_id'],x['response_index'],json.dumps(x['source_interfaces']),json.dumps(x['usage']),x['error']] for x in missing] or [['无','0','0','0','0']])
    table(['未覆盖项','数量与原因'],[['缺少动态接口事件',f'{len(missing)} 次调用；act-sim 超时没有 JSON 回执，脚本和 token 来自原始 API 响应；整次运行排除统计分母']])
    for x in missing:
        out.extend(['````python',x['script'],'````',''])
    heading('附录：每个错误分类各附 3 个脚本原文')
    for category in CATEGORIES:
        calls=[c for r in runs if r['summary']['status']=='complete' for c in r['calls'] if c['error_category']==category]
        out.extend(['### '+category,''])
        table(['正式错误调用总数','附录脚本数'],[[len(calls),min(3,len(calls))]])
        for c in calls[:3]:
            table(['组','run_id','act','原始错误'],[[c['group'],c['run_id'],c['call'],c['outcome']['error']]])
            out.extend(['````python',c['script'],'````',''])
    (ROOT/'REPORT-v2.md').write_text('\n'.join(out))
    (V2/'evidence/metrics.json').write_text(json.dumps(metrics,ensure_ascii=False,indent=2)+'\n')
    return metrics


if __name__=='__main__':
    print(json.dumps(render(read_runs('formal'),read_runs('pilot')),ensure_ascii=False,indent=2))
