# SPDX-License-Identifier: Apache-2.0
"""Built-host signed channel checks with real staging crypto and package bytes."""
import base64
import hashlib
from http.server import BaseHTTPRequestHandler, HTTPServer
import json
import platform
import subprocess
import sys
import threading
import time


def check_catalog(run, record, root, common, args):
    sha = lambda raw: hashlib.sha256(raw).hexdigest()

    def execute(name, argv, expected=0):
        argv = list(map(str, argv))
        result = subprocess.run(argv, capture_output=True, timeout=30)
        record({'name': name, 'argv': argv, 'exit_code': result.returncode,
                'stdout': result.stdout.decode(), 'stderr': result.stderr.decode(),
                'stdout_sha256': sha(result.stdout), 'stderr_sha256': sha(result.stderr)})
        assert result.returncode == expected, name
        return result

    key, public = root / 'channel-key.pem', root / 'channel-public.pem'
    key.touch(mode=0o600)
    execute('generate-channel-test-key', [args.openssl, 'genpkey', '-algorithm', 'ED25519', '-out', key])
    execute('export-channel-public-key', [args.openssl, 'pkey', '-in', key, '-pubout', '-out', public])
    der = base64.b64decode(''.join(public.read_text().splitlines()[1:-1]), validate=True)
    assert len(der) == 44 and der[:12].hex() == '302a300506032b6570032100'
    key_hex, key_id = der[-32:].hex(), sha(der[-32:])
    artifact_signer = args.catalog_signer.parent / 'sign_free_release.py'
    sources = {str(path): sha(path.read_bytes()) for path in (args.catalog_signer, artifact_signer, args.openssl)}
    candidate, artifact_signed = args.archive.parent, root / 'artifact-signed'
    rollback = candidate / 'bundle' / 'leanctx-intelligence'
    execute('artifact-signing-regression', [sys.executable, '-B', artifact_signer, '--staging',
        '--candidate', candidate, '--build-receipt-sha256', sha((candidate / 'BUILD-RECEIPT.json').read_bytes()),
        '--rollback', rollback, '--rollback-sha256', sha(rollback.read_bytes()),
        '--private-key', key, '--key-id', key_id, '--openssl', args.openssl, '--output', artifact_signed])
    run('verify-refactored-artifact-signature', ['verify', '--staging', '--manifest-sha256',
        sha((artifact_signed / 'manifest.json').read_bytes()), '--trust-key-hex', key_hex,
        '--archive', str(args.archive), '--manifest', str(artifact_signed / 'manifest.json'),
        '--signature', str(artifact_signed / 'manifest.sig')], True)
    payloads = {'/archive': args.archive.read_bytes(), '/manifest': args.manifest.read_bytes(),
                '/signature': args.signature.read_bytes()}
    requests = []
    request_effects = []

    class Handler(BaseHTTPRequestHandler):
        def do_GET(self):
            requests.append(self.path)
            for effect in request_effects:
                effect(self.path)
            body = payloads.get(self.path, b'')
            self.send_response(200 if self.path in payloads else 404)
            self.send_header('Content-Length', str(len(body)))
            self.end_headers()
            self.wfile.write(body)

        def log_message(self, *_):
            pass

    with HTTPServer(('127.0.0.1', 0), Handler) as server:
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        try:
            base = 'http://127.0.0.1:' + str(server.server_port)
            target = {('arm64', 'darwin'): 'aarch64-apple-darwin',
                      ('x86_64', 'darwin'): 'x86_64-apple-darwin',
                      ('aarch64', 'linux'): 'aarch64-unknown-linux-gnu',
                      ('x86_64', 'linux'): 'x86_64-unknown-linux-gnu'}[(platform.machine(), sys.platform)]
            catalog = {'schema': 'leanctx.runtime-channel/v1', 'channel': 'staging', 'sequence': 2,
                'expires_unix_ms': time.time_ns() // 1000000 + 3600000, 'releases': [{'target': target,
                'manifest_sha256': args.manifest_sha256, 'artifact_key_hex': common[-1],
                'archive_url': base + '/archive', 'manifest_url': base + '/manifest',
                'signature_url': base + '/signature'}]}
            source = root / 'catalog.json'
            source.write_text(json.dumps(catalog))
            signed = root / 'channel-signed'
            signer = [sys.executable, '-B', args.catalog_signer, '--staging', '--catalog', source,
                      '--private-key', key, '--key-id', key_id, '--openssl', args.openssl, '--output', signed]
            execute('sign-channel-with-product-producer', signer)
            payloads['/catalog'] = (signed / 'manifest.json').read_bytes()
            payloads['/catalog.sig'] = (signed / 'manifest.sig').read_bytes()
            good = (payloads['/catalog'], payloads['/catalog.sig'])
            state = root / 'channel-runtime'
            state.mkdir(mode=0o700)
            command = ['sync', '--staging', '--accept-proprietary', '--trust-key-hex', key_hex,
                '--root', str(state), '--expected-active', 'none', '--channel-url', base + '/catalog',
                '--channel-signature-url', base + '/catalog.sig']
            if args.catalog_rotation:
                from check_runtime_rotation import check_rotation
                run('rotation-base-channel', command, True)
                command[command.index('--expected-active') + 1] = args.manifest_sha256
                other, public = root / 'rotation-next.pem', root / 'rotation-next.der'
                other.touch(mode=0o600)
                execute('generate-rotation-successor', [args.openssl, 'genpkey', '-algorithm', 'ED25519', '-out', other])
                execute('export-rotation-successor', [args.openssl, 'pkey', '-in', other, '-pubout', '-outform', 'DER', '-out', public])
                check_rotation(run, execute, record, root, common, args, command, catalog,
                               key, key_hex, other, public.read_bytes()[-32:].hex(), payloads)
                return {'requests': requests, 'producer_sources': sources, 'root_key_sha256': key_id}
            run('channel-needs-consent', [v for v in command if v != '--accept-proprietary'], False)
            assert not requests
            payloads['/catalog.sig'] = b'x' * 64
            run('channel-rejects-forged-root-signature', command, False)
            assert requests == ['/catalog', '/catalog.sig'] and not list(state.iterdir())
            payloads['/catalog.sig'] = good[1]
            first = run('channel-platform-first-install', command, True)
            assert first['status'] == 'installed' and first['artifact_key_hex'] == common[-1]
            assert first['channel']['sequence'] == 2 and first['channel']['root_key_sha256'] == key_id
            selection = state / 'selection.json'
            assert json.loads(selection.read_bytes())['schema'] == 2
            command[command.index('--expected-active') + 1] = args.manifest_sha256
            before, mtime = selection.read_bytes(), selection.stat().st_mtime_ns
            assert run('channel-idempotent', command, True)['status'] == 'already_installed'
            assert (selection.read_bytes(), selection.stat().st_mtime_ns) == (before, mtime)
            migration = command.copy()
            migration[migration.index('--root') + 1] = str(root / 'runtime')
            legacy_selection = root / 'runtime' / 'selection.json'
            legacy = legacy_selection.read_bytes()
            assert json.loads(legacy)['schema'] == 1
            run('channel-migrates-selection-v1', migration, True)
            upgraded = json.loads(legacy_selection.read_bytes())
            assert upgraded['schema'] == 2 and upgraded['channel']['sequence'] == 2
            assert upgraded['active'] == json.loads(legacy)['active']
            legacy_selection.write_bytes(legacy)  # Restore the driver's unrelated base scenario.

            def publish_fixture(value, domain=b'leanctx-runtime-channel-v1\0'):
                raw = json.dumps(value).encode()
                payload = root / 'adversarial-signing-input'
                signature = root / 'adversarial-signature'
                payload.write_bytes(domain + raw)
                execute('sign-channel-fixture', [args.openssl, 'pkeyutl', '-sign', '-rawin',
                    '-inkey', key, '-in', payload, '-out', signature])
                payloads['/catalog'], payloads['/catalog.sig'] = raw, signature.read_bytes()

            catalog['sequence'] = 3
            publish_fixture(catalog)
            assert run('channel-advance-watermark', command, True)['channel']['sequence'] == 3
            before = selection.read_bytes()
            run('offline-install-preserves-channel-watermark', ['install', *common,
                '--archive', str(args.archive), '--manifest', str(args.manifest),
                '--signature', str(args.signature), '--root', str(state), '--accept-proprietary',
                '--expected-active', args.manifest_sha256], True)
            assert selection.read_bytes() == before
            payloads['/catalog'], payloads['/catalog.sig'] = good
            run('channel-rejects-replay', command, False)
            for name in ('conflict', 'expired', 'far-future', 'production', 'unknown', 'duplicate',
                         'foreign-target', 'wrong-artifact-key', 'wrong-manifest', 'wrong-domain'):
                changed = json.loads(json.dumps(catalog))
                if name == 'conflict': changed['expires_unix_ms'] += 1
                elif name == 'expired': changed['expires_unix_ms'] = 1
                elif name == 'far-future': changed['expires_unix_ms'] += 8 * 86400000
                elif name == 'production': changed['channel'] = 'production'
                elif name == 'unknown': changed['unknown'] = True
                elif name == 'duplicate': changed['releases'] *= 2
                elif name == 'foreign-target': changed['releases'][0]['target'] = 'foreign'
                elif name == 'wrong-artifact-key': changed['releases'][0]['artifact_key_hex'] = key_hex
                elif name == 'wrong-manifest': changed['releases'][0]['manifest_sha256'] = '0' * 64
                publish_fixture(changed, b'leanctx-release-manifest-v1\0' if name == 'wrong-domain'
                                else b'leanctx-runtime-channel-v1\0')
                run('channel-rejects-' + name, command, False)
                assert selection.read_bytes() == before
            source.write_text(json.dumps({**catalog, 'channel': 'production'}))
            execute('producer-rejects-production', signer, 1)
            other, other_public = root / 'other-key.pem', root / 'other-public.der'
            other.touch(mode=0o600)
            execute('generate-unapproved-rotation-key', [args.openssl, 'genpkey', '-algorithm', 'ED25519', '-out', other])
            execute('export-unapproved-rotation-key', [args.openssl, 'pkey', '-in', other,
                '-pubout', '-outform', 'DER', '-out', other_public])
            previous_key, key = key, other
            publish_fixture({**catalog, 'sequence': 4})
            key = previous_key
            rotated = command.copy()
            rotated[rotated.index('--trust-key-hex') + 1] = other_public.read_bytes()[-32:].hex()
            run('channel-rejects-unapproved-root-rotation', rotated, False)
            assert selection.read_bytes() == before
            assert sources == {str(path): sha(path.read_bytes()) for path in (args.catalog_signer, artifact_signer, args.openssl)}
            if args.catalog_setup:
                from check_runtime_catalog_setup import check_catalog_setup
                payloads['/catalog'], payloads['/catalog.sig'] = good
                check_catalog_setup(run, root, common, args, base, key_hex, requests,
                                    payloads, request_effects)
            return {'requests': requests, 'producer_sources': sources, 'root_key_sha256': key_id}
        finally:
            server.shutdown()
            thread.join(timeout=5)
            assert not thread.is_alive()
