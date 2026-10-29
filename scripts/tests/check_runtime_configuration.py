# SPDX-License-Identifier: Apache-2.0
"""Real setup/runtime CLI checks inside the install driver's temporary HOME."""
import tomllib


def check_configuration(run, root, installed, common, target, digest):
    status = ['status', '--staging']
    discovery = run('setup-runtime-absent', status, True, prefix=('setup', 'runtime'))
    assert discovery['status'] == 'not_configured' and not discovery['runtime_started']
    assert 'proprietary' in discovery['disclosure'] and 'public reference' in discovery['disclosure']
    activate = ['activate', *common, *target, '--expected-active', digest]
    without_consent = [value for value in activate if value != '--accept-proprietary']
    run('activation-consent-required', without_consent, False, prefix=('setup', 'runtime'))
    # Project files cannot supply pins or accept a license on behalf of the user.
    local = root / '.lean-ctx.toml'
    local.write_text('[intelligence_runtime]\nenabled = true\naccept_proprietary = true\n'
                     'staging = true\nroot = "/untrusted"\nmanifest_sha256 = "project-pin"\n'
                     'trust_key_hex = "project-key"\n')
    assert run('project-config-does-not-replace-global-status', status, True,
               prefix=('setup', 'runtime')) == discovery
    result = run('setup-activate', activate, True, prefix=('setup', 'runtime'))
    assert result['status'] == 'activated' and result['health']['status'] == 'healthy'
    assert result['release_approved'] is False
    configs = list(root.rglob('config.toml'))
    assert len(configs) == 1, configs
    config_file = configs[0]
    current = tomllib.loads(config_file.read_text())['intelligence_runtime']
    assert current['enabled'] and current['accept_proprietary'] and current['staging']
    assert current['manifest_sha256'] == digest and current['root'] != '/untrusted'
    config_file.write_text('# retain comment\nruntime_test_marker = "keep"\n' + config_file.read_text())
    printed = run('merged-project-cannot-replace-runtime-pins', [], True,
                  prefix=('config',), json_output=False)
    effective = tomllib.loads(printed.split('\n\n', 1)[1].split('\n\nLocal config', 1)[0])
    # Serialization expands defaults absent from the persisted opt-in table.
    expected = {
        'channel_url': '',
        'channel_signature_url': '',
        'channel_root_key_hex': '',
        'license_configuration': '',
        'context_policy_apply': False,
        'team_artifact_projects': [],
        **current,
    }
    assert effective['intelligence_runtime'] == expected
    configured = run('global-runtime-status-after-activation', status, True, prefix=('setup', 'runtime'))
    assert configured['status'] == 'configured' and not configured['runtime_started']
    run('setup-activate-again', activate, True, prefix=('setup', 'runtime'))
    assert '# retain comment' in config_file.read_text() and 'runtime_test_marker = "keep"' in config_file.read_text()
    before = config_file.read_bytes()
    wrong = activate.copy()
    offset = wrong.index('--trust-key-hex') + 1
    wrong[offset] = ('0' if wrong[offset][0] != '0' else '1') + wrong[offset][1:]
    run('activation-wrong-trust', wrong, False, prefix=('setup', 'runtime'))
    assert config_file.read_bytes() == before
    original = installed.read_bytes()
    installed.chmod(0o600)
    installed.write_bytes(original + b'corrupt')
    try:
        run('activation-tamper', activate, False, prefix=('setup', 'runtime'))
        run('setup-discovery-rejects-tamper', status, False, prefix=('setup', 'runtime'))
        assert config_file.read_bytes() == before
    finally:
        installed.write_bytes(original)
        installed.chmod(0o500)
    assert run('status-recovers-after-package-restoration', status, True,
               prefix=('setup', 'runtime'))['status'] == 'configured'
    config_file.write_text('intelligence_runtime = [invalid TOML')
    run('activation-corrupt-config', activate, False, prefix=('setup', 'runtime'))
    assert config_file.read_text() == 'intelligence_runtime = [invalid TOML'
    run('status-corrupt-config', status, False, prefix=('setup', 'runtime'))
    config_file.write_bytes(bytes([0xff, 0xfe]))
    run('activation-unreadable-config', activate, False, prefix=('setup', 'runtime'))
    run('deactivation-unreadable-config', ['deactivate', '--staging'], False, prefix=('setup', 'runtime'))
    assert config_file.read_bytes() == bytes([0xff, 0xfe])
    run('status-unreadable-config', status, False, prefix=('setup', 'runtime'))
    config_file.write_bytes(before)
    disabled = run('setup-deactivate', ['deactivate', '--staging'], True, prefix=('setup', 'runtime'))
    assert disabled['status'] == 'disabled'
    assert not tomllib.loads(config_file.read_text())['intelligence_runtime']['enabled']
    disabled_status = run('status-after-deactivation', status, True, prefix=('setup', 'runtime'))
    assert disabled_status['status'] == 'available' and not disabled_status['runtime_started']
    assert '# retain comment' in config_file.read_text() and 'runtime_test_marker = "keep"' in config_file.read_text()
    disabled_bytes = config_file.read_bytes()
    runtime_setup = ['configure', '--staging']
    run('runtime-prompt-requires-terminal', runtime_setup, False, prefix=('setup', 'runtime'))
    declined = run('runtime-prompt-declined', runtime_setup, True, prefix=('setup', 'runtime'),
                   json_output=False, tty_input='n\n')
    assert 'proprietary' in declined and 'Not enabled' in declined
    assert config_file.read_bytes() == disabled_bytes
    installed.chmod(0o400)  # discovery must verify bytes without executing the runtime
    try:
        available = run('setup-discovers-disabled-installation', status, True, prefix=('setup', 'runtime'))
        assert available['status'] == 'available' and not available['runtime_started']
        assert config_file.read_bytes() == disabled_bytes
    finally:
        installed.chmod(0o500)
    enable_configured = ['activate-configured', '--staging']
    run('discovered-runtime-requires-consent', enable_configured, False, prefix=('setup', 'runtime'))
    run('yes-is-not-proprietary-consent', [*enable_configured, '--yes'], False, prefix=('setup', 'runtime'))
    assert config_file.read_bytes() == disabled_bytes
    enabled = run('setup-enables-discovered-runtime', [*enable_configured, '--accept-proprietary'],
                  True, prefix=('setup', 'runtime'))
    assert enabled['status'] == 'activated' and enabled['health']['status'] == 'healthy'
    enabled_bytes = config_file.read_bytes()
    assert run('setup-reports-existing-consent', status, True, prefix=('setup', 'runtime'))['status'] == 'configured'
    assert config_file.read_bytes() == enabled_bytes
    run('disable-before-interactive-consent', ['deactivate', '--staging'], True, prefix=('setup', 'runtime'))
    accepted = run('runtime-prompt-explicit-consent', runtime_setup, True, prefix=('setup', 'runtime'),
                   json_output=False, tty_input='yes\n')
    assert 'Enabled after signature and compatibility checks.' in accepted
    assert tomllib.loads(config_file.read_text())['intelligence_runtime']['enabled']
