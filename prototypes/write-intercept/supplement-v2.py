#!/usr/bin/env python3
"""额外反例：目标端替换、符号链接 ABA、硬链接删除还原、后态冲突。"""
import mmap
import os
from pathlib import Path
import runpy
import struct
v=runpy.run_path(str(Path(__file__).with_name('experiment-v2.py')))
globals().update({k:v[k] for k in ['SCRATCH','RESULTS','PROBE','setup','run','dump','view','restore','semantic']})
r=runpy.run_path(str(Path(__file__).with_name('races-v2.py')))
control_for,wait_control=r['control_for'],r['wait_control']

def main():
    rows=[]
    for case in ['rename-destination','symlink-aba','symlinkat-aba']:
        for rep in range(20):
            name=f'{case}-{rep}';root=setup(name,case);ctl=control_for(name)
            if case=='rename-destination':(root/'dest').write_text('original destination')
            else:(root/'race-safe').unlink()
            trace=[]
            def writer(proc):
                with ctl.open('r+b') as f,mmap.mmap(f.fileno(),4096) as m:
                    wait_control(m)
                    if case=='rename-destination':
                        (root/'dest').rename(root/'saved-dest');(root/'dest').write_text('external destination')
                    else:
                        old=(root/'race-safe').lstat().st_ino;(root/'race-safe').rename(root/'saved-link');(root/'race-safe').symlink_to('a')
                        trace.append(dict(original_inode=old,replacement_inode=(root/'race-safe').lstat().st_ino))
                    struct.pack_into('i',m,0,3)
            probe='replace-rename' if case=='rename-destination' else 'replace-'+case.removesuffix('-aba')
            before=view(root);out=run('B2v2',root,name,[PROBE,probe],env={'WI_RACE_CONTROL':str(ctl),'WI_RACE_PHASE':'0' if case=='rename-destination' else '2'},external=writer)
            rows.append(dict(case=case,rep=rep,before=before,after=view(root),trace=trace,result=out));dump(RESULTS/'supplement.json',rows)
        print(case,'done',flush=True)
    for case in ['unlink-hardlink','rename-hardlink','mkdir-trailing','restore-conflict','external-unregistered','deadline']:
        name='extra-'+case;root=setup(name,case)
        if 'hardlink' in case:os.link(root/'a',root/'hard')
        before=view(root);probe='open' if case in ['restore-conflict','external-unregistered'] else case
        out=run('B2v2',root,name,[PROBE,probe],limit=2 if case=='deadline' else 30)
        if case=='restore-conflict':(root/'a').write_text('external editor after settlement')
        if case=='external-unregistered':(root/'outside').write_text('external editor')
        after=view(root);restoration=restore(out);restored=view(root)
        delta=[p for p in sorted(before.keys()|restored.keys()) if semantic(before.get(p,{'type':'Absent'}))!=semantic(restored.get(p,{'type':'Absent'}))]
        topology=None
        if 'hardlink' in case:topology={'before_same_inode':before['a']['ino']==before['hard']['ino'],'after_same_inode':restored['a']['ino']==restored['hard']['ino'],'original_a_inode_preserved':restored['a']['ino']==before['a']['ino']}
        rows.append(dict(case=case,before=before,after=after,result=out,restoration=restoration,restored=restored,semantic_difference=delta,topology=topology));dump(RESULTS/'supplement.json',rows)
        print(case,restoration,delta,topology,flush=True)

if __name__=='__main__':main()
