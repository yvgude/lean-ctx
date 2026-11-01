// SPDX-License-Identifier: Apache-2.0
//! Typed session projections at persistence and delivery boundaries.
//!
//! Session state is inspected before formatting, exporting, or deriving a
//! compaction summary. Records that contain withheld fields are omitted as a
//! whole; project identity and path fields are preserved exactly or the
//! projection fails closed.

use std::path::Path;

use serde_json::Value;

use super::{EngineSessionId, SessionState, parse_engine_session_id};
use crate::core::context_admission::{
    OutputCompleteness, admit_tool_output, stores::StoreAdmission,
};
use crate::core::policy::runtime::PublicationAuthority;

const CONTENT_FIELDS: &[&str] = &[
    "task",
    "findings",
    "decisions",
    "handoff_context",
    "files_touched",
    "test_results",
    "progress",
    "next_steps",
    "evidence",
    "intents",
    "active_structured_intent",
    "playbook",
    "last_semantic_query",
];

/// Run an operation under the active policy for the trusted session project.
/// A request-bound project remains authoritative and must match the session.
pub(crate) fn with_session_policy<T>(
    session: &SessionState,
    operation: impl FnOnce() -> T,
) -> Result<T, String> {
    let request_root = request_project_root()?;
    let root = policy_root(session, request_root.as_deref())?;
    crate::core::policy::runtime::with_project_source_view(&root, operation)
}

/// The authority one storage admission ran under: the Gateway policy snapshot
/// (digest and epoch), the project policy snapshot with the project identity
/// and role it was resolved for, and the request project that was bound.
///
/// It travels with the admitted bytes and is verified again at the commit, so
/// content admitted under an authority that no longer holds is not published.
pub(crate) struct StorageAuthority {
    store: StoreAdmission,
    publication: PublicationAuthority,
    request_root: Option<String>,
}

impl StorageAuthority {
    /// Fails when the Gateway configuration, the project policy inputs, the
    /// role, the project root itself or the bound request project differ from
    /// the ones the content was admitted under. Reads current bytes; a
    /// refresh that cannot complete is a failure, never a grant.
    pub(crate) fn verify(&self) -> Result<(), String> {
        if request_project_root()? != self.request_root {
            return Err("session request project changed before publication".into());
        }
        // The project comparison above is spelling-independent; the publication
        // authority re-binds its own canonical project for the fresh snapshot.
        crate::core::policy::runtime::REQUEST_PROJECT
            .sync_scope(std::cell::RefCell::new(None), || {
                self.store.authorize_publication(&self.publication)
            })
    }
}

#[cfg(test)]
thread_local! {
    static STORAGE_ADMISSION_PROBE: std::cell::RefCell<Option<Box<dyn Fn()>>> =
        const { std::cell::RefCell::new(None) };
}

/// Test seam: runs at the start of every storage admission on this thread,
/// before any configuration or policy input is read. It stands in for the
/// policy refresh, so a test can observe what the caller still holds then.
#[cfg(test)]
pub(crate) struct StorageAdmissionProbe;

#[cfg(test)]
impl StorageAdmissionProbe {
    pub(crate) fn install(probe: impl Fn() + 'static) -> Self {
        STORAGE_ADMISSION_PROBE.with(|slot| *slot.borrow_mut() = Some(Box::new(probe)));
        Self
    }
}

#[cfg(test)]
impl Drop for StorageAdmissionProbe {
    fn drop(&mut self) {
        STORAGE_ADMISSION_PROBE.with(|slot| *slot.borrow_mut() = None);
    }
}

/// Re-admit every persisted session text field under one current Gateway
/// snapshot and the active project policy. This runs on a detached clone.
pub(crate) fn for_storage(session: &SessionState) -> Result<SessionState, String> {
    for_storage_with_authority(session).map(|(projected, _)| projected)
}

