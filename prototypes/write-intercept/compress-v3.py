#!/usr/bin/env python3
"""无损压缩第三轮大体积原始 JSON；解压后字节必须完全相同。"""
import gzip
import hashlib
import json
from pathlib import Path
R=Path(__file__).resolve().parent/'results/v3'
rows=[]
for p in [R/'checkouts.json',R/'checkouts-before-optimization.json',*(R/'runs').glob('*.json')]:
    if not p.exists() or p.stat().st_size<262144:continue
    data=p.read_bytes();dest=p.with_suffix('.json.gz')
    if dest.exists() and gzip.decompress(dest.read_bytes())!=data:dest=p.with_name(p.stem+'-before-optimization.json.gz')
    compressed=gzip.compress(data,mtime=0);assert gzip.decompress(compressed)==data
    dest.write_bytes(compressed);p.unlink()
    rows.append(dict(path=str(dest.relative_to(R)),raw_bytes=len(data),compressed_bytes=len(compressed),raw_sha256=hashlib.sha256(data).hexdigest()))
(R/'compression.json').write_text(json.dumps(rows,indent=2)+'\n')
print('无损压缩文件',len(rows))
