// SPDX-License-Identifier: Apache-2.0
//! Project → vault mapping and the first-share confirmation (CHECKLIST
//! 2.1.2, 2.1.3; design `ee/delivery/hive-v5-project-mapping.md`).
//!
//! For a checkout this machine has not mapped yet:
//! - A Hive with `POST /v1/vaults/match` gets the identity candidates
//!   ([`super::identity_v2`]). `bound` maps at once. `suggested` and `none`
//!   (with `can_create`) go to the existing resolve, which binds or creates.
//! - Without the route (1.x prod; a 404 is remembered for a day) resolve
//!   runs as before.
//!
//! The person confirms once before knowledge first crosses a new boundary:
//! a vault this machine never shared with (`suggested`, `none`, a committed
//! link file, or any first resolve on a Hive without the route, since
//! resolve creates a vault on first sight), and a vault that moved into
//! another workspace (personal → team). It is a `link_vault` consent,
//! answered only by the person (`lean-ctx hive consent`), never by an
//! agent. The gate holds for machines whose person answered the sharing
//! question at `hive connect`; machines connected without that answer
//! (scripts, harnesses, earlier engines) keep the earlier behaviour.

use std::fmt;
use std::path::Path;

use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::auth::Session;
use super::config::HiveConfig;
use super::consent::{self, Broker, ConsentError, Kind, Trust};
use super::identity_v2;
use super::state::MATCH_FILE;
use super::vault::{VaultRecord, identity_digest, resolve_request};
use super::{HiveError, HiveStore};
use lean_ctx_protocol::hive::vault::{VaultRefV1, VaultV1};

pub const MATCH_REQUEST_SCHEMA: &str = "leanctx.hive.vault-match-request/v1";
pub const MATCH_SCHEMA: &str = "leanctx.hive.vault-match/v1";
/// A remembered "no match route" is checked again after this long.
const REPROBE_SECONDS: i64 = 24 * 60 * 60;
/// Workspace part of the consent address of a vault Hive may create.
pub const NEW_VAULT_WS: &str = "new";

/// Why a checkout is not mapped (yet).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MapError {
    Hive(HiveError),
    /// The person has not confirmed the first share (or declined it).
    Consent(ConsentError),
}

impl fmt::Display for MapError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Hive(error) => error.fmt(f),
            Self::Consent(error) => write!(f, "{}: {error}", error.code()),
        }
    }
}

impl From<HiveError> for MapError {
    fn from(error: HiveError) -> Self {
        Self::Hive(error)
    }
}

impl From<ConsentError> for MapError {
    fn from(error: ConsentError) -> Self {
        Self::Consent(error)
    }
}

impl From<MapError> for HiveError {
    fn from(error: MapError) -> Self {
        match error {
            MapError::Hive(error) => error,
            MapError::Consent(error) => error.into(),
        }
    }
}

#[derive(Serialize)]
struct MatchCandidate {
    kind: &'static str,
    digest: String,
}

/// `leanctx.hive.vault-match-request/v1`: 1-3 candidates, each kind once.
/// No label: it goes with the resolve the person confirmed.
#[derive(Serialize)]
struct MatchRequest<'a> {
    schema: &'static str,
    org_salt_id: &'a str,
    candidates: &'a [MatchCandidate],
}

/// The org-salted candidates of the checkout at `root`, strongest first
/// (a weak legacy name is left out next to an exact candidate).
fn match_candidates(root: &Path, salt: &[u8]) -> Vec<MatchCandidate> {
    salted(&identity_v2::match_candidates(root), salt)
}

fn salted(candidates: &[identity_v2::Candidate], salt: &[u8]) -> Vec<MatchCandidate> {
    candidates
        .iter()
        .map(|c| MatchCandidate {
            kind: c.kind.as_str(),
            digest: identity_digest(salt, &c.stable),
        })
        .collect()
}

/// `leanctx.hive.vault-match/v1`. Members this engine does not know are
/// ignored, so the server may add some.
#[derive(Debug, Clone, Deserialize)]
pub struct MatchAnswer {
    pub schema: String,
    #[serde(rename = "match")]
    pub outcome: String,
    #[serde(default)]
    pub vault: Option<VaultV1>,
    #[serde(default)]
    pub suggestion: Option<VaultRefV1>,
    #[serde(default)]
    pub can_create: bool,
}

