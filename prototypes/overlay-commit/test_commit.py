"""在独立小 Git 仓库中验证提交分类；需在用户命名空间运行。"""
import os
from pathlib import Path
import shutil
import stat
import subprocess
import tempfile
import unittest

from commit import classify, commit, snapshot, fs_time


class CommitTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(dir=Path.home() / 'exp/overlay')
        self.root = Path(self.tmp.name)
        self.repo = self.root / 'repo'
        self.upper = self.root / 'upper'
        self.before = self.root / 'before'
        self.repo.mkdir()
        self.upper.mkdir()
        subprocess.run(['git', 'init', '-q', str(self.repo)], check=True)
        (self.repo / 'a').write_text('原文\n')
        (self.repo / 'b').write_text('原文\n')
        self.t0 = fs_time(self.root)
        self.snap = snapshot(self.repo, False)

    def tearDown(self):
        self.tmp.cleanup()

    def apply(self):
        return commit(self.repo, self.upper, self.before, self.snap, self.t0, 'both')

    def test_whiteout_character_zero_zero(self):
        os.mknod(self.upper / 'a', stat.S_IFCHR | 0o600, os.makedev(0, 0))
        self.assertEqual(classify(self.repo, self.upper)['operations'][0]['kind'], 'delete')
        r = self.apply()
        self.assertEqual(r['outcome'], 'committed')
        self.assertFalse((self.repo / 'a').exists())
        self.assertEqual((self.before / 'a').read_text(), '原文\n')

    def test_opaque_directory(self):
        (self.repo / 'dir').mkdir()
        (self.repo / 'dir/old').write_text('旧文件')
        self.t0 = fs_time(self.root)
        self.snap = snapshot(self.repo, False)
        (self.upper / 'dir').mkdir()
        os.setxattr(self.upper / 'dir', 'user.overlay.opaque', b'y')
        (self.upper / 'dir/new').write_text('新文件')
        r = self.apply()
        self.assertEqual(r['outcome'], 'committed')
        self.assertEqual(sorted(p.name for p in (self.repo / 'dir').iterdir()), ['new'])
        self.assertEqual((self.before / 'dir/old').read_text(), '旧文件')
        self.assertEqual(r['overlay_xattr_residual_count'], 0)

    def test_timestamp_only_and_permission_change(self):
        shutil.copyfile(self.repo / 'a', self.upper / 'a')
        os.utime(self.upper / 'a', ns=(1, 1))
        self.assertEqual(classify(self.repo, self.upper)['timestamp_only'], ['a'])
        os.chmod(self.upper / 'a', 0o755)
        self.assertEqual(classify(self.repo, self.upper)['timestamp_only'], [])
        self.apply()
        self.assertEqual(stat.S_IMODE((self.repo / 'a').stat().st_mode), 0o755)

    def test_redirect_is_recorded_without_processing_children(self):
        (self.upper / 'dir').mkdir()
        (self.upper / 'dir/child').write_text('忽略')
        os.setxattr(self.upper / 'dir', 'user.overlay.redirect', b'/old')
        r = self.apply()
        self.assertEqual(r['classification']['redirect'], ['dir'])
        self.assertFalse((self.repo / 'dir').exists())

    def test_strip_all_overlay_xattrs_and_symlink(self):
        (self.upper / 'a').write_text('修改')
        os.setxattr(self.upper / 'a', 'user.overlay.test', b'x')
        (self.upper / 'link').symlink_to('a')
        r = self.apply()
        self.assertEqual(r['overlay_xattr_residual_count'], 0)
        self.assertEqual([x for x in os.listxattr(self.repo / 'a') if x.startswith('user.overlay.')], [])
        self.assertEqual(os.readlink(self.repo / 'link'), 'a')

    def test_any_conflict_prevents_all_writes(self):
        (self.upper / 'a').write_text('草稿 a')
        (self.upper / 'b').write_text('草稿 b')
        (self.repo / 'b').write_text('外部修改')
        r = self.apply()
        self.assertEqual(r['outcome'], 'conflict')
        self.assertEqual(r['conflicts'], {'C1': ['b'], 'C4': ['b']})
        self.assertEqual((self.repo / 'a').read_text(), '原文\n')
        self.assertEqual((self.repo / 'b').read_text(), '外部修改')
        self.assertFalse(self.before.exists())

    def test_target_has_no_conflict_check_or_preimage(self):
        (self.repo / 'target').mkdir()
        (self.repo / 'target/build').write_text('外部构建')
        (self.upper / 'target').mkdir()
        (self.upper / 'target/build').write_text('草稿构建')
        r = self.apply()
        self.assertEqual(r['conflicts'], {'C1': [], 'C4': []})
        self.assertFalse(any(p.startswith('target') for p in r['checked_paths']))
        self.assertFalse((self.before / 'target').exists())
        self.assertEqual((self.repo / 'target/build').read_text(), '草稿构建')


if __name__ == '__main__':
    unittest.main(verbosity=2)
