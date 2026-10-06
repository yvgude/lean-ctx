# SPDX-License-Identifier: Apache-2.0
"""Actual setup consent, signed download and activation in an isolated HOME."""
import json
import tomllib


def check_catalog_setup(run, root, common, args, base, channel_key, requests, payloads, effects):
    config = root / 'config' / 'lean-ctx' / 'config.toml'
    config.parent.mkdir(parents=True, exist_ok=True)
    original = config.read_bytes() if config.exists() else None
    local = root / '.lean-ctx.toml'
    assert not local.exists(), 'catalog setup must not overwrite another scenario'
    installed = root / 'setup-runtime'
    fixture = ('# preserve setup comment\nruntime_test_marker = "keep"\n[intelligence_runtime]\n'
        'staging = true\nroot = ' + json.dumps(str(installed)) + '\n'
        'channel_url = ' + json.dumps(base + '/catalog') + '\n'
        'channel_signature_url = ' + json.dumps(base + '/catalog.sig') + '\n'
        'channel_root_key_hex = ' + json.dumps(channel_key) + '\n')
    status = ['status', '--staging']
    consent = ['configure', '--staging']
    sync = ['sync-configured', '--staging', '--accept-proprietary']

    def invoke(name, command, success=True, **options):
        return run(name, command, success, prefix=('setup', 'runtime'), **options)

    try:
        before_requests = len(requests)
        local.write_text(fixture)
        assert invoke('project-channel-not-admitted', status)['status'] == 'not_configured'
        assert len(requests) == before_requests and not installed.exists()
        local.unlink()
        config.write_text(fixture)
        assert invoke('global-channel-discovered-without-egress', status)['status'] == 'download_available'
        invoke('channel-setup-requires-terminal', consent, False)
        for answer in ('n\n', '\n'):
            declined = invoke('channel-setup-default-no', consent, json_output=False, tty_input=answer)
            assert 'Not installed' in declined and 'proprietary' in declined
        invoke('configured-sync-requires-consent', sync[:-1], False)
        invoke('configured-sync-yes-is-not-consent', [*sync[:-1], '--yes'], False)
        assert len(requests) == before_requests and not installed.exists()
        assert config.read_text() == fixture
        accepted = invoke('channel-setup-interactive-install', consent,
                          json_output=False, tty_input='yes\n')
        assert 'Installed and enabled after signature and compatibility checks.' in accepted
        assert installed.stat().st_mode & 0o777 == 0o700
        current = tomllib.loads(config.read_text())['intelligence_runtime']
        assert current['enabled'] and current['accept_proprietary']
        assert current['trust_key_hex'] == common[-1] and current['channel_root_key_hex'] == channel_key
        assert current['manifest_sha256'] == args.manifest_sha256
        assert '# preserve setup comment' in config.read_text() and 'runtime_test_marker = "keep"' in config.read_text()
        selection = installed / 'selection.json'
        saved, mtime = selection.read_bytes(), selection.stat().st_mtime_ns
        config_saved, before_requests = config.read_bytes(), len(requests)
        assert invoke('channel-setup-installed-status', status)['status'] == 'configured'
        assert len(requests) == before_requests
        retried = invoke('configured-sync-idempotent', sync)
        assert retried['installation']['status'] == 'already_installed'
        assert (selection.read_bytes(), selection.stat().st_mtime_ns) == (saved, mtime)
        assert config.read_bytes() == config_saved
        signature = payloads['/catalog.sig']
        payloads['/catalog.sig'] = b'x' * 64
        invoke('configured-sync-bad-signature-keeps-active', sync, False)
        payloads['/catalog.sig'] = signature
        assert config.read_bytes() == config_saved and selection.read_bytes() == saved
        catalog = payloads.pop('/catalog')
        invoke('configured-sync-unavailable-keeps-active', sync, False)
        payloads['/catalog'] = catalog
        assert config.read_bytes() == config_saved and selection.read_bytes() == saved

        # Interrupt after authenticated download but before activation. Retry must
        # recover the retained verified package without resetting the store.
        interrupted_root = root / 'interrupted-setup-runtime'
        pending = fixture.replace(str(installed), str(interrupted_root))
        altered = pending.replace(base + '/catalog"', base + '/changed"')
        config.write_text(pending)

        def alter_during_download(path):
            if path == '/archive':
                config.write_text(altered)

        effects.append(alter_during_download)
        invoke('configured-sync-refuses-changed-policy', sync, False)
        effects.clear()
        assert config.read_text() == altered
        assert (interrupted_root / 'selection.json').is_file()
        assert not tomllib.loads(altered)['intelligence_runtime'].get('enabled', False)
        config.write_text(pending)
        recovered = invoke('configured-sync-recovers-interrupted-install', sync)
        assert recovered['installation']['status'] == 'already_installed'
        assert recovered['activation']['health']['status'] == 'healthy'
        assert recovered['release_approved'] is False

        # A stale configured receipt is not a usable runtime. A new authenticated
        # channel operation may re-establish the pins; status alone cannot do so.
        valid = config.read_text()
        stale = valid.replace(args.manifest_sha256, '0' * 64)
        assert stale != valid
        config.write_text(stale)
        assert invoke('channel-setup-stale-pin-needs-repair', status)['status'] == 'repair_available'
        assert config.read_text() == stale
        assert invoke('configured-sync-repairs-stale-pin', sync)['activation']['status'] == 'activated'
        invoke('channel-setup-deactivate-retains-channel', ['deactivate', '--staging'])
        assert tomllib.loads(config.read_text())['intelligence_runtime']['channel_root_key_hex'] == channel_key
    finally:
        effects.clear()
        if local.exists():
            local.unlink()
        if original is None:
            config.unlink(missing_ok=True)
        else:
            config.write_bytes(original)
