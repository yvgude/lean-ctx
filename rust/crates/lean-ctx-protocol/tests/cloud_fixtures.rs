// SPDX-License-Identifier: Apache-2.0

//! Golden fixtures of the Cloud wire contract (`tests/fixtures/cloud/`).
//!
//! The fixtures are the contract the engine connector and the Cloud service
//! share. They are generated here from fixed, test-only keys (seeds in
//! `vectors.json`) so every signature in them verifies. A contract change is
//! a reviewed fixture diff:
//!
//! `CLOUD_FIXTURES_WRITE=1 cargo test -p lean-ctx-protocol --test cloud_fixtures`

use base64::{Engine as _, engine::general_purpose::STANDARD as B64};
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use hmac::{Hmac, KeyInit as _, Mac as _};
use lean_ctx_protocol::ValidationError;
use lean_ctx_protocol::cloud::bus::*;
use lean_ctx_protocol::cloud::enroll::*;
use lean_ctx_protocol::cloud::knowledge::*;
use lean_ctx_protocol::cloud::lease::*;
use lean_ctx_protocol::cloud::policy::*;
use lean_ctx_protocol::cloud::status::*;
use lean_ctx_protocol::cloud::telemetry::*;
use lean_ctx_protocol::cloud::vault::*;
use lean_ctx_protocol::cloud::*;
use serde::{Deserialize as _, Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::path::PathBuf;

// Test-only key material. Never used outside fixtures and the fake service.
const ROOT_SEED: [u8; 32] = [0x52; 32];
const SIGNING_SEED: [u8; 32] = [0x53; 32];
const INSTANCE_SEED: [u8; 32] = [0x49; 32];
const POLICY_SEED: [u8; 32] = [0x4f; 32];
const ORG_SALT: [u8; 32] = [0x0a; 32];
const KX_PUBLIC: [u8; 32] = [0x58; 32];
// f3129 app keys (app_key_bind.vectors.json), test-only like the others.
const CHAT_KEY_SEED: [u8; 32] = [0x43; 32];
const OPTIN_KEY_SEED: [u8; 32] = [0x50; 32];
const TASK_PROPOSE_KEY_SEED: [u8; 32] = [0x54; 32];
const OTHER_APP_KEY_SEED: [u8; 32] = [0x5a; 32];
const DEVICE_CODE: &str = "dc_9f2c4e7a1b3d5f8e0a2c4e6f8a1b3d5e";

const T0: &str = "2026-10-20T12:00:00Z";
const T0_UNIX: i64 = 1_792_497_600;
const KID: &str = "cloud-2026-10";
const ORG: &str = "org_acme";
const INSTANCE: &str = "inst_01jb8z3k4m5n6p7q8r9s0t1v2w";
const VAULT: &str = "3f6c1e9a0b7d4c2e8f5a1b6c9d0e7f42";
const SALT_ID: &str = "salt_2026_10";
const IDENTITY: &str = "git:github.com/acme/payments";
const NONCE: &str = "n-3q9X2kLp7Vb0ZsW1";

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// Sent by the engine; the service refuses unknown fields.
    Request,
    /// Sent by the service; the engine accepts unknown fields.
    Response,
    /// A bus frame or CLI output; not an HTTP body.
    Other,
}

struct Fixture {
    name: &'static str,
    kind: Kind,
    value: Value,
}

fn key(seed: [u8; 32]) -> SigningKey {
    SigningKey::from_bytes(&seed)
}

fn public(seed: [u8; 32]) -> String {
    B64.encode(key(seed).verifying_key().as_bytes())
}

fn sign(seed: [u8; 32], message: &[u8]) -> String {
    B64.encode(key(seed).sign(message).to_bytes())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn identity_digest() -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(&ORG_SALT).expect("any key length");
    mac.update(stable_identity(IDENTITY).as_bytes());
    hex(&mac.finalize().into_bytes())
}

fn keyset() -> Value {
    let mut keyset = json!({
        "schema": KEYSET_SCHEMA,
        "keys": [{"kid": KID, "ed25519_pub": public(SIGNING_SEED), "not_after": "2027-10-20T12:00:00Z"}],
    });
    let input = signing_input_without_signature(KEYSET_SIGNATURE_DOMAIN, &keyset).unwrap();
    keyset["signature"] = Value::from(sign(ROOT_SEED, &input));
    keyset
}

fn lease_envelope() -> Value {
    let lease = json!({
        "schema": LEASE_SCHEMA,
        "lease_id": "lse_01jb8z4a2b3c4d5e6f7g8h9j0k",
        "org_id": ORG,
        "instance_id": INSTANCE,
        "plan": "team",
        "seats_limit": 25,
        "rights": [
            "knowledge.read", "knowledge.draft", "knowledge.merge", "metrics.upload",
            "receipts.upload", "policy.pull", "bus.relay", "bus.control", "sealed", "runtime"
        ],
        "issued_at": T0,
        "expires_at": "2026-10-21T12:00:00Z",
        "grace_until": "2026-10-27T12:00:00Z",
        "server_time": T0,
        "org_settings": {"metrics_upload": true, "receipts_upload": false},
    });
    let input = signing_input(LEASE_SIGNATURE_DOMAIN, &lease).unwrap();
    json!({
        "schema": LEASE_ENVELOPE_SCHEMA,
        "lease": lease,
        "kid": KID,
        "signature": sign(SIGNING_SEED, &input),
    })
}

fn fact(id: &str, statement: &str) -> Value {
    json!({"id": id, "statement": statement, "kind": "decision", "appliesTo": ["rust/src/core/cloud"]})
}

fn document() -> Value {
    let mut first = fact(
        "cloud.never-blocks",
        "Cloud never blocks a tool call; outbound items go through a durable outbox.",
    );
    first["from_contributions"] = json!(["p11"]);
    json!({
        "format": RESEARCH_CONTEXT_FORMAT,
        "facts": [
            first,
            fact("cloud.screening-first", "Every outbound item passes instance screening before it is queued."),
        ],
        "relations": [{"from": "cloud.screening-first", "to": "cloud.never-blocks", "type": "supports"}],
        "open_gaps": ["How are drafts rebased after a merge?"],
    })
}

fn patch_ops() -> Value {
    json!([
        {"op": "add_fact", "fact": fact("cloud.drafts-rebase", "Pending drafts are rebased onto the newest revision before upload.")},
        {"op": "add_relation", "relation": {"from": "cloud.drafts-rebase", "to": "cloud.never-blocks", "type": "related_to"}},
        {"op": "close_gap", "text": "How are drafts rebased after a merge?"},
    ])
}

fn metrics_day() -> Value {
    json!({
        "date": "2026-10-19",
        "vault": {"id": VAULT, "label": "payments"},
        "savings": {"tokens_requested": 182_000, "tokens_delivered": 61_000, "tokens_saved": 121_000, "tool_calls": 214, "cache_hits": 57},
        "spend": [{"model": "claude-opus-5-5", "provider": "anthropic", "requests": 41, "input_tokens": 120_000, "output_tokens": 18_000, "cached_tokens": 900_000, "list_usd": 3.25}],
        "ledger": {"reduction": 12, "edit": 3, "correction": 1, "cache_discount": 4, "other": 0},
        "guards": {"secrets_redacted": 2, "pii_redacted": 1, "injection_flags": 0, "content_withheld": 0, "paths_blocked": 0, "shell_blocked": 1, "partial_inspections": 0},
        "agents": [{"host": "claude", "sessions": 3, "tool_calls": 214}],
        "memory": {"facts_current": 48, "facts_added": 2, "lessons_linked": 1},
        "receipts": 2,
    })
}

fn receipt_document() -> Value {
    json!({"schema": "leanctx.engine-receipt/v1", "receipt_id": "rcpt_01jb8z5", "tool": "ctx_read", "tokens_saved": 1200})
}

fn filter_pack() -> Value {
    // Cloud signs the canonical JSON of the statement (sorted keys, no
    // whitespace); the engine verifies whatever bytes it receives.
    let pack = json!({
        "schema": "leanctx.filter-pack/v1",
        "pack_id": "ch-identifiers",
        "version": 3,
        "title": "Swiss identifiers",
        "patterns": [
            {"id": "ahv", "kind": "regex", "label": "ahv-number",
             "pattern": "756\\.\\d{4}\\.\\d{4}\\.\\d{2}", "action": "redact"},
            {"id": "iban-ch", "kind": "regex",
             "pattern": "CH\\d{2}(?: ?[0-9A-Z]{4}){4} ?[0-9A-Z]", "action": "block"},
        ],
    });
    lean_ctx_protocol::cloud::policy::FilterPackV1::deserialize(&pack)
        .unwrap()
        .validate()
        .unwrap();
    let statement = canonical_json(&pack).unwrap();
    json!({
        "pack_id": "ch-identifiers",
        "version": 3,
        "sha256": hex(&Sha256::digest(&statement)),
        "envelope": {
            "statement_b64": B64.encode(&statement),
            "kid": KID,
            "signature": sign(SIGNING_SEED, &filter_pack_signing_input(&statement)),
        },
    })
}

fn channel() -> String {
    format!("org:{ORG}/vault:{VAULT}/general")
}

fn error(code: &str, message: &str, extra: Value) -> Value {
    let mut detail = json!({"code": code, "message": message});
    if let (Some(detail), Value::Object(extra)) = (detail.as_object_mut(), extra) {
        detail.extend(extra);
    }
    json!({"error": detail})
}