/// What an answer of the match route lets the engine do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    /// Hive knows the project and this instance may read its vault.
    Mapped(VaultV1),
    /// Only the root commit matched (forks share it): bind after a yes.
    Suggested(VaultRefV1),
    /// Unknown project that this instance may create a vault for.
    Create,
    /// Unknown project, and this instance may not create a vault.
    NoVault,
}

/// The step an answer names, or why the answer breaks the contract.
pub fn step_from_answer(answer: MatchAnswer) -> Result<Step, HiveError> {
    let bad = |why: &str| HiveError::Protocol(format!("vault match: {why}"));
    if answer.schema != MATCH_SCHEMA {
        return Err(bad("unknown schema"));
    }
    match answer.outcome.as_str() {
        "bound" => {
            let vault = answer.vault.ok_or_else(|| bad("bound without a vault"))?;
            vault.validate().map_err(|e| bad(&e.to_string()))?;
            Ok(Step::Mapped(vault))
        }
        "suggested" => {
            let vault = answer
                .suggestion
                .ok_or_else(|| bad("suggested without a suggestion"))?;
            vault.validate().map_err(|e| bad(&e.to_string()))?;
            Ok(Step::Suggested(vault))
        }
        "none" if answer.can_create => Ok(Step::Create),
        "none" => Ok(Step::NoVault),
        _ => Err(bad("unknown match")),
    }
}

/// A Hive without the route: 404 (axum's own, or a `not_found` body),
/// 405, or 426 (a Hive that refuses the protocol version).
pub fn route_missing(error: &HiveError) -> bool {
    matches!(
        error,
        HiveError::NotFound
            | HiveError::Server {
                status: 404 | 405 | 426,
                ..
            }
    )
}

#[derive(Serialize, Deserialize)]
struct MatchProbe {
    /// Unix seconds of the last "no route" answer.
    unsupported_at: i64,
}

/// Whether to ask the match route: a C0 2.0 Hive that did not answer
/// "no route" within the last day.
pub fn match_available(store: &HiveStore, now: i64) -> bool {
    if !super::bus::relay_v2::speaks_v2(store) {
        return false;
    }
    store
        .read_config::<MatchProbe>(MATCH_FILE)
        .ok()
        .flatten()
        .is_none_or(|probe| {
            now < probe.unsupported_at || now - probe.unsupported_at >= REPROBE_SECONDS
        })
}

fn remember_missing(store: &HiveStore, now: i64) {
    // A lost note only costs one more 404 on the next pass.
    let _ = store.write_config(
        MATCH_FILE,
        &MatchProbe {
            unsupported_at: now,
        },
    );
}

/// The person answered the sharing question at connect for this workspace:
/// from then on a first share waits for their yes.
pub fn first_share_gated(session: &Session) -> bool {
    super::connect::decision(&session.store, &session.instance.org_id).is_some()
}

/// The vault a first share goes to, as the person confirms it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// An existing vault.
    Bind(VaultRefV1),
    /// The project's own vault in the workspace: the one Hive already
    /// binds to it, or a new private one.
    Own,
}

