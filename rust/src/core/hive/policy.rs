// SPDX-License-Identifier: Apache-2.0
//! Org policy and filter packs from Hive.
//!
//! `GET /v1/policy/bundle` (conditional) → verified here before anything is
//! kept: the org policy statement must be signed by a key the *lease* pins
//! for this organization (no trust on first use), name that key by its
//! derived id, carry the digest of its own pack, be valid now (±15 min
//! skew, grace counted) and move the policy version forward — the same
//! version with other bytes is refused. Filter packs must be signed by an
//! Hive key of the verified key set, match their digest and validate
//! completely (every pattern compiles); all packs together stay within
//! [`HIVE_MAX_RULES`]. The packs the signed policy lists in
//! `required_filter_packs` must all be there with exactly that version and
//! statement digest: Hive signs packs with its own key, so only this list
//! keeps a compromised service from dropping or swapping an org's packs. A
//! bundle that fails any check never replaces the last good one; the
//! refusal is shown in `hive status` and `lean-ctx doctor`, and local work
//! goes on under the last verified bundle.
//!
//! Stored in `<data_dir>/hive/policy/`: the verified bundle, its state,
//! and `filters/set.json`, the one file the policy snapshot reads to apply
//! the patterns ([`filter_rules`]).

use std::path::Path;

use base64::Engine as _;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use chrono::{DateTime, Duration, Utc};
use ed25519_dalek::{Signature, VerifyingKey};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use super::HiveError;
use super::auth::Session;
use super::lease::verify_keyset;
use super::state::write_private;
use lean_ctx_protocol::hive::lease::{HiveKeySetV1, HiveLeaseV1};
use lean_ctx_protocol::hive::policy::{
    FilterActionV1, FilterPackRefV1, FilterPackV1, MAX_POLICY_BUNDLE_BYTES, PolicyBundleV1,
    RequiredFilterPackV1, filter_pack_signing_input, unmet_required_packs,
    validate_required_filter_packs,
};
use lean_ctx_protocol::{
    ContentDigest, Ed25519PublicKey, Ed25519Signature, KeyId, TeamId, TeamTimestampV1,
};

pub const ORG_POLICY_SCHEMA: &str = "leanctx.org-policy/v2";
pub const ORG_POLICY_SIGNATURE_DOMAIN: &str = "leanctx.org-policy.v2\n";
pub const ORG_POLICY_UNPIN_SCHEMA: &str = "leanctx.org-policy-unpin/v2";
pub const ORG_POLICY_UNPIN_SIGNATURE_DOMAIN: &str = "leanctx.org-policy-unpin.v2\n";
pub const FILTER_SET_SCHEMA: &str = "leanctx.hive.filter-set/v1";
/// The largest pack an org policy may carry.
pub const MAX_POLICY_PACK_BYTES: usize = 1024 * 1024;
/// Hive filter rules have their own budget, apart from the user's policy
/// rules; a bundle above it is refused rather than applied in part.
pub const HIVE_MAX_RULES: usize = 128;
const CLOCK_SKEW_MINUTES: i64 = 15;
const STATE_FILE: &str = "state.json";
const BUNDLE_FILE: &str = "bundle.json";
const FILTER_SET_FILE: &str = "set.json";

/// `OrgPolicyStatementV2`, field for field and in declaration order: the
/// signature covers `serde_json::to_vec` of exactly this shape.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OrgPolicyStatementV2 {
    pub schema: String,
    pub organization_id: TeamId,
    pub key_id: KeyId,
    pub policy_version: u64,
    pub issued_at: TeamTimestampV1,
    pub not_before: TeamTimestampV1,
    pub not_after: TeamTimestampV1,
    pub grace_seconds: u64,
    pub pack_digest: ContentDigest,
    /// C 1.8: the filter packs this policy puts in force; left out when
    /// empty, so a statement without it keeps its 1.7 bytes.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub required_filter_packs: Vec<RequiredFilterPackV1>,
}

/// `OrgPolicyUnpinStatementV2`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OrgPolicyUnpinStatementV2 {
    pub schema: String,
    pub organization_id: TeamId,
    pub key_id: KeyId,
    pub policy_version: u64,
    pub issued_at: TeamTimestampV1,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OrgPolicyUnpinEnvelopeV2 {
    pub statement: OrgPolicyUnpinStatementV2,
    pub public_key: Ed25519PublicKey,
    pub signature: Ed25519Signature,
}

/// `OrgPolicyArtifactV2`: what the engine's org-policy store installs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OrgPolicyArtifactV2 {
    pub statement: OrgPolicyStatementV2,
    pub public_key: Ed25519PublicKey,
    pub signature: Ed25519Signature,
    pub pack_toml: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unpin: Option<OrgPolicyUnpinEnvelopeV2>,
}

