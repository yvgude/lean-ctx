# SPDX-License-Identifier: Apache-2.0
"""Focused actual-CLI root transition checks, using the product dual signer."""
import hashlib
import json
import sys
import time


def check_rotation(run, execute, record, root, common, args, sync, catalog,
                   old_key, old_hex, new_key, new_hex, payloads):
    state = root / 'channel-runtime' / 'selection.json'
    original = state.read_bytes()
    signer = args.catalog_signer.parent / 'sign_runtime_root_transition.py'
    signer_digest = hashlib.sha256(signer.read_bytes()).hexdigest()
    next_sequence = catalog['sequence'] + 1
    document = {'schema': 'leanctx.runtime-root-transition/v1', 'channel': 'staging',
                'previous_root_key_hex': old_hex, 'next_root_key_hex': new_hex,
                'minimum_sequence': next_sequence, 'expires_unix_ms': time.time_ns() // 1000000 + 3600000}

    def sign(name, value, previous=old_key, following=new_key, expected=0):
        source, output = root / (name + '.json'), root / name
        source.write_text(json.dumps(value))
        execute(name, [sys.executable, '-B', signer, '--staging', '--document', source,
            '--previous-private-key', previous, '--next-private-key', following,
            '--openssl', args.openssl, '--output', output], expected)
        return output / 'transition.json'

    proof_path = sign('sign-valid-root-transition', document)
    proof = json.loads(proof_path.read_bytes())
    rotate = ['rotate-root', '--staging', '--accept-proprietary', '--root', str(state.parent),
              '--expected-active', args.manifest_sha256, '--trust-key-hex', old_hex,
              '--transition', str(proof_path)]
    run('rotation-requires-consent', [v for v in rotate if v != '--accept-proprietary'], False)
    for field in ('previous_signature', 'next_signature'):
        bad = dict(proof)
        bad[field] = ('0' if bad[field][0] != '0' else '1') + bad[field][1:]
        changed = root / ('bad-' + field + '.json')
        changed.write_text(json.dumps(bad))
        run('rotation-rejects-' + field, [*rotate[:-1], str(changed)], False)
        assert state.read_bytes() == original
    wrong = rotate.copy()
    wrong[wrong.index('--trust-key-hex') + 1] = new_hex
    run('rotation-rejects-replaced-anchor', wrong, False)
    stale = sign('sign-stale-floor', {**document, 'minimum_sequence': catalog['sequence']})
    run('rotation-rejects-stale-floor', [*rotate[:-1], str(stale)], False)
    sign('producer-rejects-expired-transition', {**document, 'expires_unix_ms': 1}, expected=1)
    sign('producer-rejects-production-transition', {**document, 'channel': 'production'}, expected=1)
    assert state.read_bytes() == original
    accepted = run('rotate-root', rotate, True)
    assert accepted['status'] == 'rotated' and accepted['minimum_sequence'] == next_sequence
    assert accepted['release_approved'] is False and accepted['runtime_started'] is False
    saved, mtime = state.read_bytes(), state.stat().st_mtime_ns
    parsed = json.loads(saved)
    assert parsed['schema'] == 3 and len(parsed['rotations']) == 1
    assert parsed['active'] == json.loads(original)['active']
    assert parsed['channel'] == json.loads(original)['channel']
    if args.previous_host:
        rejected = execute('older-host-rejects-selection-v3', ['env', 'HOME=' + str(root),
            'LEAN_CTX_CONFIG_DIR=' + str(root / 'compat-config'),
            'LEAN_CTX_DATA_DIR=' + str(root / 'compat-data'),
            'XDG_CONFIG_HOME=' + str(root / 'compat-xdg-config'),
            'XDG_DATA_HOME=' + str(root / 'compat-xdg-data'),
            'XDG_STATE_HOME=' + str(root / 'compat-state'),
            'XDG_CACHE_HOME=' + str(root / 'compat-cache'), 'DO_NOT_TRACK=1',
            args.previous_host, 'engine', 'runtime', 'health', *common,
            '--accept-proprietary', '--root', state.parent,
            '--expected-active', args.manifest_sha256], 2)
        assert 'runtime selection changed or retained package is inconsistent' in rejected.stderr.decode()
        record({'name': 'older-host-digest', 'sha256': args.previous_host_sha256,
                'path': str(args.previous_host), 'user_acceptance': False})
        assert state.read_bytes() == saved
    assert run('rotation-idempotent', rotate, True)['status'] == 'already_rotated'
    assert (state.read_bytes(), state.stat().st_mtime_ns) == (saved, mtime)

    def publish(name, value, key):
        source, signed = root / (name + '.json'), root / name
        source.write_text(json.dumps(value))
        public_hex = old_hex if key == old_key else new_hex
        execute(name, [sys.executable, '-B', args.catalog_signer, '--staging', '--catalog', source,
            '--private-key', key, '--key-id', hashlib.sha256(bytes.fromhex(public_hex)).hexdigest(),
            '--openssl', args.openssl, '--output', signed])
        payloads['/catalog'] = (signed / 'manifest.json').read_bytes()
        payloads['/catalog.sig'] = (signed / 'manifest.sig').read_bytes()

    publish('sign-new-root-catalog', {**catalog, 'sequence': next_sequence}, new_key)
    advanced = run('sync-after-rotation-with-original-anchor', sync, True)
    assert advanced['status'] == 'already_installed' and advanced['channel']['sequence'] == next_sequence
    assert advanced['channel']['root_key_sha256'] == hashlib.sha256(bytes.fromhex(new_hex)).hexdigest()
    selected = state.read_bytes()
    assert json.loads(selected)['schema'] == 3
    good = (payloads['/catalog'], payloads['/catalog.sig'])
    publish('sign-old-key-later-sequence', {**catalog, 'sequence': next_sequence + 1}, old_key)
    run('retired-root-cannot-publish', sync, False)
    publish('sign-new-key-below-floor', catalog, new_key)
    run('new-root-cannot-reset-sequence', sync, False)
    assert state.read_bytes() == selected
    payloads['/catalog'], payloads['/catalog.sig'] = good
    cycle = sign('sign-key-cycle', {**document, 'previous_root_key_hex': new_hex,
                 'next_root_key_hex': old_hex, 'minimum_sequence': next_sequence + 1}, new_key, old_key)
    run('rotation-rejects-key-cycle', [*rotate[:-1], str(cycle)], False)
    assert state.read_bytes() == selected
    tampered = json.loads(selected)
    tampered['rotations'][0]['next_signature'] = '0' * 128
    state.write_text(json.dumps(tampered))
    run('persisted-transition-is-reauthenticated', sync, False)
    state.write_bytes(selected)  # Restore only the driver's deliberate corruption.
    run('offline-install-preserves-rotated-trust', ['install', *common, '--archive', str(args.archive),
        '--manifest', str(args.manifest), '--signature', str(args.signature),
        '--root', str(state.parent), '--accept-proprietary', '--expected-active', args.manifest_sha256], True)
    assert state.read_bytes() == selected
    assert run('rotated-sync-retry', sync, True)['status'] == 'already_installed'
    assert state.read_bytes() == selected
    third, third_public = root / 'third-key.pem', root / 'third-key.der'
    third.touch(mode=0o600)
    execute('generate-second-successor', [args.openssl, 'genpkey', '-algorithm', 'ED25519', '-out', third])
    execute('export-second-successor', [args.openssl, 'pkey', '-in', third, '-pubout', '-outform', 'DER', '-out', third_public])
    third_hex = third_public.read_bytes()[-32:].hex()
    second = sign('sign-second-root-transition', {**document, 'previous_root_key_hex': new_hex,
                  'next_root_key_hex': third_hex, 'minimum_sequence': next_sequence + 1}, new_key, third)
    assert run('second-rotation-keeps-original-anchor', [*rotate[:-1], str(second)], True)['status'] == 'rotated'
    chained = state.read_bytes()
    assert len(json.loads(chained)['rotations']) == 2
    run('first-transition-cannot-be-replayed', rotate, False)
    assert state.read_bytes() == chained
    assert hashlib.sha256(signer.read_bytes()).hexdigest() == signer_digest
    record({'name': 'root-transition-producer-source', 'sha256': signer_digest,
            'path': str(signer), 'user_acceptance': False})
