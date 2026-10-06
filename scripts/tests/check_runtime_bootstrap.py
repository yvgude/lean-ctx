# SPDX-License-Identifier: Apache-2.0
"""Build one staging host and exercise its bundled trust/consent path, not a full suite."""
import argparse
import base64
import hashlib
from http.server import BaseHTTPRequestHandler, HTTPServer
import json
import os
from pathlib import Path
import platform
import pty
import subprocess
import sys
import tempfile
import threading
import time
import tomllib


def sha(raw):
    return hashlib.sha256(raw).hexdigest()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ('repo', 'target-dir', 'binary', 'archive', 'manifest', 'signature',
                 'trust-root', 'catalog-signer', 'openssl', 'report'):
        parser.add_argument('--' + name, type=Path, required=True)
    for name in ('manifest-sha256', 'trust-root-sha256'):
        parser.add_argument('--' + name, required=True)
    args = parser.parse_args()
    assert not args.binary.exists(), 'never overwrite a retained binary'
    raw_manifest, public_pem = args.manifest.read_bytes(), args.trust_root.read_bytes()
    assert sha(raw_manifest) == args.manifest_sha256
    assert sha(public_pem) == args.trust_root_sha256
    manifest = json.loads(raw_manifest)
    assert sha(args.archive.read_bytes()) == manifest['artifact_sha256']
    der = base64.b64decode(''.join(public_pem.decode().splitlines()[1:-1]), validate=True)
    assert len(der) == 44 and der[:12].hex() == '302a300506032b6570032100'
    names = ['LEANCTX_STAGING_RUNTIME_CHANNEL_' + suffix
             for suffix in ('URL', 'SIGNATURE_URL', 'ROOT_KEY_HEX')]
    clean_env = {k: v for k, v in os.environ.items() if k not in names}
    report = {'schema': 'leanctx.runtime-bootstrap-component/v1', 'passed': False,
              'user_acceptance': False, 'commands': [], 'requests': [],
              'driver_sha256': sha(Path(__file__).read_bytes()),
              'manifest_sha256': args.manifest_sha256,
              'archive_sha256': manifest['artifact_sha256'],
              'signer_sha256': sha(args.catalog_signer.read_bytes())}
    with args.report.open('x') as output:
        def retain():
            output.seek(0)
            output.write(json.dumps(report, indent=2, sort_keys=True) + '\n')
            output.truncate()
            output.flush()

        def execute(name, argv, cwd, env=None, expected=0, terminal=None, timeout=30):
            argv = list(map(str, argv))
            print(name, flush=True)
            if terminal is None:
                result = subprocess.run(argv, cwd=cwd, env=env, capture_output=True, timeout=timeout)
            else:
                master, slave = pty.openpty()
                try:
                    with subprocess.Popen(argv, cwd=cwd, env=env, stdin=slave,
                                          stdout=subprocess.PIPE, stderr=subprocess.PIPE) as child:
                        os.write(master, terminal.encode())
                        try:
                            stdout, stderr = child.communicate(timeout=timeout)
                        except subprocess.TimeoutExpired:
                            child.kill()
                            child.communicate()
                            raise
                        result = subprocess.CompletedProcess(argv, child.returncode, stdout, stderr)
                finally:
                    os.close(master)
                    os.close(slave)
            report['commands'].append({'name': name, 'argv': argv, 'cwd': str(cwd),
                'exit_code': result.returncode, 'stdout': result.stdout.decode(),
                'stderr': result.stderr.decode(), 'stdout_sha256': sha(result.stdout),
                'stderr_sha256': sha(result.stderr), 'terminal_input': terminal})
            retain()
            assert result.returncode == expected, name
            return result.stdout

        with tempfile.TemporaryDirectory(prefix='leanctx-bootstrap-') as temporary:
            root = Path(temporary).resolve()
            key, public = root / 'key.pem', root / 'public.der'
            key.touch(mode=0o600)
            execute('generate-test-root', [args.openssl, 'genpkey', '-algorithm', 'ED25519', '-out', key], root)
            execute('export-test-root', [args.openssl, 'pkey', '-in', key,
                    '-pubout', '-outform', 'DER', '-out', public], root)
            public_bytes = public.read_bytes()
            assert len(public_bytes) == 44 and public_bytes[:12] == der[:12]
            root_hex = public_bytes[-32:].hex()
            payloads = {'/archive': args.archive.read_bytes(), '/manifest': raw_manifest,
                        '/signature': args.signature.read_bytes()}

            class Handler(BaseHTTPRequestHandler):
                def do_GET(self):
                    report['requests'].append(self.path)
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
                    catalog = {'schema': 'leanctx.runtime-channel/v1', 'channel': 'staging', 'sequence': 1,
                        'expires_unix_ms': time.time_ns() // 1000000 + 3600000,
                        'releases': [{'target': target, 'manifest_sha256': args.manifest_sha256,
                            'artifact_key_hex': der[-32:].hex(), 'archive_url': base + '/archive',
                            'manifest_url': base + '/manifest', 'signature_url': base + '/signature'}]}
                    source, signed = root / 'catalog.json', root / 'signed'
                    source.write_text(json.dumps(catalog))
                    execute('sign-with-product-producer', [sys.executable, '-B', args.catalog_signer,
                        '--staging', '--catalog', source, '--private-key', key,
                        '--key-id', sha(public_bytes[-32:]), '--openssl', args.openssl, '--output', signed], root)
                    payloads['/catalog'] = (signed / 'manifest.json').read_bytes()
                    payloads['/catalog.sig'] = (signed / 'manifest.sig').read_bytes()
                    policy = dict(zip(names, (base + '/catalog', base + '/catalog.sig', root_hex)))
                    report['build_policy'] = policy
                    build_env = dict(clean_env, **policy, CARGO_TARGET_DIR=str(args.target_dir))
                    execute('scoped-library-lint', ['cargo', 'clippy', '--locked', '--lib', '--',
                        '-D', 'warnings', '-A', 'clippy::too_many_lines'], args.repo / 'rust', build_env, timeout=900)
                    execute('scoped-host-build', ['cargo', 'build', '--locked', '--bin', 'lean-ctx'],
                            args.repo / 'rust', build_env, timeout=900)
                    with args.binary.open('xb') as binary:
                        binary.write((args.target_dir / 'debug' / 'lean-ctx').read_bytes())
                    args.binary.chmod(0o500)
                    report['host_binary_sha256'] = sha(args.binary.read_bytes())
                    check_setup(args, root, clean_env, names, policy, report, execute)
                    report['passed'] = True
                    retain()
                finally:
                    server.shutdown()
                    thread.join(timeout=5)
    print(json.dumps({'passed': True, 'commands': len(report['commands']), 'report': str(args.report)}))


