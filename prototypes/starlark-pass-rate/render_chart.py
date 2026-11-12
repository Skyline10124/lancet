"""从报告原始数字生成独立静态图，聊天中经 html_preview/render 展示。"""
import json
from driver import ROOT
metrics=json.loads((ROOT/'evidence/metrics.json').read_text())
rows=[]
for item in metrics:
    bars=[]
    for key,label,color in [('completed','任务完成','--chart-1'),('first','首次 act 完成','--chart-2')]:
        n,d=item[key],item['n']; pct=100*n/d if d else 0
        bars.append(f'<div class="barrow"><span>{label}</span><div class="track"><div class="bar" style="width:{pct}%;background:var({color})"></div></div><b>{n}/{d} · {pct:.2f}%</b></div>')
    rows.append(f'<section><strong>{item["model"]} · {item["mode"]}</strong>'+''.join(bars)+'</section>')
html='''<!doctype html><html lang="zh"><head><meta charset="utf-8"><style>
:root { color-scheme:light dark; } body {margin:0;color:var(--foreground);font-family:var(--font-sans);} section{margin:0 0 22px} strong{font-size:14px} .barrow{display:grid;grid-template-columns:106px 1fr 142px;gap:10px;align-items:center;margin-top:8px;font-size:12px}.track{height:15px;background:var(--secondary);border-radius:4px;overflow:hidden}.bar{height:15px;border-radius:4px} b{text-align:right;font-weight:500} p{font-size:12px;color:var(--muted-foreground);margin:8px 0 0} @media(max-width:500px){.barrow{grid-template-columns:90px 1fr 106px;gap:6px;font-size:10px}} </style></head><body>'''+''.join(rows)+'''<p>冻结判定器统计；api_error 不进入分母。read-03 的已知判定假阴性在 REPORT.md 单列。</p></body></html>'''
(ROOT/'evidence/completion-chart.html').write_text(html)
