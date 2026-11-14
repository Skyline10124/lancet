"""从 v3 原始事件重算提交、撤销、补救与 v2 对照；不改历史报告。"""
from collections import Counter
import json
import hashlib
from pathlib import Path
from driver import ROOT, V3, CATEGORIES, verify_freeze
from check import files_at, evaluate
from commands import normalize

KEEP = {'edit', 'batch', 'test'}
READ = {'read', 'probe'}


def read_runs(phase, root=V3, allow_incomplete=False):
    runs=[]
    for path in sorted((root/phase).glob('*.jsonl')):
        rows=[json.loads(s) for s in path.read_text().splitlines()]
        if not rows or rows[-1]['type']!='summary':
            if allow_incomplete:
                continue
            raise RuntimeError('记录尚未结束: '+str(path))
        runs.append(dict(path=path,rows=rows,summary=rows[-1],calls=[r for r in rows if r['type']=='act']))
    return runs


def ratio(n,d):
    return dict(n=n,d=d,pct=round(100*n/d,4) if d else None)


def fmt(v):
    return f"{v['n']}/{v['d']} ({v['pct']:.2f}%)" if v['d'] else f"{v['n']}/0（不可估计）"


def category(run):
    return run['summary']['task'].split('-')[0]


def committed(call):
    return call['outcome'].get('outcome')=='committed'


def reverted(call):
    return call['outcome'].get('outcome')=='uncommitted_reverted'


def failed_candidate_test(call, spec):
    events=call['outcome'].get('events',[])
    path,old,new=spec['replacement']
    candidate=spec['expected'][path].replace(old,new)
    for i,e in enumerate(events):
        if e['op']=='run' and normalize(e['args']['argv'])==normalize(spec['command']) and e['args'].get('files',{}).get(path)==candidate:
            if i+1<len(events) and events[i+1]['op']=='command_result' and events[i+1]['args']['code']!=0:
                return True
    return False


def misuse(call, previous=None):
    events=call['outcome'].get('events',[])
    commits=[i for i,e in enumerate(events) if e['op']=='commit']
    flags=[]
    if previous and reverted(previous) and commits and not call['outcome'].get('changes'):
        flags.append('撤销后空提交（未重做修改）')
    if commits and not call['outcome']['ran']:
        flags.append('commit 后失败')
    if any(e['op']=='commit' and e['args']['changed_files']==0 and any(x['op'] in ('write','edit') for x in events[i+1:]) for i,e in enumerate(events)):
        flags.append('写入之前 commit')
    if any(e['op']=='command_result' and e['args']['code']!=0 and any(j>i for j in commits) for i,e in enumerate(events)):
        flags.append('测试失败之后 commit')
    return flags


def recovery(runs, correct_only=False):
    notices=[]
    for r in runs:
        for i,c in enumerate(r['calls']):
            if not reverted(c) or (correct_only and not c['candidate_files_correct']):
                continue
            nxt=r['calls'][i+1] if i+1<len(r['calls']) else None
            seen=bool(nxt and nxt['response_index']>c['response_index'])
            notices.append(dict(run_id=r['summary']['run_id'],call=c['call'],category=category(r),
                                next_exists=nxt is not None,next_saw_notice=seen,
                                next_rewrite_commit=bool(seen and committed(nxt)),
                                next_correct_commit=bool(seen and committed(nxt) and nxt['candidate_files_correct'])))
    return dict(notices=notices,next_rewrite_commit_all=ratio(sum(x['next_rewrite_commit'] for x in notices),len(notices)),
                next_rewrite_commit_seen=ratio(sum(x['next_rewrite_commit'] for x in notices),sum(x['next_saw_notice'] for x in notices)),
                no_next=sum(not x['next_exists'] for x in notices),
                same_response=sum(x['next_exists'] and not x['next_saw_notice'] for x in notices))


def final_files_correct(run):
    expected=json.loads((ROOT/'tasks'/run['summary']['task']/'spec.json').read_text())['expected']
    return run['summary']['final_hashes']=={p:hashlib.sha256(v.encode()).hexdigest() for p,v in expected.items()}


