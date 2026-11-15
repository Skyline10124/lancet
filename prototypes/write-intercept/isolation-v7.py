#!/usr/bin/env python3
"""第七轮挂载、hidden、真实 inotify 溢出与子进程生命周期验收。"""
import json,os,runpy,signal,subprocess,time
from pathlib import Path
P=Path(__file__).resolve().parent
v=runpy.run_path(str(P/'verify-v7.py'));Shadow,setup,oracle,difference,dump=[v[k] for k in ['Shadow','setup','oracle','difference','dump']];W=P/'.scratch';R=P/'results/v7'
rows=[]
root,state=setup('mount-rules')
fake=W/'fake-home-v7';fake.mkdir(exist_ok=True);(fake/'sentinel').write_text('host-only')
ro=W/'readonly-v7';ro.mkdir(exist_ok=True);(ro/'a').write_text('ro-host');(ro/'hidden').mkdir(exist_ok=True);(ro/'hidden/secret').write_text('ro-hidden')
for p in [fake/'rw-alias',fake/'ro-alias']:p.unlink(missing_ok=True)
os.link(root/'a',fake/'rw-alias');os.link(ro/'a',fake/'ro-alias')
os.environ['S3_HOST_HOME']=str(fake)
o=Shadow(root,state)
try:
 o.begin()
 code=f'''import os,json,errno
from pathlib import Path
root=Path({str(root)!r});home=Path(os.environ['HOME']);ro=Path({str(ro)!r})
obs={{'sentinel_visible':(home/'sentinel').exists(),'ro_hidden_visible':(ro/'hidden/secret').exists(),'links':[],'writes':[]}}
for directory in ['target','.git']:(Path(directory)/'source').write_text('private')
for a,b in [('a','target/link'),('a','.git/link'),('target/source','regular-target'),('.git/source','regular-git'),('target/source','.git/cache-link'),('.git/source','target/git-link')]:
 try:os.link(a,b);error=0
 except OSError as e:error=e.errno
 obs['links'].append([a,b,error])
for p in [home/'rw-alias',home/'ro-alias',ro/'a']:
 try:p.write_text('attempt');error=0
 except OSError as e:error=e.errno
 obs['writes'].append([str(p),error])
Path('a').write_text('private replacement')
obs['mountinfo']=Path('/proc/self/mountinfo').read_text()
print(json.dumps(obs))'''
 out=o.execute(['python3','-c',code],env={'S3_RO_WORK':str(ro)})
 obs=json.loads(out['stdout']) if not out['returncode'] else {}
 fin=o.finish();settle=o.commit()
 rows.append(dict(case='mounts',execution=out,observations=obs,finish=fin,result=settle,pass_=not out['returncode'] and not obs['sentinel_visible'] and not obs['ro_hidden_visible'] and all(x[2]==18 for x in obs['links']) and settle['outcome']=='Conflict' and (root/'a').read_bytes()==(fake/'rw-alias').read_bytes() and (ro/'a').read_text()=='ro-host'))
finally:o.close();del os.environ['S3_HOST_HOME']
root,state=setup('hidden');(root/'hidden').mkdir(exist_ok=True);(root/'hidden/host').write_text('host secret');o=Shadow(root,state)
try:
 absent=not (o.shadow/'hidden').exists();before=oracle(root);o.begin();out=o.execute(['sh','-c','mkdir hidden; echo forbidden > hidden/new; echo changed > a']);fin=o.finish();res=o.commit()
 rows.append(dict(case='hidden',absent_initially=absent,execution=out,finish=fin,result=res,pass_=absent and res['outcome']=='Conflict' and oracle(root)==before and not (o.shadow/'hidden').exists()))
finally:o.close()
root,state=setup('overflow');o=Shadow(root,state)
try:
 o.begin();before=oracle(o.shadow);os.kill(o.proc.pid,signal.SIGSTOP)
 try:
  for i in range(20000):
   p=o.shadow/f'event-{i}';p.write_text('x');p.unlink()
  (o.shadow/'a').write_text('overflow-final')
 finally:os.kill(o.proc.pid,signal.SIGCONT)
 after=oracle(o.shadow);fin=o.finish();res=o.discard()
 rows.append(dict(case='real-overflow',finish=fin,result=res,pass_=fin['audit']['exact'] and fin['overflow'] and fin['epsilon']['paths']==difference(before,after)))
finally:o.close()
root,state=setup('bad-mount');state.mkdir(parents=True)
p=subprocess.run([str(W/'rust-target-v7/release/shadow-v7'),str(root),str(state)],env=dict(os.environ,S3_WRITABLE_SOURCES=json.dumps([str(root)])),capture_output=True,text=True)
rows.append(dict(case='reject-writable-host-source',returncode=p.returncode,stderr=p.stderr,pass_=p.returncode!=0 and '可写挂载来源不在状态目录' in p.stderr))
root,state=setup('bad-default-mount');state.mkdir(parents=True);(state/'home').symlink_to(root,target_is_directory=True)
p=subprocess.run([str(W/'rust-target-v7/release/shadow-v7'),str(root),str(state)],capture_output=True,text=True)
rows.append(dict(case='reject-default-home-symlink',returncode=p.returncode,stderr=p.stderr,pass_=p.returncode!=0 and '可写挂载来源不在状态目录' in p.stderr))
root,state=setup('bad-home-config');(state/'home').mkdir(parents=True);sentinel=root/'sentinel';sentinel.write_text('host config sentinel');(state/'home/.gitconfig').symlink_to(sentinel)
try:
 o=Shadow(root,state);o.close();rejected=False
except OSError as e:rejected=e.errno==40
rows.append(dict(case='reject-home-config-symlink',pass_=rejected and sentinel.read_text()=='host config sentinel'))
root,state=setup('exit');o=Shadow(root,state)
try:
 o.begin();out=o.execute(['sh','-c','(sleep 0.3; echo survived > target/survived) & exit 0']);time.sleep(.4);fin=o.finish();o.discard()
 rows.append(dict(case='proc-lifetime',execution=out,pass_=not (o.shadow/'target/survived').exists()))
finally:o.close()
dump(R/'isolation-v7.json',rows);print([(r['case'],r['pass_']) for r in rows])
