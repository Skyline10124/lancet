#!/usr/bin/env python3
"""从原始数据生成验收表、性能表与可独立打开的比较图。"""
import csv
import gzip
import json
from pathlib import Path
import statistics
import html
HERE=Path(__file__).resolve().parent;R=HERE/'results/v3'
def load(name):
    p=R/(name+'.json')
    return json.loads(p.read_text()) if p.exists() else json.loads(gzip.decompress(p.with_suffix('.json.gz').read_bytes()))
def describe(values):return dict(n=len(values),median=statistics.median(values),minimum=min(values),maximum=max(values),samples=values)
def main():
    summary={};csvrows=[]
    for task,file in [('build','builds'),('checkout','checkouts')]:
        rows=load(file);summary[task]={}
        for mode in ['none','ns','B2v2','B3']:
            valid=[r for r in rows if r['mode']==mode and r['returncode']==0]
            values=[r['stats']['wall_s'] for r in valid];summary[task][mode]=describe(values)
            csvrows.extend(dict(workload=task,mode=mode,route='',sample=r['name'],value=r['stats']['wall_s'],unit='s') for r in valid)
    summary['build_overhead_percent']=(summary['build']['B3']['median']/summary['build']['ns']['median']-1)*100
    rows=load('micro');summary['micro']={}
    for mode,route in [('none','native'),('ns','namespace'),('B2v2','emulate'),('B3','continue'),('B3','emulate')]:
        selected=[r for r in rows if (r['result']['mode'],r['route'])==(mode,route)]
        values=[r['result']['observations'][0]['us_per_call'] for r in selected];summary['micro'][mode+'-'+route]=describe(values)
        csvrows.extend(dict(workload='micro',mode=mode,route=route,sample=r['result']['name'],value=r['result']['observations'][0]['us_per_call'],unit='us') for r in selected)
    summary['directory']={}
    for size in [1000,10000]:
        selected=[r for r in load('directories') if r['size']==size];values=[r['result']['observations'][0]['seconds']*1000 for r in selected]
        summary['directory'][str(size)]=describe(values)
        csvrows.extend(dict(workload='rename-'+str(size),mode='B3',route='structured',sample=r['result']['name'],value=r['result']['observations'][0]['seconds']*1000,unit='ms') for r in selected)
    summary['directory_unflushed']={}
    for size in [1000,10000]:
        selected=[r for r in load('directories-unflushed') if r['size']==size]
        summary['directory_unflushed'][str(size)]=describe([r['result']['observations'][0]['seconds']*1000 for r in selected])
        csvrows.extend(dict(workload='rename-unflushed-'+str(size),mode='B3',route='structured',sample=r['result']['name'],value=r['result']['observations'][0]['seconds']*1000,unit='ms') for r in selected)
    summary['landlock']=[dict(**r['result']['observations'][0],unregistered_paths=len(r['regular_changes'])) for r in load('landlock-races')]
    ext=load('external-races');summary['external']=[]
    for case,pattern in [('unlink','remove'),('unlink','move'),('rename','move'),('rename','target'),('symlink','aba')]:
        selected=[r for r in ext if (r['case'],r['pattern'])==(case,pattern)];det=sum(r['result']['stats'].get('indeterminate',0) for r in selected)
        summary['external'].append(dict(case=case,pattern=pattern,total=len(selected),detected=det,missed=len(selected)-det))
    rows=load('crashes');summary['crashes']=dict(total=len(rows),enosys=sum(r['returned_errno']==38 for r in rows),no_return_observed=sum(r['returned_errno']==0 for r in rows),regular_fd_write=sum(r['fd_write']>0 for r in rows),allowed_fd_write=sum(r['allowed_write']>0 for r in rows),still_running=sum(bool(r['live_processes']) for r in rows),indeterminate=sum(r['recovery']['outcome']=='Indeterminate' for r in rows))
    summary['floor']={m:describe([r['us'] for r in load('notification-floor') if r['mode']==m and r['returncode']==0]) for m in ['native','cont','cont-read','addfd-read']}
    summary['groups']=[r['result']['stats']['max_batch'] for r in load('grouped')]
    summary['rule_status']=['部分通过','通过','通过','部分通过','通过','部分通过','通过','未通过']
    (R/'summary.json').write_text(json.dumps(summary,ensure_ascii=False,indent=2)+'\n')
    with (R/'performance.csv').open('w') as f:
        w=csv.DictWriter(f,fieldnames=['workload','mode','route','sample','value','unit'],lineterminator='\n');w.writeheader();w.writerows(csvrows)
    cont=summary['micro']['B3-continue']['median'];emu=summary['micro']['B3-emulate']['median'];overhead=summary['build_overhead_percent']
    status=lambda ok:'达标' if ok else '未达标'
    rules=f'''| 规则 | 结论 | 有效证据与限制 |
| --- | --- | --- |
| 1 Landlock 与 CONTINUE | 部分通过 | 七类改写各 200 次，1,400/1,400 拒绝、常规路径变化 0；空/缺失 target 构建均完成。预建硬链接 200/200 绕过；可写树之间 link、rename 各 1/1 绕过类别约束 |
| 2 目录锁 | 通过 | 内部四类 80 次、全目录锁 20 次均无锁内插入；外部 100 次检测 40、漏检 60，按已接受残余风险记录；冲突路径保留、无冲突 b 恢复 |
| 3 硬链接锚点 | 通过 | 四类各 3 次：操作实际成功、锚点存在、不同恢复进程还原后 inode/内容/mode/nlink 精确相等 |
| 4 结构化目录改名 | 部分通过 | 正确性通过，记录数不随文件数变化；预刷盘夹具 1k/10k 各 5 次约 2.00/2.24 ms，未刷盘则 7.94/49.64 ms，墙钟要求需要明确输入持久化条件 |
| 5 合并落盘 | 通过 | 八前态并发×10，观察并发 fsync，所有执行许可晚于本批完成；4,000 文件检出四配置各 3 次，串行 Git 的 max_batch=1，无数值目标 |
| 6 常驻 worker | 部分通过 | 一个进程、启动进入 user/mount 一次、宿主 PID namespace；26 对上下文中 24 对一致，动态 mount 的 2 对因 Landlock EPERM 不一致；其余边缘/连续上下文通过 |
| 7 失效联动 | 通过 | 50 次 SIGKILL；{summary['crashes']['enosys']} 次实际观察 ENOSYS，{summary['crashes']['no_return_observed']} 次未观察返回；旧 fd 写已登记对象 {summary['crashes']['regular_fd_write']} 次、允许层级 {summary['crashes']['allowed_fd_write']} 次；最终存活 0；改变项保守 Indeterminate、未变项零残余 |
| 8 性能 | 未通过 | CONTINUE {cont:.3f} µs：{status(cont<=30)}；代执行 {emu:.3f} µs：{status(emu<=30)}；全量构建 {overhead:+.2f}%：{status(overhead<=3)}；同类目录改名：结构成本达标、无条件墙钟成本未达标；密集检出已实测、目标待定 |'''
    perf='三次成功运行的中位数，单位秒。所有样本及范围见 [performance.csv](results/v3/performance.csv)、[summary.json](results/v3/summary.json)。\n\n| 工作负载 | 无沙箱 | 仅命名空间 | 第二轮 B2v2 | 本轮 B3 |\n| --- | ---: | ---: | ---: | ---: |\n'
    for task,label in [('build','LATTICE 全量构建'),('checkout','4,000 文件 checkout')]:perf+='| '+label+' | '+' | '.join(f"{summary[task][m]['median']:.6f}" for m in ['none','ns','B2v2','B3'])+' |\n'
    perf+=f'\n全量构建 B3 / ns 的中位数比值开销为 **{overhead:+.2f}%**，3% 指标**{status(overhead<=3)}**。\n\n| 20,000 次 open+close | 单次 µs | ≤30 µs |\n| --- | ---: | --- |\n'
    for key,label in [('none-native','无沙箱'),('ns-namespace','仅命名空间'),('B2v2-emulate','B2v2 代执行'),('B3-continue','B3 CONTINUE'),('B3-emulate','B3 代执行')]:
        value=summary['micro'][key]['median'];perf+=f"| {label} | {value:.3f} | {status(value<=30) if key.startswith('B3') else '对照'} |\n"
    perf+='\n不建检查点微基准均打开 `/dev/null`；B3 代执行使用 `WI_FORCE_EMULATE=1` 关闭快路，保留完整 worker 上下文复制和 ADDFD。两种路径均核对 `registered=0`。\n\n| 同类别目录改名 / 夹具 | 次数 | syscall 中位数 ms | 结构记录 / clone |\n| --- | ---: | ---: | --- |\n'
    for size in ['1000','10000']:perf+=f"| {int(size):,} 文件 / 已刷盘 | 5 | {summary['directory'][size]['median']:.3f} | 1 / 0 |\n"
    for size in ['1000','10000']:perf+=f"| {int(size):,} 文件 / 未刷盘 | 5 | {summary['directory_unflushed'][size]['median']:.3f} | 1 / 0 |\n"
    perf+='\n结构记录的算法没有遍历子树。未刷盘夹具的墙钟时间随规模增长，预刷盘后主要增长消失；这支持前次创建的持久化负担影响本次 fsync 的解释，但没有内核日志跟踪。原文没有指定输入持久化状态，本轮不据干净夹具宣称无条件墙钟成本达标。\n'
    perf+='\n独立通知下限（同步唤醒，三次中位数，µs）：'+ '；'.join(f"{m} {summary['floor'][m]['median']:.2f}" for m in summary['floor'])+'。见 [notification-floor.json](results/v3/notification-floor.json)。\n\n图见 [performance.html](results/v3/performance.html)。'
    report=HERE/'REPORT-v3.md';text=report.read_text()
    for marker,body in [('RULES',rules),('PERFORMANCE',perf)]:
        a=f'<!-- {marker}:START -->';b=f'<!-- {marker}:END -->';start=text.index(a)+len(a);end=text.index(b);text=text[:start]+'\n'+body+'\n'+text[end:]
    report.write_text(text)
    charts=[]
    labels={'none':'无沙箱','ns':'仅命名空间','B2v2':'第二轮','B3':'第三轮'}
    def chart(title,values,unit,n):
        maximum=max(v for label,v in values)*1.05
        bars=''.join(f'<div class="row"><span>{html.escape(label)}</span><div class="track"><div class="bar" style="width:{value/maximum*100:.3f}%"></div></div><b>{value:.2f}</b></div>' for label,value in values)
        return f'<section><p>{title}<small>中位数 · {unit} · n={n} · 越小越快</small></p>{bars}<div class="axis">0<span>{maximum:.1f} {unit}</span></div></section>'
    charts.append(chart('LATTICE 全量构建',[(labels[m],summary['build'][m]['median']) for m in labels],'s',3))
    charts.append(chart('4,000 文件检出',[(labels[m],summary['checkout'][m]['median']) for m in labels],'s',3))
    charts.append(chart('单次 open + close',[('无沙箱',summary['micro']['none-native']['median']),('第二轮',summary['micro']['B2v2-emulate']['median']),('第三轮放行',cont),('第三轮代执行',emu)],'µs',3))
    charts.append(chart('目录改名：持久化对照',[('1k / 未刷盘',summary['directory_unflushed']['1000']['median']),('10k / 未刷盘',summary['directory_unflushed']['10000']['median']),('1k / 已刷盘',summary['directory']['1000']['median']),('10k / 已刷盘',summary['directory']['10000']['median'])],'ms',5))
    page='''<!doctype html><html lang="zh"><head><meta charset="utf-8"><style>body{font-family:var(--font-sans,system-ui);color:var(--foreground,#222)}.plots{display:grid;grid-template-columns:repeat(auto-fit,minmax(300px,1fr));gap:22px}section{padding:0}p{font-weight:650;margin:0 0 14px}small{display:block;font-size:11px;color:var(--muted-foreground,#666);font-weight:400;margin-top:5px}.row{display:grid;grid-template-columns:92px 1fr 55px;align-items:center;gap:8px;height:33px;font-size:12px}.track{height:16px;background:var(--secondary,#eee)}.bar{height:16px;background:var(--chart-1,#3885c8);min-width:1px}b{font-variant-numeric:tabular-nums;font-weight:500;text-align:right}.axis{margin:8px 63px 0 100px;display:flex;justify-content:space-between;border-top:1px solid var(--border,#bbb);font-size:10px;color:var(--muted-foreground,#666)}.axis span{margin-left:auto}.note{margin-top:18px;font-size:12px;line-height:1.6;color:var(--muted-foreground,#666)}</style></head><body><div class="plots">'''+''.join(charts)+f'''</div><p class="note">单调用目标 ≤30 µs；构建开销目标 ≤3%，本轮 {overhead:+.2f}%。检出为副本中的两个受控提交；共享主机、每配置三次，完整样本保留在 performance.csv。</p></body></html>'''
    (R/'performance.html').write_text(page)
    print(json.dumps(summary,ensure_ascii=False,indent=2))
if __name__=='__main__':main()
