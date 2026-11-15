#!/usr/bin/env python3
"""从原始结果生成中位数、报告表格与独立 HTML 图。"""
import json,re,statistics
from pathlib import Path
HERE=Path(__file__).resolve().parent;R=HERE/'results/v5';MODES=['none','ns','B3','S','S2']
def read(n):return json.loads((R/(n+'-v5.json')).read_text())
def med(rows,key):
    a=[key(r) if callable(key) else r[key] for r in rows];return statistics.median(a)
def table(head,rows):return '\n'.join(['| '+' | '.join(head)+' |','| '+' | '.join(['---']*len(head))+' |',*['| '+' | '.join(map(str,row))+' |' for row in rows]])
def put(name,text):
    global report
    tag=f'<!-- {name} -->';end=f'<!-- /{name} -->';start=report.index(tag);stop=report.find(end,start)
    if stop<0:stop=start+len(tag)
    else:stop+=len(end)
    report=report[:start]+tag+'\n'+text+'\n'+end+report[stop:]
def initial_s(n):
    p=R/'initial-s-v5.json'
    a=[x for x in json.loads(p.read_text()) if x['files']==n] if p.exists() else []
    return f"{med(a,'init_s'):.6f}" if len(a)==3 else '待补齐三次'
def stats(rows,key):
    a=[key(x) if callable(key)else x[key]for x in rows];return dict(n=len(a),median=statistics.median(a),min=min(a),max=max(a),samples=a)
report=(HERE/'REPORT-v5.md').read_text();summary={};builds=read('builds');checkouts=read('checkouts');micro=read('micro');fixed=read('fixed');initial=read('initial');settle=read('settlement');renames=read('renames')
perf=[]
for label,rows in [('LATTICE 全量构建',builds),('checkout 甲 a→b',[x for x in checkouts if x['scenario']=='A']),('checkout 乙 b→a',[x for x in checkouts if x['scenario']=='B' and x['target']=='a']),('checkout 乙 a→b',[x for x in checkouts if x['scenario']=='B' and x['target']=='b'])]:
    summary[label]={m:stats([x for x in rows if x['mode']==m],'wall_s')for m in MODES};perf.append([label,*[f"{summary[label][m]['median']:.6f}"for m in MODES]])
text='五配置总时间中位数，单位秒；[构建原始样本](results/v5/builds-v5.json)、[checkout 样本](results/v5/checkouts-v5.json)。\n\n'+table(['工作负载',*MODES],perf)
parts=[]
for label,rows in [('全量构建',builds),('checkout 甲',[x for x in checkouts if x['scenario']=='A']),('checkout 乙 b→a',[x for x in checkouts if x['scenario']=='B' and x['target']=='a']),('checkout 乙 a→b',[x for x in checkouts if x['scenario']=='B' and x['target']=='b'])]:
    a=[x for x in rows if x['mode']=='S2'];parts.append([label,*[f'{med(a,k):.6f}'for k in ['sync_s','exec_s','scan_s']],f"{med(a,lambda x:x['settlement']['commit_s']):.6f}",f"{med(a,'wall_s'):.6f}"])
