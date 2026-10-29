# SPDX-License-Identifier: Apache-2.0
"""Signed actual-peer context outcome checks and retired-capability rejection."""
import hashlib
import json
import time


INVOCATION_ADMISSION_ERROR = (
    'engine runtime: runtime invocation is not supported by the authenticated package and this host')


def context_projection(reverse=False):
    """Match the Engine input projection produced by intelligence_runtime::context_selection."""
    candidates = [
        {'candidate_id': 0, 'relevance_milli': 850, 'confidence_milli': 900,
         'tokens': 64, 'stale': False},
        {'candidate_id': 1, 'relevance_milli': 420, 'confidence_milli': 700,
         'tokens': 96, 'stale': True},
    ]
    if reverse:
        candidates.reverse()
        for index, candidate in enumerate(candidates):
            candidate['candidate_id'] = index
    return {'schema_version': 1, 'budget_tokens': 256, 'candidates': candidates}


def assert_context_result(result, invocation, candidate_count):
    assert result['status'] == 'executed' and result['service_running'] is False
    response = result['response']
    output = json.loads(response['output'])
    assert set(output) == {'schema_version', 'strategy', 'scores_milli'}
    assert output['schema_version'] == 1 and output['strategy'] == 'budget_relevance_v1'
    scores = output['scores_milli']
    assert len(scores) == candidate_count
    assert all(type(score) is int and 1 <= score <= 1000 for score in scores)
    observation = response['observation']
    assert observation['output_digest'] == 'sha256:' + hashlib.sha256(
        response['output'].encode()).hexdigest()
    assert observation['invocation_id'] == invocation['invocation_id']
    assert observation['source_lineage'] == invocation['source_refs']
    assert observation['status'] == 'succeeded'
    assert observation.get('receipt_link') is None


def _request(projection, request_id, invocation_id, engine_version, capability_id,
             capability_version, input_ref):
    payload = json.dumps(projection)
    return {'protocol_version': 'leanctx.runtime-exchange/v1',
            'session_id': request_id, 'request_id': request_id,
            'sequence': 1, 'deadline_unix_ms': time.time_ns() // 1_000_000 + 5000,
            'input': payload,
            'invocation': {'schema_version': 1, 'invocation_id': invocation_id,
                'engine': {'engine_id': 'leanctx-intelligence', 'engine_version': engine_version},
                'operation': {'capability_id': capability_id, 'capability_version': capability_version},
                'input_ref': input_ref,
                'input_digest': 'sha256:' + hashlib.sha256(payload.encode()).hexdigest(),
                'source_refs': [input_ref],
                'policy_admission': {'policy_ref': 'policy:' + request_id, 'decision': 'admitted'}}}


def _retired_routing_projection(version):
    legacy = version == '1.0.0'
    observations = ([{'succeeded': accepted, 'cost_micros': 1, 'latency_ms': 1.0}]
                    if legacy else [{'acceptance': 'accepted' if accepted else 'rejected'}]
                    for accepted in (True, False))
    return {'schema_version': 1 if legacy else 2, 'seed': 42, 'exploration_bonus': 0.0,
            'candidates': [{'model_id': model, 'observations': history}
                           for model, history in zip(('z-model', 'a-model'), observations)]}


def check_retired_routing(run, root, common, target, digest, engine_version):
    request_file = root / 'retired-routing-request.json'
    for version in ('1.0.0', '2.0.0'):
        request_id = 'retired-routing-' + version
        request = _request(_retired_routing_projection(version), request_id,
                           request_id + '-invocation', engine_version,
                           'pro.runtime.adaptive_routing', version,
                           'projection:' + request_id)
        request_file.write_text(json.dumps(request))
        run('retired-routing-v' + version, ['invoke', *common, *target,
            '--expected-active', digest, '--request', str(request_file)], False,
            expected_error=INVOCATION_ADMISSION_ERROR)


def check_outcomes(run, root, common, target, digest, context_capability_version, engine_version):
    assert context_capability_version == '1.0.0'
    request_file = root / 'outcome-request.json'

    def invoke_context(name, projection, capability_version=context_capability_version,
                       success=True, expected_error=None):
        request_id = 'context-outcome-' + name
        request = _request(projection, request_id, request_id + '-invocation', engine_version,
                           'pro.runtime.adaptive_context', capability_version,
                           'projection:' + request_id)
        request_file.write_text(json.dumps(request))
        result = run('outcome-' + name, ['invoke', *common, *target,
            '--expected-active', digest, '--request', str(request_file)],
            success, expected_error=expected_error)
        if success:
            assert_context_result(result, request['invocation'], len(projection['candidates']))
        return result

    invoke_context('valid-before-retirement', context_projection())
    check_retired_routing(run, root, common, target, digest, engine_version)
    invoke_context('valid-after-retirement', context_projection(reverse=True))

    invalid = context_projection()
    invalid['candidates'][0]['confidence_milli'] = 1001
    invoke_context('invalid-confidence', invalid, success=False)
    invalid = context_projection()
    invalid['candidates'][0]['tokens'] = 0
    invoke_context('invalid-token-count', invalid, success=False)
    invalid = context_projection()
    invalid['schema_version'] = 2
    invoke_context('invalid-context-schema', invalid, success=False)
    invoke_context('wrong-context-version', context_projection(), '2.0.0',
                   success=False, expected_error=INVOCATION_ADMISSION_ERROR)