/// What the last verified bundle established.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PolicyState {
    pub etag: Option<String>,
    /// Highest accepted org policy (or unpin) version.
    pub policy_version: u64,
    /// `sha256:<hex>` of the accepted statement's signing bytes.
    pub statement_digest: Option<String>,
    /// `required` (a policy is in force) or `unpinned`.
    pub org_state: Option<String>,
    pub not_after: Option<String>,
    /// `pack_id → version` of the accepted filter packs.
    #[serde(default)]
    pub filter_packs: std::collections::BTreeMap<String, u64>,
    pub verified_at: Option<String>,
    /// Why the newest bundle was refused (the last good one stays).
    pub last_refusal: Option<String>,
    /// Whether the verified org policy is installed for enforcement. Needs
    /// the Intelligence runtime's org-policy verifier; without it the policy
    /// is received and verified, not enforced.
    #[serde(default)]
    pub enforced: bool,
    /// Why installing it for enforcement failed, when it was attempted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enforcement_error: Option<String>,
    /// The filter packs the accepted org policy requires (C 1.8).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub required_filter_packs: Vec<RequiredFilterPackV1>,
    /// The effective policy in force (2.x, [`super::effective_policy`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effective: Option<super::effective_policy::EffectiveState>,
    /// Why the newest effective policy was refused (the last valid stays).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effective_refusal: Option<String>,
    /// That refusal as reported to Hive (SERVER-B4), once per received
    /// version and digest.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effective_refused: Option<super::effective_policy::RefusalRecord>,
}

pub fn load_state(data_dir: &Path) -> PolicyState {
    std::fs::read(data_dir.join("policy").join(STATE_FILE))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

fn save_state(data_dir: &Path, state: &PolicyState) -> Result<(), HiveError> {
    let bytes = serde_json::to_vec_pretty(state).map_err(|e| HiveError::Local(e.to_string()))?;
    write_private(&data_dir.join("policy"), STATE_FILE, &bytes)
}

/// The installed filter set of `data_dir`, as text (what the policy
/// snapshot reads), if there is one.
pub fn load_filter_set(data_dir: &Path) -> Option<String> {
    std::fs::read_to_string(
        data_dir
            .join("policy")
            .join("filters")
            .join(FILTER_SET_FILE),
    )
    .ok()
}

/// The last verified bundle, as received.
pub fn load_bundle(data_dir: &Path) -> Option<Value> {
    std::fs::read(data_dir.join("policy").join(BUNDLE_FILE))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
}

/// The verified filter packs in force, as one file the policy snapshot reads.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FilterSet {
    pub schema: String,
    pub packs: Vec<FilterPackV1>,
    /// `pack_id → statement_b64`: the exact signed bytes of each pack, so
    /// [`required_packs_installed`] can recompute the digest the org policy
    /// signed and check that the applied pack is the one those bytes hold.
    /// Absent in sets written before C 1.8.
    #[serde(default)]
    pub statements: std::collections::BTreeMap<String, String>,
}

/// `<data_dir>/hive/policy/filters/set.json` of this installation.
pub fn filter_set_path() -> Option<std::path::PathBuf> {
    let data = crate::core::paths::data_dir().ok()?;
    Some(
        data.join("hive")
            .join("policy")
            .join("filters")
            .join(FILTER_SET_FILE),
    )
}

/// Hive filter patterns as policy rules.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FilterRules {
    /// `(rule label, pattern)`: replace matches with `[REDACTED:<label>]`.
    pub redaction: Vec<(String, String)>,
    /// `(rule label, pattern)`: refuse content that matches.
    pub blocked: Vec<(String, String)>,
    /// Defensive counter: patterns of a set that did not validate here or
    /// whose label repeats. Installation refuses such sets, so this is 0
    /// unless the file was changed by hand; report it as a diagnostic.
    pub dropped: usize,
}

/// Rules from the captured bytes of the filter set. Labels are unique
/// (`hive:<label or id>`, the first pack in id order wins) and every
/// pattern compiled at installation within the policy limits.
pub fn filter_rules(set: Option<&str>) -> FilterRules {
    let mut rules = FilterRules::default();
    let Some(text) = set else {
        return rules;
    };
    let Ok(set) = serde_json::from_str::<FilterSet>(text) else {
        rules.dropped = 1;
        return rules;
    };
    let mut seen = std::collections::BTreeSet::new();
    for pack in &set.packs {
        if pack.validate().is_err() {
            rules.dropped += pack.patterns.len().max(1);
            continue;
        }
        for pattern in &pack.patterns {
            let label = pattern.rule_label();
            if !seen.insert(label.clone()) || seen.len() > HIVE_MAX_RULES {
                rules.dropped += 1;
                continue;
            }
            let rule = (label, pattern.pattern.clone());
            match pattern.action {
                FilterActionV1::Redact => rules.redaction.push(rule),
                FilterActionV1::Block => rules.blocked.push(rule),
            }
        }
    }
    rules
}

/// The filter rules of this installation, read now (for screening, which
/// has no policy snapshot of its own).
pub fn installed_filter_rules() -> FilterRules {
    let text = filter_set_path().and_then(|path| std::fs::read_to_string(path).ok());
    filter_rules(text.as_deref())
}