fn fixtures() -> Vec<Fixture> {
    use Kind::*;
    let document = document();
    let ops = patch_ops();
    let receipt = receipt_document();
    let list = [
        Fixture {
            name: "error.authorization_pending",
            kind: Response,
            value: error(
                "authorization_pending",
                "Waiting for approval in the browser.",
                json!({}),
            ),
        },
        Fixture {
            name: "error.plan_required",
            kind: Response,
            value: error(
                "plan_required",
                "Cloud is part of Personal, Team and Enterprise.",
                json!({}),
            ),
        },
        Fixture {
            name: "error.instance_limit",
            kind: Response,
            value: error(
                "instance_limit",
                "All 25 instance seats of this organization are in use.",
                json!({"limit": 25, "used": 25}),
            ),
        },
        Fixture {
            name: "error.revision_conflict",
            kind: Response,
            value: error(
                "revision_conflict",
                "Knowledge moved on.",
                json!({"expected": 3, "actual": 4}),
            ),
        },
        Fixture {
            name: "error.rate_limited",
            kind: Response,
            value: error(
                "rate_limited",
                "Too many requests.",
                json!({"retry_after_s": 30}),
            ),
        },
        Fixture {
            name: "error.upgrade_required",
            kind: Response,
            value: error(
                "upgrade_required",
                "This instance is too old for Cloud.",
                json!({"min_instance": "3.12.0"}),
            ),
        },
        Fixture {
            name: "device_code.request",
            kind: Request,
            value: json!({"schema": DEVICE_CODE_REQUEST_SCHEMA, "client_id": CLOUD_ENGINE_CLIENT_ID, "instance_version": "3.12.0", "os": "macos"}),
        },
        Fixture {
            name: "device_code_labelled.request",
            kind: Request,
            value: json!({"schema": DEVICE_CODE_REQUEST_SCHEMA, "client_id": CLOUD_ENGINE_CLIENT_ID, "instance_version": "3.12.0", "os": "macos", "instance_label": "build-box"}),
        },
        Fixture {
            name: "device_code.response",
            kind: Response,
            value: json!({
                "schema": DEVICE_CODE_SCHEMA, "device_code": "dc_9f2c4e7a1b3d5f8e0a2c4e6f8a1b3d5e", "user_code": "WDJB-MJHT",
                "verification_uri": "https://cloud.leanctx.com/device",
                "verification_uri_complete": "https://cloud.leanctx.com/device?user_code=WDJB-MJHT",
                "interval": 5, "expires_in": 900,
            }),
        },
        Fixture {
            name: "device_token.request",
            kind: Request,
            value: json!({
                "schema": DEVICE_TOKEN_REQUEST_SCHEMA, "device_code": "dc_9f2c4e7a1b3d5f8e0a2c4e6f8a1b3d5e",
                "instance_public_key": public(INSTANCE_SEED), "instance_kx_public_key": B64.encode(KX_PUBLIC),
                "instance_label": "studio-mac", "instance_version": "3.12.0", "os": "macos",
            }),
        },
        Fixture {
            name: "enrollment.response",
            kind: Response,
            value: json!({
                "schema": ENROLLMENT_SCHEMA, "instance_id": INSTANCE, "org": {"id": ORG, "name": "Acme AG"},
                "lease": lease_envelope(), "keys": keyset(), "org_salt_id": SALT_ID,
            "web_url": CLOUD_DEFAULT_URL,
            }),
        },
        Fixture {
            name: "token.request",
            kind: Request,
            value: json!({
                "schema": TOKEN_REQUEST_SCHEMA, "iat": T0_UNIX, "nonce": NONCE,
                "assertion": sign(INSTANCE_SEED, &token_assertion_input(INSTANCE, T0_UNIX, NONCE)),
            }),
        },
        Fixture {
            name: "access_token.response",
            kind: Response,
            value: json!({"schema": ACCESS_TOKEN_SCHEMA, "access_token": "atl_fixture_not_a_real_token_0000000000", "expires_in": 3600}),
        },
        Fixture {
            name: "lease_envelope.response",
            kind: Response,
            value: lease_envelope(),
        },
        Fixture {
            name: "keyset.response",
            kind: Response,
            value: keyset(),
        },
        Fixture {
            name: "org_salt.response",
            kind: Response,
            value: json!({"schema": ORG_SALT_SCHEMA, "org_salt_id": SALT_ID, "salt": B64.encode(ORG_SALT)}),
        },
        Fixture {
            name: "vault_resolve.request",
            kind: Request,
            value: json!({"schema": VAULT_RESOLVE_REQUEST_SCHEMA, "org_salt_id": SALT_ID, "identity_digest": identity_digest(), "label": "payments"}),
        },
        Fixture {
            name: "vault_resolve.ref.request",
            kind: Request,
            value: json!({"schema": VAULT_RESOLVE_REQUEST_SCHEMA, "org_salt_id": SALT_ID, "identity_digest": identity_digest(), "vault": {"ns": "acme", "name": "payments"}}),
        },
        Fixture {
            name: "error.forbidden_right",
            kind: Response,
            value: error(
                "forbidden_right",
                "This instance may not contribute to acme/payments.",
                json!({}),
            ),
        },
        Fixture {
            name: "error.upload_disabled",
            kind: Response,
            value: error(
                "upload_disabled",
                "The organization has switched receipts upload off.",
                json!({"kind": "receipts"}),
            ),
        },
        Fixture {
            name: "vault.response",
            kind: Response,
            value: json!({"schema": VAULT_SCHEMA, "vault_id": VAULT, "vault": {"ns": "acme", "name": "payments"}, "sealed": false, "label": "payments"}),
        },
        Fixture {
            name: "knowledge_revision.response",
            kind: Response,
            value: json!({"schema": KNOWLEDGE_REVISION_SCHEMA, "vault_id": VAULT, "revision": 3, "document": document, "updated_at": T0}),
        },
        Fixture {
            name: "proposal.request",
            kind: Request,
            value: json!({
                "schema": DRAFT_PROPOSAL_SCHEMA, "state": "draft",
                "summary": "Session learnings: draft rebasing",
                "body": "Two decisions from the connector session.",
                "sources": ["session:ses_01jb8z6"],
                "context_patch": {"base_revision": 3, "ops": ops},
                "session_ref": "ses_01jb8z6",
                "engine_screening": {"scanner_version": 1, "secrets_removed": 1, "pii_removed": 0, "items_dropped": 1},
            }),
        },
        Fixture {
            name: "proposal.import.request",
            kind: Request,
            value: json!({
                "schema": DRAFT_PROPOSAL_SCHEMA, "state": "draft",
                "summary": "Import: outbox ordering from docs and history",
                "sources": ["import:docs:docs/cloud/outbox.md#ordering", "import:git:3f6c1e9a0b7d4c2e8f5a1b6c9d0e7f423f6c1e9a"],
                "context_patch": {"base_revision": 3, "ops": ops},
                "session_ref": "ses_01jb8z6",
                "engine_screening": {"scanner_version": 1, "secrets_removed": 0, "pii_removed": 0, "items_dropped": 0},
                "origin": "import",
            }),
        },
        Fixture {
            name: "error.proposal_state",
            kind: Response,
            value: error(
                "invalid_state",
                "Proposal 11 is already merged.",
                json!({"reason": "proposal_state", "number": 11}),
            ),
        },
        Fixture {
            name: "proposal_ack.response",
            kind: Response,
            value: json!({"schema": PROPOSAL_ACK_SCHEMA, "number": 12, "state": "draft"}),
        },
        Fixture {
            name: "proposal_list.response",
            kind: Response,
            value: json!({
                "schema": PROPOSAL_LIST_SCHEMA, "vault_id": VAULT,
                "proposals": [
                    {"number": 12, "state": "draft", "summary": "Session learnings: draft rebasing", "base_revision": 3, "created_at": T0, "mine": true},
                    {"number": 11, "state": "merged", "summary": "Screening decision", "base_revision": 2, "created_at": "2026-10-19T09:00:00Z", "merged_revision": 3, "mine": false},
                ],
            }),
        },
        Fixture {
            name: "metrics.request",
            kind: Request,
            value: json!({
                "schema": METRICS_SCHEMA,
                "device": {"id": "0b7d1c3e-9f2a-4c5b-8d6e-1a2b3c4d5e6f", "label": "studio-mac", "engine_version": "3.12.0", "os": "macos"},
                "days": [metrics_day()],
            }),
        },
        Fixture {
            name: "metrics_ack.response",
            kind: Response,
            value: json!({"schema": METRICS_ACK_SCHEMA, "accepted": 1}),
        },
        Fixture {
            name: "receipts.request",
            kind: Request,
            value: json!({
                "schema": RECEIPTS_SCHEMA,
                "receipts": [{"receipt_id": "rcpt_01jb8z5", "kind": "engine", "vault_id": VAULT, "created_at": T0, "digest": receipt_digest(&receipt).unwrap(), "document": receipt}],
            }),
        },
        Fixture {
            name: "receipts_ack.response",
            kind: Response,
            value: json!({"schema": RECEIPTS_ACK_SCHEMA, "accepted": 1, "duplicates": 0}),
        },
        Fixture {
            name: "policy_bundle.response",
            kind: Response,
            value: json!({"schema": POLICY_BUNDLE_SCHEMA, "filter_packs": [filter_pack()]}),
        },
        Fixture {
            name: "bus_lease.request",
            kind: Request,
            value: json!({
                "schema": BUS_LEASE_REQUEST_SCHEMA, "resource_kind": "file",
                "resource_ref": hex(&Sha256::digest(b"rust/src/core/cloud/outbox.rs")),
                "vault_id": VAULT, "owner_agent_id": "claude-3e", "duration_ms": 600_000,
            }),
        },
        Fixture {
            name: "bus_lease_renew.request",
            kind: Request,
            value: json!({"schema": BUS_LEASE_RENEW_REQUEST_SCHEMA, "fencing_token": 9, "duration_ms": 600_000}),
        },
        Fixture {
            name: "bus_lease.granted.response",
            kind: Response,
            value: json!({"schema": BUS_LEASE_SCHEMA, "granted": true, "lease_id": "bl_01jb8zc", "fencing_token": 9, "expires_at": "2026-10-20T12:10:00Z", "server_time": T0}),
        },
        Fixture {
            name: "bus_lease.refused.response",
            kind: Response,
            value: json!({"schema": BUS_LEASE_SCHEMA, "granted": false, "holder": {"agent_id": "codex-85", "instance_id": "inst_01jb8z9", "expires_at": "2026-10-20T12:07:00Z"}, "server_time": T0}),
        },
        Fixture {
            name: "error.invalid_state",
            kind: Response,
            value: error(
                "invalid_state",
                "A key rotation is pending; seal with the required epoch.",
                json!({"reason": "epoch_rotating"}),
            ),
        },
        Fixture {
            name: "status.connected",
            kind: Other,
            value: json!({
                "available": true, "connected": true, "instance": {"id": INSTANCE, "label": "studio-mac"},
                "org": {"name": "Acme AG"}, "last_seen": T0, "url": CLOUD_DEFAULT_URL,
                "drafts": {"queued": 2, "refused": 0},
                "filter_packs": [{"pack_id": "ch-identifiers", "version": 3}],
                "policy_refusal": null,
                "web_url": CLOUD_DEFAULT_URL,
                "bus_controls": [{"target_agent": "claude-3e", "action": "hold", "target_ref": null,
                                  "reason": "Owner review of the contract.", "issued_by": "admin@acme", "since": T0}],
            }),
        },
        Fixture {
            name: "status.lapsed",
            kind: Other,
            value: json!({
                "available": true, "connected": false, "instance": {"id": INSTANCE, "label": "studio-mac"},
                "org": {"name": "Acme AG"}, "last_seen": T0, "url": CLOUD_DEFAULT_URL,
                "drafts": {"queued": 0, "refused": 0}, "filter_packs": [],
                "policy_refusal": null, "web_url": CLOUD_DEFAULT_URL,
                "bus_controls_inactive": "lease_lapsed",
            }),
        },
        Fixture {
            name: "status.not_connected",
            kind: Other,
            value: serde_json::to_value(CloudCliStatusV1::not_connected(None)).unwrap(),
        },
    ];
    let mut list = list.into_iter().collect::<Vec<_>>();
    list.extend(task_fixtures());
    list.extend(knowledge_fixtures());
    list.push(metrics_cloud_fixture());
    list.extend(profile_fixtures());
    list.extend(threshold_fixtures());
    list.extend(code_graph_fixtures());
    list.extend(optin_fixtures());
    list.extend(app_key_fixtures());
    list.push(Fixture {
        name: "vectors",
        kind: Other,
        value: vectors(),
    });
    list
}

