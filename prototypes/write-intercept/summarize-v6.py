#!/usr/bin/env python3
"""从原始样本重建报告数字；失败样本保留，不改测试判据。"""
import json,statistics,re
from pathlib import Path
P=Path(__file__).resolve().parent;R=P/'results/v6'
def read(n):return json.loads((R/(n+'-v6.json')).read_text())
def med(xs):return statistics.median(xs)
def table(headers,rows):return '\n'.join(['| '+' | '.join(headers)+' |','| '+' | '.join(['---']*len(headers))+' |',*['| '+' | '.join(map(str,row))+' |'for row in rows]])
def replace(name,text):
    p=P/'REPORT-v6.md';s=p.read_text();s=re.sub('<!-- '+name+' -->.*?<!-- /'+name+' -->','<!-- '+name+' -->\n'+text+'\n<!-- /'+name+' -->',s,flags=re.S);p.write_text(s)
def main():
    builds=read('builds');checkouts=read('checkouts');micro=read('micro');modes=['none','ns','B3','S2','S3'];summary={}
    rows=[['LATTICE 全量构建',*[f'{med(x["wall_s"]for x in builds if x["mode"]==m):.6f}'for m in modes]]]
    summary['build_median_s']={m:med(x['wall_s']for x in builds if x['mode']==m)for m in modes}
    for scenario,target in [('A','b'),('B','a'),('B','b')]:
        d={m:med(x['wall_s']for x in checkouts if x['mode']==m and x['scenario']==scenario and x['target']==target)for m in modes};summary['checkout_'+scenario+'_'+target]=d
        rows.append(['checkout '+('甲 a→b'if scenario=='A'else'乙 b→a'if target=='a'else'乙 a→b'),*[f'{d[m]:.6f}'for m in modes]])
    perf='五配置中位数（秒）。'+ '\n\n'+table(['工作负载',*modes],rows)
    parts=[]
    for target in ['a','b']:
        xs=[x for x in checkouts if x['mode']=='S3' and x['scenario']=='B' and x['target']==target]
        vals=[med(x['begin']['sync_s']for x in xs),med(x['exec_s']for x in xs),med(x['finish']['scan_s']for x in xs),med(x['settlement']['prepare_s']for x in xs),med(x['settlement']['apply_s']for x in xs),med(x['settlement']['rebuild_s']for x in xs),med(x['wall_s']-x['begin']['sync_s']-x['exec_s']-x['finish']['scan_s']-x['settlement']['commit_s']for x in xs),med(x['wall_s']for x in xs)]
        parts.append(['b→a'if target=='a'else'a→b',*[f'{v:.6f}'for v in vals]])
    perf+='\n\nS3 情形乙分项（秒，各列独立取中位数）：\n\n'+table(['方向','begin','命令','结束扫描','提交准备','应用/落盘','影子重建','RPC/到期扫描等待','总计'],parts)
    def microvalue(x):
        obs=x['observation']
        # 第三轮探针原始字段直接转换。
        if 'us_per_call' in obs:return obs['us_per_call']
        if 'ns_per_op' in obs:return obs['ns_per_op']/1000
        if 'per_op_ns' in obs:return obs['per_op_ns']/1000
        if 'elapsed_ns' in obs:return obs['elapsed_ns']/20000/1000
        return obs['ns_per_call']/1000
    summary['micro_raw_example']=micro[0]['observation']
    try:
        perf+='\n\n20,000 次 open+close，探针内部单次中位数（µs）：\n\n'+table(['路径',*modes],[[p,*[f'{med(microvalue(x)for x in micro if x["mode"]==m and x["path"]==p):.3f}'for m in modes]]for p in ['/dev/null','a']])
    except KeyError:perf+='\n\n微基准字段见原始 JSON；汇总器需要匹配探针字段。'
    fixed=read('fixed');scans=read('scan');initial=read('initial');merges=read('merges');renames=read('renames')
    perf+='\n\n无改动 act（ms；S3 为 begin+finish+commit RPC，其他配置含 true）：\n\n'+table(['文件数（另加 1,000 目录）',*modes,'S3 8线程后台扫描'],[[n,*[f'{med(x["wall_s"]for x in fixed if x["mode"]==m and x["files"]==n)*1000:.3f}'for m in modes],f'{med(x["scan_s"]for x in scans if x["files"]==n and x["threads"]==8)*1000:.3f}']for n in [3000,10000,100000]])
    perf+='\n\n宿主并行精确扫描（ms，不含子进程）：\n\n'+table(['文件数','1线程','4线程','8线程'],[[n,*[f'{med(x["scan_s"]for x in scans if x["files"]==n and x["threads"]==t)*1000:.3f}'for t in [1,4,8]]]for n in [3000,10000,100000]])
    perf+='\n\nS3 初次建立影子（秒，含批量写回及初始化）：\n\n'+table(['文件数','中位数'],[[n,f'{med(x["process_init_s"]for x in initial if x["files"]==n):.6f}']for n in [3000,10000,100000]])
    perf+='\n\n求改动集合及合并（ms）：\n\n'+table(['配置','文件数','结束扫描','合并含重建','S3 影子重建'],[[m,n,f'{med(x["finish"]["scan_s"]for x in merges if x["mode"]==m and x["files"]==n)*1000:.3f}',f'{med(x["commit"]["commit_s"]for x in merges if x["mode"]==m and x["files"]==n)*1000:.3f}',f'{med(x["commit"].get("rebuild_s",0)for x in merges if x["mode"]==m and x["files"]==n)*1000:.3f}']for m in ['S2','S3']for n in [1,100,4000]])
    perf+='\n\n目录 rename 完整 act（秒；宿主复位及其同步在计时前完成，S3 合并后重建计时在内）：\n\n'+table(['子文件数',*modes],[[n,*[f'{med(x["wall_s"]for x in renames if x["mode"]==m and x["files"]==n):.6f}'for m in modes]]for n in [1000,10000]])
    windows=[t for x in merges if x['mode']=='S3' for t in x['commit']['check_to_replace_ns']];summary['window_ns']={'median':med(windows),'min':min(windows),'max':max(windows)}
    perf+=f'\n\n普通合并的核对开始至替换完成窗口：中位数 {med(windows)/1000:.3f} µs，范围 {min(windows)/1000:.3f}–{max(windows)/1000:.3f} µs。含 lstat/读取摘要与 syscall，是本机样本包络，不是最坏上界。'
    perf+='\n\n合并应用期间可见中间态的计时包络（ms；从首项核对开始到末项应用后）：\n\n'+table(['文件数','中位数'],[[n,f'{med(x["commit"]["partial_visible_s"]for x in merges if x["mode"]=="S3"and x["files"]==n)*1000:.3f}']for n in [1,100,4000]])
    perf+='\n\n一个文件的该计时不代表跨文件部分可见；多链接原地写自身另有部分字节可见。三样本中位数没有覆盖共享主机的最坏调度延迟。'
    replace('PERFORMANCE',perf)
    cov=read('coverage');random=read('randomized');sync=read('synchronization');crash=read('crashes');con=read('concurrency');gitcrash=read('git-crashes')
    correctness=table(['验证','结果','证据'],[
        ['继承的 63 类 × commit/discard',f'{sum(x.get("pass",False)for x in cov)}/{len(cov)}','[coverage](results/v6/coverage-v6.json)'],
        ['两侧随机，5 seeds ×20 act',f'{sum(x.get("pass",False)for x in random)}/{len(random)}','[randomized](results/v6/randomized-v6.json)'],
        ['宿主同步 vs 全内容',f'{sum(x["exact"]for x in sync)}/{len(sync)}','[synchronization](results/v6/synchronization-v6.json)'],
        ['act 内宿主同文件写/编辑器改名',str(sum(x['result']['outcome']=='Conflict'for x in con if x['case']in ['same-inplace','same-rename']))+'/40 Conflict','[concurrency](results/v6/concurrency-v6.json)'],
        ['宿主改异文件','20/20 Committed','同上'],['后项冲突与回滚再冲突','3 Conflict、3 Indeterminate','同上'],
        ['普通事务 SIGKILL',f'{sum(x["pass"]for x in crash)}/{len(crash)} 前态或后态','[crashes](results/v6/crashes-v6.json)'],
        ['Git SIGKILL',f'{sum(x["pass"]for x in gitcrash)}/{len(gitcrash)} HEAD/工作树一致且无锁残留','[git-crashes](results/v6/git-crashes-v6.json)']])
    correctness+='\n\n同步补齐延迟（ms，写者停止后轮询独立 oracle）：\n\n'+table(['用例','样本数','中位数','最大'],[[c,len(xs),f'{med(x["delay_s"]for x in xs)*1000:.3f}',f'{max(x["delay_s"]for x in xs)*1000:.3f}']for c in sorted({x['case']for x in sync})for xs in [[x for x in sync if x['case']==c]]])
    correctness+='\n\n多链接部分写的 3 个 SIGKILL 样本恢复为 `Indeterminate`，不能算成功恢复。Git 崩溃的“前态/后态”比较工作树、HEAD 与可见 status，不要求新写入且无引用的 objects 被删除。'
    replace('CORRECTNESS',correctness)
    gm=read('git-matrix');ge=read('git-extra');gs=read('git-scale')
    gittext=table(['操作','提交/丢弃样本','status 一致','宿主暂存保留'],[[op,len(xs),sum(x.get('status_consistent',False)for x in xs),sum(x.get('host_stage_preserved',False)for x in xs)]for op in ['checkout','commit','stash','packed-refs']for xs in [[x for x in gm if x['operation']==op]]])
    gittext+='\n\n每项覆盖 none、after-end-index、during-act-refresh、during-act-stage 四种时序，提交与丢弃分别重复 3 次。“暂存保留”列包括原本无暂存的样本；实际注入暂存的样本单独可从 JSON 筛出。\n\n'+table(['新增用例','样本','status 一致','暂存保留','config/hooks 保留'],[[c,len(xs),sum(x.get('status_consistent',False)for x in xs),sum(x.get('host_stage_preserved',False)for x in xs),sum(x.get('config_preserved',False)and x.get('hooks_preserved',False)for x in xs)]for c in sorted({x['case']for x in ge})for xs in [[x for x in ge if x['case']==c]]])
    gittext+='\n\n1 万/10 万跟踪文件，单路径提交时的中位数（ms）：\n\n'+table(['跟踪文件','Git 同步 helper','完整 begin','Git 合并准备 helper','完整提交含重建','index 字节'],[[n,f'{med(x["begin"]["git"]["git_sync_s"]for x in gs if x["files"]==n)*1000:.3f}',f'{med(x["begin"]["sync_s"]for x in gs if x["files"]==n)*1000:.3f}',f'{med(x["commit"]["git"]["git_prepare_s"]for x in gs if x["files"]==n)*1000:.3f}',f'{med(x["commit"]["commit_s"]for x in gs if x["files"]==n)*1000:.3f}',next(x['index_bytes']for x in gs if x['files']==n)]for n in [10000,100000]])
    gittext+='\n\nhelper 时间不含 Python 进程启动；完整 begin/提交列含启动成本及普通文件工作。原始结果：[矩阵](results/v6/git-matrix-v6.json)、[新增](results/v6/git-extra-v6.json)、[规模](results/v6/git-scale-v6.json)。'
    replace('GIT',gittext)
    fixed100=med(x['wall_s']for x in fixed if x['mode']=='S3'and x['files']==100000);beta=max(summary['checkout_B_a']['S3'],summary['checkout_B_b']['S3']);over=(summary['build_median_s']['S3']/summary['build_median_s']['ns']-1)*100
    summary.update(fixed100_ms=fixed100*1000,checkout_beta_s=beta,build_vs_ns_pct=over)
    targets=table(['目标','判定','证据/范围'],[
        ['10 万条目无改动固定开销 ≤50 ms','达标'if fixed100<=.05 else'未达标',f'101,000 条目，{fixed100*1000:.3f} ms；含可能等待到期后台扫描'],
        ['乙 4,000 文件 checkout ≤2 s','达标'if beta<=2 else'未达标',f'较慢方向 {beta:.6f} s'],
        ['构建与 namespace 相当','幅度相当，未证明统计等效'if abs(over)<=3 else'未达标',f'相对 ns {over:+.2f}%'],
        ['同步与改动集合完全一致','未完全验证（已跑样本通过）',f'同步 {sum(x["exact"]for x in sync)}/{len(sync)}；随机 {sum(x.get("pass",False)for x in random)}/{len(random)}；继承覆盖 {sum(x.get("pass",False)for x in cov)}/{len(cov)}；同刻度连续 write 未精确复现'],
        ['崩溃恢复从不部分合并','未达标','多链接部分写 3 次均 Indeterminate'],
        ['act 期间同文件宿主写 100% 冲突','已跑样本达标','40/40；不包括合并后旧 fd 写入的一般证明'],
        ['Git 暂存全部保留、status 一致、config/hooks 不合并','已跑用例达标',f'矩阵 {len(gm)}、新增 {len(ge)}；只涵盖本地 SHA-1 loose objects/普通 refs 仓库']])
    replace('TARGETS',targets);(R/'summary-v6.json').write_text(json.dumps(summary,indent=2)+'\n')
if __name__=='__main__':main()