text+='\n\nS2 分项中位数（秒，列独立取中位数，不保证相加等于总列）：\n\n'+table(['负载','同步（含 Git 前态）','子进程','求 ε（含 Git 后态）','提交','总计'],parts)
summary['S2_parts']=parts
text+='\n\n20,000 次 open+close 的探针内部单次中位数（µs）：\n\n'+table(['路径',*MODES],[[p,*[f"{med([x for x in micro if x['path']==p and x['mode']==m],lambda x:x['observation']['us_per_call']):.3f}"for m in MODES]]for p in ['/dev/null','a']])
text+='\n\n普通路径的 S2 open+close 明显高于 S/ns；S2 在这里还承担常驻监听和事件消费。该微基准的增幅没有在全量构建中按同样比例出现。'
rows=[]
for n in [3000,10000,100000]:rows.append([n+100,*[f"{med([x for x in fixed if x['files']==n and x['mode']==m],'wall_s')*1000:.3f}"for m in MODES+['S2-full']]])
text+='\n\n无改动固定开销中位数（ms），每树额外 100 个目录，该夹具没有 `.git/`。S2/S2-full 为 begin+finish+commit 的 RPC 总耗时，不含子进程；其他配置执行 `true`，因此还包含其启动成本，不能用该表声称 S2 启动比 none 快。S2-full 强制两侧全内容核对。\n\n'+table(['条目数',*MODES,'S2-full'],rows)
git_fixed=read('git-fixed');summary['git_fixed']={str(n):stats([x for x in git_fixed if x['files']==n],'wall_s')for n in [10000,100000]}
text+='\n\n含真实 Git 索引的无改动 S2 act 另测三次，仍为 begin+finish+commit，不运行子进程；包含 Git 前态快照、结束摘要与 settled 标记。启动全扫不计入该常驻成本。\n\n'+table(['跟踪文件数','index 字节','完整 act 中位数 ms','最小 ms','最大 ms'],[[n,next(x['index_bytes']for x in git_fixed if x['files']==n),*[f"{summary['git_fixed'][str(n)][k]*1000:.3f}"for k in ['median','min','max']]]for n in [10000,100000]])+'\n\n固定开销目标按题目的中位数口径判断；10 万文件含 Git 的首个样本为 66.993 ms，不能宣称每次均低于 50 ms。[Git 无改动 act 原始样本](results/v5/git-fixed-v5.json)。'
rows=[]
for n in [1,100,4000]:
    for m in ['S','S2']:
        a=[x for x in settle if x['changed']==n and x['mode']==m];rows.append([m,n,*[f'{med(a,k)*1000:.3f}'for k in ['scan_s','prepare_s','apply_s','commit_s']]])
