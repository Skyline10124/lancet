#!/usr/bin/env python3
"""核对保存的实验观测和报告材料；不把已知反例当作产品验收通过。"""
from datetime import datetime, timezone
import hashlib
import json
import os
from pathlib import Path
import re
from experiment import HERE, RESULTS, SCRATCH, MODES, dump

def read(name):return json.loads((RESULTS/name).read_text())

def main():
    checks=[]
    coverage=read('coverage.json');assert len(coverage)==234
    assert all(not r['prestate_bad'] and not r['external_registered'] for r in coverage)
    assert all(r['returncode']==(124 if r['case']=='deadline' else 0) for r in coverage)
    checks.append('234 个基础运行：已留存的前态均与 oracle 相同；3 个 deadline=124，其余 runner=0；外部文件均未登记')
    for mode in ['A','B1','B2']:
        rows=[r for r in read('races.json') if r['mode']==mode];assert len(rows)==3
        hits=[r['observations'][0] for r in rows]
        assert sum(h['failed'] for h in hits)==0
        assert sum(h['evil_hits'] for h in hits)==(0 if mode=='B2' else 600)
    checks.append('三种机制各 600 次实际 fd inode 观测：A/B1 绕过，B2 抵御此次内存改写')
    for r in read('crash.json'):
        assert r['result']['returncode']==-9
        assert r['before']['race-safe']['sha256']==r['after']['race-safe']['sha256']
        assert (r['before']['b']['sha256']!=r['after']['b']['sha256'])==(r['mode']!='A')
        assert len(r['result']['ledger'])==1
        assert r['result']['ledger'][0]['clone_sha256']==r['before']['race-safe']['sha256']
    checks.append('3 个真实节点 SIGKILL：写前 clone 有效；B 仍可向旧 fd 写入，A 不写入')
    benchmarks=read('benchmarks.json');assert len(benchmarks)==90
    for mode in MODES:
        for task in ['build-full','build-incremental','test-subset','git-status','git-checkout','git-commit']:
            rows=[r for r in benchmarks if r['mode']==mode and r['name'].rsplit('-',2)[0]==task]
            assert len(rows)==3
            assert all(r['returncode']==(1 if task=='git-commit' else 0) for r in rows)
    assert len(read('micro.json'))==15 and all(r['returncode']==0 for r in read('micro.json'))
    checks.append('90 个工作负载运行：75 个成功，15 个 git commit 前置失败；15 个微基准成功，均为五配置各三次')
    live=[]
    for p in Path('/proc').glob('[0-9]*'):
        try:
            cmd=(p/'cmdline').read_bytes().split(b'\0')
            if cmd and cmd[0] in [os.fsencode(SCRATCH/'intercept'),os.fsencode(SCRATCH/'probe')]:
                live.append({'pid':int(p.name),'command':b' '.join(cmd).decode(errors='replace')})
        except OSError:pass
    assert not live,live
    checks.append('未发现本实验 intercept/probe 存活进程；已结束的命名空间随进程回收')
    manifest={}
    for p in sorted(HERE.iterdir()):
        if p.is_file() and p.name!='REPORT.md':manifest[p.name]=hashlib.sha256(p.read_bytes()).hexdigest()
    manifest['.scratch/intercept']=hashlib.sha256((SCRATCH/'intercept').read_bytes()).hexdigest()
    manifest['.scratch/probe']=hashlib.sha256((SCRATCH/'probe').read_bytes()).hexdigest()
    report=dict(utc=datetime.now(timezone.utc).isoformat(),checks=checks,remaining_processes=live,
                files_sha256=manifest,product_acceptance=False,
                unresolved=['三种原型均有覆盖缺口，详见 REPORT.md','git commit 未完成，bot-run 前置失败'])
    dump(RESULTS/'verification.json',report)
    missing=[]
    for p in [HERE/'README.md',HERE/'REPORT.md',RESULTS/'performance.md']:
        for target in re.findall(r'\]\(([^)]+)\)',p.read_text()):
            if '://' in target or target.startswith('#'):continue
            target=target.split('#')[0]
            if not (p.parent/target).exists():missing.append([str(p),target])
    assert not missing,missing
    checks.append('README、REPORT 与性能表的相对链接目标存在；无本地锚点待核对')
    assert not [(str(p),i) for p in HERE.iterdir() if p.is_file()
                for i,s in enumerate(p.read_text().splitlines(),1) if s.rstrip()!=s]
    checks.append('原型顶层源码/文档无行尾空白')
    dump(RESULTS/'verification.json',report)
    print(json.dumps(report,ensure_ascii=False,indent=2))

if __name__=='__main__':main()