fn digest_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn verify(public: &[u8; 32], message: &[u8], signature: &[u8; 64]) -> bool {
    VerifyingKey::from_bytes(public).is_ok_and(|key| {
        key.verify_strict(message, &Signature::from_bytes(signature))
            .is_ok()
    })
}

fn url_key(encoded: &Ed25519PublicKey) -> Result<[u8; 32], String> {
    URL_SAFE_NO_PAD
        .decode(encoded.as_str())
        .ok()
        .and_then(|b| b.try_into().ok())
        .ok_or_else(|| "public key is malformed".to_string())
}

fn url_signature(encoded: &Ed25519Signature) -> Result<[u8; 64], String> {
    URL_SAFE_NO_PAD
        .decode(encoded.as_str())
        .ok()
        .and_then(|b| b.try_into().ok())
        .ok_or_else(|| "signature is malformed".to_string())
}

/// `key:ed25519-` + the first 32 hex characters of sha256(public key).
pub fn derived_key_id(public: &[u8; 32]) -> String {
    format!("key:ed25519-{}", &digest_hex(public)[..32])
}

/// The signer keys the lease pins for `org`, as raw bytes.
fn pinned_signers(lease: &HiveLeaseV1, org: &str) -> Vec<[u8; 32]> {
    lease
        .policy_signer_keys
        .iter()
        .filter(|k| k.org == org)
        .filter_map(|k| STANDARD.decode(&k.ed25519_pub).ok()?.try_into().ok())
        .collect()
}

fn signer_checks(
    lease: &HiveLeaseV1,
    org: &TeamId,
    key_id: &KeyId,
    public_key: &Ed25519PublicKey,
) -> Result<[u8; 32], String> {
    if org.as_str() != lease.org_id {
        return Err(format!(
            "signed for workspace {}, this instance is in {}",
            org.as_str(),
            lease.org_id
        ));
    }
    let public = url_key(public_key)?;
    if !pinned_signers(lease, org.as_str()).contains(&public) {
        return Err("signed by a key the lease does not pin for this workspace".into());
    }
    if key_id.as_str() != derived_key_id(&public) {
        return Err("key_id does not name the signing key".into());
    }
    Ok(public)
}

fn time(t: &TeamTimestampV1) -> Result<DateTime<Utc>, String> {
    DateTime::parse_from_rfc3339(t.as_str())
        .map(|d| d.with_timezone(&Utc))
        .map_err(|_| "timestamp is malformed".to_string())
}

/// When the grace after `until` ends; `None` when it ends beyond the
/// representable time (a `grace_seconds` past chrono's range is never
/// computed, which would panic: it simply never runs out).
fn grace_end(until: DateTime<Utc>, grace_seconds: u64) -> Option<DateTime<Utc>> {
    let grace = Duration::try_seconds(i64::try_from(grace_seconds).ok()?)?;
    until.checked_add_signed(grace)
}

/// Check an org policy artifact; returns its statement digest.
pub fn verify_org_policy(
    artifact: &OrgPolicyArtifactV2,
    lease: &HiveLeaseV1,
    now: DateTime<Utc>,
) -> Result<String, String> {
    let statement = &artifact.statement;
    if statement.schema != ORG_POLICY_SCHEMA {
        return Err("not an org policy v2 statement".into());
    }
    if artifact.unpin.is_some() {
        return Err("a policy artifact with an embedded unpin is not a policy in force".into());
    }
    let public = signer_checks(
        lease,
        &statement.organization_id,
        &statement.key_id,
        &artifact.public_key,
    )?;
    let mut signing = ORG_POLICY_SIGNATURE_DOMAIN.as_bytes().to_vec();
    signing.extend(serde_json::to_vec(statement).map_err(|e| e.to_string())?);
    if !verify(&public, &signing, &url_signature(&artifact.signature)?) {
        return Err("the org policy signature does not verify".into());
    }
    if artifact.pack_toml.is_empty() || artifact.pack_toml.len() > MAX_POLICY_PACK_BYTES {
        return Err("the policy pack is empty or too large".into());
    }
    if statement.pack_digest.as_str()
        != format!("sha256:{}", digest_hex(artifact.pack_toml.as_bytes()))
    {
        return Err("the policy pack is not the one the statement names".into());
    }
    validate_required_filter_packs(&statement.required_filter_packs)
        .map_err(|e| format!("org policy: {e}"))?;
    let skew = Duration::minutes(CLOCK_SKEW_MINUTES);
    let (from, until) = (time(&statement.not_before)?, time(&statement.not_after)?);
    if from >= until || time(&statement.issued_at)? > until {
        return Err("the policy validity window is empty".into());
    }
    if now + skew < from {
        return Err("the policy is not valid yet".into());
    }
    if grace_end(until, statement.grace_seconds).is_some_and(|end| now - skew > end) {
        return Err("the policy has expired".into());
    }
    Ok(format!("sha256:{}", digest_hex(&signing)))
}

