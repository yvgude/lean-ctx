// SPDX-License-Identifier: Apache-2.0
//! Current project content admission for immutable signed transfer evidence.
//!
//! A rewrite cannot preserve the source signature/digests. Refuse that package
//! and require a newly prepared safe checkpoint; never silently rewrite history.
//! This is content admission only, not current access to its original sources.
//!
//! Three gates, in this order:
//!
//! 1. the project policy, over every decoded value and the whole document;
//! 2. the secret floor, over every string;
//! 3. the Context Gateway's source admission of every SOURCE string of the
//!    document, in every mode, inside the call's open capture — so the
//!    gateway receipt of the call carries one decision per text, named by
//!    its content-free field path.
//!
//! What is a source is decided by the document's table of STRUCTURAL field
//! paths ([`Document`]), never by the shape of a value: a path on the table
//! is structural because this code verifies, recomputes or parses it into
//! its typed form; every other string — whatever it looks like — is a
//! source.
//!
//! Two cases of gate 3:
//!
//! - a document that is ALREADY BOUND by a digest or signature (a
//!   transferred package, a received personal copy, a stored envelope): a
//!   change the gateway would make is unrepresentable, so the document is
//!   refused ([`admit_bound`]);
//! - a document this host is PRODUCING and has not bound yet (a personal
//!   snapshot, a checkpoint about to be signed): its sources are admitted
//!   first and the ADMITTED text is what gets digested and signed
//!   ([`admit_fresh`]) — a masked value is masked in the document, exactly
//!   as any other delivery of that text would show it.

use std::cell::RefCell;
use std::collections::HashSet;
use std::path::Path;
use std::sync::{Arc, Weak};

use anyhow::{Result, ensure};
use serde_json::Value;

use super::CheckpointTransferPackageV1;
use crate::core::context_admission::capture::{self, AdmissionCapture};
use crate::core::policy::{content, diagnostics, runtime};

