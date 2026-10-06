# SPDX-License-Identifier: Apache-2.0
"""Focused built-host selection checks; invoked by check_runtime_install.py."""
import copy
import json
from pathlib import Path


def check_ranking(run, root, installed, common, target, digest, capability_version=None):
    fixture = Path(__file__).resolve().parents[2] / 'tests/ocla_contract_suite/v1/execution-plan/valid_minimal.json'
    plan = json.loads(fixture.read_text())
    candidates = []
    for index, model in enumerate(('reference-model', 'alternative-model', 'blocked-model')):
        item = dict(plan, plan_id=f'plan-ranking-{index}', model=model, provider=f'provider-{index}',
                    expected_cost_micros=(index + 1) * 10, expected_quality_milli=900, expected_latency_ms=30)
        candidates.append(dict(plan=item, capability_id=item['capability_ids'][0], model=model,
                               provider=item['provider'], expected_cost_micros=item['expected_cost_micros'],
                               expected_quality_milli=900, expected_latency_ms=30, exclusion_reason=None))
    source = dict(schema_version=1, candidates=candidates,
                  policy=dict(allowed_providers=['provider-0', 'provider-1'],
                              require_local_execution=False, require_reversible=False))
    request_file = root / 'selection-request.json'
    command = ['select', *common, *target, '--expected-active', digest, '--request', str(request_file)]

    def select(name, payload=None, args=None, success=True):
        request_file.write_text(json.dumps(source if payload is None else payload))
        return run(name, command if args is None else args, success)

    reference_args = [value for value in command if value != '--accept-proprietary']
    reference = select('select-reference', args=[*reference_args, '--reference-only'])
    assert reference['enhancement'] == 'reference_only' and reference['evidence'] is None
    reference_plan = reference['decision']['selected']
    assert reference_plan['model'] == 'reference-model'
    assert reference['decision']['fallback'] == reference_plan
    assert reference['decision']['candidates_evaluated'] == 3
    assert reference['decision']['candidates_excluded'] == 1
    enhanced = select('select-private')
    assert enhanced['enhancement'] == 'private_runtime' and enhanced['release_approved'] is False
    selected = enhanced['decision']['selected']
    assert selected['model'] in ('reference-model', 'alternative-model')
    assert enhanced['decision']['fallback'] == reference_plan
    assert enhanced['decision']['confidence_milli'] == 0
    assert enhanced['decision']['rationale_code'] == 'private_thompson_sampling_cold_start'
    evidence = enhanced['evidence']
    projection = json.loads(evidence['request']['input'])
    if capability_version is not None:
        assert evidence['request']['invocation']['operation']['capability_version'] == capability_version
        assert projection['schema_version'] == (2 if capability_version == '2.0.0' else 1)
    assert projection['candidates'] == [{'model_id': f'candidate-{index}', 'observations': []} for index in range(2)]
    for candidate in candidates:
        assert candidate['model'] not in json.dumps(evidence)
        assert candidate['provider'] not in json.dumps(evidence)
    assert evidence['execution']['response']['observation'].get('receipt_link') is None
    repeated = select('select-private-repeat')
    assert repeated['enhancement'] == 'private_runtime' and repeated['decision']['selected'] == selected
    missing = command.copy()
    missing[missing.index('--root') + 1] = str(root / 'absent-runtime')
    single = copy.deepcopy(source)
    single['policy']['allowed_providers'] = ['provider-0']
    result = select('select-single-without-runtime', single, missing)
    assert result['enhancement'] == 'single_permitted_candidate' and result['evidence'] is None
    assert result['decision']['selected'] == reference_plan
    for name, change in (
        ('policy-denied', lambda value: value['policy'].update(allowed_providers=[])),
        ('metadata-conflict', lambda value: value['candidates'][0].update(provider='unadmitted')),
        ('cost-conflict', lambda value: value['candidates'][0].update(expected_cost_micros=0)),
        ('task-conflict', lambda value: value['candidates'][0]['plan'].update(task_id='task-other')),
        ('duplicate-plan', lambda value: value['candidates'][1]['plan'].update(plan_id='plan-ranking-0')),
        ('empty-candidates', lambda value: value.update(candidates=[])),
    ):
        invalid = copy.deepcopy(source)
        change(invalid)
        select('select-' + name, invalid, missing, False)
    for name, args in (('missing-runtime', missing), ('wrong-trust', command.copy())):
        if name == 'wrong-trust':
            offset = args.index('--trust-key-hex') + 1
            args[offset] = ('0' if args[offset][0] != '0' else '1') + args[offset][1:]
        result = select('select-' + name, args=args)
        assert result['enhancement'] == 'runtime_unavailable_reference_fallback'
        assert result['decision'] == reference['decision'] and result['evidence'] is None
    original = installed.read_bytes()
    installed.chmod(0o600)
    installed.write_bytes(original + b'corrupt')
    try:
        result = select('select-tampered-runtime')
        assert result['enhancement'] == 'runtime_unavailable_reference_fallback'
        assert result['decision'] == reference['decision'] and result['evidence'] is None
        result = select('select-reference-without-runtime', args=[*missing, '--reference-only'])
        assert result == reference
    finally:
        installed.write_bytes(original)
        installed.chmod(0o500)
