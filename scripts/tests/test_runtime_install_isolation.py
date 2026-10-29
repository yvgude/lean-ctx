# SPDX-License-Identifier: Apache-2.0
"""A poisoned caller environment must not redirect the runtime acceptance CLI."""
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

from check_runtime_install import isolated_environment


class RuntimeInstallIsolation(unittest.TestCase):
    def test_host_pins_credentials_and_license_are_not_inherited(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            fixture = root / 'explicit-test-license.json'
            fixture.write_text('{}')
            poisoned = {
                'PATH': '/usr/bin:/bin',
                'HOME': '/unrelated-home',
                'LEAN_CTX_CONFIG_DIR': '/unrelated-config',
                'LEAN_CTX_STATE_DIR': '/unrelated-state',
                'LEAN_CTX_CACHE_DIR': '/unrelated-cache',
                'LEAN_CTX_PROJECT_ROOT': '/unrelated-project',
                'LEAN_CTX_CONFIG_PROFILE': 'unrelated-profile',
                'LEAN_CTX_EXTRA_ROOTS': '/unrelated-data',
                'LEANCTX_INTELLIGENCE_LICENSE_CONFIG': '/unrelated-license',
                'OPENAI_API_KEY': 'synthetic-fixture',
                'HTTPS_PROXY': 'http://unrelated.invalid',
            }
            with patch.dict('os.environ', poisoned, clear=True):
                env = isolated_environment(root)
                licensed = isolated_environment(root, fixture)
            for name in ('LEAN_CTX_CONFIG_DIR', 'LEAN_CTX_STATE_DIR', 'LEAN_CTX_CACHE_DIR'):
                self.assertEqual(Path(env[name]), root / 'data')
            self.assertEqual(Path(env['LEAN_CTX_PROJECT_ROOT']), root)
            self.assertEqual(Path(env['HOME']), root / 'home')
            for name in ('OPENAI_API_KEY', 'HTTPS_PROXY', 'LEAN_CTX_EXTRA_ROOTS',
                         'LEAN_CTX_CONFIG_PROFILE', 'LEANCTX_INTELLIGENCE_LICENSE_CONFIG'):
                self.assertNotIn(name, env)
            self.assertEqual(licensed['LEANCTX_INTELLIGENCE_LICENSE_CONFIG'], str(fixture))


if __name__ == '__main__':
    unittest.main()
