#!/usr/bin/env python3
"""在 reflink 缓存和本地克隆上运行五配置基准；不触碰原仓库。"""
import os
from pathlib import Path
import shutil
import subprocess
from experiment import HERE, SCRATCH, RESULTS, MODES, dump, run

SOURCE=Path('/home/gcp-vegas/Dev/lattice')
WORK=SCRATCH/'lattice'
CACHE=SCRATCH/'cargo-home'
TOOLCHAIN=Path.home()/'.rustup/toolchains/1.96.0-x86_64-unknown-linux-gnu/bin'

def setup():
    if not WORK.exists():
        subprocess.run(['git','clone','--local','--no-hardlinks',str(SOURCE),str(WORK)],check=True)
    CACHE.mkdir(exist_ok=True)
    if not (CACHE/'registry').exists():
        subprocess.run(['cp','-a','--reflink=always',str(Path.home()/'.cargo/registry'),str(CACHE)],check=True)
    (SCRATCH/'home').mkdir(exist_ok=True)
    return dict(CARGO_HOME=str(CACHE), RUSTC=str(TOOLCHAIN/'rustc'),
        RUSTDOC=str(TOOLCHAIN/'rustdoc'), RUSTUP_HOME=str(Path.home()/'.rustup'),
        HOME=str(SCRATCH/'home'), CARGO_BUILD_JOBS='4', CARGO_PROFILE_DEV_DEBUG='0',
        CARGO_PROFILE_TEST_DEBUG='0', CARGO_INCREMENTAL='0',
        GIT_CONFIG_NOSYSTEM='1', GIT_CONFIG_GLOBAL='/dev/null',
        TMPDIR=str(WORK/'target/tmp'), LC_ALL='C',
        PATH=str(TOOLCHAIN)+':'+os.environ['PATH'])

def main():
    # 每次重新创建实验副本，避免重复运行撞到前一轮的分支名。
    shutil.rmtree(WORK,ignore_errors=True)
    env=setup(); rows=[];cargo=str(TOOLCHAIN/'cargo')
    source=WORK/'crates/net-guard/src/lib.rs';original=source.read_bytes()
    for rep in range(3):
        # 轮换顺序降低暖缓存与后台负载的方向性偏差。
        order=MODES[rep:]+MODES[:rep]
        for mode in order:
            source.write_bytes(original)
            shutil.rmtree(WORK/'target',ignore_errors=True);(WORK/'target/tmp').mkdir(parents=True)
            commands=[('build-full',[cargo,'build','--offline','--workspace']),
                      ('build-incremental',[cargo,'build','--offline','--workspace']),
                      ('test-subset',[cargo,'test','--offline','-p','net-guard']),
                      ('git-status',['git','--no-optional-locks','status','--porcelain']),
                      ('git-checkout',['git','checkout','-b',f'wi-{mode}-{rep}']),
                      ('git-commit',['bot-run','git','-c','commit.gpgsign=false','commit','--allow-empty','-m','bench: 写拦截实验空提交'])]
            for task,cmd in commands:
                if task=='build-incremental':source.write_bytes(original+'\n// 写拦截实验：强制重新编译此源文件。\n'.encode())
                name=f'{task}-{mode}-{rep}'
                out=run(mode,WORK,name,cmd,limit=600,env=env)
                rows.append(out);dump(RESULTS/'benchmarks.json',rows)
                print(name,out['returncode'],out['stats'],out['stderr'][-400:],flush=True)
                if task=='build-full' and out['returncode']:
                    # 保留失败；依赖缺失时由研究者选择可行子集，避免把失败计成构建性能。
                    dump(RESULTS/'benchmark-blocker.json',out)
                    return
    source.write_bytes(original)

if __name__=='__main__':main()
