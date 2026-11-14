#!/usr/bin/env python3
"""验证证据自洽；预期的安全负例必须保留，不能等同于验收通过。"""
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
HERE=Path(__file__).resolve().parent
R=HERE/'results/v2'

def load(name):return json.loads((R/name).read_text())
def main():
    validations=[]
    def check(name,condition,detail=None):
        validations.append(dict(name=name,ok=bool(condition),detail=detail))
    cov=load('coverage.json');by={(r['mode'],r['case']):r for r in cov};v2=[r for r in cov if r['mode']=='B2v2']
    check('完整覆盖矩阵',len(cov)==224,len(cov))
    check('受控覆盖均正常结束',all(r['result']['returncode']==0 for r in cov))
    check('112 个最小求逆语义比较',all(not r['restoration']['errors'] and not r['restoration']['semantic_difference'] and r['restoration']['hardlink_inodes_preserved'] for r in v2))
    hidden=[r for r in v2 if r['case'].startswith('hidden-')]
    check('15 种 hidden 入口 EACCES',len(hidden)==15 and all(r['result']['observations'][-1]['errno']==13 and r['before']==r['after'] for r in hidden))
    inherited=[r for r in v2 if r['case'].startswith('inherited')]
    check('继承 fd 不可写',len(inherited)==3 and all(r['result']['observations'][-1]['errno']==9 and r['before']==r['after'] for r in inherited))
    for case in ['cross-target','cross-git','cross-target-git']:
        check(case,by['B2v2',case]['result']['observations'][-1]['errno']==18)
    check('target 内硬链接成功',by['B2v2','within-target']['result']['observations'][-1]['rc']==0)
    check('umask 0077',by['B2v2','umask']['after']['new']['mode']==0o600)
    for case in ['open-excl-fail','open-idle']:check(case+' 不进变更集',by['B2v2',case]['result']['changes']==[])
    xattr=[r for r in v2 if 'xattr' in r['case']];check('xattr 六入口 ENOTSUP',len(xattr)==6 and all(r['result']['observations'][-1]['errno']==95 for r in xattr))
    context=['umask','cwd','openat','chroot','nofollow','excl-symlink','tmpfile','tmpfile-empty','cloexec','not-cloexec']+[r['case'] for r in v2 if r['case'].startswith(('resolve-','perm-','tmpfs-perm'))]
    for case in context:
        a=by['ns',case]['result']['observations'][-1];b=by['B2v2',case]['result']['observations'][-1]
        check('上下文:'+case,(a['rc'],a['errno'])==(b['rc'],b['errno']))
    edges=load('context-edges.json')
    for i in range(0,len(edges),2):
        a=edges[i]['result']['observations'][0];b=edges[i+1]['result']['observations'][0]
        check('openat2 边缘:'+edges[i]['case'],(a['rc'],a['errno'])==(b['rc'],b['errno']))
    prestate_bad=[]
    def semantic(d):return {k:v for k,v in d.items() if k not in ['dev','ino','nlink']}
    for r in v2:
        for e in r['result']['ledger']:
            b=semantic(r['before'].get(e['relative'],{'type':'Absent'}))
            if e['prestate']!=b:prestate_bad.append((r['case'],e['relative']))
    check('clone 与类型前态符合 oracle',not prestate_bad,prestate_bad)
    params=load('parameter-races.json');check('参数竞态 600 次无绕过',sum(r['observations'][0]['safe_hits'] for r in params)==600 and all(r['observations'][0]['evil_hits']==r['observations'][0]['failed']==0 for r in params))
    reps=load('replacements.json');check('360 次路径替换数据',len(reps)==360)
    opens=[r for r in reps if r['case'].startswith('open')];check('内外路径替换 evil 未改',len(opens)==40 and all(r['before']['race-evil']==r['after']['race-evil'] for r in opens))
    weak=[r for r in reps if r['case'] in ['unlink','unlinkat','rmdir'] and r['pattern']=='remove-old'];check('保留 unlink/rmdir 漏检反例',len(weak)==60 and all(not r['result']['stats']['indeterminate'] for r in weak))
    strong=[r for r in reps if not r['case'].startswith('open') and r not in weak];check('可见替换均发现并停止',len(strong)==260 and all(r['result']['stats']['indeterminate'] and r['result']['returncode']==120 for r in strong))
    supp=load('supplement.json');aba=[r for r in supp if r['case'].endswith('-aba')];check('symlink ABA 漏检证据',len(aba)==40 and all(not r['result']['stats']['indeterminate'] and r['trace'][0]['original_inode']!=r['trace'][0]['replacement_inode'] for r in aba))
    dest=[r for r in supp if r['case']=='rename-destination'];check('rename 目标端漏检证据',len(dest)==20 and all(not r['result']['stats']['indeterminate'] for r in dest))
    hard=[r for r in supp if r['case'] in ['unlink-hardlink','rename-hardlink']];check('删除硬链接恢复失败如实保留',next(r for r in hard if r['case']=='unlink-hardlink')['topology']['after_same_inode']==False)
    check('改名硬链接恢复原 inode',next(r for r in hard if r['case']=='rename-hardlink')['topology']['after_same_inode'])
    cancel=load('cancellations.json');check('代执行后取消 40 次 Indeterminate',len(cancel)==40 and all(r['result']['stats']['indeterminate'] and r['result']['stats']['reply_failures']==1 for r in cancel))
    crashes=load('crashes.json');check('直接父子链死亡实验',len(crashes)==100 and all(r['result']['returncode']==-9 for r in crashes))
    deaths=[r for r in crashes if r['mode']=='B2v2'];check('保留旧 fd 死亡窗口反例',any(r['before']['b']!=r['after']['b'] for r in deaths),sum(r['before']['b']!=r['after']['b'] for r in deaths))
    bench=load('benchmarks.json');check('四配置六任务各三次',len(bench)==72 and all(r['returncode']==0 for r in bench))
    for r in bench:
        if 'link_calls' in r['stats']:check('cargo/link:'+r['name'],r['stats']['link_calls']==r['stats']['link_success'])
    commits=[r for r in bench if r['name'].startswith('git-commit')];check('普通 git 提交执行成功',len(commits)==12 and all('bot-run' not in r['command'] and 'bench@example.invalid' in ' '.join(r['command']) for r in commits))
    timings=load('timings.json');check('27 次微基准',len(timings)==27 and all(r['returncode']==0 for r in timings))
    # 使用仓库根路径，避免相对 cwd 使 pathspec 指向不存在的位置。
    p=subprocess.run(['git','--no-optional-locks','--no-pager','diff','--no-ext-diff','--no-textconv','--exit-code','origin/exp/write-intercept','--','prototypes/write-intercept/REPORT.md','prototypes/write-intercept/results',':(exclude)prototypes/write-intercept/results/v2'],cwd=HERE.parent.parent,capture_output=True,text=True)
    check('第一轮报告及已有 results 字节未变',p.returncode==0,p.stdout+p.stderr)
    p=subprocess.run(['git','--no-optional-locks','--no-pager','diff','--no-ext-diff','--no-textconv','--exit-code','HEAD','--','docs/adr'],cwd=HERE.parent.parent,capture_output=True,text=True)
    check('ADR 未修改',p.returncode==0,p.stdout+p.stderr)
    missing=[]
    for p in [HERE/'README.md',HERE/'REPORT-v2.md']:
        for href in re.findall(r'\]\(([^)]+)\)',p.read_text()):
            if not href.startswith(('http:','https:','#')) and (p.parent/href.split('#')[0])!=R/'verification.json' and not (p.parent/href.split('#')[0]).exists():missing.append([p.name,href])
    check('交付本地链接',not missing,missing)
    live=[];bins={os.fsencode(HERE/'.scratch/intercept-v2'),os.fsencode(HERE/'.scratch/probe-v2')}
    for p in Path('/proc').glob('[0-9]*'):
        try:
            args=(p/'cmdline').read_bytes().split(b'\0')
            try:cwd=os.readlink(p/'cwd')
            except OSError:cwd=''
            if (args and args[0] in bins) or cwd.startswith(str(HERE/'.scratch')+'/'):
                live.append({'pid':int(p.name),'args':[os.fsdecode(x) for x in args if x]})
        except (OSError,ProcessLookupError):pass
    check('本轮沙箱与探针无存活进程',not live,live)
    result=dict(ok=all(x['ok'] for x in validations),validations=validations,sources={p.name:hashlib.sha256(p.read_bytes()).hexdigest() for p in HERE.glob('*') if p.suffix in ['.c','.py']})
    (R/'verification.json').write_text(json.dumps(result,ensure_ascii=False,indent=2)+'\n')
    print(json.dumps(dict(ok=result['ok'],checks=len(validations),failed=[x for x in validations if not x['ok']]),ensure_ascii=False,indent=2))
    return 0 if result['ok'] else 1

if __name__=='__main__':raise SystemExit(main())
