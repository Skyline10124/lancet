"""v2 的历史假阴性、归一化和观测口径回归。"""
import io
import json
from pathlib import Path
import shutil
import sys
import tempfile
import unittest
from unittest.mock import patch
import urllib.error

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT))
import driver
from check import evaluate, files_at
from commands import normalize


class V2(unittest.TestCase):
    def test_read03_six_v1_false_negatives(self):
        task = ROOT/'tasks/read-03'
        cases = ['deepseek-A-read-03-r1', 'deepseek-A-read-03-r3',
                 'deepseek-B-read-03-r1', 'deepseek-B-read-03-r3',
                 'minimax-A-read-03-r1', 'minimax-B-read-03-r3']
        for ident in cases:
            with self.subTest(run_id=ident):
                rows = [json.loads(s) for s in (ROOT/f'results/formal/{ident}.jsonl').read_text().splitlines()]
                self.assertFalse(rows[-1]['passed'])
                calls = [r for r in rows if r['type']=='act']
                self.assertTrue(evaluate(task, files_at(task/'fixture'), calls)['passed'])

    def test_read03_nested_and_wrong_associations(self):
        task = ROOT/'tasks/read-03'
        for value, expected in [
            ({'counts': {'data/a.txt': {'lines': 3}, 'data/b.txt': {'count': 4}}, 'total': 7}, True),
            ({'files': [{'path': 'data/a.txt', 'lines': 3}, {'path': 'data/b.txt', 'lines': 4}], 'total': 7}, True),
            ('3 data/a.txt\n4 data/b.txt\n7 total\n', True),
            ('4 data/a.txt\n3 data/b.txt\n7 total\n', False),
            ({'a.txt': {'lines': 4}, 'b.txt': {'lines': 3}, 'total': 7}, False),
            ({'a.txt': {'lines': 13}, 'b.txt': {'lines': 4}, 'total': 7}, False),
        ]:
            with self.subTest(value=value):
                calls = [{'outcome': {'ran': True, 'value': value}}]
                self.assertEqual(evaluate(task, files_at(task/'fixture'), calls)['passed'], expected)

    def test_command_family_and_checker_agree(self):
        for ident in ['test-01', 'test-02', 'test-03', 'test-04', 'rollback-01']:
            task=ROOT/'tasks'/ident
            spec=json.loads((task/'spec.json').read_text())
            variants=[spec['command']+tail for tail in [[], ['-q'], ['--quiet'], ['--no-fail-fast'], ['--','--nocapture'], ['parser::empty_input'], ['--','--test-threads=1'], ['--','--test-threads','2']]]
            for argv in variants:
                with self.subTest(task=ident,argv=argv), tempfile.TemporaryDirectory() as td:
                    temp=Path(td); fixture=temp/'repo'; shutil.copytree(task/'fixture',fixture)
                    script=''
                    if ident.startswith('rollback'):
                        p,old,new=spec['replacement']
                        script=f'repo.edit({p!r}, {old!r}, {new!r})\n'
                    script+=f'r = env.run({argv!r}, check=False)\n'
                    script+= 'discard(r.stdout + r.stderr)' if ident.startswith('rollback') else 'return r.stdout + r.stderr'
                    out=driver.run_act(fixture,task,script,'A',temp)
                    self.assertTrue(out['ran'],out.get('error'))
                    self.assertTrue(evaluate(task,files_at(fixture),[{'outcome':out}])['passed'])
                    self.assertEqual(normalize(argv),normalize(spec['command']))
        for argv in [['cargo','--version'],['cargo','build'],['cargo','test','--no-run'],['cargo','test','--','--list'],['cargo','test','--bogus']]:
            self.assertEqual(normalize(argv),argv)

    def test_minimax_ten_attempts_and_capped_backoff(self):
        records=[]
        def rejected(*args, **kwargs):
            raise urllib.error.HTTPError('https://invalid',429,'rate limited',{},io.BytesIO(b'raw rate limit'))
        with patch('driver.urllib.request.urlopen',side_effect=rejected) as req, patch('driver.time.sleep') as sleep:
            with self.assertRaises(RuntimeError):
                driver.call_api(driver.MODELS['minimax'],'secret-test',[],{},records.append)
        self.assertEqual(req.call_count,10)
        self.assertEqual([c.args[0] for c in sleep.call_args_list],[1,2,4,8,16,32,64,120,120])
        self.assertNotIn('secret-test',json.dumps(records))

    def test_metrics_and_composed_examples(self):
        from metrics import act_metrics, source_calls
        self.assertEqual(source_calls('# print(1)\ns = "repo.write(1)"\nprint("x")\nprint(2)'), {'print': 2})
        task=ROOT/'tasks/rollback-01'
        for mode in 'AB':
            baseline=driver.tool_prompt('base',mode)
            composed=driver.tool_prompt('composed',mode)
            self.assertEqual(baseline.split('## Example')[0],composed.split('## Example')[0])
            code=composed.split('```python\n')[1].split('```')[0]
            self.assertLessEqual(len(code.splitlines()),20)
            for failing in (False,True):
                with self.subTest(mode=mode,failing=failing),tempfile.TemporaryDirectory() as td:
                    temp=Path(td); fixture=temp/'repo'; (fixture/'src').mkdir(parents=True)
                    original='pub const MAX_ITEMS: usize = 10;\n'
                    (fixture/'src/config.rs').write_text(original)
                    canned=temp/'canned.json'
                    canned.write_text(json.dumps([dict(argv=['cargo','test'],code=101 if failing else 0,stdout='test summary',stderr='test reason' if failing else '')]))
                    out=driver.run_act(fixture,temp,code,mode,temp)
                    self.assertTrue(out['ran'],out.get('error'))
                    self.assertEqual(out['outcome'],'discarded' if failing else 'committed')
                    self.assertEqual((fixture/'src/config.rs').read_text(),original if failing else original.replace('10','20'))
                    m=act_metrics(code,out)
                    self.assertEqual(m['category_counts']['read'],1 if failing else 2)
                    self.assertEqual(m['category_counts']['write'],1)
                    self.assertEqual(m['category_counts']['execute'],1)
                    self.assertFalse(m['read_only'])
            with tempfile.TemporaryDirectory() as td:
                temp=Path(td); fixture=temp/'repo'; shutil.copytree(task/'fixture',fixture)
                out=driver.run_act(fixture,task,'repo.list()\nrepo.status()\nrepo.diff()\nrepo.read("Cargo.toml")',mode,temp)
                m=act_metrics('',out)
                self.assertEqual(m['category_counts']['read'],4)
                self.assertTrue(m['read_only'])
                self.assertFalse(m['single_interface'])
                out=driver.run_act(fixture,task,'print("x")',mode,temp)
                self.assertFalse(out['ran'])
                self.assertEqual(act_metrics('print("x")',out)['print_calls'],1)

    def test_overview_sorted_and_truncated(self):
        with tempfile.TemporaryDirectory() as td:
            task=Path(td); (task/'fixture/nested').mkdir(parents=True); (task/'task.md').write_text('goal')
            for i in range(205): (task/f'fixture/nested/{i:03}.txt').write_text('')
            prompt=driver.user_prompt(task,'overview')
            self.assertTrue(prompt.startswith('Repository files:\nnested/000.txt\n'))
            self.assertIn('nested/199.txt\n[Truncated: showing 200 of 205 files.]',prompt)
            self.assertNotIn('nested/200.txt',prompt)
            self.assertEqual(driver.user_prompt(task,'base'),'goal')

    def test_budget_usage_and_reasoning_roundtrip(self):
        with tempfile.TemporaryDirectory() as td:
            temp=Path(td); key=temp/'key'; key.write_text('  unit-key~\n')
            config={**driver.MODELS['deepseek'],'key_file':str(key)}
            messages=[]
            def respond(request, **kwargs):
                body=json.loads(request.data); messages.append(body['messages'])
                calls=[dict(id=f'call-{i}',type='function',function=dict(name='act',arguments=json.dumps({'script':'return 1'}))) for i in range(9)]
                message=dict(role='assistant',content='',reasoning_content='unaltered reasoning',tool_calls=calls)
                if len(messages)==2: message=dict(role='assistant',content='done',reasoning_content='final reasoning')
                data=dict(choices=[dict(message=message)],usage=dict(prompt_tokens=10,completion_tokens=2,total_tokens=12))
                return io.BytesIO(json.dumps(data).encode())
            with patch.dict(driver.MODELS,deepseek=config), patch('driver.urllib.request.urlopen',side_effect=respond):
                summary=driver.run_one(('base','deepseek','A','read-02',1),'pilot',temp)
            self.assertEqual(summary['act_count'],8)
            self.assertTrue(summary['budget_exhausted'])
            self.assertEqual(summary['usage']['total_tokens'],24)
            self.assertEqual(messages[1][2]['reasoning_content'],'unaltered reasoning')
            self.assertEqual(messages[1][-1]['content'],'act call limit reached (8).')
            rows=[json.loads(line) for line in (temp/'base-deepseek-A-read-02-r1.jsonl').read_text().splitlines()]
            self.assertEqual(len([r for r in rows if r['type']=='act']),8)
            self.assertTrue(all(r['usage']['total_tokens']==12 for r in rows if r['type']=='act'))

if __name__=='__main__':
    unittest.main()
