# SPDX-License-Identifier: Apache-2.0
"""Actual hook CLI decisions in isolated homes; no agent-host qualification claim."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import time


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    binary = args.binary.resolve(strict=True)
    rows = []
    marker = 'CUSTOMER-PRIVATE-731902'
    with tempfile.TemporaryDirectory(prefix='leanctx-protected-hooks-') as directory:
        root = Path(directory)
        project = root / 'project'
        project.mkdir()
        policy = project / '.lean-ctx/policy.toml'
        policy.parent.mkdir()
        policy.write_text('name = "fixture"\nversion = "1.0.0"\ndescription = "test"\n')
        config = root / 'config/lean-ctx'
        config.mkdir(parents=True)
        config.joinpath('config.toml').write_text('hook_mode = "hybrid"\n')
        home = root / 'home'
        home.mkdir()
        env = {key: value for key, value in os.environ.items()
               if key in ('PATH', 'TMPDIR', 'SYSTEMROOT', 'WINDIR')}
        env.update(HOME=str(home), XDG_CONFIG_HOME=str(root / 'config'),
                   XDG_DATA_HOME=str(root / 'data'), XDG_STATE_HOME=str(root / 'state'),
                   XDG_CACHE_HOME=str(root / 'cache'), LEAN_CTX_CONFIG_DIR=str(config),
                   LEAN_CTX_DATA_DIR=str(root / 'data/lean-ctx'), DO_NOT_TRACK='1',
                   LEAN_CTX_LIVE_PRICING='off')
        data = Path(env['LEAN_CTX_DATA_DIR'])
        data.mkdir(parents=True)
        # Explicit dead daemon: the legacy deny hook would pass through on outage.
        daemon_data = home / 'Library/Application Support/lean-ctx' if sys.platform == 'darwin' else data
        daemon_data.mkdir(parents=True, exist_ok=True)
        daemon_data.joinpath('daemon.pid').write_text('2147483647')

        def run(name, action, payload=b'', expected='deny', extra=None, cwd=None, hold_stdin=False):
            started = time.monotonic()
            proc = subprocess.Popen([str(binary), 'hook', action], cwd=cwd or project,
                                    env=dict(env, **(extra or {})), stdin=subprocess.PIPE,
                                    stdout=subprocess.PIPE, stderr=subprocess.PIPE)
            if hold_stdin:
                # Leave the pipe open without a payload; the hook must emit a
                # denial and finish independently of an EOF from its caller.
                try:
                    proc.wait(timeout=5)
                except BaseException:
                    proc.kill()
                    proc.communicate()
                    raise
                stdout, stderr = proc.communicate()
            else:
                try:
                    stdout, stderr = proc.communicate(payload, timeout=6)
                except BaseException:
                    proc.kill()
                    proc.communicate()
                    raise
            elapsed = time.monotonic() - started
            assert marker.encode() not in stdout + stderr, (name, stdout, stderr)
            if action == 'rewrite-inline':
                assert proc.returncode == 2 and stdout.strip() == b'false', (name, stdout, stderr)
                verdict = 'deny'
            else:
                expected_exit = 2 if action == 'deny' and expected == 'deny' else 0
                assert proc.returncode == expected_exit, (name, proc.returncode, stdout, stderr)
                if expected == 'legacy-disabled':
                    assert not stdout.strip(), (name, stdout)
                    verdict = expected
                else:
                    value = json.loads(stdout)
                    verdict = value.get('hookSpecificOutput', {}).get('permissionDecision', value.get('decision'))
                    assert verdict == expected, (name, value, stderr)
            rows.append({'case': name, 'action': action, 'decision': verdict,
                         'exit_code': proc.returncode, 'elapsed_ms': round(elapsed * 1000, 3),
                         'stdout': stdout.decode(), 'stderr': stderr.decode()})

        actions = ('rewrite', 'redirect', 'deny', 'copilot', 'codex-pretooluse', 'vibe-pre-tool')
        native = json.dumps({'tool_name': 'Read', 'transcript_path': 'fixture',
                             'tool_input': {'file_path': marker + '.png'}}).encode()
        bypasses = ({}, {'LEAN_CTX_DISABLED': '1'}, {'LEAN_CTX_TOOL_SURFACE': 'shadow'},
                    {'LEAN_CTX_REPLACE_MODE': 'off', 'LEAN_CTX_SHELL_HOOK_MODE': 'rewrite'})
        for action in actions:
            for index, extra in enumerate(bypasses):
                run(f'{action}-native-bypass-{index}', action, native, extra=extra)
            run(f'{action}-malformed', action, b'{')
            run(f'{action}-empty', action)
            run(f'{action}-mcp-route', action,
                b'{"tool_name":"mcp__lean-ctx__ctx_read"}', expected='allow',
                extra={'LEAN_CTX_DISABLED': '1'})
        for tool in ('shell', 'Bash', 'exec_command', 'Read', 'Grep', 'Glob', 'Edit', 'ctx_read',
                     'mcp__foreign__ctx_read'):
            run(f'unsupported-{tool}', 'deny', json.dumps({'tool_name': tool,
                'tool_input': {'command': 'cat ' + marker, 'file_path': str(home / '.claude/projects' / marker / 'memory/MEMORY.md')}}).encode())
        run('structural-mcp', 'deny', b'{"tool_info":{"server":"lean-ctx","mcp_tool_name":"ctx_tree"}}', expected='allow')
        run('conflicting-identities', 'deny', b'{"tool_name":"mcp__lean-ctx__ctx_read","toolName":"Read"}')
        run('duplicate-identity', 'deny', b'{"tool_name":"Read","tool_name":"mcp__lean-ctx__ctx_read"}')
        run('duplicate-server', 'deny', b'{"tool_info":{"server":"foreign","server":"lean-ctx","mcp_tool_name":"ctx_read"}}')
        run('foreign-server', 'deny', b'{"tool_info":{"server":"foreign","mcp_tool_name":"ctx_read"}}')
        run('oversized', 'deny', b' ' * (256 * 1024 + 1))
        run('invalid-unicode', 'deny', b'\xff')
        run('stdin-timeout', 'deny', hold_stdin=True)
        run('inline-command-adapter', 'rewrite-inline')
        child = project / 'src/nested'
        child.mkdir(parents=True)
        run('parent-project-policy', 'deny', native, cwd=child)
        policy.write_text('[invalid policy')
        run('invalid-local-policy', 'deny', native)
        policy.unlink()
        if hasattr(os, 'mkfifo'):
            os.mkfifo(policy)
            run('policy-read-timeout', 'deny', native)
            policy.unlink()
        run('missing-explicit-org-policy', 'deny', native,
            extra={'LEANCTX_ORG_POLICY': str(root / 'missing.signed.json')})
        run('unprotected-community-disabled', 'copilot', native, expected='legacy-disabled',
            extra={'LEAN_CTX_DISABLED': '1'})
        # The protected guard emits no raw arguments into persistent diagnostics.
        for path in root.rglob('*'):
            if path.is_file():
                assert marker.encode() not in path.read_bytes(), str(path)
    report = {'binary_sha256': hashlib.sha256(binary.read_bytes()).hexdigest(),
              'scope': 'real isolated hook CLI processes; installed agent-host behavior remains unqualified',
              'passed': len(rows), 'cases': rows}
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(report, indent=2) + '\n')
    print(json.dumps({'passed': len(rows), 'report': str(args.output),
                      'binary_sha256': report['binary_sha256']}))


if __name__ == '__main__':
    main()
