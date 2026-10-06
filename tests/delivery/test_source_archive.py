# SPDX-License-Identifier: Apache-2.0
"""Execute the real release archive step against isolated Git histories."""
import os
from pathlib import Path
import re
import shutil
import subprocess
import tarfile
import tempfile
import textwrap
import unittest


ROOT = Path(__file__).resolve().parents[2]


class SourceArchiveTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.env = {key: value for key, value in os.environ.items() if not key.startswith('GIT_')}
        self.env.update(GIT_CONFIG_NOSYSTEM='1', GIT_CONFIG_GLOBAL=os.devnull,
                        GIT_AUTHOR_NAME='Fixture', GIT_AUTHOR_EMAIL='fixture@example.invalid',
                        GIT_COMMITTER_NAME='Fixture', GIT_COMMITTER_EMAIL='fixture@example.invalid')
        self.git('init', '-q')
        (self.root / 'scripts').mkdir()
        shutil.copyfile(ROOT / 'scripts/check-no-internal-artifacts.py',
                        self.root / 'scripts/check-no-internal-artifacts.py')
        (self.root / '.github-ignore').write_text('private/\n')
        (self.root / 'LICENSE').write_text('Retained license fixture\n')
        (self.root / 'public.txt').write_text('Public source\n')
        self.commit()
        self.git('tag', '-a', 'v1.2.3', '-m', 'Fixture annotated tag')
        self.env.update(GITHUB_SHA=self.git('rev-parse', 'HEAD'), GITHUB_REF_NAME='v1.2.3')
        workflow = (ROOT / '.github/workflows/release.yml').read_text()
        match = re.search(r'      - name: Create stable source tarball\n        run: \|\n((?:          .*\n)+)', workflow)
        self.assertIsNotNone(match)
        self.script = textwrap.dedent(match[1])
        self.archive = self.root / 'lean-ctx-1.2.3-source.tar.gz'

    def git(self, *args):
        return subprocess.check_output(['git', *args], cwd=self.root, env=self.env,
                                       text=True, stderr=subprocess.PIPE).strip()

    def commit(self):
        self.git('add', '.')
        self.git('-c', 'core.hooksPath=/dev/null', 'commit', '-qm', 'Fixture')

    def run_step(self):
        return subprocess.run(['bash', '-c', self.script], cwd=self.root, env=self.env,
                              capture_output=True, text=True, timeout=20)

    def assert_refused(self):
        result = self.run_step()
        self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertFalse(self.archive.exists())

    def test_public_annotated_tag_archives_exact_source_and_license(self):
        result = self.run_step()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        with tarfile.open(self.archive) as archive:
            self.assertEqual(archive.extractfile('lean-ctx-1.2.3/public.txt').read(), b'Public source\n')
            self.assertEqual(archive.extractfile('lean-ctx-1.2.3/LICENSE').read(), b'Retained license fixture\n')

    def test_committed_private_path_is_refused(self):
        (self.root / 'private').mkdir()
        (self.root / 'private/implementation.rs').write_text('private implementation fixture\n')
        self.commit()
        self.git('tag', '-f', 'v1.2.3')
        self.env['GITHUB_SHA'] = self.git('rev-parse', 'HEAD')
        self.assert_refused()

    def test_tag_drift_is_refused(self):
        (self.root / 'public.txt').write_text('Different source\n')
        self.commit()
        self.env['GITHUB_SHA'] = self.git('rev-parse', 'HEAD')
        self.assert_refused()

    def test_changed_working_policy_is_refused(self):
        (self.root / '.github-ignore').write_text('unrelated/\n')
        self.assert_refused()

    def test_staged_deletion_cannot_hide_private_archive_member(self):
        (self.root / 'private').mkdir()
        (self.root / 'private/implementation.rs').write_text('private implementation fixture\n')
        self.commit()
        self.git('tag', '-f', 'v1.2.3')
        self.env['GITHUB_SHA'] = self.git('rev-parse', 'HEAD')
        self.git('rm', '--cached', 'private/implementation.rs')
        self.assert_refused()


if __name__ == '__main__':
    unittest.main()
