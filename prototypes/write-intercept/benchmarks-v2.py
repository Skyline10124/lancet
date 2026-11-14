#!/usr/bin/env python3
"""四配置顺序工作负载；使用第一轮副本的 reflink 副本，不访问开发仓库。"""
import os
import json
from pathlib import Path
import runpy
import shutil
v=runpy.run_path(str(Path(__file__).with_name('experiment-v2.py')))
run,SCRATCH,RESULTS,dump=[v[k] for k in ['run','SCRATCH','RESULTS','dump']]
MODES=['none','ns','B2','B2v2'];WORK=SCRATCH/'lattice';CACHE=SCRATCH/'cargo-home'
TC=Path.home()/'.rustup/toolchains/1.96.0-x86_64-unknown-linux-gnu/bin'

def main():
    (SCRATCH/'home-v2').mkdir(exist_ok=True)
    env=dict(CARGO_HOME=str(CACHE),RUSTC=str(TC/'rustc'),RUSTDOC=str(TC/'rustdoc'),
        RUSTUP_HOME=str(Path.home()/'.rustup'),HOME=str(SCRATCH/'home-v2'),CARGO_BUILD_JOBS='4',
        CARGO_PROFILE_DEV_DEBUG='0',CARGO_PROFILE_TEST_DEBUG='0',CARGO_INCREMENTAL='0',
        GIT_CONFIG_NOSYSTEM='1',GIT_CONFIG_GLOBAL='/dev/null',TMPDIR=str(WORK/'target/tmp'),
        LC_ALL='C',PATH=str(TC)+':'+os.environ['PATH'])
    source=WORK/'crates/net-guard/src/lib.rs';original=source.read_bytes();rows=json.loads((RESULTS/'benchmarks.json').read_text()) if (RESULTS/'benchmarks.json').exists() else []
    rows=[r for r in rows if r['returncode']==0]
    completed={r['name'] for r in rows}
    try:
        for rep in range(3):
            for mode in MODES[rep:]+MODES[:rep]:
                if f'git-commit-{mode}-{rep}' in completed:continue
                source.write_bytes(original);shutil.rmtree(WORK/'target',ignore_errors=True);(WORK/'target/tmp').mkdir(parents=True)
                commands=[('build-full',[TC/'cargo','build','--offline','--workspace']),
                    ('build-incremental',[TC/'cargo','build','--offline','--workspace']),
                    ('test-subset',[TC/'cargo','test','--offline','-p','net-guard']),
                    ('git-status',['git','--no-optional-locks','status','--porcelain']),
                    ('git-checkout',['git','checkout','-b',f"{os.environ.get('WI_BENCH_BRANCH_PREFIX','wi-v2')}-{mode}-{rep}"]),
                    ('git-commit',['git','-c','user.name=bench','-c','user.email=bench@example.invalid','-c','commit.gpgsign=false','commit','--allow-empty','-m','bench: write intercept v2'])]
                for task,cmd in commands:
                    if task=='build-incremental':source.write_bytes(original+b'\n// v2 workload mutation\n')
                    name=f'{task}-{mode}-{rep}';out=run(mode,WORK,name,cmd,limit=1200,env=env)
                    rows.append(out);dump(RESULTS/'benchmarks.json',rows)
                    print(name,out['returncode'],out['stats'],out['stderr'][-300:],flush=True)
                    if task=='build-full' and out['returncode']:
                        dump(RESULTS/'benchmark-blocker.json',out);return
    finally:source.write_bytes(original)

if __name__=='__main__':main()
