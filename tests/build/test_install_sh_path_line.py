# SPDX-License-Identifier: Apache-2.0
"""install.sh puts the install folder on PATH where each shell reads it.

finish() adds one line to the shell's start file when the folder is not on
PATH. These runs check the file it picks and that a fresh shell then finds
the folder: zsh honours ZDOTDIR, the fish line survives a home with spaces,
and on macOS a new ~/.bash_profile keeps ~/.profile read (bash reads only the
first of .bash_profile, .bash_login and .profile).

The installer runs against a hermetic PATH as in test_install_sh_checksum.py:
a fake `curl` serves the release, a fake `uname` pins the platform.
"""

import hashlib
import io
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tarfile
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
TOOLS = (
    "awk", "basename", "cat", "chmod", "cut", "dirname", "grep", "gzip", "head",
    "install", "mkdir", "mktemp", "mv", "rm", "sed", "tar", "tr",
)
BASH = shutil.which("bash")


def sh_quoted(path):
    """The folder as it stands inside "..." in a bash/zsh line: \\ " $ ` escaped."""
    return "".join("\\" + c if c in '\\"$`' else c for c in str(path))


def fish_quoted(path):
    """The folder as it stands inside '...' in a fish line: \\ and ' escaped."""
    return "".join("\\" + c if c in "\\'" else c for c in str(path))


