#!/usr/bin/env python3
"""用原生对照明确记录尚未完整实现的 openat2 边缘语义。"""
from pathlib import Path
import runpy
v=runpy.run_path(str(Path(__file__).with_name('experiment-v2.py')))
globals().update({k:v[k] for k in ['RESULTS','setup','run','dump','view']})
SOURCE=r'''
import ctypes,os,json
libc=ctypes.CDLL(None,use_errno=True)
class How(ctypes.Structure):_fields_=[('flags',ctypes.c_uint64),('mode',ctypes.c_uint64),('resolve',ctypes.c_uint64),('extra',ctypes.c_uint64)]
case=__import__('sys').argv[1];h=How();h.flags=os.O_WRONLY;size=24;path=b'a';d=-100
if case=='zero-extension':size=32
if case=='unknown-high-bit':h.flags|=1<<63
if case=='in-root-dangling':
 d=os.open('sub',os.O_PATH);path=b'dangling';h.flags|=os.O_CREAT;h.mode=0o600;h.resolve=16
fd=libc.syscall(ctypes.c_long(437),ctypes.c_int(d),ctypes.c_char_p(path),ctypes.byref(h),ctypes.c_size_t(size));err=ctypes.get_errno() if fd<0 else 0
if fd>=0:os.close(fd)
print(json.dumps({'case':case,'rc':-1 if fd<0 else 0,'errno':err}))
'''

def main():
    rows=[]
    for case in ['zero-extension','unknown-high-bit','in-root-dangling']:
        for mode in ['ns','B2v2']:
            name=f'edge-{mode}-{case}';root=setup(name,case);(root/'sub/dangling').symlink_to('/new')
            before=view(root);out=run(mode,root,name,['python3','-c',SOURCE,case]);rows.append(dict(case=case,mode=mode,before=before,after=view(root),result=out));dump(RESULTS/'context-edges.json',rows)
            print(case,mode,out['observations'],flush=True)

if __name__=='__main__':main()
