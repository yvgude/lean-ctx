# SPDX-License-Identifier: Apache-2.0
"""Actual local SDK transport: project policy, refreshed reads and shell admission."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import queue
import subprocess
import tempfile
import threading
import time

BASE = 'name = "sdk-policy"\nversion = "1.0.0"\ndescription = "fixture"\n'
MASK = BASE + "[redaction]\ncustomer = 'CUS-[0-9]{4}'\n"


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    options = parser.parse_args()
    binary = options.binary.resolve(strict=True)
    report = {'binary_sha256': hashlib.sha256(binary.read_bytes()).hexdigest(),
              'scope': 'actual isolated Engine tool-session; synthetic data; no provider/model calls',
              'cases': [], 'passed': False}
    options.output.parent.mkdir(parents=True, exist_ok=True)
    try:
        with tempfile.TemporaryDirectory(prefix='leanctx-agent-policy-') as temporary:
            root = Path(temporary).resolve()
            project, outside = root / 'project', root / 'outside'
            project.mkdir()
            outside.mkdir()
            (project / '.git').mkdir()
            sample = project / 'sample.txt'
            sample.write_text('account CUS-1234\nreference ACC-5678\n')
            manifest = project / 'Cargo.toml'
            manifest_content = '# CONFIDENTIAL\n[package]\nname = "ctx-probe"\nversion = "0.0.0"\n[dependencies]\nacquisition_canary = "1"\n'
            manifest.write_text(manifest_content)
            src = project / 'src'
            src.mkdir()
            symbol_file = src / 'lib.rs'
            symbol_file.write_text('// CONFIDENTIAL\npub fn LoginProbe() -> &\'static str { "original_acquisition_canary CUS-1234" }\n')
            for index in range(6):
                (src / f'blocked_symbol_{index}.rs').write_text(
                    '// CONFIDENTIAL\npub fn MultiProbe() {}\n')
            (src / 'allowed.rs').write_text('pub fn MultiProbe() { /* allowed_body */ }\n')
            policy = project / '.lean-ctx/policy.toml'
            policy.parent.mkdir()
            policy.write_text(MASK)
            session_policy = root / 'session-policy.json'
            session_policy.write_text(json.dumps({'schema_version': 1, 'allow_write': False,
                'allow_exec': True, 'allowed_executables': ['printf', 'python3', 'touch'],
                'allowed_env': [], 'max_timeout_ms': 5000}))
            session_policy.chmod(0o600)
            env = {key: value for key, value in os.environ.items()
                   if key in ('PATH', 'TMPDIR', 'SystemRoot', 'WINDIR')}
            env.update(DO_NOT_TRACK='1', LEAN_CTX_HOOK_CHILD='1', LEAN_CTX_ACTIVE='1',
                LEAN_CTX_HEADLESS='1', LEAN_CTX_CONVERSATION_SCOPE='0',
                LEAN_CTX_EMBEDDINGS_AUTO_DOWNLOAD='0', LEAN_CTX_LIVE_PRICING='off',
                LEAN_CTX_PROJECT_ROOT=str(outside), LEAN_CTX_TELEMETRY='0')
            for category in ('CONFIG', 'DATA', 'STATE', 'CACHE'):
                path = root / category.lower()
                path.mkdir()
                env['LEAN_CTX_' + category + '_DIR'] = str(path)
                env['XDG_' + category + '_HOME'] = str(path)
            (root / 'config/config.toml').write_text('minimal_overhead = true\n')
            messages = queue.Queue()
            with (root / 'stderr.log').open('w+') as stderr:
                process = subprocess.Popen([str(binary), 'engine', 'tool-session',
                    '--project-root', str(project), '--policy-file', str(session_policy)],
                    cwd=outside, env=env, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                    stderr=stderr, text=True, bufsize=1)

                def receive():
                    for line in process.stdout:
                        try:
                            messages.put(json.loads(line))
                        except json.JSONDecodeError:
                            messages.put({'transport_error': line})
                    messages.put({'transport_eof': True})

                reader = threading.Thread(target=receive, daemon=True)
                reader.start()
                sequence = 0

                def request(op, **arguments):
                    nonlocal sequence
                    sequence += 1
                    request_id = str(sequence)
                    process.stdin.write(json.dumps({'id': request_id, 'op': op, **arguments}) + '\n')
                    process.stdin.flush()
                    response = messages.get(timeout=25)
                    if response.get('id') != request_id:
                        raise RuntimeError(('invalid response', response))
                    return response

                def case(name, tool='ctx_read', arguments=None, blocked=False,
                         absent=('CUS-1234',), present=(), side_effect=None,
                         error_code='permission_denied', created_side_effect=None,
                         wait_for_graph=False):
                    started = time.monotonic()
                    response = request('call', tool=tool, arguments=arguments or {
                        'path': str(sample), 'mode': 'raw', 'fresh': True})
                    pending = []
                    if wait_for_graph:
                        for _attempt in range(20):
                            if 'graph index is building in the background' not in response.get('result', {}).get('text', ''):
                                break
                            pending.append(response)
                            time.sleep(0.5)
                            response = request('call', tool=tool, arguments=arguments)
                    raw = json.dumps(response)
                    conditions = [response.get('ok') is (not blocked),
                                  all(value not in raw for value in absent),
                                  all(value in raw for value in present),
                                  process.poll() is None]
                    if pending:
                        conditions.append(all(value not in json.dumps(pending) for value in absent))
                    if blocked:
                        conditions.append(response.get('error', {}).get('code') == error_code)
                    elif response.get('ok'):
                        result = response.get('result', {})
                        conditions.append(result.get('original_tokens') ==
                                          result.get('output_tokens', -1) + result.get('saved_tokens', -1))
                    if side_effect is not None:
                        conditions.append(not side_effect.exists())
                    if created_side_effect is not None:
                        conditions.append(created_side_effect.is_file())
                    report['cases'].append({'case': name, 'passed': all(conditions),
                        'conditions': conditions, 'response': response, 'graph_pending': pending,
                        'elapsed_ms': round((time.monotonic() - started) * 1000, 3)})

                try:
                    hello = request('hello', schema_version=1, transport_version=1,
                        agent_tools_interface_version='1.0.0', sdk_version='1.1.0')
                    if not hello.get('ok'):
                        raise RuntimeError(('hello failed', hello))
                    report['hello'] = hello
                    case('explicit-project-mask-before-sdk-output', present=('REDACTED',))
                    case('shell-output-masked', 'ctx_shell', {'argv': ['printf', 'CUS-1234'],
                        'cwd': '.', 'env': {}, 'timeout_ms': 1000}, present=('REDACTED',))
                    case('shell-binary-output-withheld-after-execution', 'ctx_shell',
                        {'argv': ['printf', '\\377'], 'cwd': '.', 'env': {}, 'timeout_ms': 1000},
                        blocked=True, error_code='tool_error', present=('Do not retry automatically',))
                    ran_marker = project / 'executed-before-output-check'
                    case('executed-action-is-not-reported-as-admission-denial', 'ctx_shell',
                        {'argv': ['python3', '-c',
                            'import os, pathlib, sys; pathlib.Path(sys.argv[1]).touch(); os.write(1, bytes([255]))',
                            str(ran_marker)], 'cwd': '.', 'env': {}, 'timeout_ms': 5000},
                        blocked=True, error_code='tool_error', present=('Do not retry automatically',),
                        created_side_effect=ran_marker)
                    invalid_text = project / 'invalid.txt'
                    invalid_text.write_bytes(b'customer CUS-\xff1234\n')
                    case('lossy-file-output-withheld', arguments={
                        'path': str(invalid_text), 'mode': 'raw', 'fresh': True},
                        blocked=True, error_code='tool_error')
                    policy.write_text(BASE + "[redaction]\ncustomer = 'CUS-[0-9]{4}|ACC-[0-9]{4}'\n")
                    case('same-process-rule-refresh', absent=('CUS-1234', 'ACC-5678'), present=('REDACTED',))
                    sample.write_text('CONFIDENTIAL\nCUS-1234\n')
                    case('populate-prior-policy-view', arguments={
                        'path': str(sample), 'mode': 'full', 'fresh': False}, present=('REDACTED',))
                    policy.write_text(BASE + "[redaction]\nlabel = 'CONFIDENTIAL'\n[filters]\nclassification = 'block'\n")
                    case('original-classification-block', blocked=True, error_code='tool_error')
                    case('cached-view-after-classification-change', arguments={
                        'path': str(sample), 'mode': 'full', 'fresh': False},
                        blocked=True, error_code='tool_error')
                    sample.write_text('CONFIDENTIAL\nordinary excerpt\n')
                    case('classification-outside-line-window', arguments={
                        'path': str(sample), 'mode': 'lines:2-2', 'fresh': True},
                        blocked=True, error_code='tool_error')
                    marker = project / 'must-not-exist'
                    policy.write_text(BASE + "[context]\ndeny_tools = ['ctx_read', 'ctx_shell']\n")
                    case('project-read-denied', blocked=True)
                    case('shell-denied-before-side-effect', 'ctx_shell',
                        {'argv': ['touch', str(marker)], 'cwd': '.', 'env': {}, 'timeout_ms': 1000},
                        blocked=True, side_effect=marker)
                    policy.write_text('not valid policy = [')
                    case('invalid-policy-fails-closed', blocked=True)
                    policy.write_text(BASE + "[egress]\nforbidden_patterns = ['must-not-exist']\n")
                    egress_marker = project / 'egress-must-not-exist'
                    case('egress-denied-before-side-effect', 'ctx_shell',
                        {'argv': ['touch', str(egress_marker)], 'cwd': '.', 'env': {}, 'timeout_ms': 1000},
                        blocked=True, side_effect=egress_marker)
                    policy.write_text(MASK)
                    sample.write_text('account CUS-1234\n')
                    case('populate-before-policy-removal', arguments={
                        'path': str(sample), 'mode': 'full', 'fresh': False}, present=('REDACTED',))
                    policy.unlink()
                    case('unchanged-file-after-policy-removal', arguments={
                        'path': str(sample), 'mode': 'full', 'fresh': False},
                        absent=('REDACTED',), present=('CUS-1234',))
                    sample.write_text('ordinary community content\n')
                    case('community-read-without-policy', present=('ordinary community content',))
                    case('community-search-without-policy', 'ctx_search',
                        {'pattern': 'ordinary', 'path': str(project), 'include': 'sample.txt'},
                        present=('ordinary community content',))
                    policy.write_text(MASK)
                    sample.write_text('account CUS-1234\n')
                    case('search-masks-original-before-matching', 'ctx_search',
                        {'pattern': 'CUS-1234', 'path': str(project), 'include': 'sample.txt'},
                        absent=('CUS-1234', 'account'), present=('0 matches',))
                    case('search-displays-admitted-mask', 'ctx_search',
                        {'pattern': 'account', 'path': str(project), 'include': 'sample.txt'},
                        present=('REDACTED', 'account'))
                    sample.write_text('CONFIDENTIAL\nordinary search excerpt\n')
                    case('search-warm-permissive-policy', 'ctx_search',
                        {'pattern': 'ordinary', 'path': str(project), 'include': 'sample.txt'},
                        present=('ordinary search excerpt',))
                    policy.write_text(BASE + "[redaction]\nlabel = 'CONFIDENTIAL'\n[filters]\nclassification = 'block'\n")
                    case('search-classification-outside-match-after-policy-change', 'ctx_search',
                        {'pattern': 'ordinary', 'path': str(project), 'include': 'sample.txt'},
                        absent=('ordinary search excerpt', 'unchanged'), present=('withheld',))
                    case('search-lossy-source-withheld', 'ctx_search',
                        {'pattern': 'customer', 'path': str(project), 'include': 'invalid.txt'},
                        absent=('CUS-1234', 'invalid.txt:'), present=('withheld',))
                    policy.unlink()
                    case('search-unchanged-file-after-policy-removal', 'ctx_search',
                        {'pattern': 'ordinary', 'path': str(project), 'include': 'sample.txt'},
                        present=('ordinary search excerpt',))
                    hidden = project / '.searchable.txt'
                    hidden.write_text('ordinary hidden before\n')
                    case('search-hidden-community-before-policy', 'ctx_search',
                        {'pattern': 'ordinary', 'path': str(project), 'include': '.searchable.txt'},
                        present=('ordinary hidden before',))
                    policy.write_text(MASK)
                    hidden.write_text('ordinary hidden after edit CUS-1234\n')
                    case('search-hidden-edit-under-policy', 'ctx_search',
                        {'pattern': 'ordinary', 'path': str(project), 'include': '.searchable.txt'},
                        absent=('CUS-1234',), present=('ordinary hidden after edit', 'REDACTED'))
                    policy.unlink()
                    case('search-hidden-edit-after-policy-removal', 'ctx_search',
                        {'pattern': 'ordinary', 'path': str(project), 'include': '.searchable.txt'},
                        absent=('ordinary hidden before', 'REDACTED'),
                        present=('ordinary hidden after edit CUS-1234',))
                    policy.write_text(BASE + "[redaction]\nlabel = 'CONFIDENTIAL'\ncustomer = 'CUS-[0-9]{4}'\n")
                    warm_args = {'name': 'LoginProbe'}
                    warm = None
                    for _attempt in range(20):
                        warm = request('call', tool='ctx_symbol', arguments=warm_args)
                        if 'original_acquisition_canary' in json.dumps(warm):
                            break
                        time.sleep(0.5)
                    report['symbol_warmup'] = warm
                    ready = 'original_acquisition_canary' in json.dumps(warm)
                    report['cases'].append({'case': 'symbol-source-ready', 'passed': ready})
                    case('symbol-admitted-mask', 'ctx_symbol', warm_args,
                         present=('original_acquisition_canary', 'REDACTED'))
                    compose_args = {'task': 'Investigate LoginProbe implementation and dependencies',
                                    'path': str(project)}
                    case('compose-before-rule-change', 'ctx_compose', compose_args,
                         present=('acquisition_canary',))
                    policy.write_text(BASE + "[redaction]\nlabel = 'CONFIDENTIAL'\n[filters]\nclassification = 'block'\n")
                    case('symbol-original-classification-outside-snippet', 'ctx_symbol', warm_args,
                         absent=('original_acquisition_canary', 'src/lib.rs', 'LoginProbe@'))
                    case('symbol-disambiguation-authorizes-every-source', 'ctx_symbol',
                         {'name': 'MultiProbe'}, absent=('blocked_symbol_',), present=('allowed_body',))
                    case('symbol-handle-original-classification', 'ctx_search',
                         {'action': 'symbol', 'handle': 'src/lib.rs#LoginProbe@L2'},
                         absent=('original_acquisition_canary', 'src/lib.rs#'))
                    case('compose-original-classification-after-cache-warmup', 'ctx_compose', compose_args,
                         absent=('acquisition_canary', 'original_acquisition_canary', 'blocked_symbol_'))
                    policy.write_text(MASK)
                    symbol_file.write_text('pub fn RenamedProbe() { /* current_body */ }\n')
                    case('symbol-does-not-reveal-stale-index-name', 'ctx_symbol', warm_args,
                         absent=('original_acquisition_canary', 'LoginProbe@', 'src/lib.rs'))
                    symbol_file.write_bytes(b'pub fn LoginProbe() { /* invalid_\xff_content */ }\n')
                    case('symbol-lossy-original-withheld', 'ctx_symbol', warm_args,
                         absent=('invalid_', 'src/lib.rs'))
                    symbol_file.write_text('pub fn LoginProbe() { /* oversized_source */ }\n'
                                           + ' ' * (8 * 1024 * 1024))
                    case('symbol-original-exceeds-admission-budget', 'ctx_symbol', warm_args,
                         absent=('oversized_source', 'src/lib.rs'))
                    symbol_file.unlink()
                    outside_symbol = outside / 'escape.rs'
                    outside_symbol.write_text('pub fn LoginProbe() { /* escaped_source */ }\n')
                    symbol_file.symlink_to(outside_symbol)
                    case('symbol-replaced-by-symlink-withheld', 'ctx_symbol', warm_args,
                         absent=('escaped_source', 'src/lib.rs'))
                    outside_manifest = outside / 'Cargo.toml'
                    outside_manifest.write_text('[dependencies]\nescaped_dependency = "1"\n')
                    manifest.unlink()
                    manifest.symlink_to(outside_manifest)
                    case('compose-manifest-symlink-withheld', 'ctx_compose', compose_args,
                          absent=('escaped_dependency', 'escaped_source'))
                    # Restore the project's package identity as well as its source;
                    # graph storage is namespaced by that identity, not only the path.
                    manifest.unlink()
                    manifest.write_text(manifest_content)
                    symbol_file.unlink()
                    symbol_file.write_text('pub fn LoginProbe() { /* community_current_body */ }\n')
                    policy.unlink()
                    case('community-symbol-current-source-after-policy-removal', 'ctx_symbol', warm_args,
                         present=('community_current_body',), wait_for_graph=True)
                    report['close'] = request('close')
                    process.stdin.close()
                    report['exit_code'] = process.wait(timeout=15)
                    report['passed'] = (all(row['passed'] for row in report['cases'])
                                        and report['close'].get('ok') is True
                                        and report['exit_code'] == 0)
                finally:
                    if process.poll() is None:
                        process.kill()
                        process.wait(timeout=15)
                    reader.join(timeout=5)
                    stderr.seek(0)
                    report['stderr'] = stderr.read()
    except Exception as error:
        report['driver_error'] = repr(error)
    finally:
        options.output.write_text(json.dumps(report, indent=2) + '\n')
    print(json.dumps({'passed': report['passed'], 'cases': len(report['cases']),
        'failed': [row['case'] for row in report['cases'] if not row['passed']],
        'output': str(options.output), 'driver_error': report.get('driver_error')}))
    raise SystemExit(0 if report['passed'] else 1)


if __name__ == '__main__':
    main()