/// Check an unpin envelope; returns its statement digest.
pub fn verify_unpin(
    envelope: &OrgPolicyUnpinEnvelopeV2,
    lease: &HiveLeaseV1,
) -> Result<String, String> {
    let statement = &envelope.statement;
    if statement.schema != ORG_POLICY_UNPIN_SCHEMA {
        return Err("not an org policy unpin statement".into());
    }
    let public = signer_checks(
        lease,
        &statement.organization_id,
        &statement.key_id,
        &envelope.public_key,
    )?;
    let mut signing = ORG_POLICY_UNPIN_SIGNATURE_DOMAIN.as_bytes().to_vec();
    signing.extend(serde_json::to_vec(statement).map_err(|e| e.to_string())?);
    if !verify(&public, &signing, &url_signature(&envelope.signature)?) {
        return Err("the unpin signature does not verify".into());
    }
    Ok(format!("sha256:{}", digest_hex(&signing)))
}

/// Check one filter pack against the verified Hive key set; returns the pack.
fn verify_filter_pack(
    pack: &FilterPackRefV1,
    keyset: &HiveKeySetV1,
) -> Result<FilterPackV1, String> {
    let id = &pack.pack_id;
    let statement = STANDARD
        .decode(&pack.envelope.statement_b64)
        .map_err(|_| format!("filter pack {id}: statement is not base64"))?;
    if digest_hex(&statement) != pack.sha256 {
        return Err(format!("filter pack {id}: digest mismatch"));
    }
    let key = keyset
        .key(&pack.envelope.kid)
        .ok_or_else(|| format!("filter pack {id}: unknown signing key"))?;
    let public: [u8; 32] = STANDARD
        .decode(&key.ed25519_pub)
        .ok()
        .and_then(|b| b.try_into().ok())
        .ok_or("key set key is malformed")?;
    let signature: [u8; 64] = STANDARD
        .decode(&pack.envelope.signature)
        .ok()
        .and_then(|b| b.try_into().ok())
        .ok_or_else(|| format!("filter pack {id}: signature is malformed"))?;
    if !verify(&public, &filter_pack_signing_input(&statement), &signature) {
        return Err(format!("filter pack {id}: signature does not verify"));
    }
    let parsed: FilterPackV1 = serde_json::from_slice(&statement)
        .map_err(|_| format!("filter pack {id}: statement is not a filter pack"))?;
    parsed
        .validate()
        .map_err(|e| format!("filter pack {id}: {e}"))?;
    if parsed.pack_id != *id || parsed.version != pack.version {
        return Err(format!(
            "filter pack {id}: statement does not match its listing"
        ));
    }
    Ok(parsed)
}

