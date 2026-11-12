"""逐字提取草稿，仅应用语言探针证实的两处事实修正。"""
import hashlib
import json
from pathlib import Path
import re
import sys
ROOT = Path(__file__).resolve().parent
source = Path(sys.argv[1]).read_text()
blocks = re.findall(r'^```text\n(.*?)^```$', source, re.M | re.S)
system = blocks[0]
a = re.search(r'^````markdown\n(.*?)^````$',source,re.M|re.S)[1]
fixes = [
    ('| recursion | Not allowed. A function may not call itself. |','| recursion | Allowed in this interpreter. |','recursion'),
    ('| `print(x)` | Allowed, but the output is not shown to you. Return what you need. |','| `print(x)` | Not available in this environment. Return what you need. |','print'),
]
for old,new,_ in fixes:
    assert a.count(old) == 1
    a = a.replace(old,new)
b_return = re.search(r'^```markdown\n(## Returning a value.*?)^```$',source,re.M|re.S)[1].rstrip()
a_return = re.search(r'## Returning a value\n.*?(?=\n## Repository)',a,re.S)[0]
b = a.replace(a_return,b_return+'\n')
b_example = re.findall(r'^```python\n(.*?)^```$',source,re.M|re.S)[1]
b = re.sub(r'```python\n.*?```',lambda _: '```python\n'+b_example+'```',b,flags=re.S)
for name,content in [('system.md',system),('act_tool_A.md',a),('act_tool_B.md',b)]:
    (ROOT/'prompts'/name).write_text(content)
(ROOT/'evidence/prompt-extraction.json').write_text(json.dumps({'source_sha256':hashlib.sha256(source.encode()).hexdigest(),'fixes':[{'old':x,'new':y,'probe':p} for x,y,p in fixes]},ensure_ascii=False,indent=2)+'\n')
print('已提取固定正文；仅修正 recursion 与 print 两行')
