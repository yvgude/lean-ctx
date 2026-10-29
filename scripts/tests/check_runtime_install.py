# SPDX-License-Identifier: Apache-2.0
"""Exercise the built public CLI against an independently selected signed package.

No full suite or installed-user mutation. Optional --health runs the private
runtime's bounded description through the public host, not a service or journey.
Package trust inputs must be independently provisioned. The optional catalog
check creates and deletes an isolated test root, never installing system trust.
"""
import argparse
import base64
import hashlib
import json
import os
from pathlib import Path
import subprocess
import tempfile
import time


def sha(data):
    return hashlib.sha256(data).hexdigest()


def isolated_environment(root, runtime_license_config=None):
    # Host pins and provider credentials must not escape into an installation
    # test. Inherited LEAN_CTX_CONFIG_DIR otherwise overrides the temporary HOME.
    env = {key: value for key, value in os.environ.items()
           if key in ('PATH', 'TMPDIR', 'TMP', 'TEMP', 'SystemRoot', 'SYSTEMROOT', 'WINDIR')}
    env.update(HOME=str(root / 'home'), XDG_CONFIG_HOME=str(root / 'config'),
               XDG_DATA_HOME=str(root / 'data'), XDG_STATE_HOME=str(root / 'state'),
               XDG_CACHE_HOME=str(root / 'cache'), LEAN_CTX_DATA_DIR=str(root / 'data'),
               LEAN_CTX_CONFIG_DIR=str(root / 'data'), LEAN_CTX_STATE_DIR=str(root / 'data'),
               LEAN_CTX_CACHE_DIR=str(root / 'data'), LEAN_CTX_PROJECT_ROOT=str(root),
               DO_NOT_TRACK='1', LEAN_CTX_LIVE_PRICING='off',
               NO_PROXY='127.0.0.1,localhost')
    if runtime_license_config is not None:
        assert runtime_license_config.is_absolute() and runtime_license_config.is_file()
        env['LEANCTX_INTELLIGENCE_LICENSE_CONFIG'] = str(runtime_license_config)
    return env


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--health', action='store_true')
    parser.add_argument('--invoke', action='store_true')
    parser.add_argument('--license-cases', type=Path,
                         help='Independent private fixture index; requires --invoke')
    parser.add_argument('--runtime-license-config', type=Path,
                        help='Explicit disposable license fixture; never inherited from the host')
    parser.add_argument('--select', action='store_true')
    parser.add_argument('--configure', action='store_true')
    parser.add_argument('--channel', action='store_true')
    parser.add_argument('--catalog', action='store_true')
    parser.add_argument('--catalog-setup', action='store_true')
    parser.add_argument('--catalog-rotation', action='store_true')
    parser.add_argument('--previous-host', type=Path)
    parser.add_argument('--previous-host-sha256')
    parser.add_argument('--catalog-signer', type=Path)
    parser.add_argument('--openssl', type=Path)
    parser.add_argument('--proxy', action='store_true')
    parser.add_argument('--proxy-scope', action='store_true')
    parser.add_argument('--outcomes', action='store_true')
    for name in ('binary', 'archive', 'manifest', 'signature', 'trust-root', 'report'):
        parser.add_argument('--' + name, type=Path, required=True)
    for name in ('binary-sha256', 'manifest-sha256', 'trust-root-sha256'):
        parser.add_argument('--' + name, required=True)
    args = parser.parse_args()
    assert args.license_cases is None or args.invoke
    assert not (args.catalog_rotation and args.catalog_setup), 'select one focused catalog scenario'
    if args.previous_host:
        assert sha(args.previous_host.read_bytes()) == args.previous_host_sha256
    args.catalog = args.catalog or args.catalog_setup or args.catalog_rotation
    args.configure = args.configure or args.proxy
    args.health = args.health or args.invoke or args.outcomes
    assert sha(args.binary.read_bytes()) == args.binary_sha256
    manifest_raw = args.manifest.read_bytes()
    assert sha(manifest_raw) == args.manifest_sha256
    public_pem = args.trust_root.read_bytes()
    assert sha(public_pem) == args.trust_root_sha256
    pem_lines = public_pem.decode().splitlines()
    assert pem_lines[0] == '-----BEGIN PUBLIC KEY-----'
    assert pem_lines[-1] == '-----END PUBLIC KEY-----'
    der = base64.b64decode(''.join(pem_lines[1:-1]), validate=True)
    assert len(der) == 44 and der[:12].hex() == '302a300506032b6570032100'
    manifest = json.loads(manifest_raw)
    assert sha(args.archive.read_bytes()) == manifest['artifact_sha256']
    records = []
    report_file = args.report.open('x')

    def retain(value):
        report_file.seek(0)
        json.dump(value, report_file, indent=2, sort_keys=True)
        report_file.write('\n')
        report_file.truncate()
        report_file.flush()

    with tempfile.TemporaryDirectory(prefix='leanctx-runtime-install-') as temporary:
        root = Path(temporary).resolve()
        state = root / 'runtime'
        state.mkdir(mode=0o700)
        home = root / 'home'
        home.mkdir(mode=0o700)
        marker = state / 'unrelated-user-state'
        marker.write_bytes(b'component-state-must-survive')
        env = isolated_environment(root, args.runtime_license_config)
        if args.catalog_setup or args.catalog_rotation:
            env['LEAN_CTX_CONFIG_DIR'] = str(root / 'config' / 'lean-ctx')
            env['DO_NOT_TRACK'] = '1'
            env.pop('LEAN_CTX_CONFIG_PROFILE', None)
        common = ['--staging', '--manifest-sha256', args.manifest_sha256,
                  '--trust-key-hex', der[-32:].hex()]
        inputs = ['--archive', str(args.archive), '--manifest', str(args.manifest),
                  '--signature', str(args.signature)]
        target = ['--root', str(state), '--accept-proprietary']

        def run(name, arguments, success, prefix=('engine', 'runtime'), json_output=True,
                tty_input=None, expected_error=None):
            argv = [str(args.binary), *prefix, *arguments]
            if tty_input is None:
                result = subprocess.run(argv, cwd=root, env=env, capture_output=True, timeout=30)
            else:
                import pty  # Unix-only interactive cases; other driver paths stay importable.
                master, slave = pty.openpty()
                try:
                    with subprocess.Popen(argv, cwd=root, env=env, stdin=slave,
                                          stdout=subprocess.PIPE, stderr=subprocess.PIPE) as child:
                        os.write(master, tty_input.encode())
                        try:
                            stdout, stderr = child.communicate(timeout=30)
                        except subprocess.TimeoutExpired:
                            child.kill()
                            child.communicate()
                            raise
                        result = subprocess.CompletedProcess(argv, child.returncode, stdout, stderr)
                finally:
                    os.close(master)
                    os.close(slave)
            records.append({'name': name, 'argv': argv, 'exit_code': result.returncode,
                            'stdout': result.stdout.decode(), 'stderr': result.stderr.decode(),
                            'stdout_sha256': sha(result.stdout), 'stderr_sha256': sha(result.stderr)})
            if tty_input is not None:
                records[-1]['terminal_input'] = tty_input
            if '--request' in arguments:
                payload = Path(arguments[arguments.index('--request') + 1]).read_bytes()
                records[-1].update(request=json.loads(payload), request_sha256=sha(payload))
            retain({'passed': False, 'user_acceptance': False, 'commands': records,
                    'host_binary_sha256': args.binary_sha256})
            assert result.returncode == (0 if success else 2), name
            if expected_error is not None:
                assert result.stdout == b'', name
                assert result.stderr.decode().splitlines() == [expected_error], name
            return (json.loads(result.stdout) if json_output else result.stdout.decode()) if success else None

        result = run('verify', ['verify', *common, *inputs], True)
        binary_digest = result['receipt']['binary_sha256']
        assert result['release_approved'] is False and result['runtime_started'] is False
        wrong_key = common.copy()
        wrong_key[-1] = ('0' if wrong_key[-1][0] != '0' else '1') + wrong_key[-1][1:]
        run('wrong-independent-key', ['verify', *wrong_key, *inputs], False)
        run('consent-required', ['install', *common, *inputs, '--root', str(state),
                                 '--expected-active', 'none'], False)
        assert not (state / 'selection.json').exists()
        result = run('install', ['install', *common, *inputs, *target, '--expected-active', 'none'], True)
        assert result['status'] == 'installed'
        installed = state / 'packages' / args.manifest_sha256 / 'leanctx-intelligence'
        assert sha(installed.read_bytes()) == binary_digest
        assert installed.stat().st_mode & 0o777 == 0o500
        selection = (state / 'selection.json').read_bytes()
        result = run('idempotent', ['install', *common, *inputs, *target,
                                    '--expected-active', args.manifest_sha256], True)
        assert result['status'] == 'already_installed'
        run('stale-selection', ['install', *common, *inputs, *target, '--expected-active', 'none'], False)
        bad = root / 'changed.sig'
        raw = bytearray(args.signature.read_bytes())
        raw[0] ^= 1
        bad.write_bytes(raw)
        changed = inputs.copy()
        changed[-1] = str(bad)
        run('changed-signature', ['install', *common, *changed, *target,
                                  '--expected-active', args.manifest_sha256], False)
        if args.health:
            health = ['health', *common, *target, '--expected-active', args.manifest_sha256]
            result = run('authenticated-native-health', health, True)
            runtime_engine_version = result['receipt']['version']
            capabilities = result['description']['capabilities']
            context_versions = [capability.get('version') for capability in capabilities
                                if capability.get('id') == 'pro.runtime.adaptive_context']
            assert len(context_versions) <= 1, 'duplicate adaptive context capability'
            context_capability_version = context_versions[0] if context_versions else None
            if args.invoke or args.outcomes:
                assert context_capability_version == '1.0.0', (
                    'runtime invocation checks require the advertised adaptive context 1.0.0 capability')
                assert isinstance(runtime_engine_version, str) and runtime_engine_version
            assert result['status'] == 'healthy' and result['runtime_started'] is True
            assert result['service_running'] is False and result['release_approved'] is False
            assert result['receipt']['binary_sha256'] == binary_digest
            run('health-wrong-key', ['health', *wrong_key, *target,
                                    '--expected-active', args.manifest_sha256], False)
            run('health-stale-selection', [*health[:-1], 'none'], False)
            run('health-consent-required', ['health', *common, '--root', str(state),
                                           '--expected-active', args.manifest_sha256], False)
            original = installed.read_bytes()
            installed.chmod(0o600)
            installed.write_bytes(original + b'corrupt')
            run('health-corrupt-installed-binary', health, False)
            installed.write_bytes(original)
            installed.chmod(0o500)
            retained_signature = installed.parent / 'manifest.sig'
            retained_signature.chmod(0o600)
            retained_signature.write_bytes(bad.read_bytes())
            run('health-corrupt-retained-signature', health, False)
            retained_signature.write_bytes(args.signature.read_bytes())
            retained_signature.chmod(0o400)
            forged = json.loads(selection)
            forged['active']['version'] = '999.0.0'
            (state / 'selection.json').write_text(json.dumps(forged))
            run('health-forged-selection-receipt', health, False)
            (state / 'selection.json').write_bytes(selection)
            run('health-recovered', health, True)
        if args.invoke:
            from check_runtime_outcomes import (
                INVOCATION_ADMISSION_ERROR, assert_context_result, check_retired_routing,
                context_projection,
            )
            request_file = root / 'request.json'
            projection = json.dumps(context_projection())
            base = {'protocol_version': 'leanctx.runtime-exchange/v1', 'session_id': 'component-session',
                'request_id': 'component-request', 'sequence': 1, 'input': projection,
                'invocation': {'schema_version': 1, 'invocation_id': 'component-invocation',
                    'engine': {'engine_id': 'leanctx-intelligence', 'engine_version': runtime_engine_version},
                    'operation': {'capability_id': 'pro.runtime.adaptive_context',
                                  'capability_version': context_capability_version},
                    'input_ref': 'projection:component', 'input_digest': 'sha256:' + sha(projection.encode()),
                    'source_refs': ['projection:component'],
                    'policy_admission': {'policy_ref': 'policy:component', 'decision': 'admitted'}}}
            mismatched_engine = '0.0.1' if runtime_engine_version != '0.0.1' else '0.0.2'
            for name in ('invoke', 'expired', 'wrong-engine', 'wrong-capability', 'sequence',
                         'denied', 'input-digest', 'invalid-context', 'invoke-again'):
                request = json.loads(json.dumps(base))
                request['deadline_unix_ms'] = time.time_ns() // 1_000_000 + 5000
                invocation = request['invocation']
                if name == 'expired':
                    request['deadline_unix_ms'] -= 6000
                elif name == 'wrong-engine':
                    invocation['engine']['engine_version'] = mismatched_engine
                elif name == 'wrong-capability':
                    invocation['operation']['capability_version'] = '2.0.0'
                elif name == 'sequence':
                    request['sequence'] = 2
                elif name == 'denied':
                    invocation['policy_admission']['decision'] = 'rejected'
                elif name == 'input-digest':
                    request['input'] += 'changed'
                elif name == 'invalid-context':
                    request['input'] = '{}'
                    invocation['input_digest'] = 'sha256:' + sha(b'{}')
                request_file.write_text(json.dumps(request))
                success = name in ('invoke', 'invoke-again')
                expected_error = (INVOCATION_ADMISSION_ERROR
                                  if name in ('wrong-engine', 'wrong-capability') else None)
                result = run(name, ['invoke', *common, *target, '--expected-active', args.manifest_sha256,
                                    '--request', str(request_file)], success,
                             expected_error=expected_error)
                if success:
                    assert_context_result(result, invocation, len(context_projection()['candidates']))
                    if name == 'invoke':
                        check_retired_routing(run, root, common, target, args.manifest_sha256,
                                              runtime_engine_version)
            if args.license_cases is not None:
                cases = json.loads(args.license_cases.read_bytes())
                assert isinstance(cases, list) and 1 <= len(cases) <= 32
                original_config = env.get('LEANCTX_INTELLIGENCE_LICENSE_CONFIG')
                for case in cases:
                    assert set(case) == {'name', 'configuration', 'success'}
                    assert isinstance(case['success'], bool)
                    if case['configuration'] is None:
                        env.pop('LEANCTX_INTELLIGENCE_LICENSE_CONFIG', None)
                    else:
                        configuration = Path(case['configuration'])
                        assert configuration.is_absolute() and configuration.is_file()
                        env['LEANCTX_INTELLIGENCE_LICENSE_CONFIG'] = str(configuration)
                    request = json.loads(json.dumps(base))
                    request['deadline_unix_ms'] = time.time_ns() // 1_000_000 + 5000
                    request_file.write_text(json.dumps(request))
                    result = run('license-' + case['name'],
                                 ['invoke', *common, *target, '--expected-active',
                                  args.manifest_sha256, '--request', str(request_file)],
                                 case['success'])
                    if case['success']:
                        assert_context_result(result, request['invocation'],
                                              len(context_projection()['candidates']))
                    else:
                        stderr = records[-1]['stderr']
                        assert records[-1]['stdout'] == '' and stderr.startswith('engine runtime: ')
                        assert stderr.strip() != INVOCATION_ADMISSION_ERROR
                if original_config is None:
                    env.pop('LEANCTX_INTELLIGENCE_LICENSE_CONFIG', None)
                else:
                    env['LEANCTX_INTELLIGENCE_LICENSE_CONFIG'] = original_config
        if args.select:
            from check_runtime_ranking import check_retired_selection
            check_retired_selection(run)
        if args.outcomes:
            from check_runtime_outcomes import check_outcomes
            check_outcomes(run, root, common, target, args.manifest_sha256,
                           context_capability_version, runtime_engine_version)
        if args.configure:
            from check_runtime_configuration import check_configuration
            env['LEAN_CTX_PROJECT_ROOT'] = str(root)
            env.pop('LEAN_CTX_CONFIG_PROFILE', None)
            check_configuration(run, root, installed, common, target, args.manifest_sha256)
        if args.proxy or args.proxy_scope:
            from check_runtime_proxy import check_proxy
            def record_proxy(value):
                records.append(value)
                retain({'passed': False, 'user_acceptance': False, 'commands': records,
                        'host_binary_sha256': args.binary_sha256})
            check_proxy(run, record_proxy, root, installed, common, target,
                        args.manifest_sha256, args.binary, env, scope_only=args.proxy_scope)
        channel_requests = []
        if args.channel:
            from check_runtime_channel import check_channel
            channel_requests = check_channel(run, root, common, args)
        catalog_requests = []
        if args.catalog:
            from check_runtime_catalog import check_catalog
            assert args.catalog_signer and args.openssl
            def record_catalog(value):
                records.append(value)
                retain({'passed': False, 'user_acceptance': False, 'commands': records,
                        'host_binary_sha256': args.binary_sha256})
            catalog_requests = check_catalog(run, record_catalog, root, common, args)
        assert (state / 'selection.json').read_bytes() == selection
        assert marker.read_bytes() == b'component-state-must-survive'
        assert sha(installed.read_bytes()) == binary_digest
        report = {'schema': 'leanctx.runtime-install-component/v1', 'passed': True,
                  'user_acceptance': False, 'runtime_started': args.health or args.configure or args.proxy_scope or args.channel or args.catalog_setup,
                  'runtime_invoked': args.invoke or args.outcomes,
                  'runtime_outcomes_checked': args.outcomes,
                  'runtime_selection_retired_checked': args.select,
                  'runtime_configuration_checked': args.configure,
                  'runtime_channel_requests': channel_requests,
                  'runtime_catalog_requests': catalog_requests,
                  'runtime_proxy_checked': args.proxy,
                  'runtime_proxy_scope_checked': args.proxy_scope,
                  'driver_sha256': sha(Path(__file__).read_bytes()),
                  'host_binary_sha256': args.binary_sha256,
                  'installed_runtime_sha256': binary_digest,
                  'manifest_sha256': args.manifest_sha256,
                  'archive_sha256': manifest['artifact_sha256'],
                  'trust_root_sha256': args.trust_root_sha256,
                  'unrelated_state_preserved': True, 'commands': records}
    retain(report)
    report_file.close()
    print(json.dumps({'passed': True, 'commands': len(records), 'report': str(args.report),
                      'user_acceptance': False}, sort_keys=True))


if __name__ == '__main__':
    main()
