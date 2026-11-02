// SPDX-License-Identifier: Apache-2.0
//! Filter packs from Hive.
//!
//! `GET /v1/policy/bundle` (conditional) → verified here before anything is
//! kept: filter packs must be signed by an Hive key of the verified key
//! set, match their digest and validate completely (every pattern
//! compiles); all packs together stay within [`HIVE_MAX_RULES`]. A bundle
//! that fails any check never replaces the last good one; the refusal is
//! shown in `hive status` and `lean-ctx doctor`, and local work goes on
//! under the last verified bundle.
//!
//! Hive-Protocol 2.1 has no TOML org policy: the bundle's `org_policy`,
//! `unpin` and `policy_version` and the lease's `policy_signer_keys` are
//! not read.
//!
//! Stored in `<data_dir>/hive/policy/`: the verified bundle, its state,
//! and `filters/set.json`, the one file the policy snapshot reads to apply
//! the patterns ([`filter_rules`]).

use std::path::Path;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use chrono::{DateTime, Utc};
use ed25519_dalek::{Signature, VerifyingKey};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use super::HiveError;
use super::auth::Session;
use super::lease::verify_keyset;
use super::state::write_private;
use lean_ctx_protocol::hive::lease::HiveKeySetV1;
use lean_ctx_protocol::hive::policy::{
    FilterActionV1, FilterPackRefV1, FilterPackV1, MAX_POLICY_BUNDLE_BYTES, PolicyBundleV1,
    RequiredFilterPackV1, filter_pack_signing_input,
};

pub const FILTER_SET_SCHEMA: &str = "leanctx.hive.filter-set/v1";
/// Hive filter rules have their own budget, apart from the user's policy
/// rules; a bundle above it is refused rather than applied in part.
pub const HIVE_MAX_RULES: usize = 128;
const STATE_FILE: &str = "state.json";
const BUNDLE_FILE: &str = "bundle.json";
const FILTER_SET_FILE: &str = "set.json";

/// What the last verified bundle established.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PolicyState {
    pub etag: Option<String>,
    /// Never loaded or stored: always 0 (a bundle carries no org policy).
    /// `lean-ctx doctor` reads it.
    #[serde(skip)]
    pub policy_version: u64,
    /// Never loaded or stored: always `None`, like `policy_version`.
    #[serde(skip)]
    pub org_state: Option<String>,
    /// `pack_id → version` of the accepted filter packs.
    #[serde(default)]
    pub filter_packs: std::collections::BTreeMap<String, u64>,
    pub verified_at: Option<String>,
    /// Why the newest bundle was refused (the last good one stays).
    pub last_refusal: Option<String>,
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

/// Verify a whole bundle against the key set, and against the last
/// accepted state (filter pack versions only move forward). Returns the
/// state it establishes and the verified filter packs. The bundle's TOML
/// org policy fields are not read.
pub fn verify_bundle(
    raw: &Value,
    keyset: &HiveKeySetV1,
    previous: &PolicyState,
    now: DateTime<Utc>,
) -> Result<(PolicyState, FilterSet), String> {
    let bundle: PolicyBundleV1 =
        serde_json::from_value(raw.clone()).map_err(|e| format!("bundle: {e}"))?;
    bundle.validate().map_err(|e| format!("bundle: {e}"))?;
    let mut state = PolicyState {
        etag: None,
        verified_at: Some(now.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)),
        effective: previous.effective.clone(),
        effective_refusal: previous.effective_refusal.clone(),
        effective_refused: previous.effective_refused.clone(),
        ..PolicyState::default()
    };
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

/// The policy runtime's check for an org policy `artifact` it still holds
/// from before Hive-Protocol 2.1 (verified there, not here): for every
/// filter pack its signed statement requires, the installed `set` must
/// hold statement bytes whose SHA-256 is the signed digest, and the pack
/// it applies must be exactly the one those bytes decode to. A set whose
/// patterns, bytes or listing were changed or removed by hand is refused
/// like any other tampering.
pub fn required_packs_installed(artifact: &str, set: Option<&str>) -> Result<(), String> {
    let artifact: Value =
        serde_json::from_str(artifact).map_err(|_| "org policy input is invalid")?;
    let required: Vec<RequiredFilterPackV1> = match artifact
        .get("statement")
        .and_then(|statement| statement.get("required_filter_packs"))
    {
        Some(list) => serde_json::from_value(list.clone())
            .map_err(|_| "org policy input is invalid".to_string())?,
        None if artifact.get("statement").is_some() => Vec::new(),
        None => return Err("org policy input is invalid".into()),
    };
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
/// A newer policy in force than `previous` (effective version or digest)
/// ends every policy hold ([`super::throttle`]):
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
    if effective(state).is_some() && effective(state) != effective(previous) {
        super::throttle::policy_changed(store);
    }
}

/// Pull, verify and keep the policy bundle.
pub fn pull(session: &Session) -> Result<PolicyPull, HiveError> {
    let data = session.store.data_dir().to_path_buf();
    let previous = load_state(&data);
    // `since=0` asks for the whole bundle.
    let path = "/policy/bundle?since=0";
    let extra: Vec<(&str, &str)> = if_none_match(&previous)
        .map(|etag| ("If-None-Match", etag))
        .into_iter()
        .collect();
    let answer =
        session.with_client(|c| c.send("GET", path, None, &extra, MAX_POLICY_BUNDLE_BYTES))?;
    let now = session.lease.service_time(Utc::now()).with_timezone(&Utc);
    let stamp = now.to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    if answer.status == 304 {
        let mut state = previous.clone();
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
    match verify_bundle(&answer.body, &keyset, &previous, now) {
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
            // The effective policy is verified on its own: a refused
            // filter pack does not hold back a newer, stricter effective
            // policy.
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
