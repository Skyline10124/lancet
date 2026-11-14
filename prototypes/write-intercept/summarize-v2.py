#!/usr/bin/env python3
"""仅从第二轮原始结果生成计数、耗时表与可审查的 HTML 图。"""
import csv
import json
from pathlib import Path
import statistics
HERE=Path(__file__).resolve().parent
R=HERE/'results/v2'

def load(name):return json.loads((R/name).read_text())
def save(name,data):(R/name).write_text(json.dumps(data,ensure_ascii=False,indent=2)+'\n')
def patch(marker,content):
    p=HERE/'REPORT-v2.md';s=p.read_text();start=f'<!-- {marker}:START -->';end=f'<!-- {marker}:END -->'
    a=s.index(start)+len(start);b=s.index(end);p.write_text(s[:a]+'\n'+content+'\n'+s[b:])
def main():
    coverage=load('coverage.json');replacements=load('replacements.json');supp=load('supplement.json');crashes=load('crashes.json');cancel=load('cancellations.json');params=load('parameter-races.json')
    counts=[]
    for case,pattern in sorted({(r['case'],r['pattern']) for r in replacements}):
        rows=[r for r in replacements if (r['case'],r['pattern'])==(case,pattern)]
        counts.append(dict(case=case,pattern=pattern,n=len(rows),detected=sum(r['result']['stats']['indeterminate'] for r in rows),evil_changed=sum(r['before']['race-evil']!=r['after']['race-evil'] for r in rows)))
    for case in ['rename-destination','symlink-aba','symlinkat-aba']:
        rows=[r for r in supp if r['case']==case]
        counts.append(dict(case=case,pattern='destination' if case=='rename-destination' else 'post-execution',n=len(rows),detected=sum(r['result']['stats']['indeterminate'] for r in rows)))
    death=[]
    for mode in ['B2','B2v2']:
        rows=[r for r in crashes if r['mode']==mode];death.append(dict(mode=mode,n=len(rows),old_fd_writes=sum(r['before']['b']!=r['after']['b'] for r in rows)))
    memory=dict(n=sum(r['observations'][0]['safe_hits']+r['observations'][0]['evil_hits']+r['observations'][0]['failed'] for r in params),evil_hits=sum(r['observations'][0]['evil_hits'] for r in params))
    summary=dict(coverage_runs=len(coverage),parameter_race=memory,replacements=counts,crashes=death,cancellations=dict(n=len(cancel),detected=sum(r['result']['stats']['indeterminate'] for r in cancel)))
    save('summary.json',summary)
    rules=[
        ('1 对象绑定','通过','参数改写 0/600 绕过；沙箱内/外路径替换各 20 次，evil 均未改','parameter-races.json、replacements.json'),
        ('2 目录项 inode 核对','未通过','380 次目录项替换；unlink/rmdir remove-old、rename 目标端、symlink ABA 均存在 0/20 检出组','replacements.json、supplement.json'),
        ('3 硬链接','部分通过','三种跨类新建 EXDEV；三种已有别名写入原 inode 还原；删除已有别名的恢复仍破坏拓扑，改名恢复已修复','coverage.json、supplement.json、benchmarks.json'),
        ('4 账本与还原','部分通过','112 个 B2v2 覆盖运行及最小还原无语义差异；额外硬链接目录项反例；40 次取消仅保留证据、不自动恢复','coverage.json、supplement.json、cancellations.json'),
        ('5 hidden','通过','15 种入口各 1 次，均 EACCES，目的对象未建','coverage.json'),
        ('6 fd 卫生','通过','3 种 inherited 探针均 EBADF，原文件未改；启动 socket 在 exec 前关闭','coverage.json'),
        ('7 失效联动','未通过',f"直接 PDEATHSIG 链仍 {death[1]['old_fd_writes']}/{death[1]['n']} 次旧 fd 写入；代执行后取消 40/40 进入 Indeterminate",'crashes.json、cancellations.json'),
        ('8 子进程上下文','部分通过','cwd/root/dirfd、umask、五种 resolve 与权限正反例符合 ns；未完整实现并发 fs/凭据快照及全部 openat2/VFS 边缘','coverage.json'),
        ('9 开销','通过','四配置各 3 轮真实工作负载；20,000 次 open/close；1,000 文件目录 rename 及批量落盘各 3 次','benchmarks.json、timings.json')]
    table='| 规则 | 结论 | 用例与次数 | 原始数据（results/v2/） |\n| --- | --- | --- | --- |\n'
    for a,b,c,files in rules:table+=f'| {a} | **{b}** | {c} | '+ '、'.join(f'[{f}](results/v2/{f})' for f in files.split('、'))+' |\n'
    patch('RULES',table)
    rate='| 用例 | 替换方式 | 检出 / 次数 | evil 被改 |\n| --- | --- | ---: | ---: |\n'
    for r in counts:rate+=f"| {r['case']} | {r['pattern']} | {r['detected']}/{r['n']} | {r.get('evil_changed','—')} |\n"
    (R/'replacement-rates.md').write_text(rate)
    if not (R/'timings.json').exists():return
    bench=load('benchmarks.json');timings=load('timings.json');rows=[]
    for r in bench+timings:
        if r['returncode']!=0:continue
        name=r['name'];task=name.removesuffix('-'+name.split('-')[-1]);task=task.split('-'+r['mode'])[0]
        label=r['mode']+('-batch' if r['environment'].get('WI_BATCH') else '')
        rows.append(dict(task=task,mode=label,seconds=r['stats']['wall_s'],digest_s=r['stats']['digest_s']))
    groups=[]
    for task,mode in sorted({(r['task'],r['mode']) for r in rows}):
        xs=[r for r in rows if (r['task'],r['mode'])==(task,mode)];values=[r['seconds'] for r in xs]
        groups.append(dict(task=task,mode=mode,n=len(xs),median_s=statistics.median(values),min_s=min(values),max_s=max(values),digest_median_s=statistics.median(r['digest_s'] for r in xs)))
    save('performance.json',groups)
    with (R/'performance.csv').open('w') as f:
        w=csv.DictWriter(f,fieldnames=list(groups[0]));w.writeheader();w.writerows(groups)
    tasks=['build-full','build-incremental','test-subset','git-status','git-checkout','git-commit','micro','rename-large']
    table='三次有效运行的中位数，单位秒；范围与每个样本见 [performance.csv](results/v2/performance.csv)。\n\n| 工作负载 | 无沙箱 | 仅命名空间 | 第一轮 B2 | 第二轮 B2 |\n| --- | ---: | ---: | ---: | ---: |\n'
    for task in tasks:
        vals=[]
        for mode in ['none','ns','B2','B2v2']:
            g=next((g for g in groups if g['task']==task and g['mode']==mode),None);vals.append(f"{g['median_s']:.6f}" if g else '未完成')
        table+=f'| {task} | '+' | '.join(vals)+' |\n'
    batch=next(g for g in groups if g['task']=='rename-large' and g['mode']=='B2v2-batch')
    table+=f"\nB2v2 批量持久化的目录 rename：{batch['median_s']:.6f} s（n={batch['n']}）。图见 [performance.html](results/v2/performance.html)，原始工作负载见 [benchmarks.json](results/v2/benchmarks.json)，微基准见 [timings.json](results/v2/timings.json)。\n"
    patch('PERFORMANCE',table)
    summary['performance']=groups;save('summary.json',summary)
    html='''<!doctype html><html lang="zh"><meta charset="utf-8"><meta name="viewport" content="width=device-width"><style>
    body{font-family:var(--font-sans, sans-serif);color:var(--foreground,#202830);margin:0}section{margin:0 0 20px}h3{font-size:16px;margin:12px 0}p{font-size:12px;color:var(--muted-foreground,#75808b)}.row{display:grid;grid-template-columns:110px 1fr 90px;gap:8px;align-items:center;margin:8px 0;font-size:13px}.bar{height:19px;background:var(--chart-1,#4393c3);border-radius:3px}.b1{background:var(--chart-2,#4dba90)}.b2{background:var(--chart-3,#e4ae49)}.b3{background:var(--chart-4,#e56d68)}.track{height:19px;background:var(--secondary,#e5e8ed)}.num{font-family:var(--font-mono,monospace);text-align:right}
    </style><body>'''
    for task,title in [('build-full','全工作区构建'),('micro','20,000 次 open + close'),('rename-large','改名含 1,000 个文件的目录')]:
        gs=[next(g for g in groups if g['task']==task and g['mode']==mode) for mode in ['none','ns','B2','B2v2']];mx=max(g['median_s'] for g in gs)
        html+=f'<section><h3>{title}</h3>'
        for i,(g,label) in enumerate(zip(gs,['无沙箱','仅命名空间','第一轮 B2','第二轮 B2'])):
            html+=f'<div class="row"><span>{label}</span><div class="track"><div class="bar b{i}" style="width:{100*g["median_s"]/mx:.4f}%"></div></div><span class="num">{g["median_s"]:.4f} s</span></div>'
        html+='</section>'
    html+='<p>各配置 n=3，中位数；共享主机、未清页缓存。第一轮代码保持原样。B2v2 每通知使用一个上下文 worker。数据来自 results/v2/performance.json。</p></body></html>'
    (R/'performance.html').write_text(html)
    print(table)

if __name__=='__main__':main()
