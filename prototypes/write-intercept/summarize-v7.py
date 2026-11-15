#!/usr/bin/env python3
"""从原始样本生成五配置中位数；历史列只读取第六轮结果。"""
import json,statistics,hashlib
from pathlib import Path
P=Path(__file__).resolve().parent;R=P/'results/v7';OLD=P/'results/v6'
def read(name,old=False):
 p=(OLD if old else R)/(name+('-v6.json' if old else '-v7.json'))
 return json.loads(p.read_text()) if p.exists() else []
def med(rows,key):
 vals=[r[key] for r in rows if key in r]
 return statistics.median(vals) if vals else None
def ms(x):return '—' if x is None else f'{1000*x:.3f}'
def sec(x):return '—' if x is None else f'{x:.6f}'
modes=['none','ns','B3（v6）','S3（v6）','S3（v7）']
def samples(name,mode):
 return [r for r in read(name,mode in modes[2:4]) if r.get('mode')==({'B3（v6）':'B3','S3（v6）':'S3','S3（v7）':'S3'}.get(mode,mode))]
def table(head,rows):return '| '+' | '.join(head)+' |\n| '+' | '.join(['---']*len(head))+' |\n'+'\n'.join('| '+' | '.join(map(str,r))+' |'for r in rows)+'\n'
summary={};parts=[]
rows=[]
for label,name,sel in [('LATTICE 全量构建','builds',lambda r:True),('checkout 甲 a→b','checkouts',lambda r:r['scenario']=='A'),('checkout 乙 b→a','checkouts',lambda r:r['scenario']=='B' and r['target']=='a'),('checkout 乙 a→b','checkouts',lambda r:r['scenario']=='B' and r['target']=='b')]:
 values=[med([r for r in samples(name,m) if sel(r)],'wall_s') for m in modes];summary[label]=dict(zip(modes,values));rows.append([label,*map(sec,values)])
parts+=['五配置总时间（秒；每格至少三个样本，历史两列明确标记）：',table(['负载',*modes],rows)]
rows=[]
for path in ['/dev/null','a']:
 vals=[statistics.median(r['observation']['us_per_call'] for r in samples('micro',m) if r['path']==path) if samples('micro',m) else None for m in modes]
 rows.append([path,*['—' if x is None else f'{x:.3f}'for x in vals]])
parts+=['20,000 次 open+close，探针内部单次中位数（µs）：',table(['路径',*modes],rows)]
rows=[]
for n in [3000,10000,100000]:
 vals=[]
 for m in modes:
  data=[r for r in (read('fixed-final') if m=='S3（v7）' and read('fixed-final') else samples('fixed',m)) if r['files']==n]
  vals.append(med(data,'wall_s'))
 summary[f'fixed-{n}']=dict(zip(modes,vals));rows.append([n,*map(ms,vals)])
parts+=['空 act RPC 墙钟（ms；none/ns/B3 含 true，S3 为 begin+finish+commit）：',table(['文件数（另加 1,000 目录）',*modes],rows)]
rows=[]
for scenario,t in [('A','b'),('B','a'),('B','b')]:
 data=[r for r in samples('checkouts','S3（v7）') if r['scenario']==scenario and r['target']==t]
 columns=[statistics.median(f(r) for r in data) if data else None for f in [lambda r:r['begin']['sync_s'],lambda r:r['exec_s'],lambda r:r['finish']['scan_s'],lambda r:r['settlement']['prepare_s'],lambda r:r['settlement']['apply_s'],lambda r:r['settlement'].get('index_s',0),lambda r:r['settlement']['rebuild_s'],lambda r:r['wall_s']-r['begin']['sync_s']-r['exec_s']-r['finish']['scan_s']-r['settlement']['commit_s'],lambda r:r['wall_s']]]
 summary['checkout-components-'+scenario+'-'+t]=columns;rows.append([('甲 'if scenario=='A'else'乙 ')+('b→a'if t=='a'else'a→b'),*map(sec,columns)])
