#!/usr/bin/env python3
"""从原始运行重建中位数、相对开销和报告表格；失败不计作有效性能。"""
import csv
import html
import json
import statistics
from experiment import HERE, RESULTS, MODES, dump

LABELS={'build-full':'全工作区冷构建','build-incremental':'单源改动后构建',
        'test-subset':'net-guard 测试','git-status':'git status',
        'git-checkout':'git checkout -b','git-commit':'git commit',
        'micro':'20,000 次 open+close'}

def main():
    data=json.loads((RESULTS/'benchmarks.json').read_text())+json.loads((RESULTS/'micro.json').read_text())
    if len(data)!=105:
        raise RuntimeError(f'需要 90 个工作负载运行和 15 个微基准，当前只有 {len(data)}；不生成不完整的三次中位数表')
    summary=[]
    for task in LABELS:
        for mode in MODES:
            rows=[r for r in data if r['mode']==mode and r['name'].rsplit('-',2)[0]==task]
            ok=[r for r in rows if r['returncode']==0 and r['stats'].get('code')==0]
            row=dict(task=task,mode=mode,samples=len(rows),valid=len(ok))
            for key in ['wall_s','calls','registered','clones','digest_s']:
                vals=[r['stats'][key] for r in ok]
                row[key]=statistics.median(vals) if vals else None
                if key=='wall_s':row['min_s']=min(vals) if vals else None;row['max_s']=max(vals) if vals else None
            summary.append(row)
    for r in summary:
        base=next(x for x in summary if x['task']==r['task'] and x['mode']=='none')
        ns=next(x for x in summary if x['task']==r['task'] and x['mode']=='ns')
        r['overhead_percent']=(r['wall_s']/base['wall_s']-1)*100 if r['wall_s'] and base['wall_s'] else None
        r['over_ns_percent']=(r['wall_s']/ns['wall_s']-1)*100 if r['wall_s'] and ns['wall_s'] else None
    dump(RESULTS/'summary.json',summary)
    with (RESULTS/'summary.csv').open('w') as f:
        w=csv.DictWriter(f,fieldnames=list(summary[0]));w.writeheader();w.writerows(summary)
    table=['每格为中位数秒（相对无沙箱开销）；有效项各 3 次。失败项不提供性能估计。',
           '', '| 工作负载 | 无沙箱 | 仅命名空间 | A | B1 | B2 |', '| --- | --- | --- | --- | --- | --- |']
    for task,label in LABELS.items():
        cells=[]
        for mode in MODES:
            r=next(x for x in summary if x['task']==task and x['mode']==mode)
            cells.append(f"{r['wall_s']:.6f}（{r['overhead_percent']:+.1f}%）" if r['wall_s'] is not None else f"未完成（{r['samples']} 次失败）")
        table.append('| '+label+' | '+' | '.join(cells)+' |')
    table+=['','下面每格为“拦截次数 / 登记路径 / FICLONE 数 / 写后摘要毫秒”的中位数。无沙箱与 ns 的三种计数均为 0；失败提交为 —。',
            '', '| 工作负载 | A | B1 | B2 |','| --- | --- | --- | --- |']
    for task,label in LABELS.items():
        cells=[]
        for mode in ['A','B1','B2']:
            r=next(x for x in summary if x['task']==task and x['mode']==mode)
            cells.append(f"{r['calls']:.0f} / {r['registered']:.0f} / {r['clones']:.0f} / {r['digest_s']*1000:.4f}" if r['wall_s'] is not None else '—')
        table.append('| '+label+' | '+' | '.join(cells)+' |')
    table+=['','微基准每次 open+close 的平均墙钟（取三轮中位数）为：'+
            '、'.join(f"{m} {next(r for r in summary if r['task']=='micro' and r['mode']==m)['wall_s']/20000*1e6:.2f} µs" for m in MODES)+
            '。含 close 和循环成本，不能解释为内核单个 syscall 的纯执行时间。',
            '', '原始数据：[benchmarks.json](results/benchmarks.json)、[micro.json](results/micro.json)；机器可读汇总（含 min/max、相对 ns 开销）：[summary.csv](results/summary.csv)。']
    text='\n'.join(table)
    (RESULTS/'performance.md').write_text(text.replace('(results/','(')+'\n')
    report=HERE/'REPORT.md';s=report.read_text();start=s.index('<!-- PERFORMANCE:START -->')+len('<!-- PERFORMANCE:START -->');end=s.index('<!-- PERFORMANCE:END -->')
    report.write_text(s[:start]+'\n'+text+'\n'+s[end:])
    # 相对值条形图固定宽度比例，失败项不画，图中保留绝对值与样本数。
    chart=['<!doctype html><html lang="zh"><meta charset="utf-8"><style>',
           'body{font-family:var(--font-sans);color:var(--foreground);margin:0}p{color:var(--muted-foreground);font-size:12px}.panel{margin:16px 0}.row{display:grid;grid-template-columns:38px 1fr 115px;gap:8px;align-items:center;margin:5px 0;font-size:12px}.track{height:17px;background:var(--secondary);border-radius:3px}.bar{height:17px;background:var(--chart-1);border-radius:3px}.value{text-align:right;font-variant-numeric:tabular-nums}h4{margin:10px 0;font-size:14px}',
           '</style><p>墙钟中位数，3 次/配置；括号为相对无沙箱开销。原型及共享主机测量，不代表完整沙箱性能。</p>']
    for task in ['build-full','build-incremental','test-subset','micro']:
        rs=[r for r in summary if r['task']==task and r['wall_s'] is not None]
        maxval=max(r['wall_s'] for r in rs)
        chart.append('<section class="panel"><h4>'+html.escape(LABELS[task])+'</h4>')
        for i,r in enumerate(rs):
            value=f"{r['wall_s']:.3f}s ({r['overhead_percent']:+.1f}%)"
            chart.append(f'<div class="row"><span>{r["mode"]}</span><div class="track"><div class="bar" style="width:{r["wall_s"]/maxval*100:.2f}%;background:var(--chart-{i+1})"></div></div><span class="value">{value}</span></div>')
        chart.append('</section>')
    chart.append('<p>git commit 的 bot-run 前置检查失败，无有效提交时间。完整 syscall 计数、检查点及摘要时间见 REPORT.md。</p></html>')
    (RESULTS/'comparison.html').write_text(''.join(chart))
    print(text)

if __name__=='__main__':main()