/// [`for_storage`], returning the authority the projection was admitted under.
/// A caller that publishes the projection later verifies that authority again
/// at its commit. Reads configuration and policy inputs: never call this
/// while holding the live session lock.
pub(crate) fn for_storage_with_authority(
    session: &SessionState,
) -> Result<(SessionState, StorageAuthority), String> {
    #[cfg(test)]
    STORAGE_ADMISSION_PROBE.with(|slot| {
        if let Some(probe) = slot.borrow().as_ref() {
            probe();
        }
    });
    let store = StoreAdmission::current();
    let request_root = request_project_root()?;
    let (projected, publication) = with_session_policy(
        session,
        || -> Result<(SessionState, PublicationAuthority), String> {
            let mut value = serde_json::to_value(session)
                .map_err(|_| "session projection unavailable".to_string())?;
            let floor = store.built_in_floor();
            project_session_value(
                &mut value,
                decided_once(|text, path| admit_for_storage_as(&store, text, path, true)),
                decided_once(|text, path| admit_for_storage_as(&floor, text, path, true)),
                |id| admit_engine_session_id(&store, id),
            )?;
            let projected = restore_runtime_fields(session, value)?;
            if !checkpoint_projection_safe(
                session.canonical_checkpoint.is_some(),
                session,
                &projected,
            ) {
                return Err(
                    "canonical session projection would alter checkpoint-bound content".into(),
                );
            }
            if !store.is_current() {
                return Err("session admission policy changed; retry".into());
            }
            let publication = PublicationAuthority::capture()?;
            Ok((projected, publication))
        },
    )??;
    Ok((
        projected,
        StorageAuthority {
            store,
            publication,
            request_root,
        },
    ))
}

/// Re-admit session text under the active project and Gateway policies before
/// any response formatter or structured exporter sees it.
pub(crate) fn for_delivery(session: &SessionState) -> Result<SessionState, String> {
    with_session_policy(session, || {
        let mut value = serde_json::to_value(session)
            .map_err(|_| "session projection unavailable".to_string())?;
        let store = StoreAdmission::current();
        let floor = store.built_in_floor();
        project_session_value(
            &mut value,
            |text, path| admit_for_delivery(&store, text, path),
            |text, path| admit_for_delivery(&floor, text, path),
            |id| admit_engine_session_id(&store, id),
        )?;
        if !store.is_current() {
            return Err("session admission policy changed; retry".into());
        }
        let projected = restore_runtime_fields(session, value)?;
        if session.canonical_checkpoint.is_some()
            && !checkpoint_fields_unchanged(session, &projected)
        {
            return Err("canonical session content withheld by current policy".into());
        }
        Ok(projected)
    })?
}

pub(crate) fn admit_tool_value(session: &SessionState, text: &str) -> Result<String, String> {
    with_session_policy(session, || {
        let admitted = admit_tool_output(text, OutputCompleteness::complete());
        admitted.text.ok_or_else(|| {
            admitted
                .notice
                .unwrap_or_else(|| "[lean-ctx gateway: content withheld]".to_string())
        })
    })?
}

/// Admit a pathless string before retaining it outside a session record.
pub(crate) fn admit_storable_tool_text(text: &str) -> Result<String, String> {
    let store = StoreAdmission::current();
    let request_root = request_project_root()?;
    let root = request_root
        .or_else(|| {
            std::env::current_dir()
                .ok()
                .and_then(|path| normalize_root(&path))
        })
        .ok_or_else(|| "session project scope unavailable".to_string())?;
    let admitted = crate::core::policy::runtime::with_project_source_view(&root, || {
        admit_for_storage(&store, text, None)
            .ok_or_else(|| "session content withheld by current policy".to_string())
    })??;
    if !store.is_current() {
        return Err("session admission policy changed; retry".into());
    }
    Ok(admitted)
}

/// Admit user-provided text against the trusted live session's project policy
/// and Gateway before it is retained in session state.
pub(crate) fn admit_storable_session_text(
    session: &SessionState,
    text: &str,
) -> Result<String, String> {
    with_store_admission(session, |store| {
        admit_store_text(store, text)
            .ok_or_else(|| "session content withheld by current policy".to_string())
    })
}

/// Resolve one Gateway storage policy for a detached session operation. The
/// project view is established before any load, migration, derivation, or save,
/// and the snapshot is rejected if its epoch changes before the operation ends.
pub(crate) fn with_store_admission<T>(
    session: &SessionState,
    operation: impl FnOnce(&StoreAdmission) -> Result<T, String>,
) -> Result<T, String> {
    let store = StoreAdmission::current();
    let value = with_session_policy(session, || operation(&store))??;
    if !store.is_current() {
        return Err("session admission policy changed; retry".into());
    }
    Ok(value)
}

