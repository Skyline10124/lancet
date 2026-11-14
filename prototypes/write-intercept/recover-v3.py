#!/usr/bin/env python3
"""退出后的最小恢复节点：只读取持久账本、前态、锚点与写后摘要。"""
import json
import os
from pathlib import Path
import runpy
import shutil
import stat
import sys
v=runpy.run_path(str(Path(__file__).with_name('experiment-v2.py')))
state_of,semantic,before_entry=v['state_of'],v['semantic'],v['before_entry']

def recover(state,pending=False):
    events=[json.loads(x) for x in (state/'ledger.jsonl').read_text().splitlines()]
    if pending:
        changed=[];unchanged=[]
        for e in events:
            if e.get('record')=='rename':
                changed.append(e['to']);continue
            (unchanged if semantic(state_of(Path(e['path'])))==before_entry(e,state) else changed).append(e['path'])
        return dict(outcome='Indeterminate' if changed else 'Failed',residue=changed,unchanged=unchanged,restored=[],conflicts=[])
    receipt=json.loads((state/'poststate.json').read_text())
    conflicts=set(json.loads(x) for x in (state/'conflicts.jsonl').read_text().splitlines()) if (state/'conflicts.jsonl').exists() else set()
    for p,after in receipt.items():
        if state_of(Path(p))!=after:conflicts.add(p)
    errors=[];restored=[];trace=[];temporary_modes={};sources={};held={}
    for path,after in receipt.items():
        if after['type']=='file' and path not in conflicts:
            key=(after['dev'],after['ino']);sources[key]=Path(path)
            if key not in held:held[key]=os.open(path,os.O_RDONLY)
    for e in events:
        if e.get('record')=='rename':continue
        p=Path(e['path'])
        if str(p) in conflicts:continue
        for d in [p.parent,p]:
            if d.exists() and not d.is_symlink() and d.stat().st_uid==os.getuid():
                old=stat.S_IMODE(d.stat().st_mode)
                if not old&stat.S_IWUSR:temporary_modes[d]=old;d.chmod(old|stat.S_IWUSR)
    directory_pre={e['path']:e for e in events if e.get('before')=='directory'}
    # 已知冲突只隔离相应路径，无冲突项仍继续求逆。
    for e in reversed(events):
        if e.get('record')=='rename':
            a,b=Path(e['from']),Path(e['to']);st=state_of(b)
            if str(a) in conflicts or str(b) in conflicts:continue
            try:
                if e.get('exchange') and state_of(a).get('ino')==e['target_ino'] and st.get('ino')==e['ino']:
                    import ctypes
                    libc=ctypes.CDLL(None,use_errno=True)
                    if libc.syscall(316,-100,os.fsencode(a),-100,os.fsencode(b),2):raise OSError(ctypes.get_errno(),os.strerror(ctypes.get_errno()))
                    trace.append(dict(action='exchange',source=str(a),target=str(b)));continue
                if state_of(a).get('ino')==e['ino'] and state_of(a).get('dev')==e['dev']:
                    trace.append(dict(action='rename-not-applied',source=str(a)));continue
                if st.get('ino')==e['ino'] and st.get('dev')==e['dev'] and not a.exists():
                    b.rename(a);trace.append(dict(action='rename',source=str(b),target=str(a)))
                else:conflicts.update([str(a),str(b)])
            except OSError as ex:errors.append(str(ex))
            continue
        p=Path(e['path']);kind=e['before']
        if str(p) in conflicts:continue
        try:
            current=state_of(p)
            if kind=='Absent':
                if current['type']=='directory':p.rmdir()
                elif current['type']!='Absent':p.unlink()
            elif kind=='file':
                # 跨类别目录 clone 的父目录记录排在子项之前，逆序时按登记集补齐祖先。
                for parent in reversed(p.parents):
                    if not parent.exists() and str(parent) in directory_pre:
                        parent.mkdir(mode=stat.S_IMODE(directory_pre[str(parent)]['mode'])|0o700)
                anchor=state/f"{e['dev']}-{e['ino']}.anchor"
                if current['type']!='Absent' and (current['type']!='file' or (anchor.exists() and current.get('ino')!=e['ino'])):
                    p.rmdir() if current['type']=='directory' else p.unlink()
                if not p.exists() and anchor.exists():os.link(anchor,p)
                key=(e['dev'],e['ino']);source=sources.get(key)
                if not p.exists() and source and source.exists() and source.stat().st_ino==e['ino']:os.link(source,p)
                if not p.exists() and key in held and os.fstat(held[key]).st_nlink>0:
                    import ctypes
                    libc=ctypes.CDLL(None,use_errno=True)
                    if libc.linkat(-100,os.fsencode(f'/proc/self/fd/{held[key]}'),-100,os.fsencode(p),0x400):raise OSError(ctypes.get_errno(),os.strerror(ctypes.get_errno()))
                if p.exists():p.chmod(stat.S_IMODE(p.stat().st_mode)|0o200)
                with p.open('wb') as dst,(state/f"{e['index']}.clone").open('rb') as src:shutil.copyfileobj(src,dst)
                p.chmod(stat.S_IMODE(e['mode']))
            elif kind=='directory':p.mkdir(exist_ok=True);p.chmod(stat.S_IMODE(e['mode']))
            elif kind=='symlink':
                if current['type']!='Absent':p.unlink()
                p.symlink_to(e['link'])
            elif kind=='fifo':
                if current['type']!='Absent':p.unlink()
                os.mkfifo(p,stat.S_IMODE(e['mode']))
            restored.append(str(p));trace.append(dict(action='object',path=str(p)))
        except OSError as ex:errors.append([str(p),str(ex)])
    for fd in held.values():os.close(fd)
    for p,mode in temporary_modes.items():
        if p.exists():p.chmod(mode)
    if not conflicts and not errors:
        for a in state.glob('*.anchor'):a.unlink()
    return dict(outcome='Indeterminate' if (state/'indeterminate.json').exists() else ('Conflict' if conflicts else ('Error' if errors else 'Restored')),
                restored=restored,conflicts=sorted(conflicts),errors=errors,trace=trace)
if __name__=='__main__':
    state=Path(sys.argv[1]);result=recover(state,'--pending' in sys.argv)
    result['recovery_pid']=os.getpid()
    text=json.dumps(result,ensure_ascii=False,indent=2)+'\n'
    (state/'recovery.json').write_text(text);print(text,end='')
