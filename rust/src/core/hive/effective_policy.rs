// SPDX-License-Identifier: Apache-2.0
//! Unified Control on the engine (Release B 3.1.4, 3.1.6): receive, verify,
//! apply and acknowledge the signed effective policy a 2.x Hive adds to
//! `GET /v1/policy/bundle` (`effective_policy`).
//!
//! - Verification ([`verify`]): the bundle names this engine's org and
//!   instance; its key is a key-set v3 key of purpose `policy` that was
//!   valid when the bundle arrived; the signature covers the raw bundle; the
//!   digest is the policy's own; the version only grows — a lower version,
//!   or the same version with another digest, is a downgrade and refused.
//! - A refused bundle (or one dropped to `null` once a policy is in force)
//!   never replaces the last valid one. That one stays installed
//!   ([`super::effective_floor`]) and keeps restricting local work, also
//!   offline and after `not_after`.
//! - After installing, the engine acks `{version, digest, unenforced}`
//!   (`POST /v1/policy/ack`); `unenforced` lists every member it received
//!   but does not enforce. A failed ack is retried on the next pull.
//! - Sensitive operations (team sharing, a new trust boundary, rights
//!   expansion: the consent kinds of [`sensitive_kind`]) fail closed when
//!   the policy's authorisation has expired or the policy in force is
//!   missing or damaged ([`sensitive_gate`]).
//! - Agents never change it: only the scheduler/`hive sync` pull writes
//!   these files, no MCP tool reaches them, and a local pack can only
//!   tighten the floor. Hive enforces on paths through LeanCTX only.
//!
//! A 1.x Hive (prod until the cutover) sends no `effective_policy`; then
//! nothing here runs and the 1.x policy path is unchanged.

use std::path::Path;

use chrono::{DateTime, Utc};
use ed25519_dalek::Signature;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::HiveError;
use super::effective_floor::{
    FLOOR_FILE, FLOOR_SCHEMA, Floor, REQUIRED_FILE, Required, floor_dir_in,
};
use super::keyset_v3::{KeySetV3, Purpose};
use super::policy::PolicyState;
use super::state::write_private;
use lean_ctx_protocol::hive::effective_policy::{
    EffectivePolicyBundleV1, POLICY_MISMATCH, PolicyAckRefusalV1, PolicyAckRefusedResponseV1,
    PolicyAckRequestV1, PolicyAckResponseV1, PolicyAckStateV1, PolicyRefusalReasonV1,
    policy_digest, refusal_detail, signing_input_raw, unenforced_members,
};
use lean_ctx_protocol::hive::lease::HiveLeaseV1;

/// What the engine keeps of the effective policy in force (in the policy
/// state, `<data_dir>/hive/policy/state.json`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EffectiveState {
    pub version: u64,
    pub digest: String,
    pub issued_at: String,
    pub not_after: String,
    pub key_id: String,
    /// Members received that neither this engine nor Hive enforces
    /// (default and every vault), sorted: what the ack reports.
    pub unenforced: Vec<String>,
    /// Members received that Hive enforces itself (`x-enforcement:
    /// server`), sorted; shown in status, never acked as unenforced.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub by_hive: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ack: Option<AckRecord>,
    /// Why the last ack of this version failed (retried on the next pull).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ack_error: Option<String>,
}

/// The answer to an ack of this version.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AckRecord {
    pub version: u64,
    pub digest: String,
    /// `acked` or `outdated`.
    pub state: String,
    pub acked_at: String,
}

/// The verdict on a received bundle.
#[derive(Debug, Clone, PartialEq)]
pub enum Verdict {
    /// The version and digest already in force.
    Same,
    /// A newer, verified bundle.
    New(Box<EffectivePolicyBundleV1>),
}

fn hex_signature(text: &str) -> Option<Signature> {
    let mut bytes = [0u8; 64];
    hex::decode_to_slice(text, &mut bytes).ok()?;
    Some(Signature::from_bytes(&bytes))
}

/// Verify `raw` (the `effective_policy` object as received) for this
/// engine. `received_at` is the service time in ms.
pub fn verify(
    raw: &Value,
    keyset: &KeySetV3,
    lease: &HiveLeaseV1,
    previous: Option<&EffectiveState>,
    received_at: u64,
) -> Result<Verdict, String> {
    verify_coded(raw, keyset, lease, previous, received_at).map_err(|(_, why)| why)
}