parts+=['本轮情形甲/乙分项（秒；每列独立中位数）：',table(['方向','begin','命令','结束候选','准备','应用/落盘','索引','重建','RPC差额','总计'],rows)]
rows=[]
for n in [3000,10000,100000]:
 data=[r for r in read('fixed-final') if r['files']==n]
 columns=[statistics.median(f(r) for r in data) if data else None for f in [lambda r:r['begin']['sync_s'],lambda r:r['finish']['scan_s'],lambda r:r['commit']['commit_s'],lambda r:r['wall_s']-r['begin']['sync_s']-r['finish']['scan_s']-r['commit']['commit_s'],lambda r:r['wall_s']]]
 rows.append([n,*map(ms,columns)])
parts+=['性能最终版本空 act 分项（ms；每样本前静置 110 ms，允许后台扫描到期）：',table(['文件数','begin','finish','commit','RPC差额','总计'],rows)]
rows=[]
for n in [1000,10000]:rows.append([n,*[sec(med([r for r in samples('renames',m) if r['files']==n],'wall_s'))for m in modes]])
parts+=['目录改名完整 act（秒；宿主复位同步在计时前）：',table(['目录内文件数',*modes],rows)]
rows=[]
for n in [1,100,4000]:
 for old in [True,False]:
  data=[r for r in read('merges',old)if r['mode']=='S3' and r['files']==n]
  rows.append(['S3 v6' if old else'S3 v7',n,ms(statistics.median(r['finish']['scan_s']for r in data))if data else'—',ms(statistics.median(r['commit']['commit_s']for r in data))if data else'—',ms(statistics.median(r['commit'].get('partial_visible_s',0)for r in data))if data else'—'])
parts+=['合并阶段（ms；none/ns/B3 原地写入，没有独立合并阶段）：',table(['配置','文件数','求改动集合','合并含索引/重建','中间可见包络'],rows)]
parts+=['初次建立影子（客户端墙钟秒；每规模三次）：',table(['文件数','S3 v6','S3 v7'],[[n,sec(med([r for r in read('initial',True) if r['files']==n],'process_init_s')),sec(med([r for r in read('initial') if r['files']==n],'process_init_s'))] for n in [3000,10000,100000]])]
parts+=['Git 索引（ms）：',table(['跟踪文件数','未变化 helper','未变化 begin RPC','宿主变化 helper','提交总计'],[[n,ms(statistics.median(r['begin']['git']['git_sync_s']for r in read('git-scale-warm')if r['files']==n))if read('git-scale-warm')else'—',ms(med([r for r in read('git-scale-warm')if r['files']==n],'rpc_s')),ms(statistics.median(r['begin']['git']['git_sync_s']for r in read('git-scale')if r['files']==n and r['rep']>0))if any(r['files']==n and r['rep']>0 for r in read('git-scale'))else'—',ms(statistics.median(r['commit']['commit_s']for r in read('git-scale')if r['files']==n))if any(r['files']==n for r in read('git-scale'))else'—']for n in [10000,100000]])]
parts+=['私有缓存及 target 种子（秒；各三次）：',table(['操作','中位数'],[['Cargo cache/index reflink',sec(med(read('private-cache'),'seed_s'))],['首次 cargo metadata 与源码解压',sec(med(read('private-cache'),'metadata_and_first_extract_s'))],['LATTICE target reflink',sec(med(read('target-seed'),'seed_s'))]])]
(R/'tables-v7.md').write_text('\n\n'.join(parts)+'\n');(R/'summary-v7.json').write_text(json.dumps(summary,ensure_ascii=False,indent=2)+'\n')
report=P/'REPORT-v7.md'
if report.exists():
 text=report.read_text();start=text.index('<!-- PERFORMANCE -->')+len('<!-- PERFORMANCE -->');end=text.index('<!-- /PERFORMANCE -->');report.write_text(text[:start]+'\n\n'+'\n\n'.join(parts)+'\n\n'+text[end:])