/// Whose content an already bound document carries.
#[derive(Debug, Clone, Copy)]
pub(in crate::core::execution_ledger::host) enum Origin {
    /// This host's own, bound earlier (a stored signed envelope and the
    /// digest-addressed evidence packaged with it).
    Local,
    /// Another host's: foreign content entering this project. It is admitted
    /// as a remote source under the tool label of the operation, before it
    /// is written to any local store or returned.
    Foreign(&'static str),
}

/// The refusal of a document one of whose sources the Context Gateway
/// withholds. Content-free; the sibling of the secret floor's refusal.
const GATEWAY_WITHHELD: &str = "checkpoint content withheld by the context gateway";
/// The refusal of a BOUND document one of whose sources the gateway would
/// mask: the document cannot carry the mask. Content-free.
const REWRITE_UNREPRESENTABLE: &str =
    "checkpoint content refused: a context gateway rewrite is unrepresentable in a bound document";

/// Levels of JSON documents serialized inside a string that are decoded.
const MAX_EMBEDDED_DOCUMENTS: u8 = 4;

/// Structural string fields of a checkpoint (V1, V2, V3), `[]` for any
/// index. Each is here because this code verifies or types it:
///
/// - `lineage.project_id`, `.tenant_id`, `.task_id` and the whole
///   `lineage.artifact_lineage` are compared with the verified task, plan
///   and receipts (`context_checkpoint/lineage.rs`, `validate_artifact_lineage`);
/// - ids and references are parsed by `validate_checkpoint_identifier` /
///   `validate_checkpoint_reference` in the `validate` of their struct
///   (task, decision, file, policy pin, package pin, session identity,
///   encryption metadata; reference lists by `validate_reference_list`);
/// - `status`, `role`, `algorithm` are closed enums; versions parse as
///   `SemanticVersion`, timestamps as `UtcTimestamp`.
///
/// NOT here, and therefore sources: every text field, `identity.*`,
/// `lineage.workspace_id`, `.plan_id`, `.receipt_ids`, every digest that is
/// only carried (`context_ir_digest`, `hosted_index_digest`, file, pin,
/// learning-state and ciphertext digests), `profile_id`, `carrier`,
/// `learning_state` and everything under `session_state.state`.
const CHECKPOINT: &[&str] = &[
    "lineage.project_id",
    "lineage.tenant_id",
    "lineage.task_id",
    "lineage.artifact_lineage.task_ref",
    "lineage.artifact_lineage.plan_ref",
    "lineage.artifact_lineage.receipt_refs[]",
    "lineage.evidence_refs[]",
    "lineage.knowledge_refs[]",
    "lineage.gotcha_refs[]",
    "lineage.snapshot_refs[]",
    "live_state.task.task_id",
    "live_state.task.status",
    "live_state.task.plan_id",
    "live_state.decisions[].decision_id",
    "live_state.decisions[].status",
    "live_state.decisions[].evidence_refs[]",
    "live_state.files[].source_id",
    "live_state.files[].role",
    "live_state.policy_pins[].policy_id",
    "live_state.package_pins[].package_id",
    "live_state.package_pins[].version",
    "live_state.session_state.identity.session_id",
    "live_state.session_state.identity.task_id",
    "engine_version",
    "created_at",
    "updated_at",
    "encryption_metadata.algorithm",
    "encryption_metadata.key_ref",
];

/// The one subtree of a checkpoint whose object keys are not known field
/// names of a struct this code parses.
const CHECKPOINT_FREE_KEYS: &str = "live_state.session_state.state";

/// Structural string fields of a signed envelope besides its `checkpoint`:
/// the schema is matched, the signer compared with the admitted or pinned
/// signer, the signature verified over the checkpoint's bytes
/// (`admit_local_envelope`, `admit_pinned_envelope`, `resume_checkpoint`).
const ENVELOPE: &[&str] = &[
    "schema_version",
    "signer.key_id",
    "signer.public_key_digest",
    "signature",
];

/// Structural string fields of a transfer package: schemas are matched,
/// `artifact_digest` and every entry digest recomputed, the signer compared
/// with the pin (or taken from this host's own admission), the lineage ids
/// compared with the verified checkpoint. `checkpoint_digest` and
/// `lineage.current_receipt_digest` are not on it.
const PACKAGE: &[&str] = &[
    "schema_version",
    "envelope_schema",
    "artifact_digest",
    "source_signer.key_id",
    "source_signer.public_key_digest",
    "source_signer.admitted_at",
    "source_signer.expires_at",
    "source_signer.revoked_at",
    "lineage.task_id",
    "lineage.project_id",
    "lineage.tenant_id",
    "lineage.workspace_id",
    "receipts[].digest",
    "evidence[].digest",
];

/// Which document a value is: its table of structural field paths.
#[derive(Debug, Clone, Copy)]
pub(in crate::core::execution_ledger::host) enum Document {
    Checkpoint,
    Envelope,
    Package,
    /// A document without a table — a receipt, plan, outcome or evidence
    /// document of a package, a JSON document serialized inside a string,
    /// another component's answer: EVERY string and every key is a source.
    Unlisted,
}

impl Document {
    /// Whether the string at `shape` (the field path with `[]` for indexes)
    /// is structural.
    fn structural(self, shape: &str) -> bool {
        match self {
            Self::Checkpoint => CHECKPOINT.contains(&shape),
            Self::Envelope => {
                ENVELOPE.contains(&shape)
                    || shape
                        .strip_prefix("checkpoint.")
                        .is_some_and(|inner| CHECKPOINT.contains(&inner))
            }
            Self::Package => PACKAGE.contains(&shape),
            Self::Unlisted => false,
        }
    }