/// [`verify`] with the contract's reason code of a refusal (SERVER-B4).
fn verify_coded(
    raw: &Value,
    keyset: &KeySetV3,
    lease: &HiveLeaseV1,
    previous: Option<&EffectiveState>,
    received_at: u64,
) -> Result<Verdict, (PolicyRefusalReasonV1, String)> {
    use PolicyRefusalReasonV1 as R;
    let invalid = |e: &dyn std::fmt::Display| (R::InvalidBundle, format!("effective policy: {e}"));
    let bundle: EffectivePolicyBundleV1 =
        serde_json::from_value(raw.clone()).map_err(|e| invalid(&e))?;
    bundle.validate().map_err(|e| invalid(&e))?;
    if bundle.org_id != lease.org_id || bundle.instance_id != lease.instance_id {
        return Err((
            R::WrongTarget,
            "the effective policy was issued for another workspace or engine".into(),
        ));
    }
    let key = keyset
        .key_for(&bundle.key_id, Purpose::Policy, received_at)
        .map_err(|r| (R::KeyUnavailable, format!("effective policy: {r}")))?;
    let input = signing_input_raw(raw).map_err(|e| invalid(&e))?;
    let verified = hex_signature(&bundle.signature)
        .is_some_and(|signature| key.verify_strict(&input, &signature).is_ok());
    if !verified {
        return Err((
            R::SignatureInvalid,
            "the effective policy signature does not verify".into(),
        ));
    }
    if policy_digest(&raw["policy"]).ok().as_deref() != Some(bundle.digest.as_str()) {
        return Err((
            R::DigestMismatch,
            "the effective policy digest is not the digest of its policy".into(),
        ));
    }
    if let Some(previous) = previous {
        match bundle.version.cmp(&previous.version) {
            std::cmp::Ordering::Less => {
                return Err((
                    R::Downgrade,
                    format!(
                        "effective policy v{} is older than v{} in force (downgrade refused)",
                        bundle.version, previous.version
                    ),
                ));
            }
            std::cmp::Ordering::Equal if bundle.digest != previous.digest => {
                return Err((
                    R::Downgrade,
                    format!(
                        "effective policy v{} was accepted with another digest (downgrade refused)",
                        bundle.version
                    ),
                ));
            }
            std::cmp::Ordering::Equal => return Ok(Verdict::Same),
            std::cmp::Ordering::Greater => {}
        }
    }
    Ok(Verdict::New(Box::new(bundle)))
}

/// Install a verified bundle under `hive_data`: the floor first, then the
/// requirement (a crash in between leaves an older requirement, which the
/// newer floor satisfies).
pub fn install(
    hive_data: &Path,
    bundle: &EffectivePolicyBundleV1,
) -> Result<EffectiveState, HiveError> {
    let dir = floor_dir_in(hive_data);
    let floor = Floor {
        schema: FLOOR_SCHEMA.into(),
        version: bundle.version,
        digest: bundle.digest.clone(),
        not_after: bundle.not_after.as_str().to_string(),
        policy: bundle.policy.clone(),
    };
    let local = |e: serde_json::Error| HiveError::Local(e.to_string());
    write_private(
        &dir,
        FLOOR_FILE,
        &serde_json::to_vec_pretty(&floor).map_err(local)?,
    )?;
    write_private(
        &dir,
        REQUIRED_FILE,
        &serde_json::to_vec(&Required {
            version: bundle.version,
        })
        .map_err(local)?,
    )?;
    Ok(EffectiveState {
        version: bundle.version,
        digest: bundle.digest.clone(),
        issued_at: bundle.issued_at.as_str().to_string(),
        not_after: bundle.not_after.as_str().to_string(),
        key_id: bundle.key_id.clone(),
        unenforced: unenforced_members(&bundle.policy),
        by_hive: lean_ctx_protocol::hive::effective_policy::server_members(&bundle.policy),
        ack: None,
        ack_error: None,
    })
}