def check_setup(args, root, clean_env, names, policy, report, execute):
    home, config_dir, data = root / 'home', root / 'config', root / 'data' / 'nested'
    home.mkdir(mode=0o700)
    env = {k: v for k, v in clean_env.items() if not k.startswith('LEAN_CTX_')}
    env.update(HOME=str(home), LEAN_CTX_CONFIG_DIR=str(config_dir), LEAN_CTX_DATA_DIR=str(data),
               XDG_CONFIG_HOME=str(root / 'xdg-config'), XDG_DATA_HOME=str(root / 'xdg-data'),
               XDG_STATE_HOME=str(root / 'state'), XDG_CACHE_HOME=str(root / 'cache'), DO_NOT_TRACK='1')
    # Runtime environment must not replace build-time trust metadata.
    env.update(dict(zip(names, ('http://127.0.0.1:1/evil', 'http://127.0.0.1:1/evil.sig', '0' * 64))))
    config, installed = config_dir / 'config.toml', data / 'intelligence-runtime'

    def cli(name, command, expected=0, terminal=None):
        return execute(name, [args.binary, 'setup', 'runtime', *command, '--staging'],
                       root, env, expected=expected, terminal=terminal)

    assert json.loads(cli('bundled-discovery', ['status']))['status'] == 'download_available'
    assert not installed.exists() and not config.exists() and not report['requests']
    cli('no-terminal-no-consent', ['configure'], expected=2)
    declined = cli('default-no', ['configure'], terminal='\n').decode()
    assert 'Not installed' in declined and 'proprietary' in declined
    cli('explicit-consent-required', ['sync-configured'], expected=2)
    assert not installed.exists() and not config.exists() and not report['requests']
    config_dir.mkdir(mode=0o700, exist_ok=True)
    config.write_text('invalid = [\n')
    cli('corrupt-global-is-not-absence', ['status'], expected=2)
    cli('corrupt-global-blocks-download', ['sync-configured', '--accept-proprietary'], expected=2)
    assert config.read_text() == 'invalid = [\n' and not report['requests']
    config.unlink()  # Restore only this driver's deliberately invalid fixture.
    accepted = cli('fresh-bundled-install', ['configure'], terminal='yes\n').decode()
    assert 'Installed and enabled after signature and compatibility checks.' in accepted
    selected = tomllib.loads(config.read_text())['intelligence_runtime']
    assert selected['enabled'] and selected['accept_proprietary']
    assert selected['channel_root_key_hex'] == policy[names[2]]
    assert selected['channel_url'] == policy[names[0]]
    assert selected['manifest_sha256'] == args.manifest_sha256
    assert Path(selected['root']) == installed and installed.stat().st_mode & 0o777 == 0o700
    assert all(p.stat().st_mode & 0o777 == 0o700 for p in (data, data.parent))
    assert home.stat().st_mode & 0o777 == 0o700
    before, requests = config.read_bytes(), len(report['requests'])
    assert json.loads(cli('installed-status', ['status']))['status'] == 'configured'
    assert len(report['requests']) == requests
    repeated = json.loads(cli('bundled-retry-idempotent', ['sync-configured', '--accept-proprietary']))
    assert repeated['installation']['status'] == 'already_installed'
    assert repeated['activation']['health']['status'] == 'healthy' and config.read_bytes() == before
    cli('deactivate-keeps-pinned-channel', ['deactivate'])
    selected = tomllib.loads(config.read_text())['intelligence_runtime']
    assert not selected['enabled'] and selected['channel_root_key_hex'] == policy[names[2]]


if __name__ == '__main__':
    main()