/// C0 1.9 section 1: tasks, claims and results.
fn task_fixtures() -> Vec<Fixture> {
    use Kind::*;
    use lean_ctx_protocol::cloud::tasks::*;
    let claim = json!({"lease_id": "lse_01jbtask7", "fencing_token": 7});
    let anchor = json!({
        "path": "src/retry.rs", "symbol": "retry::budget",
        "digest": format!("sha256:{}", hex(&Sha256::digest(b"fn budget() -> u32 { 3 }\n"))),
        "lang": "rust",
    });
    let person = json!({"kind": "person", "label": "Ada", "mine": false});
    let me = json!({"kind": "instance", "label": "ada-laptop", "agent": "claude-3e", "owner": "Ada", "model": "claude-opus-5-5", "mine": true});
    let receipt = hex(&Sha256::digest(b"receipt-1"));
    let evidence = json!([
        {"kind": "commit", "reference": "9f01cf32a3"},
        {"kind": "receipt", "reference": receipt},
        {"kind": "file_anchor", "anchor": anchor},
    ]);
    let evidence_view = json!([
        {"kind": "commit", "reference": "9f01cf32a3", "verified": false},
        {"kind": "receipt", "reference": receipt, "verified": true},
        {"kind": "file_anchor", "anchor": anchor, "verified": false},
    ]);
    let acceptance = "A fact anchored on retry::budget states the budget and why";
    let summary = "Budget is 3 retries, see retry.rs";
    vec![
        Fixture {
            name: "task_propose.request",
            kind: Request,
            value: json!({
                "schema": TASK_PROPOSE_SCHEMA, "title": "Explain the retry budget",
                "acceptance": acceptance, "role": "backend", "origin": {"kind": "agent"},
                "session_ref": "ses_01jb8z6",
            }),
        },
        Fixture {
            name: "task_propose.contradiction.request",
            kind: Request,
            value: json!({
                "schema": TASK_PROPOSE_SCHEMA, "title": "Settle the retry budget: 3 or 5?",
                "acceptance": "One of the two facts is superseded, with evidence",
                "origin": {"kind": "contradiction", "facts": ["f-retry-3", "f-retry-5"]},
                "session_ref": "ses_01jb8z6",
            }),
        },
        Fixture {
            name: "task_ack.response",
            kind: Response,
            value: json!({"schema": TASK_ACK_SCHEMA, "number": 7, "state": "proposed"}),
        },
        Fixture {
            name: "task_claim.request",
            kind: Request,
            value: json!({"schema": TASK_CLAIM_REQUEST_SCHEMA, "agent": "claude-3e", "session_ref": "ses_01jb8z6", "minutes": 15, "optin_id": "optin_01jb8z"}),
        },
        Fixture {
            name: "task_claim_renew.request",
            kind: Request,
            value: json!({"schema": TASK_CLAIM_RENEW_SCHEMA, "claim": claim, "minutes": 15}),
        },
        Fixture {
            name: "task_claim.response",
            kind: Response,
            value: json!({"schema": TASK_CLAIM_ACK_SCHEMA, "number": 7, "claim": claim, "until": "2026-10-20T12:15:00Z"}),
        },
        Fixture {
            name: "task_release.request",
            kind: Request,
            value: json!({"schema": TASK_RELEASE_SCHEMA, "claim": claim, "reason": "Needs access to the payments sandbox"}),
        },
        Fixture {
            name: "task_update.request",
            kind: Request,
            value: json!({"schema": TASK_UPDATE_SCHEMA, "claim": claim, "summary": "Found the budget constant; checking the call sites"}),
        },
        Fixture {
            name: "task_result.request",
            kind: Request,
            value: json!({"schema": TASK_RESULT_SCHEMA, "claim": claim, "summary": summary, "evidence": evidence, "usage": {"tokens": 48210, "usd_micros": 412000}}),
        },
        Fixture {
            name: "task.claimed.response",
            kind: Response,
            value: json!({
                "schema": TASK_SCHEMA, "vault_id": VAULT, "number": 7, "state": "claimed",
                "title": "Explain the retry budget", "acceptance": acceptance,
                "role": "backend", "origin": {"kind": "agent"},
                "created_by": me, "created_at": T0, "approved_by": person,
                "claim": {"holder": me, "since": T0, "until": "2026-10-20T12:15:00Z"},
                "updates": [{"by": me, "at": "2026-10-20T12:20:00Z", "summary": "Found the budget constant; checking the call sites"}],
            }),
        },
        Fixture {
            name: "task.done.response",
            kind: Response,
            value: json!({
                "schema": TASK_SCHEMA, "vault_id": VAULT, "number": 7, "state": "done",
                "title": "Explain the retry budget", "acceptance": acceptance,
                "role": "backend", "origin": {"kind": "agent"},
                "created_by": me, "created_at": T0, "approved_by": person,
                "result": {"by": me, "at": "2026-10-20T12:40:00Z", "proposal": 13, "summary": summary, "evidence": evidence_view},
                "decision": {"outcome": "accepted", "decided_by": {"kind": "person", "label": "Ada"}, "at": "2026-10-20T14:00:00Z"},
            }),
        },
        Fixture {
            // Pro: the auto-merge threshold accepted the result.
            name: "task.threshold.response",
            kind: Response,
            value: json!({
                "schema": TASK_SCHEMA, "vault_id": VAULT, "number": 8, "state": "done",
                "title": "Note the release checklist", "acceptance": "A procedure fact",
                "origin": {"kind": "person"}, "created_by": person, "created_at": T0,
                "result": {"by": me, "at": "2026-10-20T12:40:00Z", "proposal": 14, "summary": "Checklist recorded",
                           "evidence": [{"kind": "fact", "reference": "f-release-checklist", "verified": false}]},
                "decision": {"outcome": "accepted", "decided_by": {"kind": "threshold"}, "at": "2026-10-20T15:00:00Z"},
            }),
        },
        Fixture {
            name: "task_list.response",
            kind: Response,
            value: json!({
                "schema": TASK_LIST_SCHEMA, "vault_id": VAULT,
                "tasks": [
                    {"number": 7, "state": "claimed", "title": "Explain the retry budget", "role": "backend", "origin": "agent", "updated_at": T0, "mine": true},
                    {"number": 9, "state": "open", "title": "Settle the retry budget: 3 or 5?", "origin": "contradiction", "updated_at": T0, "mine": false},
                ],
            }),
        },
        Fixture {
            name: "error.task_claimed",
            kind: Response,
            value: error(
                "invalid_state",
                "Another agent holds this task.",
                json!({"reason": "task_claimed", "number": 7}),
            ),
        },
        Fixture {
            name: "error.stale_claim",
            kind: Response,
            value: error(
                "invalid_state",
                "This claim is no longer current.",
                json!({"reason": "stale_claim", "number": 7}),
            ),
        },
        Fixture {
            name: "error.task_not_permitted",
            kind: Response,
            value: error(
                "forbidden_right",
                "This agent may not work on this task.",
                json!({"reason": "not_permitted", "number": 7}),
            ),
        },
        Fixture {
            name: "error.task_not_permitted_role",
            kind: Response,
            value: error(
                "forbidden_right",
                "Agent claude-a works as role web; this task needs role backend.",
                json!({"reason": "not_permitted", "number": 7, "check": "role",
                       "role": "backend", "agent_role": "web"}),
            ),
        },
        Fixture {
            name: "error.vetoed",
            kind: Response,
            value: error(
                "forbidden_right",
                "A veto holds this agent.",
                json!({"reason": "vetoed"}),
            ),
        },
    ]
}

const OUTBOX_ENQUEUE: &str =
    "pub fn enqueue(item: OutboundItem) -> Result<()> {\n    outbox().push(item)\n}\n";

fn anchor(path: &str, symbol: &str, content: &str) -> Value {
    json!({"path": path, "symbol": symbol, "digest": anchor::anchor_digest(content.as_bytes()), "lang": "rust"})
}

fn wiki_document() -> Value {
    let mut document = document();
    let mut gotcha = fact(
        "cloud.outbox-order",
        "The outbox keeps upload order per project; reordering breaks expected_revision.",
    );
    gotcha["kind"] = json!("gotcha");
    gotcha["anchors"] = json!([anchor(
        "rust/src/core/cloud/outbox.rs",
        "cloud::outbox::enqueue",
        OUTBOX_ENQUEUE
    )]);
    document["facts"].as_array_mut().unwrap().push(gotcha);
    document["relations"].as_array_mut().unwrap().push(
        json!({"from": "cloud.outbox-order", "to": "cloud.never-blocks", "type": "explains"}),
    );
    document["pages"] = json!([{
        "id": "cloud.outbound",
        "title": "Outbound path",
        "items": [
            {"kind": "prose", "text": "Everything an instance sends to Cloud takes one path."},
            {"kind": "fact", "id": "cloud.screening-first"},
            {"kind": "fact", "id": "cloud.never-blocks"},
            {"kind": "fact", "id": "cloud.outbox-order"},
        ],
    }]);
    document
}

/// Section 3: pages, anchors, fact states, quarantine and rollback.
fn knowledge_fixtures() -> Vec<Fixture> {
    use Kind::*;
    let wiki = wiki_document();
    let to_verify = json!({
        "fact": "cloud.outbox-order", "state": "to_verify", "reasons": ["anchor_changed"],
        "since": "2026-10-21T09:00:00Z",
    });
    let contradicted = json!({
        "fact": "cloud.never-blocks", "state": "contradicted", "reasons": ["contradiction"],
        "contradicted_by": ["cloud.screening-first"], "since": "2026-10-21T09:00:00Z",
    });
    vec![
        Fixture {
            name: "knowledge_revision.wiki.response",
            kind: Response,
            value: json!({
                "schema": KNOWLEDGE_REVISION_SCHEMA, "vault_id": VAULT, "revision": 4, "document": wiki,
                "updated_at": T0, "quarantined_after": 1, "fact_states": [contradicted, to_verify],
            }),
        },
        Fixture {
            name: "fact_states.request",
            kind: Request,
            value: json!({
                "schema": facts::FACT_STATE_REPORT_SCHEMA, "revision": 4,
                "states": [
                    contradicted,
                    {
                        "fact": "cloud.outbox-order", "state": "to_verify", "reasons": ["anchor_moved"],
                        "moved_to": anchor("rust/src/core/cloud/outbox/queue.rs", "cloud::outbox::queue::enqueue", OUTBOX_ENQUEUE),
                        "since": "2026-10-21T09:00:00Z",
                    },
                ],
            }),
        },
        Fixture {
            name: "fact_states_ack.response",
            kind: Response,
            value: json!({"schema": facts::FACT_STATE_ACK_SCHEMA, "revision": 4, "to_verify": 1, "contradicted": 1}),
        },
        Fixture {
            name: "graph.response",
            kind: Response,
            value: json!({
                "schema": graph::GRAPH_SCHEMA, "vault_id": VAULT, "revision": 4,
                "nodes": [
                    {"id": "actor:claude-3e", "kind": "actor", "label": "claude-3e"},
                    {"id": "code:rust/src/core/cloud/outbox.rs#cloud::outbox::enqueue", "kind": "code", "label": "cloud::outbox::enqueue"},
                    {"id": "fact:cloud.never-blocks", "kind": "knowledge", "label": "Cloud never blocks a tool call"},
                    {"id": "fact:cloud.outbox-order", "kind": "knowledge", "label": "The outbox keeps upload order per project"},
                    {"id": "page:cloud.outbound", "kind": "knowledge", "label": "Outbound path"},
                    {"id": "proposal:12", "kind": "work", "label": "Outbox ordering"},
                ],
                "edges": [
                    {"from": "actor:claude-3e", "to": "proposal:12", "type": "worked_on"},
                    {"from": "fact:cloud.outbox-order", "to": "code:rust/src/core/cloud/outbox.rs#cloud::outbox::enqueue", "type": "applies_to"},
                    {"from": "fact:cloud.outbox-order", "to": "fact:cloud.never-blocks", "type": "explains"},
                    {"from": "page:cloud.outbound", "to": "fact:cloud.never-blocks", "type": "shows"},
                    {"from": "page:cloud.outbound", "to": "fact:cloud.outbox-order", "type": "shows"},
                    {"from": "proposal:12", "to": "fact:cloud.outbox-order", "type": "changed"},
                ],
            }),
        },
        Fixture {
            name: "error.quarantine_blocked",
            kind: Response,
            value: error(
                "invalid_state",
                "Revision 5 is quarantined; release or roll it back first.",
                json!({"reason": "quarantine_blocked"}),
            ),
        },
        Fixture {
            name: "anchor_digest.vectors",
            kind: Other,
            value: anchor_digest_vectors(),
        },
    ]
}

