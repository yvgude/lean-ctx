# SPDX-License-Identifier: Apache-2.0
"""Local git hooks: the pre-commit stamp, and light and full hook modes.

The pre-commit hook checks a staged tree once and trusts only that stamp.

Light and full modes of the local git hooks (owner decision 2026-11-19).

pre-commit: light (fmt + marker guard) on feature branches; full on main, on a
detached HEAD and with LEANCTX_HOOKS=full. The marker guard runs in both
modes and also judges the lines a commit adds to Rust sources.

pre-push: light for feature-branch pushes to the GitLab origin; the full
preflight for pushes that update refs/heads/main or a tag, for the public
GitHub remote (by name, URL host or leanctx.publicRemote) and with
LEANCTX_HOOKS=full. The decision reads the pushed target refs. A main or
tag push must push the checked-out commit from a clean tree, a dirty tree
never earns an "already green" stamp, and SSH aliases resolve through the
real `ssh -G` (pointed at a temporary config by a PATH shim), failing closed.
"""

import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest


class PrecommitStampTests(unittest.TestCase):
    def test_a_checked_tree_is_skipped_and_any_change_is_checked_again(self):
        root = Path(__file__).resolve().parents[2]
        with tempfile.TemporaryDirectory(prefix="leanctx-precommit-") as tmp:
            repo = Path(tmp) / "repo"
            (repo / ".githooks").mkdir(parents=True)
            (repo / "rust/src").mkdir(parents=True)
            shutil.copyfile(root / ".githooks/pre-commit", repo / ".githooks/pre-commit")
            log = Path(tmp) / "cargo.log"
            bin_dir = Path(tmp) / "bin"
            bin_dir.mkdir()
            # A stand-in cargo records each call, so the test sees what ran.
            (bin_dir / "cargo").write_text(
                f'#!/bin/sh\necho "$*" >> "{log}"\n', encoding="utf-8"
            )
            (bin_dir / "cargo").chmod(0o755)
            env = dict(os.environ, PATH=f"{bin_dir}:{os.environ['PATH']}")
            env.pop("PRECOMMIT_FORCE_CARGO", None)
            # The stamp belongs to the full local gates (light mode stops after
            # fmt), so pin them whatever branch `git init` names.
            env["LEANCTX_HOOKS"] = "full"

            def git(*args):
                subprocess.run(
                    ["git", *args], cwd=repo, env=env, check=True, capture_output=True
                )

            def hook():
                result = subprocess.run(
                    ["bash", ".githooks/pre-commit"],
                    cwd=repo,
                    env=env,
                    capture_output=True,
                    text=True,
                    timeout=60,
                    check=False,
                )
                self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
                calls = log.read_text(encoding="utf-8").splitlines() if log.exists() else []
                log.unlink(missing_ok=True)
                return calls

            git("init", "-q")
            lib = repo / "rust/src/lib.rs"
            lib.write_text("pub fn a() {}\n", encoding="utf-8")
            git("add", "rust/src/lib.rs")

            self.assertTrue(hook(), "the first run checks the staged tree")
            self.assertEqual(hook(), [], "the same staged tree is not checked twice")

            lib.write_text("pub fn b() {}\n", encoding="utf-8")
            self.assertTrue(hook(), "unstaged Rust changes are checked, never trusted")
            self.assertTrue(hook(), "no stamp is written for a tree cargo did not see")

            git("add", "rust/src/lib.rs")
            self.assertTrue(hook(), "a new staged tree is checked")
            self.assertEqual(hook(), [])

            env["PRECOMMIT_FORCE_CARGO"] = "1"
            self.assertTrue(hook(), "forcing cargo ignores the stamp")


ROOT = Path(__file__).resolve().parents[2]
ZERO = "0" * 40
# Assembled so this file never spells the marker the guard looks for.
MARKER = "[lean-ctx: " + "omitted 12 lines]"
GITLAB = "git@gitlab.example.invalid:root/lean-ctx.git"
# Pre-push resolves SSH hosts with `ssh -G` (no connection is made).
needs_ssh = unittest.skipUnless(shutil.which("ssh"), "ssh client not installed")