/// Admit pathless content under a previously captured storage policy.
pub(crate) fn admit_store_text(store: &StoreAdmission, text: &str) -> Option<String> {
    admit_for_storage(store, text, None)
}

fn request_project_root() -> Result<Option<String>, String> {
    let request_root = crate::core::policy::runtime::REQUEST_PROJECT
        .try_with(|slot| slot.borrow().clone())
        .ok()
        .flatten();
    request_root
        .map(|root| {
            normalize_root(Path::new(&root))
                .ok_or_else(|| "request project scope unavailable".to_string())
        })
        .transpose()
}

fn policy_root(session: &SessionState, request_root: Option<&str>) -> Result<String, String> {
    if session.project_root.is_some() && session.strict_project_root().is_none() {
        return Err("session project scope unavailable".into());
    }
    if let Some(session_root) = session.strict_project_root() {
        if request_root.is_some_and(|request| request != session_root) {
            return Err("session project scope changed".into());
        }
        return Ok(session_root);
    }
    if let Some(request_root) = request_root {
        return Ok(request_root.to_owned());
    }
    if let Some(cwd) = session.shell_cwd.as_deref()
        && let Some(root) = normalize_root(Path::new(cwd))
    {
        return Ok(root);
    }
    std::env::current_dir()
        .ok()
        .and_then(|cwd| normalize_root(&cwd))
        .ok_or_else(|| "session project scope unavailable".to_string())
}

fn normalize_root(path: &Path) -> Option<String> {
    if !path.is_absolute() || crate::core::pathutil::is_broad_or_unsafe_root(path) {
        return None;
    }
    let root = crate::core::pathutil::safe_canonicalize_or_self(path);
    if !root.is_absolute() || crate::core::pathutil::is_broad_or_unsafe_root(&root) {
        return None;
    }
    Some(root.to_string_lossy().into_owned())
}

/// One decision per distinct `(text, path)` within ONE projection.
///
/// A projection admits every string of the session under one policy snapshot,
/// in one pass; the same string under the same snapshot is the same question.
/// A session repeats most of its strings (every evidence record carries the
/// tool, the client and the agent again), and with a semantic detector bound
/// an admission is a model request that no memo may answer. Asked once per
/// occurrence, a save held the session's save lane for longer than its
/// waiters wait, and a shutdown outlived the host's patience.
///
/// The decisions live for this projection only: nothing is carried to the
/// next save, to another snapshot or to another caller. Used by the storage
/// projection of a save; the delivery projection, which runs inside a tool
/// call and records every admission in that call's receipt, is unchanged.
fn decided_once(
    mut admit: impl FnMut(&str, Option<&Path>) -> Option<String>,
) -> impl FnMut(&str, Option<&Path>) -> Option<String> {
    let mut decided =
        std::collections::HashMap::<(String, Option<std::path::PathBuf>), Option<String>>::new();
    move |text, path| {
        decided
            .entry((text.to_owned(), path.map(Path::to_path_buf)))
            .or_insert_with(|| admit(text, path))
            .clone()
    }
}

fn admit_for_storage(store: &StoreAdmission, text: &str, path: Option<&Path>) -> Option<String> {
    admit_for_storage_as(store, text, path, false)
}

/// [`admit_for_storage`]. With `same_question_once` the Gateway is not asked
/// a second time about a text the project policy left exactly as it is.
fn admit_for_storage_as(
    store: &StoreAdmission,
    text: &str,
    path: Option<&Path>,
    same_question_once: bool,
) -> Option<String> {
    // Both policies inspect original text before either is allowed to mask it.
    let project_original = crate::core::policy::content::protect_active(text).ok()?;
    let gateway_original = match path {
        Some(path) => store.admit(text, path)?,
        None => store.admit_text(text)?,
    };
    let project_after_gateway = crate::core::policy::content::protect_active(&gateway_original)
        .ok()?
        .into_owned();
    // The other order: the Gateway on what the project policy left. Where
    // the project policy left the text as it is, that is the admission just
    // made — the same text under the same snapshot. A save's projection
    // (`same_question_once`) does not ask it a second time; the comparison
    // below is unchanged either way.
    let gateway_after_project = if same_question_once && project_original.as_ref() == text {
        gateway_original
    } else {
        match path {
            Some(path) => store.admit(project_original.as_ref(), path)?,
            None => store.admit_text(project_original.as_ref())?,
        }
    };
    (project_after_gateway == gateway_after_project).then_some(project_after_gateway)
}