/// Verify a whole bundle against the lease and key set, and against the
/// last accepted state (forward-only). Returns the state it establishes and
/// the verified filter packs.
pub fn verify_bundle(
    raw: &Value,
    lease: &HiveLeaseV1,
    keyset: &HiveKeySetV1,
    previous: &PolicyState,
    now: DateTime<Utc>,
) -> Result<(PolicyState, FilterSet), String> {
    let bundle: PolicyBundleV1 =
        serde_json::from_value(raw.clone()).map_err(|e| format!("bundle: {e}"))?;
    bundle.validate().map_err(|e| format!("bundle: {e}"))?;
    let mut state = PolicyState {
        etag: None,
        policy_version: previous.policy_version,
        statement_digest: previous.statement_digest.clone(),
        org_state: previous.org_state.clone(),
        not_after: previous.not_after.clone(),
        filter_packs: std::collections::BTreeMap::new(),
        verified_at: Some(now.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)),
        last_refusal: None,
        enforced: false,
        enforcement_error: None,
        required_filter_packs: previous.required_filter_packs.clone(),
        effective: previous.effective.clone(),
        effective_refusal: previous.effective_refusal.clone(),
        effective_refused: previous.effective_refused.clone(),
    };
    let mut required = Vec::new();
    let accepted = match (&bundle.org_policy, &bundle.unpin) {
        (Some(policy), _) => {
            let artifact: OrgPolicyArtifactV2 =
                serde_json::from_value(policy.clone()).map_err(|e| format!("org policy: {e}"))?;
            let digest = verify_org_policy(&artifact, lease, now)?;
            required.clone_from(&artifact.statement.required_filter_packs);
            Some((
                artifact.statement.policy_version,
                digest,
                "required",
                Some(artifact.statement.not_after.as_str().to_string()),
            ))
        }
        (None, Some(unpin)) => {
            let envelope: OrgPolicyUnpinEnvelopeV2 =
                serde_json::from_value(unpin.clone()).map_err(|e| format!("unpin: {e}"))?;
            let digest = verify_unpin(&envelope, lease)?;
            Some((envelope.statement.policy_version, digest, "unpinned", None))
        }
        (None, None) => None,
    };
    if let Some((version, digest, org_state, not_after)) = accepted {
        if version != bundle.policy_version {
            return Err("the bundle's policy_version does not match its statement".into());
        }
        match version.cmp(&previous.policy_version) {
            std::cmp::Ordering::Less => {
                return Err(format!(
                    "policy version {version} is older than the accepted {}",
                    previous.policy_version
                ));
            }
            std::cmp::Ordering::Equal
                if previous.statement_digest.as_deref() != Some(digest.as_str()) =>
            {
                return Err(format!(
                    "policy version {version} was accepted with other content"
                ));
            }
            _ => {}
        }
        state.policy_version = version;
        state.statement_digest = Some(digest);
        state.org_state = Some(org_state.into());
        state.not_after = not_after;
        // What the accepted statement requires (nothing for an unpin).
        state.required_filter_packs.clone_from(&required);
    } else if previous.policy_version > 0 {
        return Err("the bundle drops the org policy without a signed unpin".into());
    }
    // Fail closed: a policy whose packs are not all there is not the policy
    // the organization signed (HIVE-RT-D5).
    let unmet = unmet_required_packs(&required, &bundle.filter_packs);
    if !unmet.is_empty() {
        return Err(unmet.join("; "));
    }
    let mut packs = Vec::new();
    let mut statements = std::collections::BTreeMap::new();
    let mut labels = std::collections::BTreeSet::new();
    for pack in &bundle.filter_packs {
        let verified = verify_filter_pack(pack, keyset)?;
        statements.insert(pack.pack_id.clone(), pack.envelope.statement_b64.clone());
        if previous
            .filter_packs
            .get(&verified.pack_id)
            .is_some_and(|accepted| verified.version < *accepted)
        {
            return Err(format!(
                "filter pack {} version {} is older than the accepted one",
                verified.pack_id, verified.version
            ));
        }
        for pattern in &verified.patterns {
            if !labels.insert(pattern.rule_label()) {
                return Err(format!(
                    "filter pack {}: rule {} repeats across packs",
                    verified.pack_id,
                    pattern.rule_label()
                ));
            }
        }
        state
            .filter_packs
            .insert(verified.pack_id.clone(), verified.version);
        packs.push(verified);
    }
    if labels.len() > HIVE_MAX_RULES {
        return Err(format!(
            "the filter packs hold {} patterns; at most {HIVE_MAX_RULES} are applied as a whole",
            labels.len()
        ));
    }
    packs.sort_by(|a, b| a.pack_id.cmp(&b.pack_id));
    Ok((
        state,
        FilterSet {
            schema: FILTER_SET_SCHEMA.into(),
            packs,
            statements,
        },
    ))
}

/// The policy runtime's side of [`verify_bundle`]'s required-pack check,
/// on what is installed: for every filter pack the verified org policy
/// `artifact` requires, the installed `set` must hold statement bytes whose
/// SHA-256 is the signed digest, and the pack it applies must be exactly
/// the one those bytes decode to. A set whose patterns, bytes or listing
/// were changed or removed by hand is refused like any other tampering.
pub fn required_packs_installed(artifact: &str, set: Option<&str>) -> Result<(), String> {
    let artifact: OrgPolicyArtifactV2 =
        serde_json::from_str(artifact).map_err(|_| "org policy input is invalid")?;
    let required = &artifact.statement.required_filter_packs;
    if required.is_empty() {
        return Ok(());
    }
    let set: FilterSet = set
        .and_then(|text| serde_json::from_str(text).ok())
        .ok_or("the org policy requires filter packs and none are installed")?;
    if !applies_in_full(&set) {
        return Err("the installed filter set is not as verified".into());
    }
    if let Some(r) = required.iter().find(|r| !installed_as_signed(&set, r)) {
        return Err(format!(
            "required filter pack {} version {} is not installed as signed",
            r.pack_id, r.version
        ));
    }
    Ok(())
}

/// Per required pack, whether the installed set holds it as signed — the
/// rule of [`required_packs_installed`], one answer per pack (for `hive
/// status`). A set that is missing, unreadable or does not apply in full
/// installs none of them.
pub fn required_packs_status(required: &[RequiredFilterPackV1], set: Option<&str>) -> Vec<bool> {
    let set = set
        .and_then(|text| serde_json::from_str::<FilterSet>(text).ok())
        .filter(applies_in_full);
    required
        .iter()
        .map(|r| set.as_ref().is_some_and(|s| installed_as_signed(s, r)))
        .collect()
}

/// The set applies in full, as verify_bundle wrote it: filter_rules drops
/// repeated labels and rules beyond the budget first come, first served, so
/// a shadow pack placed before a required one could otherwise silence its
/// rules while the required pack is still "there".
fn applies_in_full(set: &FilterSet) -> bool {
    let mut ids = std::collections::BTreeSet::new();
    let mut labels = std::collections::BTreeSet::new();
    for pack in &set.packs {
        if !ids.insert(pack.pack_id.as_str()) || pack.validate().is_err() {
            return false;
        }
        for pattern in &pack.patterns {
            if !labels.insert(pattern.rule_label()) {
                return false;
            }
        }
    }
    labels.len() <= HIVE_MAX_RULES
}