class HookModeTests(unittest.TestCase):
    def setUp(self):
        self._tmp = tempfile.TemporaryDirectory(prefix="leanctx-hookmode-")
        tmp = Path(self._tmp.name)
        self.repo = tmp / "repo"
        (self.repo / ".githooks").mkdir(parents=True)
        (self.repo / "scripts").mkdir()
        (self.repo / "rust/src/core").mkdir(parents=True)
        for hook in ("pre-commit", "pre-push"):
            shutil.copyfile(ROOT / ".githooks" / hook, self.repo / ".githooks" / hook)
        self.cargo_log = tmp / "cargo.log"
        self.preflight_log = tmp / "preflight.log"
        bin_dir = tmp / "bin"
        bin_dir.mkdir()
        # Stand-ins record each call, so a test sees which gates ran.
        cargo = bin_dir / "cargo"
        cargo.write_text(f'#!/bin/sh\necho "$*" >> "{self.cargo_log}"\n', encoding="utf-8")
        cargo.chmod(0o755)
        # The preflight stand-in fails, so no "already green" marker is
        # written and every full-mode push is visible as a blocked push.
        preflight = self.repo / "scripts/preflight.sh"
        preflight.write_text(
            f'#!/bin/sh\necho "$* force=${{PREFLIGHT_FORCE_ALL:-}}" >> "{self.preflight_log}"\nexit 1\n',
            encoding="utf-8")
        preflight.chmod(0o755)
        # ssh reads ~/.ssh/config from the passwd home, not $HOME. A PATH shim
        # runs the real client with -F <temporary config>, so the hook needs
        # no test knob and no real config leaks in.
        real_ssh = shutil.which("ssh")
        if real_ssh:
            shim = bin_dir / "ssh"
            shim.write_text(f'#!/bin/sh\nexec "{real_ssh}" -F "$HOOKTEST_SSH_CONFIG" "$@"\n',
                            encoding="utf-8")
            shim.chmod(0o755)
        self.env = dict(
            os.environ,
            PATH=f"{bin_dir}:{os.environ['PATH']}",
            GIT_AUTHOR_NAME="t", GIT_AUTHOR_EMAIL="t@example.invalid",
            GIT_COMMITTER_NAME="t", GIT_COMMITTER_EMAIL="t@example.invalid",
            GIT_CONFIG_NOSYSTEM="1", HOME=str(tmp),
            HOOKTEST_SSH_CONFIG=str(tmp / "ssh_config"),
        )
        (tmp / "ssh_config").write_text(
            "Host gh-work gh.work\n    HostName github.com\n"
            "Host gitlab-alias\n    HostName gitlab.example.invalid\n",
            encoding="utf-8")
        self.tmp = tmp
        for key in ("LEANCTX_HOOKS", "PRECOMMIT_FORCE_CARGO", "SKIP_PREFLIGHT", "SKIP_DERIVATIONS"):
            self.env.pop(key, None)
        self.git("init", "-q", "-b", "feature/x")
        self.write("rust/src/lib.rs", "pub fn a() {}\n")
        self.git("add", "-A")
        self.git("-c", "core.hooksPath=/dev/null", "commit", "-q", "-m", "base")

    def tearDown(self):
        self._tmp.cleanup()

    def git(self, *args):
        return subprocess.run(["git", *args], cwd=self.repo, env=self.env, check=True,
                              capture_output=True, text=True).stdout.strip()

    def write(self, rel, text):
        path = self.repo / rel
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text, encoding="utf-8")

    def run_hook(self, *args, stdin="", env=None):
        result = subprocess.run(["bash", *args], cwd=self.repo, env=env or self.env, input=stdin,
                                capture_output=True, text=True, timeout=60, check=False)
        return result

    def precommit(self, env=None):
        result = self.run_hook(".githooks/pre-commit", env=env)
        calls = self.cargo_log.read_text(encoding="utf-8").splitlines() if self.cargo_log.exists() else []
        self.cargo_log.unlink(missing_ok=True)
        return result, calls

    def stage_rust_change(self, body="pub fn b() {}\n"):
        self.write("rust/src/lib.rs", body)
        self.git("add", "rust/src/lib.rs")

    def assert_light(self, result, calls):
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("light mode", result.stdout)
        self.assertEqual(calls, ["fmt --check"])

    def assert_full(self, result, calls):
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertNotIn("light mode", result.stdout)
        self.assertTrue(any(call.startswith("clippy") for call in calls), calls)

    # -- pre-commit ------------------------------------------------------

    def test_precommit_is_light_on_a_feature_branch(self):
        self.stage_rust_change()
        self.assert_light(*self.precommit())

    def test_precommit_is_full_on_main_on_a_detached_head_and_on_request(self):
        self.stage_rust_change()
        # A green full run stamps the staged tree; force cargo so each mode
        # is judged on its own.
        self.env["PRECOMMIT_FORCE_CARGO"] = "1"
        self.assert_full(*self.precommit(env=dict(self.env, LEANCTX_HOOKS="full")))
        self.git("checkout", "-q", "-b", "main")
        self.assert_full(*self.precommit())
        self.git("checkout", "-q", "--detach")
        self.assert_full(*self.precommit())
        self.git("checkout", "-q", "-b", "feature/y")
        self.assert_light(*self.precommit())

    def test_marker_guard_blocks_rust_in_light_mode(self):
        self.stage_rust_change(f"// {MARKER}\npub fn b() {{}}\n")
        result, calls = self.precommit()
        self.assertEqual(result.returncode, 1, result.stdout)
        self.assertIn("rust/src/lib.rs", result.stdout)
        self.assertEqual(calls, [], "the guard stops the commit before any cargo step")
        for section in ("§ function", "§ impl"):
            self.stage_rust_change(f'const S: &str = "{section} x";\n')
            result, _ = self.precommit()
            self.assertEqual(result.returncode, 1, result.stdout)

    def test_marker_guard_judges_only_added_rust_lines_and_spares_detectors(self):
        # A marker literal already in the history does not block an unrelated edit.
        self.write("rust/src/old.rs", f'const M: &str = "{MARKER}";\n')
        self.git("add", "-A")
        self.git("-c", "core.hooksPath=/dev/null", "commit", "-q", "-m", "literal")
        self.write("rust/src/old.rs", f'const M: &str = "{MARKER}";\nconst N: u8 = 1;\n')
        self.git("add", "-A")
        self.assert_light(*self.precommit())
        # A listed detector file may add marker literals.
        self.write("rust/src/core/entropy.rs", f'const M: &str = "{MARKER}";\n')
        self.git("add", "-A")
        self.assert_light(*self.precommit())

    def test_marker_guard_still_blocks_other_files(self):
        self.write("docs/a.md", f"text\n{MARKER}\n")
        self.git("add", "-A")
        result, _ = self.precommit()
        self.assertEqual(result.returncode, 1, result.stdout)

    # -- pre-push --------------------------------------------------------

    def push_line(self, remote_ref, sha=None, local_ref="refs/heads/feature/x", remote_sha=ZERO):
        sha = sha or self.git("rev-parse", "HEAD")
        return f"{local_ref} {sha} {remote_ref} {remote_sha}\n"

    def prepush_raw(self, remote, url, stdin, env=None):
        result = self.run_hook(".githooks/pre-push", remote, url, stdin=stdin, env=env)
        calls = self.preflight_log.read_text(encoding="utf-8").splitlines() if self.preflight_log.exists() else []
        self.preflight_log.unlink(missing_ok=True)
        return result, calls

    def prepush(self, remote, url, remote_ref=None, env=None, stdin=None):
        stdin = stdin if stdin is not None else self.push_line(remote_ref)
        result, calls = self.prepush_raw(remote, url, stdin, env=env)
        if calls:
            self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
            self.assertIn("pre-push BLOCKED: preflight failed", result.stdout)
        else:
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            self.assertIn("light mode", result.stdout)
        self.last_calls = calls
        return bool(calls)

    @needs_ssh
    def test_prepush_is_light_for_feature_branches_to_the_origin(self):
        self.assertFalse(self.prepush("origin", GITLAB, "refs/heads/feature/x"))
        # The checked-out branch does not matter, only the pushed target.
        self.git("checkout", "-q", "-b", "main")
        self.assertFalse(self.prepush("origin", GITLAB, "refs/heads/feature/x"))

    @needs_ssh
    def test_prepush_is_full_for_main_tags_and_on_request(self):
        gitlab = "ssh://git@gitlab.example.invalid:2222/root/lean-ctx.git"
        self.assertTrue(self.prepush("origin", gitlab, "refs/heads/main"))
        # Main and tags run every step, whatever the diff against origin/main.
        self.assertEqual(self.last_calls, ["fast force=1"])
        self.assertTrue(self.prepush("origin", gitlab, "refs/tags/v9.9.9"))
        self.assertEqual(self.last_calls, ["fast force=1"])
        self.assertTrue(self.prepush("origin", gitlab, "refs/heads/feature/x",
                                     env=dict(self.env, LEANCTX_HOOKS="full")))
        self.assertEqual(self.last_calls, ["fast force=0"])

    @needs_ssh
    def test_prepush_reads_every_pushed_ref(self):
        # Any one main or tag line makes the push full, wherever it stands.
        branch = self.push_line("refs/heads/feature/x")
        self.assertTrue(self.prepush("origin", GITLAB, stdin=branch + self.push_line("refs/heads/main")))
        self.assertTrue(self.prepush("origin", GITLAB, stdin=branch + self.push_line(
            "refs/tags/v9.9.9", local_ref="refs/tags/v9.9.9")))
        # Deleting main or a tag pushes no commit: light.
        head = self.git("rev-parse", "HEAD")
        delete = f"(delete) {ZERO} refs/heads/main {head}\n"
        self.assertFalse(self.prepush("origin", GITLAB, stdin=delete))
        self.assertFalse(self.prepush("origin", GITLAB, stdin=branch + delete))

    @needs_ssh
    def test_prepush_blocks_a_main_or_tag_push_of_another_commit(self):
        old = self.git("rev-parse", "HEAD")
        self.write("rust/src/lib.rs", "pub fn c() {}\n")
        self.git("-c", "core.hooksPath=/dev/null", "commit", "-q", "-am", "next")
        for line in (self.push_line("refs/heads/main", sha=old),
                     self.push_line("refs/tags/v1.0.0", sha=old, local_ref="refs/tags/v1.0.0")):
            result, calls = self.prepush_raw("origin", GITLAB, line)
            self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
            self.assertIn("pre-push BLOCKED", result.stdout)
            self.assertIn(f"git switch --detach {old[:12]}", result.stdout)
            self.assertEqual(calls, [], "the preflight would check the wrong tree")
        # An annotated tag is peeled to its commit: at HEAD it runs, elsewhere it blocks.
        self.git("tag", "-a", "-m", "t", "v2.0.0")
        tag_object = self.git("rev-parse", "v2.0.0")
        self.assertTrue(self.prepush("origin", GITLAB, stdin=self.push_line(
            "refs/tags/v2.0.0", sha=tag_object, local_ref="refs/tags/v2.0.0")))
        self.git("tag", "-a", "-m", "t", "v0.9.0", old)
        result, calls = self.prepush_raw("origin", GITLAB, self.push_line(
            "refs/tags/v0.9.0", sha=self.git("rev-parse", "v0.9.0"), local_ref="refs/tags/v0.9.0"))
        self.assertEqual((result.returncode, calls), (1, []), result.stdout)

    @needs_ssh
    def test_prepush_blocks_a_main_or_tag_push_from_a_dirty_tree(self):
        self.write("rust/src/lib.rs", "pub fn dirty() {}\n")
        for line in (self.push_line("refs/heads/main"),
                     self.push_line("refs/tags/v1.0.0", local_ref="refs/tags/v1.0.0")):
            result, calls = self.prepush_raw("origin", GITLAB, line)
            self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
            self.assertIn("pre-push BLOCKED: tracked files have uncommitted changes", result.stdout)
            self.assertEqual(calls, [], "the preflight would check the dirty tree")
        # Staged-only changes count as dirty too.
        self.git("add", "rust/src/lib.rs")
        result, calls = self.prepush_raw("origin", GITLAB, self.push_line("refs/heads/main"))
        self.assertEqual((result.returncode, calls), (1, []), result.stdout)
        # Untracked files do not block.
        self.git("reset", "-q", "--hard")
        self.write("scratch.txt", "x\n")
        self.assertTrue(self.prepush("origin", GITLAB, "refs/heads/main"))

    @needs_ssh
    def test_prepush_stamps_only_a_clean_tree(self):
        preflight = self.repo / "scripts/preflight.sh"
        preflight.write_text(f'#!/bin/sh\necho "$*" >> "{self.preflight_log}"\nexit 0\n',
                             encoding="utf-8")
        # The stand-ins are tracked, so commit the passing one to start clean.
        self.git("-c", "core.hooksPath=/dev/null", "commit", "-q", "-am", "passing preflight")
        marker = Path(self.git("rev-parse", "--absolute-git-dir")) / "preflight-passed"
        env = dict(self.env, LEANCTX_HOOKS="full")
        head = self.git("rev-parse", "HEAD")
        self.write("rust/src/lib.rs", "pub fn dirty() {}\n")
        result, calls = self.prepush_raw("origin", GITLAB, self.push_line("refs/heads/feature/x"), env=env)
        self.assertEqual((result.returncode, calls), (0, ["fast"]), result.stdout + result.stderr)
        self.assertFalse(marker.exists(), "a dirty tree must not stamp HEAD green")
        self.git("checkout", "-q", "--", "rust/src/lib.rs")
        result, calls = self.prepush_raw("origin", GITLAB, self.push_line("refs/heads/feature/x"), env=env)
        self.assertEqual((result.returncode, calls), (0, ["fast"]), result.stdout + result.stderr)
        self.assertEqual(marker.read_text(encoding="utf-8").strip(), head)
        # The stamp now skips the next identical push.
        result, calls = self.prepush_raw("origin", GITLAB, self.push_line("refs/heads/feature/x"), env=env)
        self.assertEqual((result.returncode, calls), (0, []), result.stdout)
        self.assertIn("already green", result.stdout)

    def test_prepush_has_no_ssh_config_knob(self):
        hook = (ROOT / ".githooks/pre-push").read_text(encoding="utf-8")
        self.assertNotIn("LEANCTX_SSH_CONFIG", hook)
        self.assertNotIn("ssh -F", hook)

    @needs_ssh
    def test_prepush_is_full_for_the_public_remote(self):
        self.assertTrue(self.prepush("github", "/nowhere", "refs/heads/feature/x"))
        self.assertEqual(self.last_calls, ["fast force=0"])
        self.assertTrue(self.prepush("public", "git@github.com:org/repo.git", "refs/heads/feature/x"))
        self.assertTrue(self.prepush("public", "https://GitHub.com/org/repo", "refs/heads/feature/x"))
        # SSH aliases for github.com, with and without a dot, resolve via ssh -G.
        self.assertTrue(self.prepush("pub", "gh-work:org/repo.git", "refs/heads/feature/x"))
        self.assertTrue(self.prepush("pub", "git@gh.work:org/repo.git", "refs/heads/feature/x"))
        self.assertTrue(self.prepush("pub", "ssh://git@gh.work/org/repo.git", "refs/heads/feature/x"))
        self.assertFalse(self.prepush("origin", "gitlab-alias:root/lean-ctx.git", "refs/heads/feature/x"))
        # An alias the config does not map stays light unless configured.
        self.assertFalse(self.prepush("pub", "gh-alias-unknown:org/repo.git", "refs/heads/feature/x"))
        self.git("config", "leanctx.publicRemote", "pub")
        self.assertTrue(self.prepush("pub", "gh-alias-unknown:org/repo.git", "refs/heads/feature/x"))

    @needs_ssh
    def test_prepush_fails_closed_when_ssh_cannot_resolve_the_host(self):
        broken = self.tmp / "ssh_config_broken"
        broken.write_text("NotAnSshOption yes\n", encoding="utf-8")
        env = dict(self.env, HOOKTEST_SSH_CONFIG=str(broken))
        self.assertTrue(self.prepush("origin", GITLAB, "refs/heads/feature/x", env=env))
        # A host that looks like an option is passed after `--`, never as one.
        self.assertTrue(self.prepush("origin", "-oProxyCommand=x:org/repo.git", "refs/heads/feature/x"))
        # https URLs involve no ssh alias.
        self.assertFalse(self.prepush("origin", "https://gitlab.example.invalid/root/lean-ctx.git",
                                      "refs/heads/feature/x", env=env))


if __name__ == "__main__":
    unittest.main()
