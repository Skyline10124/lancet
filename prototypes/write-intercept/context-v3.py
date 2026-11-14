#!/usr/bin/env python3
"""常驻 worker 的单请求与连续上下文切换，以及 namespace 可见性。"""
from pathlib import Path
import runpy
import json
import mmap
import struct
import os
import time
v=runpy.run_path(str(Path(__file__).with_name('experiment-v3.py')))
globals().update({k:v[k] for k in ['SCRATCH','RESULTS','PROBE','setup','run','dump','coverage']})
CASES='cwd openat chroot umask perm-file perm-file-nocap perm-dir perm-dir-nocap tmpfs-perm tmpfs-perm-nocap openat2 resolve-beneath resolve-beneath-escape resolve-in-root resolve-in-root-create resolve-nosym resolve-nomagic resolve-noxdev resolve-noxdev-ok resolve-nosym-ok resolve-nomagic-ok resolve-dangling-nosym nofollow excl-symlink cloexec not-cloexec'.split()

def main():
    coverage(CASES)
    # 每次并发使用两个不同 cwd、不同 umask；验证 worker 通道复用时能切换上下文。
    source='''import os,subprocess,sys,json
code="import os;os.chdir(__import__('sys').argv[1]);os.umask(int(__import__('sys').argv[2],8));f=os.open('new',os.O_CREAT|os.O_WRONLY,0o666);os.write(f,os.getcwd().encode());os.close(f)"
for i in range(20):
 a=subprocess.Popen([sys.executable,'-c',code,'sub','077']);b=subprocess.Popen([sys.executable,'-c',code,'empty','022']);assert a.wait()==b.wait()==0
 assert os.stat('sub/new').st_mode&0o777==0o600
 assert os.stat('empty/new').st_mode&0o777==0o644
 assert open('sub/new').read()==os.path.join(os.getcwd(),'sub')
 assert open('empty/new').read()==os.path.join(os.getcwd(),'empty')
 os.unlink('sub/new');os.unlink('empty/new')
print(json.dumps({'iterations':20,'ok':True}))'''
    rows=[]
    for mode in ['ns','B3']:
        root=setup(f'context-alternating-{mode}','open')
        out=run(mode,root,f'context-alternating-{mode}',['python3','-c',source]);rows.append(out)
    dump(RESULTS/'context-alternating.json',rows)
    invalid=[]
    for mode in ['ns','B3']:
        root=setup(f'invalid-dirfd-{mode}','open');invalid.append(run(mode,root,f'invalid-dirfd-{mode}',[PROBE,'invalid-dirfd']))
    dump(RESULTS/'invalid-dirfd.json',invalid)
    # 用检查窗口保持环境存活，宿主直接比较 namespace inode；无浏览器或调试器依赖。
    root=setup('worker-visibility','race');ctl=SCRATCH/'v3-visibility.control';ctl.write_bytes(struct.pack('i',1)+bytes(4092));evidence={}
    def inspect(proc):
        state=SCRATCH/'states-v3/worker-visibility'
        with ctl.open('r+b') as f,mmap.mmap(f.fileno(),4096) as m:
            end=time.monotonic()+10
            while struct.unpack_from('i',m)[0]!=2:
                if time.monotonic()>end:raise RuntimeError('检查窗口超时')
                time.sleep(.001)
            pids=json.loads((state/'pids.json').read_text());evidence['pids']=pids
            evidence['namespaces']={key:{ns:os.readlink(f'/proc/{pid}/ns/{ns}') for ns in ['user','mnt','pid']} for key,pid in pids.items()}
            evidence['worker_nspid']=[s for s in Path(f"/proc/{pids['worker']}/status").read_text().splitlines() if s.startswith('NSpid:')]
            evidence['worker_absent_in_sandbox_proc']=not Path(f"/proc/{pids['init']}/root/proc/{pids['worker']}").exists()
            struct.pack_into('i',m,0,3)
    out=run('B3',root,'worker-visibility',[PROBE,'replace-open'],env={'WI_RACE_CONTROL':str(ctl)},external=inspect)
    dump(RESULTS/'worker-visibility.json',dict(result=out,evidence=evidence))
if __name__=='__main__':main()
