# SPDX-License-Identifier: Apache-2.0
"""Focused built-proxy measurement check; no private runtime or live provider."""
import argparse
import hashlib
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import http.client
import json
import os
from pathlib import Path
import socket
import subprocess
import tempfile
import threading
import time
import urllib.error
import urllib.request


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', type=Path, required=True)
    parser.add_argument('--binary-sha256', required=True)
    parser.add_argument('--report', type=Path, required=True)
    args = parser.parse_args()
    assert hashlib.sha256(args.binary.read_bytes()).hexdigest() == args.binary_sha256
    cases, requests = [], []
    active = {'name': 'unknown-cost'}

    class Provider(BaseHTTPRequestHandler):
        protocol_version = 'HTTP/1.1'

        def do_POST(self):
            request = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
            name = active['name']
            requests.append(name)
            time.sleep(0.08)
            usage = {'prompt_tokens': 10, 'completion_tokens': 2}
            if name in ('measured-cost', 'body-beats-header'):
                usage['cost'] = 0.125
            value = {'model': request['model'], 'choices': [{'finish_reason': 'stop',
                     'message': {'role': 'assistant', 'content': 'local response'}}], 'usage': usage}
            stream = name.startswith('stream-')
            if name == 'stream-failed-terminal':
                value = {'type': 'response.failed', 'response': {'model': request['model'],
                         'usage': {'input_tokens': 10, 'output_tokens': 2}}}
            if name == 'stream-terminal':
                value['choices'] = []
            if name in ('stream-partial', 'stream-error'):
                value['usage'] = {'prompt_tokens': 10}
            payload = json.dumps(value).encode()
            if stream:
                payload = b'data: ' + payload + b'\n\n'
            self.send_response(400 if name == 'http-error' else 200)
            self.send_header('Content-Type', 'text/event-stream' if stream else 'application/json')
            self.send_header('Content-Length', str(len(payload) + (100 if name == 'stream-error' else 0)))
            self.send_header('Connection', 'close')
            if name in ('header-cost', 'body-beats-header'):
                self.send_header('x-litellm-response-cost', '0.25')
            self.end_headers()
            self.wfile.write(payload)
            self.wfile.flush()
            self.close_connection = True

        def log_message(self, *args):
            pass

    report = {'passed': False, 'user_acceptance': False, 'full_suite': False,
              'host_binary_sha256': args.binary_sha256, 'cases': cases}
    with args.report.open('x') as output, tempfile.TemporaryDirectory(prefix='leanctx-usage-observation-') as tmp:
        root = Path(tmp).resolve()
        env = {'PATH': '/usr/bin:/bin:/usr/sbin:/sbin', 'HOME': str(root / 'home'),
               'TMPDIR': str(root), 'LEAN_CTX_DATA_DIR': str(root / 'data'),
               'LEAN_CTX_CONFIG_DIR': str(root / 'config'), 'LEAN_CTX_PROJECT_ROOT': str(root),
               'XDG_CONFIG_HOME': str(root / 'xdg-config'), 'XDG_DATA_HOME': str(root / 'xdg-data'),
               'XDG_STATE_HOME': str(root / 'state'), 'XDG_CACHE_HOME': str(root / 'cache'),
               'DO_NOT_TRACK': '1', 'LEAN_CTX_LIVE_PRICING': 'off', 'NO_PROXY': '127.0.0.1,localhost'}
        for key in ('HOME', 'LEAN_CTX_DATA_DIR', 'LEAN_CTX_CONFIG_DIR'):
            Path(env[key]).mkdir(mode=0o700)
        (root / 'config/config.toml').write_text('[proxy]\nallow_custom_upstream = true\n[proxy.routing]\nenabled = false\n')
        token = os.urandom(32).hex()
        env['LEAN_CTX_PROXY_TOKEN'] = token
        server = ThreadingHTTPServer(('127.0.0.1', 0), Provider)
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        for provider in ('OPENAI', 'ANTHROPIC', 'GEMINI', 'CHATGPT'):
            env['LEAN_CTX_' + provider + '_UPSTREAM'] = 'http://127.0.0.1:' + str(server.server_port)
        opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
        usage_file = root / 'data/proxy_usage.json'
        usage_file.write_text(json.dumps({'ts': 1, 'models': {'gpt-4o': {
            'requests': 0, 'input_tokens': 0, 'output_tokens': 0, 'cache_read_tokens': 0,
            'cache_write_tokens': 0, 'reasoning_tokens': 0}}}))
        process = None

        def retain():
            output.seek(0)
            json.dump(report, output, indent=2, sort_keys=True)
            output.write('\n')
            output.truncate()
            output.flush()

        def stop():
            nonlocal process
            if process is None:
                return
            process.terminate()
            try:
                stdout, stderr = process.communicate(timeout=10)
            except subprocess.TimeoutExpired:
                process.kill()
                stdout, stderr = process.communicate(timeout=5)
            report.setdefault('processes', []).append({'argv': process.args, 'exit_code': process.returncode,
                'stdout': stdout.decode(), 'stderr': stderr.decode(),
                'stdout_sha256': hashlib.sha256(stdout).hexdigest(), 'stderr_sha256': hashlib.sha256(stderr).hexdigest()})
            process = None

        def start():
            nonlocal process
            with socket.socket() as reservation:
                reservation.bind(('127.0.0.1', 0))
                port = reservation.getsockname()[1]
            process = subprocess.Popen([str(args.binary), 'proxy', 'start', '--port=' + str(port)],
                       cwd=root, env=env, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
            url = 'http://127.0.0.1:' + str(port)
            deadline = time.monotonic() + 15
            while True:
                assert process.poll() is None, 'proxy exited before ready'
                try:
                    opener.open(url + '/health', timeout=1).close()
                    return url
                except (OSError, urllib.error.HTTPError):
                    assert time.monotonic() < deadline, 'proxy readiness timeout'
                    time.sleep(0.1)

        def case(name, expected_boundary, expected_cost, expected_status=200):
            active['name'] = name
            previous = json.loads(usage_file.read_bytes())['models'].get('gpt-4o', {}).get('requests', 0) if usage_file.exists() else 0
            payload = json.dumps({'model': 'gpt-4o', 'stream': name.startswith('stream-'),
                       'messages': [{'role': 'user', 'content': 'Local measurement fixture ' + name}]}).encode()
            request = urllib.request.Request(url + '/v1/chat/completions', data=payload,
                      headers={'Content-Type': 'application/json', 'Authorization': 'Bearer ' + token})
            response_bytes, read_error, status = b'', None, None
            try:
                with opener.open(request, timeout=15) as response:
                    status = response.status
                    try:
                        response_bytes = response.read()
                    except (http.client.IncompleteRead, OSError) as error:
                        read_error = type(error).__name__
            except urllib.error.HTTPError as error:
                status, response_bytes = error.code, error.read()
            except (http.client.HTTPException, OSError) as error:
                assert name == 'stream-error', name
                read_error = type(error).__name__
            assert status == expected_status or (name == 'stream-error' and read_error is not None)
            assert read_error is None or name == 'stream-error'
            deadline = time.monotonic() + 5
            while True:
                snapshot = json.loads(usage_file.read_bytes()) if usage_file.exists() else {'models': {}}
                model = snapshot['models'].get('gpt-4o', {})
                if model.get('requests', 0) == previous + 1:
                    break
                assert time.monotonic() < deadline, (name, snapshot)
                time.sleep(0.05)
            observation = model['response_observations'][-1]
            assert set(observation) == {'schema_version', 'http_status', 'usage_observed_ms', 'boundary', 'provider_cost_usd'}
            assert observation['schema_version'] == 1 and observation['http_status'] == expected_status
            assert observation['boundary'] == expected_boundary, observation
            assert observation['provider_cost_usd'] == expected_cost, observation
            assert 60 <= observation['usage_observed_ms'] < 15000, observation
            if name == 'unknown-cost':
                assert len(model['response_observations']) == 1, 'legacy usage file starts with no history'
            cases.append({'name': name, 'observation': observation, 'meter': model,
                          'response_sha256': hashlib.sha256(response_bytes).hexdigest(),
                          'client_http_status': status, 'read_error': read_error})
            retain()

        try:
            url = start()
            for name, cost in [('unknown-cost', None), ('measured-cost', 0.125), ('header-cost', 0.25), ('body-beats-header', 0.125)]:
                case(name, 'body_end', cost)
            case('http-error', 'body_end', None, 400)
            case('stream-terminal', 'provider_terminal_usage', None)
            case('stream-failed-terminal', 'provider_terminal_usage', None)
            case('stream-partial', 'stream_end', None)
            case('stream-error', 'stream_error', None)
            stop()
            before = json.loads(usage_file.read_bytes())['models']['gpt-4o']['response_observations']
            url = start()
            case('restart-preserves-history', 'body_end', None)
            after = json.loads(usage_file.read_bytes())['models']['gpt-4o']['response_observations']
            assert after[:-1] == before
            for index in range(25):
                case('bounded-history-' + str(index), 'body_end', None)
            assert len(json.loads(usage_file.read_bytes())['models']['gpt-4o']['response_observations']) == 32
            assert len(requests) == len(cases), 'exactly one provider request and one usage record per case'
            report['passed'] = True
        finally:
            stop()
            server.shutdown()
            server.server_close()
            thread.join(timeout=5)
            retain()
    print(json.dumps({'passed': True, 'cases': len(cases), 'report_sha256': hashlib.sha256(args.report.read_bytes()).hexdigest()}))


if __name__ == '__main__':
    main()