/// Section 4: engine opt-in and its claim errors.
fn optin_fixtures() -> Vec<Fixture> {
    use Kind::*;
    vec![
        Fixture {
            name: "instance_optin.response",
            kind: Response,
            value: json!({
                "schema": optin::ENGINE_OPTIN_SCHEMA, "optin_id": "opt_01jbq7k2", "active": true,
                "vaults": [VAULT],
                "networks": [{"kind": "org", "channel": channel()}],
                "daily_budget": {"tasks": 5, "tokens": 2_000_000, "usd_micros": 20_000_000},
                "used_today": {"day": "2026-10-20", "tasks": 2, "tokens": 310_400, "usd_micros": 1_870_000},
                "updated_at": "2026-10-20T08:00:00Z",
            }),
        },
        // Cloud task #19: every limit left out is no limit, the task count too.
        Fixture {
            name: "instance_optin.unlimited.response",
            kind: Response,
            value: json!({
                "schema": optin::ENGINE_OPTIN_SCHEMA, "optin_id": "opt_01jbq7k4", "active": true,
                "vaults": [VAULT], "networks": [],
                "daily_budget": {},
                "used_today": {"day": "2026-10-20", "tasks": 140, "tokens": 9_310_400, "usd_micros": 41_870_000},
                "updated_at": "2026-10-20T08:00:00Z",
            }),
        },
        Fixture {
            name: "instance_optin.stopped.response",
            kind: Response,
            value: json!({
                "schema": optin::ENGINE_OPTIN_SCHEMA, "optin_id": "opt_01jbq7k3", "active": false,
                "vaults": [VAULT], "networks": [],
                "daily_budget": {"tasks": 5},
                "used_today": {"day": "2026-10-20", "tasks": 2, "tokens": 310_400, "usd_micros": 1_870_000},
                "stopped_at": T0, "updated_at": T0,
            }),
        },
        Fixture {
            name: "instance_optin.none.response",
            kind: Response,
            value: json!({
                "schema": optin::ENGINE_OPTIN_SCHEMA, "active": false, "vaults": [], "networks": [],
                "daily_budget": {"tasks": 5},
                "used_today": {"day": "2026-10-20", "tasks": 0, "tokens": 0, "usd_micros": 0},
            }),
        },
        Fixture {
            name: "instance_optin_stop.request",
            kind: Request,
            value: json!({"schema": optin::ENGINE_OPTIN_STOP_SCHEMA, "active": false}),
        },
        Fixture {
            name: "error.owner_only",
            kind: Response,
            value: error(
                "forbidden_right",
                "Only the owner turns an opt-in on.",
                json!({"reason": "owner_only"}),
            ),
        },
        Fixture {
            name: "error.optin_changed",
            kind: Response,
            value: error(
                "invalid_state",
                "The opt-in changed; read it again before pulling.",
                json!({"reason": "optin_changed"}),
            ),
        },
        Fixture {
            name: "error.optin_inactive",
            kind: Response,
            value: error(
                "invalid_state",
                "This instance is not opted in.",
                json!({"reason": "optin_inactive"}),
            ),
        },
        Fixture {
            name: "error.optin_scope",
            kind: Response,
            value: error(
                "forbidden_right",
                "The opt-in does not cover this vault.",
                json!({"reason": "optin_scope"}),
            ),
        },
        // p37 docs/API.md (!298), POST /v1/vaults/{pid}/tasks with origin
        // person: no valid task-propose key signature, or a stale signed_at.
        Fixture {
            name: "error.person_unproven",
            kind: Response,
            value: error(
                "forbidden_right",
                "The person's signature on this task is missing or invalid.",
                json!({"reason": "person_unproven"}),
            ),
        },
        Fixture {
            name: "error.signature_expired",
            kind: Response,
            value: error(
                "forbidden_right",
                "The signature on this task is too old.",
                json!({"reason": "signature_expired"}),
            ),
        },
        Fixture {
            name: "error.daily_budget",
            kind: Response,
            value: error(
                "rate_limited",
                "Today's task budget is used up.",
                json!({"reason": "daily_budget", "limit": 5, "used": 5, "retry_after_s": 43_200}),
            ),
        },
    ]
}