/// `[a-z0-9-]{2,40}` from the checkout folder name, else `project`.
pub fn new_vault_name(root: &Path) -> String {
    let raw = root
        .file_name()
        .map(|n| n.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    let slug: String = raw
        .chars()
        .map(|c| {
            if c.is_ascii_lowercase() || c.is_ascii_digit() {
                c
            } else {
                '-'
            }
        })
        .collect();
    let slug: String = slug.trim_matches('-').chars().take(40).collect();
    let slug = slug.trim_end_matches('-');
    if slug.len() >= 2 {
        slug.to_string()
    } else {
        "project".into()
    }
}

/// The `link_vault` subject for sharing the checkout at `root` (identity
/// `stable`) with `target`. `vault` is what the person types to approve.
pub fn consent_subject(target: &Target, stable: &str, root: &Path) -> Value {
    let vault = match target {
        Target::Bind(vault) => format!("{}/{}", vault.ns, vault.name),
        Target::Own => format!("{NEW_VAULT_WS}/{}", new_vault_name(root)),
    };
    let text = |value: Option<String>| {
        value
            .filter(|v| (1..=500).contains(&v.chars().count()))
            .map_or(Value::Null, Value::String)
    };
    consent::subject(&[
        (
            "bound_repo",
            text(root.file_name().map(|n| n.to_string_lossy().into_owned())),
        ),
        ("link_digest", Value::String(stable.to_string())),
        ("remote_seen", text(identity_v2::origin_remote(root))),
        ("target_vault", Value::Null),
        ("vault", Value::String(vault)),
    ])
}

fn require(
    session: &Session,
    config: &HiveConfig,
    target: &Target,
    stable: &str,
    root: &Path,
) -> Result<(), MapError> {
    let broker = Broker::open(&session.store, Trust::load(&session.store, config));
    let now = u64::try_from(Utc::now().timestamp_millis()).unwrap_or_default();
    broker.require(Kind::LinkVault, &consent_subject(target, stable, root), now)?;
    Ok(())
}

/// A first-share request for a checkout `map` already knows: nothing to
/// answer, so `hive consent` and `hive status` never list it as waiting
/// (the next sync closes it here and in Hive, [`close_satisfied`]).
pub fn satisfied(record: &consent::Record, map: &super::vault::VaultMap) -> bool {
    record
        .subject
        .get("link_digest")
        .and_then(Value::as_str)
        .and_then(|stable| map.vaults.get(stable))
        .is_some_and(|mapped| satisfied_by(record, mapped))
}

/// `record` asks to share into what `mapped` already is: the vault it is
/// mapped to, or a new vault when one exists now. A move into another
/// vault (personal → team) is not satisfied and keeps waiting.
fn satisfied_by(record: &consent::Record, mapped: &VaultRecord) -> bool {
    if record.kind != Kind::LinkVault || !record.subject["target_vault"].is_null() {
        return false;
    }
    let vault = record.subject["vault"].as_str().unwrap_or_default();
    vault == format!("{}/{}", mapped.ns, mapped.name)
        || vault.starts_with(&format!("{NEW_VAULT_WS}/"))
}

/// The checkout with identity `stable` is mapped: a first-share request
/// still open for it (opened before another engine's sync created the
/// vault) asks for nothing any more. It is closed here and withdrawn in
/// Hive, so neither `hive consent`, `hive status` nor a web page lists
/// it as waiting. Returns the closed ids.
pub fn close_satisfied(
    session: &Session,
    config: &HiveConfig,
    stable: &str,
    mapped: &VaultRecord,
) -> Vec<String> {
    let broker = Broker::open(&session.store, Trust::load(&session.store, config));
    let closed = broker
        .close_satisfied(&|r| r.subject["link_digest"] == stable && satisfied_by(r, mapped))
        .unwrap_or_else(|error| {
            tracing::info!("hive consent close: {error}");
            Vec::new()
        });
    for id in &closed {
        let _ = session.with_client(|client| {
            consent::web::withdraw_relayed(client, id);
            Ok(())
        });
    }
    closed
}

/// The vault of the checkout at `root` (identity `stable`), which this
/// machine has not mapped there. `known` is this machine's record of the
/// same repository from another checkout, if any.
pub fn map_root(
    session: &Session,
    config: &HiveConfig,
    salt: &(String, Vec<u8>),
    stable: &str,
    root: &Path,
    known: Option<&VaultRecord>,
) -> Result<VaultV1, MapError> {
    // sharing.personal_workspace: a team project of a forbidding company
    // gets no vault in a Personal workspace (and is recorded in the team).
    super::company_bound::on_map(&session.store, &session.instance, stable)?;
    let (resolved, mut request) = resolve_request(root, &salt.0, &salt.1, config.vault_labels)?;
    if resolved != stable {
        return Err(HiveError::Config(format!(
            "{} changed its identity since the draft was made",
            root.display()
        ))
        .into());
    }
    let gated = first_share_gated(session) && known.is_none();
    let now = Utc::now().timestamp();
    let candidates = match_candidates(root, &salt.1);
    // Set once the match route answered: resolve then carries the exact
    // digest (C0 vault_match), and match runs once more after it.
    let mut matched = false;
    // A committed link file is a signal of the repository, not this
    // person's yes: it names the target, the person still confirms.
    let target = match request.vault.clone() {
        Some(vault) => Target::Bind(vault),
        None if match_available(&session.store, now) => {
            let step = ask_match(session, &salt.0, &candidates, now)?;
            matched = step.is_some();
            match step {
                Some(Step::Mapped(vault)) => {
                    return confirm_move(session, config, stable, root, known, vault);
                }
                Some(Step::Suggested(vault)) if gated => Target::Bind(vault),
                Some(Step::NoVault) => {
                    return Err(HiveError::Refused(
                        "this project has no vault in the workspace and this engine may not create one; ask a maintainer, or name one with `lean-ctx hive link <workspace>/<vault>`".into(),
                    )
                    .into());
                }
                // Without the gate a suggestion is not bound: like before
                // the route existed, resolve creates the project's vault.
                Some(Step::Suggested(_) | Step::Create) | None => Target::Own,
            }
        }
        None => Target::Own,
    };
    if gated {
        require(session, config, &target, stable, root)?;
    }
    if let Target::Bind(vault) = &target {
        request.vault = Some(vault.clone());
    }
    if matched && let Some(exact) = candidates.iter().find(|c| c.kind == "exact") {
        request.identity_digest.clone_from(&exact.digest);
    }
    let body = serde_json::to_value(&request).map_err(|e| HiveError::Local(e.to_string()))?;
    let answer = session.with_client(|c| c.post("/vaults/resolve", &body))?;
    let vault: VaultV1 =
        serde_json::from_value(answer).map_err(|e| HiveError::Protocol(format!("vault: {e}")))?;
    vault
        .validate()
        .map_err(|e| HiveError::Protocol(e.to_string()))?;
    if matched {
        // The bound match records the history alias of the new binding;
        // the resolve already succeeded, so its outcome does not matter.
        let _ = ask_match(session, &salt.0, &candidates, now);
    }
    confirm_move(session, config, stable, root, known, vault)
}

/// Personal → team: a repository this machine shared into one workspace
/// whose vault now answers from another waits for the person's yes.
fn confirm_move(
    session: &Session,
    config: &HiveConfig,
    stable: &str,
    root: &Path,
    known: Option<&VaultRecord>,
    vault: VaultV1,
) -> Result<VaultV1, MapError> {
    if first_share_gated(session)
        && let Some(known) = known
        && known.ns != vault.vault.ns
    {
        require(
            session,
            config,
            &Target::Bind(vault.vault.clone()),
            stable,
            root,
        )?;
    }
    Ok(vault)
}

/// `POST /v1/vaults/match`; `None` when this Hive has no such route.
fn ask_match(
    session: &Session,
    org_salt_id: &str,
    candidates: &[MatchCandidate],
    now: i64,
) -> Result<Option<Step>, HiveError> {
    if candidates.is_empty() {
        return Ok(None);
    }
    let body = match_body(org_salt_id, candidates)?;
    match session.with_client(|c| c.post("/vaults/match", &body)) {
        Ok(answer) => {
            let answer: MatchAnswer = serde_json::from_value(answer)
                .map_err(|e| HiveError::Protocol(format!("vault match: {e}")))?;
            step_from_answer(answer).map(Some)
        }
        Err(error) if route_missing(&error) => {
            remember_missing(&session.store, now);
            Ok(None)
        }
        Err(error) => Err(error),
    }
}

fn match_body(org_salt_id: &str, candidates: &[MatchCandidate]) -> Result<Value, HiveError> {
    serde_json::to_value(MatchRequest {
        schema: MATCH_REQUEST_SCHEMA,
        org_salt_id,
        candidates,
    })
    .map_err(|e| HiveError::Local(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn vault_json() -> Value {
        json!({
            "schema": "leanctx.hive.vault/v1",
            "vault_id": "3f6c1e9a0b7d4c2e8f5a1b6c9d0e7f42",
            "vault": {"ns": "acme", "name": "payments"},
            "sealed": false,
        })
    }

    fn answer(value: Value) -> MatchAnswer {
        serde_json::from_value(value).unwrap()
    }

    fn route_fixture(name: &str) -> Value {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("crates/lean-ctx-protocol/tests/fixtures/hive-v2/routes")
            .join(name);
        serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
    }

    /// C0 `vault_match.request`: the body the engine builds for the first
    /// identity vector is the fixture, byte for byte as JSON (no label).
    #[test]
    fn the_match_request_is_the_contract_fixture() {
        use base64::Engine as _;
        let vectors = identity_v2::contract_vectors();
        let salt = base64::engine::general_purpose::STANDARD
            .decode(vectors["salt"].as_str().unwrap())
            .unwrap();
        let input = &vectors["cases"][0]["input"];
        let candidates = identity_v2::candidates_from(
            &identity_v2::vector_parts(input),
            input["legacy_identity"].as_str(),
        );
        let body = match_body(
            vectors["org_salt_id"].as_str().unwrap(),
            &salted(&candidates, &salt),
        )
        .unwrap();
        assert_eq!(body, route_fixture("vault_match.request.json"));
    }

    /// C0 `vault_match.*.response` and the response cases of `invalid/`
    /// the engine itself must refuse.
    #[test]
    fn the_contract_responses_become_steps() {
        let step = |name: &str| step_from_answer(answer(route_fixture(name)));
        assert!(
            matches!(step("vault_match.bound.response.json"), Ok(Step::Mapped(v)) if v.vault.name == "payments")
        );
        assert!(
            matches!(step("vault_match.bound_legacy.response.json"), Ok(Step::Mapped(v)) if v.sealed)
        );
        assert!(
            matches!(step("vault_match.suggested.response.json"), Ok(Step::Suggested(v)) if v.ns == "acme")
        );
        assert_eq!(step("vault_match.none.response.json"), Ok(Step::Create));
        let mut bound = route_fixture("vault_match.bound.response.json");
        bound.as_object_mut().unwrap().remove("vault");
        assert!(
            step_from_answer(answer(bound)).is_err(),
            "bound_without_vault"
        );
    }

    #[test]
    fn match_answers_become_steps_and_broken_ones_are_refused() {
        let bound = answer(json!({
            "schema": MATCH_SCHEMA, "match": "bound", "matched": "legacy",
            "vault": vault_json(), "something_new": 1,
        }));
        match step_from_answer(bound).unwrap() {
            Step::Mapped(vault) => assert_eq!(vault.vault.name, "payments"),
            other => panic!("{other:?}"),
        }
        let suggested = answer(json!({
            "schema": MATCH_SCHEMA, "match": "suggested", "matched": "history",
            "suggestion": {"ns": "acme", "name": "payments"},
        }));
        assert!(
            matches!(step_from_answer(suggested).unwrap(), Step::Suggested(v) if v.ns == "acme")
        );
        let create = answer(json!({"schema": MATCH_SCHEMA, "match": "none", "can_create": true}));
        assert_eq!(step_from_answer(create).unwrap(), Step::Create);
        let none = answer(json!({"schema": MATCH_SCHEMA, "match": "none"}));
        assert_eq!(step_from_answer(none).unwrap(), Step::NoVault);

        for broken in [
            json!({"schema": MATCH_SCHEMA, "match": "bound"}),
            json!({"schema": MATCH_SCHEMA, "match": "suggested"}),
            json!({"schema": MATCH_SCHEMA, "match": "maybe"}),
            json!({"schema": "leanctx.hive.vault-match/v0", "match": "none"}),
            json!({"schema": MATCH_SCHEMA, "match": "suggested",
                   "suggestion": {"ns": "acme", "name": "../etc"}}),
        ] {
            assert!(
                step_from_answer(answer(broken.clone())).is_err(),
                "{broken}"
            );
        }
    }

    #[test]
    fn only_a_missing_route_means_no_match() {
        assert!(route_missing(&HiveError::NotFound));
        assert!(route_missing(&HiveError::Server {
            status: 404,
            code: "unknown".into()
        }));
        assert!(route_missing(&HiveError::Server {
            status: 405,
            code: "unknown".into()
        }));
        assert!(!route_missing(&HiveError::Forbidden));
        assert!(!route_missing(&HiveError::Offline("x".into())));
        assert!(!route_missing(&HiveError::Server {
            status: 500,
            code: "internal".into()
        }));
    }

    #[test]
    fn the_match_route_is_asked_on_c0_2_hivees_and_a_404_rests_for_a_day() {
        let dir = tempfile::tempdir().unwrap();
        let store = HiveStore::at(dir.path().join("config"), dir.path().join("data"));
        let now = 1_800_000_000;
        assert!(!match_available(&store, now), "1.x: never asked");
        super::super::bus::relay_v2::record_protocol(&store, true).unwrap();
        assert!(match_available(&store, now));
        remember_missing(&store, now);
        assert!(!match_available(&store, now + 60));
        assert!(match_available(&store, now + REPROBE_SECONDS));
        assert!(
            match_available(&store, now - 60),
            "a clock that moved back asks again"
        );
        store.forget(true).unwrap();
        assert!(!store.config_dir().join(MATCH_FILE).exists());
    }

    #[test]
    fn the_consent_subject_names_the_vault_and_stays_on_shape() {
        let base = tempfile::tempdir().unwrap();
        let root = base.path().join("Payments Service");
        std::fs::create_dir_all(root.join(".git")).unwrap();
        let stable =
            lean_ctx_protocol::hive::vault::stable_identity("git:github.com/acme/payments");
        let own = consent_subject(&Target::Own, &stable, &root);
        consent::check_subject(Kind::LinkVault, &own).unwrap();
        assert_eq!(own["vault"], "new/payments-service");
        assert_eq!(own["bound_repo"], "Payments Service");
        let bind = consent_subject(
            &Target::Bind(VaultRefV1 {
                ns: "acme".into(),
                name: "payments".into(),
            }),
            &stable,
            &root,
        );
        consent::check_subject(Kind::LinkVault, &bind).unwrap();
        assert_eq!(bind["vault"], "acme/payments");
        assert_ne!(
            consent::subject_digest(&own).unwrap(),
            consent::subject_digest(&bind).unwrap(),
            "a yes for one target never covers another"
        );
        assert_eq!(new_vault_name(Path::new("/w/x")), "project");
        assert_eq!(new_vault_name(Path::new("/w/Über_App")), "ber-app");
        assert_eq!(new_vault_name(&base.path().join("a".repeat(60))).len(), 40);
    }

    /// E2E-FINAL: a second engine asked for a first share before the
    /// vault existed; once its checkout maps (another engine created the
    /// vault), that request asks for nothing and is closed, while a move
    /// into another vault keeps waiting.
    #[test]
    fn a_first_share_for_a_mapped_checkout_is_closed_a_move_keeps_waiting() {
        let dir = tempfile::tempdir().unwrap();
        let store = HiveStore::at(dir.path().join("config"), dir.path().join("data"));
        let broker = Broker::open(
            &store,
            Trust {
                presence: None,
                consent_keys: std::collections::BTreeMap::new(),
                owner_user_id: None,
            },
        );
        let root = dir.path().join("journey-demo");
        std::fs::create_dir_all(root.join(".git")).unwrap();
        let stable =
            lean_ctx_protocol::hive::vault::stable_identity("git:github.com/acme/journey-demo");
        let pending = |target: &Target| match broker.require(
            Kind::LinkVault,
            &consent_subject(target, &stable, &root),
            1_792_800_000_000,
        ) {
            Err(ConsentError::Required { consent_id, .. }) => broker.get(&consent_id).unwrap(),
            other => panic!("{other:?}"),
        };
        let own = pending(&Target::Own);
        let team = pending(&Target::Bind(VaultRefV1 {
            ns: "acme-team".into(),
            name: "journey-demo".into(),
        }));
        let mut map = super::super::vault::VaultMap::default();
        assert!(!satisfied(&own, &map), "not mapped yet: it waits");
        map.vaults.insert(
            stable.clone(),
            VaultRecord {
                vault_id: "vlt_1".into(),
                ns: "ada".into(),
                name: "journey-demo".into(),
                sealed: false,
                roots: vec![root.to_string_lossy().into_owned()],
            },
        );
        assert!(satisfied(&own, &map), "mapped: nothing to answer");
        assert!(!satisfied(&team, &map), "a move into another vault waits");
        let mapped = map.vaults[&stable].clone();
        let closed = broker
            .close_satisfied(&|r| {
                r.subject["link_digest"] == stable.as_str() && satisfied_by(r, &mapped)
            })
            .unwrap();
        assert_eq!(closed, vec![own.consent_id.clone()]);
        assert!(broker.get(&own.consent_id).is_err(), "closed");
        assert_eq!(
            broker.get(&team.consent_id).unwrap().state,
            consent::State::Pending
        );
    }

    #[test]
    fn consent_errors_keep_their_code_for_the_outbox_and_hide_the_id() {
        let error = MapError::Consent(ConsentError::Required {
            consent_id: "cns_aaaaaaaaaaaaaaaa".into(),
            kind: Kind::LinkVault,
        });
        let shown = error.to_string();
        assert!(shown.starts_with("consent_required"), "{shown}");
        assert!(
            !shown.contains("cns_"),
            "the id is never handed to an agent"
        );
        assert!(matches!(HiveError::from(error), HiveError::Refused(_)));
    }
}