text+='\n\n提交应用中位数（ms）：\n\n'+table(['配置','改变文件数','求 ε','准备','应用及持久化','提交总计'],rows)
text+='\n\n目录 rename 完整 act 中位数（秒）：\n\n'+table(['目录内文件数',*MODES],[[n,*[f"{med([x for x in renames if x['files']==n and x['mode']==m],'wall_s'):.6f}"for m in MODES]]for n in [1000,10000]])
text+='\n\nS2 初次建影子中位数（秒、MiB），每规模三次新建脏输入，预刷盘计入初始化：\n\n'+table(['文件数','节点初始化秒','影子 apparent MiB','影子 allocated MiB','状态 allocated MiB'],[[n,f"{med([x for x in initial if x['files']==n],'init_s'):.6f}",*[f"{med([x for x in initial if x['files']==n],k)/2**20:.2f}"for k in ['apparent','allocated','state_allocated']]]for n in [3000,10000,100000]])
text+='\n\n同主机重新运行 S 初建（三次，秒）：\n\n'+table(['文件数','S','S2'],[[n,initial_s(n),f"{med([x for x in initial if x['files']==n],'init_s'):.6f}"]for n in [3000,10000,100000]])+'\n\n无沙箱/ns/B3 不建立影子；提交应用表仅适用于 S/S2，其余配置原地修改，没有此合并阶段。'
text+='\n\n当前提交应用与初建仍有显著逐文件成本；这些数值是本原型的结果，不是 S 路线的性能下限。'
text+='\n\n`du` 的 allocated 包括 reflink 共享块，不能当成新增物理块。初建、同步、提交、目录改名原始结果分别见 [initial](results/v5/initial-v5.json)、[sync](results/v5/sync-v5.json)、[settlement](results/v5/settlement-v5.json)、[renames](results/v5/renames-v5.json)。'
put('PERFORMANCE',text)
f100=med([x for x in fixed if x['files']==100000 and x['mode']=='S2'],'wall_s');beta=summary['checkout 乙 b→a']['S2']['median'];beta2=summary['checkout 乙 a→b']['S2']['median'];over=(summary['LATTICE 全量构建']['S2']['median']/summary['LATTICE 全量构建']['ns']['median']-1)*100
fgit=summary['git_fixed']['100000']['median']
summary['targets']={'fixed_100k_s':f100,'fixed_git_100k_s':fgit,'fixed_pass':max(f100,fgit)<=.05,'checkout_b_to_a_s':beta,'checkout_a_to_b_s':beta2,'checkout_pass':max(beta,beta2)<=2,'build_overhead_vs_ns_percent':over,'epsilon_complete':False}
put('TARGETS',table(['判断目标','结论','实测/理由'],[['100,000 条目无改动固定开销 ≤50 ms','达标'if max(f100,fgit)<=.05 else'未全面达标',f'普通树 {f100*1000:.3f} ms（100,100 条目）；10 万跟踪文件含 Git 快照 {fgit*1000:.3f} ms'],['情形乙 4,000 文件 checkout ≤2 s','达标'if max(beta,beta2)<=2 else'未达标',f'b→a {beta:.3f} s；a→b {beta2:.3f} s'],['构建开销与 ns 相当','幅度相当，未做等效性统计',f'相对 ns {over:+.2f}%'],['ε 与全量扫描完全一致','未达标','常规与随机用例另表；跨 act mmap 与未监听别名有反例']]))
coverage=read('coverage');random=read('randomized');crashes=read('crashes');win=read('commit-window');complete=read('completeness');supp=read('supplement');concurrent=read('concurrent')
window=[n/1000 for x in win if x['injection'] is None for n in x['result'].get('check_to_rename_ns',[])];summary['window_us']=stats([{'x':x}for x in window],'x')
rows=[['第四轮 63 类 × commit/discard',f"{sum(x.get('pass',False)for x in coverage)}/{len(coverage)}",'独立 oracle、namespace 对照、ε、应用、丢弃、新进程 revert'],['随机 5 seed × 两侧 ×20 act',f"{sum(x['pass']for x in random)}/{len(random)}",f"每 act 20 次选择，共 4,000 次（缺失目标可跳过）；{sum(x['finish'].get('uncertain_history',False)or x['begin'].get('uncertain_history',False)for x in random)} 个 act 因历史不确定回退全扫"],['运行中外部并发写',f"{sum(x['external_preserved']and x['external_not_in_epsilon']for x in concurrent)}/{len(concurrent)}",'同文件 commit 报冲突；其他文件不混入 ε'],['真实 IN_Q_OVERFLOW',str(next(x for x in complete if x['case']=='kernel-overflow')['overflow']),'暂停节点后产生 20,000 文件事件；回退结果与独立全扫一致'],['SIGKILL 后重启及下一 act',f"{sum(x['pass']for x in crashes)}/{len(crashes)}",'12 阶段 ×5，含目录 rename 后'],['提交准备后外部写','见 commit-window 原始结果','before-expected 与 after-ready 两点；真实最后核对保护'],['最后核对后外部写','仍可覆盖','after-check 注入；不是原子 CAS']]
put('CORRECTNESS',table(['验证','结果','口径'],rows)+f"\n\n无注入时核对开始到 rename 完成（含 stat 的窗口包络）：中位数 **{statistics.median(window):.3f} µs**，范围 {min(window):.3f}–{max(window):.3f} µs（3 次）。[窗口原始结果](results/v5/commit-window-v5.json)、[候选完整性反例](results/v5/completeness-v5.json)、[随机负载](results/v5/randomized-v5.json)、[崩溃与重启](results/v5/crashes-v5.json)、[补充验证](results/v5/supplement-v5.json)、[停机外部写](results/v5/downtime-v5.json)均保留。")
git=read('git-discard');idx=read('git-index');rows=[]
for interference in ['none','after-end-index','during-act-refresh','during-act-stage']:
    a=[x for x in git if x['interference']==interference];rows.append([interference,len(a),sum(x['status_consistent']for x in a),sum(x['result']['outcome']=='Conflict'for x in a),sum(x['external_index_preserved'] is True for x in a),sum(x['host_staging_preserved']for x in a)])