def aggregate(runs):
    valid=[r for r in runs if r['summary']['status']=='complete']
    calls=[c for r in valid for c in r['calls']]
    keep=[r for r in valid if category(r) in KEEP]
    keep_calls=[c for r in keep for c in r['calls']]
    changes=[c for c in keep_calls if c['outcome'].get('changes')]
    opportunities=[c for c in changes if c['outcome']['ran']]
    correct_changes=[c for c in opportunities if c['candidate_files_correct']]
    missed=[c for c in correct_changes if reverted(c)]
    missed_runs=[r for r in keep if any(reverted(c) and c['candidate_files_correct'] for c in r['calls'])]
    rollback=[r for r in valid if category(r)=='rollback']
    rollback_pairs=[]
    for r in rollback:
        spec=json.loads((ROOT/'tasks'/r['summary']['task']/'spec.json').read_text())
        rollback_pairs.extend((r,c) for c in r['calls'] if failed_candidate_test(c,spec))
    read=[r for r in valid if category(r) in READ]
    read_calls=[c for r in read for c in r['calls']]
    commit_events=lambda cs: sum(sum(e['op']=='commit' for e in c['outcome'].get('events',[])) for c in cs)
    rollback_seen={r['summary']['run_id'] for r,c in rollback_pairs}
    wrong_runs={r['summary']['run_id'] for r,c in rollback_pairs if c['outcome'].get('commit_called')}
    misuse_checks=[(r,i,c,misuse(c,r['calls'][i-1] if i else None)) for r in valid for i,c in enumerate(r['calls'])]
    misuse_cases=[dict(run_id=r['summary']['run_id'],call=c['call'],flags=flags) for r,i,c,flags in misuse_checks if flags]
    return dict(planned=len(runs),valid=len(valid),statuses=dict(Counter(r['summary']['status'] for r in runs)),
        completion=ratio(sum(r['summary']['passed'] for r in valid),len(valid)),
        first_completion=ratio(sum(r['summary']['first_act_passed'] for r in valid),len(valid)),
        acts=len(calls),parse=ratio(sum(c['outcome']['parsed'] for c in calls),len(calls)),execution=ratio(sum(c['outcome']['ran'] for c in calls),len(calls)),
        categories={k:ratio(sum(r['summary']['passed'] for r in valid if category(r)==k),sum(category(r)==k for r in valid)) for k in ['read','edit','batch','test','rollback','probe']},
        keep_completion=ratio(sum(r['summary']['passed'] for r in keep),len(keep)),
        required_write_completion=ratio(sum(r['summary']['passed'] for r in keep if category(r) in {'edit','batch'}),sum(category(r) in {'edit','batch'} for r in keep)),
        keep_acts=len(keep_calls),changed_acts=len(changes),normal_changed_acts=len(opportunities),correct_changed_acts=len(correct_changes),
        missed_correct_acts=ratio(len(missed),len(correct_changes)),missed_correct_of_all_keep_acts=ratio(len(missed),len(keep_calls)),
        missed_correct_runs=ratio(len(missed_runs),len(keep)),missed_write_runs=ratio(len(missed_runs),sum(category(r) in {'edit','batch'} for r in keep)),
        missed_strict_acts=sum(c['candidate_check']['passed'] for c in missed),
        rescued_runs=ratio(sum(r['summary']['passed'] for r in missed_runs),len(missed_runs)),
        rescued_files_runs=ratio(sum(final_files_correct(r) for r in missed_runs),len(missed_runs)),
        omissions=[dict(run_id=r['summary']['run_id'],call=c['call'],candidate_check=c['candidate_check'],final_passed=r['summary']['passed']) for r in missed_runs for c in r['calls'] if reverted(c) and c['candidate_files_correct']],
        all_keep_reverts=sum(reverted(c) for c in keep_calls),
        rollback_failed_test_acts=len(rollback_pairs),rollback_wrong_commit_acts=ratio(sum(bool(c['outcome'].get('commit_called')) for _,c in rollback_pairs),len(rollback_pairs)),
        rollback_commit_after_failure_acts=ratio(sum('测试失败之后 commit' in misuse(c) for _,c in rollback_pairs),len(rollback_pairs)),
        rollback_correct_noncommit_acts=ratio(sum(not c['outcome'].get('commit_called') for _,c in rollback_pairs),len(rollback_pairs)),
        rollback_normal_noncommit_acts=ratio(sum(not c['outcome'].get('commit_called') and c['outcome']['ran'] for _,c in rollback_pairs),len(rollback_pairs)),
        rollback_error_noncommit_acts=ratio(sum(not c['outcome'].get('commit_called') and not c['outcome']['ran'] for _,c in rollback_pairs),len(rollback_pairs)),
        rollback_wrong_commit_runs=ratio(len(wrong_runs),len(rollback_seen)),rollback_correct_noncommit_runs=ratio(len(rollback_seen-wrong_runs),len(rollback_seen)),
        rollback_bad_persist_acts=ratio(sum(committed(c) for _,c in rollback_pairs),len(rollback_pairs)),
        rollback_unobserved_runs=len(rollback)-len(rollback_seen),
        omission_recovery=recovery(keep, correct_only=True),keep_recovery=recovery(keep),all_recovery=recovery(valid),
        read_commit_calls=commit_events(read_calls),read_commit_acts=ratio(sum(c['outcome'].get('commit_called',False) for c in read_calls),len(read_calls)),
        read_commit_runs=ratio(sum(any(c['outcome'].get('commit_called') for c in r['calls']) for r in read),len(read)),
        read_completion=ratio(sum(r['summary']['passed'] for r in read),len(read)),
        errors=dict(Counter(c['error_category'] for c in calls if c['error_category'])),
        error_recovery={k:ratio(sum(i+1<len(r['calls']) and r['calls'][i+1]['outcome']['ran'] for r in valid for i,c in enumerate(r['calls']) if c['error_category']==k),sum(i+1<len(r['calls']) for r in valid for i,c in enumerate(r['calls']) if c['error_category']==k)) for k in CATEGORIES},
        error_no_next={k:sum(i+1==len(r['calls']) for r in valid for i,c in enumerate(r['calls']) if c['error_category']==k) for k in CATEGORIES},
        misuse=misuse_cases,
        misuse_recovery=ratio(sum(i+1<len(r['calls']) and r['calls'][i+1]['outcome']['ran'] for r,i,c,flags in misuse_checks if flags),sum(i+1<len(r['calls']) for r,i,c,flags in misuse_checks if flags)),
        misuse_no_next=sum(i+1==len(r['calls']) for r,i,c,flags in misuse_checks if flags),
        usage={k:sum(r['summary']['usage'].get(k,0) for r in runs) for k in ['prompt_tokens','completion_tokens','total_tokens']},
        response_cost=sum((row['response'].get('usage') or {}).get('cost') or 0 for r in runs for row in r['rows'] if row['type']=='api_response'),
        providers=dict(Counter(str(row.get('actual_provider')) for r in runs for row in r['rows'] if row['type']=='api_response')),
        returned_models=dict(Counter(str(row.get('actual_model')) for r in runs for row in r['rows'] if row['type']=='api_response')))