/// Take the `effective_policy` of a pulled bundle response into `state`
/// (which starts as the previous state's copy): verify, install, record.
/// Never fails the pull; a refusal is recorded and the last valid policy
/// stays. `false` when the response's effective policy was not taken
/// (refused, or dropped while one is in force): the pull then keeps no
/// ETag of it, so the next pull fetches the bundle in full.
pub fn receive(
    hive_data: &Path,
    response: &Value,
    keyset: Option<&KeySetV3>,
    lease: &HiveLeaseV1,
    state: &mut PolicyState,
    received_at: u64,
) -> bool {
    let raw = response.get("effective_policy").unwrap_or(&Value::Null);
    if raw.is_null() {
        state.effective_refusal = state.effective.as_ref().map(|e| {
            format!(
                "Hive sent no effective policy; v{} stays in force",
                e.version
            )
        });
        return state.effective_refusal.is_none();
    }
    let verdict = match keyset {
        None => Err((
            PolicyRefusalReasonV1::KeyUnavailable,
            "no verified key set v3 to check the effective policy with".to_string(),
        )),
        Some(keyset) => verify_coded(raw, keyset, lease, state.effective.as_ref(), received_at),
    };
    let refused = match verdict {
        Ok(Verdict::Same) => None,
        Ok(Verdict::New(bundle)) => match install(hive_data, &bundle) {
            Ok(installed) => {
                state.effective = Some(installed);
                None
            }
            Err(error) => Some((
                PolicyRefusalReasonV1::InstallFailed,
                format!("install the effective policy: {error}"),
            )),
        },
        Err(refused) => Some(refused),
    };
    match refused {
        None => {
            state.effective_refusal = None;
            state.effective_refused = None;
            true
        }
        Some((code, why)) => {
            let fresh = RefusalRecord::of(raw, code, &why, state.effective.as_ref());
            state.effective_refused = match (state.effective_refused.take(), fresh) {
                // The same bundle again: it is reported once.
                (Some(known), Some(fresh))
                    if known.version == fresh.version && known.digest == fresh.digest =>
                {
                    Some(known)
                }
                (_, fresh) => fresh,
            };
            state.effective_refusal = Some(why);
            false
        }
    }
}

/// A refused bundle as Hive hears of it (SERVER-B4 `refused`): once per
/// received version and digest, with the bundle the engine keeps.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefusalRecord {
    /// `version` and `digest` of the bundle as received.
    pub version: u64,
    pub digest: String,
    pub refused: PolicyAckRefusalV1,
    /// `refused_at` of Hive' answer, once reported.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reported_at: Option<String>,
    /// Why the last report failed (retried on the next pull).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub report_error: Option<String>,
}

impl RefusalRecord {
    /// `None` when the received object names no usable version and digest
    /// (nothing Hive could match it to).
    fn of(
        raw: &Value,
        code: PolicyRefusalReasonV1,
        why: &str,
        applied: Option<&EffectiveState>,
    ) -> Option<Self> {
        let version = raw["version"].as_u64().filter(|v| *v >= 1)?;
        let digest = raw["digest"].as_str().filter(|d| {
            d.strip_prefix("sha256:").is_some_and(|h| {
                h.len() == 64
                    && h.bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            })
        })?;
        // The bundle in force itself (only its key set is missing now):
        // nothing was refused that Hive does not already see applied.
        if applied.is_some_and(|e| e.version == version && e.digest == digest) {
            return None;
        }
        Some(Self {
            version,
            digest: digest.to_string(),
            refused: PolicyAckRefusalV1 {
                reason_code: code,
                detail: refusal_detail(why),
                applied_version: applied.map(|e| e.version),
                applied_digest: applied.map(|e| e.digest.clone()),
            },
            reported_at: None,
            report_error: None,
        })
    }
}

/// Tell Hive once that this engine refused the bundle `record` names
/// (again after a failure). Only a 2.x Hive sends effective policies, so
/// only one ever gets a refusal.
pub fn report_refusal_with(client: &super::transport::HiveClient, record: &mut RefusalRecord) {
    if record.reported_at.is_some() {
        return;
    }
    let request = PolicyAckRequestV1::refusal(
        record.version,
        record.digest.clone(),
        record.refused.clone(),
    );
    let body = match serde_json::to_value(&request) {
        Ok(body) => body,
        Err(error) => {
            record.report_error = Some(error.to_string());
            return;
        }
    };
    let answer = client.post("/policy/ack", &body).and_then(|answer| {
        let answer: PolicyAckRefusedResponseV1 = serde_json::from_value(answer)
            .map_err(|e| HiveError::Protocol(format!("policy refusal: {e}")))?;
        answer
            .validate_for(&request)
            .map_err(|e| HiveError::Protocol(format!("policy refusal: {e}")))?;
        Ok(answer)
    });
    match answer {
        Ok(answer) => {
            record.reported_at = Some(answer.refused_at.as_str().to_string());
            record.report_error = None;
        }
        Err(HiveError::Server { status: 409, .. }) => {
            record.report_error = Some(format!(
                "{POLICY_MISMATCH}: Hive did not issue v{} with this digest to this engine",
                record.version
            ));
        }
        Err(error) => record.report_error = Some(error.to_string()),
    }
}

