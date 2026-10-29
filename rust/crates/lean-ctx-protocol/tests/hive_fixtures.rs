// SPDX-License-Identifier: Apache-2.0

//! Golden fixtures of the Hive wire contract (`tests/fixtures/hive/`).
//!
//! The fixtures are the contract the engine connector and the Hive service
//! share. They are generated here from fixed, test-only keys (seeds in
//! `vectors.json`) so every signature in them verifies. A contract change is
//! a reviewed fixture diff:
//!
//! `HIVE_FIXTURES_WRITE=1 cargo test -p lean-ctx-protocol --test hive_fixtures`

use base64::{Engine as _, engine::general_purpose::STANDARD as B64};
use chacha20poly1305::aead::{Aead, KeyInit as _, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use hmac::{Hmac, KeyInit as _, Mac as _};
use lean_ctx_protocol::ValidationError;
use lean_ctx_protocol::hive::bus::*;
use lean_ctx_protocol::hive::enroll::*;
use lean_ctx_protocol::hive::knowledge::*;
use lean_ctx_protocol::hive::lease::*;
use lean_ctx_protocol::hive::policy::*;
use lean_ctx_protocol::hive::sealed::*;
use lean_ctx_protocol::hive::status::*;
use lean_ctx_protocol::hive::telemetry::*;
use lean_ctx_protocol::hive::vault::*;
use lean_ctx_protocol::hive::*;
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
const REPO_KEY: [u8; 32] = [0x4b; 32];
const KX_PUBLIC: [u8; 32] = [0x58; 32];
/// Seed of the recipient's X25519 key in the key-wrap vector.
const KX_SEED: [u8; 32] = [0x59; 32];
/// Fixed ephemeral X25519 scalar of the key-wrap vector.
const EPHEMERAL_SCALAR: [u8; 32] = [0x45; 32];

const T0: &str = "2026-10-20T12:00:00Z";
const T0_UNIX: i64 = 1_792_497_600;
const KID: &str = "hive-2026-10";
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

fn seal(plaintext: &[u8], revision: u64, content_schema: &str, nonce: [u8; 24]) -> Value {
    let aad = sealed_aad(VAULT, revision, 1, content_schema);
    let ciphertext = XChaCha20Poly1305::new((&REPO_KEY).into())
        .encrypt(
            XNonce::from_slice(&nonce),
            Payload {
                msg: plaintext,
                aad: &aad,
            },
        )
        .expect("encrypt");
    json!({
        "alg": SEALED_CONTENT_ALG,
        "epoch": 1,
        "content_schema": content_schema,
        "nonce": B64.encode(nonce),
        "ciphertext": B64.encode(ciphertext),
    })
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

/// The recipient's X25519 key pair: the Montgomery form of an Ed25519 seed,
/// as engines derive it (`kx.key`).
fn recipient_kx() -> ([u8; 32], [u8; 32]) {
    let kx = key(KX_SEED);
    (
        kx.to_scalar_bytes(),
        kx.verifying_key().to_montgomery().to_bytes(),
    )
}

/// The 32-byte wrap key: HKDF-SHA256(salt = ephemeral || recipient,
/// ikm = X25519 shared secret, info = SEALED_WRAP_INFO).
fn wrap_key(shared: &[u8; 32], ephemeral: &[u8; 32], recipient: &[u8; 32]) -> [u8; 32] {
    let mut salt = ephemeral.to_vec();
    salt.extend_from_slice(recipient);
    let mut out = [0u8; 32];
    hkdf::Hkdf::<Sha256>::new(Some(&salt), shared)
        .expand(SEALED_WRAP_INFO.as_bytes(), &mut out)
        .expect("32 bytes");
    out
}

/// REPO_KEY wrapped for the fixture recipient (fixed ephemeral and nonce).
fn wrap_repo_key(instance_id: &str, epoch: u32) -> Value {
    use curve25519_dalek::montgomery::MontgomeryPoint;
    let (_, recipient) = recipient_kx();
    let ephemeral = MontgomeryPoint::mul_base_clamped(EPHEMERAL_SCALAR).to_bytes();
    let shared = MontgomeryPoint(recipient)
        .mul_clamped(EPHEMERAL_SCALAR)
        .to_bytes();
    let nonce = [0x4e; 24];
    let ciphertext = XChaCha20Poly1305::new((&wrap_key(&shared, &ephemeral, &recipient)).into())
        .encrypt(
            XNonce::from_slice(&nonce),
            Payload {
                msg: &REPO_KEY,
                aad: &sealed_wrap_aad(VAULT, epoch, instance_id),
            },
        )
        .expect("encrypt");
    json!({
        "alg": SEALED_WRAP_ALG,
        "ephemeral_pub": B64.encode(ephemeral),
        "nonce": B64.encode(nonce),
        "ciphertext": B64.encode(ciphertext),
    })
}

/// Approved recipients without a key at epoch 2, signed by the Hive key.
fn sealed_pending() -> Value {
    let mut pending = json!({
        "schema": SEALED_PENDING_SCHEMA, "vault_id": VAULT, "epoch_required": 2,
        "recipients": [{
            "instance_id": "inst_01jb8z9", "label": "build-box",
            "owner": {"id": "usr_01jb8zd", "name": "Ada Muster"},
            "instance_kx_public_key": B64.encode(KX_PUBLIC),
            "kx_fingerprint": kx_fingerprint(&KX_PUBLIC),
            "approved_by": {"id": "usr_01jb8ze", "name": "Repo Maintainer"},
            "approved_at": T0,
        }],
        "kid": KID,
    });
    let input = SealedPendingV1::signing_input_raw(&pending).unwrap();
    pending["signature"] = Value::from(sign(SIGNING_SEED, &input));
    pending
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
        "policy_signer_keys": [{"org": ORG, "ed25519_pub": public(POLICY_SEED)}],
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
    json!({"id": id, "statement": statement, "kind": "decision", "appliesTo": ["rust/src/core/hive"]})
}

fn document() -> Value {
    let mut first = fact(
        "hive.never-blocks",
        "Hive never blocks a tool call; outbound items go through a durable outbox.",
    );
    first["from_contributions"] = json!(["p11"]);
    json!({
        "format": RESEARCH_CONTEXT_FORMAT,
        "facts": [
            first,
            fact("hive.screening-first", "Every outbound item passes engine screening before it is queued."),
        ],
        "relations": [{"from": "hive.screening-first", "to": "hive.never-blocks", "type": "supports"}],
        "open_gaps": ["How are drafts rebased after a merge?"],
    })
}

fn patch_ops() -> Value {
    json!([
        {"op": "add_fact", "fact": fact("hive.drafts-rebase", "Pending drafts are rebased onto the newest revision before upload.")},
        {"op": "add_relation", "relation": {"from": "hive.drafts-rebase", "to": "hive.never-blocks", "type": "related_to"}},
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

/// `OrgPolicyStatementV2` (ee/contracts/team-company org_policy/wire.rs):
/// signed as `serde_json::to_vec` in this declaration order, not key-sorted.
#[derive(Serialize)]
struct OrgPolicyStatementV2 {
    schema: &'static str,
    organization_id: &'static str,
    key_id: String,
    policy_version: u64,
    issued_at: &'static str,
    not_before: &'static str,
    not_after: &'static str,
    grace_seconds: u64,
    pack_digest: String,
    /// C 1.8; left out when empty, so the 1.7 bytes stay valid.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    required_filter_packs: Vec<RequiredFilterPackV1>,
}

/// `OrgPolicyArtifactV2` (ee/contracts/team-company verifier.rs) without `unpin`.
#[derive(Serialize)]
struct OrgPolicyArtifactV2 {
    statement: OrgPolicyStatementV2,
    public_key: String,
    signature: String,
    pack_toml: &'static str,
}

const ORG_POLICY_SIGNATURE_DOMAIN: &str = "leanctx.org-policy.v2\n";
const POLICY_PACK_TOML: &str = "name = \"acme-floor\"\nversion = \"1.0.0\"\ndescription = \"ACME org floor\"\n\n[context]\ndeny_tools = [\"ctx_url_read\"]\n";

/// The signed org policy plus the byte-level values behind it.
fn org_policy() -> (Value, Value) {
    org_policy_with(1, Vec::new())
}

/// Version 2 of the policy, requiring the fixture filter pack (C 1.8).
fn org_policy_requiring_packs() -> (Value, Value) {
    let pack = filter_pack();
    org_policy_with(
        2,
        vec![RequiredFilterPackV1 {
            pack_id: pack["pack_id"].as_str().unwrap().into(),
            version: pack["version"].as_u64().unwrap(),
            sha256: pack["sha256"].as_str().unwrap().into(),
        }],
    )
}

fn org_policy_with(version: u64, required: Vec<RequiredFilterPackV1>) -> (Value, Value) {
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    validate_required_filter_packs(&required).unwrap();
    let public = key(POLICY_SEED).verifying_key().to_bytes();
    let statement = OrgPolicyStatementV2 {
        schema: "leanctx.org-policy/v2",
        organization_id: ORG,
        key_id: format!("key:ed25519-{}", &hex(&Sha256::digest(public))[..32]),
        policy_version: version,
        issued_at: T0,
        not_before: T0,
        not_after: "2026-11-20T12:00:00Z",
        grace_seconds: 7 * 24 * 60 * 60,
        pack_digest: format!(
            "sha256:{}",
            hex(&Sha256::digest(POLICY_PACK_TOML.as_bytes()))
        ),
        required_filter_packs: required,
    };
    let statement_bytes = serde_json::to_vec(&statement).unwrap();
    let mut signing_input = ORG_POLICY_SIGNATURE_DOMAIN.as_bytes().to_vec();
    signing_input.extend_from_slice(&statement_bytes);
    let signature = URL_SAFE_NO_PAD.encode(key(POLICY_SEED).sign(&signing_input).to_bytes());
    let vector = json!({
        "statement_json": String::from_utf8(statement_bytes).unwrap(),
        "signing_input": String::from_utf8(signing_input).unwrap(),
        "public_key": URL_SAFE_NO_PAD.encode(public),
        "signature": signature,
        "pack_toml": POLICY_PACK_TOML,
    });
    let artifact = OrgPolicyArtifactV2 {
        statement,
        public_key: URL_SAFE_NO_PAD.encode(public),
        signature,
        pack_toml: POLICY_PACK_TOML,
    };
    (serde_json::to_value(artifact).unwrap(), vector)
}

fn receipt_document() -> Value {
    json!({"schema": "leanctx.engine-receipt/v1", "receipt_id": "rcpt_01jb8z5", "tool": "ctx_read", "tokens_saved": 1200})
}

fn filter_pack() -> Value {
    // Hive signs the canonical JSON of the statement (sorted keys, no
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
    lean_ctx_protocol::hive::policy::FilterPackV1::deserialize(&pack)
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
    let wrapped = wrap_repo_key(INSTANCE, 1);
    // Nothing readable in the clear (1.5): summary, body and ops are sealed.
    let sealed_draft = json!({
        "schema": DRAFT_PROPOSAL_SCHEMA, "state": "draft",
        "context_patch": {"base_revision": 3, "sealed": seal(&canonical_json(&json!({
            "schema": SEALED_PATCH_CONTENT_SCHEMA,
            "summary": "Session learnings: draft rebasing",
            "body": "Two decisions from the connector session.",
            "ops": ops,
        })).unwrap(), 3, SEALED_PATCH_CONTENT_SCHEMA, [0x32; 24])},
        "session_ref": "ses_01jb8z7",
        "engine_screening": {"scanner_version": 1, "secrets_removed": 0, "pii_removed": 0, "items_dropped": 0},
    });
    let deliver = json!({
        "t": "deliver", "seq": 42, "channel": channel(), "from": "claude-3e", "to": "codex-85",
        "kind": "status", "priority": "normal", "privacy": "team",
        "body": {"text": "STATUS C0 Review"}, "server_time": T0,
    });
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
                "Hive is part of Personal, Team and Enterprise.",
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
                "This engine is too old for Hive.",
                json!({"min_engine": "3.12.0"}),
            ),
        },
        Fixture {
            name: "device_code.request",
            kind: Request,
            value: json!({"schema": DEVICE_CODE_REQUEST_SCHEMA, "client_id": HIVE_ENGINE_CLIENT_ID, "engine_version": "3.12.0", "os": "macos"}),
        },
        Fixture {
            name: "device_code_labelled.request",
            kind: Request,
            value: json!({"schema": DEVICE_CODE_REQUEST_SCHEMA, "client_id": HIVE_ENGINE_CLIENT_ID, "engine_version": "3.12.0", "os": "macos", "instance_label": "build-box"}),
        },
        Fixture {
            name: "device_code.response",
            kind: Response,
            value: json!({
                "schema": DEVICE_CODE_SCHEMA, "device_code": "dc_9f2c4e7a1b3d5f8e0a2c4e6f8a1b3d5e", "user_code": "WDJB-MJHT",
                "verification_uri": "https://hive.leanctx.com/device",
                "verification_uri_complete": "https://hive.leanctx.com/device?user_code=WDJB-MJHT",
                "interval": 5, "expires_in": 900,
            }),
        },
        Fixture {
            name: "device_token.request",
            kind: Request,
            value: json!({
                "schema": DEVICE_TOKEN_REQUEST_SCHEMA, "device_code": "dc_9f2c4e7a1b3d5f8e0a2c4e6f8a1b3d5e",
                "instance_public_key": public(INSTANCE_SEED), "instance_kx_public_key": B64.encode(KX_PUBLIC),
                "instance_label": "studio-mac", "engine_version": "3.12.0", "os": "macos",
            }),
        },
        Fixture {
            name: "enrollment.response",
            kind: Response,
            value: json!({
                "schema": ENROLLMENT_SCHEMA, "instance_id": INSTANCE, "org": {"id": ORG, "name": "Acme AG"},
                "lease": lease_envelope(), "keys": keyset(), "org_salt_id": SALT_ID,
            "web_url": HIVE_DEFAULT_URL,
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
            name: "vault.sealed.response",
            kind: Response,
            value: json!({"schema": VAULT_SCHEMA, "vault_id": VAULT, "vault": {"ns": "acme", "name": "payments"}, "sealed": true, "key_epoch": 1}),
        },
        Fixture {
            name: "knowledge_revision.response",
            kind: Response,
            value: json!({"schema": KNOWLEDGE_REVISION_SCHEMA, "vault_id": VAULT, "revision": 3, "document": document, "updated_at": T0}),
        },
        Fixture {
            name: "knowledge_revision.sealed.response",
            kind: Response,
            value: json!({
                "schema": KNOWLEDGE_REVISION_SCHEMA, "vault_id": VAULT, "revision": 3,
                "sealed": seal(&canonical_json(&document).unwrap(), 3, RESEARCH_CONTEXT_FORMAT, [0x31; 24]),
                "updated_at": T0,
            }),
        },
        Fixture {
            name: "revision_list.response",
            kind: Response,
            value: json!({
                "schema": REVISION_LIST_SCHEMA, "vault_id": VAULT,
                "revisions": [
                    {"revision": 2, "created_at": "2026-10-19T08:30:00Z", "proposal": 10, "digest": hex(&Sha256::digest(b"revision-2"))},
                    {"revision": 3, "created_at": T0, "proposal": 11, "digest": hex(&Sha256::digest(canonical_json(&document).unwrap()))},
                ],
            }),
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
            name: "proposal.sealed.request",
            kind: Request,
            value: sealed_draft.clone(),
        },
        Fixture {
            name: "proposal.import.request",
            kind: Request,
            value: json!({
                "schema": DRAFT_PROPOSAL_SCHEMA, "state": "draft",
                "summary": "Import: outbox ordering from docs and history",
                "sources": ["import:docs:docs/hive/outbox.md#ordering", "import:git:3f6c1e9a0b7d4c2e8f5a1b6c9d0e7f423f6c1e9a"],
                "context_patch": {"base_revision": 3, "ops": ops},
                "session_ref": "ses_01jb8z6",
                "engine_screening": {"scanner_version": 1, "secrets_removed": 0, "pii_removed": 0, "items_dropped": 0},
                "origin": "import",
            }),
        },
        Fixture {
            name: "proposal.import.sealed.request",
            kind: Request,
            value: {
                let mut draft = sealed_draft.clone();
                draft["origin"] = json!("import");
                draft
            },
        },
        Fixture {
            name: "proposal_view.sealed.response",
            kind: Response,
            value: json!({
                "schema": PROPOSAL_VIEW_SCHEMA, "number": 12, "state": "draft",
                "created_at": T0, "mine": false, "draft": sealed_draft,
            }),
        },
        Fixture {
            // An engine merged proposal 12 of a sealed vault: revision 4, sealed
            // for exactly that number.
            name: "knowledge_put.sealed.request",
            kind: Request,
            value: json!({
                "schema": KNOWLEDGE_PUT_REQUEST_SCHEMA, "expected_revision": 3,
                "sealed": seal(&canonical_json(&document).unwrap(), 4, RESEARCH_CONTEXT_FORMAT, [0x34; 24]),
                "merged_proposals": [12],
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
            name: "knowledge_put.request",
            kind: Request,
            value: json!({"schema": KNOWLEDGE_PUT_REQUEST_SCHEMA, "expected_revision": 3, "document": document}),
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
            value: json!({"schema": POLICY_BUNDLE_SCHEMA, "policy_version": 0, "org_policy": null, "unpin": null, "filter_packs": [filter_pack()]}),
        },
        Fixture {
            name: "policy_bundle.signed.response",
            kind: Response,
            value: json!({"schema": POLICY_BUNDLE_SCHEMA, "policy_version": 1, "org_policy": org_policy().0, "unpin": null, "filter_packs": [filter_pack()]}),
        },
        Fixture {
            name: "policy_bundle.required_packs.response",
            kind: Response,
            value: json!({"schema": POLICY_BUNDLE_SCHEMA, "policy_version": 2, "org_policy": org_policy_requiring_packs().0, "unpin": null, "filter_packs": [filter_pack()]}),
        },
        Fixture {
            name: "bus.hello",
            kind: Other,
            value: json!({"t": "hello", "instance_id": INSTANCE, "agents": [{"agent_id": "claude-3e", "role": "hive", "vault_id": VAULT}], "cursors": {channel(): 41}}),
        },
        Fixture {
            name: "bus.presence",
            kind: Other,
            value: json!({"t": "presence", "agent_id": "claude-3e", "state": "active"}),
        },
        Fixture {
            name: "bus.publish",
            kind: Other,
            value: json!({
                "t": "publish", "client_msg_id": "01jb8z8m2q4r6s8t0v2w4x6y8z", "channel": channel(), "to": "codex-85",
                "from": "claude-3e", "kind": "status", "priority": "normal", "privacy": "team", "body": {"text": "STATUS C0 Review"},
            }),
        },
        Fixture {
            name: "bus.published",
            kind: Other,
            value: json!({"t": "published", "client_msg_id": "01jb8z8m2q4r6s8t0v2w4x6y8z", "channel": channel(), "seq": 42, "server_time": T0}),
        },
        Fixture {
            name: "bus.deliver",
            kind: Other,
            value: deliver.clone(),
        },
        Fixture {
            name: "bus.ack",
            kind: Other,
            value: json!({"t": "ack", "channel": channel(), "seq": 42}),
        },
        Fixture {
            name: "bus.presence_update",
            kind: Other,
            value: json!({"t": "presence_update", "agent_id": "codex-85", "instance_id": "inst_01jb8z9", "role": "hive-server", "vault_id": VAULT, "state": "idle", "server_time": T0}),
        },
        Fixture {
            name: "bus.control",
            kind: Other,
            value: json!({"t": "control", "control_id": "ctl_01jb8za", "target_agent": "claude-3e", "action": "hold", "reason": "Owner review of the contract.", "issued_by": "admin@acme", "seq": 43, "server_time": T0}),
        },
        Fixture {
            name: "bus.handoff_offer",
            kind: Other,
            value: json!({"t": "handoff.offer", "handoff_id": "hof_01jb8zb", "channel": channel(), "from": "claude-3e", "to": "codex-85", "bundle_ref": "bundle:sha256:5d41402abc4b2a76b9719d911017c592", "deadline": "2026-10-20T12:30:00Z"}),
        },
        Fixture {
            name: "bus.handoff_ack",
            kind: Other,
            value: json!({"t": "handoff.ack", "handoff_id": "hof_01jb8zb", "by": "codex-85"}),
        },
        Fixture {
            name: "bus.handoff_reject",
            kind: Other,
            value: json!({"t": "handoff.reject", "handoff_id": "hof_01jb8zb", "by": "codex-85", "reason": "Busy with E3."}),
        },
        Fixture {
            name: "bus.ping",
            kind: Other,
            value: json!({"t": "ping", "nonce": 7}),
        },
        Fixture {
            name: "bus.error",
            kind: Other,
            value: json!({"t": "error", "client_msg_id": "01jb8z8m2q4r6s8t0v2w4x6y8z", "error": {"code": "forbidden_right", "message": "This lease has no bus.relay right."}}),
        },
        Fixture {
            name: "bus_poll.response",
            kind: Response,
            value: json!({"schema": BUS_POLL_SCHEMA, "frames": [deliver.clone()], "cursor": "c_42"}),
        },
        Fixture {
            name: "bus_events.response",
            kind: Response,
            value: json!({"schema": BUS_EVENTS_SCHEMA, "channel": channel(), "events": [deliver]}),
        },
        Fixture {
            name: "bus_lease.request",
            kind: Request,
            value: json!({
                "schema": BUS_LEASE_REQUEST_SCHEMA, "resource_kind": "file",
                "resource_ref": hex(&Sha256::digest(b"rust/src/core/hive/outbox.rs")),
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
            name: "sealed_key.response",
            kind: Response,
            // A member was removed: epoch 2 is required, this instance holds 1.
            value: json!({"schema": SEALED_KEY_SCHEMA, "vault_id": VAULT, "instance_id": INSTANCE, "epoch": 1, "wrapped_key": wrapped, "epoch_required": 2}),
        },
        Fixture {
            name: "sealed_grant.request",
            kind: Request,
            value: json!({"schema": SEALED_GRANT_REQUEST_SCHEMA, "instance_id": "inst_01jb8z9", "epoch": 1, "wrapped_key": wrapped}),
        },
        Fixture {
            name: "sealed_pending.response",
            kind: Response,
            value: sealed_pending(),
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
            name: "error.key_exists",
            kind: Response,
            value: error(
                "revision_conflict",
                "This sealed vault already has a key.",
                json!({"expected": 1, "actual": 1, "reason": "key_exists"}),
            ),
        },
        Fixture {
            name: "error.sealed_plaintext",
            kind: Response,
            value: error(
                "invalid_request",
                "sealed drafts carry no plaintext",
                json!({"field": "summary", "reason": "sealed_plaintext"}),
            ),
        },
        Fixture {
            name: "status.connected",
            kind: Other,
            value: json!({
                "available": true, "connected": true, "instance": {"id": INSTANCE, "label": "studio-mac"},
                "org": {"name": "Acme AG"}, "last_seen": T0, "url": HIVE_DEFAULT_URL,
                "drafts": {"queued": 2, "refused": 0},
                "policy": {"version": 1, "state": "required", "verified_at": T0,
                           "not_after": "2026-11-20T12:00:00Z", "enforced": false},
                "filter_packs": [{"pack_id": "ch-identifiers", "version": 3}],
                "policy_refusal": null,
                "web_url": HIVE_DEFAULT_URL,
                "bus_controls": [{"target_agent": "claude-3e", "action": "hold", "target_ref": null,
                                  "reason": "Owner review of the contract.", "issued_by": "admin@acme", "since": T0}],
            }),
        },
        // C 1.8, additive status field: the policy of `policy_bundle.required_packs`
        // in force, its required pack installed as signed.
        Fixture {
            name: "status.required_packs",
            kind: Other,
            value: json!({
                "available": true, "connected": true, "instance": {"id": INSTANCE, "label": "studio-mac"},
                "org": {"name": "Acme AG"}, "last_seen": T0, "url": HIVE_DEFAULT_URL,
                "drafts": {"queued": 0, "refused": 0},
                "policy": {"version": 2, "state": "required", "verified_at": T0,
                           "not_after": "2026-11-20T12:00:00Z", "enforced": true,
                           "required_filter_packs": [{"pack_id": "ch-identifiers", "version": 3, "installed": true}]},
                "filter_packs": [{"pack_id": "ch-identifiers", "version": 3}],
                "policy_refusal": null,
                "web_url": HIVE_DEFAULT_URL,
            }),
        },
        Fixture {
            name: "status.lapsed",
            kind: Other,
            value: json!({
                "available": true, "connected": false, "instance": {"id": INSTANCE, "label": "studio-mac"},
                "org": {"name": "Acme AG"}, "last_seen": T0, "url": HIVE_DEFAULT_URL,
                "drafts": {"queued": 0, "refused": 0}, "policy": null, "filter_packs": [],
                "policy_refusal": null, "web_url": HIVE_DEFAULT_URL,
                "bus_controls_inactive": "lease_lapsed",
            }),
        },
        Fixture {
            name: "status.not_connected",
            kind: Other,
            value: serde_json::to_value(HiveCliStatusV1::not_connected(None)).unwrap(),
        },
    ];
    let mut list = list.into_iter().collect::<Vec<_>>();
    list.extend(task_fixtures());
    list.extend(message_fixtures());
    list.extend(knowledge_fixtures());
    list.push(metrics_hive_fixture());
    list.extend(profile_fixtures());
    list.extend(swarm_fixtures());
    list.extend(automerge_fixtures());
    list.extend(code_graph_fixtures());
    list.extend(optin_fixtures());
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
    use lean_ctx_protocol::hive::tasks::*;
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
            // Sealed vault: no readable text; a proposed task has no number yet (AAD 0).
            name: "task_propose.sealed.request",
            kind: Request,
            value: json!({
                "schema": TASK_PROPOSE_SCHEMA, "origin": {"kind": "agent"}, "session_ref": "ses_01jb8z7",
                "sealed": seal(&canonical_json(&json!({
                    "schema": SEALED_TASK_CONTENT_SCHEMA, "title": "Explain the retry budget",
                    "acceptance": acceptance,
                })).unwrap(), 0, SEALED_TASK_CONTENT_SCHEMA, [0x70; 24]),
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
            // Sealed: summary and evidence travel only inside the blob (AAD: task number 7).
            name: "task_result.sealed.request",
            kind: Request,
            value: json!({
                "schema": TASK_RESULT_SCHEMA, "claim": claim,
                "sealed": seal(&canonical_json(&json!({
                    "schema": SEALED_TASK_RESULT_SCHEMA, "summary": summary, "evidence": evidence,
                })).unwrap(), 7, SEALED_TASK_RESULT_SCHEMA, [0x71; 24]),
            }),
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
    ]
}

/// C0 1.9 section 2: agent messages and networks.
fn message_fixtures() -> Vec<Fixture> {
    use Kind::*;
    use lean_ctx_protocol::hive::messages::*;
    let org = json!({"kind": "org", "channel": channel()});
    let swarm = json!({"kind": "swarm", "swarm_id": "sw_01jbretry"});
    let question = json!({
        "t": "message", "client_msg_id": "01JBMSGQ1", "network": org, "type": "question",
        "from": "claude-3e", "deadline": "2026-10-21T12:00:00Z",
        "body": {"text": "Which retry budget does payments use, 3 or 5?",
                 "references": [{"kind": "task", "number": 9}, {"kind": "fact", "id": "f-retry-3"}]},
    });
    vec![
        Fixture {
            name: "bus.message.question",
            kind: Other,
            value: question,
        },
        Fixture {
            name: "bus.message.answer",
            kind: Other,
            value: json!({
                "t": "message", "client_msg_id": "01JBMSGA1", "network": org, "type": "answer",
                "from": "codex-85", "to": "claude-3e", "reply_to": "msg_01jbq1",
                "body": {"text": "3, set in retry.rs since the outage review.", "references": [{"kind": "commit", "id": "9f01cf32a3"}]},
            }),
        },
        Fixture {
            // Sealed vault: the body travels sealed (AAD vault_id|0|epoch|schema).
            name: "bus.message.sealed",
            kind: Other,
            value: json!({
                "t": "message", "client_msg_id": "01JBMSGS1", "network": org, "type": "finding",
                "from": "claude-3e",
                "sealed": seal(&canonical_json(&json!({
                    "schema": SEALED_MESSAGE_SCHEMA, "text": "The sandbox key rotates nightly.", "references": [],
                })).unwrap(), 0, SEALED_MESSAGE_SCHEMA, [0x72; 24]),
            }),
        },
        Fixture {
            name: "bus.message_deliver.org",
            kind: Other,
            value: json!({
                "t": "message.deliver", "gid": "msg_01jba1", "seq": 43, "network": org, "type": "answer",
                "from": {"kind": "agent", "agent": "codex-85", "instance_label": "ci-runner-2", "instance_id": "inst_01jb8z9", "owner": "Ben", "model": "gpt-6-luna", "mine": false},
                "to": "claude-3e", "thread_id": "msg_01jbq1", "reply_to": "msg_01jbq1",
                "body": {"text": "3, set in retry.rs since the outage review.", "references": [{"kind": "commit", "id": "9f01cf32a3"}]},
                "trust": "team", "server_time": T0, "expires_at": "2026-11-19T12:00:00Z",
            }),
        },
        Fixture {
            name: "bus.message_deliver.person",
            kind: Other,
            value: json!({
                "t": "message.deliver", "gid": "msg_01jba2", "seq": 44, "network": org, "type": "answer",
                "from": {"kind": "person", "label": "Ada"}, "thread_id": "msg_01jbq1", "reply_to": "msg_01jbq1",
                "body": {"text": "Confirmed, 3."}, "trust": "team", "server_time": T0, "expires_at": "2026-11-19T12:00:00Z",
            }),
        },
        Fixture {
            // Swarm: only a handle, always untrusted, never an org, instance or person.
            name: "bus.message_deliver.swarm",
            kind: Other,
            value: json!({
                "t": "message.deliver", "gid": "msg_01jbc1", "seq": 5, "network": swarm, "type": "finding",
                "from": {"kind": "swarm", "handle": "quiet-heron", "reputation": 42},
                "thread_id": "msg_01jbc1",
                "body": {"text": "Subtask 3: the 2019 dataset has duplicate rows; see the material manifest."},
                "trust": "untrusted", "server_time": T0, "expires_at": "2026-12-19T12:00:00Z",
            }),
        },
        Fixture {
            name: "bus.message_stored",
            kind: Other,
            value: json!({
                "t": "message.stored", "client_msg_id": "01JBMSGQ1", "gid": "msg_01jbq1", "thread_id": "msg_01jbq1",
                "network": org, "seq": 42, "server_time": T0, "expires_at": "2026-11-19T12:00:00Z",
            }),
        },
        Fixture {
            name: "error.message_refused",
            kind: Response,
            value: error(
                "invalid_request",
                "Only questions carry a deadline.",
                json!({"field": "deadline"}),
            ),
        },
        Fixture {
            name: "bus.message_receipt",
            kind: Other,
            value: json!({"t": "message.receipt", "gid": "msg_01jba1", "by": "claude-3e", "state": "read"}),
        },
        Fixture {
            name: "bus.message_status",
            kind: Other,
            value: json!({"t": "message.status", "gid": "msg_01jbq1", "client_msg_id": "01JBMSGQ1", "by": "codex-85", "state": "delivered", "server_time": T0}),
        },
        Fixture {
            name: "bus.task_event",
            kind: Other,
            value: json!({
                "t": "task.event", "channel": channel(), "seq": 45, "number": 7, "event": "claimed", "state": "claimed",
                "by": {"kind": "instance", "label": "ada-laptop", "agent": "claude-3e", "owner": "Ada", "mine": true},
                "server_time": T0,
            }),
        },
        Fixture {
            name: "message_list.response",
            kind: Response,
            value: json!({
                "schema": MESSAGE_LIST_SCHEMA,
                "messages": [{
                    "gid": "msg_01jba2", "seq": 44, "network": org, "type": "answer",
                    "from": {"kind": "person", "label": "Ada"}, "thread_id": "msg_01jbq1", "reply_to": "msg_01jbq1",
                    "body": {"text": "Confirmed, 3."}, "trust": "team", "server_time": T0, "expires_at": "2026-11-19T12:00:00Z",
                }],
                "next": "msg_01jba2",
            }),
        },
        Fixture {
            name: "bus.message.direct",
            kind: Other,
            value: json!({
                "t": "message", "client_msg_id": "01JBMSGD1", "network": org, "type": "handoff",
                "from": "claude-3e", "to": "codex-85", "to_instance": "inst_01jb8z9",
                "body": {"text": "The outbox ordering task is yours from here.", "references": [{"kind": "task", "number": 9}]},
            }),
        },
        Fixture {
            name: "vault.sealed_new.response",
            kind: Response,
            value: json!({"schema": VAULT_SCHEMA, "vault_id": VAULT, "vault": {"ns": "acme", "name": "payments"}, "sealed": true, "key_epoch": 0}),
        },
        Fixture {
            name: "error.to_instance_not_found",
            kind: Response,
            value: error(
                "not_found",
                "No such instance in this organization.",
                json!({"field": "to_instance"}),
            ),
        },
        Fixture {
            name: "error.ambiguous_recipient",
            kind: Response,
            value: error(
                "invalid_state",
                "Several instances run this agent; name one in to_instance.",
                json!({"reason": "ambiguous_recipient"}),
            ),
        },
        Fixture {
            name: "error.reply_not_found",
            kind: Response,
            value: error(
                "not_found",
                "No such message.",
                json!({"field": "reply_to"}),
            ),
        },
        Fixture {
            name: "error.cross_network",
            kind: Response,
            value: error(
                "invalid_state",
                "Replies stay in their network.",
                json!({"reason": "cross_network"}),
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
        "hive.outbox-order",
        "The outbox keeps upload order per project; reordering breaks expected_revision.",
    );
    gotcha["kind"] = json!("gotcha");
    gotcha["anchors"] = json!([anchor(
        "rust/src/core/hive/outbox.rs",
        "hive::outbox::enqueue",
        OUTBOX_ENQUEUE
    )]);
    document["facts"].as_array_mut().unwrap().push(gotcha);
    document["relations"]
        .as_array_mut()
        .unwrap()
        .push(json!({"from": "hive.outbox-order", "to": "hive.never-blocks", "type": "explains"}));
    document["pages"] = json!([{
        "id": "hive.outbound",
        "title": "Outbound path",
        "items": [
            {"kind": "prose", "text": "Everything an engine sends to Hive takes one path."},
            {"kind": "fact", "id": "hive.screening-first"},
            {"kind": "fact", "id": "hive.never-blocks"},
            {"kind": "fact", "id": "hive.outbox-order"},
        ],
    }]);
    document
}

/// Section 3: pages, anchors, fact states, quarantine and rollback.
fn knowledge_fixtures() -> Vec<Fixture> {
    use Kind::*;
    let wiki = wiki_document();
    let to_verify = json!({
        "fact": "hive.outbox-order", "state": "to_verify", "reasons": ["anchor_changed"],
        "since": "2026-10-21T09:00:00Z",
    });
    let contradicted = json!({
        "fact": "hive.never-blocks", "state": "contradicted", "reasons": ["contradiction"],
        "contradicted_by": ["hive.screening-first"], "since": "2026-10-21T09:00:00Z",
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
            name: "knowledge_revision.quarantined.response",
            kind: Response,
            value: json!({
                "schema": KNOWLEDGE_REVISION_SCHEMA, "vault_id": VAULT, "revision": 5, "document": wiki,
                "updated_at": "2026-10-20T13:00:00Z", "state": "quarantined",
            }),
        },
        Fixture {
            name: "revision_list.quarantine.response",
            kind: Response,
            value: json!({
                "schema": REVISION_LIST_SCHEMA, "vault_id": VAULT,
                "revisions": [
                    {"revision": 4, "created_at": T0, "proposal": 12, "digest": hex(&Sha256::digest(canonical_json(&wiki).unwrap()))},
                    {"revision": 5, "created_at": "2026-10-20T13:00:00Z", "proposal": 13, "digest": hex(&Sha256::digest(b"revision-5")), "state": "rolled_back"},
                    {"revision": 6, "created_at": "2026-10-20T14:00:00Z", "digest": hex(&Sha256::digest(canonical_json(&wiki).unwrap())), "rolls_back": 4},
                ],
            }),
        },
        Fixture {
            name: "knowledge_put.rollback.request",
            kind: Request,
            value: json!({"schema": KNOWLEDGE_PUT_REQUEST_SCHEMA, "expected_revision": 5, "document": wiki, "rolls_back": 4}),
        },
        Fixture {
            name: "knowledge_diff.response",
            kind: Response,
            value: json!({
                "schema": REVISION_DIFF_SCHEMA, "vault_id": VAULT, "from": 3, "to": 4,
                "ops": [
                    {"op": "add_fact", "fact": wiki["facts"][2]},
                    {"op": "add_relation", "relation": wiki["relations"][1]},
                    {"op": "add_page", "page": wiki["pages"][0]},
                ],
            }),
        },
        Fixture {
            name: "knowledge_diff.credits.response",
            kind: Response,
            value: {
                let mut credited = wiki["facts"][2].clone();
                credited["from_contributions"] = json!(["p12"]);
                json!({
                    "schema": REVISION_DIFF_SCHEMA, "vault_id": VAULT, "from": 3, "to": 4,
                    "ops": [
                        {"op": "add_fact", "fact": credited},
                        {"op": "add_relation", "relation": wiki["relations"][1]},
                        {"op": "add_page", "page": wiki["pages"][0]},
                    ],
                })
            },
        },
        Fixture {
            name: "fact_states.request",
            kind: Request,
            value: json!({
                "schema": facts::FACT_STATE_REPORT_SCHEMA, "revision": 4,
                "states": [
                    contradicted,
                    {
                        "fact": "hive.outbox-order", "state": "to_verify", "reasons": ["anchor_moved"],
                        "moved_to": anchor("rust/src/core/hive/outbox/queue.rs", "hive::outbox::queue::enqueue", OUTBOX_ENQUEUE),
                        "since": "2026-10-21T09:00:00Z",
                    },
                ],
            }),
        },
        Fixture {
            name: "fact_states.sealed.request",
            kind: Request,
            value: json!({"schema": facts::FACT_STATE_REPORT_SCHEMA, "revision": 4, "counts": {"to_verify": 1, "contradicted": 1}}),
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
                    {"id": "code:rust/src/core/hive/outbox.rs#hive::outbox::enqueue", "kind": "code", "label": "hive::outbox::enqueue"},
                    {"id": "fact:hive.never-blocks", "kind": "knowledge", "label": "Hive never blocks a tool call"},
                    {"id": "fact:hive.outbox-order", "kind": "knowledge", "label": "The outbox keeps upload order per project"},
                    {"id": "page:hive.outbound", "kind": "knowledge", "label": "Outbound path"},
                    {"id": "proposal:12", "kind": "work", "label": "Outbox ordering"},
                ],
                "edges": [
                    {"from": "actor:claude-3e", "to": "proposal:12", "type": "worked_on"},
                    {"from": "fact:hive.outbox-order", "to": "code:rust/src/core/hive/outbox.rs#hive::outbox::enqueue", "type": "applies_to"},
                    {"from": "fact:hive.outbox-order", "to": "fact:hive.never-blocks", "type": "explains"},
                    {"from": "page:hive.outbound", "to": "fact:hive.never-blocks", "type": "shows"},
                    {"from": "page:hive.outbound", "to": "fact:hive.outbox-order", "type": "shows"},
                    {"from": "proposal:12", "to": "fact:hive.outbox-order", "type": "changed"},
                ],
            }),
        },
        Fixture {
            name: "bus.knowledge_event",
            kind: Other,
            value: json!({
                "t": "knowledge.event", "channel": channel(), "seq": 46, "revision": 5, "event": "quarantined",
                "review_by": "2026-10-21T13:00:00Z", "server_time": "2026-10-20T13:00:00Z",
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
            name: "error.sealed_view",
            kind: Response,
            value: error(
                "invalid_state",
                "A sealed vault has no server-side view; derive it on the engine.",
                json!({"reason": "sealed"}),
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
            name: "engine_optin.response",
            kind: Response,
            value: json!({
                "schema": optin::ENGINE_OPTIN_SCHEMA, "optin_id": "opt_01jbq7k2", "active": true,
                "vaults": [VAULT],
                "networks": [{"kind": "org", "channel": channel()}, {"kind": "swarm", "swarm_id": "sw_01jbretry"}],
                "topics": ["payments", "retries"],
                "daily_budget": {"tasks": 5, "tokens": 2_000_000, "usd_micros": 20_000_000},
                "used_today": {"day": "2026-10-20", "tasks": 2, "tokens": 310_400, "usd_micros": 1_870_000},
                "updated_at": "2026-10-20T08:00:00Z",
            }),
        },
        // Hive task #19: every limit left out is no limit, the task count too.
        Fixture {
            name: "engine_optin.unlimited.response",
            kind: Response,
            value: json!({
                "schema": optin::ENGINE_OPTIN_SCHEMA, "optin_id": "opt_01jbq7k4", "active": true,
                "vaults": [VAULT], "networks": [], "topics": [],
                "daily_budget": {},
                "used_today": {"day": "2026-10-20", "tasks": 140, "tokens": 9_310_400, "usd_micros": 41_870_000},
                "updated_at": "2026-10-20T08:00:00Z",
            }),
        },
        Fixture {
            name: "engine_optin.stopped.response",
            kind: Response,
            value: json!({
                "schema": optin::ENGINE_OPTIN_SCHEMA, "optin_id": "opt_01jbq7k3", "active": false,
                "vaults": [VAULT], "networks": [], "topics": [],
                "daily_budget": {"tasks": 5},
                "used_today": {"day": "2026-10-20", "tasks": 2, "tokens": 310_400, "usd_micros": 1_870_000},
                "stopped_at": T0, "updated_at": T0,
            }),
        },
        Fixture {
            name: "engine_optin.none.response",
            kind: Response,
            value: json!({
                "schema": optin::ENGINE_OPTIN_SCHEMA, "active": false, "vaults": [], "networks": [], "topics": [],
                "daily_budget": {"tasks": 5},
                "used_today": {"day": "2026-10-20", "tasks": 0, "tokens": 0, "usd_micros": 0},
            }),
        },
        Fixture {
            name: "engine_optin_stop.request",
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
                "This engine is not opted in.",
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
fn profile_fixtures() -> Vec<Fixture> {
    use Kind::*;
    let profile = json!({"display_name": "Claude (contracts)", "model": "claude-opus-5-5", "roles": ["contracts", "review"]});
    vec![
        Fixture {
            name: "bus.presence.profile",
            kind: Other,
            value: json!({"t": "presence", "agent_id": "claude-3e", "state": "active", "profile": profile}),
        },
        Fixture {
            name: "bus.presence_update.profile",
            kind: Other,
            value: json!({
                "t": "presence_update", "agent_id": "claude-3e", "instance_id": "inst_01jb8z3", "role": "contracts",
                "vault_id": VAULT, "state": "active", "server_time": T0,
                "profile_id": "prof_01jbc3e", "profile": profile,
            }),
        },
        Fixture {
            name: "agent_profile_list.response",
            kind: Response,
            value: json!({
                "schema": profiles::AGENT_PROFILE_LIST_SCHEMA,
                "profiles": [
                    {
                        "profile_id": "prof_01jbc3e", "agent": "claude-3e", "instance_label": "studio-mac",
                        "owner": "Ada Muster", "display_name": "Claude (contracts)", "model": "claude-opus-5-5",
                        "roles": ["contracts", "review"], "strengths": ["hive protocol", "decision facts"],
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
        },
        Fixture {
            name: "handle.response",
            kind: Response,
            value: json!({
                "schema": profiles::HANDLE_SCHEMA, "handle": "quiet-heron",
                "reputation": {"score": 3, "contributions": 2, "weight_pct": 25},
                "strengths": ["retries"], "since": "2026-10-01T00:00:00Z",
            }),
        },
        Fixture {
            name: "error.limit_exceeded",
            kind: Response,
            value: error(
                "rate_limited",
                "Free handles keep one open question.",
                json!({"reason": "limit_exceeded", "field": "open_swarms", "limit": 1, "used": 1, "retry_after_s": 3600}),
            ),
        },
    ]
}

const SWARM_ID: &str = "sw_01jbretry";
const SUBTASK_ID: &str = "ss_01jbretry7";
const REVEAL_NONCE: &str = "AAECAwQFBgcICQoLDA0ODxAREhM=";

fn swarm_result() -> Value {
    json!({
        "schema": swarm::SWARM_RESULT_SCHEMA, "fencing_token": 3,
        "summary": "payments retries 3 times; the 5 in the runbook is stale",
        "body": "retry.rs sets RETRY_BUDGET = 3 since 2026-09; the runbook predates it.",
        "value": 3, "usage": {"tokens": 48_200, "usd_micros": 190_000},
    })
}

fn swarm_reveal() -> Value {
    json!({"schema": swarm::VERIFICATION_REVEAL_SCHEMA, "verdict": "agree", "value": 3, "nonce": REVEAL_NONCE})
}

/// Section 6: Swarm engine routes.
fn swarm_fixtures() -> Vec<Fixture> {
    use Kind::*;
    let trust = "untrusted";
    let swarm_ref = json!({"swarm_id": SWARM_ID, "title": "Which retry budget does payments use?", "licence": "CC-BY-4.0", "scope": "public"});
    let subtask = json!({
        "subtask_id": SUBTASK_ID, "swarm_id": SWARM_ID, "number": 7,
        "title": "Find the retry budget in code", "acceptance": "The constant and the file it lives in.",
        "expected_kind": "number", "state": "open", "mine": false, "swarm": swarm_ref,
    });
    let result: swarm::SwarmResultRequestV1 = serde_json::from_value(swarm_result()).unwrap();
    let reveal: swarm::VerificationRevealV1 = serde_json::from_value(swarm_reveal()).unwrap();
    let seed = json!({
        "schema": swarm::SWARM_SEED_SCHEMA, "title": "HF incident replay", "body": "Reconstruct the incident timeline from the excerpts.",
        "topics": ["incident", "replay"], "licence": "CC-BY-4.0",
        "subtasks": [
            {"title": "When did the first alert fire?", "acceptance": "A UTC timestamp from the logs.", "expected_kind": "text"},
            {"title": "Which deploy started it?", "acceptance": "The deploy hash.", "expected_kind": "hash"},
        ],
        "material": [{"name": "timeline.txt", "media_type": "text/plain", "sha256": hex(&Sha256::digest(b"timeline")), "bytes": 8}],
    });
    vec![
        Fixture {
            name: "swarm.response",
            kind: Response,
            value: json!({
                "schema": swarm::SWARM_SCHEMA, "swarm_id": SWARM_ID,
                "title": "Which retry budget does payments use?", "body": "Code says 3, the runbook says 5.",
                "topics": ["payments", "retries"], "licence": "CC-BY-4.0", "scope": "public", "state": "working",
                "owner": {"handle": "quiet-heron"},
                "material": [{"name": "runbook.md", "media_type": "text/markdown", "sha256": hex(&Sha256::digest(b"runbook")), "bytes": 7}],
                "answer_revision": 0, "trust": trust,
            }),
        },
        Fixture {
            name: "swarm.team.response",
            kind: Response,
            value: json!({
                "schema": swarm::SWARM_SCHEMA, "swarm_id": SWARM_ID,
                "title": "Which retry budget does payments use?", "body": "Code says 3, the runbook says 5.",
                "topics": ["payments"], "licence": "CC-BY-4.0", "scope": "team", "org_id": ORG, "state": "working",
                "owner": {"handle": "quiet-heron"}, "material": [],
                "answer_revision": 0, "trust": trust,
            }),
        },
        Fixture {
            name: "swarm_subtask_list.response",
            kind: Response,
            value: json!({"schema": swarm::SWARM_SUBTASK_LIST_SCHEMA, "items": [subtask], "trust": trust}),
        },
        Fixture {
            name: "swarm_subtask_propose.request",
            kind: Request,
            value: json!({
                "schema": swarm::SWARM_SUBTASK_PROPOSE_SCHEMA, "title": "Check the runbook's history",
                "acceptance": "The commit that last changed the retry line.", "expected_kind": "hash",
            }),
        },
        Fixture {
            name: "swarm_subtask.response",
            kind: Response,
            value: json!({
                "subtask_id": "ss_01jbretry8", "swarm_id": SWARM_ID, "number": 8,
                "title": "Check the runbook's history", "acceptance": "The commit that last changed the retry line.",
                "expected_kind": "hash", "state": "proposed", "mine": false,
            }),
        },
        Fixture {
            name: "swarm_claim.request",
            kind: Request,
            value: json!({"schema": swarm::SWARM_CLAIM_REQUEST_SCHEMA, "optin_id": "opt_01jbq7k2", "minutes": 15}),
        },
        Fixture {
            name: "swarm_claim_renew.request",
            kind: Request,
            value: json!({"schema": swarm::SWARM_CLAIM_RENEW_SCHEMA, "fencing_token": 3, "minutes": 15}),
        },
        Fixture {
            name: "swarm_claim_release.request",
            kind: Request,
            value: json!({"schema": swarm::SWARM_CLAIM_RELEASE_SCHEMA, "fencing_token": 3}),
        },
        Fixture {
            name: "swarm_claim.response",
            kind: Response,
            value: json!({
                "schema": swarm::SWARM_CLAIM_SCHEMA, "subtask_id": SUBTASK_ID, "swarm_id": SWARM_ID,
                "number": 7, "fencing_token": 3, "until": "2026-10-20T12:15:00Z", "trust": trust,
            }),
        },
        Fixture {
            name: "swarm_result.request",
            kind: Request,
            value: swarm_result(),
        },
        Fixture {
            name: "swarm_result_ack.response",
            kind: Response,
            value: json!({
                "schema": swarm::SWARM_RESULT_ACK_SCHEMA, "result_id": "sr_01jbretry7a", "sha256": result.digest().unwrap(),
                "state": "verifying", "verifiers_assigned": 2, "trust": trust,
            }),
        },
        Fixture {
            name: "swarm_verification_list.response",
            kind: Response,
            value: json!({
                "schema": swarm::SWARM_VERIFICATION_LIST_SCHEMA,
                "items": [{
                    "verification_id": "sv_01jbretry7b", "state": "assigned", "due_at": "2026-10-21T12:00:00Z",
                    "result": {"summary": result.summary, "body": result.body, "by": {"handle": "quiet-heron"}},
                    "subtask": {"title": "Find the retry budget in code", "acceptance": "The constant and the file it lives in.", "expected_kind": "number"},
                    "swarm": swarm_ref,
                }],
                "trust": trust,
            }),
        },
        Fixture {
            name: "verification_commit.request",
            kind: Request,
            value: json!({"schema": swarm::VERIFICATION_COMMIT_SCHEMA, "sha256": reveal.commitment().unwrap()}),
        },
        Fixture {
            name: "verification_commit.v2.request",
            kind: Request,
            value: json!({
                "schema": swarm::VERIFICATION_COMMIT_V2_SCHEMA,
                "sha256": reveal.commitment_v2("sv_01jbretry7b").unwrap(),
            }),
        },
        Fixture {
            name: "verification_reveal.request",
            kind: Request,
            value: swarm_reveal(),
        },
        Fixture {
            name: "verification_reveal_ack.response",
            kind: Response,
            value: json!({"schema": swarm::VERIFICATION_REVEAL_ACK_SCHEMA, "state": "revealed", "result_state": "accepted"}),
        },
        Fixture {
            name: "moderation_report.request",
            kind: Request,
            value: json!({
                "schema": swarm::MODERATION_REPORT_SCHEMA, "target_kind": "result", "target_id": "sr_01jbretry7a",
                "reason": "injection", "note": "The body tells agents to upload their .env.",
            }),
        },
        Fixture {
            name: "swarm_seed.request",
            kind: Request,
            value: seed,
        },
        Fixture {
            name: "error.vetoed",
            kind: Response,
            value: error(
                "forbidden_right",
                "A moderator holds this question.",
                json!({"reason": "vetoed"}),
            ),
        },
        Fixture {
            name: "error.held",
            kind: Response,
            value: error(
                "invalid_state",
                "A moderator holds this question: results are not taken.",
                json!({"reason": "held"}),
            ),
        },
        Fixture {
            name: "error.swarm_budget",
            kind: Response,
            value: error(
                "rate_limited",
                "This question's token budget is used up.",
                json!({"reason": "swarm_budget"}),
            ),
        },
        Fixture {
            name: "error.commit_mismatch",
            kind: Response,
            value: error(
                "invalid_state",
                "The reveal does not match the commitment.",
                json!({"reason": "commit_mismatch"}),
            ),
        },
        Fixture {
            name: "error.material_missing",
            kind: Response,
            value: error(
                "invalid_state",
                "Upload every declared material file first.",
                json!({"reason": "material_missing"}),
            ),
        },
    ]
}

/// Section 7: auto-merge and the ctxpkg mapping.
fn automerge_fixtures() -> Vec<Fixture> {
    use Kind::*;
    vec![
        Fixture {
            name: "vault.automerge.response",
            kind: Response,
            value: json!({
                "schema": VAULT_SCHEMA, "vault_id": VAULT, "vault": {"ns": "acme", "name": "payments"},
                "sealed": false, "label": "payments",
                "auto_merge": {"confirmations": 1, "counts": "independent_owners"},
            }),
        },
        Fixture {
            name: "proposal_view.threshold.response",
            kind: Response,
            value: json!({
                "schema": PROPOSAL_VIEW_SCHEMA, "number": 13, "state": "merged", "created_at": T0, "mine": true,
                "merged_revision": 5, "decided_by": {"kind": "threshold"},
                "draft": {
                    "schema": DRAFT_PROPOSAL_SCHEMA, "state": "draft", "summary": "Outbox ordering is per project",
                    "context_patch": {"base_revision": 4, "ops": [{"op": "retract_fact", "id": "hive.drafts-rebase"}]},
                    "session_ref": "ses_01jbq9a",
                    "engine_screening": {"scanner_version": 1, "secrets_removed": 0, "pii_removed": 0, "items_dropped": 0},
                },
            }),
        },
        Fixture {
            name: "proposal_confirm.request",
            kind: Request,
            value: json!({"schema": automerge::PROPOSAL_CONFIRM_SCHEMA, "session_ref": "ses_01jbq9c", "verdict": "confirm"}),
        },
        Fixture {
            name: "proposal_confirm.object.request",
            kind: Request,
            value: json!({
                "schema": automerge::PROPOSAL_CONFIRM_SCHEMA, "session_ref": "ses_01jbq9d", "verdict": "object",
                "reason": "The runbook still says 5; the fact needs its source.",
            }),
        },
        Fixture {
            name: "proposal_confirm_ack.response",
            kind: Response,
            value: json!({
                "schema": automerge::PROPOSAL_CONFIRM_ACK_SCHEMA, "number": 12, "confirmations": 1,
                "required": 1, "state": "ready",
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
            name: "error.keys_lost",
            kind: Response,
            value: error(
                "invalid_state",
                "This sealed vault has content but no member holds a key for it.",
                json!({"reason": "keys_lost"}),
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

/// `hive::ctxpkg` ids of the section 3 wiki document, and import slugs.
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
        "note": "ids are sha256: + hex of canonical_json of the typed objects in hive::ctxpkg; the document is knowledge_revision.wiki.response",
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
            {"id": "code:rust/src/core/hive/outbox.rs", "kind": "file", "lang": "rust"},
            {"id": "code:rust/src/core/hive/outbox.rs#hive::outbox::enqueue", "kind": "symbol", "lang": "rust", "symbol_kind": "fn"},
            {"id": "code:rust/src/core/hive/queue.rs", "kind": "file", "lang": "rust"},
            {"id": "code:rust/src/core/hive/queue.rs#hive::queue::push", "kind": "symbol", "lang": "rust", "symbol_kind": "fn"},
        ],
        "edges": [
            {"from": "code:rust/src/core/hive/outbox.rs", "to": "code:rust/src/core/hive/outbox.rs#hive::outbox::enqueue", "type": "contains"},
            {"from": "code:rust/src/core/hive/outbox.rs", "to": "code:rust/src/core/hive/queue.rs", "type": "imports"},
            {"from": "code:rust/src/core/hive/outbox.rs#hive::outbox::enqueue", "to": "code:rust/src/core/hive/queue.rs#hive::queue::push", "type": "calls"},
            {"from": "code:rust/src/core/hive/queue.rs", "to": "code:rust/src/core/hive/queue.rs#hive::queue::push", "type": "contains"},
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
                "sealed": true, "key_epoch": 1, "code_graph": "full",
            }),
        },
        Fixture {
            name: "code_graph_put.request",
            kind: Request,
            value: json!({"schema": codegraph::CODE_GRAPH_PUT_SCHEMA, "graph": graph}),
        },
        Fixture {
            name: "code_graph_put.sealed.request",
            kind: Request,
            value: json!({
                "schema": codegraph::CODE_GRAPH_PUT_SCHEMA,
                "sealed": seal(&canonical_json(&graph).unwrap(), 0, codegraph::CODE_GRAPH_SCHEMA, [0x47; 24]),
            }),
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
            name: "error.no_handle",
            kind: Response,
            value: error(
                "forbidden_right",
                "Choose a Swarm handle first.",
                json!({"reason": "no_handle"}),
            ),
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

/// X4 addendum: a metrics day with `hive`.
fn metrics_hive_fixture() -> Fixture {
    let mut day = metrics_day();
    day["hive"] = json!({
        "knowledge_hits_referenced": 14,
        "task_usage": [
            {"number": 7, "input_tokens": 182_000, "output_tokens": 9_400, "list_usd_micros": 687_000},
            {"number": 9, "input_tokens": 41_500, "output_tokens": 2_100, "list_usd_micros": 156_000},
        ],
        "first_context_ms_p50": 840,
        "first_context_sessions": 6,
    });
    Fixture {
        name: "metrics.hive.request",
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
        "note": "Test-only keys; never valid outside fixtures and the fake Hive service.",
        "keys": {
            "root_seed_hex": hex(&ROOT_SEED), "root_ed25519_pub": public(ROOT_SEED),
            "signing_seed_hex": hex(&SIGNING_SEED), "signing_kid": KID, "signing_ed25519_pub": public(SIGNING_SEED),
            "instance_seed_hex": hex(&INSTANCE_SEED), "instance_ed25519_pub": public(INSTANCE_SEED),
            "policy_signer_seed_hex": hex(&POLICY_SEED),
            "org_salt_hex": hex(&ORG_SALT), "repo_key_hex": hex(&REPO_KEY),
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
        "sealed_aad": String::from_utf8(sealed_aad(VAULT, 3, 1, RESEARCH_CONTEXT_FORMAT)).unwrap(),
        "sealed_wrap": {
            "recipient_kx_seed_hex": hex(&KX_SEED),
            "recipient_kx_public_b64": B64.encode(recipient_kx().1),
            "recipient_kx_fingerprint": kx_fingerprint(&recipient_kx().1),
            "ephemeral_scalar_hex": hex(&EPHEMERAL_SCALAR),
            "aad": String::from_utf8(sealed_wrap_aad(VAULT, 1, INSTANCE)).unwrap(),
            "wrapped_key": wrap_repo_key(INSTANCE, 1),
        },
        "receipt_digest": receipt_digest(&receipt_document()).unwrap(),
        "org_policy": org_policy().1,
        "org_policy_required_packs": org_policy_requiring_packs().1,
    })
}

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/hive")
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

/// Fixtures that originate in Hive (signed with the vendor test root there)
/// and are copied here unchanged; [`root_cert_fixtures_chain_to_the_lease`]
/// checks them.
const EXTERNAL_FIXTURES: [&str; 3] = [
    "root_cert.response.json",
    "root_cert.vectors.json",
    "root_cert_request.json",
];

#[test]
fn root_cert_fixtures_chain_to_the_lease() {
    use lean_ctx_protocol::hive::root_cert::RootCertV1;
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
    let at = |t: &str| chrono::DateTime::parse_from_rfc3339(t).unwrap();
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
    let write = std::env::var_os("HIVE_FIXTURES_WRITE").is_some();
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
            panic!("{}: {error} (run with HIVE_FIXTURES_WRITE=1)", fixture.name)
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
            n if n.starts_with("error.") => check::<HiveErrorBody>(&fixture, ok),
            "device_code.request" | "device_code_labelled.request" => {
                check(&fixture, DeviceCodeRequestV1::validate)
            }
            "device_code.response" => check(&fixture, DeviceCodeV1::validate),
            "device_token.request" => check(&fixture, DeviceTokenRequestV1::validate),
            "enrollment.response" => check(&fixture, EnrollmentV1::validate),
            "token.request" => check(&fixture, InstanceTokenRequestV1::validate),
            "access_token.response" => check(&fixture, AccessTokenV1::validate),
            "lease_envelope.response" => check(&fixture, |envelope: &HiveLeaseEnvelopeV1| {
                envelope.validate_shape()?;
                envelope.lease().map(|_| ())
            }),
            "keyset.response" => check(&fixture, HiveKeySetV1::validate),
            "org_salt.response" => check(&fixture, OrgSaltV1::validate),
            n if n.starts_with("vault_resolve.") => {
                check(&fixture, VaultResolveRequestV1::validate)
            }
            n if n.starts_with("vault.") => check(&fixture, VaultV1::validate),
            n if n.starts_with("knowledge_revision.") => {
                check(&fixture, KnowledgeRevisionV1::validate)
            }
            n if n.starts_with("revision_list.") => check(&fixture, RevisionListV1::validate),
            n if n.starts_with("knowledge_diff.") => check(&fixture, RevisionDiffV1::validate),
            n if n.starts_with("fact_states.") => {
                check(&fixture, facts::FactStateReportV1::validate)
            }
            "fact_states_ack.response" => check::<facts::FactStateAckV1>(&fixture, ok),
            "graph.response" => check(&fixture, graph::GraphV1::validate),
            n if n.starts_with("proposal_confirm.") => {
                check(&fixture, automerge::ProposalConfirmV1::validate)
            }
            "proposal_confirm_ack.response" => {
                check(&fixture, automerge::ProposalConfirmAckV1::validate)
            }
            "ctxpkg.vectors" => {}
            n if n.starts_with("code_graph_put.") => {
                check(&fixture, codegraph::CodeGraphPutV1::validate)
            }
            "code_graph_ack.response" => check::<codegraph::CodeGraphAckV1>(&fixture, ok),
            "swarm.response" | "swarm.team.response" => check(&fixture, swarm::SwarmV1::validate),
            "swarm_subtask_list.response" => check(&fixture, swarm::SwarmSubtaskListV1::validate),
            "swarm_subtask.response" => check(&fixture, swarm::SwarmSubtaskV1::validate),
            "swarm_subtask_propose.request" => {
                check(&fixture, swarm::SwarmSubtaskProposeV1::validate)
            }
            "swarm_claim.request" => check(&fixture, swarm::SwarmClaimRequestV1::validate),
            "swarm_claim_renew.request" => check(&fixture, swarm::SwarmClaimRenewV1::validate),
            "swarm_claim_release.request" => check(&fixture, swarm::SwarmClaimReleaseV1::validate),
            "swarm_claim.response" => check(&fixture, swarm::SwarmClaimV1::validate),
            "swarm_result.request" => check(&fixture, swarm::SwarmResultRequestV1::validate),
            "swarm_result_ack.response" => check::<swarm::SwarmResultAckV1>(&fixture, ok),
            "swarm_verification_list.response" => {
                check(&fixture, swarm::SwarmVerificationListV1::validate)
            }
            n if n.starts_with("verification_commit.") => {
                check(&fixture, swarm::VerificationCommitV1::validate)
            }
            "verification_reveal.request" => check(&fixture, swarm::VerificationRevealV1::validate),
            "verification_reveal_ack.response" => {
                check::<swarm::VerificationRevealAckV1>(&fixture, ok)
            }
            "moderation_report.request" => check(&fixture, swarm::ModerationReportV1::validate),
            "swarm_seed.request" => check(&fixture, swarm::SwarmSeedV1::validate),
            "agent_profile_list.response" => {
                check(&fixture, profiles::AgentProfileListV1::validate)
            }
            "handle.response" => check(&fixture, profiles::HandleV1::validate),
            "engine_optin_stop.request" => check(&fixture, optin::EngineOptinStopV1::validate),
            n if n.starts_with("engine_optin.") => check(&fixture, optin::EngineOptinV1::validate),
            "anchor_digest.vectors" => {}
            n if n.starts_with("proposal.") => check(&fixture, DraftProposalV1::validate),
            "proposal_ack.response" => check::<ProposalAckV1>(&fixture, ok),
            "proposal_list.response" => check::<ProposalListV1>(&fixture, ok),
            n if n.starts_with("knowledge_put.") => {
                check(&fixture, KnowledgePutRequestV1::validate)
            }
            n if n.starts_with("proposal_view.") => check(&fixture, ProposalV1::validate),
            n if n.starts_with("metrics.") => check(&fixture, MetricsUploadV1::validate),
            "metrics_ack.response" => check::<MetricsAckV1>(&fixture, ok),
            "receipts.request" => check(&fixture, ReceiptsUploadV1::validate),
            "receipts_ack.response" => check::<ReceiptsAckV1>(&fixture, ok),
            n if n.starts_with("policy_bundle.") => check(&fixture, PolicyBundleV1::validate),
            n if n.starts_with("bus.") => check(&fixture, |frame: &BusFrameV1| match frame {
                BusFrameV1::MessageDeliver(delivery) => delivery.validate(),
                BusFrameV1::Published { .. }
                | BusFrameV1::Deliver { .. }
                | BusFrameV1::PresenceUpdate { .. }
                | BusFrameV1::Control { .. }
                | BusFrameV1::MessageStored(_)
                | BusFrameV1::MessageStatus(_)
                | BusFrameV1::TaskEvent(_)
                | BusFrameV1::KnowledgeEvent(_)
                | BusFrameV1::Error { .. } => Ok(()),
                client => client.validate(),
            }),
            "bus_poll.response" => check::<BusPollV1>(&fixture, ok),
            "bus_events.response" => check(&fixture, BusEventsV1::validate),
            "bus_lease.request" => check(&fixture, BusLeaseRequestV1::validate),
            "bus_lease_renew.request" => check(&fixture, BusLeaseRenewRequestV1::validate),
            n if n.starts_with("bus_lease.") => check(&fixture, BusLeaseV1::validate),
            "sealed_key.response" => check(&fixture, SealedKeyV1::validate),
            "sealed_grant.request" => check(&fixture, SealedGrantRequestV1::validate),
            "sealed_pending.response" => check(&fixture, SealedPendingV1::validate),
            n if n.starts_with("status.") => check::<HiveCliStatusV1>(&fixture, ok),
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
            "message_list.response" => check(&fixture, messages::MessageListV1::validate),
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
        &HiveKeySetV1::signing_input_raw(keys_raw).unwrap(),
        &signature(keys_raw["signature"].as_str().unwrap()),
    )
    .expect("key set signed by root");
    let keys: HiveKeySetV1 = serde_json::from_value(keys_raw.clone()).unwrap();
    let envelope: HiveLeaseEnvelopeV1 =
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
fn sealed_fixtures_decrypt_with_the_test_repo_key_and_bind_their_aad() {
    let blob: SealedBlobV1 = serde_json::from_value(
        fixtures()
            .into_iter()
            .find(|fixture| fixture.name == "knowledge_revision.sealed.response")
            .unwrap()
            .value["sealed"]
            .clone(),
    )
    .unwrap();
    let cipher = XChaCha20Poly1305::new((&REPO_KEY).into());
    let nonce = B64.decode(&blob.nonce).unwrap();
    let ciphertext = B64.decode(&blob.ciphertext).unwrap();
    let open = |revision| {
        cipher.decrypt(
            XNonce::from_slice(&nonce),
            Payload {
                msg: &ciphertext,
                aad: &sealed_aad(VAULT, revision, blob.epoch, &blob.content_schema),
            },
        )
    };
    assert_eq!(open(3).unwrap(), canonical_json(&document()).unwrap());
    assert!(
        open(4).is_err(),
        "a sealed revision cannot be replayed as another"
    );
}

#[test]
fn swarm_digests_are_canonical_sha256_and_the_reveal_opens_the_commit() {
    let fixtures = fixtures();
    let get = |name: &str| {
        fixtures
            .iter()
            .find(|f| f.name == name)
            .unwrap()
            .value
            .clone()
    };
    let reveal: swarm::VerificationRevealV1 =
        serde_json::from_value(get("verification_reveal.request")).unwrap();
    let doc = json!({"nonce": REVEAL_NONCE, "value": 3, "verdict": "agree"});
    let expected = hex(&Sha256::digest(canonical_json(&doc).unwrap()));
    assert_eq!(get("verification_commit.request")["sha256"], expected);
    assert_eq!(reveal.commitment().unwrap(), expected);
    let doc = json!({
        "domain": "leanctx.hive.verification-commit/v2", "verification_id": "sv_01jbretry7b",
        "nonce": REVEAL_NONCE, "value": 3, "verdict": "agree",
    });
    let v2 = hex(&Sha256::digest(canonical_json(&doc).unwrap()));
    assert_eq!(get("verification_commit.v2.request")["sha256"], v2);
    assert_ne!(v2, expected, "v2 is domain-separated");
    assert_ne!(
        reveal.commitment_v2("sv_other").unwrap(),
        v2,
        "bound to its assignment"
    );
    let result = swarm_result();
    let doc =
        json!({"summary": result["summary"], "body": result["body"], "value": result["value"]});
    assert_eq!(
        get("swarm_result_ack.response")["sha256"],
        hex(&Sha256::digest(canonical_json(&doc).unwrap()))
    );
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
fn a_sealed_code_graph_shows_no_path_and_opens_to_the_clear_graph() {
    let value = fixtures()
        .into_iter()
        .find(|fixture| fixture.name == "code_graph_put.sealed.request")
        .unwrap()
        .value;
    assert!(!value.to_string().contains("outbox"), "a path in the clear");
    let blob: SealedBlobV1 = serde_json::from_value(value["sealed"].clone()).unwrap();
    let opened = XChaCha20Poly1305::new((&REPO_KEY).into())
        .decrypt(
            XNonce::from_slice(&B64.decode(&blob.nonce).unwrap()),
            Payload {
                msg: &B64.decode(&blob.ciphertext).unwrap(),
                aad: &sealed_aad(VAULT, 0, blob.epoch, codegraph::CODE_GRAPH_SCHEMA),
            },
        )
        .unwrap();
    assert_eq!(opened, canonical_json(&code_graph()).unwrap());
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
fn sealed_task_writes_carry_nothing_readable_and_bind_the_task_number() {
    for (name, number) in [
        ("task_result.sealed.request", 7),
        ("task_propose.sealed.request", 0),
    ] {
        let value = fixtures()
            .into_iter()
            .find(|fixture| fixture.name == name)
            .unwrap()
            .value;
        let text = value.to_string();
        for plaintext in ["retry", "Budget", "summary\"", "title\"", "evidence\""] {
            assert!(
                !text.contains(plaintext),
                "{name}: {plaintext} in the clear"
            );
        }
        let blob: SealedBlobV1 = serde_json::from_value(value["sealed"].clone()).unwrap();
        let cipher = XChaCha20Poly1305::new((&REPO_KEY).into());
        let open = |number| {
            cipher.decrypt(
                XNonce::from_slice(&B64.decode(&blob.nonce).unwrap()),
                Payload {
                    msg: &B64.decode(&blob.ciphertext).unwrap(),
                    aad: &sealed_aad(VAULT, number, blob.epoch, &blob.content_schema),
                },
            )
        };
        let opened: Value = serde_json::from_slice(&open(number).unwrap()).unwrap();
        assert_eq!(opened["schema"], blob.content_schema.as_str(), "{name}");
        assert!(
            open(number + 1).is_err(),
            "{name}: sealed task content cannot move to another task"
        );
    }
}

#[test]
fn a_sealed_draft_carries_nothing_readable_and_opens_to_its_content() {
    let draft = fixtures()
        .into_iter()
        .find(|fixture| fixture.name == "proposal.sealed.request")
        .unwrap()
        .value;
    let text = draft.to_string();
    for plaintext in ["Session learnings", "Two decisions", "summary\"", "body\""] {
        assert!(!text.contains(plaintext), "{plaintext} in the clear");
    }
    let blob: SealedBlobV1 =
        serde_json::from_value(draft["context_patch"]["sealed"].clone()).unwrap();
    let opened = XChaCha20Poly1305::new((&REPO_KEY).into())
        .decrypt(
            XNonce::from_slice(&B64.decode(&blob.nonce).unwrap()),
            Payload {
                msg: &B64.decode(&blob.ciphertext).unwrap(),
                aad: &sealed_aad(VAULT, 3, blob.epoch, &blob.content_schema),
            },
        )
        .unwrap();
    let content: SealedDraftContentV1 = serde_json::from_slice(&opened).unwrap();
    content.validate().unwrap();
    assert_eq!(content.summary, "Session learnings: draft rebasing");
    // In the clear, a sealed draft with any free text is refused.
    let mut leaky: DraftProposalV1 = serde_json::from_value(draft).unwrap();
    leaky.summary = Some("Sealed draft".into());
    assert!(leaky.validate().is_err());
}

#[test]
fn the_wrapped_repo_key_opens_for_its_recipient_repo_and_epoch_only() {
    use curve25519_dalek::montgomery::MontgomeryPoint;
    let wrapped: WrappedKeyV1 = serde_json::from_value(wrap_repo_key(INSTANCE, 1)).unwrap();
    wrapped.validate().unwrap();
    let (secret, recipient) = recipient_kx();
    let ephemeral: [u8; 32] = B64
        .decode(&wrapped.ephemeral_pub)
        .unwrap()
        .try_into()
        .unwrap();
    let shared = MontgomeryPoint(ephemeral).mul_clamped(secret).to_bytes();
    let cipher = XChaCha20Poly1305::new((&wrap_key(&shared, &ephemeral, &recipient)).into());
    let nonce = B64.decode(&wrapped.nonce).unwrap();
    let ciphertext = B64.decode(&wrapped.ciphertext).unwrap();
    let open = |aad: Vec<u8>| {
        cipher.decrypt(
            XNonce::from_slice(&nonce),
            Payload {
                msg: &ciphertext,
                aad: &aad,
            },
        )
    };
    assert_eq!(
        open(sealed_wrap_aad(VAULT, 1, INSTANCE)).unwrap(),
        REPO_KEY.to_vec()
    );
    assert!(
        open(sealed_wrap_aad(VAULT, 2, INSTANCE)).is_err(),
        "another epoch"
    );
    assert!(
        open(sealed_wrap_aad(VAULT, 1, "inst_other")).is_err(),
        "another instance"
    );
}

#[test]
fn the_pending_recipients_are_signed_by_an_hive_key() {
    let pending = sealed_pending();
    let parsed: SealedPendingV1 = serde_json::from_value(pending.clone()).unwrap();
    let input = SealedPendingV1::signing_input_raw(&pending).unwrap();
    let signature =
        Signature::from_bytes(&B64.decode(&parsed.signature).unwrap().try_into().unwrap());
    assert!(
        key(SIGNING_SEED)
            .verifying_key()
            .verify(&input, &signature)
            .is_ok()
    );
    let mut swapped = pending;
    swapped["recipients"][0]["instance_kx_public_key"] = Value::from(B64.encode([0x59u8; 32]));
    assert_ne!(SealedPendingV1::signing_input_raw(&swapped).unwrap(), input);
    assert_eq!(
        parsed.recipients[0].kx_fingerprint,
        kx_fingerprint(&KX_PUBLIC)
    );
}

#[test]
fn org_policy_fixture_verifies_under_the_policy_signer_key() {
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    let (artifact, vector) = org_policy();
    let statement = serde_json::to_vec(&artifact["statement"]).unwrap();
    let mut input = ORG_POLICY_SIGNATURE_DOMAIN.as_bytes().to_vec();
    input.extend_from_slice(vector["statement_json"].as_str().unwrap().as_bytes());
    assert_eq!(input, vector["signing_input"].as_str().unwrap().as_bytes());
    let public: [u8; 32] = URL_SAFE_NO_PAD
        .decode(artifact["public_key"].as_str().unwrap())
        .unwrap()
        .try_into()
        .unwrap();
    let signature: [u8; 64] = URL_SAFE_NO_PAD
        .decode(artifact["signature"].as_str().unwrap())
        .unwrap()
        .try_into()
        .unwrap();
    VerifyingKey::from_bytes(&public)
        .unwrap()
        .verify(&input, &Signature::from_bytes(&signature))
        .expect("org policy statement signed by the policy signer key");
    let pack = artifact["pack_toml"].as_str().unwrap();
    assert_eq!(
        artifact["statement"]["pack_digest"],
        format!("sha256:{}", hex(&Sha256::digest(pack.as_bytes())))
    );
    // Declaration order is the signed form; a key-sorted re-encoding differs.
    assert_ne!(
        canonical_json(&artifact["statement"]).unwrap(),
        input[ORG_POLICY_SIGNATURE_DOMAIN.len()..]
    );
    assert_eq!(
        statement.len(),
        input.len() - ORG_POLICY_SIGNATURE_DOMAIN.len()
    );
}

#[test]
fn a_policy_requiring_packs_signs_them_and_binds_the_bundle() {
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    let (artifact, vector) = org_policy_requiring_packs();
    // Without required packs the statement keeps its 1.7 bytes.
    assert!(
        !org_policy().1["statement_json"]
            .as_str()
            .unwrap()
            .contains("required_filter_packs")
    );
    let statement = vector["statement_json"].as_str().unwrap();
    assert!(statement.ends_with(&format!(
        ",\"required_filter_packs\":[{{\"pack_id\":\"ch-identifiers\",\"version\":3,\"sha256\":\"{}\"}}]}}",
        filter_pack()["sha256"].as_str().unwrap()
    )));
    let public: [u8; 32] = URL_SAFE_NO_PAD
        .decode(artifact["public_key"].as_str().unwrap())
        .unwrap()
        .try_into()
        .unwrap();
    let signature: [u8; 64] = URL_SAFE_NO_PAD
        .decode(artifact["signature"].as_str().unwrap())
        .unwrap()
        .try_into()
        .unwrap();
    VerifyingKey::from_bytes(&public)
        .unwrap()
        .verify(
            vector["signing_input"].as_str().unwrap().as_bytes(),
            &Signature::from_bytes(&signature),
        )
        .expect("the required packs are covered by the policy signature");

    let required: Vec<RequiredFilterPackV1> =
        serde_json::from_value(artifact["statement"]["required_filter_packs"].clone()).unwrap();
    validate_required_filter_packs(&required).unwrap();
    let bundle: PolicyBundleV1 = serde_json::from_value(
        fixtures()
            .into_iter()
            .find(|f| f.name == "policy_bundle.required_packs.response")
            .unwrap()
            .value,
    )
    .unwrap();
    assert!(unmet_required_packs(&required, &bundle.filter_packs).is_empty());
    // Dropped, older, or another statement under the same version: unmet.
    assert_eq!(
        unmet_required_packs(&required, &[]),
        ["required filter pack ch-identifiers is missing"]
    );
    let mut older = bundle.filter_packs.clone();
    older[0].version = 2;
    assert_eq!(
        unmet_required_packs(&required, &older),
        ["required filter pack ch-identifiers is version 2, the policy requires 3"]
    );
    let mut other = bundle.filter_packs;
    other[0].sha256 = "0".repeat(64);
    assert_eq!(
        unmet_required_packs(&required, &other),
        ["required filter pack ch-identifiers version 3 is not the one the policy signed"]
    );
    // One encoding per set: ascending ids, no repeats, real digests.
    let mut twice = required.clone();
    twice.extend(required.clone());
    assert!(validate_required_filter_packs(&twice).is_err());
    let mut upper = required;
    upper[0].sha256 = upper[0].sha256.to_uppercase();
    assert!(validate_required_filter_packs(&upper).is_err());
}

#[test]
fn lease_fixture_is_valid_for_a_day_then_in_grace() {
    let envelope: HiveLeaseEnvelopeV1 = serde_json::from_value(lease_envelope()).unwrap();
    let lease = envelope.lease().unwrap();
    let at = |value| chrono::DateTime::parse_from_rfc3339(value).unwrap();
    assert_eq!(lease.state_at(at(T0)).unwrap(), LeaseState::Valid);
    assert_eq!(
        lease.state_at(at("2026-10-22T00:00:00Z")).unwrap(),
        LeaseState::Grace
    );
    assert_eq!(
        lease.state_at(at("2026-10-27T12:00:00Z")).unwrap(),
        LeaseState::Lapsed
    );
    assert!(lease.grants(HiveRight::BusRelay));
}

#[test]
fn org_settings_narrow_uploads_and_their_absence_allows_both() {
    let envelope: HiveLeaseEnvelopeV1 = serde_json::from_value(lease_envelope()).unwrap();
    let lease = envelope.lease().unwrap();
    assert!(lease.org_allows_metrics_upload());
    assert!(!lease.org_allows_receipts_upload());
    // A 1.3 lease without the object, as an older service signs it.
    let mut older = envelope.lease.clone();
    older.as_object_mut().unwrap().remove("org_settings");
    let older: lean_ctx_protocol::hive::lease::HiveLeaseV1 = serde_json::from_value(older).unwrap();
    assert!(older.org_allows_metrics_upload() && older.org_allows_receipts_upload());
}

/// Hive task #19: `tasks` may be absent (no limit), but a named count is
/// 1-100; a budget nobody set still starts at 5.
#[test]
fn a_daily_task_limit_is_absent_or_one_to_a_hundred() {
    let with = |tasks: serde_json::Value| {
        let mut v = optin_fixtures()
            .into_iter()
            .find(|f| f.name == "engine_optin.unlimited.response")
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
