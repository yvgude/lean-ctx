#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""Actual MCP compose utility and source-admission checks; no model calls."""

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

from check_semantic_source_policy import response_text, has_error


BASE = 'name = "compose-fixture"\nversion = "1.0.0"\ndescription = "test"\n'
RULES = BASE + "[filters]\nclassification = 'block'\n[redaction]\ncustomer = 'CUS-[0-9]{4}'\n"
TASK = 'investigate authentication retry'
BASELINE = '92931864bf619ee0cd361b2521ff6f7cf0a29ed21c5533412f3f159c041ccab8'


def ranked_section(text):
    marker = '\n## Ranked files ('
    if marker not in text:
        return ''
    return text.split(marker, 1)[1].split('\n## ', 1)[0]


def source(name, marker, label='PUBLIC______'):
    return (f'// {label}\n' + '// fixture padding\n' * 20
            + f'fn {name}() {{\n    // authentication retry\n'
            + f'    let answer = "{marker} CUS-1234 ACC-5678";\n}}\n')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--mode', choices=['candidate', 'baseline'], default='candidate')
    args = parser.parse_args()
    binary = args.binary.resolve()
    report = {'binary_sha256': hashlib.sha256(binary.read_bytes()).hexdigest(),
              'mode': args.mode, 'cases': [], 'scope': 'isolated actual MCP; synthetic sources; no model or provider calls'}
    try:
        with tempfile.TemporaryDirectory(prefix='leanctx-compose-admission-') as temporary:
            root = Path(temporary)
            project = root / 'project'
            project.mkdir()
            (project / '.git').mkdir()
            (project / 'Cargo.toml').write_text('[package]\nname="fixture"\nversion="0.1.0"\n')
            mutable = project / 'mutable.rs'
            mutable.write_text(source('alpha', 'MUTABLE_BEFORE'))
            (project / 'allowed.rs').write_text(source('bravo', 'PERMITTED_CONTEXT'))
            (project / 'private.rs').write_text(source('charlie', 'CLASSIFIED_CONTEXT', 'CONFIDENTIAL'))
            policy = project / '.lean-ctx/policy.toml'
            policy.parent.mkdir()
            env = {key: value for key, value in os.environ.items()
                   if key in ('PATH', 'TMPDIR', 'SystemRoot', 'WINDIR')}
            (root / 'home').mkdir()
            env.update(HOME=str(root / 'home'), USERPROFILE=str(root / 'home'),
                       APPDATA=str(root / 'config'), LOCALAPPDATA=str(root / 'data'),
                       DO_NOT_TRACK='1', LEAN_CTX_HOOK_CHILD='1', LEAN_CTX_ACTIVE='1',
                       LEAN_CTX_HEADLESS='1', LEAN_CTX_CONVERSATION_SCOPE='0',
                       LEAN_CTX_EMBEDDINGS_AUTO_DOWNLOAD='0', LEAN_CTX_LIVE_PRICING='off',
                       LEAN_CTX_DEBUG_LOG='0', LEAN_CTX_JOURNAL='0', LEAN_CTX_AUTO_CAPTURE='0',
                       LEAN_CTX_PROJECT_ROOT=str(project))
            for kind in ('CONFIG', 'DATA', 'STATE', 'CACHE'):
                directory = root / kind.lower()
                directory.mkdir()
                env['LEAN_CTX_' + kind + '_DIR'] = str(directory)
                env['XDG_' + kind + '_HOME'] = str(directory)
            (root / 'config/config.toml').write_text('minimal_overhead = true\n[cache]\ncompose_cache_enabled = true\n')
            messages = queue.Queue()
            with (root / 'stderr.log').open('w+') as stderr:
                process = subprocess.Popen([str(binary), 'mcp'], cwd=project, env=env,
                    stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=stderr, text=True, bufsize=1)
                initial_pid = process.pid
                def receive():
                    for line in process.stdout:
                        try:
                            messages.put(json.loads(line))
                        except json.JSONDecodeError:
                            messages.put({'transport_error': line})
                    messages.put({'transport_eof': True})
                reader = threading.Thread(target=receive, daemon=True)
                reader.start()
                def send(value):
                    process.stdin.write(json.dumps(value) + '\n')
                    process.stdin.flush()
                next_id = 0
                roots_requested = False
                def request(method, params):
                    nonlocal next_id, roots_requested
                    next_id += 1
                    current = next_id
                    send({'jsonrpc': '2.0', 'id': current, 'method': method, 'params': params})
                    deadline = time.monotonic() + 40
                    while True:
                        remaining = deadline - time.monotonic()
                        if remaining <= 0:
                            raise TimeoutError('MCP response deadline')
                        reply = messages.get(timeout=remaining)
                        if 'transport_error' in reply or 'transport_eof' in reply:
                            raise RuntimeError(reply)
                        if reply.get('method') == 'roots/list':
                            roots_requested = True
                            send({'jsonrpc': '2.0', 'id': reply['id'], 'result': {
                                'roots': [{'uri': project.as_uri(), 'name': 'fixture'}]}})
                        elif reply.get('id') == current and 'method' not in reply:
                            return reply
                def case(name, *, present=(), absent=(), ranked=(), blocked=False):
                    begin = time.perf_counter()
                    reply = request('tools/call', {'name': 'ctx_compose', 'arguments': {
                        'task': TASK, 'path': str(project), 'task_aware': False}})
                    text = response_text(reply)
                    section = ranked_section(text)
                    forbidden = tuple(absent) + (('private.rs', 'CLASSIFIED_CONTEXT') if policy.exists() else ())
                    checks = {'present': all(value in text for value in present),
                              'absent': all(value not in json.dumps(reply) for value in forbidden),
                              'ranked': all(value in section for value in ranked),
                              'blocked': has_error(reply) == blocked,
                              'same_process': process.poll() is None and process.pid == initial_pid}
                    report['cases'].append({'case': name, 'checks': checks, 'passed': all(checks.values()),
                                            'response': reply, 'elapsed_ms': 1000 * (time.perf_counter() - begin)})
                def same_metadata_write(content):
                    before = mutable.stat()
                    assert len(content.encode()) == before.st_size
                    mutable.write_text(content)
                    os.utime(mutable, ns=(before.st_atime_ns, before.st_mtime_ns))
                    after = mutable.stat()
                    assert before.st_size == after.st_size and before.st_mtime_ns == after.st_mtime_ns
                try:
                    report['initialize'] = request('initialize', {'protocolVersion': '2024-11-05',
                        'capabilities': {'roots': {'listChanged': False}},
                        'clientInfo': {'name': 'compose-admission-fixture', 'version': '1'}})
                    assert 'result' in report['initialize']
                    send({'jsonrpc': '2.0', 'method': 'notifications/initialized'})
                    request('tools/list', {})
                    case('community-ranked-precondition', ranked=('allowed.rs', 'mutable.rs', 'private.rs'))
                    policy.write_text(RULES)
                    protected = {'ranked': ('allowed.rs', 'PERMITTED_CONTEXT', 'REDACTED'),
                                 'absent': ('CLASSIFIED_CONTEXT', 'private.rs', 'CUS-1234')}
                    case('protected-useful-ranked-context', **protected)
                    case('protected-repeated-context', **protected)
                    changed = source('alpha', 'MUTABLE_AFTER_')
                    same_metadata_write(changed)
                    case('same-metadata-content-change', present=('MUTABLE_AFTER_',), absent=('MUTABLE_BEFORE', 'CUS-1234', 'CLASSIFIED_CONTEXT'), ranked=('mutable.rs',))
                    same_metadata_write(changed.replace('PUBLIC______', 'CONFIDENTIAL'))
                    case('same-metadata-classification-change', ranked=('allowed.rs', 'PERMITTED_CONTEXT'), absent=('MUTABLE_AFTER_', 'mutable.rs', 'CLASSIFIED_CONTEXT', 'CUS-1234'))
                    policy.write_text(RULES.replace('CUS-[0-9]{4}', 'CUS-[0-9]{4}|ACC-[0-9]{4}'))
                    case('changed-mask-policy', ranked=('allowed.rs', 'REDACTED'), absent=('CUS-1234', 'ACC-5678', 'CLASSIFIED_CONTEXT', 'MUTABLE_AFTER_'))
                    policy.write_text(RULES + "[context]\ndeny_tools = ['ctx_search']\n")
                    case('nested-search-revoked', present=('source search is not authorized',), absent=('allowed.rs', 'PERMITTED_CONTEXT', 'CLASSIFIED_CONTEXT', 'MUTABLE_AFTER_'))
                    policy.write_text(RULES + "[context]\ndeny_tools = ['ctx_compose']\n")
                    case('compose-revoked', absent=('PERMITTED_CONTEXT', 'CLASSIFIED_CONTEXT'), blocked=True)
                    policy.write_text('[invalid')
                    case('invalid-policy', absent=('PERMITTED_CONTEXT', 'CLASSIFIED_CONTEXT'), blocked=True)
                    policy.write_text(RULES)
                    case('policy-repaired', **protected)
                    policy.unlink()
                    case('policy-removal-uses-current-content', ranked=('private.rs', 'mutable.rs', 'MUTABLE_AFTER_'), absent=('MUTABLE_BEFORE',))
                    assert roots_requested
                    report['client_root_adopted'] = roots_requested
                finally:
                    process.terminate()
                    try:
                        process.wait(timeout=5)
                    except subprocess.TimeoutExpired:
                        process.kill()
                        process.wait(timeout=5)
                    reader.join(timeout=2)
                    stderr.seek(0)
                    report['stderr'] = stderr.read()
    except Exception as error:
        report['error'] = repr(error)
    report['passed'] = not report.get('error') and len(report['cases']) == 11 and all(item['passed'] for item in report['cases'])
    report['baseline_reproduced'] = (args.mode == 'baseline' and report['binary_sha256'] == BASELINE
        and not report.get('error') and len(report['cases']) == 11
        and report['cases'][0]['passed'] and not report['cases'][1]['checks']['ranked'])
    args.output.write_text(json.dumps(report, indent=2) + '\n')
    print(json.dumps({'passed': report['passed'], 'baseline_reproduced': report['baseline_reproduced'],
                      'cases': len(report['cases']), 'failed': [x['case'] for x in report['cases'] if not x['passed']], 'error': report.get('error')}))
    return 0 if (report['baseline_reproduced'] if args.mode == 'baseline' else report['passed']) else 1


if __name__ == '__main__':
    raise SystemExit(main())