fn admit_engine_session_id(store: &StoreAdmission, id: EngineSessionId<'_>) -> Option<String> {
    let text = id.as_str();
    let project_output = crate::core::policy::content::protect_active_engine_identity(id).ok()?;
    if project_output.as_ref() != text {
        return None;
    }
    store.admit_engine_session_id(id)
}

fn admit_for_delivery(store: &StoreAdmission, text: &str, path: Option<&Path>) -> Option<String> {
    if path.is_some_and(|path| crate::core::io_boundary::is_secret_like(path).is_some()) {
        return None;
    }
    // Structured session projections cannot carry a per-field warning notice.
    // Use the same composed snapshot as durable storage, which withholds
    // Restricted content even when a developer tool would merely warn.
    let admitted = admit_for_storage(store, text, path)?;
    if path.is_some() && admitted != text {
        return None;
    }
    Some(admitted)
}

/// Project user/content-bearing fields; identity strings must remain exact,
/// while timestamps, counts, and enum tags remain structural metadata.
/// `admit_exact` decides the strings that must stay exact (identifiers,
/// configuration tags, paths): the built-in floor without the advisory
/// semantic stage, whose finding there could only withhold the record.
fn project_session_value(
    value: &mut Value,
    mut admit: impl FnMut(&str, Option<&Path>) -> Option<String>,
    mut admit_exact: impl FnMut(&str, Option<&Path>) -> Option<String>,
    mut admit_engine_id: impl for<'a> FnMut(EngineSessionId<'a>) -> Option<String>,
) -> Result<(), String> {
    let object = value
        .as_object_mut()
        .ok_or_else(|| "session projection unavailable".to_string())?;

    for key in ["id", "compression_level", "last_aaak_hash"] {
        if let Some(Value::String(original)) = object.get(key) {
            let admitted = if key == "id" {
                match parse_engine_session_id(original) {
                    Some(id) => admit_engine_id(id),
                    None => admit_exact(original, None),
                }
            } else {
                admit_exact(original, None)
            };
            if admitted.as_deref() != Some(original.as_str()) {
                return Err("session identity or configuration withheld by current policy".into());
            }
        }
    }

    for key in ["project_root", "shell_cwd"] {
        if let Some(Value::String(path)) = object.get(key) {
            let Some(admitted) = admit_exact(path, Some(Path::new(path))) else {
                return Err("session identity withheld by current policy".into());
            };
            if admitted != *path {
                return Err("session identity would be rewritten by current policy".into());
            }
        }
    }
    if let Some(Value::Array(roots)) = object.get("extra_roots") {
        for root in roots {
            let Some(path) = root.as_str() else {
                return Err("session identity unavailable".into());
            };
            let Some(admitted) = admit_exact(path, Some(Path::new(path))) else {
                return Err("session identity withheld by current policy".into());
            };
            if admitted != path {
                return Err("session identity would be rewritten by current policy".into());
            }
        }
    }

    for key in CONTENT_FIELDS {
        let Some(field) = object.get_mut(*key) else {
            continue;
        };
        if project_node(field, None, &mut admit, &mut admit_exact)? {
            object.remove(*key);
        }
    }
    Ok(())
}

/// Returns true when a record contains a field that must be withheld.
fn project_node(
    value: &mut Value,
    field_name: Option<&str>,
    admit: &mut impl FnMut(&str, Option<&Path>) -> Option<String>,
    admit_exact: &mut impl FnMut(&str, Option<&Path>) -> Option<String>,
) -> Result<bool, String> {
    match value {
        Value::String(text) => {
            if is_timestamp_string(field_name) {
                return Ok(false);
            }
            let path = is_path_field(field_name).then(|| Path::new(text.as_str()));
            let exact = path.is_some() || is_identity_string(field_name);
            let admitted = if exact {
                admit_exact(text, path)
            } else {
                admit(text, path)
            };
            let Some(admitted) = admitted else {
                return Ok(true);
            };
            if exact && admitted != *text {
                return Ok(true);
            }
            *text = admitted;
            Ok(false)
        }
        Value::Array(items) => {
            let mut kept = Vec::with_capacity(items.len());
            for mut item in std::mem::take(items) {
                if project_node(&mut item, field_name, admit, admit_exact)? {
                    // Required text withheld: omit the complete record/value.
                    continue;
                }
                kept.push(item);
            }
            *items = kept;
            Ok(false)
        }
        Value::Object(fields) => {
            let mut withheld = false;
            let keys: Vec<String> = fields.keys().cloned().collect();
            for key in keys {
                if is_timestamp_string(Some(&key)) {
                    continue;
                }
                let child_withheld = if let Some(child) = fields.get_mut(&key) {
                    project_node(child, Some(&key), admit, admit_exact)?
                } else {
                    false
                };
                if child_withheld {
                    fields.remove(&key);
                    withheld = true;
                }
            }
            Ok(withheld)
        }
        _ => Ok(false),
    }
}