    /// Whether the keys of the object at `shape` are field names of a struct
    /// this code parses the document into (structural by provenance), as
    /// opposed to keys someone wrote.
    fn typed_keys(self, shape: &str) -> bool {
        match self {
            Self::Checkpoint => !shape.starts_with(CHECKPOINT_FREE_KEYS),
            Self::Envelope => shape
                .strip_prefix("checkpoint.")
                .is_none_or(|inner| !inner.starts_with(CHECKPOINT_FREE_KEYS)),
            Self::Package => true,
            Self::Unlisted => false,
        }
    }
}

/// Where a walk stands: the content-free reference of the field (document
/// prefix and field path, with indexes) and its shape for the table.
struct Position {
    reference: String,
    shape: String,
}

/// Call `visit(reference, text)` for every SOURCE string of `value` — every
/// string whose path is not on `document`'s table, and every key that is
/// not a typed field name. A returned replacement is written into the
/// value; `Ok(true)`: something was replaced.
///
/// A source that is itself a serialized JSON object or array is an embedded
/// document: it is decoded and walked as [`Document::Unlisted`]
/// (`<path>#…`), its strings visited one by one — never the serialized
/// form — and re-serialized if one was replaced. `embedded` bounds that
/// nesting; beyond it the string is one source. A key cannot be replaced: a
/// replacement for one is a refusal.
fn sources(
    value: &mut Value,
    document: Document,
    at: &mut Position,
    embedded: u8,
    visit: &mut dyn FnMut(&str, &str) -> Result<Option<String>>,
) -> Result<bool> {
    let (reference, shape) = (at.reference.len(), at.shape.len());
    let mut changed = false;
    match value {
        Value::String(text) if !document.structural(&at.shape) => {
            let nested = if embedded < MAX_EMBEDDED_DOCUMENTS && text.starts_with(['{', '[']) {
                serde_json::from_str::<Value>(text).ok()
            } else {
                None
            };
            if let Some(mut nested) = nested {
                at.reference.push('#');
                let outer_shape = std::mem::take(&mut at.shape);
                let rewritten = sources(&mut nested, Document::Unlisted, at, embedded + 1, visit);
                at.shape = outer_shape;
                at.reference.truncate(reference);
                if rewritten? {
                    *text = nested.to_string();
                    changed = true;
                }
            } else if let Some(replacement) = visit(&at.reference, text)? {
                *text = replacement;
                changed = true;
            }
        }
        Value::Array(items) => {
            for (index, item) in items.iter_mut().enumerate() {
                at.reference.push('[');
                at.reference.push_str(&index.to_string());
                at.reference.push(']');
                at.shape.push_str("[]");
                let rewritten = sources(item, document, at, embedded, visit);
                at.reference.truncate(reference);
                at.shape.truncate(shape);
                changed |= rewritten?;
            }
        }
        Value::Object(fields) => {
            let typed = document.typed_keys(&at.shape);
            for (key, item) in fields.iter_mut() {
                if !at.reference.ends_with([':', '#']) {
                    at.reference.push('.');
                }
                if !at.shape.is_empty() {
                    at.shape.push('.');
                }
                at.shape.push_str(key);
                let rewritten = if typed {
                    at.reference.push_str(key);
                    sources(item, document, at, embedded, visit)
                } else {
                    at.reference.push('*');
                    match visit(&at.reference, key) {
                        Ok(None) => sources(item, document, at, embedded, visit),
                        Ok(Some(_)) => Err(anyhow::anyhow!(REWRITE_UNREPRESENTABLE)),
                        Err(refusal) => Err(refusal),
                    }
                };
                at.reference.truncate(reference);
                at.shape.truncate(shape);
                changed |= rewritten?;
            }
        }
        _ => {}
    }
    Ok(changed)
}

thread_local! {
    /// The texts this thread already admitted into the capture that is open
    /// on it: one document is inspected several times in one call (before
    /// and after a store's compare-and-swap), and each text is decided — and
    /// recorded — once. The weak handle pins the capture's identity: texts
    /// admitted into another capture never count.
    static ADMITTED: RefCell<(Weak<AdmissionCapture>, HashSet<[u8; 32]>)> =
        RefCell::new((Weak::new(), HashSet::new()));
}

fn text_digest(text: &str) -> [u8; 32] {
    *blake3::hash(text.as_bytes()).as_bytes()
}

fn admitted_in(open: Option<&Arc<AdmissionCapture>>, text: &str, remember: bool) -> bool {
    let Some(open) = open else {
        return false;
    };
    ADMITTED.with(|slot| {
        let mut slot = slot.borrow_mut();
        if !slot
            .0
            .upgrade()
            .is_some_and(|held| Arc::ptr_eq(&held, open))
        {
            *slot = (Arc::downgrade(open), HashSet::new());
        }
        if remember {
            slot.1.insert(text_digest(text))
        } else {
            slot.1.contains(&text_digest(text))
        }
    })
}

/// One source of a BOUND document through the Context Gateway
/// (`admit_bound_context_source`): admitted only unchanged; a text the
/// gateway would mask or withhold is refused and recorded as a refusal in
/// the open capture. For foreign text the project policy judges the
/// original first and records its audit under the operation's tool label,
/// as `tools::admitted_sources::admit_remote_text` does for a remote source.
fn admit_bound_source(text: &str, origin: Origin) -> Result<()> {
    if let Origin::Foreign(tool) = origin
        && let Some(policy) = runtime::active()
    {
        let outcome = content::evaluate_text(text, &policy);
        crate::server::policy_guard::audit_filter(tool, &outcome.audit, outcome.blocked);
        ensure!(!outcome.blocked, "checkpoint content withheld");
    }
    match crate::core::context_admission::admit_bound_context_source(text) {
        Ok(_) => Ok(()),
        // The gateway's own, content-free reason codes say which it was.
        Err(refusal) if refusal.to_string().contains("source.bytes_not_preserved") => {
            Err(anyhow::anyhow!(REWRITE_UNREPRESENTABLE))
        }
        Err(_) => Err(anyhow::anyhow!(GATEWAY_WITHHELD)),
    }
}

/// Gate 3 for an ALREADY BOUND document, in every mode: each source of
/// `value` is admitted by the Context Gateway as its own source, recorded in
/// the call's open capture and named `<prefix><field path>`
/// (`envelope:checkpoint.live_state.findings[0]`). A source the gateway
/// would mask refuses the document ([`REWRITE_UNREPRESENTABLE`]), one it
/// withholds likewise ([`GATEWAY_WITHHELD`]); the document is never changed.
pub(in crate::core::execution_ledger::host) fn admit_bound(
    prefix: &str,
    document: Document,
    value: &Value,
    origin: Origin,
) -> Result<()> {
    let open = capture::current();
    let mut at = Position {
        reference: prefix.to_owned(),
        shape: String::new(),
    };
    let mut walked = value.clone();
    let mut visit = |reference: &str, text: &str| -> Result<Option<String>> {
        if !admitted_in(open.as_ref(), text, false) {
            admit_bound_source(text, origin)?;
            capture::note_source_id(reference);
            admitted_in(open.as_ref(), text, true);
        }
        Ok(None)
    };
    sources(&mut walked, document, &mut at, 0, &mut visit).map(|_| ())
}

/// Gate 3 for a document this host is PRODUCING and has not bound yet, in
/// every mode: each source of `value` is admitted by the Context Gateway
/// and REPLACED by its admitted text, so what the caller then digests and
/// signs is the admitted form. A withheld source refuses the document
/// ([`GATEWAY_WITHHELD`]). `Ok(true)`: at least one text was changed.
pub(in crate::core::execution_ledger::host) fn admit_fresh(
    prefix: &str,
    document: Document,
    value: &mut Value,
) -> Result<bool> {
    let open = capture::current();
    let mut at = Position {
        reference: prefix.to_owned(),
        shape: String::new(),
    };
    let mut visit = |reference: &str, text: &str| -> Result<Option<String>> {
        if admitted_in(open.as_ref(), text, false) {
            return Ok(None);
        }
        let admitted = crate::core::context_admission::admit_context_source(text)
            .map_err(|_| anyhow::anyhow!(GATEWAY_WITHHELD))?
            .text;
        capture::note_source_id(reference);
        // The admitted form is what later inspections of the bound document
        // see: it is decided, and is not decided again.
        admitted_in(open.as_ref(), &admitted, true);
        Ok((admitted != text).then_some(admitted))
    };
    sources(value, document, &mut at, 0, &mut visit)
}

/// The package without its embedded documents (the envelope, the receipt
/// and evidence entries): those are admitted decoded, never as serialized
/// JSON.
fn without_embedded(mut package: Value) -> Value {
    if let Some(fields) = package.as_object_mut() {
        fields.remove("envelope_json");
        for list in ["receipts", "evidence"] {
            let entries = fields.get_mut(list).and_then(Value::as_array_mut);
            for entry in entries.into_iter().flatten() {
                if let Some(entry) = entry.as_object_mut() {
                    entry.remove("json");
                }
            }
        }
    }
    package
}

pub(super) fn admit(
    package: &CheckpointTransferPackageV1,
    root: &Path,
    origin: Origin,
) -> Result<()> {
    ensure!(
        runtime::request_project_is(root),
        "checkpoint policy scope mismatch"
    );
    let root = root
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("checkpoint policy root unavailable"))?;
    runtime::with_project_source_view(root, || {
        ensure!(
            runtime::REQUEST_PROJECT
                .try_with(|slot| slot.borrow().as_deref() == Some(Path::new(root)))
                .unwrap_or(false),
            "checkpoint policy scope mismatch"
        );
        ensure!(
            crate::core::roles::active_role().is_tool_allowed("ctx_read"),
            "checkpoint content withheld"
        );
        let active = runtime::active();
        if let Some(policy) = active.as_ref() {
            ensure!(
                policy.tool_allowed("ctx_read"),
                "checkpoint content withheld"
            );
        }
        // Inspect the transport as well as each decoded embedded document. A
        // regex over serialized JSON alone misses escapes and anchored fields.
        // Project policy and secret floor first, over everything; only then
        // the gateway's admission of the sources, document by document. The
        // package is bound: nothing in it can be rewritten.
        let outer = serde_json::to_value(package)?;
        let envelope: Value = serde_json::from_str(&package.envelope_json)?;
        inspect(&outer, active.as_deref())?;
        inspect(&envelope, active.as_deref())?;
        let mut entries = Vec::new();
        for (list, items) in [
            ("receipt", &package.receipts),
            ("evidence", &package.evidence),
        ] {
            for (index, entry) in items.iter().enumerate() {
                let decoded: Value = serde_json::from_str(&entry.json)?;
                inspect(&decoded, active.as_deref())?;
                entries.push((format!("{list}[{index}]:"), decoded));
            }
        }
        admit_bound(
            "package:",
            Document::Package,
            &without_embedded(outer),
            origin,
        )?;
        admit_bound("envelope:", Document::Envelope, &envelope, origin)?;
        for (prefix, decoded) in &entries {
            // Receipt, plan, outcome and evidence documents have no table:
            // every string in them is a source.
            admit_bound(prefix, Document::Unlisted, decoded, origin)?;
        }
        Ok(())
    })
    .map_err(|_| anyhow::anyhow!("checkpoint policy changed or unavailable"))?
}