/// The set holds statement bytes whose SHA-256 is the signed digest, and
/// applies exactly the pack those bytes decode to.
fn installed_as_signed(set: &FilterSet, r: &RequiredFilterPackV1) -> bool {
    set.statements
        .get(&r.pack_id)
        .and_then(|b64| STANDARD.decode(b64).ok())
        .filter(|bytes| digest_hex(bytes) == r.sha256)
        .and_then(|bytes| serde_json::from_slice::<FilterPackV1>(&bytes).ok())
        .is_some_and(|pack| {
            pack.pack_id == r.pack_id && pack.version == r.version && set.packs.contains(&pack)
        })
}

/// Put the verified org policy of `bundle` in force, once the runtime's
/// org-policy verifier is available: pin the lease-pinned signer for the
/// organization, install the artifact, and record the first required
/// marker if none exists (a stored marker only ever advances through a
/// verification, never here). Without the verifier nothing is written, so
/// local work is never gated on a runtime that is not there.
pub fn enforce(
    bundle: &Value,
    state: &mut PolicyState,
    verifier_available: bool,
) -> Result<(), String> {
    state.enforced = false;
    state.enforcement_error = None;
    if !verifier_available {
        return Ok(());
    }
    let bundle: PolicyBundleV1 =
        serde_json::from_value(bundle.clone()).map_err(|e| format!("bundle: {e}"))?;
    let installed = crate::core::policy::org::store::installed_path()?;
    match (
        state.org_state.as_deref(),
        &bundle.org_policy,
        &bundle.unpin,
    ) {
        (Some("required"), Some(policy), _) => {
            let artifact: OrgPolicyArtifactV2 =
                serde_json::from_value(policy.clone()).map_err(|e| format!("org policy: {e}"))?;
            let public = hex::encode(url_key(&artifact.public_key)?);
            let org = &artifact.statement.organization_id;
            if let crate::core::policy::org::trust::PinOutcome::PinnedForAnother(other) =
                crate::core::policy::org::trust::pin_for(org.as_str(), &public)?
            {
                return Err(format!(
                    "the policy signer is already trusted for workspace {other}"
                ));
            }
            install_artifact(&installed, &artifact)?;
            enroll_first_marker(org, state)?;
        }
        (Some("unpinned"), _, Some(unpin)) => {
            // The lifted requirement travels inside the installed artifact;
            // with nothing installed there is nothing to lift.
            let envelope: OrgPolicyUnpinEnvelopeV2 =
                serde_json::from_value(unpin.clone()).map_err(|e| format!("unpin: {e}"))?;
            if let Some(mut artifact) = std::fs::read(&installed)
                .ok()
                .and_then(|b| serde_json::from_slice::<OrgPolicyArtifactV2>(&b).ok())
            {
                artifact.unpin = Some(envelope);
                install_artifact(&installed, &artifact)?;
            }
        }
        _ => return Ok(()),
    }
    state.enforced = true;
    Ok(())
}

fn install_artifact(path: &Path, artifact: &OrgPolicyArtifactV2) -> Result<(), String> {
    let bytes = serde_json::to_vec_pretty(artifact).map_err(|e| e.to_string())?;
    crate::core::atomic_fs::try_atomic_write(path, &bytes, None)
        .map_err(|e| format!("install the org policy: {e}"))
}

/// The endpoint's first required marker, authorized by the lease that pins
/// the organization's signer: the separately authorized first enrollment.
fn enroll_first_marker(organization: &TeamId, state: &PolicyState) -> Result<(), String> {
    use crate::core::policy::org::marker::{Predecessor, advance_within, marker_path_read_only};
    use lean_ctx_protocol::org_policy_verifier::{
        OrgRequiredMarkerEntryV1, OrgRequiredMarkerStateV1,
    };
    use lean_ctx_protocol::team_context::RecordDigest;
    if marker_path_read_only()?.exists() {
        return Ok(());
    }
    let digest = state
        .statement_digest
        .clone()
        .ok_or("no verified statement to enroll")?;
    let entry = OrgRequiredMarkerEntryV1 {
        sequence: state.policy_version,
        recorded_at: TeamTimestampV1::new(
            Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Nanos, true),
        )
        .map_err(|e| e.to_string())?,
        state: OrgRequiredMarkerStateV1::Required {
            policy_digest: RecordDigest::new(digest).map_err(|e| e.to_string())?,
        },
    };
    advance_within(
        organization,
        entry,
        Predecessor::NeverStored,
        std::time::Instant::now() + std::time::Duration::from_secs(2),
    )
    .map(|_| ())
}

