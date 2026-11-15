#!/usr/bin/env python3
"""Rust 常驻节点的测试客户端；使用第七轮私有根启动器。"""
import atexit
import json
import os
from pathlib import Path
import runpy
import subprocess
import sys
import shutil
import time

HERE=Path(__file__).resolve().parent
W=HERE/'.scratch'
BIN=W/'rust-target-v7/release/shadow-v7'
v4=runpy.run_path(str(HERE/'shadow-v4.py'))
dump=v4['dump']
EXCLUDED=v4['EXCLUDED']
OPEN=set()
@atexit.register
def cleanup():
    for o in list(OPEN):o.close()
class Shadow:
    def __init__(self,real,state):
        self.real=Path(real).resolve();self.state=Path(state).resolve();self.shadow=self.state/'shadow'
        self.state.mkdir(parents=True,exist_ok=True)
        home=self.state/'home';home.mkdir(exist_ok=True)
        if not home.resolve().is_relative_to(self.state):raise RuntimeError('可写挂载来源不在状态目录：home')
        with os.fdopen(os.open(home/'.gitconfig',os.O_WRONLY|os.O_CREAT|os.O_TRUNC|os.O_NOFOLLOW,0o600),'w') as config:
            config.write('[user]\n name = bench\n email = bench@example.invalid\n')
        self.err=(self.state/'node-v7.stderr').open('w')
        self.proc=subprocess.Popen([str(BIN),str(self.real),str(self.state)],stdin=subprocess.PIPE,stdout=subprocess.PIPE,stderr=self.err,text=True,bufsize=1)
        OPEN.add(self)
        self.initial=self.read()
    def read(self):
        line=self.proc.stdout.readline()
        if not line:raise RuntimeError(f'节点已退出 {self.proc.poll()}：'+(self.state/'node-v7.stderr').read_text())
        result=json.loads(line)
        if 'error' in result and 'outcome' not in result:raise RuntimeError(result['error'])
        return result
    def call(self,op,**kw):
        self.proc.stdin.write(json.dumps(dict(op=op,**kw))+'\n');self.proc.stdin.flush();return self.read()
    def begin(self,full=False):return self.call('begin',full=full)
    def finish(self,audit=True,overflow=False):return self.call('finish',audit=audit,overflow=overflow)
    def commit(self):return self.call('commit')
    def discard(self):return self.call('discard')
    def execute(self,command,env=None,timeout=1200):
        import time,signal
        start=time.monotonic()
        host_home=os.environ.get('S3_HOST_HOME',str(Path.home()))
        private_cargo=self.state/'home/.cargo'
        if env and 'CARGO_HOME' in env and not (private_cargo/'registry').exists():
            for name in ['cache','index']:
                source=Path(env['CARGO_HOME'])/'registry'/name
                if source.exists():
                    (private_cargo/'registry').mkdir(parents=True,exist_ok=True)
                    subprocess.run(['cp','-a','--reflink=always',str(source),str(private_cargo/'registry'/name)],check=True)
        env=dict(env or {})
        if 'CARGO_HOME' in env:env['CARGO_HOME']=host_home+'/.cargo'
        env.update(HOME=host_home,S3_HOST_HOME=host_home,S3_TOOLCHAIN=str(Path.home()/'.rustup'),S3_CARGO_BIN=str(Path.home()/'.cargo/bin'),S3_READONLY=str(W/'probe-v3'))
        cmd=[str(W/'namespace-v7'),str(self.real),str(self.state),'--',*map(str,command)]
        proc=subprocess.Popen(cmd,stdout=subprocess.PIPE,stderr=subprocess.PIPE,env=dict(os.environ,**(env or {})),start_new_session=True)
        try:out,err=proc.communicate(timeout=timeout)
        except subprocess.TimeoutExpired:
            os.killpg(proc.pid,signal.SIGKILL);out,err=proc.communicate()
        return dict(returncode=proc.returncode,stdout=out.decode(errors='replace'),stderr=err.decode(errors='replace'),exec_s=time.monotonic()-start)
    def close(self):
        if self.proc.poll() is None:
            self.proc.stdin.write('{"op":"stop"}\n');self.proc.stdin.flush()
            try:self.proc.wait(timeout=10)
            except subprocess.TimeoutExpired:self.proc.kill();self.proc.wait()
        self.proc.stdin.close();self.proc.stdout.close();self.err.close();OPEN.discard(self)
if __name__=='__main__':
    if sys.argv[1]=='recover':raise SystemExit(subprocess.call([str(BIN),*sys.argv[1:]]))
