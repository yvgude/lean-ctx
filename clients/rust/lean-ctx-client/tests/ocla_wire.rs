//! Public-fixture compatibility and adversarial OCLA offline-verifier tests.

use std::fs;
use std::path::Path;

use jsonschema::{Draft, JSONSchema};
use lean_ctx_client::{
    decode_agent_envelope, decode_canonical_token_envelope, verify_agent_gateway_admissibility,
    AgentEnvelopeV1, CanonicalTokenEnvelopeV1, OclaGatewayAdmissibilityError, OclaWireError,
    AGENT_ENVELOPE_SCHEMA_ID, CANONICAL_TOKEN_ENVELOPE_SCHEMA_ID, MAX_OCLA_WIRE_BYTES,
};
use serde_json::Value;

const TOKEN_FIXTURE: &[u8] = include_bytes!("fixtures/canonical-token-envelope-v1.json");
const AGENT_FIXTURE: &[u8] = include_bytes!("fixtures/agent-envelope-v1.json");
const SELF_RELAY_FIXTURE: &[u8] = include_bytes!("fixtures/self-relay-agent-envelope-v1.json");
const INVALID_TOKEN_FIXTURE: &[u8] = include_bytes!("fixtures/invalid-token-envelope-v1.json");
const INVALID_AGENT_FIXTURE: &[u8] = include_bytes!("fixtures/invalid-agent-envelope-v1.json");
const TOKEN_SCHEMA: &str = include_str!("../contracts/ocla-wire-v1.schema.json");
const AGENT_SCHEMA: &str = include_str!("../contracts/ocla-agent-envelope-v1.schema.json");

#[test]
fn committed_public_fixtures_decode_to_typed_contracts() {
    let token = decode_canonical_token_envelope(TOKEN_FIXTURE).expect("token fixture verifies");
    assert_eq!(token.provider, "openai");
    assert_eq!(token.token_balance.delivered_tokens, 60);

    let agent = decode_agent_envelope(AGENT_FIXTURE).expect("agent fixture verifies");
    assert_eq!(agent.from_agent_id, "owner-agent");
    assert_eq!(agent.budget_tokens, 900);

    let self_relay =
        decode_agent_envelope(SELF_RELAY_FIXTURE).expect("self-relay fixture verifies");
    assert_eq!(self_relay.from_agent_id, self_relay.to_agent_id);
    assert_eq!(
        serde_json::to_vec(&self_relay).expect("serialize self-relay fixture"),
        SELF_RELAY_FIXTURE
    );
}

#[test]
fn fixtures_pass_packaged_draft_2020_12_schemas() {
    assert_fixture_valid(
        TOKEN_FIXTURE,
        TOKEN_SCHEMA,
        CANONICAL_TOKEN_ENVELOPE_SCHEMA_ID,
    );
    assert_fixture_valid(AGENT_FIXTURE, AGENT_SCHEMA, AGENT_ENVELOPE_SCHEMA_ID);
    assert_fixture_valid(SELF_RELAY_FIXTURE, AGENT_SCHEMA, AGENT_ENVELOPE_SCHEMA_ID);

    let invalid_token: Value =
        serde_json::from_slice(INVALID_TOKEN_FIXTURE).expect("invalid token fixture JSON");
    let invalid_agent: Value =
        serde_json::from_slice(INVALID_AGENT_FIXTURE).expect("invalid agent fixture JSON");
    assert!(!compile_schema(TOKEN_SCHEMA).is_valid(&invalid_token));
    assert!(!compile_schema(AGENT_SCHEMA).is_valid(&invalid_agent));
}

