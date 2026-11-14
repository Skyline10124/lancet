"""用同一 metrics.json 绘制 v2/v3 完成率对照，供内联展示。"""
import html
import json
from driver import V3


def main():
    data=json.loads((V3/'evidence/metrics.json').read_text())
    sections=[]
    for alias,label in [('deepseek','DeepSeek'),('minimax','MiniMax')]:
        rows=[]
        for version,color in [('v2','--chart-2'),('v3','--chart-1')]:
            m=data['v2_composed'][alias]['A']['completion'] if version=='v2' else data['formal'][alias]['completion']
            rows.append(f'<div class="row"><span>{version} · composed/A</span><div class="track"><div class="bar" style="width:{m["pct"]}%;background:var({color})"></div></div><b>{m["n"]}/{m["d"]} · {m["pct"]:.2f}%</b></div>')
        sections.append('<section><strong>'+html.escape(label)+'</strong>'+''.join(rows)+'</section>')
    page='''<!doctype html><html lang="zh"><head><meta charset="utf-8"><style>
body{margin:0;color:var(--foreground);font-family:var(--font-sans)}
section{margin:0 0 20px}strong{font-size:14px}.row{display:grid;grid-template-columns:125px 1fr 132px;gap:8px;align-items:center;margin-top:8px;font-size:12px}.track{height:14px;background:var(--secondary);border-radius:4px;overflow:hidden}.bar{height:14px;border-radius:4px}b{text-align:right;font-weight:500}p{font-size:12px;color:var(--muted-foreground);line-height:1.5;margin:8px 0 0}@media(max-width:500px){.row{grid-template-columns:100px 1fr 110px;gap:5px;font-size:10px}}
</style></head><body>'''+''.join(sections)+'''<p>任务完成率，沿用 v2 严格判定；API/测台异常排除分母。DeepSeek 的模型名、端点及提供方已变化；差值不能解释为 commit 语义的因果效果。</p><p>来源：REPORT-v2.md、REPORT-v3.md；results/v3/evidence/metrics.json。</p></body></html>'''
    (V3/'evidence/completion-chart.html').write_text(page)


if __name__=='__main__':main()
