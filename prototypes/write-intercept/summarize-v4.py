#!/usr/bin/env python3
"""只由保存的有效样本生成第四轮统计表与图。"""
import json
from pathlib import Path
import re
import statistics

HERE=Path(__file__).resolve().parent;R=HERE/'results/v4'
def read(name):return json.loads((R/name).read_text())
def med(xs):return statistics.median(xs)
def replace(name,text):
    p=HERE/'REPORT-v4.md';s=p.read_text();s=re.sub(r'<!-- '+name+r':START -->.*?<!-- '+name+r':END -->','<!-- '+name+':START -->\n'+text+'\n<!-- '+name+':END -->',s,flags=re.S);p.write_text(s)
def table(headers,rows):return '\n'.join(['| '+' | '.join(headers)+' |','| '+' | '.join(['---']*len(headers))+' |',*['| '+' | '.join(map(str,row))+' |' for row in rows]])
def scan_counts(out):return sum(int(e['value']) for e in out['trace'] if e['key']=='refresh/sum_scan')


def main():
    summary={};modes=['none','ns','B3','S'];perf=[]
    for filename,label in [('builds-v4.json','LATTICE 全量构建'),('checkouts-v4.json','4,000 文件 checkout')]:
        rows=read(filename);summary[filename]={}
        line=[label]
        for mode in modes:
            xs=[r['wall_s'] for r in rows if r['mode']==mode and r['returncode']==0]
            assert len(xs)==3,(filename,mode,len(xs))
            summary[filename][mode]=dict(n=len(xs),median_s=med(xs),min_s=min(xs),max_s=max(xs),samples_s=xs)
            line.append(f'{med(xs):.6f}')
        perf.append(line)
    parts=[]
    for filename,label in [('builds-v4.json','全量构建'),('checkouts-v4.json','checkout')]:
        rows=[r for r in read(filename) if r['mode']=='S']
        vals={k:med([r[k] for r in rows]) for k in ['sync_s','exec_s','scan_s','wall_s']}
        vals['commit_s']=med([r['settlement'].get('commit_s',0) for r in rows])
        summary[label+'-S']=vals
        parts.append([label,*[f'{vals[k]:.6f}' for k in ['sync_s','exec_s','scan_s','commit_s','wall_s']]])
    micro=[]
    for path in ['/dev/null','a']:
        row=[path]
        for mode in modes:
            xs=[x['observation']['us_per_call'] for x in read('micro-v4.json') if x['mode']==mode and x['path']==path]
            assert len(xs)==3
            row.append(f'{med(xs):.3f}')
        micro.append(row)
    summary['micro_us']=micro
    sync=[]
    for n in [3000,10000,100000]:
        row=[str(n+100)+' 条目（'+str(n)+' 文件）']
        for changed in [0,10,1000]:
            xs=[x['sync_s'] for x in read('sync-v4.json') if x['files']==n and x['changed']==changed]
            assert len(xs)==3
            row.append(f'{med(xs):.6f}')
        sync.append(row)
    settlement=[]
    for changed in [1,100,4000]:
        rows=[x for x in read('settlement-v4.json') if x['changed']==changed]
        assert len(rows)==3 and all(x['epsilon_count']==changed and x['outcome']=='Committed' for x in rows)
        settlement.append([changed,*[f'{med([r[k] for r in rows]):.6f}' for k in ['scan_s','prepare_s','apply_s','commit_s']]])
    initial=[]
    for n in [3000,10000,100000]:
        rows=[x for x in read('initial-v4.json') if x['files']==n]
        assert len(rows)==3
        initial.append([n,f'{med([r["init_total_s"] for r in rows]):.6f}',*[f'{med([r[k] for r in rows])/1048576:.2f}' for k in ['du_apparent_bytes','du_allocated_bytes','state_allocated_bytes']]])
    summary.update(sync_s=sync,settlement_s=settlement,initial=initial)
    b=summary['builds-v4.json'];overhead={m:100*(b[m]['median_s']/b['ns']['median_s']-1) for m in ['B3','S']}
    checkout=summary['checkouts-v4.json'];speed=checkout['B3']['median_s']/checkout['S']['median_s']
    summary.update(build_overhead_percent=overhead,checkout_speedup_B3_over_S=speed)
    replace('PERFORMANCE','\n\n'.join([
        '四配置中位数，秒；原始样本见 [builds-v4.json](results/v4/builds-v4.json)、[checkouts-v4.json](results/v4/checkouts-v4.json)。',
        table(['工作负载','无沙箱','仅命名空间','B3','S（总计）'],perf),
        f'相对 ns，全量构建 B3 {overhead["B3"]:+.2f}%，S {overhead["S"]:+.2f}%。S checkout 总时间为 B3 的 {1/speed:.1%}（{speed:.2f} 倍速度）；不等于无沙箱成本。',
        'S 分项中位数，秒；各列独立取中位数，故相加不一定等于总计。',
        table(['工作负载','同步','子进程','结束扫描','提交（含准备）','总计'],parts),
        '20,000 次 O_WRONLY open+close 的探针内部单次中位数，µs。普通文件没有实际改写，B3 只在第一次打开建前态。见 [micro-v4.json](results/v4/micro-v4.json)。',
        table(['路径','无沙箱','仅命名空间','B3','S'],micro),
        'act 前同步中位数，秒；包含两棵树 stat、变更内容摘要、增量物化与两个 JSON 快照落盘。目录条目另有 100 个。见 [sync-v4.json](results/v4/sync-v4.json)。',
        table(['规模','外部改 0 文件','外部改 10 文件','外部改 1,000 文件'],sync),
        '结束扫描与提交中位数，秒；固定 4,000 个常规文件。准备包含真实目录三方核对、前态与 ready 日志；应用包含后态及索引落盘。见 [settlement-v4.json](results/v4/settlement-v4.json)。',
        table(['改变文件数','扫描','准备','应用及结束','提交总计'],settlement),
        '初次建立中位数，各规模三次；秒 / MiB。包含初次摘要与索引建立，未预刷盘输入。三次均新建相同字节的输入文件，未把已刷盘输入混入初建中位数。',
        table(['文件数','初建秒','影子 apparent MiB','影子 allocated MiB','整个状态 allocated MiB'],initial),
        '统计范围和全部样本见 [summary-v4.json](results/v4/summary-v4.json)，图见 [performance-v4.html](results/v4/performance-v4.html)。'
    ]))
    git=[]
    for config in ['default','minimal-inside','minimal-noctime-inside','minimal-noctime-both']:
        for operation in ['status','add','commit','checkout']:
            rows=[r for r in read('git-v4.json') if r['config']==config and r['operation']==operation]
            assert len(rows)==3
            git.append([config,operation,f'{med([r["host"]["wall_s"] for r in rows])*1000:.3f}',
                        int(med([scan_counts(r['host']) for r in rows])),
                        f'{med([r["inside_after_host"]["exec_s"] for r in rows])*1000:.3f}',
                        int(med([scan_counts(r['inside_after_host']) for r in rows])),
                        str(sum(r['status_expected'] for r in rows))+'/3'])
    summary['git']=git
    replace('GIT',table(['配置','沙箱命令','宿主 status ms','宿主 sum_scan','再入沙箱 status ms（含启动）','影子 sum_scan','预期输出'],git)+'\n\n'+
        '原始结果见 [git-v4.json](results/v4/git-v4.json)。`*-inside` 只设沙箱进程环境变量，没有修改仓库配置；`*-both` 额外改变本次宿主进程设置用于诊断。计数是实测，不假定 minimal 一定减少全部扫描。')
    inc=read('incremental-v4.json');summary['incremental']=dict(compiled3=inc['compiled3'],compiled_host=inc['compiled_host'])
    replace('INCREMENTAL',f"act 3 编译项：`{'; '.join(inc['compiled3']) or '无'}`；宿主随后编译项：`{'; '.join(inc['compiled_host']) or '无'}`。act 1 / act 3 总时间分别 {inc['act1']['wall_s']:.3f} / {inc['act3']['wall_s']:.3f} 秒；宿主 {inc['host']['wall_s']:.3f} 秒。")
    coverage=read('coverage-v4.json');crashes=read('crashes-v4.json');external=read('concurrent-v4.json');restarts=read('restart-v4.json')
    assert len(coverage)==126 and all(r['pass'] for r in coverage)
    assert len(crashes)==55 and all(r['pass'] for r in crashes)
    assert all(r['external_preserved'] and r['external_not_in_epsilon'] for r in external)
    assert len(restarts)==5 and all(r['killed'] and r['restart_code']==0 and not r['difference'] and r['next_outcome']=='Unchanged' for r in restarts)
    replace('VALIDATION',table(['验证项','结论','证据与限制'],[
        ['1 正确性','适用覆盖通过，原型边界未完成','63 类 × commit/discard = 126/126；与 ns 实际行为一致，ε、提交、丢弃、新进程 revert 均核对；固定分类、xattr 策略、正式 fs.* 接线未实现'],
        ['2 外部并发写','运行期归属/提交前冲突通过，合并窗口仍有风险','12/12 运行保留外部字节；其他文件不进 ε；比对后外部写仍可丢失，discard 历史影子不保证'],
        ['3 提交崩溃恢复','55/55 SIGKILL 通过','11 阶段 × 5；新进程恢复前态/后态和账本；5/5 重启后下一 act 可用；未测断电/恢复中二次崩溃/停机外部写'],
        ['4 Git 交互','成功提交无虚假普通变更；缓存与丢弃问题存在','4 配置 × 4 命令 × 3；共享索引重新比对、HEAD/普通文件丢弃不一致及 minimal 漏检反例均保留'],
        ['5 跨 act 增量构建','见 Cargo 实际编译项','act 3：'+('; '.join(inc['compiled3']) or '无')+'；宿主随后：'+('; '.join(inc['compiled_host']) or '无')],
        ['6 性能','要求的四配置及规模样本已完成',f'全量/checkout/两类 micro 各 3；3×3 同步、3 类提交、3 类初建各 3；checkout S/B3={1/speed:.1%}'],
        ['7 复杂度','内核代理减少，持久同步/合并状态增加','附机制、进程、状态和实际行数；Python S 与 C B3 不等安全覆盖，行数仅描述原型']
    ]))
    b3=['intercept.c','intercept-v3.c','v3-node.h','v3-runtime.h','recover-v3.py'];sn=['shadow-v4.py','namespace-v4.c','recover-v4.py']
    loc={p:len((HERE/p).read_text().splitlines()) for p in b3+sn}
    summary['loc']=loc
    replace('LOC',f"物理行数（含空行、注释，非可维护性评分）：B3 执行/恢复源共 {sum(loc[p] for p in b3)} 行；S 执行/恢复源共 {sum(loc[p] for p in sn)} 行。B3 统计 `{', '.join(b3)}`；S 统计 `{', '.join(sn)}`。测试和结果生成脚本另计，不混入实现行数。")
    (R/'summary-v4.json').write_text(json.dumps(summary,ensure_ascii=False,indent=2)+'\n')
    chart_data=[dict(label=row[0],values=[float(x) for x in row[1:]],unit='s') for row in perf]+[dict(label='open+close '+row[0],values=[float(x) for x in row[1:]],unit='µs') for row in micro]
    html='''<!doctype html><html><head><meta charset="utf-8"><style>
:root {font-family:var(--font-sans, sans-serif);color:var(--foreground, CanvasText)}
body{margin:0}select{font:inherit;color:var(--foreground);background:var(--card);border:1px solid var(--border);padding:8px;border-radius:var(--radius)}
.chart{height:220px;margin-top:16px}.row{display:grid;grid-template-columns:90px 1fr 90px;align-items:center;gap:12px;height:48px}.track{height:24px;background:var(--secondary);border-radius:4px}.bar{height:24px;border-radius:4px}.num{font-variant-numeric:tabular-nums}.note{color:var(--muted-foreground);font-size:13px}
</style></head><body><select id="choice"></select><div class="chart" id="chart"></div><p class="note">三次中位数。S 构建/checkout 含同步、执行、扫描与提交；微基准是探针内部单次耗时。共享主机，未清页缓存。</p><script>
const data=DATA;const names=['无沙箱','命名空间','B3','S'];const choice=document.getElementById('choice');
data.forEach((d,i)=>choice.add(new Option(d.label,i)));function draw(){const d=data[choice.value],max=Math.max(...d.values);document.getElementById('chart').innerHTML=d.values.map((v,i)=>`<div class="row"><span>${names[i]}</span><div class="track"><div class="bar" style="width:${v/max*100}%;background:var(--chart-${i+1})"></div></div><span class="num">${v.toFixed(3)} ${d.unit}</span></div>`).join('')}choice.onchange=draw;draw();
</script></body></html>'''.replace('DATA',json.dumps(chart_data,ensure_ascii=False))
    (R/'performance-v4.html').write_text(html)
    print(json.dumps({k:summary[k] for k in ['build_overhead_percent','checkout_speedup_B3_over_S','incremental']},ensure_ascii=False,indent=2))


if __name__=='__main__':main()
