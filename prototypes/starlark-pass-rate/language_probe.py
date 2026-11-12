"""通过同一 act-sim 实测提示词列出的语法与标准库。"""
import json
from pathlib import Path
import subprocess
import tempfile
ROOT = Path(__file__).resolve().parent
PROBES = {
    'while': 'while False:\n    pass',
    'try': 'try:\n    x = 1\nexcept:\n    pass',
    'raise': 'raise "oops"',
    'import': 'import json',
    'class': 'class C:\n    pass',
    'with': 'with x:\n    pass',
    'yield': 'def f():\n    yield 1',
    'global': 'global x',
    'assert': 'assert True',
    'f-string': 'x = 1\nOUT(f"{x}")',
    'is None': 'x = None\nOUT(x is None)',
    'string iteration': 'a = []\nfor c in "ab":\n    a.append(c)\nOUT(a)',
    'string elems': 'OUT([c for c in "ab".elems()])',
    'chained comparison': 'OUT(1 < 2 < 3)',
    'recursion': 'def f(n):\n    if n == 0:\n        return 0\n    return f(n - 1)\nOUT(f(2))',
    'set': 'OUT(set())',
    'print': 'print("PRINT_SENTINEL")\nOUT("returned")',
    'def': 'def f(x):\n    return x + 1\nOUT(f(2))',
    'lambda': 'f = lambda x: x + 1\nOUT(f(2))',
    'list comprehension': 'OUT([x * 2 for x in [1, 2]])',
    'dict comprehension': 'OUT({str(x): x * 2 for x in [1, 2]})',
    'if elif else': 'x = 2\nif x == 1:\n    a = 1\nelif x == 2:\n    a = 2\nelse:\n    a = 3\nOUT(a)',
    'for break continue': 'a = []\nfor x in range(5):\n    if x == 1:\n        continue\n    if x == 3:\n        break\n    a.append(x)\nOUT(a)',
    'len': 'OUT(len([1, 2]))',
    'sorted': 'OUT(sorted([3, 1, 2]))',
    'enumerate': 'OUT(list(enumerate(["a", "b"])))',
    'zip': 'OUT(list(zip([1, 2], [3, 4])))',
    'min': 'OUT(min([3, 1, 2]))',
    'max': 'OUT(max([3, 1, 2]))',
    'any': 'OUT(any([False, True]))',
    'all': 'OUT(all([False, True]))',
    'format': 'OUT("{}:{}".format("x", 2))',
    'percent format': 'OUT("%s: %d" % ("x", 2))',
    'string methods': 'OUT([" X ".strip().lower(), "a,b".split(","), "a\\nb".splitlines(), "ab".replace("a","c"), "ab".startswith("a"), "ab".endswith("b"), "-".join(["a","b"])])',
    'json': 'OUT(json.decode(json.encode({"x": [1, True, None]})))',
}


def run():
    with tempfile.TemporaryDirectory() as tmp:
        root = Path(tmp); fixture = root / 'repo'; fixture.mkdir()
        with (ROOT / 'evidence/language-probes.jsonl').open('w') as output:
            for mode in ['A', 'B']:
                for name, source in PROBES.items():
                    script = source.replace('OUT(', 'return (' if mode == 'A' else 'result(')
                    p = root / 'probe.star'; p.write_text(script)
                    proc = subprocess.run([str(ROOT/'target/debug/act-sim'),'--fixture',str(fixture),'--script',str(p),'--mode',mode],text=True,capture_output=True,check=True)
                    row = dict(probe=name,mode=mode,script=script,**json.loads(proc.stdout),process_stderr=proc.stderr)
                    output.write(json.dumps(row,ensure_ascii=False)+'\n')
                    print(mode,name,row['parsed'],row['ran'],str(row['value']) if row['ran'] else row['error'].splitlines()[0])


if __name__ == '__main__':
    run()
