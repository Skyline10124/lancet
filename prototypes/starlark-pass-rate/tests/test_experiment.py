import importlib
import json
from pathlib import Path
import shutil
import sys
import tempfile
import unittest
from unittest.mock import patch
import urllib.error
ROOT=Path(__file__).resolve().parents[1]
sys.path.insert(0,str(ROOT))
import driver
from check import evaluate, files_at


def oracle(task):
    spec=json.loads((task/'spec.json').read_text()); category=spec['category']; ident=spec['id']
    if category=='edit':
        p,old,new=spec['replacement']
        return [f'return repo.read({json.dumps(p)})',f'repo.edit({json.dumps(p)}, {json.dumps(old)}, {json.dumps(new)})\nreturn repo.diff()']
    if category=='batch':
        _,old,new=spec['replacement']
        return [f'paths = {{h.path: True for h in repo.search({json.dumps(old)}, glob="src/**")}}\nfor p in paths:\n    repo.edit(p, {json.dumps(old)}, {json.dumps(new)}, all=True)\nreturn repo.diff()']
    if category=='rollback':
        p,old,new=spec['replacement']
        return [f'repo.edit({json.dumps(p)}, {json.dumps(old)}, {json.dumps(new)})\nr = env.run({json.dumps(spec["command"])}, check=False)\nif r.code != 0:\n    discard(r.stdout + r.stderr)\nreturn r.stdout']
    if category=='test':
        return [f'r = env.run({json.dumps(spec["command"])}, check=False)\nreturn r.stdout + r.stderr']
    return [{
        'read-01':'return repo.search("TODO", glob="src/**")',
        'read-02':'return str(text(repo.read("logs/service.log")).grep("ALERT", before=1, after=1))',
        'read-03':'counts = {p: len(repo.read("data/"+p).splitlines()) for p in repo.list("data") if p.endswith(".txt")}\nreturn {"counts": counts, "total": counts["a.txt"] + counts["b.txt"]}',
        'read-04':'return repo.search("FIXME", glob="src/**")',
        'probe-01':'v = repo.read("config/optional.json", missing_ok=True)\nreturn {"optional": "optional missing" if v == None else v, "region": json.decode(repo.read("config/default.json"))["region"]}',
        'probe-02':'services = [s for s in json.decode(repo.read("services.json"))["services"] if s["enabled"]]\ntotal = 0\nfor s in services:\n    total += s["port"]\nreturn {"names": sorted([s["name"] for s in services]), "ports": total}',
        'probe-03':'errors = text(repo.read("logs/large.log")).grep("ERROR")\nreturn {"errors": errors.lines(), "count": len(errors.lines())}',
        'probe-04':'targets = [t for t in json.decode(repo.read("deploy.json"))["targets"] if t["env"] == "prod"]\ntotal = 0\nfor t in targets:\n    total += t["replicas"]\nreturn {"names": sorted([t["name"] for t in targets]), "replicas": total, "overrides": repo.read("overrides.json", missing_ok=True) != None}',
    }[ident]]


def as_b(script):
    # 参考脚本只有最后一行使用顶层 return；discard 分支保持原样。
    return '\n'.join('result('+l[7:]+')' if l.startswith('return ') else l for l in script.splitlines())


class Experiment(unittest.TestCase):
    def test_all_tasks_have_working_oracles(self):
        rows=[]
        for task in sorted((ROOT/'tasks').iterdir()):
            for mode in ['A','B']:
                with self.subTest(task=task.name,mode=mode),tempfile.TemporaryDirectory() as tmp:
                    temp=Path(tmp); fixture=temp/'repo'; shutil.copytree(task/'fixture',fixture)
                    calls=[]
                    for script in oracle(task):
                        if mode=='B': script=as_b(script)
                        out=driver.run_act(fixture,task,script,mode,temp)
                        self.assertTrue(out['ran'],out['error'])
                        calls.append(dict(script=script,outcome=out))
                    check=evaluate(task,files_at(fixture),calls)
                    self.assertTrue(check['passed'],check['reasons'])
                    rows.append(dict(task=task.name,mode=mode,calls=calls,check=check))
                    bad=dict(files_at(fixture)); bad['unexpected.txt']='extra'
                    self.assertFalse(evaluate(task,bad,calls)['passed'],'额外改动必须导致失败')
                    if json.loads((task/'spec.json').read_text())['facts']:
                        self.assertFalse(evaluate(task,files_at(fixture),[])['passed'],'缺少返回事实必须失败')
        (ROOT/'results/v2/evidence/oracles.jsonl').write_text(''.join(json.dumps(row,ensure_ascii=False)+'\n' for row in rows))

    def test_api_five_attempts_and_no_auth_logging(self):
        records=[]
        error=urllib.error.HTTPError('https://invalid',429,'rate limited',{},None)
        with patch('driver.urllib.request.urlopen',side_effect=error) as open_mock,patch('driver.time.sleep') as sleep:
            with self.assertRaises(RuntimeError):
                driver.call_api(dict(base='https://invalid',model='model'),'secret-test',[],{},records.append)
        self.assertEqual(open_mock.call_count,5)
        self.assertEqual([x.args[0] for x in sleep.call_args_list],[1,2,4,8])
        self.assertNotIn('secret-test',json.dumps(records))

    def test_error_classes(self):
        for script,error,mode,expected in [
            ('return 1','parse error','B','Python 习惯'),
            ('repo.edit("x","a","b")','EditMatchError: 匹配 2 次','A','唯一匹配失败'),
            ('repo.unknown()','Object has no attribute unknown','A','接口误用'),
            ('env.run(["x"])','模拟环境未提供该命令','A','命令未预置'),
            ('return "x"*99999','OutputLimitExceeded','A','超出上限')]:
            self.assertEqual(driver.classify(script,dict(ran=False,error=error),mode),expected)

if __name__=='__main__': unittest.main()