#[test]
fn u64_boundaries_are_schema_gated_before_typed_decode() {
    let token_schema = compile_schema(TOKEN_SCHEMA);
    let agent_schema = compile_schema(AGENT_SCHEMA);
    let above_u64: Value =
        serde_json::from_str("18446744073709551616").expect("valid JSON integer above u64");

    let mut max_token: CanonicalTokenEnvelopeV1 =
        serde_json::from_slice(TOKEN_FIXTURE).expect("typed token fixture");
    max_token.token_balance.original_tokens = u64::MAX;
    max_token.token_balance.materialized_tokens = u64::MAX;
    max_token.token_balance.delivered_tokens = u64::MAX;
    max_token.token_balance.provider_billed_tokens = u64::MAX;
    let max_token_value = serde_json::to_value(&max_token).expect("maximum token JSON");
    assert!(token_schema.is_valid(&max_token_value));
    decode_canonical_token_envelope(
        &serde_json::to_vec(&max_token).expect("serialize maximum token envelope"),
    )
    .expect("typed decoder accepts schema maximum");

    let mut max_agent: AgentEnvelopeV1 =
        serde_json::from_slice(AGENT_FIXTURE).expect("typed agent fixture");
    max_agent.budget_tokens = u64::MAX;
    max_agent.relay_id = "agent-relay:pending".to_string();
    max_agent.relay_id = max_agent
        .canonical_relay_id()
        .expect("derive maximum-budget relay ID");
    let max_agent_value = serde_json::to_value(&max_agent).expect("maximum agent JSON");
    assert!(agent_schema.is_valid(&max_agent_value));
    decode_agent_envelope(
        &serde_json::to_vec(&max_agent).expect("serialize maximum agent envelope"),
    )
    .expect("typed decoder accepts schema maximum");

    let token: Value = serde_json::from_slice(TOKEN_FIXTURE).expect("token JSON");
    for field in [
        "original_tokens",
        "materialized_tokens",
        "delivered_tokens",
        "provider_billed_tokens",
    ] {
        let invalid = mutate(&token, |value| {
            value["token_balance"][field] = above_u64.clone()
        });
        assert!(
            !token_schema.is_valid(&invalid),
            "{field} above u64 must be schema-rejected before typed decode"
        );
    }

    let agent: Value = serde_json::from_slice(AGENT_FIXTURE).expect("agent JSON");
    let invalid = mutate(&agent, |value| value["budget_tokens"] = above_u64);
    assert!(
        !agent_schema.is_valid(&invalid),
        "budget_tokens above u64 must be schema-rejected before typed decode"
    );
}

#[test]
fn packaged_schemas_match_monorepo_contracts_when_present() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../..");
    for name in [
        "ocla-wire-v1.schema.json",
        "ocla-agent-envelope-v1.schema.json",
    ] {
        let public = root.join("docs/contracts").join(name);
        if public.exists() {
            let packaged = Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("contracts")
                .join(name);
            assert_eq!(
                fs::read(&packaged).expect("read packaged schema"),
                fs::read(&public).expect("read monorepo schema"),
                "packaged schema drift: {name}"
            );
        }
    }
}

#[test]
fn nested_and_scalar_schema_constraints_fail_closed() {
    let token_schema = compile_schema(TOKEN_SCHEMA);
    let token: Value = serde_json::from_slice(TOKEN_FIXTURE).expect("token JSON");
    for invalid in [
        mutate(&token, |value| {
            value["context"]["unknown"] = Value::Bool(true)
        }),
        mutate(&token, |value| {
            value["token_balance"]["unknown"] = Value::Bool(true)
        }),
        mutate(&token, |value| {
            value["provider"] = Value::String("   ".into())
        }),
        mutate(&token, |value| {
            value["surface"] = Value::String("grpc".into())
        }),
        mutate(&token, |value| {
            value["context"]["session_id"] = Value::String(String::new())
        }),
    ] {
        assert!(
            !token_schema.is_valid(&invalid),
            "invalid token schema case: {invalid}"
        );
    }

    let agent_schema = compile_schema(AGENT_SCHEMA);
    let agent: Value = serde_json::from_slice(AGENT_FIXTURE).expect("agent JSON");
    for invalid in [
        mutate(&agent, |value| {
            value["context"]["unknown"] = Value::Bool(true)
        }),
        mutate(&agent, |value| {
            value["from_agent_id"] = Value::String("bad id".into())
        }),
        mutate(&agent, |value| {
            value["from_agent_id"] = Value::String("bad\n".into())
        }),
        mutate(&agent, |value| {
            value["to_agent_id"] = Value::String("é".into())
        }),
        mutate(&agent, |value| {
            value["to_agent_id"] = Value::String("a".repeat(257))
        }),
        mutate(&agent, |value| {
            value["capsule_ref"] = Value::String("capsule:ABC".into())
        }),
        mutate(&agent, |value| value["budget_tokens"] = Value::from(0)),
    ] {
        assert!(
            !agent_schema.is_valid(&invalid),
            "invalid agent schema case: {invalid}"
        );
    }
}

