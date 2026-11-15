#!/usr/bin/env python3
"""生成可独立打开的比较图；每个指标均显示原始中位数和计量单位。"""
import json
from pathlib import Path
P=Path(__file__).resolve().parent;R=P/'results/v7';s=json.loads((R/'summary-v7.json').read_text())
data={}
for name,key,unit,mult in [('全量构建','LATTICE 全量构建','s',1),('Checkout 乙 b→a','checkout 乙 b→a','s',1),('Checkout 乙 a→b','checkout 乙 a→b','s',1),('100,000 文件空 act','fixed-100000','ms',1000)]:
 data[name]={'unit':unit,'values':{k:v*mult for k,v in s[key].items()}}
html='''<!doctype html><html lang="zh"><meta charset="utf-8"><style>
:root{font-family:var(--font-sans,system-ui);color:var(--foreground,#222)}
body{margin:0}label{display:flex;gap:12px;align-items:center;font-size:14px}select{font:inherit;padding:7px;border-radius:var(--radius,8px);color:inherit;background:var(--card,#f5f5f5);border:1px solid var(--border,#ddd)}
#plot{height:260px;margin-top:16px}.row{display:grid;grid-template-columns:100px 1fr 92px;gap:10px;align-items:center;height:48px;font-size:13px}.track{height:20px;background:var(--secondary,#eee);border-radius:4px}.bar{height:20px;border-radius:4px;min-width:2px;background:var(--chart-1,#5975d8)}.value{text-align:right;font-family:var(--font-mono,monospace)}.note{font-size:12px;color:var(--muted-foreground,#777);line-height:1.6}
</style><label>查看指标 <select id="metric"></select></label><div id="plot" role="img" aria-label="五配置中位数比较"></div><p class="note">每格至少三次，取中位数。B3 与 S3 v6 引用第六轮；其他配置重新测量。空 act：none/ns/B3 执行 true；S3 为 begin+finish+commit RPC。横条为各指标内的线性比例，数值保留三位小数。</p><script>
const data=DATA;const menu=document.querySelector('#metric');for(const key of Object.keys(data)){const o=document.createElement('option');o.textContent=key;menu.append(o)}
function draw(){const d=data[menu.value],max=Math.max(...Object.values(d.values)),p=document.querySelector('#plot');p.replaceChildren();Object.entries(d.values).forEach(([name,val],i)=>{const row=document.createElement('div');row.className='row';const text=document.createElement('span');text.textContent=name;const t=document.createElement('div');t.className='track';const bar=document.createElement('div');bar.className='bar';bar.style.width=(100*val/max)+'%';bar.style.background='var(--chart-'+(i+1)+',#5975d8)';t.append(bar);const v=document.createElement('span');v.className='value';v.textContent=val.toFixed(3)+' '+d.unit;row.append(text,t,v);p.append(row)})}menu.onchange=draw;draw();
</script></html>'''.replace('DATA',json.dumps(data,ensure_ascii=False))
(R/'performance-v7.html').write_text(html)
