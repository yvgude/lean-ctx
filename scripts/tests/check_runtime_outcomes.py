# SPDX-License-Identifier: Apache-2.0
"""Signed actual-peer outcome contract checks, not host history integration."""
import hashlib
import json
import time


def check_outcomes(run, root, common, target, digest, capability_version):
    request_file = root / 'outcome-request.json'
    for case in ('legacy', 'accepted', 'reversed', 'unknown', 'metric', 'schema', 'version'):
        legacy = case == 'legacy'
        version = '1.0.0' if legacy else '2.0.0'
        observations = ([{'succeeded': accepted, 'cost_micros': 1, 'latency_ms': 1.0}]
                        if legacy else [{'acceptance': 'accepted' if accepted else 'rejected'}]
                        for accepted in ((False, True) if case == 'reversed' else (True, False)))
        projection = {'schema_version': 1 if legacy else 2, 'seed': 42, 'exploration_bonus': 0.0,
                      'candidates': [{'model_id': model, 'observations': history}
                                     for model, history in zip(('z-model', 'a-model'), observations)]}
        if case == 'unknown':
            projection['candidates'][0]['observations'][0]['acceptance'] = 'unknown'
        elif case == 'metric':
            projection['candidates'][0]['observations'][0]['cost_micros'] = 0
        elif case == 'schema':
            projection['schema_version'] = 1
        elif case == 'version':
            version = '3.0.0'
        payload = json.dumps(projection)
        request = {'protocol_version': 'leanctx.runtime-exchange/v1',
                   'session_id': 'outcome-contract-session', 'request_id': 'outcome-contract-request',
                   'sequence': 1, 'deadline_unix_ms': time.time_ns() // 1_000_000 + 5000,
                   'input': payload,
                   'invocation': {'schema_version': 1, 'invocation_id': 'outcome-contract-invocation',
                       'engine': {'engine_id': 'leanctx-intelligence', 'engine_version': '4.0.0'},
                       'operation': {'capability_id': 'pro.runtime.adaptive_routing', 'capability_version': version},
                       'input_ref': 'projection:outcome-contract',
                       'input_digest': 'sha256:' + hashlib.sha256(payload.encode()).hexdigest(),
                       'source_refs': ['projection:outcome-contract'],
                       'policy_admission': {'policy_ref': 'policy:outcome-contract', 'decision': 'admitted'}}}
        request_file.write_text(json.dumps(request))
        success = legacy or (capability_version == '2.0.0' and case in ('accepted', 'reversed'))
        result = run('outcome-' + case, ['invoke', *common, *target, '--expected-active', digest,
                                       '--request', str(request_file)], success)
        if success:
            response = result['response']
            output = json.loads(response['output'])
            assert output['schema_version'] == (1 if legacy else 2)
            assert output['model_id'] == ('a-model' if case == 'reversed' else 'z-model')
            assert output['candidate_count'] == 2 and output['strategy'] == 'thompson_sampling'
            assert response['observation']['measurements'] == []
            assert response['observation'].get('receipt_link') is None
            assert response['observation']['output_digest'] == 'sha256:' + hashlib.sha256(response['output'].encode()).hexdigest()