#[test]
fn unknown_duplicate_and_malformed_fields_fail_closed() {
    let mut unknown = TOKEN_FIXTURE.to_vec();
    let closing = unknown.pop().expect("fixture has closing brace");
    assert_eq!(closing, b'}');
    unknown.extend_from_slice(br#","unknown":true}"#);
    assert!(matches!(
        decode_canonical_token_envelope(&unknown),
        Err(OclaWireError::Malformed(_))
    ));

    let mut duplicate = br#"{"schema_version":1,"#.to_vec();
    duplicate.extend_from_slice(&TOKEN_FIXTURE[1..]);
    let duplicate_error =
        decode_canonical_token_envelope(&duplicate).expect_err("duplicate must fail");
    assert!(
        matches!(&duplicate_error, OclaWireError::Malformed(_))
            && duplicate_error.to_string().contains("duplicate field"),
        "unexpected duplicate result: {duplicate_error}"
    );
    assert!(matches!(
        decode_agent_envelope(b"{"),
        Err(OclaWireError::Malformed(_))
    ));
}

#[test]
fn oversized_documents_are_rejected_before_decode() {
    let oversized = vec![b' '; MAX_OCLA_WIRE_BYTES + 1];
    assert_eq!(
        decode_canonical_token_envelope(&oversized),
        Err(OclaWireError::Oversize {
            actual: MAX_OCLA_WIRE_BYTES + 1,
            maximum: MAX_OCLA_WIRE_BYTES,
        })
    );
    assert!(matches!(
        decode_agent_envelope(&oversized),
        Err(OclaWireError::Oversize { .. })
    ));
}

#[test]
fn noncanonical_json_is_rejected() {
    let mut spaced = Vec::with_capacity(TOKEN_FIXTURE.len() + 1);
    spaced.push(b' ');
    spaced.extend_from_slice(TOKEN_FIXTURE);
    assert_eq!(
        decode_canonical_token_envelope(&spaced),
        Err(OclaWireError::NonCanonical)
    );

    let mut agent_with_newline = AGENT_FIXTURE.to_vec();
    agent_with_newline.push(b'\n');
    assert_eq!(
        decode_agent_envelope(&agent_with_newline),
        Err(OclaWireError::NonCanonical)
    );
}

#[test]
fn token_balance_order_is_enforced_without_conflating_provider_billing() {
    let mut token: CanonicalTokenEnvelopeV1 =
        serde_json::from_slice(TOKEN_FIXTURE).expect("typed fixture");

    token.token_balance.materialized_tokens = token.token_balance.original_tokens + 1;
    let wire = serde_json::to_vec(&token).expect("serialize mutation");
    assert!(matches!(
        decode_canonical_token_envelope(&wire),
        Err(OclaWireError::InvalidInvariant(
            "materialized_tokens exceeds original_tokens"
        ))
    ));

    token.token_balance.original_tokens = 100;
    token.token_balance.materialized_tokens = 80;
    token.token_balance.delivered_tokens = 81;
    let wire = serde_json::to_vec(&token).expect("serialize mutation");
    assert!(matches!(
        decode_canonical_token_envelope(&wire),
        Err(OclaWireError::InvalidInvariant(
            "delivered_tokens exceeds materialized_tokens"
        ))
    ));

    token.token_balance.delivered_tokens = 60;
    token.token_balance.provider_billed_tokens = 150;
    let wire = serde_json::to_vec(&token).expect("serialize independent billing");
    decode_canonical_token_envelope(&wire)
        .expect("provider billing is distinct from payload lifecycle ordering");
}

#[test]
fn incomplete_or_mismatched_lineage_is_rejected() {
    let mut token: CanonicalTokenEnvelopeV1 =
        serde_json::from_slice(TOKEN_FIXTURE).expect("typed fixture");
    token.context.session_id.clear();
    let wire = serde_json::to_vec(&token).expect("serialize mutation");
    assert!(matches!(
        decode_canonical_token_envelope(&wire),
        Err(OclaWireError::InvalidInvariant("session_id is required"))
    ));

    let mut agent: AgentEnvelopeV1 = serde_json::from_slice(AGENT_FIXTURE).expect("typed fixture");
    agent.context.agent_id = "different-agent".to_string();
    let wire = serde_json::to_vec(&agent).expect("serialize mutation");
    assert!(matches!(
        decode_agent_envelope(&wire),
        Err(OclaWireError::InvalidInvariant(
            "context agent_id must match from_agent_id"
        ))
    ));
}

#[test]
fn relay_and_capsule_ids_are_integrity_checked() {
    let mut relay: AgentEnvelopeV1 = serde_json::from_slice(AGENT_FIXTURE).expect("typed fixture");
    relay.budget_tokens += 1;
    let wire = serde_json::to_vec(&relay).expect("serialize mutation");
    assert!(matches!(
        decode_agent_envelope(&wire),
        Err(OclaWireError::InvalidInvariant(
            "relay_id does not match canonical relay content"
        ))
    ));

    relay.capsule_ref = format!("capsule:{}", "A".repeat(64));
    let wire = serde_json::to_vec(&relay).expect("serialize mutation");
    assert!(matches!(
        decode_agent_envelope(&wire),
        Err(OclaWireError::InvalidInvariant("invalid capsule_ref"))
    ));
}

#[test]
fn self_relay_is_wire_valid_but_gateway_policy_rejects_it() {
    let normal: AgentEnvelopeV1 = serde_json::from_slice(AGENT_FIXTURE).expect("typed fixture");
    verify_agent_gateway_admissibility(&normal).expect("cross-agent relay");

    let decoded =
        decode_agent_envelope(SELF_RELAY_FIXTURE).expect("self-relay fixture is wire-valid");
    assert_eq!(
        verify_agent_gateway_admissibility(&decoded),
        Err(OclaGatewayAdmissibilityError::SelfRelay)
    );
    assert!(
        compile_schema(AGENT_SCHEMA)
            .is_valid(&serde_json::to_value(decoded).expect("self-relay JSON")),
        "schema is transport-shaped and does not assert gateway policy"
    );
}

#[test]
fn unsupported_versions_are_distinct_from_malformed_json() {
    let mut token: CanonicalTokenEnvelopeV1 =
        serde_json::from_slice(TOKEN_FIXTURE).expect("typed fixture");
    token.schema_version = 2;
    let wire = serde_json::to_vec(&token).expect("serialize mutation");
    assert_eq!(
        decode_canonical_token_envelope(&wire),
        Err(OclaWireError::UnsupportedVersion {
            expected: 1,
            actual: 2,
        })
    );

    let mut agent: AgentEnvelopeV1 = serde_json::from_slice(AGENT_FIXTURE).expect("typed fixture");
    agent.schema_version = 2;
    let wire = serde_json::to_vec(&agent).expect("serialize mutation");
    assert_eq!(
        decode_agent_envelope(&wire),
        Err(OclaWireError::UnsupportedVersion {
            expected: 1,
            actual: 2,
        })
    );
}

fn assert_fixture_valid(fixture: &[u8], schema: &str, expected_id: &str) {
    let fixture: Value = serde_json::from_slice(fixture).expect("valid fixture JSON");
    let schema: Value = serde_json::from_str(schema).expect("valid public schema JSON");

    assert_eq!(schema["$id"], expected_id);
    assert!(
        compile_schema_value(&schema).is_valid(&fixture),
        "fixture rejected by Draft 2020-12 schema"
    );
}

fn compile_schema(schema: &str) -> JSONSchema {
    let schema: Value = serde_json::from_str(schema).expect("valid public schema JSON");
    compile_schema_value(&schema)
}

fn compile_schema_value(schema: &Value) -> JSONSchema {
    JSONSchema::options()
        .with_draft(Draft::Draft202012)
        .compile(schema)
        .expect("Draft 2020-12 schema compiles")
}

fn mutate(value: &Value, edit: impl FnOnce(&mut Value)) -> Value {
    let mut mutated = value.clone();
    edit(&mut mutated);
    mutated
}