def legacy_metrics(runs, alias, mode=None):
    selected=[r for r in runs if r['summary']['group']=='composed' and r['summary']['alias']==alias and (mode is None or r['summary']['mode']==mode)]
    valid=[r for r in selected if r['summary']['status']=='complete']
    read=[r for r in valid if category(r) in READ]
    rollback=[r for r in valid if category(r)=='rollback']
    return dict(planned=len(selected),valid=len(valid),completion=ratio(sum(r['summary']['passed'] for r in valid),len(valid)),
                read_completion=ratio(sum(r['summary']['passed'] for r in read),len(read)),
                rollback_completion=ratio(sum(r['summary']['passed'] for r in rollback),len(rollback)),
                statuses=dict(Counter(r['summary']['status'] for r in selected)))


def main():
    frozen=verify_freeze()
    runs=read_runs('formal'); pilot=read_runs('pilot')
    legacy=read_runs('formal',ROOT/'results/v2')
    aliases=['deepseek','minimax']
    labels={'deepseek':'DeepSeek（OpenRouter）','minimax':'MiniMax（直连）','all':'合计'}
    metrics={a:aggregate([r for r in runs if r['summary']['alias']==a]) for a in aliases}
    metrics['all']=aggregate(runs)
    pilot_metrics={a:aggregate([r for r in pilot if r['summary']['alias']==a]) for a in aliases}
    old={a:{mode:legacy_metrics(legacy,a,None if mode=='AB' else mode) for mode in ['A','AB']} for a in aliases}
    old['all']={}
    for mode in ['A','AB']:
        old['all'][mode]={k:ratio(sum(old[a][mode][k]['n'] for a in aliases),sum(old[a][mode][k]['d'] for a in aliases)) for k in ['completion','read_completion','rollback_completion']}
    start=json.loads((V3/'evidence/usage-start.json').read_text())
    end_path=V3/'evidence/usage-end.json'
    end=json.loads(end_path.read_text()) if end_path.exists() else None
    preflight=json.loads((V3/'evidence/minimax-preflight.json').read_text())
    archive=V3/'attempt-1'
    interrupted=json.loads((archive/'evidence/interruption.json').read_text())
    archived_responses=[];archived_retries=[];archived_summaries=[]
    for phase in ['pilot','formal']:
        for path in (archive/phase).glob('*.jsonl'):
            old_rows=[json.loads(line) for line in path.read_text().splitlines()]
            archived_responses.extend(row for row in old_rows if row['type']=='api_response')
            archived_retries.extend(row for row in old_rows if row['type']=='api_retry')
            if old_rows[-1]['type']=='summary':archived_summaries.append(old_rows[-1])
    archived_cost=sum((row['response'].get('usage') or {}).get('cost') or 0 for row in archived_responses)
    archived_tokens={k:sum((row['response'].get('usage') or {}).get(k,0) for row in archived_responses if row['alias']=='minimax') for k in ['prompt_tokens','completion_tokens','total_tokens']}
    costs=dict(start=start,end=end,delta=end['usage']-start['usage'] if end else None,
               openrouter_response_cost=archived_cost+sum(m['response_cost'] for m in [metrics['deepseek'],pilot_metrics['deepseek']]),
               minimax_tokens={k:metrics['minimax']['usage'][k]+pilot_metrics['minimax']['usage'][k]+preflight['response']['usage'].get(k,0)+archived_tokens[k] for k in metrics['minimax']['usage']})
    data=dict(attempt1=dict(interruption=interrupted,openrouter_cost=archived_cost,minimax_usage=archived_tokens,retries=archived_retries,summaries=archived_summaries),formal=metrics,pilot=pilot_metrics,v2_composed=old,costs=costs)
    (V3/'evidence/metrics.json').write_text(json.dumps(data,ensure_ascii=False,indent=2)+'\n')
    out=['# 实验 1 v3：显式 `commit()` 与撤销提示','']
    def text(s):out.extend([s,''])
    def heading(s):text('## '+s)
    def table(headers, rows):
        out.extend(['| '+' | '.join(headers)+' |','| '+' | '.join(['---']*len(headers))+' |'])
        for row in rows:
            out.append('| '+' | '.join(str(v).replace('|','\\|').replace('\n','<br>') for v in row)+' |')
        out.append('')
    allm=metrics['all']
    heading('结论')
    text(f"正式运行 {len(runs)}/144 次，状态 complete {allm['valid']}/{len(runs)}；沿用 v2 判定器的任务完成率为 {fmt(allm['completion'])}。")
    text(f"在 edit/batch/test 三类中，文件已正确却未提交的 act 为 {fmt(allm['missed_correct_acts'])}（分母为正常返回且候选文件正确的改动 act）；受影响运行 {fmt(allm['missed_correct_runs'])}。真正要求写入的 edit/batch 运行遗漏率为 {fmt(allm['missed_write_runs'])}。")
    text(f"回滚任务在要求的候选修改上测试失败后仍带提交标记的比例为 {fmt(allm['rollback_wrong_commit_acts'])}，正确不提交 {fmt(allm['rollback_correct_noncommit_acts'])}；回滚任务完成率 {fmt(allm['categories']['rollback'])}。")
    if not allm['omission_recovery']['next_rewrite_commit_all']['d']:
        text('需要保留修改的任务没有发生正确改动因遗漏提交而撤销，因此“看到撤销提示后下一次 act 重做并提交”的补救率为 0/0，不能估计，不能记成 0% 或 100%。回滚任务的主动撤销不是遗漏。')
    else:
        text(f"正确改动被撤销后，下一次 act 重做并提交比例为 {fmt(allm['omission_recovery']['next_rewrite_commit_all'])}；遗漏后最终完成 {fmt(allm['rescued_runs'])}。")
    text('这批小型任务支持继续采用显式提交的默认撤销语义；没有无提示对照组，不能证明提示本身提高了完成率。重复仅 3 次、写入任务只有 8 个，模型版本和提供方也与 v2 不同，不能将完成率差异解释为语义修改的因果效果。')
    heading('范围、输入与运行方式')
    import time
    table(['项目','记录'],[
        ['日期（UTC）',time.strftime('%Y-%m-%d',time.gmtime(frozen['time']))],
        ['worktree',str(ROOT.parents[1])],['worktree HEAD',frozen['base_commit']],
        ['起点','按任务要求执行 git checkout origin/exp/act-v2 -- prototypes/starlark-pass-rate'],
        ['正式网格','composed/A × 2 模型 × 24 任务 × 3 次 = 144；每任务最多 8 次 act'],
        ['试跑','每模型 read-02、probe-02、edit-01、batch-01、test-01、rollback-01 各 1 次，共 12；不并入正式分母'],
        ['模式选择','A 支持顶层 return，与 ADR 示例一致；B 使用 result()，本轮未运行'],
        ['调度','Random(20261114) 打乱任务；DeepSeek 4 并发、MiniMax 1 并发'],
        ['请求参数','未指定温度/采样参数；max_tokens=8192（v2 未设该上限）；请求超时 180 秒'],
        ['重试','沿用 v2：DeepSeek 最多 5 次、MiniMax 最多 10 次，指数退避封顶 120 秒；额外将 ConnectionResetError 纳入网络重试'],
        ['冻结','results/v3/evidence/freeze.json；输入 '+str(len(frozen['files']))+' 文件、测台 '+str(len(frozen['harness_files']))+' 文件，含二进制 SHA256 与试跑哈希'],
        ['解释器','Starlark 0.14.2；保留 v2 纯计算环境、命令归一化表、24 个任务与判定器'],
        ['返回上限','65536 UTF-8 字节；统一为提示预留 256 字节，脚本值上限 65280 字节；提示不额外触发失败'],
        ['临时目录','results/v3/tmp；模拟器的副本也放入夹具父目录；超时终止进程组'],
        ['隔离范围','保留 v2 模拟器，非生产沙箱：真实只读命令可能访问绝对路径；正式前设置 GIT_CEILING_DIRECTORIES，防止夹具内 git 向上发现研究仓库。所有实际脚本的外部路径/写命令需结合证据审查'],
    ])
    heading('端点与实际返回标识')
    table(['模型','base URL','请求模型','实际 model（响应数）','实际提供方（响应数）'],[
        [labels['deepseek'],'https://openrouter.ai/api/v1','deepseek/deepseek-v4.1-flash',json.dumps(metrics['deepseek']['returned_models'],ensure_ascii=False),json.dumps(metrics['deepseek']['providers'],ensure_ascii=False)],
        [labels['minimax'],'https://api.minimaxi.com/v1','MiniMax-M3.1-Flash',json.dumps(metrics['minimax']['returned_models'],ensure_ascii=False),json.dumps(metrics['minimax']['providers'],ensure_ascii=False)],
    ])
    text('启动前查询 [DeepSeek endpoints](https://openrouter.ai/api/v1/models/deepseek/deepseek-v4.1-flash/endpoints)，快照保存在 results/v3/evidence/deepseek-endpoints.json；当时没有 provider_name=DeepSeek，因此未发送官方固定路由参数，使用 OpenRouter 默认路由。每个 api_response 保留完整响应及 actual_model/actual_provider；单次任务内部也可能切换提供方。MiniMax 预检返回 200，未启用 OpenRouter 回退；直连响应不含 provider 字段，记录明确标为基于端点推断，而不是伪称响应原文。')
    heading('提示与结算语义')
    text('实际工具说明见 [act_tool_A_composed.md](prompts/act_tool_A_composed.md)。使用 v2 composed/A 的说明框架，仅替换结算段和示例；共同 system 与任务原文未改。接口移除 discard()，加入全局 commit()。只读或写后 reset 至基线无需提交；有净文件改动且未提交的正常返回仍为 Succeeded，副本不写回，receipt_summary 标注 uncommitted_reverted。commit 是本次调用的保留标记，不立即持久化；后续 fail、报错或超限仍撤销。')
    text('实际撤销提示原文（中文；其余提示主体沿用 v2 英文）：\n\n> 未调用 commit()：本次对 N 个文件的修改已撤销；需要保留改动时请调用 commit()')
    text('交给模型的 tool 消息为序列化返回值，随后换行附提示；candidate_changes、候选正确性、任务判定和宿主事件只写审计日志，不交给模型。与 v2 的 outcome/value/error/line JSON 包装不同，这是为了让提示处在 content 的真实末尾；也属于比较中的可见格式差异。')
    text('ADR 示例适配项：lancet/dev 改为 v2 的 repo/env；省略 env 不支持的 cwd；保留 v2 命令白名单，将 cargo test --offline 写为 cargo test；TextView 仍需 str(...) 序列化。没有顺带开启 ADR 的 SetType/sum/Print、静态类型检查或完整资源沙箱，避免把本实验扩成整个 ADR 实现。')
    text('ADR 对提前 commit 未规定运行时拒绝；模拟器按整次调用标记执行，单独将“写入之前 commit”“commit 后 fail/错误”“测试失败之后 commit”“撤销后空提交（未重做修改）”列作误用观察，不擅自改变保留规则。')
    heading('统计口径')
    table(['指标','分母与判定'],[
        ['有效运行','status=complete；api_error、harness_error、budget_stop 排除主分母并单列。失败运行保留，不因低通过率重跑'],
        ['需要保留改动分组','按任务要求列 edit/batch/test 共 12 项 ×3；但 v2 的 4 个 test 只执行测试、汇报错误，expected 等于初始文件，根本不要求写入。真正写入组另列 edit/batch 8 项 ×3'],
        ['遗漏 act','正常返回、发生净变更、候选文件精确等于 spec.expected、未提交且被撤销。分母首选正常返回且候选文件正确的改动 act；同时给全部三类 act 分母'],
        ['遗漏运行','至少一次上述遗漏；分母分别为 edit/batch/test 运行和真正写入运行。候选全判定通过的严格计数另列，区分文件正确与事实/先读后写时序'],
        ['最终补救','分别报告最后完整任务判定通过、最后文件哈希正确 / 有遗漏运行；避免把旧时序判定失败误认为提交补救失败'],
        ['下一次补救','撤销后紧邻 act 实际重新产生净变更且提交；必须来自后续 API 响应（同一响应的后续 tool_call 尚未看到提示，不算）。主分母为保留修改组全部提示；条件分母为确实存在下一次并看到提示的调用'],
        ['回滚误提交','在任务规定的候选文件上执行指定测试并得到非零结果的 act 中，commit_called=true；另列实际在失败结果后调用，以及最终保留错误修改。提前 commit 后测失败同样是不安全标记'],
        ['正确不提交','同一回滚测试分母中无 commit 调用，包含正常返回或 fail/error 撤销；最终任务完成还要求返回失败事实'],
        ['只读多余提交','read/probe 任务中的真实 commit 宿主事件，不将未执行分支的词法调用点算入'],
        ['表 1–4','沿用 v2：解析与运行通过、任务完成、原六类错误、失败后下一次运行通过。下一次“运行通过”不等于任务完成；commit 误用另列可与原错误重叠'],
        ['tokens','按 api_response 去重汇总，含最终答复与预算后答复；不重复相加各 act 关联的 usage'],
    ])
    heading('试跑与异常状态')
    table(['模型','试跑完成率','试跑状态','正式计划/有效','正式状态'],[[labels[a],fmt(pilot_metrics[a]['completion']),json.dumps(pilot_metrics[a]['statuses']),f"{metrics[a]['planned']}/{metrics[a]['valid']}",json.dumps(metrics[a]['statuses'])] for a in aliases])
    table(['试跑未通过 run_id','判定原因'],[[r['summary']['run_id'],'；'.join(r['summary']['reasons'])] for r in pilot if not r['summary']['passed']] or [['无','—']])
    heading('中止批次与重新运行')
    text('初次正式运行发现 Git 隔离缺陷：临时夹具位于本 worktree 内，GIT_CEILING_DIRECTORIES 最初设为夹具自身，不能阻止 Git 向上查找。git status 因而返回研究仓库的状态并触发虚假的 OutputLimitExceeded。修复为夹具父目录，并新增端到端回归；不改变任务、验收或命令归一化表。')
    table(['初次批次','数量'],[['试跑',12],['正式已启动',interrupted['started_formal']],['正式已结束',sum(x['status']=='complete' for x in interrupted['runs'])],['强制中止',sum(x['status']=='interrupted' for x in interrupted['runs'])],['尚未启动',interrupted['not_started_formal']]])
    text('发现后终止唯一驱动，保存全部原始记录、初次冻结清单和匹配其哈希的测台源码至 results/v3/attempt-1/，没有覆盖原始对话。初次正式批次整体排除主分析；随后重新执行 12 次试跑、重新冻结并运行完整 144 次网格，避免只重跑受影响或失败样本。此次额外运行偏离原计划，全部 OpenRouter 费用和已返回的 MiniMax usage 都纳入本实验成本。中止时一条 MiniMax 在途请求没有返回 usage，其 token 无法核算，不作零值估计。')
    table(['初次批次阶段','已结束任务完成率（仅作审计）'],[[phase,fmt(ratio(sum(r['passed'] for r in archived_summaries if r['phase']==phase),sum(r['phase']==phase for r in archived_summaries)))] for phase in ['pilot','formal']])
    table(['初次批次未通过运行','阶段','原因'],[[r['run_id'],r['phase'],'；'.join(r['reasons'])] for r in archived_summaries if not r['passed']])
    text('初次批次 API 重试事件 '+str(len(archived_retries))+' 次，原文均保留在对应 JSONL；超时未返回 usage 的请求同样无法计入 MiniMax token 合计，以下仅报告已返回的用量。')
    text('初次批次 Git 命令审计仅见 diff、status、ls-files、rev-parse 等读取命令，见 attempt-1/evidence/git-command-audit.json；源文件与旧报告的最终完整性另由验证器检查。完整中止/未启动清单见 attempt-1/evidence/interruption.json，归档哈希见 attempt-1/evidence/archive-integrity.json。')
    heading('表 1：每次 act 调用')
    table(['模型','act 数','解析通过率','运行通过率'],[[labels[a],metrics[a]['acts'],fmt(metrics[a]['parse']),fmt(metrics[a]['execution'])] for a in aliases+['all']])
    heading('表 2：每个任务')
    table(['模型','任务完成率','首次 act 完成率','平均 act 次数','用尽 8 次预算'],[[labels[a],fmt(metrics[a]['completion']),fmt(metrics[a]['first_completion']),f"{metrics[a]['acts']}/{metrics[a]['valid']} = {metrics[a]['acts']/metrics[a]['valid']:.4f}",sum(r['summary']['budget_exhausted'] for r in runs if a=='all' or r['summary']['alias']==a)] for a in aliases+['all']])
    table(['任务类别',*[labels[a] for a in aliases]],[[k,*[fmt(metrics[a]['categories'][k]) for a in aliases]] for k in ['read','probe','edit','batch','test','rollback']])
    heading('表 3：错误分类与 commit 误用')
    table(['模型',*CATEGORIES,'commit 误用 act'],[[labels[a],*[metrics[a]['errors'].get(k,0) for k in CATEGORIES],len(metrics[a]['misuse'])] for a in aliases+['all']])
    text('原六类只统计 ran=false 的 act，互斥；新增 commit 误用是独立观察标签，可出现在正常返回或报错 act 中，不能直接与前六列相加当成总错误数。未提交遗漏也不属于解释器运行失败。')
    heading('表 4：失败后下一次调用运行通过')
    table(['模型','错误类别','下次运行通过 / 有下次 act','无下次 act'],[[labels[a],k,fmt(metrics[a]['error_recovery'][k]),metrics[a]['error_no_next'][k]] for a in aliases for k in CATEGORIES])
    table(['模型','独立观察类别','下次运行通过 / 有下次 act','无下次 act'],[[labels[a],'commit 误用',fmt(metrics[a]['misuse_recovery']),metrics[a]['misuse_no_next']] for a in aliases])
    heading('提交、遗漏与最终补救')
    table(['指标',*[labels[a] for a in aliases+['all']]],[[label,*[fmt(metrics[a][key]) for a in aliases+['all']]] for label,key in [
        ['edit/batch/test 完成率','keep_completion'],['edit/batch 真正写入完成率','required_write_completion'],
        ['改动正确却遗漏 / 正常且文件正确的改动 act','missed_correct_acts'],['改动正确却遗漏 / 三类全部 act','missed_correct_of_all_keep_acts'],
        ['有遗漏运行 / 三类运行','missed_correct_runs'],['有遗漏运行 / 真正写入运行','missed_write_runs'],['遗漏后最终完整判定通过 / 有遗漏运行','rescued_runs'],['遗漏后最终文件正确 / 有遗漏运行','rescued_files_runs']]])
    table(['模型','三类全部 act','有净变更 act（含失败）','正常有净变更 act','候选完整判定通过但被撤销 act','全部未提交撤销 act'],[[labels[a],*[metrics[a][k] for k in ['keep_acts','changed_acts','normal_changed_acts','missed_strict_acts','all_keep_reverts']]] for a in aliases+['all']])
    if not allm['missed_write_runs']['n']:
        n=allm['missed_write_runs']['d']
        text(f'零遗漏不代表真实风险为零。将 {n} 次真正写入运行暂按独立同分布 Bernoulli 近似，单侧 95% 零事件上界为 1−0.05^(1/{n}) = {100*(1-0.05**(1/n)):.2f}%；任务重复和模型混合使该近似偏乐观，不据此作生产保证。')
    table(['遗漏 run_id','act','候选完整判定通过','最终任务通过'],[[x['run_id'],x['call'],x['candidate_check']['passed'],x['final_passed']] for x in allm['omissions']] or [['无','—','—','—']])
    heading('回滚与错误保留')
    table(['指标',*[labels[a] for a in aliases+['all']]],[[label,*[fmt(metrics[a][key]) for a in aliases+['all']]] for label,key in [
        ['测试失败 act 仍有提交标记','rollback_wrong_commit_acts'],['测试失败结果之后实际调用 commit','rollback_commit_after_failure_acts'],
        ['测试失败 act 正确不提交','rollback_correct_noncommit_acts'],['其中正常返回且不提交','rollback_normal_noncommit_acts'],['其中 fail/错误中止且不提交','rollback_error_noncommit_acts'],['测试失败 act 最终保留错误修改','rollback_bad_persist_acts'],
        ['有误提交运行 / 观察到候选测试失败的运行','rollback_wrong_commit_runs'],['正确不提交运行 / 观察到候选测试失败的运行','rollback_correct_noncommit_runs']]])
    text('未在规定候选修改上观察到测试失败的回滚运行：'+ '；'.join(labels[a]+f" {metrics[a]['rollback_unobserved_runs']}" for a in aliases)+'。这些运行仍计入任务完成率，不混入上述条件分母。')
    heading('撤销提示后的下一次 act')
    table(['范围','模型','下一次重做并提交 / 全部撤销','下一次重做并提交 / 已见提示且有下次 act','无下次 act','同 API 响应的下次 act'],[[scope,labels[a],fmt(metrics[a][key]['next_rewrite_commit_all']),fmt(metrics[a][key]['next_rewrite_commit_seen']),metrics[a][key]['no_next'],metrics[a][key]['same_response']] for scope,key in [('正确改动遗漏','omission_recovery'),('需保留组三类全部撤销','keep_recovery'),('全部类别（含主动回滚）','all_recovery')] for a in aliases+['all']])
    delayed=[x for x in allm['omission_recovery']['notices'] if not x['next_rewrite_commit']]
    table(['未在紧邻下一次 act 补救的遗漏','遗漏 act','下一次实际结局','最终完整任务通过'],[[x['run_id'],x['call'],next(r for r in runs if r['summary']['run_id']==x['run_id'])['calls'][x['call']]['outcome']['outcome'] if x['next_exists'] else '无下一次',next(r for r in runs if r['summary']['run_id']==x['run_id'])['summary']['passed']] for x in delayed])
    text('本轮两次延迟补救：batch-02-r2 的下一次脚本用了不支持的 assert，解析失败，再下一次成功；edit-03-r3 的下一次仅调用 commit() 并读取文件，没有重做已撤销的编辑，再下一次才重做并提交。这后一种是实测的跨调用空提交误用，不能记作即时补救成功。')
    text('全类别分母包含任务要求的正常回滚，下一次不重做/不提交通常是正确行为；该行只描述提示暴露，不可当作遗漏补救失败率。')
    if allm['omissions']:
        case=allm['omissions'][0]
        r=next(r for r in runs if r['summary']['run_id']==case['run_id'])
        call=next(c for c in r['calls'] if c['call']==case['call'])
        nxt=next((c for c in r['calls'] if c['call']==case['call']+1),None)
        text('遗漏/补救实例：['+case['run_id']+'](results/v3/formal/'+r['path'].name+')，act '+str(case['call'])+' 原文：\n\n```python\n'+call['script'].rstrip()+'\n```')
        text('该次返回的最后一行：`'+call['outcome']['revert_notice']+'`。')
        if nxt:text('紧邻下一次 act 原文：\n\n```python\n'+nxt['script'].rstrip()+'\n```')
    heading('只读任务与 v2 对照')
    table(['模型','read/probe 完成率','v2 composed/A read/probe','多余 commit 事件数','有 commit 的 act / 只读类别 act','有 commit 的运行 / 只读类别运行'],[[labels[a],fmt(metrics[a]['read_completion']),fmt(old[a]['A']['read_completion']),metrics[a]['read_commit_calls'],fmt(metrics[a]['read_commit_acts']),fmt(metrics[a]['read_commit_runs'])] for a in aliases])
    table(['模型','v2 composed/A 总完成率','v3 composed/A 总完成率','差值（百分点）','v2 composed A+B 总完成率','v2→v3 回滚完成率（A）'],[[labels[a],fmt(old[a]['A']['completion']),fmt(metrics[a]['completion']),f"{metrics[a]['completion']['pct']-old[a]['A']['completion']['pct']:+.2f}",fmt(old[a]['AB']['completion']),fmt(old[a]['A']['rollback_completion'])+' → '+fmt(metrics[a]['categories']['rollback'])] for a in aliases+['all']])
    text('v2 DeepSeek 请求 deepseek-flash，直连 https://api.deepseek.com；v3 请求 deepseek/deepseek-v4.1-flash，经 OpenRouter 且非官方固定提供方，不能确认两者为同一权重/服务。MiniMax 请求名与直连端点相同。v2 composed/A 的有效分母分别为 71、70（各 72 计划运行中的测台异常已排除）；A+B 为 143、142。比较同时受版本、路由、示例、返回包装、输出 token 上限与随机调度变化影响。')
    heading('失败与误用个案')
    failures=[r for r in runs if r['summary']['status']!='complete' or not r['summary']['passed']]
    table(['run_id','状态','判定原因或测台错误','act 数','最终回复见原记录'],[[r['summary']['run_id'],r['summary']['status'],'；'.join(r['summary']['reasons']) or r['summary'].get('harness_error') or r['summary'].get('api_error') or '',r['summary']['act_count'],f"[JSONL](results/v3/formal/{r['path'].name})"] for r in failures])
    table(['run_id','act','commit 误用标签'],[[x['run_id'],x['call'],'；'.join(x['flags'])] for x in allm['misuse']] or [['无','—','—']])
    text('MiniMax rollback-02-r2 在已记录 6 次 act 后，下一次脚本对 / 递归 grep memory_budget，超过 60 秒并被终止进程组；没有该次的完整宿主回执。整次运行排除主分母，其前 6 条回执仍离线复核，最后一次请求的原脚本和 usage 在 api_response 中保留。rollback-03-r2 则未实际修改文件就耗尽 8 次预算，属任务失败，仍计入完成率分母。')
    text('以上严格完成率不作事后纠正。仅在最终自然语言给出事实、返回值格式未被旧判定器接受、同一 act 读写违反 edit 时序等情况，均须查原始对话再讨论验收口径；它们不能归因为忘记 commit。')
    heading('费用与 token')
    table(['项','值'],[['OpenRouter GET /key 开始 usage（USD）',start['usage']],['OpenRouter GET /key 结束 usage（USD）',end['usage'] if end else '未查询'],['OpenRouter usage 差额（USD，含中止批次与两轮试跑）',costs['delta']],['OpenRouter 响应 usage.cost 合计（USD，交叉核对）',costs['openrouter_response_cost']],['费用上限','5 USD；每次请求前检查 key usage 与本地响应成本较大值，并预留并发请求的费用上界'],['MiniMax 预检 tokens',preflight['response']['usage']['total_tokens']],['MiniMax 已返回 tokens（含预检、中止批次、两轮试跑和正式）',costs['minimax_tokens']['total_tokens']]])
    token_rows=[]
    for phase,mm in [('试跑',pilot_metrics),('正式',metrics)]:
        for a in aliases:
            token_rows.append([labels[a],phase,*[mm[a]['usage'][k] for k in ['prompt_tokens','completion_tokens','total_tokens']]])
    table(['模型','阶段','prompt tokens','completion tokens','total tokens'],token_rows)
    table(['中止批次 MiniMax 输入','中止批次 MiniMax 输出','中止批次 MiniMax 总 token'],[[archived_tokens[k] for k in ['prompt_tokens','completion_tokens','total_tokens']]])
    table(['MiniMax 合计输入','MiniMax 合计输出','MiniMax 合计总 token'],[[costs['minimax_tokens'][k] for k in ['prompt_tokens','completion_tokens','total_tokens']]])
    finish=Counter(row['response']['choices'][0].get('finish_reason') for r in runs+pilot for row in r['rows'] if row['type']=='api_response')
    retries=[row for r in runs+pilot for row in r['rows'] if row['type']=='api_retry']
    text('修复后试跑与正式运行的 finish_reason 分布：'+json.dumps(dict(finish),ensure_ascii=False)+'。API 重试事件 '+str(len(retries))+' 次。单次请求的 8192 输出 token 上限是否实际截断，见 length 计数。')
    heading('验证、交付与未覆盖范围')
    text('实际验证命令：\n\n```sh\ncargo build --manifest-path prototypes/starlark-pass-rate/Cargo.toml -j 4\nTMPDIR="$PWD/prototypes/starlark-pass-rate/results/v3/tmp" python3 -m unittest discover -s prototypes/starlark-pass-rate/tests\npython3 prototypes/starlark-pass-rate/freeze.py\npython3 prototypes/starlark-pass-rate/validate_v3.py --phase pilot\npython3 prototypes/starlark-pass-rate/validate_v3.py --phase formal\npython3 prototypes/starlark-pass-rate/report_v3.py\npython3 prototypes/starlark-pass-rate/crosscheck_v3.py\npython3 prototypes/starlark-pass-rate/audit_v3.py\npython3 prototypes/starlark-pass-rate/check_docs_v3.py\n```')
    for phase in ['pilot','formal']:
        p=V3/f'evidence/{phase}-validation.json'
        if p.exists():text(phase+' 离线验证：`'+json.dumps(json.loads(p.read_text()),ensure_ascii=False)+'`。')
    doc_status=V3/'evidence/docs-validation.json'
    if doc_status.exists():text('文档链接验证：`'+json.dumps({k:v for k,v in json.loads(doc_status.read_text()).items() if k!='temporary_entries'},ensure_ascii=False)+'`。直接在真实索引运行第一次检查时，尚未跟踪的 REPORT-v3.md 被判为不存在；该失败保留于 docs-check-untracked.txt。临时索引仅声明交付文件参与检查，没有修改检查器或真实暂存区。')
    if (V3/'evidence/independent-crosscheck.json').exists():text('独立 SQL 重算通过：完成率、正确改动遗漏 act/运行、最终补救、紧邻下一次补救、只读 commit 计数均与报告一致，见 independent-crosscheck.json。')
    if (V3/'evidence/replay-difference-audit.json').exists():
        changes=json.loads((V3/'evidence/replay-difference-audit.json').read_text())
        text('回放返回值差异审计：'+json.dumps(dict(Counter(x['kind'] for x in changes)),ensure_ascii=False)+'；不修改原始值。')
    text('22 项单元/集成测试见 results/v3/evidence/tests.txt；覆盖正常/未提交/报错结算、提示真实传入下一轮、候选态判定、并发费用预留、48 个参考任务实例（24×A/B）、v2 历史假阴性、命令归一化、预算、reasoning 回填与密钥不落日志。测试先在旧语义上失败，证据为 tdd-before.txt。')
    text('旧 REPORT.md、REPORT-v2.md、results/v2/、v1 记录、原任务、check.py 与命令归一化表逐字节对比 origin/exp/act-v2；docs/ 对比本 worktree HEAD。正式记录、API 原文、工具提示、可见 tool 消息、事件、候选改动、最终文件哈希、指标与费用均在 results/v3/。')
    text('临时目录偏离：最初在旧实现上运行的 3 条 TDD 失败回归沿用了默认 /tmp，而非本 worktree；这些测试的临时目录已由清理回调删除。随后将模拟器副本、所有模型试跑/正式运行、回放及后续测试统一放入本 worktree 的 results/v3/tmp。没有修改其他 worktree 或 ~/Dev 下的源文件。')
    text('未提交、未推送、未写 GitHub、未修改 docs/adr/。没有运行全仓 CI：本次只改独立原型，使用该原型构建、测试与已记录回执的全量回放。没有扩展到生产的并发文件冲突、沙箱、seam、崩溃恢复或跨 act revert；v2 的命令白名单、静态 canned 测试输出和纯计算缺项仍可能影响模型表现。')
    heading('建议维护者裁决')
    text(f"ADR 更新提案（本次未修改 ADR）：在‘已核实的事实’新增本轮冻结记录，注明正式完成率 {fmt(allm['completion'])}、正确改动遗漏运行 {fmt(allm['missed_correct_runs'])}、真正写入运行遗漏 {fmt(allm['missed_write_runs'])}、遗漏后下一次重做并提交 {fmt(allm['omission_recovery']['next_rewrite_commit_all'])}、回滚误提交 {fmt(allm['rollback_wrong_commit_acts'])}，并分模型引用本报告。第 3 节维持显式 commit 与 content 末尾提示；‘尚未核实’仍应保留复杂真实项目上的外推及提示因果效应。")
    text('1. 继续保留显式 commit 与未提交自动撤销；本轮结果应作为小样本行为证据，不将零事件写成“不会遗漏”。\n2. 下一轮专门制造“正确写入但漏提交”的可控提示暴露，并设置有/无提示对照，才能测量提示补救效应；本轮不追加诱导样本污染自然遗漏率。\n3. 是否将最终自然语言中正确事实纳入普通任务验收，以及 edit 是否允许同次 act 先读后写，应单独裁决；本轮保持 v2 判定。\n4. 若要比较模型本身，应在下一轮固定同一可用提供方并记录版本，避免 OpenRouter 在一次任务内换提供方；当前没有官方 DeepSeek 端点。\n5. 是否把撤销提示改为更明确的‘未调用 commit()：本次对 N 个文件的修改已撤销；需要保留时请重新执行修改，并在同一次 act 中调用 commit()’，以避免撤销后的空提交。本轮实际提示仍是前文默认原文，新措辞只是待裁决提案。提前 commit 的运行时限制也需另行裁决，当前遵循整次调用标记。')
    (ROOT/'REPORT-v3.md').write_text('\n'.join(out)+'\n')
    print(json.dumps(dict(formal=len(runs),pilot=len(pilot),complete=allm['completion'],cost=costs['delta']),ensure_ascii=False))


if __name__=='__main__':main()
