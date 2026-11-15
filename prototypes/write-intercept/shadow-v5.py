#!/usr/bin/env python3
"""Rust 常驻节点的测试客户端；复用第四轮命名空间启动器。"""
import atexit
import json
import os
from pathlib import Path
import runpy
import subprocess
import sys

HERE=Path(__file__).resolve().parent
W=HERE/'.scratch'
BIN=W/'rust-target-v5/release/shadow-v5'
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
        self.err=(self.state/'node-v5.stderr').open('w')
        self.proc=subprocess.Popen([str(BIN),str(self.real),str(self.state)],stdin=subprocess.PIPE,stdout=subprocess.PIPE,stderr=self.err,text=True,bufsize=1)
        OPEN.add(self)
        self.initial=self.read()
    def read(self):
        line=self.proc.stdout.readline()
        if not line:raise RuntimeError(f'节点已退出 {self.proc.poll()}：'+(self.state/'node-v5.stderr').read_text())
        result=json.loads(line)
        if 'error' in result and 'outcome' not in result:raise RuntimeError(result['error'])
        return result
    def call(self,op,**kw):
        self.proc.stdin.write(json.dumps(dict(op=op,**kw))+'\n');self.proc.stdin.flush();return self.read()
    def begin(self,full=False):return self.call('begin',full=full)
    def finish(self,audit=True,overflow=False):return self.call('finish',audit=audit,overflow=overflow)
    def commit(self):return self.call('commit')
    def discard(self):return self.call('discard')
    execute=v4['Shadow'].execute
    def close(self):
        if self.proc.poll() is None:
            self.proc.stdin.write('{"op":"stop"}\n');self.proc.stdin.flush()
            try:self.proc.wait(timeout=10)
            except subprocess.TimeoutExpired:self.proc.kill();self.proc.wait()
        self.proc.stdin.close();self.proc.stdout.close();self.err.close();OPEN.discard(self)
if __name__=='__main__':
    if sys.argv[1]=='recover':raise SystemExit(subprocess.call([str(BIN),*sys.argv[1:]]))