/// Section 5: profiles and reputation.
fn b64url(bytes: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

fn app_key_seed(kind: app_keys::AppKeyKind) -> [u8; 32] {
    match kind {
        app_keys::AppKeyKind::Chat => CHAT_KEY_SEED,
        app_keys::AppKeyKind::Optin => OPTIN_KEY_SEED,
        app_keys::AppKeyKind::TaskPropose => TASK_PROPOSE_KEY_SEED,
    }
}

fn announcement(kind: app_keys::AppKeyKind, seed: [u8; 32]) -> app_keys::AppKeyAnnouncementV1 {
    app_keys::AppKeyAnnouncementV1::new(kind, key(seed).verifying_key().as_bytes())
}

/// All three app keys, sorted by kind.
fn announced() -> Vec<app_keys::AppKeyAnnouncementV1> {
    app_keys::AppKeyKind::ALL
        .into_iter()
        .map(|kind| announcement(kind, app_key_seed(kind)))
        .collect()
}

/// The offer of `kind`'s key from `seed`, its own bind signature included.
fn app_key_offer(kind: app_keys::AppKeyKind, seed: [u8; 32]) -> Value {
    let a = announcement(kind, seed);
    let input =
        app_keys::app_key_offer_bind_input(kind, &public(INSTANCE_SEED), &a.key_id, &a.public_key)
            .unwrap();
    json!({
        "alg": a.alg, "public_key": a.public_key, "key_id": a.key_id,
        "bind_signature": b64url(&key(seed).sign(&input).to_bytes()),
    })
}

/// The identity point: a small-order Ed25519 public key.
const SMALL_ORDER_KEY: [u8; 32] = {
    let mut key = [0u8; 32];
    key[0] = 1;
    key
};

/// `R` = the base point, `s` = 1. Under [`SMALL_ORDER_KEY`] a plain
/// `verify` accepts it for every message (`[s]B = R + [k]A` with `A` the
/// identity); `verify_strict` refuses the small-order key.
fn small_order_signature() -> [u8; 64] {
    let mut signature = [0x66u8; 64];
    signature[0] = 0x58;
    signature[32..].fill(0);
    signature[32] = 1;
    signature
}

/// The S3 signature of the instance key over `keys` for `device_code`.
fn app_keys_bind_signature(device_code: &str, keys: &[app_keys::AppKeyAnnouncementV1]) -> String {
    let input =
        app_keys::app_key_bind_signing_input(device_code, &public(INSTANCE_SEED), keys).unwrap();
    b64url(&key(INSTANCE_SEED).sign(&input).to_bytes())
}

fn device_code_app_keys_request() -> Value {
    json!({
        "schema": DEVICE_CODE_REQUEST_SCHEMA, "client_id": CLOUD_ENGINE_CLIENT_ID,
        "instance_version": "3.12.9", "os": "macos", "app_keys": announced(),
    })
}

/// The token request binding `keys` (offers and S3 over the same set).
fn device_token_app_keys_request(keys: &[app_keys::AppKeyAnnouncementV1]) -> Value {
    let mut request = json!({
        "schema": DEVICE_TOKEN_REQUEST_SCHEMA, "device_code": DEVICE_CODE,
        "instance_public_key": public(INSTANCE_SEED), "instance_kx_public_key": B64.encode(KX_PUBLIC),
        "instance_label": "studio-mac", "instance_version": "3.12.9", "os": "macos",
        "app_keys_bind_signature": app_keys_bind_signature(DEVICE_CODE, keys),
    });
    for a in keys {
        request[a.kind.request_member()] = app_key_offer(a.kind, app_key_seed(a.kind));
    }
    request
}

fn app_key_error(field: &str, message: &str, reason: Option<&str>) -> Value {
    let mut extra = json!({"field": field});
    if let Some(reason) = reason {
        extra["reason"] = Value::from(reason);
    }
    error(app_keys::APP_KEY_ERROR_CODE, message, extra)
}

/// Every 400 the Cloud answers in f3129 S2/S3, each with the request that
/// draws it. `announced` is the `app_keys` the Cloud stored with the code.
fn app_key_negative_vectors() -> Value {
    use app_keys::AppKeyKind::*;
    let code_request = device_code_app_keys_request();
    let code_case = |case: &str, mutate: &dyn Fn(&mut Value)| {
        let mut request = code_request.clone();
        mutate(&mut request);
        json!({
            "case": case, "route": "POST /v1/device/code", "request": request,
            "response": app_key_error("app_keys", "app_keys is invalid.", None),
        })
    };
    let all = announced();
    let token_request = device_token_app_keys_request(&all);
    let bind_invalid = app_key_error(
        "app_keys_bind_signature",
        "app_keys_bind_signature does not verify.",
        Some(app_keys::REASON_APP_KEY_BIND_INVALID),
    );
    let not_announced = |member: &str| {
        app_key_error(
            member,
            "The offered app keys differ from the announced ones.",
            Some(app_keys::REASON_APP_KEY_NOT_ANNOUNCED),
        )
    };
    let token_case = |case: &str,
                      announced: &[app_keys::AppKeyAnnouncementV1],
                      request: Value,
                      response: Value| {
        json!({
            "case": case, "route": "POST /v1/device/token", "announced": announced,
            "request": request, "response": response,
        })
    };
    let without = |members: &[&str]| {
        let mut request = token_request.clone();
        for member in members {
            request.as_object_mut().unwrap().remove(*member);
        }
        request
    };
    let mut other_code = token_request.clone();
    other_code["app_keys_bind_signature"] =
        Value::from(app_keys_bind_signature("dc_another_device_code", &all));
    let mut by_app_key = token_request.clone();
    by_app_key["app_keys_bind_signature"] = Value::from(b64url(
        &key(CHAT_KEY_SEED)
            .sign(
                &app_keys::app_key_bind_signing_input(DEVICE_CODE, &public(INSTANCE_SEED), &all)
                    .unwrap(),
            )
            .to_bytes(),
    ));
    let mut offer_forged = token_request.clone();
    offer_forged["chat_key"]["bind_signature"] =
        app_key_offer(Chat, OTHER_APP_KEY_SEED)["bind_signature"].clone();
    let mut swapped = token_request.clone();
    swapped["chat_key"] = app_key_offer(Chat, OTHER_APP_KEY_SEED);
    let mut weak = all.clone();
    weak[0] = app_keys::AppKeyAnnouncementV1::new(Chat, &SMALL_ORDER_KEY);
    let mut small_order = device_token_app_keys_request(&weak);
    small_order["chat_key"] = json!({
        "alg": weak[0].alg, "public_key": weak[0].public_key, "key_id": weak[0].key_id,
        "bind_signature": b64url(&small_order_signature()),
    });
    let mut unknown_code = token_request.clone();
    unknown_code["device_code"] = json!("dc_unknown_device_code");
    unknown_code["app_keys_bind_signature"] =
        Value::from(app_keys_bind_signature("dc_unknown_device_code", &all));
    let code_refused = || {
        error(
            "expired_token",
            "The code is unknown, expired or used; start again.",
            json!({}),
        )
    };
    let mut offer_unknown = token_request.clone();
    offer_unknown["chat_key"]["scope"] = json!("person_publish.v1");
    let two = all[..2].to_vec();
    let mut extra = device_token_app_keys_request(&two);
    extra["task_propose_key"] = app_key_offer(TaskPropose, TASK_PROPOSE_KEY_SEED);
    json!([
        code_case("unknown_kind", &|r| r["app_keys"][0]["kind"] =
            json!("share")),
        code_case("duplicate_kind", &|r| r["app_keys"][1] =
            r["app_keys"][0].clone()),
        code_case("unsorted", &|r| r["app_keys"]
            .as_array_mut()
            .unwrap()
            .reverse()),
        code_case("too_many", &|r| {
            let first = r["app_keys"][0].clone();
            r["app_keys"].as_array_mut().unwrap().push(first);
        }),
        code_case("wrong_alg", &|r| r["app_keys"][0]["alg"] = json!("ed448")),
        code_case("padded_public_key", &|r| {
            let padded = format!("{}=", r["app_keys"][0]["public_key"].as_str().unwrap());
            r["app_keys"][0]["public_key"] = Value::from(padded);
        }),
        code_case("standard_base64_public_key", &|r| {
            // Only the alphabet is wrong: no padding, a key with `-`/`_`.
            let a = (1..=u8::MAX)
                .map(|byte| announcement(Chat, [byte; 32]))
                .find(|a| a.public_key.contains(['-', '_']))
                .unwrap();
            r["app_keys"][0] = json!({
                "kind": "chat", "alg": a.alg, "key_id": a.key_id,
                "public_key": a.public_key.replace('-', "+").replace('_', "/"),
            });
        }),
        code_case("noncanonical_trailing_bits", &|r| {
            // 32 bytes leave two unused low bits in the last character.
            let mut key = r["app_keys"][0]["public_key"].as_str().unwrap().to_owned();
            let alphabet = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
            let index = alphabet.find(key.pop().unwrap()).unwrap() | 1;
            key.push(alphabet.as_bytes()[index] as char);
            r["app_keys"][0]["public_key"] = Value::from(key);
        }),
        code_case("unknown_member", &|r| r["app_keys"][0]["label"] =
            json!("Chat messages")),
        code_case("short_public_key", &|r| {
            r["app_keys"][0]["public_key"] = Value::from(b64url(&[0x43; 31]));
        }),
        code_case("key_id_mismatch", &|r| {
            r["app_keys"][0]["key_id"] = r["app_keys"][1]["key_id"].clone();
        }),
        token_case(
            "bind_signature_missing",
            &all,
            without(&["app_keys_bind_signature"]),
            bind_invalid.clone()
        ),
        token_case(
            "nothing_offered",
            &all,
            without(&[
                "app_keys_bind_signature",
                "chat_key",
                "optin_key",
                "task_propose_key"
            ]),
            bind_invalid.clone(),
        ),
        token_case(
            "bind_signature_other_device_code",
            &all,
            other_code,
            bind_invalid.clone()
        ),
        token_case(
            "bind_signature_not_by_instance_key",
            &all,
            by_app_key,
            bind_invalid.clone()
        ),
        token_case(
            "offer_bind_signature_forged",
            &all,
            offer_forged,
            app_key_error(
                "chat_key.bind_signature",
                "chat_key.bind_signature does not verify.",
                None,
            ),
        ),
        token_case(
            "announced_key_not_offered",
            &all,
            without(&["task_propose_key"]),
            not_announced("task_propose_key")
        ),
        token_case(
            "offered_key_swapped",
            &all,
            swapped,
            not_announced("chat_key")
        ),
        token_case(
            "offered_key_not_announced",
            &two,
            extra,
            not_announced("task_propose_key")
        ),
        token_case(
            "offers_without_announcement",
            &[],
            without(&["app_keys_bind_signature"]),
            not_announced("chat_key")
        ),
        token_case(
            "signature_without_announcement",
            &[],
            without(&["chat_key", "optin_key", "task_propose_key"]),
            bind_invalid.clone()
        ),
        token_case(
            "offer_unknown_member",
            &all,
            offer_unknown,
            app_key_error("chat_key", "chat_key is invalid.", None)
        ),
        token_case(
            "offer_small_order_key",
            &weak,
            small_order,
            app_key_error(
                "chat_key.bind_signature",
                "chat_key.bind_signature does not verify.",
                None,
            ),
        ),
        token_case("device_code_unknown", &[], unknown_code, code_refused()),
        token_case(
            "device_code_expired",
            &all,
            token_request.clone(),
            code_refused()
        ),
        token_case(
            "device_code_used",
            &all,
            token_request.clone(),
            code_refused()
        ),
    ])
}

fn app_key_bind_vectors() -> Value {
    let all = announced();
    let statement = app_keys::app_key_bind_statement(DEVICE_CODE, &public(INSTANCE_SEED), &all);
    let input =
        app_keys::app_key_bind_signing_input(DEVICE_CODE, &public(INSTANCE_SEED), &all).unwrap();
    let per_key = all
        .iter()
        .map(|a| {
            let input = app_keys::app_key_offer_bind_input(
                a.kind,
                &public(INSTANCE_SEED),
                &a.key_id,
                &a.public_key,
            )
            .unwrap();
            (
                a.kind.as_str().to_owned(),
                Value::from(String::from_utf8(input).unwrap()),
            )
        })
        .collect::<serde_json::Map<_, _>>();
    json!({
        "note": "Test-only keys; never valid outside fixtures and the fake Cloud service.",
        "keys": {
            "instance_seed_hex": hex(&INSTANCE_SEED), "instance_ed25519_pub": public(INSTANCE_SEED),
            "chat_seed_hex": hex(&CHAT_KEY_SEED),
            "optin_seed_hex": hex(&OPTIN_KEY_SEED),
            "task_propose_seed_hex": hex(&TASK_PROPOSE_KEY_SEED),
            "other_app_key_seed_hex": hex(&OTHER_APP_KEY_SEED),
        },
        "feature": app_keys::FEATURE_DEVICE_APP_KEYS,
        "device_code": DEVICE_CODE,
        "device_code_sha256": hex(&Sha256::digest(DEVICE_CODE.as_bytes())),
        "statement": statement,
        "signing_input": String::from_utf8(input).unwrap(),
        "app_keys_bind_signature": app_keys_bind_signature(DEVICE_CODE, &all),
        "offer_bind_inputs": per_key,
        "labels": app_keys::AppKeyKind::ALL
            .into_iter()
            .map(|kind| (kind.as_str().to_owned(), Value::from(kind.label())))
            .collect::<serde_json::Map<_, _>>(),
        "user_text": app_keys::PAIRING_FAILED_TEXT,
        "negative": app_key_negative_vectors(),
    })
}

/// f3129 S2+S3 (`docs/contracts/app-key-bind-v1.md`).
fn app_key_fixtures() -> Vec<Fixture> {
    use Kind::*;
    vec![
        Fixture {
            name: "device_code_app_keys.request",
            kind: Request,
            value: device_code_app_keys_request(),
        },
        Fixture {
            name: "device_code_app_keys.response",
            kind: Response,
            value: json!({
                "schema": DEVICE_CODE_SCHEMA, "device_code": DEVICE_CODE, "user_code": "WDJB-MJHT",
                "verification_uri": "https://cloud.leanctx.com/device",
                "verification_uri_complete": "https://cloud.leanctx.com/device?user_code=WDJB-MJHT",
                "interval": 5, "expires_in": 900,
                "features": [app_keys::FEATURE_DEVICE_APP_KEYS],
            }),
        },
        Fixture {
            name: "device_token_app_keys.request",
            kind: Request,
            value: device_token_app_keys_request(&announced()),
        },
        Fixture {
            name: "app_key_bind.vectors",
            kind: Other,
            value: app_key_bind_vectors(),
        },
    ]
}

fn profile_fixtures() -> Vec<Fixture> {
    use Kind::*;
    vec![Fixture {
        name: "agent_profile_list.response",
        kind: Response,
        value: json!({
            "schema": profiles::AGENT_PROFILE_LIST_SCHEMA,
            "profiles": [
                {
                    "profile_id": "prof_01jbc3e", "agent": "claude-3e", "instance_label": "studio-mac",
                    "owner": "Ada Muster", "display_name": "Claude (contracts)", "model": "claude-opus-5-5",
                    "roles": ["contracts", "review"], "strengths": ["cloud protocol", "decision facts"],
                    "reputation": {"score": 42, "contributions": 17, "weight_pct": 100},
                    "mine": true, "last_seen": T0,
                },
                {
                    "profile_id": "prof_01jbc85", "agent": "codex-85", "instance_label": "build-box",
                    "owner": "Repo Maintainer", "display_name": "Codex (server)", "roles": [], "strengths": [],
                    "reputation": {"score": 0, "contributions": 0, "weight_pct": 100}, "mine": false,
                },
            ],
        }),
    }]
}

/// Section 7: the threshold proposal view and the ctxpkg mapping.
fn threshold_fixtures() -> Vec<Fixture> {
    use Kind::*;
    vec![
        Fixture {
            name: "proposal_view.threshold.response",
            kind: Response,
            value: json!({
                "schema": PROPOSAL_VIEW_SCHEMA, "number": 13, "state": "merged", "created_at": T0, "mine": true,
                "merged_revision": 5, "decided_by": {"kind": "threshold"},
                "draft": {
                    "schema": DRAFT_PROPOSAL_SCHEMA, "state": "draft", "summary": "Outbox ordering is per project",
                    "context_patch": {"base_revision": 4, "ops": [{"op": "retract_fact", "id": "cloud.drafts-rebase"}]},
                    "session_ref": "ses_01jbq9a",
                    "engine_screening": {"scanner_version": 1, "secrets_removed": 0, "pii_removed": 0, "items_dropped": 0},
                },
            }),
        },
        Fixture {
            name: "error.task_not_open",
            kind: Response,
            value: error(
                "invalid_state",
                "Task 7 is claimed, not open.",
                json!({"reason": "task_not_open", "number": 7}),
            ),
        },
        Fixture {
            name: "error.task_closed",
            kind: Response,
            value: error(
                "invalid_state",
                "Task 7 is done; it takes no more writes.",
                json!({"reason": "task_closed", "number": 7}),
            ),
        },
        Fixture {
            name: "error.field_required",
            kind: Response,
            value: error(
                "invalid_request",
                "summary is required.",
                json!({"field": "summary", "reason": "required"}),
            ),
        },
        Fixture {
            name: "error.reviewer_own_instance",
            kind: Response,
            value: error(
                "invalid_state",
                "A confirmation from the instance that wrote the proposal does not count; confirm from your other machine.",
                json!({"reason": "reviewer_not_independent", "number": 12}),
            ),
        },
        Fixture {
            name: "error.reviewer_not_independent",
            kind: Response,
            value: error(
                "invalid_state",
                "A confirmation from the author's own instance does not count.",
                json!({"reason": "reviewer_not_independent", "number": 12}),
            ),
        },
        Fixture {
            name: "ctxpkg.vectors",
            kind: Other,
            value: ctxpkg_vectors(),
        },
    ]
}

/// `cloud::ctxpkg` ids of the section 3 wiki document, and import slugs.
fn ctxpkg_vectors() -> Value {
    let document: ResearchContextDocumentV1 = serde_json::from_value(wiki_document()).unwrap();
    let blobs: Vec<Value> = document
        .facts
        .iter()
        .map(|fact| json!({"fact": fact.id, "blob": ctxpkg::fact_blob_id(fact).unwrap()}))
        .collect();
    let page = ctxpkg::page_tree_id(&document.pages[0], &document).unwrap();
    let tree = ctxpkg::document_tree_id(&document).unwrap();
    let first = ctxpkg::revision_commit_id(3, &tree, None, &[11], None).unwrap();
    let second = ctxpkg::revision_commit_id(4, &tree, Some(&first), &[13, 12], None).unwrap();
    let slugs: Vec<Value> = [
        "## Retry budget (payments)",
        "Ünïcode — only",
        "!!!",
        "Outbox: ordering & retries",
    ]
    .iter()
    .map(|heading| json!({"heading": heading, "slug": ctxpkg::import_slug(heading)}))
    .collect();
    json!({
        "note": "ids are sha256: + hex of canonical_json of the typed objects in cloud::ctxpkg; the document is knowledge_revision.wiki.response",
        "blobs": blobs,
        "page_tree": {"page": document.pages[0].id, "tree": page},
        "document_tree": tree,
        "commits": [
            {"revision": 3, "parent": null, "merged_proposals": [11], "commit": first},
            {"revision": 4, "parent": first, "merged_proposals": [12, 13], "commit": second},
        ],
        "import_slugs": slugs,
    })
}

fn code_graph() -> Value {
    json!({
        "schema": codegraph::CODE_GRAPH_SCHEMA,
        "revision_hint": "3f6c1e9a0b7d4c2e8f5a1b6c9d0e7f423f6c1e9a",
        "nodes": [
            {"id": "code:rust/src/core/cloud/outbox.rs", "kind": "file", "lang": "rust"},
            {"id": "code:rust/src/core/cloud/outbox.rs#cloud::outbox::enqueue", "kind": "symbol", "lang": "rust", "symbol_kind": "fn"},
            {"id": "code:rust/src/core/cloud/queue.rs", "kind": "file", "lang": "rust"},
            {"id": "code:rust/src/core/cloud/queue.rs#cloud::queue::push", "kind": "symbol", "lang": "rust", "symbol_kind": "fn"},
        ],
        "edges": [
            {"from": "code:rust/src/core/cloud/outbox.rs", "to": "code:rust/src/core/cloud/outbox.rs#cloud::outbox::enqueue", "type": "contains"},
            {"from": "code:rust/src/core/cloud/outbox.rs", "to": "code:rust/src/core/cloud/queue.rs", "type": "imports"},
            {"from": "code:rust/src/core/cloud/outbox.rs#cloud::outbox::enqueue", "to": "code:rust/src/core/cloud/queue.rs#cloud::queue::push", "type": "calls"},
            {"from": "code:rust/src/core/cloud/queue.rs", "to": "code:rust/src/core/cloud/queue.rs#cloud::queue::push", "type": "contains"},
        ],
    })
}

/// Section 8: the published code graph.
fn code_graph_fixtures() -> Vec<Fixture> {
    use Kind::*;
    let graph = code_graph();
    vec![
        Fixture {
            name: "knowledge_revision.ctxpkg.response",
            kind: Response,
            value: {
                let document: ResearchContextDocumentV1 =
                    serde_json::from_value(wiki_document()).unwrap();
                let tree = ctxpkg::document_tree_id(&document).unwrap();
                let first = ctxpkg::revision_commit_id(3, &tree, None, &[11], None).unwrap();
                let commit =
                    ctxpkg::revision_commit_id(4, &tree, Some(&first), &[13, 12], None).unwrap();
                json!({
                    "schema": KNOWLEDGE_REVISION_SCHEMA, "vault_id": VAULT, "revision": 4, "document": wiki_document(),
                    "updated_at": T0, "ctxpkg_tree": tree, "ctxpkg_commit": commit,
                })
            },
        },
        Fixture {
            name: "vault.code_graph.response",
            kind: Response,
            value: json!({
                "schema": VAULT_SCHEMA, "vault_id": VAULT, "vault": {"ns": "acme", "name": "payments"},
                "sealed": false, "code_graph": "full",
            }),
        },
        Fixture {
            name: "code_graph_put.request",
            kind: Request,
            value: json!({"schema": codegraph::CODE_GRAPH_PUT_SCHEMA, "graph": graph}),
        },
        Fixture {
            name: "code_graph_ack.response",
            kind: Response,
            value: json!({
                "schema": codegraph::CODE_GRAPH_ACK_SCHEMA, "nodes": 4, "edges": 4,
                "revision_hint": "3f6c1e9a0b7d4c2e8f5a1b6c9d0e7f423f6c1e9a",
            }),
        },
        Fixture {
            name: "error.code_graph_disabled",
            kind: Response,
            value: error(
                "invalid_state",
                "The owner publishes anchors only for this vault.",
                json!({"reason": "code_graph_disabled"}),
            ),
        },
    ]
}

/// X4 addendum: a metrics day with `cloud`.
fn metrics_cloud_fixture() -> Fixture {
    let mut day = metrics_day();
    day["cloud"] = json!({
        "knowledge_hits_referenced": 14,
        "task_usage": [
            {"number": 7, "input_tokens": 182_000, "output_tokens": 9_400, "list_usd_micros": 687_000},
            {"number": 9, "input_tokens": 41_500, "output_tokens": 2_100, "list_usd_micros": 156_000},
        ],
        "first_context_ms_p50": 840,
        "first_context_sessions": 6,
    });
    Fixture {
        name: "metrics.cloud.request",
        kind: Kind::Request,
        value: json!({
            "schema": METRICS_SCHEMA,
            "device": {"id": "0b7d1c3e-9f2a-4c5b-8d6e-1a2b3c4d5e6f", "label": "studio-mac", "engine_version": "3.12.0", "os": "macos"},
            "days": [day],
        }),
    }
}

/// `anchor::anchor_digest` reference cases; `input_hex` is the raw content.
fn anchor_digest_vectors() -> Value {
    let cases: [(&str, &[u8]); 7] = [
        ("symbol", OUTBOX_ENQUEUE.as_bytes()),
        (
            "crlf_bom_trailing",
            b"\xEF\xBB\xBFpub fn enqueue(item: OutboundItem) -> Result<()> {  \r\n    outbox().push(item)\t\r\n}\r\n",
        ),
        ("no_final_newline", b"pub fn enqueue(item: OutboundItem) -> Result<()> {\n    outbox().push(item)\n}"),
        ("lone_cr", b"a\rb"),
        ("blank_lines_keep", b"x\t \n\t\n\n  y\n"),
        ("indent_keeps", b"  x\n"),
        ("empty", b""),
    ];
    let cases: Vec<Value> = cases
        .iter()
        .map(|(name, input)| {
            json!({
                "name": name,
                "input_hex": hex(input),
                "normalized_hex": hex(&anchor::normalize_anchor_content(input)),
                "digest": anchor::anchor_digest(input),
            })
        })
        .collect();
    json!({
        "note": "sha256: + hex over the normalized bytes: BOM dropped, CRLF/CR to LF, trailing spaces/tabs per line removed.",
        "cases": cases,
    })
}

/// Byte-level vectors both implementations must reproduce.
fn vectors() -> Value {
    let canonical_input = json!({"b": [1, {"d": "ü\n", "c": true}], "a": null});
    json!({
        "note": "Test-only keys; never valid outside fixtures and the fake Cloud service.",
        "keys": {
            "root_seed_hex": hex(&ROOT_SEED), "root_ed25519_pub": public(ROOT_SEED),
            "signing_seed_hex": hex(&SIGNING_SEED), "signing_kid": KID, "signing_ed25519_pub": public(SIGNING_SEED),
            "instance_seed_hex": hex(&INSTANCE_SEED), "instance_ed25519_pub": public(INSTANCE_SEED),
            "policy_signer_seed_hex": hex(&POLICY_SEED),
            "org_salt_hex": hex(&ORG_SALT),
        },
        "canonical_json": {"input": canonical_input, "output": String::from_utf8(canonical_json(&canonical_input).unwrap()).unwrap()},
        "token_assertion": {
            "instance_id": INSTANCE, "iat": T0_UNIX, "nonce": NONCE,
            "input": String::from_utf8(token_assertion_input(INSTANCE, T0_UNIX, NONCE)).unwrap(),
        },
        "repo_identity": {
            "repo_identity": IDENTITY,
            "stable_identity": stable_identity(IDENTITY),
            "identity_digest": identity_digest(),
        },
        "receipt_digest": receipt_digest(&receipt_document()).unwrap(),
    })
}

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/cloud")
}