/// Ack the installed version once (again after a failure, or once a newer
/// version is in force). Only after [`install`]: the ack says "applied".
pub fn ack_with(client: &super::transport::HiveClient, state: &mut EffectiveState) {
    if state
        .ack
        .as_ref()
        .is_some_and(|a| a.version == state.version && a.digest == state.digest)
    {
        return;
    }
    // A state written by an older build may still list server members.
    let mut unenforced: Vec<String> = state
        .unenforced
        .iter()
        .filter(|m| !lean_ctx_protocol::hive::effective_policy::is_server_member(m))
        .cloned()
        .collect();
    unenforced.truncate(lean_ctx_protocol::hive::effective_policy::MAX_UNENFORCED);
    let request = PolicyAckRequestV1 {
        version: state.version,
        digest: state.digest.clone(),
        unenforced,
        refused: None,
    };
    let body = match serde_json::to_value(&request) {
        Ok(body) => body,
        Err(error) => {
            state.ack_error = Some(error.to_string());
            return;
        }
    };
    let answer = client.post("/policy/ack", &body).and_then(|answer| {
        let answer: PolicyAckResponseV1 = serde_json::from_value(answer)
            .map_err(|e| HiveError::Protocol(format!("policy ack: {e}")))?;
        answer
            .validate_for(&request)
            .map_err(|e| HiveError::Protocol(format!("policy ack: {e}")))?;
        Ok(answer)
    });
    match answer {
        Ok(answer) => {
            state.ack = Some(AckRecord {
                version: answer.version,
                digest: answer.digest,
                state: match answer.state {
                    PolicyAckStateV1::Acked => "acked".into(),
                    PolicyAckStateV1::Outdated => "outdated".into(),
                },
                acked_at: answer.acked_at.as_str().to_string(),
            });
            state.ack_error = None;
        }
        // `policy_mismatch` is the one 409 of this route (C0).
        Err(HiveError::Server { status: 409, .. }) => {
            state.ack_error = Some(format!(
                "{POLICY_MISMATCH}: Hive did not issue v{} with this digest to this engine",
                state.version
            ));
        }
        Err(error) => state.ack_error = Some(error.to_string()),
    }
}

/// One pull's effective-policy work (`None` body: 304, only the ack retry).
/// `false` when the body's effective policy was not taken ([`receive`]).
pub(crate) fn on_pull(
    session: &super::auth::Session,
    body: Option<&Value>,
    state: &mut PolicyState,
    now: DateTime<Utc>,
) -> bool {
    let mut taken = true;
    if let Some(body) = body {
        let present = body
            .get("effective_policy")
            .is_some_and(|raw| !raw.is_null());
        if !present && state.effective.is_none() {
            return true;
        }
        let keyset = if present {
            // A signing key the stored set does not know yet (a rotation):
            // fetch the current set once.
            let key_id = body["effective_policy"]["key_id"]
                .as_str()
                .unwrap_or_default();
            super::keyset_v3::load(&session.store, session.root_key())
                .filter(|set| set.keys.iter().any(|k| k.key_id == key_id))
                .or_else(|| super::keyset_v3::refresh(session).ok())
        } else {
            None
        };
        let received_at = u64::try_from(now.timestamp_millis()).unwrap_or(0);
        taken = receive(
            session.store.data_dir(),
            body,
            keyset.as_ref(),
            &session.lease.lease,
            state,
            received_at,
        );
    }
    super::company_bound::on_policy(&session.store, &session.instance);
    if let Some(effective) = state.effective.as_mut()
        && let Ok(client) = session.client()
    {
        ack_with(&client, effective);
    }
    if let Some(record) = state.effective_refused.as_mut()
        && record.reported_at.is_none()
        && let Ok(client) = session.client()
    {
        report_refusal_with(&client, record);
    }
    taken
}

/// Consent kinds that are sensitive operations: they open a new trust
/// boundary, share with a team or the public, or expand rights.
pub fn sensitive_kind(kind: super::consent::Kind) -> bool {
    use super::consent::Kind;
    matches!(
        kind,
        Kind::LinkVault
            | Kind::PublicPresence
            | Kind::SwarmJoin
            | Kind::PledgeChange
            | Kind::UnattendedStart
            | Kind::ReviewerEnable
            | Kind::SealedGrant
    )
}

