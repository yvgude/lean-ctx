# SPDX-License-Identifier: Apache-2.0
"""Same-process MCP policy changes, root adoption, repeated reads/search and replay."""
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


BASE = 'name = "refresh"\nversion = "1.0.0"\ndescription = "fixture"\n'


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    modes = parser.add_mutually_exclusive_group()
    modes.add_argument('--diagnostics', action='store_true', help='Exercise protected diagnostic and automatic finding writes')
    modes.add_argument('--knowledge', action='store_true', help='Exercise protected knowledge persistence and archive recovery')
    options = parser.parse_args()
    binary = options.binary.resolve(strict=True)
    report = {'binary_sha256': hashlib.sha256(binary.read_bytes()).hexdigest(),
              'cases': [], 'passed': False, 'scope': 'isolated direct MCP; no model providers'}
    options.output.parent.mkdir(parents=True, exist_ok=True)
    try:
        with tempfile.TemporaryDirectory(prefix='leanctx-policy-refresh-') as temporary:
            root = Path(temporary)
            project = root / 'project'
            outside = root / 'outside'
            project.mkdir()
            outside.mkdir()
            project.joinpath('.git').mkdir()
            sample = project / 'sample.txt'
            sample.write_text('account CUS-1234\nreference ACC-5678\n')
            source = project / 'src/lib.rs'
            source.parent.mkdir()
            source.write_text('// CONFIDENTIAL\npub fn LoginProbe() { /* source_canary CUS-1234 */ }\n')
            (project / 'Cargo.toml').write_text('# CONFIDENTIAL\n[dependencies]\nmanifest_canary = "1"\n')
            policy = project / '.lean-ctx/policy.toml'
            policy.parent.mkdir()
            env = {key: value for key, value in os.environ.items()
                   if key in ('PATH', 'TMPDIR', 'SystemRoot', 'WINDIR')}
            env.update(HOME=str(root / 'home'), USERPROFILE=str(root / 'home'),
                       APPDATA=str(root / 'config'), LOCALAPPDATA=str(root / 'data'),
                       DO_NOT_TRACK='1', LEAN_CTX_HOOK_CHILD='1', LEAN_CTX_ACTIVE='1',
                       LEAN_CTX_HEADLESS='1', LEAN_CTX_CONVERSATION_SCOPE='0',
                       LEAN_CTX_EMBEDDINGS_AUTO_DOWNLOAD='0', LEAN_CTX_LIVE_PRICING='off',
                       LEAN_CTX_PROJECT_ROOT=str(outside))
            for category in ('CONFIG', 'DATA', 'STATE', 'CACHE'):
                path = root / category.lower()
                path.mkdir()
                env['LEAN_CTX_' + category + '_DIR'] = str(path)
                env['XDG_' + category + '_HOME'] = str(path)
            root.joinpath('home').mkdir()
            root.joinpath('config/config.toml').write_text('minimal_overhead = true\n')
            if options.knowledge:
                env.update(LEAN_CTX_DEBUG_LOG='0', LEAN_CTX_JOURNAL='0', LEAN_CTX_AUTO_CAPTURE='0')
                policy.write_text(BASE + "[redaction]\ncustomer = 'CUS-[0-9]{4}'\n")
            if options.diagnostics:
                env.update(LEAN_CTX_DEBUG_LOG='1', LEAN_CTX_JOURNAL='1', LEAN_CTX_AUTO_CAPTURE='1')
                policy.write_text(BASE + "[redaction]\ncustomer = 'CUS-[0-9]{4}'\n")
                root.joinpath('state/logs').mkdir(parents=True)
                root.joinpath('state/logs/debug.log').write_text('LEGACY-UNSCOPED-CUS-9999\n')
            messages = queue.Queue()
            with root.joinpath('stderr.log').open('w+') as stderr:
                process = subprocess.Popen([str(binary), 'mcp'], cwd=outside, env=env,
                                           stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                           stderr=stderr, text=True, bufsize=1)
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

                def request(method, params, sent=None):
                    nonlocal next_id, roots_requested
                    next_id += 1
                    request_id = next_id
                    send({'jsonrpc': '2.0', 'id': request_id, 'method': method, 'params': params})
                    if sent is not None:
                        sent.set()
                    deadline = time.monotonic() + 25
                    while True:
                        message = messages.get(timeout=max(0.01, deadline - time.monotonic()))
                        assert 'transport_error' not in message and 'transport_eof' not in message, message
                        if message.get('method') == 'roots/list':
                            roots_requested = True
                            send({'jsonrpc': '2.0', 'id': message['id'],
                                  'result': {'roots': [{'uri': project.as_uri(), 'name': 'fixture'}]}})
                        elif message.get('id') == request_id and 'method' not in message:
                            return message
                        if time.monotonic() >= deadline:
                            raise TimeoutError('MCP response deadline exceeded')

                def case(name, tool='ctx_read', arguments=None, present=(), absent=(), blocked=False, key=None, sent=None):
                    params = {'name': tool, 'arguments': arguments or {
                        'path': str(sample), 'mode': 'full', 'fresh': True,
                        'limit': 200 if key else 100 + len(report['cases'])}}
                    if key:
                        params['_meta'] = {'idempotencyKey': key}
                    started = time.monotonic()
                    response = request('tools/call', params, sent)
                    row = {'case': name, 'elapsed_ms': round((time.monotonic() - started) * 1000, 3),
                           'response': response}
                    report['cases'].append(row)
                    raw = json.dumps(response)
                    for marker in present:
                        assert marker in raw, (name, 'missing', marker, response)
                    for marker in absent:
                        assert marker not in raw, (name, 'leaked', marker, response)
                    result = response.get('result', {})
                    denied = result.get('isError', False) or 'error' in response or 'POLICY BLOCKED' in raw
                    assert bool(denied) == blocked, (name, response)
                    assert process.poll() is None and process.pid == initial_pid

                try:
                    init = request('initialize', {'protocolVersion': '2024-11-05',
                        'capabilities': {'roots': {'listChanged': False}},
                        'clientInfo': {'name': 'policy-refresh-fixture', 'version': '1'}})
                    assert 'result' in init, init
                    send({'jsonrpc': '2.0', 'method': 'notifications/initialized'})
                    if options.knowledge:
                        def state():
                            matches = [(path, json.loads(path.read_text()))
                                for path in root.joinpath('data/knowledge').rglob('knowledge.json')]
                            matches = [(path, data) for path, data in matches
                                if Path(data.get('project_root', '')).resolve() == project.resolve()]
                            assert len(matches) == 1, ('expected one canonical knowledge store', matches)
                            return matches[0]

                        remember = {'action': 'remember', 'category': 'finding', 'key': 'account',
                            'content': 'Customer account CUS-1234 reference ACC-5678'}
                        case('knowledge-remember-masks-before-save', 'ctx_knowledge', remember,
                            present=('Remembered', 'REDACTED'), absent=('CUS-1234',))
                        path, first = state()
                        assert 'CUS-1234' not in json.dumps(first), ('raw knowledge leak', first)
                        assert any(fact['key'] == 'account' for fact in first['facts'])
                        report['knowledge'] = {'initial_store': first}
                        case('knowledge-recall-uses-safe-store', 'ctx_knowledge',
                            {'action': 'recall', 'query': 'Customer account', 'mode': 'full'},
                            present=('REDACTED',), absent=('CUS-1234',))

                        archived = dict(next(fact for fact in first['facts'] if fact['key'] == 'account'))
                        archived.update(key='archive-account', value='Recovered account CUS-2222')
                        archive = root / 'data/memory/archive/facts' / first['project_hash'] / 'archive-20260922-000000-000001.json'
                        archive.parent.mkdir(parents=True)
                        archive.write_text(json.dumps({'archived_at': first['updated_at'], 'store': 'facts',
                            'scope': first['project_hash'], 'items': [archived]}))
                        original_archive = archive.read_bytes()
                        case('knowledge-archive-restore-rechecks-content', 'ctx_knowledge',
                            {'action': 'restore', 'category': 'facts', 'query': 'Recovered'},
                            present=('Restored 1',), absent=('CUS-2222',))
                        _, restored = state()
                        assert any(fact['key'] == 'archive-account' for fact in restored['facts']), restored
                        assert 'CUS-2222' not in json.dumps(restored), restored
                        assert archive.read_bytes() == original_archive, 'recovery must retain the original archive'
                        report['knowledge']['after_restore'] = restored

                        before = path.read_bytes()
                        policy.write_text(BASE + "[redaction]\ncustomer = 'CUS-[0-9]{4}|ACC-[0-9]{4}'\n")
                        case('knowledge-rule-change-filters-existing-read', 'ctx_knowledge',
                            {'action': 'recall', 'query': 'Customer account', 'mode': 'full'},
                            present=('REDACTED',), absent=('CUS-1234', 'ACC-5678'))
                        assert path.read_bytes() == before, 'filtered view must not silently migrate old data'
                        case('knowledge-unsafe-legacy-update-is-explicit-error', 'ctx_knowledge',
                            {'action': 'remember', 'category': 'finding', 'key': 'new-fact', 'content': 'Safe new decision'},
                            present=('knowledge was not saved',), absent=('Remembered', 'ACC-5678'), blocked=True)
                        assert path.read_bytes() == before
                        policy.write_text(BASE + "[context]\ndeny_tools = ['ctx_knowledge']\n")
                        case('knowledge-revocation-stops-access', 'ctx_knowledge',
                            {'action': 'recall', 'query': 'Customer'}, absent=('ACC-5678', 'Customer account'), blocked=True)
                        assert path.read_bytes() == before
                        report['knowledge']['original_store_and_archive_retained'] = True
                        if os.name != 'posix':
                            raise RuntimeError('knowledge lock journey currently requires a POSIX host')
                        import fcntl
                        policy.write_text(BASE + "[redaction]\ncustomer = 'CUS-[0-9]{4}'\n")
                        sent = threading.Event()
                        errors = []

                        def pending_write():
                            try:
                                case('knowledge-revocation-while-write-is-pending', 'ctx_knowledge',
                                    {'action': 'remember', 'category': 'finding', 'key': 'queued-write',
                                     'content': 'Queued account CUS-4444'},
                                    absent=('Remembered', 'CUS-4444'), blocked=True, sent=sent)
                            except BaseException as error:
                                errors.append(error)

                        with path.with_name('.knowledge.lock').open('a+') as held:
                            fcntl.flock(held, fcntl.LOCK_EX)
                            worker = threading.Thread(target=pending_write, daemon=True)
                            worker.start()
                            assert sent.wait(timeout=5), 'MCP write was not submitted'
                            worker.join(timeout=0.2)
                            assert worker.is_alive(), 'write unexpectedly completed while its lock was held'
                            policy.write_text(BASE + "[context]\ndeny_tools = ['ctx_knowledge']\n")
                            fcntl.flock(held, fcntl.LOCK_UN)
                            worker.join(timeout=5)
                            assert not worker.is_alive(), 'revoked pending write did not complete'
                        if errors:
                            raise errors[0]
                        assert path.read_bytes() == before, 'revoked pending write changed the store'
                        report['knowledge']['revoked_pending_write_preserved_store'] = True
                    elif options.diagnostics:
                        secret = 'sk-proj-abcdefghijklmnopqrstuvwxyz0123456789'

                        def snapshot(marker):
                            deadline = time.monotonic() + 8
                            while True:
                                logs = {str(path.relative_to(root)): path.read_text()
                                    for path in root.joinpath('state').rglob('*')
                                    if path.is_file() and 'projects' in path.parts
                                    and path.name in ('debug.log', 'journal.md')}
                                knowledge = {str(path.relative_to(root)): json.loads(path.read_text())
                                    for path in root.joinpath('data/knowledge').rglob('knowledge.json')}
                                captured = any(Path(data.get('project_root', '')).resolve() == project.resolve()
                                    and any(fact.get('key') == 'auto:sample.txt' for fact in data.get('facts', []))
                                    for data in knowledge.values())
                                if any(marker in text for text in logs.values()) and len(logs) == 2 and captured:
                                    return {'logs': logs, 'knowledge': knowledge}
                                if time.monotonic() > deadline:
                                    raise AssertionError(('diagnostic writes did not complete', logs, knowledge))
                                time.sleep(0.02)

                        case('protected-diagnostic-fields', arguments={
                            'path': str(sample), 'mode': 'full', 'fresh': True, 'limit': 201,
                            'a': {'CUS-1234': ['CUS-9999', 'ACC-5678', secret]}, 'b': 'DIAGNOSTICPROBE'},
                            present=('REDACTED',), absent=('CUS-1234', secret))
                        first = snapshot('DIAGNOSTICPROBE')
                        for marker in ('CUS-1234', 'CUS-9999', secret, 'LEGACY-UNSCOPED'):
                            assert marker not in json.dumps(first), ('diagnostic leak', marker, first)
                        assert 'ACC-5678' in json.dumps(first['logs']), 'fixture must retain an initially permitted field'
                        policy.write_text(BASE + "[redaction]\ncustomer = 'CUS-[0-9]{4}|ACC-[0-9]{4}'\n")
                        case('existing-diagnostics-rechecked', arguments={
                            'path': str(sample), 'mode': 'full', 'fresh': True, 'limit': 202,
                            'b': 'DIAGNOSTICUPDATED'}, present=('REDACTED',), absent=('CUS-1234', 'ACC-5678'))
                        second = snapshot('DIAGNOSTICUPDATED')
                        for marker in ('CUS-1234', 'CUS-9999', 'ACC-5678', secret, 'LEGACY-UNSCOPED'):
                            assert marker not in json.dumps(second['logs']), ('stored diagnostic leak', marker, second)
                        private_path = project / 'CUS-4321.txt'
                        private_path.write_text('private file CUS-4321\n')
                        case('read-path-diagnostic-has-no-sensitive-filename', arguments={
                            'path': str(private_path), 'mode': 'full', 'fresh': True, 'limit': 203,
                            'b': 'DIAGNOSTICPATH'}, present=('REDACTED',), absent=('CUS-4321',))
                        after_path = snapshot('DIAGNOSTICPATH')
                        assert 'CUS-4321' not in json.dumps(after_path), ('stored path leak', after_path)
                        report['diagnostics'] = {'before_rule_change': first, 'after_rule_change': second,
                            'after_sensitive_path_read': after_path,
                            'scope': 'project-scoped diagnostic logs and automatic finding store; other indexes/stores not scanned',
                            'legacy_unscoped_file_retained': root.joinpath('state/logs/debug.log').exists()}
                    else:
                        case('community-read', present=('CUS-1234', 'ACC-5678'))
                        assert roots_requested, 'fixture must exercise client root adoption on the first call'
                        case('initial-replay', present=('CUS-1234',), key='before-policy')
                        search = {'pattern': 'account', 'path': str(project), 'include': '*.txt'}
                        case('initial-search', 'ctx_search', search, present=('CUS-1234',))
                        case('repeated-search', 'ctx_search', search, present=('CUS-1234',))
                        policy.write_text(BASE + "[redaction]\ncustomer = 'CUS-[0-9]{4}'\n")
                        case('policy-added-without-restart', present=('ACC-5678', 'REDACTED'), absent=('CUS-1234',))
                        case('search-after-policy-change', 'ctx_search', search, absent=('CUS-1234',), present=('REDACTED',))
                        case('old-idempotent-replay-withheld', absent=('CUS-1234',), blocked=True, key='before-policy')
                        prior = policy.stat()
                        policy.write_text(BASE + "[redaction]\ncustomer = 'ACC-[0-9]{4}'\n")
                        os.utime(policy, ns=(prior.st_atime_ns, prior.st_mtime_ns))
                        case('same-size-and-mtime-rule-change', present=('CUS-1234', 'REDACTED'), absent=('ACC-5678',))
                        policy.write_text(BASE + "[context]\ndeny_tools = ['ctx_read']\n")
                        case('tool-revoked', absent=('CUS-1234', 'ACC-5678'), blocked=True)
                        policy.write_text('[invalid')
                        case('invalid-policy-denied', absent=('CUS-1234', 'ACC-5678'), blocked=True)
                        policy.write_text(BASE)
                        case('policy-repaired-without-restart', present=('CUS-1234', 'ACC-5678'))
                        policy.write_text(BASE + "[redaction]\ncustomer = 'CUS-[0-9]{4}'\n")
                        symbol = {'name': 'LoginProbe', 'path': str(project)}
                        ready = False
                        for attempt in range(20):
                            reply = request('tools/call', {'name': 'ctx_symbol', 'arguments': symbol})
                            if 'source_canary' in json.dumps(reply):
                                ready = True
                                break
                            time.sleep(0.5)
                        assert ready, 'symbol fixture needs a populated graph before negative checks'
                        case('symbol-admitted-original', 'ctx_symbol', symbol,
                             present=('source_canary', 'REDACTED'), absent=('CUS-1234',))
                        compose = {'task': 'investigate LoginProbe', 'path': str(project)}
                        case('compose-admitted-original', 'ctx_compose', compose,
                             present=('source_canary', 'manifest_canary'), absent=('CUS-1234',))
                        policy.write_text(BASE + "[redaction]\nlabel = 'CONFIDENTIAL'\n[filters]\nclassification = 'block'\n")
                        case('symbol-original-classification', 'ctx_symbol', symbol,
                             absent=('source_canary', 'CUS-1234'))
                        case('compose-original-classification', 'ctx_compose', compose,
                             absent=('source_canary', 'manifest_canary', 'CUS-1234'))
                    assert roots_requested, 'fixture must exercise client root adoption'
                    report['passed'] = True
                    report['single_process'] = True
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
                    if options.diagnostics and 'CUS-4321' in report['stderr']:
                        report['passed'] = False
                        raise AssertionError('sensitive read filename leaked through stderr')
    finally:
        options.output.write_text(json.dumps(report, indent=2) + '\n')
    print(json.dumps({'passed': len(report['cases']), 'report': str(options.output),
                      'binary_sha256': report['binary_sha256']}))


if __name__ == '__main__':
    main()