/// Sorted keys regardless of serde_json's `preserve_order` feature, which a
/// workspace build may enable.
fn sorted(value: &Value) -> Value {
    match value {
        Value::Object(fields) => Value::Object(
            fields
                .iter()
                .map(|(key, value)| (key.clone(), sorted(value)))
                .collect::<std::collections::BTreeMap<_, _>>()
                .into_iter()
                .collect(),
        ),
        Value::Array(values) => Value::Array(values.iter().map(sorted).collect()),
        scalar => scalar.clone(),
    }
}

fn pretty(value: &Value) -> String {
    let mut text = serde_json::to_string_pretty(&sorted(value)).unwrap();
    text.push('\n');
    text
}

/// Fixtures that originate in Cloud (signed with the vendor test root there)
/// and are copied here unchanged; [`root_cert_fixtures_chain_to_the_lease`]
/// checks them.
const EXTERNAL_FIXTURES: [&str; 9] = [
    "root_cert.response.json",
    "root_cert.vectors.json",
    "root_cert_request.json",
    // LeanCTX Cloud's `GET /v1/usage` answer (`cloud::usage` tests it).
    "usage.response.json",
    // LeanCTX Cloud's `GET /v1/cpao` answer (p37 !150; `cloud::cpao` tests it).
    "cpao.response.json",
    // One-step trial (p37 !134; `cloud::enroll`, `cloud::lease` and the
    // engine's `CloudError` test them).
    "device_code_trial.request.json",
    "device_code_trial.response.json",
    "error.trial_used.json",
    "lease_envelope.trialing.response.json",
];

#[test]
fn root_cert_fixtures_chain_to_the_lease() {
    use lean_ctx_protocol::cloud::root_cert::RootCertV1;
    let read = |name: &str| -> Value {
        serde_json::from_str(&std::fs::read_to_string(fixture_dir().join(name)).unwrap()).unwrap()
    };
    let raw = read("root_cert.response.json");
    let cert: RootCertV1 = serde_json::from_value(raw.clone()).unwrap();
    cert.validate().unwrap();
    // vendor root -> certificate
    let vendor: [u8; 32] = B64
        .decode(
            read("root_cert.vectors.json")["keys"]["vendor_root_ed25519_pub"]
                .as_str()
                .unwrap(),
        )
        .unwrap()
        .try_into()
        .unwrap();
    let signature =
        Signature::from_bytes(&B64.decode(&cert.signature).unwrap().try_into().unwrap());
    let input = RootCertV1::signing_input_raw(&raw).unwrap();
    assert!(
        VerifyingKey::from_bytes(&vendor)
            .unwrap()
            .verify(&input, &signature)
            .is_ok()
    );
    // certificate -> the root of the key set -> the lease's organization
    assert_eq!(cert.root_ed25519_pub, public(ROOT_SEED));
    assert!(cert.covers_org(ORG));
    assert!(!cert.covers_org("org_other"));
    let at = |t: &str| lean_ctx_time::OffsetDateTime::parse_from_rfc3339(t).unwrap();
    assert!(cert.valid_at(at("2026-10-21T12:00:00Z")).unwrap());
    assert!(
        !cert.valid_at(at("2027-10-21T00:20:00Z")).unwrap(),
        "no grace after not_after"
    );
    // A changed field breaks the vendor signature.
    let mut forged = raw;
    forged["max_seats"] = Value::from(10_000);
    assert!(
        VerifyingKey::from_bytes(&vendor)
            .unwrap()
            .verify(&RootCertV1::signing_input_raw(&forged).unwrap(), &signature)
            .is_err()
    );
}

#[test]
fn fixtures_are_current() {
    let dir = fixture_dir();
    let write = std::env::var_os("CLOUD_FIXTURES_WRITE").is_some();
    if write {
        std::fs::create_dir_all(&dir).unwrap();
    }
    let mut expected: std::collections::BTreeSet<std::ffi::OsString> = EXTERNAL_FIXTURES
        .iter()
        .map(std::ffi::OsString::from)
        .collect();
    for fixture in fixtures() {
        let path = dir.join(format!("{}.json", fixture.name));
        expected.insert(path.file_name().unwrap().to_owned());
        if write {
            std::fs::write(&path, pretty(&fixture.value)).unwrap();
            continue;
        }
        let on_disk = std::fs::read_to_string(&path).unwrap_or_else(|error| {
            panic!(
                "{}: {error} (run with CLOUD_FIXTURES_WRITE=1)",
                fixture.name
            )
        });
        assert_eq!(on_disk, pretty(&fixture.value), "{} is stale", fixture.name);
    }
    let present = std::fs::read_dir(&dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        present, expected,
        "fixture directory has unknown or missing files"
    );
}

fn check<T: DeserializeOwned + Serialize>(
    fixture: &Fixture,
    validate: impl Fn(&T) -> Result<(), ValidationError>,
) {
    let typed: T = serde_json::from_value(fixture.value.clone())
        .unwrap_or_else(|error| panic!("{}: {error}", fixture.name));
    validate(&typed).unwrap_or_else(|error| panic!("{}: {error}", fixture.name));
    assert_eq!(
        serde_json::to_value(&typed).unwrap(),
        fixture.value,
        "{} does not round-trip",
        fixture.name
    );
    let mut extended = fixture.value.clone();
    if let Some(object) = extended.as_object_mut() {
        object.insert("unexpected_field".into(), Value::from(true));
        let decoded = serde_json::from_value::<T>(extended);
        match fixture.kind {
            Kind::Request => assert!(decoded.is_err(), "{} accepts unknown fields", fixture.name),
            Kind::Response => assert!(decoded.is_ok(), "{} refuses unknown fields", fixture.name),
            Kind::Other => {}
        }
    }
}

fn ok<T>(_: &T) -> Result<(), ValidationError> {
    Ok(())
}

