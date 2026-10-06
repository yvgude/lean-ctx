# SPDX-License-Identifier: Apache-2.0
"""Serve real signed bytes over loopback to the built staging installer."""
from http.server import BaseHTTPRequestHandler, HTTPServer
import threading


def check_channel(run, root, common, args):
    payloads = {'/archive': args.archive.read_bytes(), '/manifest': args.manifest.read_bytes(),
                '/signature': args.signature.read_bytes(), '/oversize-manifest': b' ' * 65537,
                '/oversize-signature': b'x' * 65, '/tampered-archive': b'changed',
                '/tampered-manifest': b'{}', '/tampered-signature': b'x' * 64}
    requests = []

    class Handler(BaseHTTPRequestHandler):
        def do_GET(self):
            requests.append(self.path)
            body = payloads.get(self.path, b'')
            self.send_response(302 if self.path == '/redirect' else 200 if self.path in payloads else 503)
            self.send_header('Content-Length', str(len(body)))
            if self.path == '/redirect':
                self.send_header('Location', '/manifest')
            self.end_headers()
            self.wfile.write(body)

        def log_message(self, *_):
            pass

    with HTTPServer(('127.0.0.1', 0), Handler) as server:
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        try:
            base = 'http://127.0.0.1:' + str(server.server_port)
            state = root / 'downloaded-runtime'
            state.mkdir(mode=0o700)
            command = ['download', *common, '--root', str(state), '--expected-active', 'none',
                       '--accept-proprietary', '--archive-url', base + '/archive',
                       '--manifest-url', base + '/manifest', '--signature-url', base + '/signature']
            run('download-needs-consent', [v for v in command if v != '--accept-proprietary'], False)
            run('download-needs-staging', [v for v in command if v != '--staging'], False)
            assert not requests and not list(state.iterdir())
            for url in ('http://example.invalid/archive', 'http://localhost/archive',
                        base + '/archive?query=value', base + '/archive#fragment',
                        'https://user:password@example.invalid/archive', 'file:///archive'):
                changed = command.copy()
                changed[changed.index('--archive-url') + 1] = url
                run('download-rejects-url-' + str(len(url)), changed, False)
                assert not requests and not list(state.iterdir())
            for flag, path, expected in (
                ('--manifest-url', '/redirect', ['/redirect']),
                ('--manifest-url', '/unavailable', ['/unavailable']),
                ('--manifest-url', '/oversize-manifest', ['/oversize-manifest']),
                ('--manifest-url', '/tampered-manifest', ['/tampered-manifest', '/signature']),
                ('--signature-url', '/oversize-signature', ['/manifest', '/oversize-signature']),
                ('--signature-url', '/tampered-signature', ['/manifest', '/tampered-signature']),
                ('--archive-url', '/tampered-archive', ['/manifest', '/signature', '/tampered-archive']),
            ):
                changed = command.copy()
                changed[changed.index(flag) + 1] = base + path
                offset = len(requests)
                run('download-rejects-' + path[1:], changed, False)
                assert requests[offset:] == expected and not list(state.iterdir())
            offset = len(requests)
            installed = run('download-first-install', command, True, prefix=('setup', 'runtime'))
            assert installed['status'] == 'installed' and not installed['runtime_started']
            assert requests[offset:] == ['/manifest', '/signature', '/archive']
            selected = (state / 'selection.json').read_bytes()
            command[command.index('--expected-active') + 1] = args.manifest_sha256
            assert run('download-idempotent', command, True)['status'] == 'already_installed'
            assert (state / 'selection.json').read_bytes() == selected
            activated = run('download-then-activate', ['activate', *common, '--root', str(state),
                '--accept-proprietary', '--expected-active', args.manifest_sha256], True,
                prefix=('setup', 'runtime'))
            assert activated['status'] == 'activated' and activated['health']['status'] == 'healthy'
            run('download-deactivate', ['deactivate', '--staging'], True, prefix=('setup', 'runtime'))
            return requests
        finally:
            server.shutdown()
            thread.join(timeout=5)
            assert not thread.is_alive()