fn is_path_field(field: Option<&str>) -> bool {
    matches!(
        field,
        Some("path" | "file" | "project_root" | "shell_cwd" | "extra_roots")
    ) || field.is_some_and(|name| name.ends_with("_path"))
}

/// DateTime fields have already passed serde's typed parser; preserving their
/// canonical values keeps the session schema intact.
fn is_timestamp_string(field: Option<&str>) -> bool {
    field.is_some_and(|name| {
        matches!(
            name,
            "timestamp"
                | "created_at"
                | "updated_at"
                | "started_at"
                | "last_confirmed"
                | "last_retrieved"
                | "last_feedback"
                | "valid_from"
                | "valid_until"
        )
    })
}

/// These strings carry source identity or typed structure. Inspect them like
/// any other input, but omit their containing record if policy would rewrite it.
fn is_identity_string(field: Option<&str>) -> bool {
    field.is_some_and(|name| {
        name.ends_with("_id")
            || name.ends_with("_md5")
            || matches!(
                name,
                "id" | "source_session"
                    | "task_id"
                    | "agent_id"
                    | "client_name"
                    | "tool"
                    | "kind"
                    | "intent_type"
                    | "source"
                    | "position"
                    | "profile"
                    | "last_mode"
                    | "privacy"
                    | "sensitivity"
                    | "archetype"
            )
    })
}

fn restore_runtime_fields(original: &SessionState, value: Value) -> Result<SessionState, String> {
    let mut projected: SessionState =
        serde_json::from_value(value).map_err(|_| "session projection unavailable".to_string())?;
    projected
        .canonical_checkpoint
        .clone_from(&original.canonical_checkpoint);
    projected.save_gate = original.save_gate.clone();
    projected.last_save_failed = original.last_save_failed;
    projected.live_zone = original.live_zone.clone();
    projected.last_flush = original.last_flush;
    Ok(projected)
}

fn checkpoint_fields_unchanged(before: &SessionState, after: &SessionState) -> bool {
    before.project_root == after.project_root
        && match (&before.task, &after.task) {
            (Some(before), Some(after)) => {
                before.description == after.description && before.progress_pct == after.progress_pct
            }
            (None, None) => true,
            _ => false,
        }
        && before.findings.len() == after.findings.len()
        && before
            .findings
            .iter()
            .zip(&after.findings)
            .all(|(before, after)| before.summary == after.summary)
        && before.decisions.len() == after.decisions.len()
        && before
            .decisions
            .iter()
            .zip(&after.decisions)
            .all(|(before, after)| {
                before.summary == after.summary && before.rationale == after.rationale
            })
        && before.next_steps == after.next_steps
}

