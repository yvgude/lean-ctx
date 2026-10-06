# SPDX-License-Identifier: Apache-2.0
"""Bounded built-proxy flow; a loopback provider fixture, not provider-quality proof."""
import hashlib
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import os
from pathlib import Path
import socket
import subprocess
import threading
import time
import tomllib
import urllib.error
import urllib.request


def check_proxy(run, record, root, installed, common, target, digest, binary, env, scope_only=False):
    if scope_only:
        run('proxy-runtime-activate', ['activate', *common, *target, '--expected-active', digest], True,
            prefix=('setup', 'runtime'))
    configs = list(root.rglob('config.toml'))
    assert len(configs) == 1
    config = configs[0]
    original_config = config.read_bytes()
    assert 'proxy' not in tomllib.loads(original_config.decode())
    served = []

    class Provider(BaseHTTPRequestHandler):
        def do_POST(self):
            body = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
            served.append(body)
            payload = json.dumps({'id': 'fixture-completion', 'object': 'chat.completion',
                                  'model': body['model'], 'choices': [{'index': 0, 'finish_reason': 'stop',
                                  'message': {'role': 'assistant', 'content': 'loopback fixture'}}],
                                  'usage': {'prompt_tokens': 10, 'completion_tokens': 2, 'total_tokens': 12}}).encode()
            self.send_response(200)
            self.send_header('Content-Type', 'application/json')
            self.send_header('Content-Length', str(len(payload)))
            self.end_headers()
            self.wfile.write(payload)

        def log_message(self, *args):
            pass

    server = ThreadingHTTPServer(('127.0.0.1', 0), Provider)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    local_upstream = 'http://127.0.0.1:' + str(server.server_port)
    proxy_env = {key: value for key, value in env.items()
                 if key in ('PATH', 'HOME', 'TMPDIR', 'XDG_CONFIG_HOME', 'XDG_DATA_HOME',
                            'XDG_STATE_HOME', 'XDG_CACHE_HOME', 'LEAN_CTX_DATA_DIR', 'LEAN_CTX_PROJECT_ROOT')}
    proxy_env.update(DO_NOT_TRACK='1', LEAN_CTX_LIVE_PRICING='off', NO_PROXY='127.0.0.1,localhost')
    token = os.urandom(32).hex()
    proxy_env['LEAN_CTX_PROXY_TOKEN'] = token
    for provider in ('OPENAI', 'ANTHROPIC', 'GEMINI', 'CHATGPT'):
        proxy_env['LEAN_CTX_' + provider + '_UPSTREAM'] = local_upstream
    # dirs::config_dir follows the platform layout, independent of LeanCTX's pin.
    policy_home = Path(proxy_env['HOME']) / 'Library/Application Support' if os.uname().sysname == 'Darwin' else Path(proxy_env['XDG_CONFIG_HOME'])
    policy_file = policy_home / 'lean-ctx/router-policy.toml'
    policy_file.parent.mkdir(parents=True, exist_ok=True)
    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))

    def evidence():
        found = {}
        for path in root.rglob('execution/evidence/*.json'):
            raw = path.read_bytes()
            value = json.loads(raw)
            if 'enhancement' in value:
                found[hashlib.sha256(raw).hexdigest()] = value
        return found

    def case(name, expected_status=200, expected_model=None, enhancement=None, policy='', alias=False, enabled=True,
             fixed_prompt=None, lineage_headers=None):
        before_evidence = evidence()
        before_requests = len(served)
        policy_file.write_text(policy)
        routing = ('\n[proxy]\nallow_custom_upstream = true\n'
                   '[proxy.routing]\nenabled = ' + str(enabled).lower() + '\n'
                   '[proxy.routing.tiers]\nfast = "openai:gpt-4o-mini"\n'
                   'standard = "openai:gpt-4o-mini"\npremium = "openai:gpt-4o-mini"\n')
        if alias:
            routing += '[proxy.routing.aliases]\n"gpt-4o" = "openai:gpt-4o-mini"\n'
        config.write_bytes(base_config + routing.encode())
        with socket.socket() as reserve:
            reserve.bind(('127.0.0.1', 0))
            port = reserve.getsockname()[1]
        argv = [str(binary), 'proxy', 'start', '--port=' + str(port)]
        process = subprocess.Popen(argv, cwd=root, env=proxy_env, stdin=subprocess.DEVNULL,
                                   stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        row = {'name': 'proxy-' + name, 'argv': argv, 'provider_fixture': True}
        try:
            deadline = time.monotonic() + 15
            while True:
                assert process.poll() is None, 'proxy exited before ready'
                try:
                    opener.open('http://127.0.0.1:' + str(port) + '/health', timeout=1).close()
                    break
                except (OSError, urllib.error.HTTPError):
                    assert time.monotonic() < deadline, 'proxy readiness timeout'
                    time.sleep(0.1)
            prompt = fixed_prompt or 'Summarize this short text. Local fixture ' + name
            payload = {'model': 'gpt-4o', 'stream': False, 'messages': [{'role': 'user', 'content': prompt}]}
            raw_request = json.dumps(payload).encode()
            request = urllib.request.Request('http://127.0.0.1:' + str(port) + '/v1/chat/completions',
                      data=raw_request, headers={'Content-Type': 'application/json', 'Authorization': 'Bearer ' + token,
                                                 **(lineage_headers or {})})
            try:
                with opener.open(request, timeout=15) as response:
                    status, raw_response = response.status, response.read()
            except urllib.error.HTTPError as error:
                status, raw_response = error.code, error.read()
            after_evidence = evidence()
            delta = {key: value for key, value in after_evidence.items() if key not in before_evidence}
            row.update(http_status=status, request=payload, response=raw_response.decode(),
                       lineage_headers=lineage_headers or {},
                       request_sha256=hashlib.sha256(raw_request).hexdigest(),
                       response_sha256=hashlib.sha256(raw_response).hexdigest(),
                       upstream_requests=served[before_requests:], evidence=delta)
            assert status == expected_status, row
            if enhancement is None:
                assert not delta, row
            if status == 200:
                assert len(served) == before_requests + 1
                selected = served[-1]['model']
                assert selected in ('gpt-4o', 'gpt-4o-mini')
                if expected_model:
                    assert selected == expected_model, row
                if enhancement:
                    assert len(delta) == 1, row
                    proof = next(iter(delta.values()))
                    assert proof['enhancement'] == enhancement, row
                    assert proof['decision']['selected']['model'] == selected
                    if enhancement == 'private_runtime':
                        projection = proof['evidence']['request']['input']
                        assert prompt not in projection and 'gpt-4o' not in projection
                        assert proof['evidence']['execution']['status'] == 'executed'
            else:
                assert len(served) == before_requests, row
        finally:
            process.terminate()
            try:
                stdout, stderr = process.communicate(timeout=10)
            except subprocess.TimeoutExpired:
                process.kill()
                stdout, stderr = process.communicate(timeout=5)
            row.update(exit_code=process.returncode, stdout=stdout.decode(), stderr=stderr.decode(),
                       stdout_sha256=hashlib.sha256(stdout).hexdigest(), stderr_sha256=hashlib.sha256(stderr).hexdigest())
            record(row)
        return row

    try:
        base_config = original_config
        if scope_only:
            rows = [case('scope-' + label, enhancement='private_runtime',
                         fixed_prompt='Summarize this short text. Same body across scoped requests.',
                         lineage_headers={'x-leanctx-request-id': 'spoofed-request',
                                          'x-leanctx-session-id': 'spoofed-session',
                                          'x-leanctx-agent-id': 'spoofed-agent-' + label,
                                          'x-leanctx-task-id': 'spoofed-task',
                                          'x-leanctx-tenant-id': 'spoofed-tenant'})
                    for label in ('a', 'b')]
            assert rows[0]['request_sha256'] == rows[1]['request_sha256']
            plans = [next(iter(row['evidence'].values()))['decision']['selected'] for row in rows]
            assert plans[0]['task_id'] != plans[1]['task_id']
            assert plans[0]['plan_id'] != plans[1]['plan_id']
            for row, plan in zip(rows, plans):
                assert plan['task_id'].startswith('mcp-task-')
                assert plan['executor_agent_id'] == 'OpenAI'
                assert 'spoofed-' not in json.dumps(row['evidence'])
            return
        case('disabled-reference', expected_model='gpt-4o-mini')
        config.write_bytes(original_config)
        run('proxy-runtime-activate', ['activate', *common, *target, '--expected-active', digest], True,
            prefix=('setup', 'runtime'))
        base_config = config.read_bytes()
        case('signed-private', enhancement='private_runtime')
        valid_config = base_config
        key = tomllib.loads(base_config.decode())['intelligence_runtime']['trust_key_hex'].encode()
        base_config = base_config.replace(key, b'invalid-independent-trust')
        case('invalid-trust-reference', expected_model='gpt-4o-mini')
        base_config = valid_config
        original = installed.read_bytes()
        installed.chmod(0o600)
        installed.write_bytes(original + b'corrupt')
        try:
            case('tamper-fallback', expected_model='gpt-4o-mini', enhancement='runtime_unavailable_reference_fallback')
        finally:
            installed.write_bytes(original)
            installed.chmod(0o500)
        case('denied-tier', 403, policy='model_denylist = ["gpt-4o-mini"]\n')
        case('denied-alias', 403, policy='model_denylist = ["gpt-4o-mini"]\n', alias=True)
        case('denied-passthrough', 403, policy='model_denylist = ["gpt-4o"]\n', enabled=False)
        case('malformed-policy', 403, policy='model_allowlist = [broken', enabled=False)
        case('unknown-policy-field', 403, policy='model_allowlst = ["gpt-4o"]\n', enabled=False)
        case('unverified-cost', 403, policy='max_cost_micros = 1\n', alias=True)
    finally:
        config.write_bytes(original_config)
        server.shutdown()
        server.server_close()
        thread.join(timeout=5)
