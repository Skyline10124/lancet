"""用临时 Git 索引校验未暂存的交付文档，不改变维护者的真实索引。"""
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile

ROOT=Path(__file__).resolve().parent
REPO=ROOT.parents[1]
EVIDENCE=ROOT/'results/v3/evidence'


def main():
    git=['git','--no-optional-locks','--no-pager']
    path=Path(subprocess.check_output(git+['rev-parse','--git-path','index'],cwd=REPO,text=True).strip())
    if not path.is_absolute():path=REPO/path
    before=hashlib.sha256(path.read_bytes()).hexdigest()
    names=subprocess.check_output(git+['ls-files','--others','--exclude-standard','-z','--','prototypes/starlark-pass-rate'],cwd=REPO).decode().split('\0')
    names=[n for n in names if n and '/results/v3/tmp/' not in n]
    with tempfile.TemporaryDirectory(dir=ROOT/'results/v3/tmp',prefix='docs-index-') as td:
        index=Path(td)/'index';shutil.copyfile(path,index)
        env={**os.environ,'GIT_INDEX_FILE':str(index)}
        subprocess.run(['git','add','--intent-to-add','--',*names],cwd=REPO,env=env,check=True,capture_output=True)
        result=subprocess.run(['cargo','xtask','docs-check'],cwd=REPO,env=env,capture_output=True,text=True)
    after=hashlib.sha256(path.read_bytes()).hexdigest()
    assert before==after,'真实索引被改变'
    (EVIDENCE/'docs-check.txt').write_text(result.stdout+result.stderr)
    audit=dict(command='cargo xtask docs-check（临时 GIT_INDEX_FILE，包含未跟踪交付文件）',exit_code=result.returncode,real_index_unchanged=True,temporary_entries=len(names))
    (EVIDENCE/'docs-validation.json').write_text(json.dumps(audit,ensure_ascii=False,indent=2)+'\n')
    print(json.dumps(audit,ensure_ascii=False))
    if result.returncode:raise SystemExit(result.returncode)


if __name__=='__main__':main()
