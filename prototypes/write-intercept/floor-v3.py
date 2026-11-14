#!/usr/bin/env python3
"""独立下限对照，不能当作完整 B3 路径性能。"""
import json
import re
import subprocess
from pathlib import Path
HERE=Path(__file__).resolve().parent
rows=[]
for rep in range(3):
    for mode in ['native','cont','cont-read','addfd-read']:
        command=[str(HERE/'.scratch/notification-floor-v3'),mode,'20000','sync']
        result=subprocess.run(command,capture_output=True,text=True)
        value=re.search(r'([\d.]+) us/iter',result.stdout)
        rows.append(dict(rep=rep,mode=mode,command=command,returncode=result.returncode,stdout=result.stdout,stderr=result.stderr,us=float(value[1]) if value else None))
(HERE/'results/v3/notification-floor.json').write_text(json.dumps(rows,indent=2)+'\n')
