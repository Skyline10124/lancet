#!/usr/bin/env python3
"""复制压缩包/索引、首次解压，以及 LATTICE target 种子成本。"""
import json,os,runpy,shutil,subprocess,time
from pathlib import Path
P=Path(__file__).resolve().parent;W=P/'.scratch';R=P/'results/v7'
b=runpy.run_path(str(P/'benchmarks-v7.py'));env=b['environment'];dump=b['dump'];TC=b['TC'];rows=[]
for rep in range(3):
 cache=W/f'private-cache-{rep}-v7';shutil.rmtree(cache,ignore_errors=True);(cache/'registry').mkdir(parents=True)
 start=time.monotonic()
 for name in ['cache','index']:subprocess.run(['cp','-a','--reflink=always',str(W/'cargo-home/registry'/name),str(cache/'registry'/name)],check=True)
 seed=time.monotonic()-start
 before_src=(cache/'registry/src').exists();e=dict(os.environ,**env());e["CARGO_HOME"]=str(cache);start=time.monotonic()
 p=subprocess.run([str(TC/'cargo'),'metadata','--offline','--format-version','1'],cwd=W/'lattice',env=e,capture_output=True)
 extract=time.monotonic()-start
 rows.append(dict(rep=rep,seed_s=seed,source_absent_before=not before_src,metadata_and_first_extract_s=extract,returncode=p.returncode,stderr=p.stderr.decode(),source_directories=sum(1 for x in (cache/'registry/src').glob('*/*') if x.is_dir())))
 dump(R/'private-cache-v7.json',rows)
 shutil.rmtree(cache)
seed_rows=[]
files=[p for p in (W/"lattice/target").rglob("*") if p.is_file()];target_files=len(files);target_bytes=sum(p.stat().st_size for p in files)
for rep in range(3):
 dest=W/f'target-seed-{rep}-v7';shutil.rmtree(dest,ignore_errors=True);start=time.monotonic()
 p=subprocess.run(['cp','-a','--reflink=always',str(W/'lattice/target'),str(dest)],capture_output=True,text=True)
 seed_rows.append(dict(files=target_files,logical_bytes=target_bytes,rep=rep,seed_s=time.monotonic()-start,returncode=p.returncode,stderr=p.stderr));dump(R/'target-seed-v7.json',seed_rows);shutil.rmtree(dest,ignore_errors=True)
print('cache',[(r['seed_s'],r['metadata_and_first_extract_s']) for r in rows]);print('target',seed_rows)