fn checkpoint_projection_safe(
    has_checkpoint: bool,
    before: &SessionState,
    after: &SessionState,
) -> bool {
    !has_checkpoint || checkpoint_fields_unchanged(before, after)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::config::SecretDetectionConfig;
    use crate::core::context_admission::{AdmissionPolicy, ContextGatewayConfig};
    use crate::core::input_filters::FilterAction;

    fn store_policy(classification: FilterAction) -> StoreAdmission {
        let gateway = ContextGatewayConfig {
            classification,
            ..ContextGatewayConfig::default()
        };
        StoreAdmission::with_policy(AdmissionPolicy::builtin(
            gateway,
            SecretDetectionConfig::default(),
        ))
    }

    // Formerly `legacy_generated_id_survives_default_gateway_only_as_typed_metadata`.
    #[test]
    fn legacy_generated_id_projects_exactly_and_free_form_ids_stay_under_content_policy() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let project = tempfile::tempdir().expect("project root");
        let _policy = crate::core::policy::runtime::TestPolicyOverride::set(None);
        let legacy_id = "20261004-123406-123456p43210s0";
        let store = store_policy(FilterAction::Redact);
        // The standard Luhn-valid test number in groups of four, assembled at
        // run time so the source carries no card-shaped literal.
        let group = "1".repeat(4);
        let card = format!("4{} {group} {group} {group}", "1".repeat(3));
        assert_ne!(
            store.admit_text(&card).as_deref(),
            Some(card.as_str()),
            "the Gateway must still redact a card number in free text"
        );
        // Changed expectation: this asserted that the Gateway masks the id
        // itself in free text. Since the 3.11 detector rule
        // (`pii::card_layout_plausible`: a separated digit run is a card only
        // in an issuer layout) its `YYYYmmdd-HHMMSS` run is no card, so the id
        // passes free-text admission as well.
        assert_eq!(store.admit_text(legacy_id).as_deref(), Some(legacy_id));

        let mut session = SessionState::new();
        session.id = legacy_id.to_string();
        session.project_root = Some(project.path().to_string_lossy().into_owned());
        session.shell_cwd = session.project_root.clone();
        let projected = with_session_policy(&session, || {
            let mut value = serde_json::to_value(&session).expect("serialize");
            project_session_value(
                &mut value,
                |text, path| admit_for_storage(&store, text, path),
                |text, path| admit_for_storage(&store, text, path),
                |id| admit_engine_session_id(&store, id),
            )
            .expect("strict legacy ID is typed metadata");
            restore_runtime_fields(&session, value).expect("typed projection")
        })
        .expect("project scope");
        assert_eq!(projected.id, legacy_id);

        // Changed sample (same rule): a free-form id is refused for what the
        // content policy masks in it, here the card number.
        session.id = format!("caller supplied {card}");
        let rejected = with_session_policy(&session, || {
            let mut value = serde_json::to_value(&session).expect("serialize");
            project_session_value(
                &mut value,
                |text, path| admit_for_storage(&store, text, path),
                |text, path| admit_for_storage(&store, text, path),
                |id| admit_engine_session_id(&store, id),
            )
        })
        .expect("project scope");
        assert!(rejected.is_err(), "free-form IDs stay under content policy");
    }

    /// A semantic model reads a user name in a project path as a person.
    /// Identity strings must stay exact, so before every save of a session
    /// failed ("session identity or configuration withheld"); now identifiers
    /// and paths are decided by the built-in floor and content keeps the model.
    #[test]
    fn a_semantic_finding_in_an_identity_masks_content_but_never_fails_the_save() {
        use crate::core::context_admission::semantic::SemanticDetector;
        use crate::core::intelligence_runtime::semantic_detectors::{
            SemanticCategory, SemanticFinding, SemanticReport,
        };
        // Everything is personal data to this model.
        fn everything(text: &str) -> crate::core::intelligence_runtime::Result<SemanticReport> {
            Ok(SemanticReport {
                bytes_inspected: text.len(),
                findings: vec![SemanticFinding {
                    category: SemanticCategory::Pii,
                    start: 0,
                    end: text.len(),
                    confidence_milli: 900,
                }],
            })
        }
        let _data = crate::core::data_dir::isolated_data_dir();
        let project = tempfile::tempdir().expect("project root");
        let _policy = crate::core::policy::runtime::TestPolicyOverride::set(None);
        let mut policy = AdmissionPolicy::builtin(
            ContextGatewayConfig::default(),
            SecretDetectionConfig::default(),
        );
        policy.semantic = Some(SemanticDetector::fixed(everything));
        let store = StoreAdmission::with_policy(policy);
        let floor = store.built_in_floor();
        let mut session = SessionState::new();
        session.project_root = Some(project.path().to_string_lossy().into_owned());
        session.shell_cwd = session.project_root.clone();
        session.last_aaak_hash = Some("9f2c4e".into());
        session.next_steps = vec!["call Anna Meier".into()];
        let projected = with_session_policy(&session, || {
            let mut value = serde_json::to_value(&session).expect("serialize");
            project_session_value(
                &mut value,
                |text, path| admit_for_storage(&store, text, path),
                |text, path| admit_for_storage(&floor, text, path),
                |id| admit_engine_session_id(&store, id),
            )
            .expect("identities pass the built-in floor");
            restore_runtime_fields(&session, value).expect("typed projection")
        })
        .expect("project scope");
        assert_eq!(projected.project_root, session.project_root);
        assert_eq!(projected.last_aaak_hash, session.last_aaak_hash);
        assert_eq!(projected.next_steps, ["[REDACTED:pii]"]);
    }

    /// A save's projection asks about each distinct string once, and what it
    /// projects is exactly what asking for every occurrence projects. Six
    /// evidence records of one tool repeat their tool, key, client and agent:
    /// asked per occurrence that was the request count that kept a save in
    /// its lane beyond its waiters' deadline.
    #[test]
    fn a_projection_decides_each_distinct_text_once_and_projects_the_same() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let project = tempfile::tempdir().expect("project root");
        let _policy = crate::core::policy::runtime::TestPolicyOverride::set(None);
        let store = store_policy(FilterAction::Redact);
        let mut session = SessionState::new();
        session.project_root = Some(project.path().to_string_lossy().into_owned());
        session.shell_cwd = session.project_root.clone();
        session.next_steps = vec!["review the release notes".into(); 3];
        let group = "1".repeat(4);
        let card = format!("4{} {group} {group} {group}", "1".repeat(3));
        for call in 0..6 {
            session.findings.push(super::super::Finding {
                file: Some("notes/release.md".into()),
                line: Some(call),
                summary: format!("pay with {card}"),
                timestamp: chrono::Utc::now(),
            });
        }

        let project_with = |admit: &mut dyn FnMut(&str, Option<&Path>) -> Option<String>| {
            with_session_policy(&session, || {
                let mut value = serde_json::to_value(&session).expect("serialize");
                // Content and exact strings are asked through the same counter.
                let admit = std::cell::RefCell::new(admit);
                project_session_value(
                    &mut value,
                    |text, path| (*admit.borrow_mut())(text, path),
                    |text, path| (*admit.borrow_mut())(text, path),
                    |id| admit_engine_session_id(&store, id),
                )
                .expect("projection");
                value
            })
            .expect("project scope")
        };

        let mut asked_per_occurrence = Vec::new();
        let every_occurrence = project_with(&mut |text, path| {
            asked_per_occurrence.push((text.to_owned(), path.map(Path::to_path_buf)));
            admit_for_storage(&store, text, path)
        });
        let mut asked_once = Vec::new();
        let once = project_with(&mut decided_once(|text, path| {
            asked_once.push((text.to_owned(), path.map(Path::to_path_buf)));
            admit_for_storage_as(&store, text, path, true)
        }));

        assert_eq!(
            once, every_occurrence,
            "the same projection, masks included"
        );
        let distinct: std::collections::HashSet<_> = asked_per_occurrence.iter().collect();
        assert_eq!(
            asked_once.len(),
            distinct.len(),
            "one decision per distinct text"
        );
        assert!(
            asked_once.len() < asked_per_occurrence.len(),
            "the fixture repeats strings: {} of {}",
            asked_once.len(),
            asked_per_occurrence.len()
        );
        // What was to be masked is masked in both.
        assert!(!once.to_string().contains(&card));
    }

    #[test]
    fn storage_projection_omits_restricted_records_and_keeps_session_scope() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let project = tempfile::tempdir().expect("project root");
        let _policy = crate::core::policy::runtime::TestPolicyOverride::set(None);
        let mut session = SessionState::new();
        session.project_root = Some(project.path().to_string_lossy().into_owned());
        session.shell_cwd = session.project_root.clone();
        session.findings.push(super::super::Finding {
            file: Some("src/lib.rs".into()),
            line: Some(7),
            summary: "TOP SECRET\nfinding".into(),
            timestamp: chrono::Utc::now(),
        });

        let store = store_policy(FilterAction::Warn);
        let projected = with_session_policy(&session, || {
            let mut value = serde_json::to_value(&session).expect("serialize");
            project_session_value(
                &mut value,
                |text, path| admit_for_storage(&store, text, path),
                |text, path| admit_for_storage(&store, text, path),
                |id| admit_engine_session_id(&store, id),
            )
            .expect("project");
            restore_runtime_fields(&session, value).expect("typed projection")
        })
        .expect("project scope");

        assert!(projected.findings.is_empty());
        assert_eq!(projected.id, session.id);
        assert_eq!(projected.project_root, session.project_root);
        assert_eq!(projected.shell_cwd, session.shell_cwd);
        let stored = serde_json::to_string(&projected).expect("serialize projection");
        assert!(!stored.contains("TOP SECRET"));
        assert!(!stored.contains("Finding withheld"));
    }

    #[test]
    fn checkpoint_projection_guard_preserves_identity_and_rejects_rewrite() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let project = tempfile::tempdir().expect("project root");
        let _policy = crate::core::policy::runtime::TestPolicyOverride::set(None);
        let mut session = SessionState::new();
        session.project_root = Some(project.path().to_string_lossy().into_owned());
        session.shell_cwd = session.project_root.clone();
        session.task = Some(super::super::TaskInfo {
            description: "safe task".into(),
            intent: None,
            progress_pct: Some(0),
        });
        let store = store_policy(FilterAction::Warn);
        let projected = with_session_policy(&session, || {
            let mut value = serde_json::to_value(&session).expect("serialize");
            project_session_value(
                &mut value,
                |text, path| admit_for_storage(&store, text, path),
                |text, path| admit_for_storage(&store, text, path),
                |id| admit_engine_session_id(&store, id),
            )
            .expect("project");
            restore_runtime_fields(&session, value).expect("typed projection")
        })
        .expect("project scope");

        assert!(checkpoint_projection_safe(true, &session, &projected));
        let mut rewritten = projected.clone();
        rewritten
            .task
            .as_mut()
            .expect("task")
            .description
            .push_str(" [redacted]");
        assert!(!checkpoint_projection_safe(true, &session, &rewritten));
        assert!(checkpoint_projection_safe(false, &session, &rewritten));
    }

    #[test]
    fn restricted_compact_summary_is_not_admitted_for_storage() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let project = tempfile::tempdir().expect("project root");
        let _policy = crate::core::policy::runtime::TestPolicyOverride::set(None);
        let store = store_policy(FilterAction::Warn);
        let admitted = crate::core::policy::runtime::with_project_source_view(
            &project.path().to_string_lossy(),
            || admit_for_storage(&store, "TOP SECRET\ncompact summary", None),
        )
        .expect("project scope");
        assert!(admitted.is_none());
    }

    #[test]
    fn rewritten_session_lineage_omits_its_containing_record() {
        let mut value = serde_json::json!({
            "source_session": "session-123",
            "value": "safe content"
        });
        fn masking(text: &str, _: Option<&Path>) -> Option<String> {
            Some(if text == "session-123" {
                "masked-session".to_string()
            } else {
                text.to_string()
            })
        }
        let withheld =
            project_node(&mut value, None, &mut masking, &mut masking).expect("typed projection");
        assert!(withheld);
    }

    #[test]
    fn delivery_rechecks_changed_project_policy_before_recall() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let project = tempfile::tempdir().expect("project root");
        let mut session = SessionState::new();
        session.project_root = Some(project.path().to_string_lossy().into_owned());
        session.shell_cwd = session.project_root.clone();
        session.next_steps.push("publish CUSTOMER-482193".into());

        let allow = crate::core::policy::parse(
            "name = \"session-policy\"\nversion = \"1.0.0\"\ndescription = \"test\"",
        )
        .and_then(|pack| crate::core::policy::resolve(&pack))
        .expect("allow policy");
        let _allow = crate::core::policy::runtime::TestPolicyOverride::set(Some(allow));
        assert!(
            for_delivery(&session)
                .expect("initial delivery")
                .next_steps
                .iter()
                .any(|step| step.contains("CUSTOMER-482193"))
        );
        assert!(
            for_storage(&session)
                .expect("initial storage projection")
                .next_steps
                .iter()
                .any(|step| step.contains("CUSTOMER-482193"))
        );
        drop(_allow);

        let block = crate::core::policy::parse(
            "name = \"session-policy\"\nversion = \"1.0.0\"\ndescription = \"test\"\n[filters.blocked_patterns]\ncustomer = 'CUSTOMER-482193'",
        )
        .and_then(|pack| crate::core::policy::resolve(&pack))
        .expect("block policy");
        let _block = crate::core::policy::runtime::TestPolicyOverride::set(Some(block));
        let projected = for_delivery(&session).expect("changed policy delivery");
        assert!(projected.next_steps.is_empty());
        let stored = for_storage(&session).expect("changed policy storage");
        assert!(stored.next_steps.is_empty());
    }
}
