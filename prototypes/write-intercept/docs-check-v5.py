#!/usr/bin/env python3
"""让未暂存的新文件进入相同 docs-check；只写本 worktree 内的临时索引，不写 Git 对象。"""
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
HERE=Path(__file__).resolve().parent
ROOT=HERE.parent.parent
SCRATCH=HERE/'.scratch'

def main():
    actual=Path(subprocess.check_output(['git','rev-parse','--git-path','index'],cwd=ROOT,text=True).strip())
    if not actual.is_absolute():actual=ROOT/actual
    before=hashlib.sha256(actual.read_bytes()).hexdigest();index=SCRATCH/'docs-index-v5';shutil.copyfile(actual,index)
    env=dict(os.environ,GIT_INDEX_FILE=str(index),CARGO_HOME=str(SCRATCH/'cargo-home'),CARGO_TARGET_DIR=str(SCRATCH/'docs-target-v5'),TMPDIR=str(SCRATCH))
    files=[p for p in HERE.rglob('*') if p.is_file() and '.scratch' not in p.parts and '__pycache__' not in p.parts]
    entries=[]
    for p in files:
        data=p.read_bytes();blob=hashlib.sha1(b'blob '+str(len(data)).encode()+b'\0'+data).hexdigest()
        entries.append(f'100644 {blob}\t{p.relative_to(ROOT)}\n')
    subprocess.run(['git','update-index','--add','--info-only','--index-info'],cwd=ROOT,input=''.join(entries),text=True,env=env,check=True)
    command=['cargo','--offline','--locked','xtask','docs-check']
    r=subprocess.run(command,cwd=ROOT,capture_output=True,text=True,env=env)
    unchanged=before==hashlib.sha256(actual.read_bytes()).hexdigest()
    result=dict(command=command,temporary_index=str(index),tracked_paths_added=len(files),real_index_unchanged=unchanged,returncode=r.returncode,stdout=r.stdout,stderr=r.stderr)
    (HERE/'results/v5/docs-check-temporary-index-v5.json').write_text(json.dumps(result,ensure_ascii=False,indent=2)+'\n')
    print(r.stdout,r.stderr);print('real_index_unchanged',unchanged)
    return r.returncode if unchanged else 1

if __name__=='__main__':raise SystemExit(main())