def scans(out):return sum(int(x['value'])for x in out['trace']if x.get('key')=='refresh/sum_scan')
text=table(['干扰时序','样本数','status 回初态','报告 Conflict','保留外部 index 字节','保留宿主 stage'],rows)+'\n\n索引往返中位数（ms）；`sum_scan` 是 Git 内容核对路径计数，不是 stat 次数。沙箱列包含 namespace 启动：\n\n'
rows=[]
for n in [10000,100000]:
    for m in ['shared','private']:
        a=[x for x in idx if x['files']==n and x['mode']==m];rows.append([n,m,f"{med(a,lambda x:x['host']['wall_s'])*1000:.3f}",int(med(a,lambda x:scans(x['host']))),f"{med(a,lambda x:x['inside_after_host']['exec_s'])*1000:.3f}",int(med(a,lambda x:scans(x['inside_after_host']))),a[0]['index_bytes']])
text+=table(['文件数','索引','宿主 status','宿主 sum_scan','再次沙箱 status','沙箱 sum_scan','索引字节'],rows)+'\n\n[Git 撤销样本](results/v5/git-discard-v5.json)、[索引往返](results/v5/git-index-v5.json)、[私有索引语义反例](results/v5/git-private-semantics-v5.json)。'
summary['git_index']=rows;put('GIT',text)
rows=[]
for n in [3000,10000,100000]:
    a=[x for x in initial if x['files']==n];rss=[int(re.search(r'VmRSS:\s+(\d+)',x['stats']['status'])[1])for x in a];rows.append([n+100,a[0]['real_watches'],a[0]['shadow_watches'],f'{statistics.median(rss)/1024:.2f}'])
put('WATCHES',table(['条目数','真实 watch','影子 watch','节点 RSS MiB（中位数）'],rows)+'\n\n监听上限实测为 **250,895 / UID**。两棵树均需要每目录一个 watch，忽略同 UID 其他程序时最多约 125,447 个被跟踪目录；文件数本身不等于 watch 数。该规模夹具把文件分在 100 个目录，故不能用 100,000 文件通过证明任意目录布局都可用。额外 [watch 密度样本](results/v5/watch-density-v5.json)记录 1,000/10,000 目录布局。RSS 包括 Rust 两棵索引和线程等进程内存，不含内核 watch slab；没有权限隔离同 UID 其他消费者，也没有将内核内存估算冒充测量。')
(HERE/'REPORT-v5.md').write_text(report);(R/'summary-v5.json').write_text(json.dumps(summary,ensure_ascii=False,indent=2)+'\n')
# 无第三方资源的独立图，使用 T3 主题变量。
charts=[]
for title in ['LATTICE 全量构建','checkout 甲 a→b','checkout 乙 a→b']:
    data=summary[title];maximum=max(v['median']for v in data.values());bars=[]
    for i,m in enumerate(MODES):
        v=data[m]['median'];bars.append(f'<div class="row"><span>{m}</span><div class="track"><div class="bar" style="width:{v/maximum*100:.2f}%;background:var(--chart-{i+1},currentColor)"></div></div><b>{v:.3f} s</b></div>')
    charts.append('<section><h3>'+title+'</h3>'+''.join(bars)+'</section>')
html='<!doctype html><html lang="zh"><meta charset="utf-8"><style>:root{font-family:var(--font-sans,system-ui);color:var(--foreground)}body{margin:0}section{margin:0 0 24px}h3{font-size:15px;margin:4px 0 12px}.row{display:grid;grid-template-columns:48px 1fr 90px;align-items:center;gap:12px;height:28px;font-size:13px}.track{height:14px}.bar{height:100%;min-width:2px;border-radius:3px}b{text-align:right;font-family:var(--font-mono,monospace);font-weight:400}p{font-size:12px;color:var(--muted-foreground)}</style><body>'+''.join(charts)+'<p>每配置 3 次，完整 act 中位数；初建单列。性能收益不代表 mmap、提交窗口与共享 Git 归属问题已经解决。</p></body></html>'
(R/'performance-v5.html').write_text(html)
print(json.dumps(summary['targets'],ensure_ascii=False,indent=2))
