import json
from pathlib import Path
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[1]
BIN = ROOT / 'target/debug/act-sim'


class Semantics(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.base = Path(self.temp.name)
        self.fixture = self.base / 'repo'
        self.fixture.mkdir()
        (self.fixture / 'a.txt').write_text('old old\n')
        self.addCleanup(self.temp.cleanup)

    def run_script(self, script, mode='A'):
        source = self.base / 'script.star'
        source.write_text(script)
        proc = subprocess.run([str(BIN), '--fixture', str(self.fixture), '--script', str(source), '--mode', mode], capture_output=True, text=True, check=True)
        return json.loads(proc.stdout)

    def unchanged(self):
        self.assertEqual((self.fixture / 'a.txt').read_text(), 'old old\n', '失败应回滚文件')

    def test_a_return_and_line(self):
        self.assertEqual(self.run_script('return repo.read("a.txt")')['value'], 'old old\n')
        out = self.run_script('x = 1\nfail("坏了")')
        self.assertEqual(out['line'], 2, '包装后的行号应减一')
        self.assertIn('script.star:2:', out['error'])
        out = self.run_script('x = 1\nreturn (')
        self.assertFalse(out['parsed'])
        self.assertEqual(out['line'], 2)

    def test_b_top_return_and_double_result(self):
        self.assertFalse(self.run_script('return 1', 'B')['parsed'])
        out = self.run_script('repo.write("a.txt", "changed")\nresult(1)\nresult(2)', 'B')
        self.assertFalse(out['ran'])
        self.assertIn('第二次', out['error'])
        self.unchanged()

    def test_commit_discard_fail(self):
        for mode in ['A', 'B']:
            out = self.run_script('repo.write("a.txt", "changed")\ndiscard({"why": "failed"})\nrepo.write("a.txt", "wrong")', mode)
            self.assertEqual(out['value'], {'why': 'failed'})
            self.assertEqual(out['outcome'], 'discarded')
            self.unchanged()
            self.assertFalse(self.run_script('repo.write("a.txt", "changed")\nfail("failed")', mode)['ran'])
            self.unchanged()
        out = self.run_script('repo.write("a.txt", "kept")')
        self.assertEqual(out['outcome'], 'committed')
        self.assertEqual((self.fixture / 'a.txt').read_text(), 'kept')

    def test_edit_cardinality(self):
        for old, all_arg in [('absent', ''), ('old', ''), ('absent', ', all = True')]:
            out = self.run_script(f'repo.edit("a.txt", "{old}", "new"{all_arg})')
            self.assertFalse(out['ran'])
            self.assertIn('EditMatchError', out['error'])
            self.unchanged()
        self.assertTrue(self.run_script('repo.edit("a.txt", "old old", "once")')['ran'])
        self.assertEqual((self.fixture / 'a.txt').read_text(), 'once\n')
        (self.fixture / 'a.txt').write_text('old old\n')
        self.assertTrue(self.run_script('repo.edit("a.txt", "old", "new", all = True)')['ran'])
        self.assertEqual((self.fixture / 'a.txt').read_text(), 'new new\n')

    def test_output_limit(self):
        for ending, mode in [('return', 'A'), ('result', 'B'), ('discard', 'A')]:
            code = '"界" * 22000'
            expr = f'{ending} {code}' if ending == 'return' else f'{ending}({code})'
            out = self.run_script('repo.write("a.txt", "changed")\n' + expr, mode)
            self.assertEqual(out['outcome'], 'OutputLimitExceeded')
            self.unchanged()
        self.assertTrue(self.run_script('return "x" * 65534')['ran'])
        self.assertFalse(self.run_script('return "x" * 65535')['ran'])

    def test_repo_text_and_commands(self):
        out = self.run_script('repo.write("dir/b.txt", "one\\nTODO fix\\nthree\\n")\nreturn {"hits": repo.search("TODO", glob="**/*.txt"), "entries": repo.list(), "view": str(text(repo.read("dir/b.txt")).grep("TODO", before=1).tail(2)), "lines": text("a\\nb\\n").head(1).lines(), "status": repo.status(), "diff": repo.diff()}')
        self.assertEqual(out['value']['hits'][0]['line'], 2)
        self.assertEqual(out['value']['lines'], ['a'])
        self.assertIn('dir/', out['value']['entries'])
        self.assertIn('+TODO fix', out['value']['diff'])
        self.assertEqual(self.run_script('return repo.read("absent", missing_ok=True)')['value'], None)
        self.assertTrue(self.run_script('repo.write("a.txt", "bad")\nrepo.reset("a.txt")')['ran'])
        self.unchanged()
        out = self.run_script('return env.pipe([["cat", "a.txt"], ["wc", "-l"]])')
        self.assertEqual(out['value']['stdout'].strip(), '1')
        self.assertIn('模拟环境未提供该命令', self.run_script('env.run(["python", "-V"])')['error'])


if __name__ == '__main__':
    unittest.main()