/// `now` is at or past `not_after` (an unreadable instant counts as past).
fn expired_at(not_after: &str, now: DateTime<Utc>) -> bool {
    !DateTime::parse_from_rfc3339(not_after).is_ok_and(|until| now < until.with_timezone(&Utc))
}

/// Whether a sensitive operation may go on under the Hive policy in
/// `hive_data`: never once an installed policy's authorisation expired
/// (`now ≥ not_after`), nor while the policy in force is missing or
/// damaged. Without any installed Hive policy (1.x, or never sent) there
/// is nothing to check.
pub fn sensitive_gate(hive_data: &Path, now: DateTime<Utc>) -> Result<(), String> {
    let (floor, required) = super::effective_floor::read(&floor_dir_in(hive_data))?;
    let Some(floor) = super::effective_floor::check(floor.as_deref(), required.as_deref())? else {
        return Ok(());
    };
    let expired = expired_at(&floor.not_after, now);
    if expired {
        return Err(format!(
            "the Hive policy v{} expired at {}; sensitive operations stay refused until this engine reaches Hive again (`lean-ctx hive sync`)",
            floor.version, floor.not_after
        ));
    }
    Ok(())
}

/// `hive status` lines of the effective policy, deterministic in `state`,
/// `now` and the installed files; with `project_root`, also the local
/// settings there that the Hive policy overrides.
pub fn status_lines(
    hive_data: &Path,
    state: &PolicyState,
    now: DateTime<Utc>,
    project_root: Option<&Path>,
) -> Vec<String> {
    let mut lines = Vec::new();
    let installed = super::effective_floor::read(&floor_dir_in(hive_data))
        .and_then(|(f, r)| super::effective_floor::check(f.as_deref(), r.as_deref()));
    match (&state.effective, &installed) {
        (_, Err(problem)) => lines.push(format!(
            "Policy:       {problem}; every tool is refused until the next pull installs it"
        )),
        (Some(e), Ok(_)) => {
            let expired = expired_at(&e.not_after, now);
            lines.push(format!(
                "Policy:       v{} {} ({}), {} {}",
                e.version,
                e.digest.get(..19).unwrap_or(&e.digest),
                if expired { "EXPIRED" } else { "valid" },
                if expired {
                    "its rules still apply; sensitive operations refused since"
                } else {
                    "applied until"
                },
                e.not_after
            ));
            lines.push(match (&e.ack, &e.ack_error) {
                (Some(a), _) if a.version == e.version && a.digest == e.digest => {
                    format!("Policy ack:   {} at {}", a.state, a.acked_at)
                }
                (_, Some(error)) => {
                    format!("Policy ack:   not yet ({error}); retried on the next pull")
                }
                _ => "Policy ack:   pending".to_string(),
            });
            // Status says what Hive enforces itself and what nobody does.
            // (A state of an older build lists server members as unenforced.)
            let (mut by_hive, nowhere): (Vec<&str>, Vec<&str>) = e
                .unenforced
                .iter()
                .map(String::as_str)
                .partition(|m| lean_ctx_protocol::hive::effective_policy::is_server_member(m));
            by_hive.extend(e.by_hive.iter().map(String::as_str));
            by_hive.sort_unstable();
            by_hive.dedup();
            if !by_hive.is_empty() {
                lines.push(format!(
                    "By Hive:     {} (Hive refuses, this engine does not check)",
                    by_hive.join(", ")
                ));
            }
            lines.push(if nowhere.is_empty() {
                "Unenforced:   none".to_string()
            } else {
                format!("Unenforced:   {}", nowhere.join(", "))
            });
        }
        (None, Ok(Some(_))) => {
            lines.push("Policy:       installed, state lost; applied until the next pull".into());
        }
        (None, Ok(None)) => {}
    }
    if let Some(refusal) = &state.effective_refusal {
        lines.push(format!(
            "Policy refused: {refusal} (the last valid policy stays)"
        ));
    }
    if let Some(root) = project_root {
        for line in super::effective_floor::local_overrides(hive_data, root) {
            lines.push(format!(
                "Ignored:      {line} (local settings only tighten)"
            ));
        }
    }
    lines
}

#[cfg(test)]
#[path = "effective_policy_tests.rs"]
mod tests;
