#!/usr/bin/env python3
"""验证自写索引、影子 inode 保留、最终二进制的 RPC 墙钟及交换窗口恢复。"""
import json,os,runpy,shutil,time
from pathlib import Path
P=Path(__file__).resolve().parent;v=runpy.run_path(str(P/'verify-v7.py'));Shadow,setup,oracle,difference,dump=[v[k]for k in ['Shadow','setup','oracle','difference','dump']];W=P/'.scratch';R=P/'results/v7'
def self_writes():
 root,state=setup('self-write');o=Shadow(root,state)
 try:
  o.begin();(o.shadow/'a').write_text('agent');ino=(o.shadow/'a').stat().st_ino;o.finish();out=o.commit();before=o.call('stats')['scan_digests'];time.sleep(.35);after=o.call('stats')['scan_digests']
  row=dict(result=out,shadow_inode_preserved=ino==(o.shadow/'a').stat().st_ino,background_digest_delta=after-before,pass_=ino==(o.shadow/'a').stat().st_ino and after==before)
  dump(R/'self-writes-v7.json',row)
 finally:o.close()
def exchange_crashes():
 rows=[]
 for rep in range(3):
  root,state=setup(f'exchange-gap-{rep}');o=Shadow(root,state);before=oracle(root);o.begin();(o.shadow/'a').write_text('after');o.finish();o.call('env',values={'S3_KILL_AT':'after-exchange'})
  try:o.commit();killed=False
  except RuntimeError:o.proc.wait();killed=o.proc.returncode==-9
  o.close();o=Shadow(root,state)
  rows.append(dict(rep=rep,killed=killed,recovery=o.initial['recovery'],pass_=killed and oracle(root)==before));o.close()
 dump(R/'exchange-crashes-v7.json',rows)
def fixed():
 rows=[]
 for n in [3000,10000,100000]:
  root=W/f'fixed-final-{n}-v7';state=W/'states-v7'/f'fixed-final-{n}';shutil.rmtree(root,ignore_errors=True);shutil.rmtree(state,ignore_errors=True);root.mkdir()
  for i in range(n):
   p=root/f'd{i%1000:03}'/f'f{i:06}';p.parent.mkdir(exist_ok=True);p.write_text(f'before-{i}\n'*32)
  o=Shadow(root,state)
  try:
   for rep in range(9):
    time.sleep(.11);start=time.monotonic();a=o.begin();b=o.finish(audit=False);c=o.commit();rows.append(dict(mode='S3',files=n,rep=rep,wall_s=time.monotonic()-start,begin=a,finish=b,commit=c));dump(R/'fixed-final-v7.json',rows)
  finally:o.close()
  shutil.rmtree(root);shutil.rmtree(state)
if __name__=='__main__':
 import sys
 for task in sys.argv[1:] or ['self_writes','exchange_crashes','fixed']:globals()[task]()
