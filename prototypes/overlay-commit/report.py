#!/usr/bin/env python3
"""由冻结的原始 JSON 生成实验数据表，不改写运行数据。"""
from collections import Counter, defaultdict
import gzip
import hashlib
import json
from pathlib import Path

HERE = Path(__file__).resolve().parent
RESULTS = HERE / 'results'


def read(path):
    if path.suffix == '.gz':
        return json.loads(gzip.decompress(path.read_bytes()))
    return json.loads(path.read_text())


def cell(v):
    if v is None:
        return '—'
    if isinstance(v, float):
        return f'{v:.6f}'
    if isinstance(v, (dict,list)):
        v=json.dumps(v, ensure_ascii=False, separators=(',', ':'))
    return str(v).replace('|','\\|').replace('\n','<br>')


def table(headers, rows):
    out=['| '+' | '.join(headers)+' |','| '+' | '.join('---' for _ in headers)+' |']
    out.extend('| '+' | '.join(cell(x) for x in row)+' |' for row in rows)
    return '\n'.join(out)+'\n'


def main():
    all_records=[]
    for p in sorted(RESULTS.glob('*.json*')):
        d=read(p)
        if isinstance(d,dict) and 'scenario' in d and 'outcome' in d:
            d['_file']=p.name
            all_records.append(d)
    all_rows=sorted(all_records,key=lambda d:(d.get('start_utc',''),d['run_id']))
    exclusions=read(RESULTS/'excluded-runs.json') if (RESULTS/'excluded-runs.json').exists() else {}
    rows=[r for r in all_rows if r['run_id'] not in exclusions]
    by_id={r['run_id']:r for r in rows}
    env=read(RESULTS/'environment.json')
    sections=['# Overlay 提交实验 2 原始数据报告\n']
    def section(title, headers, values):
        sections.append('## '+title+'\n\n'+table(headers, values))
    def link(r):
        return '['+r['run_id']+'](results/'+r['_file']+')'
    def label(r):
        return r['scenario']+'/'+r.get('variant','')+'/'+str(r.get('repetition',''))
    def short_git(g):
        s=g.get('status',{}); f=g.get('fsck',{})
        return {'status_code':s.get('code'),'status':s.get('stdout'), 'fsck_code':f.get('code'),
                'fsck_stdout_lines':len(f.get('stdout','').splitlines()),
                'fsck_stderr':f.get('stderr'), 'branch':g.get('branch',{}).get('stdout','').strip()}
    section('环境',['项目','原始值'],[
        ['OS',env['os']], ['内核',env['kernel']], ['SELinux',env['selinux']['stdout'].strip()],
        ['文件系统',env['filesystem']['stdout']],['预热后磁盘（字节）',env['free']['stdout']],
        ['LATTICE HEAD',env['repo_head']['stdout'].strip()],['LANCET 基线',env['lancet_base']['stdout'].strip()],
        ['开始前可用字节（最初 df 工具输出）',87095607296],['开始 / 场景阈值字节',[30*1024**3,15*1024**3]],
        ['Python',env['python']],['Git',env['git']['stdout'].strip()],['rustc',env['rustc']['stdout'].strip()],
        ['仓库计数：含 target（条目 / 非目录文件）',env['repo_counts']['True']],
        ['仓库计数：不含 target（条目 / 非目录文件）',env['repo_counts']['False']],
        ['预热 cargo check --workspace（秒 / code）',[read(RESULTS/'precheck.json')['seconds'],read(RESULTS/'precheck.json')['code']]],
        ['计时口径','秒；total_s 含 T0、快照、命名空间、脚本、分类/预检/提交、xattr 和内外 Git 观测；不含驱动恢复基线'],
        ['提交耗时口径','commit_s = reflink 前态 + 写回；分类、预检、getfattr 另列'],
        ['文件数口径','lstat，含 .git；目录与文件分别计；符号链接按非目录条目计；不跟随链接'],
        ['正式运行数',len(rows)],['排除的前置失败批次运行数',len(all_rows)-len(rows)],['原始环境','[environment.json](results/environment.json)'],
    ])
    section('表 1：各场景结局（每次运行一行）',
            ['场景 / 变体 / 重复','git 策略','冲突检查','运行','结局','是否符合期望','总耗时 s','提交耗时 s','计入正式统计'],
            [[label(r),r['git_policy'],r['check'],link(r),r['outcome'],
              r.get('matches_expectation'),r.get('total_s'),r.get('timings',{}).get('commit_s'),r['run_id'] not in exclusions] for r in all_rows])
    section('表 2：冲突检查',
            ['运行','C1 判出','C4 判出','实际冲突路径','外部注入路径','C1 误报 / 漏报','C4 误报 / 漏报'],
            [[link(r),r.get('conflicts',{}).get('C1'),r.get('conflicts',{}).get('C4'),r.get('truth_conflict_paths',[]),
              list(r.get('external',{})),
              [len(r.get('confusion',{}).get('C1',{}).get(k,[])) for k in ('false_positive','false_negative')],
              [len(r.get('confusion',{}).get('C4',{}).get(k,[])) for k in ('false_positive','false_negative')]] for r in rows])
    section('冲突检查计数口径',['项','值'],[
        ['单位','一次运行 × 一条路径；聚合不去除跨运行重复'],
        ['真冲突','S3 的 README.md；S9 same-B 的 README.md'],
        ['E-other','Cargo.toml 不在提交集合，真冲突集合为空'],
        ['其他运行','背景噪声和 P3 直接 Git 写入不作为人为注入的真实源码冲突'],
        ['both','两者各自记录；实际提交决策采用 C1'],
        ['丢弃与脚本失败','仍分类并计算两种检查结果；按 finish/on-fail 决定是否写回'],
        ['C4 不存在的路径','当前 lstat 为 None 时不判冲突；此为写死 ctime > T0 规则的实现'],
    ])
    section('C1 快照与阶段耗时',['运行','含 target s','不含 target s','含 / 不含条目数','挂载 s','脚本 s','分类 s','冲突检查 s','reflink s','写回 s','属性扫描 s'],
            [[link(r),r.get('snapshot',{}).get('with_target',{}).get('seconds'),r.get('snapshot',{}).get('without_target',{}).get('seconds'),
              [r.get('snapshot',{}).get(k,{}).get('entries') for k in ('with_target','without_target')],
              r.get('child',{}).get('mount_s'),r.get('child',{}).get('script',{}).get('seconds'),
              *[r.get('timings',{}).get(k) for k in ('classify_s','conflict_s','preimage_s','write_s','xattr_scan_s')]] for r in rows])
    section('表 3：上层构成',['运行','.git 条目','target 条目 / 常规文件字节','其他条目','只改时间戳条目','opaque / redirect','提交后 overlay 属性残留'],
            [[link(r),r.get('upper_counts',{}).get('.git',{}).get('entries'),
              [r.get('upper_counts',{}).get('target',{}).get(k) for k in ('entries','bytes')],
              r.get('upper_counts',{}).get('other',{}).get('entries'),len(r.get('classification',{}).get('timestamp_only',[])),
              [len(r.get('classification',{}).get(k,[])) for k in ('opaque','redirect')],
              r.get('overlay_xattr_residual_count') if r.get('committed_paths') else '未提交 / 未扫描'] for r in rows])
    s6=[r for r in rows if r['scenario']=='S6']
    section('表 4：.git 策略（S6）',['策略','噪声','重复','提交脚本 code','丢弃脚本 code','提交后 status / fsck','丢弃后 status / fsck','噪声 index 冲突 C1 / C4','运行'],
            [[policy,noise,rep,
              a.get('child',{}).get('script',{}).get('code') if a else None,
              b.get('child',{}).get('script',{}).get('code') if b else None,
              short_git(a.get('after_git',{})) if a else None,short_git(b.get('after_git',{})) if b else None,
              [sum('.git/index' in r.get('conflicts',{}).get(c,[]) for r in (a,b) if r) for c in ('C1','C4')],
              ' / '.join(link(r) for r in (a,b) if r)]
             for policy in ('P1','P2','P3') for noise in ('quiet','noise')
             for rep in sorted({r.get('repetition') for r in s6})
             for a in [next((r for r in s6 if r['git_policy']==policy and r['variant']==noise+'-commit' and r['repetition']==rep),None)]
             for b in [next((r for r in s6 if r['git_policy']==policy and r['variant']==noise+'-discard' and r['repetition']==rep),None)]])
    section('S6 Git 原始状态校验',['运行','HEAD 变化','after status = before','after fsck stdout = before','fsck code','全部判定字段'],
            [[link(r),r.get('observed',{}).get('head_changed'),
              r.get('after_git',{}).get('status',{}).get('stdout')==r.get('before_git',{}).get('status',{}).get('stdout'),
              r.get('after_git',{}).get('fsck',{}).get('stdout')==r.get('before_git',{}).get('fsck',{}).get('stdout'),
              r.get('after_git',{}).get('fsck',{}).get('code'),r.get('assertions')] for r in s6])
    section('表 5：S5 重新构建耗时',['运行','失败策略','脚本 code','target 条目 / 字节','只改时间戳条目','下一次 cargo build -p net-guard s','code'],
            [[link(r),r['on_fail'],r['child']['script']['code'],
              [r['upper_counts']['target'][k] for k in ('entries','bytes')],len(r['classification']['timestamp_only']),
              r.get('next_build',{}).get('seconds'),r.get('next_build',{}).get('code')] for r in rows if r['scenario']=='S5'])
    selection=read(RESULTS/'crate-selection.json')
    section('S5 crate 产物大小',['crate','自有产物逻辑字节','文件数','选用'],
            [[r['crate'],r['bytes'],r['files'],r['crate']==selection['selected']] for r in selection['own_artifacts']])
    section('表 6：S7 各命令的结果',['运行','命令','code','EXDEV','stderr 原文'],
            [[link(r),r['script_text'].strip(),r['child']['script']['code'],r['observed']['exdev'],r['child']['script']['stderr']] for r in rows if r['scenario']=='S7'])
    section('表 7：S9 最终内容',['运行','提交次序','结局','路径','最终首行','最终内容 SHA-256','丢失/拒绝的修改','完整内容'],
            [[link(r),r['commit_order'],r['outcome'],p,v.splitlines()[0],hashlib.sha256(v.encode()).hexdigest(),r['lost_changes'],'JSON observed.'+p]
             for r in rows if r['scenario']=='S9' for p,v in r['observed'].items()])
    cals=sorted(RESULTS.glob('calibration-*/calibration.json'))
    for cal_path in cals:
        cal=read(cal_path)
        section('真实写入统计：'+cal_path.parent.name,
                ['记录段 UTC','覆盖秒 / 小时','全部事件','主 .git/index','其他 .git','源码/其他','其他 Agent 沙箱','target 汇总行','overflow'],
                [[s['start_utc']+' → '+s['end_utc'],[s['duration_s'],s['duration_s']/3600],s['events'],
                  s['counts'].get('git_index',0),s['counts'].get('git_other',0),s['counts'].get('source_other',0),
                  s['counts'].get('other_agent_sandbox',0),len(s['target_records']),s['counts'].get('overflow',0)] for s in cal['coverage']])
        section('真实写入统计口径',['项','原始值'],[
            ['两段覆盖秒数',cal['covered_seconds']],['覆盖不足 24 小时',cal['covered_seconds']<86400],
            ['第一段截止','最后一条事件；已知进程于约 08:10 UTC 意外退出'],
            ['第二段截止','校准冻结时间；服务当时 active (running)'],
            ['其他 Agent 沙箱来源',cal['other_agent_sandbox_attribution']],
            ['target_per_minute.jsonl','本次冻结文件 0 字节 / 0 行；无可报告的 target 写入计数'],
            ['overflow','按 kind=overflow 计；原始文件没有该类记录'],
            ['事件字段','create/close_write/delete/attrib/moved_from/moved_to；同一次操作可生成多条事件；空 ev 原样保留'],
            ['start 事件','每段 1 条 start；计入全部事件，不计入路径类别'],
            ['校准源',cal['sources']],
        ])
        for idx,s in enumerate(cal['coverage'],1):
            section('真实写入路径频数：段 '+str(idx),['路径','事件条数','事件 / 分钟'],
                    [[p,n,n*60/s['duration_s']] for p,n in sorted(s['paths'].items())])
        section('噪声生成器校准',['参数','校准前','校准后','实测操作数','记录覆盖秒'],
                [['git status 周期 s',1,cal['git_status_interval_s'],'不可从只读调用推出',cal['covered_seconds']],
                 ['cargo check 次/启动',1,int(cal['cargo_check_once']),'target 汇总 0 行',cal['covered_seconds']],
                 *[[k+' 周期 s',5 if k=='source_touch' else '未配置',v,cal['observed_operations'][k],cal['covered_seconds']] for k,v in cal['intervals_s'].items()],
                 ['其他 Agent 沙箱目录寿命 s','未配置',cal['ephemeral_lifetime_s'],'实测小于 1 秒',cal['covered_seconds']],
                 ['噪声窗口 s','未规定',cal['duration_s'],'每次实际运行时长见下一表',cal['covered_seconds']],
                 ['初始相位','未规定',cal['initial_phase'],'按运行种子均匀抽取',cal['covered_seconds']]])
    section('噪声实际产生记录',['运行','配置来源','实际时长 s','各类事件数','cargo check code','噪声进程 code'],
            [[link(r),r['noise']['config']['source'],r['noise']['duration_s'],dict(Counter(e['type'] for e in r['noise']['events'])),
              r['noise'].get('cargo',{}).get('code') if r['noise'].get('cargo') else None,r['noise'].get('process_code')]
             for r in rows if 'noise' in r])
    section('原型与方案的偏差',['项','实际执行'],[
        ['分支 / 范围','按覆盖指令使用 exp/overlay-commit；Python 原型，无新增 Cargo.toml；未修改 docs/'],
        ['S6 提交入口','bot-run git commit -m t；脚本其余 Git 命令保持方案；set -e 遇非零退出停止'],
        ['S6 首批前置失败','本地 clone origin 使 bot-run 拒绝；原始失败保留，修正实验 repo/baseline 的 origin 到 https://github.com/Skyline10124/lattice.git 后重新运行；未推送 LATTICE'],
        ['连续失败处理偏差','首批只将 namespace blocked 视为基础设施失败，bot-run 拒绝曾连续执行 3 次；现识别 bot-run 前置拒绝并在连续两次后跳过'],
        ['S6 初始改动','脚本前在实验克隆 README 追加 S6 input，确保 commit 非空'],
        ['S6 丢弃','新增 --finish discard，对成功脚本丢弃草稿层；P3 真实 Git 写入按实记录'],
        ['S9 时序','双 overlay 同时运行并产生草稿；同步点控制 A 完成提交后 B 检查；未测双方同时进入预检-写回区间'],
        ['C1','快照记录路径、size、mtime_ns、ctime_ns、inode，另保留 mode；含/不含 target 分别计时'],
        ['目录','对删除目录的后代也预检、留前态；只对 opaque=y 清理下层子项'],
        ['T0','取实验文件系统中新建文件 ctime_ns；不使用系统时钟作为冲突基准'],
        ['S5 产物计数','target 字节为上层常规文件逻辑大小之和；非实际磁盘占用；每次恢复相同预热基线'],
        ['噪声频率','inotify 不提供只读 git status 次数、无写入 cargo check 次数；这两项保留原方案'],
        ['噪声新增类型','其他 Agent 沙箱目录、FETCH_HEAD、配置/引用锁、独立检查点索引/对象/引用、工作树索引'],
        ['噪声动作映射','FETCH_HEAD 原值写回；Git 锁创建后删除；工作树事件使用独立 index；检查点生成新的有效树引用；未重放真实对象内容或网络 fetch'],
        ['噪声时间窗口','30 秒 + 脚本/核查收尾；随机初相位；低频动作可能未在每个窗口出现'],
        ['崩溃与并发原子性','无 WAL/崩溃回滚；冲突预检后无跨写者锁；脚本后的逐文件 rename 不是跨文件原子提交'],
        ['EXDEV 和预置失败','S7 EXDEV、S5 测试失败和 P2 只读拒绝作为场景结局记录；不计为基础设施阻塞'],
    ])
    audit_path=RESULTS/'resume-audit.json'
    if audit_path.exists():
        a=read(audit_path)
        section('中断恢复',['项','原始值'],[
            ['完整运行数',sum(s['complete'] for s in a['states'])],['不完整运行',a['incomplete']],
            ['实验 overlay 挂载',a['overlay_mounts']],['恢复前 status',a['before_git']['status']['stdout']],
            ['恢复后 status',a['after_git']['status']['stdout']],['恢复后 fsck code',a['after_git']['fsck']['code']],
            ['fsck 与克隆基线 stdout 相同',a['baseline_fsck']['stdout']==a['after_git']['fsck']['stdout']],
            ['S9','6 组 / 12 份 JSON 均完整；删除不完整记录 0；重跑 0'],
        ])
    validation=[]
    for name in ('tests-red.json','tests-first-implementation.json','tests-green.json','syntax-check.json','docs-check.json','data-validation.json'):
        path=RESULTS/name
        if path.exists():
            r=read(path)
            validation.append([name,r.get('command'),r.get('code'),r.get('seconds'),r.get('summary',r.get('stdout','')+r.get('stderr',''))])
    section('验证',['记录','命令','退出码','秒','结果原文/字段'],validation)
    section('未覆盖范围',['项','范围'],[
        ['文件系统','仅当前 XFS、当前内核与 SELinux；未测其他文件系统/内核'],
        ['目标','真实仓库的单文件/三文件编辑与一个 crate；未做全 workspace build/test'],
        ['并发','S9 预检-写回互撞和真实外部写者在提交过程中修改未覆盖'],
        ['C4','全部场景使用 both 并按 C1 提交；未单独按 C4 决策执行'],
        ['记录时间','两段短于 24 小时；停止区间没有数据'],
        ['工具范围','S7 仅 mv、git mv、os.rename；redirect 仅分类测试，不跟随处理'],
        ['git stash','提交后无新工作区改动；实际返回 No local changes to save；非空 stash 未覆盖'],
    ])
    blockers=[]
    for path in sorted(RESULTS.glob('blocked-*.json')):
        b=read(path);blockers.append([path.name,b.get('command'),b.get('consecutive_failures'),b.get('error',b.get('reason'))])
    for r in rows:
        if r['outcome']=='blocked':
            blockers.append([link(r),r.get('invocation',{}).get('command'),None,r.get('error',r.get('namespace',{}).get('stderr'))])
    section('阻塞项（命令与原始报错）',['记录','命令','连续失败次数','原始报错'],blockers or [['无','—',0,'—']])
    if (RESULTS/'blocked-origin.json').exists():
        b=read(RESULTS/'blocked-origin.json')
        section('阻塞项处理记录',['项','值'],[[k,b.get(k)] for k in ('status','repair_commands','identity_check','excluded_runs','interrupted_runs','driver_deviation')])
    if (RESULTS/'blocked-cleanup.json').exists():
        b=read(RESULTS/'blocked-cleanup.json')
        section('清理阻塞处理',['项','值'],[[k,b.get(k)] for k in ('status','affected_directories','raw','remaining')])
    if (RESULTS/'cleanup.json').exists():
        c=read(RESULTS/'cleanup.json')
        section('清理',['项','原始值'],[
            ['删除 upper/work 目录数（两次合计）',len(c['deleted'])+len(c.get('previous_deleted',[]))],['剩余 upper/work',c['remaining_upper_work']],
            ['清理前 / 后可用字节',[c['free_before_bytes'],c['free_after_bytes']]],
            ['活动实验进程 / overlay 挂载',[c['active_processes'],c['overlay_mounts']]],
            ['最终克隆 status',c['after_git']['status']['stdout']],['最终克隆 fsck code',c['after_git']['fsck']['code']],
            ['保留路径',c['retained']],['清理失败',c['failures']],['记录器状态',c['recorder_status']['stdout']],
            ['完整清理清单','[cleanup.json](results/cleanup.json)'],
        ])
    if (RESULTS/'packaging.json').exists():
        p=read(RESULTS/'packaging.json')
        section('原始文件封装',['项','值'],[['压缩前结果总字节',p['before_bytes']],['阈值字节',p['threshold_bytes']],['gzip 运行数',len(p['compressed'])],['清单与原文 SHA-256','[packaging.json](results/packaging.json)']])
    grouped=defaultdict(list)
    for r in rows:
        grouped[(r['scenario'],r['git_policy'])].append(r)
    section('场景与策略汇总',['场景','策略','符合期望 / 完整运行','C1 误报 / 漏报','C4 误报 / 漏报'],
            [[s,p,[sum(r.get('matches_expectation') is True for r in rr),len(rr)],
              *[[sum(len(r.get('confusion',{}).get(c,{}).get(k,[])) for r in rr) for k in ('false_positive','false_negative')] for c in ('C1','C4')]]
             for (s,p),rr in sorted(grouped.items())])
    totals={c:{k:sum(len(r.get('confusion',{}).get(c,{}).get(k,[])) for r in rows) for k in ('false_positive','false_negative')} for c in ('C1','C4')}
    section('冲突检查总数',['检查','误报路径次数','漏报路径次数'],[[c,v['false_positive'],v['false_negative']] for c,v in totals.items()])
    (HERE/'REPORT.md').write_text('\n'.join(sections))
    print(json.dumps(dict(runs=len(rows),totals=totals,blockers=len(blockers)),ensure_ascii=False))


if __name__=='__main__':
    main()
