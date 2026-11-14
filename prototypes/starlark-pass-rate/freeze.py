"""12 次试跑结束后一次性冻结；禁止覆盖已经存在的冻结清单。"""
import hashlib
import json
import platform
import subprocess
import sys
import time
from driver import ROOT, V3, GROUPS, MODELS, MODE, frozen_files
from report_v3 import read_runs


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def main():
    pilot=read_runs('pilot')
    expected={(g,a,m,t,1) for g in GROUPS for a in MODELS for m in [MODE] for t in ['read-02','probe-02','edit-01','batch-01','test-01','rollback-01']}
    actual={(r['summary']['group'],r['summary']['alias'],r['summary']['mode'],r['summary']['task'],r['summary']['repeat']) for r in pilot}
    if len(pilot)!=12 or actual!=expected or any(r['summary']['status']!='complete' for r in pilot):
        raise RuntimeError('12 次试跑必须完整结束且没有 API/测试台异常')
    if list((V3/'formal').glob('*.jsonl')):
        raise RuntimeError('正式运行已存在，禁止重新冻结')
    names=['budget.py','driver.py','metrics.py','commands.py','command-normalization.json','Cargo.toml','Cargo.lock']
    paths=[ROOT/n for n in names]+list((ROOT/'act-sim').rglob('*.rs'))+list((ROOT/'tests').glob('*.py'))+[ROOT/'act-sim/Cargo.toml']
    manifest=dict(time=time.time(),base_commit=subprocess.check_output(['git','rev-parse','HEAD'],text=True).strip(),
                  runtime=platform.platform(),rustc=subprocess.check_output(['rustc','--version'],text=True).strip(),
                  python=sys.version,files=frozen_files(),binary_sha256=digest(ROOT/'target/debug/act-sim'),
                  harness_files={str(p.relative_to(ROOT)):digest(p) for p in sorted(paths)},
                  pilot_files={str(r['path'].relative_to(ROOT)):digest(r['path']) for r in pilot},
                  prompt_git_blobs={str(p.relative_to(ROOT)):subprocess.check_output(['git','hash-object',str(p)],text=True).strip() for p in sorted((ROOT/'prompts').glob('*.md'))})
    with (V3/'evidence/freeze.json').open('x') as f:
        json.dump(manifest,f,ensure_ascii=False,indent=2); f.write('\n')
    print(json.dumps(dict(frozen_inputs=len(manifest['files']),harness_files=len(manifest['harness_files']),pilot_runs=len(pilot))))


if __name__=='__main__':
    main()