fn inspect(value: &Value, active: Option<&runtime::ActivePolicy>) -> Result<()> {
    // The existing bounded walker inspects decoded keys, strings and scalars;
    // equality is mandatory because all these documents are digest-bound.
    ensure!(
        diagnostics::inspect(value, active).as_ref() == Some(value),
        "checkpoint content withheld"
    );
    inspect_secrets(value)?;
    // Also check the complete document so multi-field patterns cannot bypass
    // the field-by-field scan. Warnings are not mutations or blocking rules.
    if let Some(active) = active {
        let text = serde_json::to_string(value)?;
        let result = content::evaluate_text(&text, active);
        ensure!(
            !result.blocked && result.text == text,
            "checkpoint content withheld"
        );
    }
    Ok(())
}

fn inspect_secrets(value: &Value) -> Result<()> {
    // This walk follows the shared bounded inspector above, so recursion and
    // total input have already been limited. Community retains its secret floor.
    let text = |value: &str| {
        ensure!(
            crate::core::secret_detection::detect_secrets(value).is_empty(),
            "checkpoint contains credential-shaped material"
        );
        Ok(())
    };
    match value {
        Value::String(value) => text(value),
        Value::Array(values) => values.iter().try_for_each(inspect_secrets),
        Value::Object(values) => values.iter().try_for_each(|(key, value)| {
            text(key)?;
            inspect_secrets(value)
        }),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy(extra: &str) -> runtime::ActivePolicy {
        runtime::ActivePolicy::from_resolved(
            crate::core::policy::load(&format!(
                "name = 'checkpoint'\nversion = '1.0.0'\ndescription = 'test'\n{extra}"
            ))
            .unwrap(),
        )
    }

    #[test]
    fn decoded_fields_cannot_hide_anchored_rules_or_secrets() {
        let active = policy("[redaction]\ncustomer = '^K-[0-9]{6}$'");
        let value: Value = serde_json::from_str(r#"{"summary":"K-482193"}"#).unwrap();
        assert!(inspect(&value, Some(&active)).is_err());
        assert!(
            inspect(
                &serde_json::json!({"summary":"safe continuation"}),
                Some(&active)
            )
            .is_ok()
        );
        // Assembled at runtime so no source line carries a token-shaped literal
        // that repository secret scanners would flag.
        let token = format!("{}{}", "gh", "p_abcdefghijklmnopqrstuvwxyz1234567890");
        assert!(inspect(&serde_json::json!({ "note": token }), None).is_err());
    }

    #[test]
    fn signed_document_is_never_rewritten_and_blocking_stays_distinct() {
        let mask = policy("[redaction]\ncustomer = 'K-[0-9]{6}'");
        let block = policy("[filters]\nclassification = 'block'");
        let value = serde_json::json!({"notes":["K-482193", "CONFIDENTIAL"]});
        let before = value.clone();
        assert!(inspect(&value, Some(&mask)).is_err());
        assert!(inspect(&value, Some(&block)).is_err());
        assert_eq!(value, before);
    }

    const CARD: &str = "Customer paid with card 4111 1111 1111 1111";

    fn gateway(mode: &str) -> crate::core::data_dir::IsolatedDataDir {
        let data = crate::core::data_dir::isolated_data_dir();
        let dir = crate::core::paths::config_dir_read_only().expect("config dir");
        std::fs::create_dir_all(&dir).expect("config dir");
        let config = format!("[context_gateway]\nmode = \"{mode}\"\n");
        std::fs::write(dir.join("config.toml"), config).expect("config");
        data
    }

    /// A signed envelope as these operations carry it: structural fields
    /// (schema, signer, signature, verified lineage, typed ids), sources
    /// (texts, a digest that is only carried), and a JSON document kept as
    /// a serialized string. `finding` is the one text that varies.
    fn envelope(finding: &str) -> Value {
        let signature = format!("{}==", "Qk".repeat(43));
        let digest = format!("sha256:{}", "ab".repeat(32));
        let kept = serde_json::json!({ "summary": "nested hand-off text" });
        serde_json::json!({
            "schema_version": "leanctx.host-checkpoint/v1",
            "signer": {"key_id": "host-key-1", "public_key_digest": digest},
            "signature": signature,
            "checkpoint": {
                "created_at": "2026-10-06T22:19:32Z",
                "lineage": {"task_id": "task-1", "artifact_lineage": {"task_ref": digest}},
                "live_state": {
                    "task": {"task_id": "task-1", "title": "Refactor the parser module"},
                    "findings": [finding],
                    "files": [{"source_id": "src-1", "content_digest": digest}],
                    "session_state": {"state": {"note": kept.to_string()}},
                },
            },
        })
    }

    fn references(open: &AdmissionCapture) -> Vec<String> {
        use crate::core::context_admission::capture::SourceName;
        let mut references: Vec<String> = open
            .source_references()
            .0
            .into_iter()
            .map(|(_, name)| match name {
                SourceName::Id(reference) | SourceName::Path(reference) => reference,
            })
            .collect();
        references.sort();
        references
    }

    /// G08: the gateway decides on every SOURCE of a bound document — each
    /// once, named by its field path. A source is whatever is not on the
    /// document's table of verified or typed paths: texts, a digest that is
    /// only carried, the keys and strings of an untyped subtree and of a
    /// serialized document — never the verified signature or lineage. A
    /// source it would mask refuses the bound document as unrepresentable,
    /// recorded as a refusal; so does a token in a field off the table.
    #[test]
    fn sources_are_admitted_field_by_field_or_the_bound_document_is_refused() {
        use crate::core::context_admission::capture::{ADMISSIONS, CallSnapshot};
        use lean_ctx_protocol::context_gateway::ContextDispositionV1;
        let _data = gateway("governed");
        let _community = runtime::TestPolicyOverride::set(None);
        let clean = envelope("The lexer drops trailing commas");
        let open = AdmissionCapture::for_call(CallSnapshot::take());
        ADMISSIONS.sync_scope(Some(open.clone()), || {
            for _ in 0..2 {
                // Inspected twice in the call: decided once.
                admit_bound("envelope:", Document::Envelope, &clean, Origin::Local)
                    .expect("admitted");
            }
        });
        assert_eq!(
            references(&open),
            [
                "envelope:checkpoint.live_state.files[0].content_digest",
                "envelope:checkpoint.live_state.findings[0]",
                "envelope:checkpoint.live_state.session_state.state.*",
                "envelope:checkpoint.live_state.session_state.state.*#*",
                "envelope:checkpoint.live_state.session_state.state.*#*",
                "envelope:checkpoint.live_state.task.title",
            ]
        );
        assert_eq!(open.itemized().len(), 6, "one decision per source");

        let masked = envelope(CARD);
        let mut token = envelope("The parser keeps its lookahead");
        token["checkpoint"]["identity"] = serde_json::json!({
            "device_id": concat!("AK", "IAIOSFODNN7EXAMPLE"),
        });
        for origin in [Origin::Local, Origin::Foreign("context-checkpoint-import")] {
            for refused in [&masked, &token] {
                let refusal = admit_bound("envelope:", Document::Envelope, refused, origin)
                    .expect_err("refused");
                assert_eq!(refusal.to_string(), REWRITE_UNREPRESENTABLE);
            }
        }
        let open = AdmissionCapture::for_call(CallSnapshot::take());
        ADMISSIONS
            .sync_scope(Some(open.clone()), || {
                admit_bound("envelope:", Document::Envelope, &masked, Origin::Local)
            })
            .expect_err("refused");
        let decisions = open.itemized();
        assert!(
            decisions
                .iter()
                .any(|(decision, _)| decision.disposition == ContextDispositionV1::Deny),
            "the refusal is on record"
        );
        assert!(
            decisions
                .iter()
                .all(|(decision, _)| decision.disposition != ContextDispositionV1::AllowRedacted),
            "a mask of a bound text is never recorded as a redaction"
        );
    }

    /// The same rules in developer mode: a clean document is admitted and
    /// left byte for byte as it was; a bound document the gateway would mask
    /// is refused, local or foreign; a document this host is still producing
    /// carries the admitted — masked — text instead.
    #[test]
    fn developer_mode_admits_clean_refuses_bound_and_masks_fresh_documents() {
        use crate::core::context_admission::capture::{ADMISSIONS, CallSnapshot};
        let _data = gateway("developer");
        let _community = runtime::TestPolicyOverride::set(None);
        let clean = envelope("The lexer drops trailing commas");
        let open = AdmissionCapture::for_call(CallSnapshot::take());
        ADMISSIONS.sync_scope(Some(open.clone()), || {
            admit_bound("envelope:", Document::Envelope, &clean, Origin::Local).expect("admitted");
            let mut fresh = clean.clone();
            let changed = admit_fresh("envelope:", Document::Envelope, &mut fresh).expect("fresh");
            assert!(!changed, "clean sources are not changed");
            assert_eq!(fresh, clean, "byte for byte as before");
        });
        assert!(!open.is_empty(), "the decisions are recorded in every mode");

        let masked = envelope(CARD);
        for origin in [Origin::Local, Origin::Foreign("context-checkpoint-import")] {
            let refusal = admit_bound("envelope:", Document::Envelope, &masked, origin)
                .expect_err("a bound document cannot carry the mask");
            assert_eq!(refusal.to_string(), REWRITE_UNREPRESENTABLE);
        }
        let mut fresh = masked.clone();
        assert!(admit_fresh("envelope:", Document::Envelope, &mut fresh).expect("masked"));
        let finding = fresh["checkpoint"]["live_state"]["findings"][0]
            .as_str()
            .expect("finding");
        assert!(!finding.contains("4111 1111 1111 1111"), "{finding}");
        assert_eq!(
            fresh["signature"], masked["signature"],
            "structure untouched"
        );
        // The admitted form is what gets bound: it passes as a bound document.
        admit_bound("envelope:", Document::Envelope, &fresh, Origin::Local).expect("idempotent");
    }
}