class InstallShPathLineTests(unittest.TestCase):
    HOME_NAME = "home with space"

    def setUp(self):
        tmp = tempfile.TemporaryDirectory(prefix="leanctx-path-line-")
        self.addCleanup(tmp.cleanup)
        self.root = Path(tmp.name)
        self.bin = self.root / "bin"
        self.releases = self.root / "releases"
        self.home = self.root / self.HOME_NAME
        for directory in (self.bin, self.releases, self.home):
            directory.mkdir()
        self.install_dir = self.home / ".local" / "bin"
        for tool in TOOLS:
            found = shutil.which(tool)
            self.assertIsNotNone(found, tool)
            (self.bin / tool).symlink_to(found)
        for tool in ("sha256sum", "shasum"):
            found = shutil.which(tool)
            if found:
                (self.bin / tool).symlink_to(found)
                break
        else:
            self.skipTest("no sha256sum/shasum on this host")
        self._script(
            "uname",
            "import os, sys\n"
            "os_name = os.environ.get('FAKE_OS', 'Linux')\n"
            "print({'-s': os_name, '-m': 'x86_64'}.get(sys.argv[1], os_name))\n",
        )
        self._script(
            "curl",
            "import os, pathlib, sys\n"
            "args = sys.argv[1:]\n"
            "url = [a for a in args if a.startswith('https://')][-1]\n"
            "if 'api.github.com' in url:\n"
            "    print('  \"tag_name\": \"v9.9.9\",')\n"
            "    sys.exit(0)\n"
            "src = pathlib.Path(os.environ['FIXTURE_RELEASES']) / url.rsplit('/', 1)[-1]\n"
            "if not src.is_file():\n"
            "    sys.exit(22)\n"
            "pathlib.Path(args[args.index('-o') + 1]).write_bytes(src.read_bytes())\n",
        )
        payload = b"#!/bin/sh\necho 'lean-ctx 9.9.9'\n"
        buffer = io.BytesIO()
        with tarfile.open(fileobj=buffer, mode="w:gz") as archive:
            info = tarfile.TarInfo("lean-ctx")
            info.size, info.mode = len(payload), 0o755
            archive.addfile(info, io.BytesIO(payload))
        sums = []
        for target in ("x86_64-unknown-linux-musl", "x86_64-apple-darwin"):
            asset = f"lean-ctx-{target}.tar.gz"
            (self.releases / asset).write_bytes(buffer.getvalue())
            sums.append(f"{hashlib.sha256(buffer.getvalue()).hexdigest()}  {asset}\n")
        (self.releases / "SHA256SUMS").write_text("".join(sums), encoding="utf-8")

    def _script(self, name, body):
        path = self.bin / name
        path.write_text(f"#!{sys.executable}\n{body}", encoding="utf-8")
        path.chmod(0o755)

    def _install(self, shell, os_name="Linux", **extra):
        env = {
            "PATH": str(self.bin),
            "HOME": str(self.home),
            "SHELL": f"/bin/{shell}",
            "FAKE_OS": os_name,
            "FIXTURE_RELEASES": str(self.releases),
            "LEAN_CTX_NO_ONBOARD": "1",
            **extra,
        }
        result = subprocess.run(
            ["/bin/sh", str(ROOT / "install.sh"), "--download"], cwd=self.root, env=env,
            capture_output=True, text=True, timeout=60, check=False,
        )
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertTrue((self.install_dir / "lean-ctx").is_file())
        return result

    def _login_bash(self, command):
        """A fresh login bash with this HOME, as Terminal on macOS starts it."""
        if not BASH:
            self.skipTest("no bash on this host")
        return subprocess.run(
            [BASH, "-l", "-c", command], cwd=self.root,
            env={"PATH": "/usr/bin:/bin", "HOME": str(self.home)},
            capture_output=True, text=True, timeout=30, check=False,
        ).stdout.strip()

    def _sourced_path(self, rc):
        """The PATH entries a shell has after reading `rc` (bash and zsh lines are the same)."""
        if not BASH:
            self.skipTest("no bash on this host")
        out = subprocess.run(
            [BASH, "-c", '. "$1"; printf %s "$PATH"', "_", str(rc)], cwd=self.root,
            env={"PATH": "/usr/bin:/bin", "HOME": str(self.home)},
            capture_output=True, text=True, timeout=30, check=False,
        )
        self.assertEqual(out.returncode, 0, out.stderr)
        return out.stdout.split(":")

    def test_zsh_writes_the_line_into_zdotdir(self):
        zdot = self.root / "zdot"
        zdot.mkdir()
        self._install("zsh", ZDOTDIR=str(zdot))
        line = f'export PATH="{sh_quoted(self.install_dir)}:$PATH"'
        rc = zdot / ".zshrc"
        self.assertTrue(rc.is_file(), "the line goes to $ZDOTDIR/.zshrc")
        self.assertIn(line, rc.read_text(encoding="utf-8").splitlines())
        self.assertFalse((self.home / ".zshrc").exists(), "zsh with ZDOTDIR never reads ~/.zshrc")

    def test_zsh_without_zdotdir_writes_home_zshrc(self):
        self._install("zsh")
        self.assertIn(str(self.install_dir), self._sourced_path(self.home / ".zshrc"))

    def test_the_bash_line_gives_a_shell_the_folder_as_it_is(self):
        self._install("bash")
        rc = self.home / ".bashrc"
        self.assertIn(f'export PATH="{sh_quoted(self.install_dir)}:$PATH"',
                      rc.read_text(encoding="utf-8").splitlines())
        self.assertIn(str(self.install_dir), self._sourced_path(rc),
                      "a shell reading the line gets the folder itself")

    def test_a_missing_fish_config_folder_is_created(self):
        self._install("fish")
        config = self.home / ".config" / "fish" / "config.fish"
        self.assertTrue(config.is_file(), "the fish config folder is created when missing")

    def test_the_fish_line_is_quoted_for_a_home_with_spaces(self):
        config = self.home / ".config" / "fish" / "config.fish"
        config.parent.mkdir(parents=True)
        self._install("fish")
        lines = config.read_text(encoding="utf-8").splitlines()
        self.assertIn(f"fish_add_path '{fish_quoted(self.install_dir)}'", lines)
        fish = shutil.which("fish")
        if fish:
            out = subprocess.run(
                [fish, "-l", "-c", "string join : $PATH"],
                env={"PATH": "/usr/bin:/bin", "HOME": str(self.home)},
                capture_output=True, text=True, timeout=30, check=False,
            ).stdout
            self.assertIn(str(self.install_dir), out.split(":"))

    def test_bash_on_macos_keeps_reading_profile_through_a_new_bash_profile(self):
        (self.home / ".profile").write_text('export FROM_PROFILE=yes\n', encoding="utf-8")
        self._install("bash", "Darwin")
        self.assertEqual(self._login_bash('printf %s "$FROM_PROFILE"'), "yes",
                         "the new ~/.bash_profile hides ~/.profile")
        self.assertIn(str(self.install_dir), self._login_bash('printf %s "$PATH"').split(":"))

    def test_bash_on_macos_without_profile_still_reads_bashrc(self):
        self._install("bash", "Darwin")
        self.assertFalse((self.home / ".profile").exists())
        self.assertIn(str(self.install_dir), self._login_bash('printf %s "$PATH"').split(":"))

    def test_bash_on_macos_leaves_an_existing_bash_profile_alone_but_for_bashrc(self):
        (self.home / ".profile").write_text('export FROM_PROFILE=yes\n', encoding="utf-8")
        (self.home / ".bash_profile").write_text('export OWN=1\n', encoding="utf-8")
        self._install("bash", "Darwin")
        text = (self.home / ".bash_profile").read_text(encoding="utf-8")
        self.assertTrue(text.startswith("export OWN=1\n"), text)
        self.assertNotIn(".profile", text.replace(".bash_profile", ""),
                         "an existing ~/.bash_profile is the person's choice")
        self.assertIn(str(self.install_dir), self._login_bash('printf %s "$PATH"').split(":"))


class InstallShPathLineOddHomeTests(InstallShPathLineTests):
    """Every check again with a home holding a space, ' " $ ` and \\."""

    HOME_NAME = "home with space ' \" $HOME ` \\ end"


if __name__ == "__main__":
    unittest.main()
