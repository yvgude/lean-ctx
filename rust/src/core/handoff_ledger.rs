use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::core::knowledge::ProjectKnowledge;
use crate::core::protocol::ToolCallRecord;
use crate::core::session::SessionState;
use crate::core::workflow::WorkflowRun;

#[path = "handoff_ledger_authority.rs"]
mod authority;
#[path = "handoff_ledger_merge.rs"]
mod merge;

pub(crate) use authority::LedgerPart;
use authority::{BoundEntry, CURATED_TOOL, INCONSISTENT_CODE, LedgerAuthority, PartKind};
pub(crate) use merge::{MAX_MERGE_INPUTS, MergeOutcome, merge_ledgers};

pub(crate) const SCHEMA_VERSION: u32 = crate::core::contracts::HANDOFF_LEDGER_V1_SCHEMA_VERSION;
const MAX_KNOWLEDGE_FACTS: usize = 50;
const MAX_CURATED_REFS: usize = 20;
const MAX_LEDGER_FINDINGS: usize = 20;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct HandoffLedgerV1 {
    pub schema_version: u32,
    pub created_at: String,
    pub content_md5: String,
    pub manifest_md5: String,
    pub project_root: Option<String>,
    pub agent_id: Option<String>,
    pub client_name: Option<String>,
    pub workflow: Option<WorkflowRun>,
    pub session_snapshot: String,
    pub session: SessionExcerpt,
    pub tool_calls: ToolCallsSummary,
    pub evidence_keys: Vec<String>,
    pub knowledge: KnowledgeExcerpt,
    pub curated_refs: Vec<CuratedRef>,
    #[serde(default)]
    pub active_overlays: Vec<serde_json::Value>,
    /// Content digests of the ledgers this one was merged from
    /// (`ctx_handoff merge`), in canonical order; empty for a created ledger.
    /// Covered by `content_md5`, so the receipt of the merge is the ledger.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub merged_from: Vec<String>,
    /// Idempotency key the ledger was created with (`ctx_handoff create
    /// request_id`) and the digest of that request (AP07-G9); absent on a
    /// ledger created without one. Covered by `content_md5`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_digest: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SessionExcerpt {
    pub id: String,
    pub task: Option<String>,
    pub decisions: Vec<String>,
    pub findings: Vec<String>,
    pub next_steps: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ToolCallsSummary {
    pub total: usize,
    pub by_tool: BTreeMap<String, u64>,
    pub by_ctx_read_mode: BTreeMap<String, u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct KnowledgeExcerpt {
    pub project_hash: Option<String>,
    pub facts: Vec<KnowledgeFactMini>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnowledgeFactMini {
    pub category: String,
    pub key: String,
    pub value: String,
    pub confidence: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CuratedRef {
    pub path: String,
    pub mode: String,
    pub content_md5: String,
    pub content: String,
}

#[derive(Debug, Clone)]
pub(crate) struct CreateLedgerInput {
    pub agent_id: Option<String>,
    pub client_name: Option<String>,
    pub project_root: Option<String>,
    pub session: SessionState,
    pub tool_calls: Vec<ToolCallRecord>,
    pub workflow: Option<WorkflowRun>,
    pub curated_refs: Vec<(String, String)>, // (abs_path, signatures_text)
    /// Optional idempotency key: see [`create_ledger`].
    pub request_id: Option<String>,
}

/// Origin markers of a ledger's binding (`StoredBinding::origin`). They say
/// what was established about the ledger's findings and facts when it was
/// written — a ledger is one stored unit and keeps no binding per item:
///
/// - [`ORIGIN_UNCHECKED`]: nothing. Every ledger written before findings and
///   facts were checked at creation carries it, and so does a ledger written
///   without protection, where no part is checked either;
///
/// The two markers of a ledger created under protection are decided from
/// the ledger as it is STORED, by one predicate
/// (`HandoffLedgerV1::checked_origin`): all-bound exactly when every finding
/// and fact in the stored lists is recorded as bound AND the ledger carries
/// no other text without source authority (`has_unbound_text`).
///
/// - [`ORIGIN_ALL_BOUND`]: every finding and fact in the ledger was bound to
///   sources that held at creation, and those sources are in the ledger's
///   lineage (also a ledger without any finding or fact);
/// - [`ORIGIN_ORIGINLESS_PART`]: the bound parts are in the lineage as above,
///   and at least one finding or fact had no binding.
const ORIGIN_UNCHECKED: &str = "handoff";
const ORIGIN_ALL_BOUND: &str = "handoff:all-bound";
const ORIGIN_ORIGINLESS_PART: &str = "handoff:originless-part";

/// What the findings and facts copied into a new ledger add to its lineage.
struct CopiedParts {
    /// Protection is active: every part is asked about. Otherwise none is.
    enforced: bool,
    /// The ledger's project as a binding names it (canonical).
    project: Option<String>,
    /// The sources of every bound part that is copied.
    sources: Vec<crate::core::context_admission::stored::SourceRef>,
}

impl CopiedParts {
    fn new(project_root: Option<&str>, enforced: bool) -> Self {
        let project = project_root.filter(|root| !root.is_empty()).map(|root| {
            crate::core::pathutil::canonicalize_secure(Path::new(root))
                .ok()
                .and_then(|path| path.to_str().map(str::to_owned))
                .unwrap_or_else(|| root.to_owned())
        });
        Self {
            enforced,
            project,
            sources: Vec::new(),
        }
    }

    /// Whether `part`, a stored finding or fact, is copied into the ledger,
    /// and if so whether it was bound: `None` — NOT copied; `Some(true)` —
    /// copied as a bound element; `Some(false)` — copied, originless.
    /// `names` is that one item under every identity its store may have
    /// bound it by (`archive::bindings::held` for each):
    ///
    /// - bound, holding now, in the ledger's project: copied, its sources
    ///   joined into the ledger's lineage;
    /// - no binding under any name: copied, originless;
    /// - bound and not holding, or bound in another project: not copied.
    ///
    /// It is called only for a part that is then STORED when it is copied
    /// (`create_ledger` cuts its lists first), so no source of a part that
    /// is not in the ledger is ever joined.
    ///
    /// Without protection every part is copied (`Some(false)`) and nothing
    /// is asked or joined: such a ledger is marked [`ORIGIN_UNCHECKED`].
    fn admit(
        &mut self,
        names: &[crate::core::archive::bindings::Item<'_>],
        part: &LedgerPart<'_>,
        reader: Option<&str>,
    ) -> Option<bool> {
        if !self.enforced {
            return Some(false);
        }
        let mut held = Vec::new();
        for item in names {
            match crate::core::archive::bindings::held(item, part.text(), reader) {
                Ok(Some((project, sources))) if self.in_project(&project) => held.extend(sources),
                Ok(None) => {}
                Ok(Some(_)) | Err(_) => return None,
            }
        }
        let bound = !held.is_empty();
        self.sources.extend(held);
        Some(bound)
    }

    fn in_project(&self, project: &str) -> bool {
        self.project.as_deref() == Some(project)
    }
}

/// Create and store a ledger of the session in `input`.
///
/// With a `request_id` (AP07-G9) the call is idempotent for its agent in its
/// project: a ledger stored with the same key and the same request (the
/// curated paths, in order) is served again (`load_ledger`) instead of a
/// second one being written; the same key with another request is refused.
/// The key is stored in its ledger, so it lives exactly as long as that
/// ledger (`clear` removes both). A key needs a project: a project-less
/// ledger is served to no one and could never be replayed.
pub(crate) fn create_ledger(
    input: CreateLedgerInput,
) -> Result<(HandoffLedgerV1, PathBuf), String> {
    let CreateLedgerInput {
        agent_id,
        client_name,
        project_root,
        mut session,
        tool_calls,
        workflow,
        curated_refs,
        request_id,
    } = input;
    // A keyed create holds its (agent, project) lock from the lookup of the
    // key to the write of its ledger: two first calls with one key cannot
    // both miss the lookup and each store a ledger (C03).
    let (_keyed_create, request_digest) = match request_id.as_deref() {
        Some(key) => {
            crate::core::agents::check_request_id(key)?;
            let Some(root) = project_root.as_deref() else {
                return Err("request_id requires a project".to_string());
            };
            let lock = keyed_create_lock(root, agent_id.as_deref())?;
            let paths: Vec<&str> = curated_refs
                .iter()
                .take(MAX_CURATED_REFS)
                .map(|(path, _)| path.as_str())
                .collect();
            let digest = crate::core::agents::request_digest(
                "leanctx/handoff-create/v1",
                &serde_json::json!(paths),
            );
            if let Some(path) = keyed_ledger(root, agent_id.as_deref(), key, &digest)? {
                return load_ledger(&path).map(|ledger| (ledger, path));
            }
            #[cfg(test)]
            keyed_race::pause();
            (Some(lock), Some(digest))
        }
        None => (None, None),
    };
    let manifest_md5 = manifest_md5();

    let mut evidence_keys: BTreeSet<String> = BTreeSet::new();
    for ev in &session.evidence {
        evidence_keys.insert(ev.key.clone());
    }

    let mut by_tool: BTreeMap<String, u64> = BTreeMap::new();
    let mut by_mode: BTreeMap<String, u64> = BTreeMap::new();
    for call in &tool_calls {
        *by_tool.entry(call.tool.clone()).or_insert(0) += 1;
        if call.tool == "ctx_read"
            && let Some(m) = call.mode.as_deref()
        {
            *by_mode.entry(m.to_string()).or_insert(0) += 1;
        }
    }

    // Findings and facts are copies of stored items. The session copy is cut
    // to the findings a ledger keeps — the last ones — BEFORE any is asked
    // about, and each of those is then asked about once
    // (`CopiedParts::admit`): a bound finding that holds brings its sources
    // into the ledger's lineage, one without a binding is copied originless,
    // a bound one that does not hold is removed. What is left is exactly the
    // stored list, in its order: the flags below are indexed like it, no
    // source of a finding outside it is joined, and the snapshot below is
    // built from this same list — it cannot carry a finding that was cut or
    // removed.
    let authority_required = crate::core::context_admission::stored::source_authority_required();
    let mut parts = CopiedParts::new(project_root.as_deref(), authority_required);
    let (session_id, session_root) = (session.id.clone(), session.project_root.clone());
    let cut = session.findings.len().saturating_sub(MAX_LEDGER_FINDINGS);
    session.findings.drain(..cut);
    let mut finding_bound: Vec<bool> = Vec::new();
    session.findings.retain(|finding| {
        let item = crate::core::archive::bindings::Item::SessionFinding {
            session: &session_id,
            file: finding.file.as_deref(),
        };
        let part = LedgerPart::Finding(&finding.summary);
        let copied = parts.admit(&[item], &part, session_root.as_deref());
        finding_bound.extend(copied);
        copied.is_some()
    });

    let session_excerpt = SessionExcerpt {
        id: session.id.clone(),
        task: session.task.as_ref().map(|t| t.description.clone()),
        decisions: session
            .decisions
            .iter()
            .rev()
            .take(10)
            .map(|d| d.summary.clone())
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect(),
        findings: session.findings.iter().map(|f| f.summary.clone()).collect(),
        next_steps: session.next_steps.iter().take(20).cloned().collect(),
    };

    let (knowledge_excerpt, fact_bound) =
        build_knowledge_excerpt(project_root.as_deref(), &mut parts);

    // Source lineage of the ledger: every curated excerpt is bound to the file
    // it was derived from, read now through the admitted rooted reader. While
    // protection is active an excerpt whose source cannot be bound is left
    // out; Community developer mode keeps it as caller-supplied text. The
    // sources of the bound findings and facts are joined to these; more than
    // the lineage bound refuses the ledger (`Lineage::of`), never truncates.
    let mut sources = Vec::new();
    let mut curated = Vec::new();
    for (p, text) in curated_refs.into_iter().take(MAX_CURATED_REFS) {
        let bound = project_root.as_deref().and_then(|root| {
            crate::core::context_admission::stored::SourceRef::read(&p, root, CURATED_TOOL).ok()
        });
        match bound {
            Some((_, source)) => sources.push(source),
            None if authority_required => continue,
            None => {}
        }
        let md5 = crate::core::hasher::hash_hex(text.as_bytes());
        curated.push(CuratedRef {
            path: p,
            mode: "signatures".to_string(),
            content_md5: md5,
            content: text,
        });
    }
    sources.extend(parts.sources);

    let mut ledger = HandoffLedgerV1 {
        schema_version: SCHEMA_VERSION,
        created_at: chrono::Local::now().to_rfc3339(),
        content_md5: String::new(),
        manifest_md5,
        project_root,
        agent_id,
        client_name,
        workflow,
        session_snapshot: session.build_compaction_snapshot(),
        session: session_excerpt,
        tool_calls: ToolCallsSummary {
            total: tool_calls.len(),
            by_tool,
            by_ctx_read_mode: by_mode,
        },
        evidence_keys: evidence_keys.into_iter().collect(),
        knowledge: knowledge_excerpt,
        curated_refs: curated,
        active_overlays: {
            let overlay_store = crate::core::context_overlay::OverlayStore::load_project(
                &std::env::current_dir().unwrap_or_default(),
            );
            overlay_store
                .all()
                .iter()
                .filter_map(|o| serde_json::to_value(o).ok())
                .collect()
        },
        merged_from: Vec::new(),
        request_id,
        request_digest,
    };

    let md5 = ledger_content_md5(&ledger);
    ledger.content_md5.clone_from(&md5);

    let path = persist_ledger(&ledger, sources, |kind, index| {
        let flags = match kind {
            PartKind::Finding => &finding_bound,
            PartKind::Fact => &fact_bound,
        };
        flags.get(index).copied().unwrap_or(false)
    })?;
    Ok((ledger, path))
}

/// Admit and store `ledger` (its `content_md5` already set) with its
/// lineage `sources`; `bound(kind, index)` says which of its stored findings
/// and facts were bound. The one write path of created and merged ledgers.
fn persist_ledger(
    ledger: &HandoffLedgerV1,
    sources: Vec<crate::core::context_admission::stored::SourceRef>,
    bound: impl Fn(PartKind, usize) -> bool,
) -> Result<PathBuf, String> {
    let authority_required = crate::core::context_admission::stored::source_authority_required();
    let path = ledger_path(&ledger.created_at, &ledger.content_md5)?;
    let json = serde_json::to_string_pretty(ledger).map_err(|e| format!("serialize: {e}"))?;
    // Admission precedes persistence. The ledger is digest-bound
    // (`content_md5`), so it is never rewritten: content the current policy
    // would redact or withhold refuses the ledger with the typed reason.
    // The bound record and the marker are both taken from the ledger as it
    // is stored — the final lists — and written for every ledger created
    // under protection, the record also with no entry.
    let bound_parts = authority_required.then(|| ledger.bound_record(&bound));
    let origin = bound_parts
        .as_ref()
        .map_or(ORIGIN_UNCHECKED, |record| ledger.checked_origin(record));
    let text = ledger_text(&json)?;
    let binding = admit_ledger(&text, ledger.project_root.as_deref(), sources, origin)?;
    // Written only if it would be served right now: the whole lineage
    // re-read against the one budget a reader uses. A ledger that fits each
    // source alone but not together would otherwise be stored and then
    // withheld on every read.
    if authority_required {
        serve_ledger(Some(&binding), &text, request_project().as_deref())?;
    }
    crate::config_io::write_atomic_with_backup(&path, &(json + "\n"))
        .map_err(|e| format!("write {}: {e}", path.display()))?;
    let authority = LedgerAuthority {
        binding,
        bound_parts,
    };
    let authority = serde_json::to_string(&authority).map_err(|e| format!("serialize: {e}"))?;
    if let Err(error) =
        crate::config_io::write_atomic_with_backup(&authority_path(&path), &authority)
    {
        // A ledger without its binding would be served as a legacy record in
        // Community mode and never under protection: do not leave it behind.
        let _ = std::fs::remove_file(&path);
        return Err(format!("write {}: {error}", path.display()));
    }
    Ok(path)
}

/// The binding of a ledger lives next to it, so the ledger file and
/// `HandoffLedgerV1` keep their shape. It does not end in `.json`: listings
/// and digests of the store see ledgers only.
fn authority_path(ledger: &Path) -> PathBuf {
    ledger.with_extension("authority")
}

/// The text of a ledger as admission inspects it: every string value, decoded,
/// one per line. Detectors then see the content the reader will see — not
/// JSON escapes, and not field names next to digests.
fn ledger_text(json: &str) -> Result<String, String> {
    fn collect<'a>(value: &'a Value, leaves: &mut Vec<&'a str>) {
        match value {
            Value::String(text) if !text.is_empty() => leaves.push(text),
            Value::Array(items) => items.iter().for_each(|item| collect(item, leaves)),
            Value::Object(map) => map.values().for_each(|child| collect(child, leaves)),
            _ => {}
        }
    }
    let value: Value = serde_json::from_str(json).map_err(|e| format!("parse ledger: {e}"))?;
    let mut leaves = Vec::new();
    collect(&value, &mut leaves);
    Ok(leaves.join("\n"))
}

fn admit_ledger(
    json: &str,
    project_root: Option<&str>,
    sources: Vec<crate::core::context_admission::stored::SourceRef>,
    origin_marker: &str,
) -> Result<crate::core::context_admission::stored::StoredBinding, String> {
    use crate::core::context_admission::carrier::Withheld;
    use crate::core::context_admission::stored::{Lineage, admit_and_bind};
    let refused = |withheld: Withheld| withheld.message("handoff ledger");
    let lineage = Lineage::of(sources).map_err(refused)?;
    let (admitted, binding) =
        admit_and_bind(json, origin_marker, project_root, lineage).map_err(refused)?;
    if admitted != json {
        return Err(refused(Withheld::new(&["policy.content_redacted"])));
    }
    Ok(binding)
}

/// The project of the running request, as the reader of a stored ledger.
fn request_project() -> Option<String> {
    crate::core::policy::runtime::REQUEST_PROJECT
        .try_with(|slot| slot.borrow().clone())
        .ok()
        .flatten()
        .and_then(|project| project.to_str().map(str::to_owned))
}

/// The ledger text as a new delivery under the CURRENT policy: project
/// authority, every source of its lineage, then the content rules. A ledger
/// written before bindings existed (`binding` is `None`) follows the
/// originless rule.
///
/// The lineage is the joined set of the curated excerpts and of every bound
/// finding and fact copied at creation. The ledger text is ONE stored unit
/// without per-item spans, so the rule is whole-ledger withholding: when any
/// one of those sources is gone, changed, reclassified or no longer permitted,
/// the whole ledger is withheld — not only the item that came from it. It is
/// served again when the source delivers the bound bytes again.
fn serve_ledger(
    binding: Option<&crate::core::context_admission::stored::StoredBinding>,
    text: &str,
    reader: Option<&str>,
) -> Result<(), String> {
    use crate::core::context_admission::carrier::Withheld;
    use crate::core::context_admission::stored::serve_stored;
    let served = serve_stored(text, binding, reader)
        .map_err(|withheld| withheld.message("handoff ledger"))?;
    if served == text {
        Ok(())
    } else {
        Err(Withheld::new(&["policy.content_redacted"]).message("handoff ledger"))
    }
}

/// Whether a ledger stored for `stored` belongs to the project `reader`
/// works in. A ledger without a project belongs to none (AP05-G7).
fn same_project(stored: Option<&str>, reader: Option<&str>) -> bool {
    let canonical =
        |root: &str| crate::core::pathutil::safe_canonicalize_bounded(Path::new(root), 2000);
    match (stored, reader) {
        (Some(stored), Some(reader)) => canonical(stored) == canonical(reader),
        _ => false,
    }
}

/// The project a stored ledger file records, without serving its content.
fn stored_project(path: &Path) -> Option<String> {
    let json: Value = serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()?;
    json.get("project_root")?.as_str().map(str::to_owned)
}

/// Test seam: holds a keyed create that missed its lookup until the armed
/// number of creates has reached the same point, or for a bounded time. A
/// second create that cannot get past the keyed lock never arrives, so the
/// first one goes on after the bound; without the lock both arrive and race.
#[cfg(test)]
pub(crate) mod keyed_race {
    use std::sync::{Condvar, Mutex, PoisonError};
    use std::time::{Duration, Instant};

    /// (expected, arrived); `expected == 0` is unarmed.
    static STATE: Mutex<(usize, usize)> = Mutex::new((0, 0));
    static ARRIVAL: Condvar = Condvar::new();
    const BOUND: Duration = Duration::from_millis(500);

    /// Arm for `expected` creates until the guard drops.
    pub(crate) fn arm(expected: usize) -> impl Drop {
        struct Disarm;
        impl Drop for Disarm {
            fn drop(&mut self) {
                *STATE.lock().unwrap_or_else(PoisonError::into_inner) = (0, 0);
            }
        }
        *STATE.lock().unwrap_or_else(PoisonError::into_inner) = (expected, 0);
        Disarm
    }

    pub(super) fn pause() {
        let mut state = STATE.lock().unwrap_or_else(PoisonError::into_inner);
        if state.0 == 0 {
            return;
        }
        state.1 += 1;
        ARRIVAL.notify_all();
        let deadline = Instant::now() + BOUND;
        while state.1 < state.0 {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            state = ARRIVAL
                .wait_timeout(state, left)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }
}

/// The lock that serializes keyed creates of `agent_id` in `project_root`
/// across threads and processes; released on drop. Bounded: a create that
/// cannot take it within the wait is refused, never left blocking.
fn keyed_create_lock(
    project_root: &str,
    agent_id: Option<&str>,
) -> Result<crate::core::agents::FileLock, String> {
    const WAIT: std::time::Duration = std::time::Duration::from_secs(10);
    let dir = handoffs_dir()?;
    std::fs::create_dir_all(&dir).map_err(|e| format!("create_dir_all {}: {e}", dir.display()))?;
    let scope = serde_json::json!([project_root, agent_id]).to_string();
    let name = crate::core::hasher::hash_hex(scope.as_bytes());
    crate::core::agents::FileLock::acquire_with_timeout(
        &dir.join(format!("keyed-{name}.lock")),
        WAIT,
    )
    .map_err(|error| format!("handoff create is busy: {error}"))
}

/// The stored ledger of `project_root` that `agent_id` created with the
/// idempotency key `key`: `Ok(Some)` when it was created by the same request
/// (`digest`), `Err` when the key was used with another one.
fn keyed_ledger(
    project_root: &str,
    agent_id: Option<&str>,
    key: &str,
    digest: &str,
) -> Result<Option<PathBuf>, String> {
    for path in list_ledgers(project_root) {
        let Some(stored) = std::fs::read_to_string(&path)
            .ok()
            .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        else {
            continue;
        };
        if stored["request_id"].as_str() != Some(key) || stored["agent_id"].as_str() != agent_id {
            continue;
        }
        return if stored["request_digest"].as_str() == Some(digest) {
            Ok(Some(path))
        } else {
            Err(crate::core::agents::request_id_conflict())
        };
    }
    Ok(None)
}

/// The ledgers stored for `project_root`, newest first. Ledgers of other
/// projects, and ledgers without a project, are not listed.
pub(crate) fn list_ledgers(project_root: &str) -> Vec<PathBuf> {
    let dir = handoffs_dir().ok();
    let Some(dir) = dir else {
        return Vec::new();
    };
    let Ok(rd) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut items: Vec<PathBuf> = rd
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("json"))
        .filter(|p| same_project(stored_project(p).as_deref(), Some(project_root)))
        .collect();
    items.sort();
    items.reverse();
    items
}

/// The lineage a served ledger hands on to the findings and facts restored
/// from it (`ctx_handoff pull`). It exists for every ledger that was checked
/// at creation, has at least one bound part and was just served — also one
/// with an originless part: the marker decides whether the ledger as a whole
/// may register a lineage, not whether a restored part gets a binding.
pub(crate) struct LedgerLineage {
    binding: crate::core::context_admission::stored::StoredBinding,
    /// The elements that were bound at creation; the record was found
    /// consistent with the stored ledger when it was loaded.
    bound: Vec<BoundEntry>,
    /// The text the binding binds (`ledger_text`).
    text: String,
    reader: Option<String>,
}

impl LedgerLineage {
    /// The binding of `part` — the element at `index` of its own list in the
    /// stored ledger — restored as `derived`.
    ///
    /// `Ok(None)` — the item is restored originless — for an element that is
    /// not recorded as bound at that place: it had no binding at creation. A
    /// part is never upgraded because the ledger has bound siblings, nor
    /// because another element has the same text.
    ///
    /// `Err` (`store.ledger_authority_inconsistent`) for an element that IS
    /// recorded at that place while `part` does not have the recorded
    /// digest: a known-bound element is refused, never restored unbound.
    ///
    /// A recorded element is bound to the ledger's joined source set
    /// (`archive::bindings::carry_from`). This over-approximates per item:
    /// the ledger keeps no per-item source lists, so the item is bound to
    /// ALL files of the ledger, not only to those of the finding or fact it
    /// once was. More sources have to hold, never fewer; the cost is that a
    /// restored item is withheld when an unrelated file of the same ledger
    /// changes.
    pub(crate) fn carry(
        &self,
        part: &LedgerPart<'_>,
        index: usize,
        derived: &crate::core::archive::bindings::Item<'_>,
    ) -> Result<
        Option<crate::core::archive::bindings::Carried>,
        crate::core::context_admission::carrier::Withheld,
    > {
        let recorded = self
            .bound
            .iter()
            .find(|entry| entry.kind == part.kind() && entry.index == index);
        let Some(recorded) = recorded else {
            return Ok(None);
        };
        if recorded.digest != part.digest() {
            return Err(crate::core::context_admission::carrier::Withheld::new(&[
                INCONSISTENT_CODE,
            ]));
        }
        crate::core::archive::bindings::carry_from(
            derived,
            part.text(),
            &self.binding,
            &self.text,
            self.reader.as_deref(),
        )
    }
}

/// A stored ledger for a reader that only renders it (`ctx_handoff show`).
pub(crate) fn load_ledger(path: &Path) -> Result<HandoffLedgerV1, String> {
    load_ledger_bound(path).map(|(ledger, _)| ledger)
}

/// A stored ledger, and the lineage its restored items inherit
/// ([`LedgerLineage`]; `None` for an unchecked ledger and for a ledger none
/// of whose parts was bound — every item restored from those is originless).
///
/// Every reader of a stored ledger (`show`, `pull`) passes here, and the
/// ledger is re-authorized before it is parsed, rendered or handed on
/// ([`serve_ledger`]: whole-ledger withholding). An `Err` is a typed,
/// content-free refusal, returned before anything is written anywhere.
pub(crate) fn load_ledger_bound(
    path: &Path,
) -> Result<(HandoffLedgerV1, Option<LedgerLineage>), String> {
    load_served(path).map(|served| (served.ledger, served.lineage))
}

/// A stored ledger as it was just served, with what a reader that derives a
/// new ledger from it needs (`merge`): its lineage and the files its binding
/// names, `(project, [(path, tool, admitted digest)])`.
struct ServedLedger {
    ledger: HandoffLedgerV1,
    lineage: Option<LedgerLineage>,
    files: Option<(String, Vec<(String, String, String)>)>,
}

fn load_served(path: &Path) -> Result<ServedLedger, String> {
    use crate::core::context_admission::carrier::Withheld;
    use crate::core::context_admission::stored;
    // A ledger serves only the project it was stored for (AP05-G7). Missing,
    // unreadable, another project's or project-less: one answer, decided
    // before anything else about the file is reported.
    let reader = request_project();
    let not_found = || format!("handoff ledger {} not found", path.display());
    let s = std::fs::read_to_string(path).map_err(|_| not_found())?;
    let stored: Value = serde_json::from_str(&s).map_err(|_| not_found())?;
    if !same_project(stored["project_root"].as_str(), reader.as_deref()) {
        return Err(not_found());
    }
    let text = ledger_text(&s)?;
    // The sidecar. One that is missing or does not parse — also a damaged
    // bound record — is no binding at all: the originless rule below.
    let authority: Option<LedgerAuthority> = std::fs::read_to_string(authority_path(path))
        .ok()
        .and_then(|authority| serde_json::from_str(&authority).ok());
    let binding = authority.as_ref().map(|authority| &authority.binding);
    let ledger: HandoffLedgerV1 = match serde_json::from_str(&s) {
        Ok(ledger) => ledger,
        Err(e) => {
            // Not a ledger: authorized like any stored text before the
            // parse error — which may quote a value — is reported.
            serve_ledger(binding, &text, reader.as_deref())?;
            return Err(format!("parse {}: {e}", path.display()));
        }
    };
    // A ledger written under protection must agree with its bound record
    // (`agrees_with`), and that is established first, on digests alone.
    // Disagreement — or no record at all — withholds the whole ledger:
    // nothing is shown, nothing is restored, and a part that was known as
    // bound is never downgraded to unbound text.
    let checked = authority
        .as_ref()
        .filter(|authority| checked_at_creation(authority.binding.origin()));
    if checked.is_some_and(|authority| !ledger.agrees_with(authority)) {
        return Err(Withheld::new(&[INCONSISTENT_CODE]).message("handoff ledger"));
    }
    serve_ledger(binding, &text, reader.as_deref())?;
    // An unchecked ledger (written before findings and facts were checked at
    // creation, or without protection): its curated sources were re-read
    // above, but its findings and facts were never bound to anything. While
    // protection is active they are text without source authority, and the
    // ledger is one unit — it is withheld. A ledger checked at creation is
    // not caught here: its originless part follows the originless rule
    // through its own marker (served, never registered).
    let unchecked = binding.is_some_and(|binding| !checked_at_creation(binding.origin()));
    if unchecked && ledger.has_copied_parts() && stored::source_authority_required() {
        return Err(Withheld::no_source_authority().message("handoff ledger"));
    }
    // Delivered lineage: the binding names the files of the curated excerpts
    // and of the bound findings and facts, and nothing else. Only a ledger
    // that consists of those has that lineage (`has_file_lineage`); with any
    // other text it has none (unknown), whatever its sources are.
    stored::publish_served(binding.filter(|binding| ledger.has_file_lineage(binding.origin())));
    let files = binding
        .and_then(|binding| binding.file_sources())
        .map(|(project, files)| {
            let files = files
                .into_iter()
                .map(|(path, tool, digest)| (path.to_owned(), tool.to_owned(), digest.to_owned()))
                .collect();
            (project.to_owned(), files)
        });
    // What restored items inherit. An unchecked ledger has none: nothing
    // was ever known as bound there, and its record — if any — is not read.
    let lineage = authority
        .filter(|authority| checked_at_creation(authority.binding.origin()))
        .and_then(|authority| {
            let record = authority.bound_parts.filter(|record| record.count > 0)?;
            Some(LedgerLineage {
                binding: authority.binding,
                bound: record.entries,
                text,
                reader,
            })
        });
    Ok(ServedLedger {
        ledger,
        lineage,
        files,
    })
}

/// Whether a ledger's findings and facts were checked when it was written.
/// Any other marker — the one of older ledgers, or one this version does not
/// know — is treated as unchecked.
fn checked_at_creation(origin: &str) -> bool {
    origin == ORIGIN_ALL_BOUND || origin == ORIGIN_ORIGINLESS_PART
}

impl HandoffLedgerV1 {
    /// Whether the ledger carries copies of stored findings or facts.
    fn has_copied_parts(&self) -> bool {
        !self.session.findings.is_empty() || !self.knowledge.facts.is_empty()
    }

    /// Whether every text of the ledger that came from a store has its
    /// source in the ledger's binding, whose marker is `origin`: the curated
    /// excerpts always, findings and facts only in a ledger marked
    /// [`ORIGIN_ALL_BOUND`]. The session snapshot, task, decisions, next
    /// steps, evidence keys, a workflow and overlays are copies from stores
    /// that keep no source binding; a ledger with any of them has no file
    /// lineage. (`create_ledger` always writes a snapshot, so in practice
    /// this holds for snapshot-free ledgers only.)
    fn has_file_lineage(&self, origin: &str) -> bool {
        !self.has_unbound_text() && (origin == ORIGIN_ALL_BOUND || !self.has_copied_parts())
    }

    /// Whether the stored ledger carries text that no store keeps a source
    /// binding for: a session snapshot, a task, decisions, next steps,
    /// evidence keys, a workflow or overlays. The one list of such text, for
    /// the marker (`checked_origin`) and for publication alike.
    fn has_unbound_text(&self) -> bool {
        !(self.session_snapshot.is_empty()
            && self.session.task.is_none()
            && self.session.decisions.is_empty()
            && self.session.next_steps.is_empty()
            && self.evidence_keys.is_empty()
            && self.workflow.is_none()
            && self.active_overlays.is_empty())
    }
}

pub(crate) fn compute_content_md5_for_ledger(ledger: &HandoffLedgerV1) -> String {
    ledger_content_md5(ledger)
}

/// Stored ledgers that name no project: served to no project (AP05-G7).
/// Read for the explicit `lean-ctx migrate legacy-context` adoption only.
pub(crate) fn projectless_ledgers() -> Vec<(PathBuf, HandoffLedgerV1)> {
    let Ok(rd) = handoffs_dir().and_then(|dir| std::fs::read_dir(dir).map_err(|e| e.to_string()))
    else {
        return Vec::new();
    };
    let mut items: Vec<(PathBuf, HandoffLedgerV1)> = rd
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("json"))
        .filter_map(|p| {
            let ledger: HandoffLedgerV1 =
                serde_json::from_str(&std::fs::read_to_string(&p).ok()?).ok()?;
            ledger.project_root.is_none().then_some((p, ledger))
        })
        .collect();
    items.sort_by(|a, b| a.0.cmp(&b.0));
    items
}

/// Store a copy of the project-less ledger at `path` as a ledger of
/// `project_root`, through the admission of a newly created ledger. The
/// original stays untouched.
pub(crate) fn adopt_projectless_ledger(path: &Path, project_root: &str) -> Result<PathBuf, String> {
    let mut ledger: HandoffLedgerV1 = serde_json::from_str(
        &std::fs::read_to_string(path).map_err(|e| format!("read {}: {e}", path.display()))?,
    )
    .map_err(|e| format!("parse {}: {e}", path.display()))?;
    if ledger.project_root.is_some() {
        return Err("ledger already belongs to a project".into());
    }
    ledger.project_root = Some(project_root.to_string());
    ledger.content_md5 = ledger_content_md5(&ledger);
    persist_ledger(&ledger, Vec::new(), |_, _| false)
}

/// Remove the ledgers of `project_root`; other projects' ledgers stay.
pub(crate) fn clear_ledgers(project_root: &str) -> Result<u32, String> {
    let mut removed = 0u32;
    for p in list_ledgers(project_root) {
        if std::fs::remove_file(&p).is_ok() {
            removed += 1;
        }
        let _ = std::fs::remove_file(authority_path(&p));
    }
    Ok(removed)
}

fn build_knowledge_excerpt(
    project_root: Option<&str>,
    parts: &mut CopiedParts,
) -> (KnowledgeExcerpt, Vec<bool>) {
    use crate::core::archive::bindings::Item;
    let Some(knowledge) = project_root.and_then(ProjectKnowledge::load) else {
        return (KnowledgeExcerpt::default(), Vec::new());
    };
    // Whether the fact at the same index of `facts` was bound.
    let mut bound = Vec::new();

    // A fact is bound under the project root its writer's request named:
    // this store's root, or the request's spelling of it. It is asked about
    // under both, with the running request as its reader — the facts
    // `ProjectKnowledge::delivery_filter` delivers, here with their sources.
    let request = crate::tools::admitted_sources::request_root();
    let reader = Some(request.as_str()).filter(|root| !root.is_empty());
    let one_name = reader.is_none_or(|root| root == knowledge.project_root);
    let mut facts = Vec::new();
    for f in knowledge.facts.iter().filter(|f| f.is_current()) {
        let names = [
            Item::KnowledgeFact {
                project: &knowledge.project_root,
                category: &f.category,
                key: &f.key,
            },
            Item::KnowledgeFact {
                project: &request,
                category: &f.category,
                key: &f.key,
            },
        ];
        let names = if one_name { &names[..1] } else { &names[..] };
        let part = LedgerPart::Fact {
            category: &f.category,
            key: &f.key,
            value: &f.value,
        };
        let Some(part_bound) = parts.admit(names, &part, reader) else {
            continue;
        };
        bound.push(part_bound);
        facts.push(KnowledgeFactMini {
            category: f.category.clone(),
            key: f.key.clone(),
            value: f.value.clone(),
            confidence: f.confidence,
        });
        if facts.len() >= MAX_KNOWLEDGE_FACTS {
            break;
        }
    }

    let excerpt = KnowledgeExcerpt {
        project_hash: Some(knowledge.project_hash.clone()),
        facts,
    };
    (excerpt, bound)
}

fn ledger_path(created_at: &str, md5: &str) -> Result<PathBuf, String> {
    let dir = handoffs_dir()?;
    std::fs::create_dir_all(&dir).map_err(|e| format!("create_dir_all {}: {e}", dir.display()))?;
    let ts = created_at
        .chars()
        .filter(char::is_ascii_digit)
        .take(14)
        .collect::<String>();
    let name = format!("{ts}-{md5}.json");
    Ok(dir.join(name))
}

fn handoffs_dir() -> Result<PathBuf, String> {
    let dir = crate::core::data_dir::lean_ctx_data_dir()
        .map_err(|e| e.clone())?
        .join("handoffs");
    Ok(dir)
}

fn manifest_md5() -> String {
    let v = crate::core::mcp_manifest::manifest_value();
    let canon = crate::core::canonical::canonical_value(&v);
    crate::core::hasher::hash_hex(canon.to_string().as_bytes())
}

fn ledger_content_md5(ledger: &HandoffLedgerV1) -> String {
    let mut tmp = ledger.clone();
    tmp.content_md5.clear();
    let v = serde_json::to_value(&tmp).unwrap_or(Value::Null);
    let canon = crate::core::canonical::canonical_value(&v);
    crate::core::hasher::hash_hex(canon.to_string().as_bytes())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct HandoffPackage {
    pub ledger: HandoffLedgerV1,
    pub intent: Option<IntentSnapshot>,
    pub context_snapshot: Option<ContextSnapshot>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct IntentSnapshot {
    pub task_type: String,
    pub scope: String,
    pub targets: Vec<String>,
    pub keywords: Vec<String>,
    pub language_hint: Option<String>,
    pub urgency: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ContextSnapshot {
    pub window_size: usize,
    pub tokens_used: usize,
    pub tokens_saved: usize,
    pub files_loaded: Vec<LoadedFileInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct LoadedFileInfo {
    pub path: String,
    pub mode: String,
    pub tokens: usize,
}

impl HandoffPackage {
    pub(crate) fn build(
        ledger: HandoffLedgerV1,
        intent: Option<&super::intent_engine::StructuredIntent>,
        context: Option<&crate::core::context_ledger::ContextLedger>,
    ) -> Self {
        let intent_snap = intent.map(|i| IntentSnapshot {
            task_type: i.task_type.as_str().to_string(),
            scope: match i.scope {
                super::intent_engine::IntentScope::SingleFile => "single_file",
                super::intent_engine::IntentScope::MultiFile => "multi_file",
                super::intent_engine::IntentScope::CrossModule => "cross_module",
                super::intent_engine::IntentScope::ProjectWide => "project_wide",
            }
            .to_string(),
            targets: i.targets.clone(),
            keywords: i.keywords.clone(),
            language_hint: i.language_hint.clone(),
            urgency: i.urgency,
        });

        let ctx_snap = context.map(|c| ContextSnapshot {
            window_size: c.window_size,
            tokens_used: c.total_tokens_sent,
            tokens_saved: c.total_tokens_saved,
            files_loaded: c
                .entries
                .iter()
                .map(|e| LoadedFileInfo {
                    path: e.path.clone(),
                    mode: e.mode.clone(),
                    tokens: e.sent_tokens,
                })
                .collect(),
        });

        HandoffPackage {
            ledger,
            intent: intent_snap,
            context_snapshot: ctx_snap,
        }
    }

    pub(crate) fn format_compact(&self) -> String {
        let mut out = String::new();

        out.push_str("--- HANDOFF ---\n");
        if let Some(ref intent) = self.intent {
            out.push_str(&format!(
                "TASK: {} (scope: {}, conf: {})\n",
                intent.task_type,
                intent.scope,
                if intent.urgency > 0.5 {
                    "URGENT"
                } else {
                    "normal"
                }
            ));
            if !intent.targets.is_empty() {
                out.push_str(&format!("TARGETS: {}\n", intent.targets.join(", ")));
            }
            if let Some(ref lang) = intent.language_hint {
                out.push_str(&format!("LANG: {lang}\n"));
            }
        }

        if let Some(ref ctx) = self.context_snapshot {
            out.push_str(&format!(
                "CTX: {}/{} tokens, {} files, saved {}\n",
                ctx.tokens_used,
                ctx.window_size,
                ctx.files_loaded.len(),
                ctx.tokens_saved,
            ));
        }

        if !self.ledger.session.decisions.is_empty() {
            out.push_str("DECISIONS:\n");
            for d in &self.ledger.session.decisions {
                out.push_str(&format!("  - {d}\n"));
            }
        }

        if !self.ledger.session.findings.is_empty() {
            out.push_str("FINDINGS:\n");
            for f in self.ledger.session.findings.iter().take(5) {
                out.push_str(&format!("  - {f}\n"));
            }
        }

        if !self.ledger.session.next_steps.is_empty() {
            out.push_str("NEXT:\n");
            for s in &self.ledger.session.next_steps {
                out.push_str(&format!("  - {s}\n"));
            }
        }

        out.push_str("---\n");
        out
    }
}

#[cfg(test)]
#[path = "handoff_ledger_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "handoff_ledger_lineage_tests.rs"]
mod lineage_tests;