/// Whether the runtime would verify this organization's policy here now:
/// a package that declares the verifier, and something that admits the
/// call — this lease (Team or Enterprise with the `runtime` right) or a
/// configured commercial licence. Otherwise a recorded requirement could
/// not be verified and would gate local work.
fn verifier_admitted(lease: &HiveLeaseV1) -> bool {
    use lean_ctx_protocol::hive::lease::{HivePlan, HiveRight};
    let lease_admits = lease.grants(HiveRight::Runtime)
        && matches!(lease.plan, HivePlan::Team | HivePlan::Enterprise);
    let licensed = !crate::core::config::Config::load_global()
        .intelligence_runtime
        .license_configuration
        .is_empty();
    (lease_admits || licensed)
        && crate::core::intelligence_runtime::org_policy_verifier::org_policy_verifier_available()
}

/// Install `bundle` for enforcement, recording a failure instead of failing.
fn try_enforce(bundle: &Value, state: &mut PolicyState, lease: &HiveLeaseV1) {
    if let Err(error) = enforce(bundle, state, verifier_admitted(lease)) {
        state.enforced = false;
        state.enforcement_error = Some(error);
    }
}

/// What a policy pull did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PolicyPull {
    Unchanged,
    Accepted(Box<PolicyState>),
    Refused(String),
}

/// The `If-None-Match` of the next pull: the kept ETag, if any.
pub(crate) fn if_none_match(state: &PolicyState) -> Option<&str> {
    state.etag.as_deref().filter(|etag| !etag.is_empty())
}

/// The last step of a pull with a full answer, once its effective policy
/// was received (`effective_taken`, [`super::effective_policy::receive`]):
/// a refused effective policy keeps no ETag. Hive tags the bundle it
/// signed, so a copy refused here (e.g. a signature broken on the way, the
/// genuine ETag passed through) would otherwise turn every later pull into
/// a 304 that never fetches the genuine bundle; without a tag the next pull
/// fetches it in full.
///
/// A newer policy in force than `previous` (effective version or digest,
/// or the 1.x org policy) ends every policy hold ([`super::throttle`]):
/// each was the old policy's answer, and an admin who loosened it expects
/// the next claim or publish to ask Hive again. Quota throttles stay.
pub(crate) fn settle(
    store: &super::HiveStore,
    previous: &PolicyState,
    state: &mut PolicyState,
    effective_taken: bool,
) {
    if !effective_taken {
        state.etag = None;
    }
    let effective = |s: &PolicyState| s.effective.as_ref().map(|e| (e.version, e.digest.clone()));
    let moved = (effective(state).is_some() && effective(state) != effective(previous))
        || state.policy_version != previous.policy_version
        || state.statement_digest != previous.statement_digest;
    if moved {
        super::throttle::policy_changed(store);
    }
}

/// Pull, verify and keep the policy bundle.
pub fn pull(session: &Session) -> Result<PolicyPull, HiveError> {
    let data = session.store.data_dir().to_path_buf();
    let previous = load_state(&data);
    let path = format!("/policy/bundle?since={}", previous.policy_version);
    let extra: Vec<(&str, &str)> = if_none_match(&previous)
        .map(|etag| ("If-None-Match", etag))
        .into_iter()
        .collect();
    let answer =
        session.with_client(|c| c.send("GET", &path, None, &extra, MAX_POLICY_BUNDLE_BYTES))?;
    let now = session.lease.service_time(Utc::now()).with_timezone(&Utc);
    let stamp = now.to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    if answer.status == 304 {
        // The runtime may have arrived since the bundle was verified.
        let mut state = previous.clone();
        if !previous.enforced
            && previous.org_state.is_some()
            && let Some(bundle) = load_bundle(&data)
        {
            try_enforce(&bundle, &mut state, &session.lease.lease);
        }
        // An ack that failed (offline) is sent again.
        super::effective_policy::on_pull(session, None, &mut state, now);
        if state != previous {
            save_state(&data, &state)?;
        }
        session.store.record_success("policy", &stamp);
        return Ok(PolicyPull::Unchanged);
    }
    let keys_raw: Value = session
        .store
        .read_config(super::state::KEYS_FILE)?
        .ok_or(HiveError::NotConnected)?;
    let keyset = verify_keyset(&keys_raw, session.root_key())?;
    match verify_bundle(&answer.body, &session.lease.lease, &keyset, &previous, now) {
        Ok((mut state, filters)) => {
            state.etag = answer.etag;
            let local = |e: serde_json::Error| HiveError::Local(e.to_string());
            write_private(
                &data.join("policy"),
                BUNDLE_FILE,
                &serde_json::to_vec_pretty(&answer.body).map_err(local)?,
            )?;
            write_private(
                &data.join("policy").join("filters"),
                FILTER_SET_FILE,
                &serde_json::to_vec(&filters).map_err(local)?,
            )?;
            try_enforce(&answer.body, &mut state, &session.lease.lease);
            let taken =
                super::effective_policy::on_pull(session, Some(&answer.body), &mut state, now);
            settle(&session.store, &previous, &mut state, taken);
            save_state(&data, &state)?;
            session.store.record_success("policy", &stamp);
            Ok(PolicyPull::Accepted(Box::new(state)))
        }
        Err(reason) => {
            let mut kept = previous.clone();
            kept.last_refusal = Some(reason.clone());
            // The effective policy is verified on its own: a refused 1.x
            // part does not hold back a newer, stricter effective policy.
            let taken =
                super::effective_policy::on_pull(session, Some(&answer.body), &mut kept, now);
            settle(&session.store, &previous, &mut kept, taken);
            save_state(&data, &kept)?;
            session.store.record_error(&format!("policy: {reason}"));
            Ok(PolicyPull::Refused(reason))
        }
    }
}