#[test]
fn every_fixture_decodes_validates_and_round_trips() {
    for fixture in fixtures() {
        let name = fixture.name;
        match name {
            n if n.starts_with("error.") => check::<CloudErrorBody>(&fixture, ok),
            "device_code.request" | "device_code_labelled.request" => {
                check(&fixture, DeviceCodeRequestV1::validate)
            }
            "device_code.response" => check(&fixture, DeviceCodeV1::validate),
            "device_token.request" => check(&fixture, DeviceTokenRequestV1::validate),
            "enrollment.response" => check(&fixture, EnrollmentV1::validate),
            "token.request" => check(&fixture, InstanceTokenRequestV1::validate),
            "access_token.response" => check(&fixture, AccessTokenV1::validate),
            "lease_envelope.response" => check(&fixture, |envelope: &CloudLeaseEnvelopeV1| {
                envelope.validate_shape()?;
                envelope.lease().map(|_| ())
            }),
            "keyset.response" => check(&fixture, CloudKeySetV1::validate),
            "org_salt.response" => check(&fixture, OrgSaltV1::validate),
            n if n.starts_with("vault_resolve.") => {
                check(&fixture, VaultResolveRequestV1::validate)
            }
            n if n.starts_with("vault.") => check(&fixture, VaultV1::validate),
            n if n.starts_with("knowledge_revision.") => {
                check(&fixture, KnowledgeRevisionV1::validate)
            }
            n if n.starts_with("fact_states.") => {
                check(&fixture, facts::FactStateReportV1::validate)
            }
            "fact_states_ack.response" => check::<facts::FactStateAckV1>(&fixture, ok),
            "graph.response" => check(&fixture, graph::GraphV1::validate),
            "ctxpkg.vectors" => {}
            n if n.starts_with("code_graph_put.") => {
                check(&fixture, codegraph::CodeGraphPutV1::validate)
            }
            "code_graph_ack.response" => check::<codegraph::CodeGraphAckV1>(&fixture, ok),
            "agent_profile_list.response" => {
                check(&fixture, profiles::AgentProfileListV1::validate)
            }
            "instance_optin_stop.request" => check(&fixture, optin::EngineOptinStopV1::validate),
            n if n.starts_with("instance_optin.") => {
                check(&fixture, optin::EngineOptinV1::validate)
            }
            "anchor_digest.vectors" => {}
            n if n.starts_with("proposal.") => check(&fixture, DraftProposalV1::validate),
            "proposal_ack.response" => check::<ProposalAckV1>(&fixture, ok),
            "proposal_list.response" => check::<ProposalListV1>(&fixture, ok),
            n if n.starts_with("proposal_view.") => check(&fixture, ProposalV1::validate),
            n if n.starts_with("metrics.") => check(&fixture, MetricsUploadV1::validate),
            "metrics_ack.response" => check::<MetricsAckV1>(&fixture, ok),
            "receipts.request" => check(&fixture, ReceiptsUploadV1::validate),
            "receipts_ack.response" => check::<ReceiptsAckV1>(&fixture, ok),
            n if n.starts_with("policy_bundle.") => check(&fixture, PolicyBundleV1::validate),
            "bus_lease.request" => check(&fixture, BusLeaseRequestV1::validate),
            "bus_lease_renew.request" => check(&fixture, BusLeaseRenewRequestV1::validate),
            n if n.starts_with("bus_lease.") => check(&fixture, BusLeaseV1::validate),
            n if n.starts_with("status.") => check::<CloudCliStatusV1>(&fixture, ok),
            n if n.starts_with("task_propose.") => {
                check(&fixture, tasks::TaskProposeRequestV1::validate)
            }
            "task_ack.response" => check::<tasks::TaskAckV1>(&fixture, ok),
            "task_claim.request" => check(&fixture, tasks::TaskClaimRequestV1::validate),
            "task_claim_renew.request" => check(&fixture, tasks::TaskClaimRenewRequestV1::validate),
            "task_claim.response" => check::<tasks::TaskClaimAckV1>(&fixture, ok),
            "task_release.request" => check(&fixture, tasks::TaskReleaseRequestV1::validate),
            "task_update.request" => check(&fixture, tasks::TaskUpdateRequestV1::validate),
            n if n.starts_with("task_result.") => {
                check(&fixture, tasks::TaskResultRequestV1::validate)
            }
            n if n.starts_with("task.") => check(&fixture, tasks::TaskV1::validate),
            "task_list.response" => check(&fixture, tasks::TaskListV1::validate),
            "device_code_app_keys.request" => check(&fixture, DeviceCodeRequestV1::validate),
            "device_code_app_keys.response" => check(&fixture, |code: &DeviceCodeV1| {
                assert!(code.offers_app_keys());
                code.validate()
            }),
            "device_token_app_keys.request" => check(&fixture, DeviceTokenRequestV1::validate),
            "app_key_bind.vectors" => {}
            "vectors" => {}
            other => panic!("fixture {other} has no type mapping"),
        }
    }
}

fn verifying(public_b64: &str) -> VerifyingKey {
    let bytes: [u8; 32] = B64.decode(public_b64).unwrap().try_into().unwrap();
    VerifyingKey::from_bytes(&bytes).unwrap()
}

fn signature(b64: &str) -> Signature {
    let bytes: [u8; 64] = B64.decode(b64).unwrap().try_into().unwrap();
    Signature::from_bytes(&bytes)
}

#[test]
fn signatures_in_fixtures_verify_along_the_trust_chain() {
    let enrollment = fixtures()
        .into_iter()
        .find(|fixture| fixture.name == "enrollment.response")
        .unwrap()
        .value;
    let root = verifying(&public(ROOT_SEED));
    let keys_raw = &enrollment["keys"];
    root.verify(
        &CloudKeySetV1::signing_input_raw(keys_raw).unwrap(),
        &signature(keys_raw["signature"].as_str().unwrap()),
    )
    .expect("key set signed by root");
    let keys: CloudKeySetV1 = serde_json::from_value(keys_raw.clone()).unwrap();
    let envelope: CloudLeaseEnvelopeV1 =
        serde_json::from_value(enrollment["lease"].clone()).unwrap();
    let signer = verifying(&keys.key(&envelope.kid).unwrap().ed25519_pub);
    signer
        .verify(
            &envelope.signing_input().unwrap(),
            &signature(&envelope.signature),
        )
        .expect("lease signed by listed key");
    let mut tampered = envelope.clone();
    tampered.lease["seats_limit"] = Value::from(250);
    assert!(
        signer
            .verify(
                &tampered.signing_input().unwrap(),
                &signature(&tampered.signature)
            )
            .is_err()
    );
    let token: InstanceTokenRequestV1 = serde_json::from_value(
        fixtures()
            .into_iter()
            .find(|fixture| fixture.name == "token.request")
            .unwrap()
            .value,
    )
    .unwrap();
    verifying(&public(INSTANCE_SEED))
        .verify(
            &token_assertion_input(INSTANCE, token.iat, &token.nonce),
            &signature(&token.assertion),
        )
        .expect("assertion signed by instance key");
    let pack: FilterPackRefV1 = serde_json::from_value(filter_pack()).unwrap();
    let statement = B64.decode(&pack.envelope.statement_b64).unwrap();
    assert_eq!(hex(&Sha256::digest(&statement)), pack.sha256);
    signer
        .verify(
            &filter_pack_signing_input(&statement),
            &signature(&pack.envelope.signature),
        )
        .expect("filter pack signed by listed key");
}

#[test]
fn ctxpkg_ids_are_canonical_sha256_of_the_typed_objects() {
    let document: ResearchContextDocumentV1 = serde_json::from_value(wiki_document()).unwrap();
    let fact = &document.facts[0];
    let object = json!({"type": "fact", "fact": serde_json::to_value(fact).unwrap()});
    assert_eq!(
        ctxpkg::fact_blob_id(fact).unwrap(),
        format!(
            "sha256:{}",
            hex(&Sha256::digest(canonical_json(&object).unwrap()))
        )
    );
    let vectors = ctxpkg_vectors();
    let tree = vectors["document_tree"].as_str().unwrap();
    let commit = json!({
        "type": "commit", "revision": 3, "tree": tree, "parent": null,
        "merged_proposals": [11], "rolls_back": null,
    });
    assert_eq!(
        vectors["commits"][0]["commit"],
        format!(
            "sha256:{}",
            hex(&Sha256::digest(canonical_json(&commit).unwrap()))
        )
    );
}

#[test]
fn anchor_digest_vectors_hash_the_normalized_bytes() {
    let vectors = anchor_digest_vectors();
    let cases = vectors["cases"].as_array().unwrap();
    let digest = |name: &str| {
        cases.iter().find(|case| case["name"] == name).unwrap()["digest"]
            .as_str()
            .unwrap()
            .to_owned()
    };
    for case in cases {
        let normalized = case["normalized_hex"].as_str().unwrap();
        let bytes: Vec<u8> = (0..normalized.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&normalized[i..i + 2], 16).unwrap())
            .collect();
        assert_eq!(
            case["digest"],
            format!("sha256:{}", hex(&Sha256::digest(&bytes)))
        );
    }
    assert_eq!(digest("symbol"), digest("crlf_bom_trailing"));
    assert_ne!(digest("symbol"), digest("no_final_newline"));
    assert_eq!(
        cases.iter().find(|c| c["name"] == "lone_cr").unwrap()["normalized_hex"],
        hex(b"a\nb")
    );
    assert_eq!(
        cases
            .iter()
            .find(|c| c["name"] == "blank_lines_keep")
            .unwrap()["normalized_hex"],
        hex(b"x\n\n\n  y\n")
    );
}

#[test]
fn lease_fixture_is_valid_for_a_day_then_in_grace() {
    let envelope: CloudLeaseEnvelopeV1 = serde_json::from_value(lease_envelope()).unwrap();
    let lease = envelope.lease().unwrap();
    let at = |value| lean_ctx_time::OffsetDateTime::parse_from_rfc3339(value).unwrap();
    assert_eq!(lease.state_at(at(T0)).unwrap(), LeaseState::Valid);
    assert_eq!(
        lease.state_at(at("2026-10-22T00:00:00Z")).unwrap(),
        LeaseState::Grace
    );
    assert_eq!(
        lease.state_at(at("2026-10-27T12:00:00Z")).unwrap(),
        LeaseState::Lapsed
    );
    assert!(lease.grants(CloudRight::BusRelay));
}

#[test]
fn org_settings_narrow_uploads_and_their_absence_allows_both() {
    let envelope: CloudLeaseEnvelopeV1 = serde_json::from_value(lease_envelope()).unwrap();
    let lease = envelope.lease().unwrap();
    assert!(lease.org_allows_metrics_upload());
    assert!(!lease.org_allows_receipts_upload());
    // A 1.3 lease without the object, as an older service signs it.
    let mut older = envelope.lease.clone();
    older.as_object_mut().unwrap().remove("org_settings");
    let older: lean_ctx_protocol::cloud::lease::CloudLeaseV1 =
        serde_json::from_value(older).unwrap();
    assert!(older.org_allows_metrics_upload() && older.org_allows_receipts_upload());
}

/// Cloud task #19: `tasks` may be absent (no limit), but a named count is
/// 1-100; a budget nobody set still starts at 5.
#[test]
fn a_daily_task_limit_is_absent_or_one_to_a_hundred() {
    let with = |tasks: serde_json::Value| {
        let mut v = optin_fixtures()
            .into_iter()
            .find(|f| f.name == "instance_optin.unlimited.response")
            .unwrap()
            .value;
        v["daily_budget"]["tasks"] = tasks;
        serde_json::from_value::<optin::EngineOptinV1>(v).unwrap()
    };
    assert!(with(json!(1)).validate().is_ok());
    assert!(with(json!(100)).validate().is_ok());
    assert!(with(json!(0)).validate().is_err());
    assert!(with(json!(101)).validate().is_err());
    let none = with(serde_json::Value::Null);
    assert_eq!(none.daily_budget.tasks, None);
    assert!(none.validate().is_ok());
    assert_eq!(
        serde_json::to_value(none.daily_budget).unwrap(),
        json!({}),
        "no limit is sent as no field"
    );
    assert_eq!(
        optin::DailyBudgetV1::default().tasks,
        Some(optin::DEFAULT_DAILY_TASKS)
    );
}

