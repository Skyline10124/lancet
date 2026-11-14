"""显式提交与撤销提示的核心事务回归。"""
import json
import sys
import unittest
from pathlib import Path
sys.path.insert(0, str(Path(__file__).resolve().parent))
import test_semantics


class CommitSemantics(unittest.TestCase):
    setUp = test_semantics.Semantics.setUp
    run_script = test_semantics.Semantics.run_script
    unchanged = test_semantics.Semantics.unchanged
    def test_v3_uncommitted_commit_and_failure(self):
        out = self.run_script('repo.write("a.txt", "changed")\nreturn {"ok": True}')
        self.assertTrue(out['ran'])
        self.assertEqual(out['outcome'], 'uncommitted_reverted')
        self.assertEqual(out['value'], {'ok': True})
        self.assertEqual(out['candidate_changes'], {'a.txt': 'changed'})
        self.assertEqual(out['revert_notice'], '未调用 commit()：本次对 1 个文件的修改已撤销；需要保留改动时请调用 commit()')
        self.unchanged()
        out = self.run_script('repo.write("a.txt", "changed")\ncommit()\nfail("bad")')
        self.assertFalse(out['ran'])
        self.unchanged()
        out = self.run_script('repo.write("a.txt", "kept")\ncommit()\nreturn 42')
        self.assertEqual(out['outcome'], 'committed')
        self.assertEqual((self.fixture/'a.txt').read_text(), 'kept')

    def test_v3_read_reset_and_marker_scope(self):
        out = self.run_script('return repo.read("a.txt")')
        self.assertEqual(out['outcome'], 'read_only')
        self.assertIsNone(out['revert_notice'])
        out = self.run_script('repo.write("a.txt", "x")\nrepo.reset("a.txt")')
        self.assertEqual(out['outcome'], 'read_only')
        out = self.run_script('commit()\nrepo.write("a.txt", "early")')
        self.assertEqual(out['outcome'], 'committed', 'commit 是整次调用的标记，提前调用另作误用观察')
        self.assertEqual(out['events'][0]['args']['changed_files'], 0)
        out = self.run_script('repo.write("a.txt", "next")')
        self.assertEqual(out['outcome'], 'uncommitted_reverted', '提交标记不得跨调用')
        self.assertEqual((self.fixture/'a.txt').read_text(), 'early')
        self.assertFalse(self.run_script('discard(1)')['ran'])

    def test_v3_notice_reservation(self):
        out = self.run_script('repo.write("a.txt", "changed")\nreturn "x" * 65278')
        self.assertTrue(out['ran'])
        self.assertLessEqual(len(out['content'].encode()), 65536)
        self.unchanged()
        out = self.run_script('repo.write("a.txt", "changed")\ncommit()\nreturn "x" * 65279')
        self.assertEqual(out['outcome'], 'OutputLimitExceeded')
        self.unchanged()



class DriverRecords(unittest.TestCase):
    def test_git_cannot_discover_research_worktree(self):
        import tempfile
        import driver
        with tempfile.TemporaryDirectory(dir=driver.TEMP) as td:
            root=Path(td);fixture=root/'repo';fixture.mkdir()
            (root/'canned.json').write_text('[]')
            out=driver.run_act(fixture,root,'r = env.run(["git", "rev-parse", "--show-toplevel"], check=False)\nreturn {"code": r.code, "stdout": r.stdout}', 'A',root)
            self.assertTrue(out['ran'])
            self.assertNotEqual(out['value']['code'],0,'夹具不能向上发现研究仓库')
            self.assertEqual(out['value']['stdout'],'')

    def test_notice_roundtrip_and_correct_candidate(self):
        import io
        import tempfile
        from unittest.mock import patch
        import driver
        requests=[]
        with tempfile.TemporaryDirectory(dir=driver.TEMP) as td:
            root=Path(td); key=root/'key'; key.write_text('  secret-sentinel~\n')
            config={**driver.MODELS['deepseek'],'key_file':str(key)}
            scripts=['return repo.read("config.json")',
                     'repo.edit("config.json", "30", "60")\nreturn repo.diff()',
                     'repo.edit("config.json", "30", "60")\ncommit()\nreturn repo.diff()']
            # 替换内容直接来自任务，不把测试锁在某个字面值。
            spec=json.loads((driver.ROOT/'tasks/edit-01/spec.json').read_text())
            p,old,new=spec['replacement']
            scripts[1]=f'repo.edit({p!r}, {old!r}, {new!r})\nreturn repo.diff()'
            scripts[2]=f'repo.edit({p!r}, {old!r}, {new!r})\ncommit()\nreturn repo.diff()'
            def respond(request, **kwargs):
                body=json.loads(request.data);requests.append(body)
                i=len(requests)-1
                msg=dict(role='assistant',content='done')
                if i<len(scripts):
                    msg['tool_calls']=[dict(id=str(i),type='function',function=dict(name='act',arguments=json.dumps(dict(script=scripts[i]))))]
                return io.BytesIO(json.dumps(dict(model='test-model',provider='test-provider',choices=[dict(message=msg)],usage=dict(total_tokens=1))).encode())
            with patch.dict(driver.MODELS,deepseek=config), patch('driver.urllib.request.urlopen',side_effect=respond):
                summary=driver.run_one(('composed','deepseek','A','edit-01',1),'unit',root)
            rows=[json.loads(s) for s in (root/(summary['run_id']+'.jsonl')).read_text().splitlines()]
            calls=[r for r in rows if r['type']=='act']
            self.assertFalse(calls[1]['check']['passed'])
            self.assertTrue(calls[1]['candidate_check']['passed'])
            self.assertTrue(summary['passed'])
            self.assertTrue(requests[2]['messages'][-1]['content'].endswith(calls[1]['outcome']['revert_notice']))
            self.assertNotIn('candidate',requests[2]['messages'][-1]['content'])
            self.assertNotIn('secret-sentinel',json.dumps(rows))
            from report_v3 import aggregate
            m=aggregate([dict(summary=summary,calls=calls,rows=rows)])
            self.assertEqual(m['missed_correct_runs']['n'],1)
            self.assertEqual(m['rescued_runs']['n'],1)
            self.assertEqual(m['keep_recovery']['next_rewrite_commit_all'],dict(n=1,d=1,pct=100.0))

    def test_budget_reserves_inflight_and_stops(self):
        import tempfile
        import threading
        from unittest.mock import patch
        import budget
        import driver
        with tempfile.TemporaryDirectory(dir=driver.TEMP) as td:
            b=budget.Budget.__new__(budget.Budget)
            b.lock=threading.Lock(); b.reserved=0; b.local_cost=4.5; b.stopped=False
            b.baseline=0; b.key='sentinel'; b.prompt_price=0; b.completion_price=0.3/budget.MAX_TOKENS
            with patch.object(budget,'EVIDENCE',Path(td)),patch.object(budget,'usage',return_value=dict(time=1,usage=4.5)):
                reserve=b.admit({})
                with self.assertRaises(budget.BudgetStop): b.admit({})
                self.assertTrue(b.stopped)
                b.finish(reserve,{'usage':{'cost':0.1}})
                self.assertAlmostEqual(b.reserved,0)
                self.assertAlmostEqual(b.local_cost,4.6)