#[cfg(test)]
pub(crate) mod test_support {
    use super::*;
    use ed25519_dalek::{Signer as _, SigningKey};

    pub(crate) const PACK: &str = "name = \"acme-floor\"\nversion = \"1.0.0\"\ndescription = \"ACME org floor\"\n\n[context]\ndeny_tools = [\"ctx_url_read\"]\n";

    pub(crate) fn public_b64(seed: [u8; 32]) -> String {
        STANDARD.encode(SigningKey::from_bytes(&seed).verifying_key().as_bytes())
    }

    /// A signed artifact for `org` at `version`, valid `from..until`.
    pub(crate) fn artifact(
        seed: [u8; 32],
        org: &str,
        version: u64,
        from: &str,
        until: &str,
        pack: &str,
    ) -> Value {
        artifact_requiring(seed, org, version, from, until, pack, Vec::new())
    }

    /// [`artifact`] that also requires `packs` (C 1.8).
    pub(crate) fn artifact_requiring(
        seed: [u8; 32],
        org: &str,
        version: u64,
        from: &str,
        until: &str,
        pack: &str,
        packs: Vec<RequiredFilterPackV1>,
    ) -> Value {
        let key = SigningKey::from_bytes(&seed);
        let public = key.verifying_key().to_bytes();
        let statement = OrgPolicyStatementV2 {
            schema: ORG_POLICY_SCHEMA.into(),
            organization_id: TeamId::new(org).unwrap(),
            key_id: KeyId::new(derived_key_id(&public)).unwrap(),
            policy_version: version,
            issued_at: TeamTimestampV1::new(from).unwrap(),
            not_before: TeamTimestampV1::new(from).unwrap(),
            not_after: TeamTimestampV1::new(until).unwrap(),
            grace_seconds: 3600,
            pack_digest: ContentDigest::new(format!("sha256:{}", digest_hex(pack.as_bytes())))
                .unwrap(),
            required_filter_packs: packs,
        };
        let mut signing = ORG_POLICY_SIGNATURE_DOMAIN.as_bytes().to_vec();
        signing.extend(serde_json::to_vec(&statement).unwrap());
        serde_json::to_value(OrgPolicyArtifactV2 {
            statement,
            public_key: Ed25519PublicKey::new(URL_SAFE_NO_PAD.encode(public)).unwrap(),
            signature: Ed25519Signature::new(URL_SAFE_NO_PAD.encode(key.sign(&signing).to_bytes()))
                .unwrap(),
            pack_toml: pack.into(),
            unpin: None,
        })
        .unwrap()
    }

    /// A signed filter pack listing for `pack_id` at `version` with `patterns`.
    #[allow(clippy::needless_pass_by_value)] // builder: callers hand over fresh JSON
    pub(crate) fn filter_pack_with(
        seed: [u8; 32],
        kid: &str,
        pack_id: &str,
        version: u64,
        patterns: Value,
    ) -> Value {
        let statement = serde_json::to_vec(&serde_json::json!({
            "schema": lean_ctx_protocol::hive::policy::FILTER_PACK_SCHEMA,
            "pack_id": pack_id, "version": version, "patterns": patterns,
        }))
        .unwrap();
        let signature = SigningKey::from_bytes(&seed).sign(&filter_pack_signing_input(&statement));
        serde_json::json!({
            "pack_id": pack_id, "version": version, "sha256": digest_hex(&statement),
            "envelope": {"statement_b64": STANDARD.encode(&statement), "kid": kid, "signature": STANDARD.encode(signature.to_bytes())},
        })
    }

    pub(crate) fn filter_pack(seed: [u8; 32], kid: &str, pack_id: &str, version: u64) -> Value {
        filter_pack_with(
            seed,
            kid,
            pack_id,
            version,
            serde_json::json!([{"id": "ahv", "kind": "regex", "pattern": "756\\.\\d{4}\\.\\d{4}\\.\\d{2}", "action": "redact"}]),
        )
    }

    #[allow(clippy::needless_pass_by_value)] // builder: callers hand over fresh JSON
    pub(crate) fn bundle(version: u64, org_policy: Option<Value>, packs: Vec<Value>) -> Value {
        serde_json::json!({
            "schema": lean_ctx_protocol::hive::policy::POLICY_BUNDLE_SCHEMA,
            "policy_version": version, "org_policy": org_policy, "unpin": null, "filter_packs": packs,
        })
    }
}

#[cfg(test)]
#[path = "policy_tests.rs"]
mod tests;