/// Reference of the Cloud's f3129 token checks in their contract order:
/// code status (step 0: unknown, expired or used) → schema → `app_keys_bind_signature` → each offer's
/// `bind_signature` → announced-set equality. Signatures verify strictly.
/// `Err` is the 400 body.
fn reference_token_check(
    announced: &[app_keys::AppKeyAnnouncementV1],
    request: &Value,
    code_refused: bool,
) -> Result<(), Value> {
    use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64URL;
    let verify = |public: &[u8], message: &[u8], signature: &str| -> bool {
        let (Ok(public), Ok(signature)) = (
            <[u8; 32]>::try_from(public),
            B64URL
                .decode(signature)
                .ok()
                .and_then(|s| <[u8; 64]>::try_from(s).ok())
                .ok_or(()),
        ) else {
            return false;
        };
        VerifyingKey::from_bytes(&public).is_ok_and(|key| {
            key.verify_strict(message, &Signature::from_bytes(&signature))
                .is_ok()
        })
    };
    if code_refused {
        return Err(error(
            "expired_token",
            "The code is unknown, expired or used; start again.",
            json!({}),
        ));
    }
    // An offer that does not decode or validate is refused by its member,
    // before the generic schema check of the rest.
    for kind in app_keys::AppKeyKind::ALL {
        let member = kind.request_member();
        if let Some(offer) = request.get(member) {
            let invalid = || app_key_error(member, &format!("{member} is invalid."), None);
            serde_json::from_value::<app_keys::AppKeyOfferV1>(offer.clone())
                .map_err(|_| invalid())?
                .validate(kind)
                .map_err(|_| invalid())?;
        }
    }
    let typed: DeviceTokenRequestV1 = serde_json::from_value(request.clone())
        .map_err(|_| error("invalid_request", "schema", json!({})))?;
    let bind_invalid = || {
        app_key_error(
            "app_keys_bind_signature",
            "app_keys_bind_signature does not verify.",
            Some(app_keys::REASON_APP_KEY_BIND_INVALID),
        )
    };
    if announced.is_empty() {
        if typed.app_keys_bind_signature.is_some() {
            return Err(bind_invalid());
        }
    } else {
        let instance = B64.decode(&typed.instance_public_key).unwrap();
        let input = app_keys::app_key_bind_signing_input(
            &typed.device_code,
            &typed.instance_public_key,
            announced,
        )
        .unwrap();
        match &typed.app_keys_bind_signature {
            Some(signature) if verify(&instance, &input, signature) => {}
            _ => return Err(bind_invalid()),
        }
    }
    for (kind, offer) in typed.app_key_offers() {
        let raw = app_keys::decode_b64url_exact(&offer.public_key, 32, "public_key").unwrap();
        let input = app_keys::app_key_offer_bind_input(
            kind,
            &typed.instance_public_key,
            &offer.key_id,
            &offer.public_key,
        )
        .unwrap();
        if !verify(&raw, &input, &offer.bind_signature) {
            // The field names the exact request path of the signature.
            let field = format!("{}.bind_signature", kind.request_member());
            return Err(app_key_error(
                &field,
                &format!("{field} does not verify."),
                None,
            ));
        }
    }
    app_keys::offered_equals_announced(announced, &typed.app_key_offers()).map_err(|member| {
        app_key_error(
            member,
            "The offered app keys differ from the announced ones.",
            Some(app_keys::REASON_APP_KEY_NOT_ANNOUNCED),
        )
    })
}

/// Reference of the Cloud's f3129 code check: any failure in `app_keys`,
/// decode errors (unknown kind or member) included, is 400 `invalid_field`,
/// `field: "app_keys"`, never a generic schema error.
fn reference_code_check(request: &Value) -> Result<(), Value> {
    let refuse = || app_key_error("app_keys", "app_keys is invalid.", None);
    if let Some(keys) = request.get("app_keys") {
        let keys: Vec<app_keys::AppKeyAnnouncementV1> =
            serde_json::from_value(keys.clone()).map_err(|_| refuse())?;
        app_keys::validate_app_keys(&keys).map_err(|_| refuse())?;
    }
    let schema = || error("invalid_request", "schema", json!({}));
    let mut rest = request.clone();
    rest.as_object_mut().ok_or_else(schema)?.remove("app_keys");
    serde_json::from_value::<DeviceCodeRequestV1>(rest)
        .map_err(|_| schema())?
        .validate()
        .map_err(|_| schema())
}

fn on_disk(name: &str) -> Value {
    serde_json::from_str(&std::fs::read_to_string(fixture_dir().join(name)).unwrap()).unwrap()
}

/// The committed f3129 fixtures verify on their own: every signature in
/// them checks against the documented test keys, so 63 can rely on them and
/// a drift of the generator or the files fails here.
#[test]
fn app_key_fixtures_verify_from_disk() {
    reference_code_check(&on_disk("device_code_app_keys.request.json")).unwrap();
    let code: DeviceCodeRequestV1 =
        serde_json::from_value(on_disk("device_code_app_keys.request.json")).unwrap();
    code.validate().unwrap();
    assert_eq!(code.app_keys, announced());
    let answer: DeviceCodeV1 =
        serde_json::from_value(on_disk("device_code_app_keys.response.json")).unwrap();
    assert!(answer.offers_app_keys());
    let raw = on_disk("device_token_app_keys.request.json");
    let token: DeviceTokenRequestV1 = serde_json::from_value(raw.clone()).unwrap();
    token.validate().unwrap();
    assert_eq!(token.device_code, answer.device_code);
    assert_eq!(token.instance_public_key, public(INSTANCE_SEED));
    reference_token_check(&code.app_keys, &raw, false).unwrap();

    let vectors = on_disk("app_key_bind.vectors.json");
    let input = app_keys::app_key_bind_signing_input(
        &token.device_code,
        &token.instance_public_key,
        &code.app_keys,
    )
    .unwrap();
    assert_eq!(
        vectors["signing_input"],
        String::from_utf8(input.clone()).unwrap()
    );
    assert_eq!(
        vectors["app_keys_bind_signature"],
        token.app_keys_bind_signature.clone().unwrap()
    );
    assert_eq!(
        vectors["device_code_sha256"],
        hex(&Sha256::digest(token.device_code.as_bytes()))
    );
    // Precondition under which canonical_json equals JCS (RFC 8785): every
    // key and string is printable ASCII without `"` or `\`, so both sort by
    // the same code units and neither escapes anything. This checks that
    // precondition; it does not compare against an independent JCS
    // implementation.
    fn plain_ascii(value: &Value) -> bool {
        let ok = |s: &str| {
            s.bytes()
                .all(|b| (0x20..0x7f).contains(&b) && b != b'"' && b != b'\\')
        };
        match value {
            Value::Object(fields) => fields.iter().all(|(k, v)| ok(k) && plain_ascii(v)),
            Value::Array(values) => values.iter().all(plain_ascii),
            Value::String(s) => ok(s),
            _ => false,
        }
    }
    assert!(plain_ascii(&vectors["statement"]));
    assert_eq!(
        app_keys::APP_KEY_BIND_DOMAIN
            .as_bytes()
            .to_vec()
            .into_iter()
            .chain(canonical_json(&vectors["statement"]).unwrap())
            .collect::<Vec<_>>(),
        input
    );
    // Seeds in the vectors are the keys the fixtures carry.
    for kind in app_keys::AppKeyKind::ALL {
        let seed: [u8; 32] = (0..32)
            .map(|i| {
                let hex_seed = vectors["keys"][format!("{}_seed_hex", kind.as_str())]
                    .as_str()
                    .unwrap();
                u8::from_str_radix(&hex_seed[2 * i..2 * i + 2], 16).unwrap()
            })
            .collect::<Vec<_>>()
            .try_into()
            .unwrap();
        assert_eq!(seed, app_key_seed(kind));
        assert_eq!(raw[kind.request_member()], app_key_offer(kind, seed));
    }

    for case in vectors["negative"].as_array().unwrap() {
        let name = case["case"].as_str().unwrap();
        let body: CloudErrorBody = serde_json::from_value(case["response"].clone())
            .unwrap_or_else(|e| panic!("{name}: {e}"));
        if case["response"]["error"]["code"] == "invalid_field" {
            assert_eq!(body.error.code, CloudErrorCode::InvalidField, "{name}");
        }
        // A dotted field is a path that exists in the request it refuses.
        if let Some(field) = body.error.field.as_deref().filter(|f| f.contains('.')) {
            let found = field
                .split('.')
                .try_fold(&case["request"], |at, member| at.get(member));
            assert!(found.is_some(), "{name}: {field} is not in the request");
        }
        if case["route"] == "POST /v1/device/code" {
            let refused = serde_json::from_value::<DeviceCodeRequestV1>(case["request"].clone())
                .map_or(true, |request| request.validate().is_err());
            assert!(refused, "{name} passes the code request validator");
            assert_eq!(body.error.field.as_deref(), Some("app_keys"), "{name}");
            assert_eq!(
                reference_code_check(&case["request"]),
                Err(case["response"].clone()),
                "{name}"
            );
            continue;
        }
        let announced: Vec<app_keys::AppKeyAnnouncementV1> =
            serde_json::from_value(case["announced"].clone()).unwrap();
        app_keys::validate_app_keys(&announced).unwrap();
        // Step 0 is the Cloud's own state of the code, not in the request.
        let refused = name.starts_with("device_code_");
        assert_eq!(
            reference_token_check(&announced, &case["request"], refused),
            Err(case["response"].clone()),
            "{name}"
        );
        if name == "offer_small_order_key" {
            // Only strict verification refuses this offer.
            let offer = &case["request"]["chat_key"];
            let raw = app_keys::decode_b64url_exact(
                offer["public_key"].as_str().unwrap(),
                32,
                "public_key",
            )
            .unwrap();
            let public = VerifyingKey::from_bytes(&raw.try_into().unwrap()).unwrap();
            assert!(public.is_weak());
            let input = app_keys::app_key_offer_bind_input(
                app_keys::AppKeyKind::Chat,
                case["request"]["instance_public_key"].as_str().unwrap(),
                offer["key_id"].as_str().unwrap(),
                offer["public_key"].as_str().unwrap(),
            )
            .unwrap();
            let bytes: [u8; 64] = app_keys::decode_b64url_exact(
                offer["bind_signature"].as_str().unwrap(),
                64,
                "bind_signature",
            )
            .unwrap()
            .try_into()
            .unwrap();
            let signature = Signature::from_bytes(&bytes);
            assert!(public.verify(&input, &signature).is_ok(), "lenient accepts");
            assert!(
                public.verify_strict(&input, &signature).is_err(),
                "strict refuses"
            );
        }
        if let Some(text) = app_keys::app_key_refusal_text(body.error.reason.as_deref()) {
            assert_eq!(text, vectors["user_text"]);
        }
    }
}

/// Old requests stay byte-identical: no app keys, no new members.
#[test]
fn requests_without_app_keys_keep_their_bytes() {
    let plain = DeviceCodeRequestV1::new("3.12.0", EngineOs::Macos);
    assert_eq!(
        serde_json::to_value(&plain).unwrap(),
        on_disk("device_code.request.json")
    );
    let token: DeviceTokenRequestV1 =
        serde_json::from_value(on_disk("device_token.request.json")).unwrap();
    assert!(token.app_key_offers().is_empty() && token.app_keys_bind_signature.is_none());
    assert_eq!(
        serde_json::to_value(&token).unwrap(),
        on_disk("device_token.request.json")
    );
    let answer: DeviceCodeV1 =
        serde_json::from_value(on_disk("device_code.response.json")).unwrap();
    assert!(!answer.offers_app_keys(), "no feature, no app-key binding");
}
