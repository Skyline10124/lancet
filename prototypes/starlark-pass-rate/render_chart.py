"""从 v2 原始计数生成聊天中的通过率对照图；不修改 Markdown 报告。"""
import html
import json
from driver import V2

metrics=json.loads((V2/'evidence/metrics.json').read_text())
rows=[]
for item in metrics:
    bars=[]
    for key,label,color in [('completed','任务完成','--chart-1'),('first','首次 act 完成','--chart-2')]:
        n,d=item[key],item['n']; percent=100*n/d if d else 0
        bars.append(f'<div class="barrow"><span>{label}</span><div class="track"><div class="bar" style="width:{percent}%;background:var({color})"></div></div><b>{n}/{d} · {percent:.2f}%</b></div>')
    title=html.escape(f'{item["group"]} · {item["model"]} · {item["mode"]}')
    rows.append(f'<section><strong>{title}</strong>'+''.join(bars)+'</section>')
page='''<!doctype html><html lang="zh"><head><meta charset="utf-8"><style>
body{margin:0;color:var(--foreground);font-family:var(--font-sans)}section{margin:0 0 18px}strong{font-size:13px}.barrow{display:grid;grid-template-columns:96px 1fr 140px;gap:8px;align-items:center;margin-top:6px;font-size:12px}.track{height:12px;background:var(--secondary);border-radius:3px;overflow:hidden}.bar{height:12px;border-radius:3px}b{text-align:right;font-weight:500}p{font-size:12px;color:var(--muted-foreground);margin:8px 0 0}@media(max-width:500px){.barrow{grid-template-columns:84px 1fr 110px;gap:5px;font-size:10px}}
</style></head><body>'''+''.join(rows)+'''<p>仅 status=complete 的正式运行进入分母；原始计数与异常见 REPORT-v2.md。</p></body></html>'''
(V2/'evidence/completion-chart.html').write_text(page)
