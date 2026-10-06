# SPDX-License-Identifier: Apache-2.0
"""Real setup/runtime CLI checks inside the install driver's temporary HOME."""
import tomllib


def check_configuration(run, root, installed, common, target, digest):
    status = ['status', '--staging']
    discovery = run('setup-runtime-absent', status, True, prefix=('setup', 'runtime'))
    assert discovery['status'] == 'not_configured' and not discovery['runtime_started']
    assert 'proprietary' in discovery['disclosure'] and 'public reference' in discovery['disclosure']
    configured = ['select-configured', '--staging', '--request', str(root / 'selection-request.json')]
    activate = ['activate', *common, *target, '--expected-active', digest]
    reference = run('configured-default-reference', configured, True)
    assert reference['enhancement'] == 'reference_only'
    without_consent = [value for value in activate if value != '--accept-proprietary']
    run('activation-consent-required', without_consent, False, prefix=('setup', 'runtime'))
    # Project files cannot supply pins or accept a license on behalf of the user.
    local = root / '.lean-ctx.toml'
    local.write_text('[intelligence_runtime]\nenabled = true\naccept_proprietary = true\n'
                     'staging = true\nroot = "/untrusted"\nmanifest_sha256 = "project-pin"\n'
                     'trust_key_hex = "project-key"\n')
    assert run('project-cannot-activate', configured, True) == reference
    assert run('project-cannot-supply-discovery', status, True, prefix=('setup', 'runtime')) == discovery
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
    printed = run('merged-project-cannot-replace-pins', [], True, prefix=('config',), json_output=False)
    effective = tomllib.loads(printed.split('\n\n', 1)[1].split('\n\nLocal config', 1)[0])
    assert effective['intelligence_runtime'] == current
    active = run('configured-private-selection', configured, True)
    assert active['enhancement'] == 'private_runtime'
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
        fallback = run('configured-tamper-fallback', configured, True)
        assert fallback['enhancement'] == 'runtime_unavailable_reference_fallback'
        assert fallback['decision'] == reference['decision']
    finally:
        installed.write_bytes(original)
        installed.chmod(0o500)
    config_file.write_text('intelligence_runtime = [invalid TOML')
    run('activation-corrupt-config', activate, False, prefix=('setup', 'runtime'))
    assert config_file.read_text() == 'intelligence_runtime = [invalid TOML'
    assert run('corrupt-config-reference', configured, True) == reference
    config_file.write_bytes(b'\xff\xfe')
    run('activation-unreadable-config', activate, False, prefix=('setup', 'runtime'))
    run('deactivation-unreadable-config', ['deactivate', '--staging'], False, prefix=('setup', 'runtime'))
    assert config_file.read_bytes() == b'\xff\xfe'
    assert run('unreadable-config-reference', configured, True) == reference
    config_file.write_bytes(before)
    disabled = run('setup-deactivate', ['deactivate', '--staging'], True, prefix=('setup', 'runtime'))
    assert disabled['status'] == 'disabled'
    assert not tomllib.loads(config_file.read_text())['intelligence_runtime']['enabled']
    assert run('configured-disabled-reference', configured, True) == reference
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
