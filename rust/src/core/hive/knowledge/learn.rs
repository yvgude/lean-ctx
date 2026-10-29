// SPDX-License-Identifier: Apache-2.0
//! Bridges between LeanCTX project knowledge and Hive repo knowledge.
//!
//! * [`plan_learn`] turns what the agent learned in a project since the last
//!   draft into new repo facts: every fact through the capture gate
//!   ([`crate::core::hive::capture`]: one claim, a reference, evidence,
//!   dedupe, admission, screening), stable content ids, a lean-ctx
//!   provenance source.
//! * [`sync_into_knowledge`] makes a pulled revision available to the agent
//!   like an installed package: merged into the project knowledge store,
//!   tagged with `imported_from`, replaced on every new revision.

use chrono::{DateTime, Utc};
use sha2::{Digest, Sha256};

use crate::core::hive::capture::{self, CaptureRun, ReasonCode, dedupe};
use crate::core::knowledge::{KnowledgeFact, NodeType, ProjectKnowledge};
use crate::core::sensitivity::SensitivityLevel;
use lean_ctx_protocol::hive::knowledge::{FactV1, ResearchContextDocumentV1};

/// What [`plan_learn`] proposes.
#[derive(Debug, Clone, Default)]
pub struct LearnPlan {
    pub added: Vec<FactV1>,
    /// `(category/key, reason)` of every fact the gate refused; texts are
    /// never included.
    pub dropped: Vec<(String, ReasonCode)>,
    /// Near duplicates that bring references the held fact lacks: `(id of
    /// the held fact, new references)`, for the event builder (V4).
    pub merges: Vec<(String, Vec<String>)>,
    /// `created_at` of the newest fact considered; the next draft starts after it.
    pub watermark: Option<DateTime<Utc>>,
    /// [`plan_import`]: the import source of each fact in `added` (same
    /// order), `import:docs:<path>#<slug>` or `import:git:<sha>`.
    pub import_sources: Vec<String>,
    /// Every fact in `added`, as the gate passed it (same order): what the
    /// event path (C0 2.0) builds its `node.add` from.
    pub captured: Vec<capture::CapturedFact>,
}

/// Lowercase ASCII slug: alphanumerics kept, runs of anything else → `-`.
fn slug(s: &str, max: usize) -> String {
    let mut out = String::new();
    for c in s.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
    }
    let mut out: String = out.trim_matches('-').chars().take(max).collect();
    while out.ends_with('-') {
        out.pop();
    }
    out
}

fn short_hash(parts: &[&str]) -> String {
    let mut h = Sha256::new();
    for p in parts {
        h.update(p.as_bytes());
        h.update([0u8]);
    }
    hex::encode(&h.finalize()[..4])
}

/// Stable id from content: `lc-<key-slug>-<sha256(category, key, value)[..8]>`.
pub fn fact_id(category: &str, key: &str, value: &str) -> String {
    let hash = short_hash(&[category, key, value]);
    let s = slug(key, 60);
    if s.is_empty() {
        format!("lc-{hash}")
    } else {
        format!("lc-{s}-{hash}")
    }
}

/// The C0 1.x fact kind of a captured node type.
pub fn kind_of(node_type: NodeType) -> &'static str {
    match node_type {
        NodeType::Fact | NodeType::Finding => "observation",
        NodeType::Decision => "decision",
        NodeType::Gotcha => "gotcha",
        NodeType::Procedure => "procedure",
    }
}

/// The node type a held C0 1.x fact counts as for dedupe.
fn node_type_of_kind(kind: &str) -> NodeType {
    match kind {
        "decision" => NodeType::Decision,
        "gotcha" => NodeType::Gotcha,
        "procedure" => NodeType::Procedure,
        _ => NodeType::Fact,
    }
}

/// The text a C0 1.x fact carries for a captured candidate: the claim, or a
/// procedure's title with its numbered steps.
fn statement_of(c: &capture::CaptureCandidate) -> String {
    let mut text = c.statement.clone();
    if c.node_type != NodeType::Procedure {
        return text;
    }
    for (i, step) in c.steps.iter().enumerate() {
        text.push_str(&format!("\n{}. {step}", i + 1));
    }
    text
}

fn humanize(key: &str) -> String {
    key.split(['-', '_', '.', ':', '/'])
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// A fact the agent itself learned in this project: not imported (packages,
/// Hive revisions), current, not a machine-derived capture. Its sensitivity
/// is the gate's rule G11.
fn eligible(f: &KnowledgeFact) -> bool {
    f.imported_from.is_none()
        && f.is_current()
        && !crate::core::knowledge::is_machine_derived(&f.category, &f.key)
}

/// A fact `lean-ctx import docs|git` made from the repository itself (K1):
/// current, not machine-derived, public. Hive revisions and packages are
/// imported too, but never proposed back.
fn eligible_import(f: &KnowledgeFact) -> bool {
    matches!(f.imported_from.as_deref(), Some("docs" | "git"))
        && f.review.is_none()
        && f.is_current()
        && !crate::core::knowledge::is_machine_derived(&f.category, &f.key)
        && f.sensitivity == SensitivityLevel::Public
}

/// The contract's import source of an imported fact, from its anchor key:
/// `git:<sha>` → `import:git:<sha>`, `<path>#<section path>/<n>` →
/// `import:docs:<path>#<section slug>` (the section path through the
/// contract's `ctxpkg::import_slug`). `None` when the key names neither.
pub fn import_source(f: &KnowledgeFact) -> Option<String> {
    let source = match f.imported_from.as_deref()? {
        "git" => format!("import:git:{}", f.key.strip_prefix("git:")?),
        "docs" => {
            let (path, section) = f.key.split_once('#')?;
            let section = section.rsplit_once('/').map_or(section, |(head, _)| head);
            format!(
                "import:docs:{path}#{}",
                lean_ctx_protocol::hive::ctxpkg::import_slug(&section.replace('/', " "))
            )
        }
        _ => return None,
    };
    lean_ctx_protocol::hive::knowledge::import_source_ok(&source).then_some(source)
}

/// Plan the facts learned after `since` (and those waiting for their commit)
/// that pass the capture gate and the repo does not hold yet, at most
/// `limit` of them (oldest first).
pub fn plan_learn(
    knowledge: &ProjectKnowledge,
    repo: &ResearchContextDocumentV1,
    since: Option<DateTime<Utc>>,
    limit: usize,
    run: &mut CaptureRun<'_>,
) -> LearnPlan {
    plan_facts(knowledge, repo, since, limit, run, false)
}

/// Plan the imported facts (K1) new or changed after `since` that pass the
/// capture gate (evidence `imported`, the import source as reference) and
/// the repo does not hold yet, at most `limit` (oldest first), each with
/// its import source.
pub fn plan_import(
    knowledge: &ProjectKnowledge,
    repo: &ResearchContextDocumentV1,
    since: Option<DateTime<Utc>>,
    limit: usize,
    run: &mut CaptureRun<'_>,
) -> LearnPlan {
    plan_facts(knowledge, repo, since, limit, run, true)
}

/// When an imported fact last changed (a re-import updates in place).
fn changed_at(f: &KnowledgeFact) -> DateTime<Utc> {
    f.created_at.max(f.last_confirmed)
}

/// The reference an import source names: `file:<path>` for docs,
/// `commit:<sha>` for git history.
fn import_reference(source: &str) -> Option<String> {
    if let Some(sha) = source.strip_prefix("import:git:") {
        return Some(format!("commit:{}", sha.to_ascii_lowercase()));
    }
    let path = source.strip_prefix("import:docs:")?.split('#').next()?;
    Some(format!("file:{path}"))
}

/// The dedupe index of what the repo holds.
fn held_index(pk_id: &[u8; 32], repo: &ResearchContextDocumentV1) -> (dedupe::Index, Vec<String>) {
    let mut index = dedupe::Index::default();
    let mut ids = Vec::new();
    for f in &repo.facts {
        let node_type = node_type_of_kind(&f.kind);
        let refs = f
            .anchors
            .iter()
            .map(|a| format!("file:{}", a.path))
            .chain(f.sources.iter().cloned())
            .collect();
        index.insert(
            dedupe::exact_key(pk_id, node_type, &f.statement),
            dedupe::Known {
                node_type,
                signature: dedupe::signature(pk_id, &f.statement),
                refs,
            },
        );
        ids.push(f.id.clone());
    }
    (index, ids)
}

fn plan_facts(
    knowledge: &ProjectKnowledge,
    repo: &ResearchContextDocumentV1,
    since: Option<DateTime<Utc>>,
    limit: usize,
    run: &mut CaptureRun<'_>,
    imports: bool,
) -> LearnPlan {
    let mut plan = LearnPlan::default();
    // A learned fact counts from its creation, or from the `remember` that
    // last changed its capture fields (refs added to a fact the gate had
    // already rejected for having none).
    let stamp = |f: &KnowledgeFact| {
        if imports {
            changed_at(f)
        } else {
            f.capture
                .changed_at
                .map_or(f.created_at, |at| at.max(f.created_at))
        }
    };
    let mut facts: Vec<&KnowledgeFact> = knowledge
        .facts
        .iter()
        .filter(|f| {
            (if imports {
                eligible_import(f)
            } else {
                eligible(f)
            }) && (since.is_none_or(|since| stamp(f) > since) || run.is_pending(f))
        })
        .collect();
    facts.sort_by(|a, b| {
        (stamp(a), &a.category, &a.key, &a.value).cmp(&(stamp(b), &b.category, &b.key, &b.value))
    });

    let (mut index, mut held_ids) = held_index(&run.pk_id, repo);
    let mut seen_ids: std::collections::HashSet<String> = held_ids.iter().cloned().collect();

    for f in facts {
        if plan.added.len() == limit {
            run.counts.reject(ReasonCode::BudgetBatchFull);
            break;
        }
        plan.watermark = plan.watermark.max(Some(stamp(f)));
        let import = if imports {
            let Some(source) = import_source(f) else {
                continue;
            };
            Some(source)
        } else {
            None
        };
        run.counts.considered += 1;
        let label = format!("{}/{}", f.category, f.key);
        let verdict = capture_one(f, import.as_deref(), run, &mut index);
        let Captured {
            candidate,
            refs,
            anchors,
        } = match verdict {
            Ok(ok) => ok,
            Err(Rejected::Reason(reason)) => {
                run.counts.reject(reason);
                plan.dropped.push((label, reason));
                continue;
            }
            Err(Rejected::Merge { index: i, refs }) => {
                run.counts.reject(ReasonCode::NearDuplicateMerged);
                plan.dropped.push((label, ReasonCode::NearDuplicateMerged));
                if let Some(id) = held_ids.get(i) {
                    plan.merges.push((id.clone(), refs));
                }
                continue;
            }
        };
        let statement = statement_of(&candidate);
        let id = fact_id(&f.category, &f.key, &statement);
        if !seen_ids.insert(id.clone()) {
            run.counts.reject(ReasonCode::Duplicate);
            plan.dropped.push((label, ReasonCode::Duplicate));
            continue;
        }
        held_ids.push(id.clone());
        let applies = humanize(&f.key);
        let applies_to = if applies.is_empty() || applies.chars().count() > 500 {
            Vec::new()
        } else {
            vec![applies]
        };
        let mut sources = vec![format!(
            "lean-ctx:knowledge/{}/{}#{}",
            slug(&f.category, 60),
            slug(&f.key, 80),
            short_hash(&[&f.category, &f.key, &statement])
        )];
        sources.extend(refs.iter().cloned());
        run.counts.captured += 1;
        plan.import_sources.extend(import);
        plan.captured.push(capture::CapturedFact {
            candidate: candidate.clone(),
            refs,
            category: f.category.clone(),
            key: f.key.clone(),
            value: f.value.clone(),
            derived_from: Vec::new(),
        });
        plan.added.push(FactV1 {
            id,
            statement,
            kind: kind_of(candidate.node_type).to_string(),
            applies_to,
            sources,
            from_contributions: Vec::new(),
            anchors,
        });
    }
    plan
}

enum Rejected {
    Reason(ReasonCode),
    Merge { index: usize, refs: Vec<String> },
}

impl From<ReasonCode> for Rejected {
    fn from(reason: ReasonCode) -> Self {
        Self::Reason(reason)
    }
}

/// The capture pipeline for one fact (SPEC-ENGINE §3.1.2 steps 2–5): shape,
/// gate rules, admission, screening, dedupe. On success the candidate joins
/// `index` and its valid references are returned.
fn capture_one(
    f: &KnowledgeFact,
    import: Option<&str>,
    run: &mut CaptureRun<'_>,
    index: &mut dedupe::Index,
) -> Result<Captured, Rejected> {
    // J2: anchors go along when their paths and symbols pass screening (a
    // path is information); otherwise the fact goes without them, and they
    // do not count as its references either.
    let anchors_ok = f.anchors.iter().all(|a| {
        run.screen
            .check(&format!(
                "{}\n{}",
                a.path,
                a.symbol.as_deref().unwrap_or_default()
            ))
            .is_ok()
    });
    let mut shaped = f.clone();
    if !anchors_ok {
        shaped.anchors.clear();
    }
    let candidate = capture::shape(&shaped, import.and_then(import_reference))?;
    let refs = match capture::gate(&candidate, &mut run.checkout, run.sealed) {
        Ok(refs) => refs,
        Err(reason) => {
            if reason == ReasonCode::UnpublishedRef {
                run.unpublished(f);
            } else {
                run.settled(f);
            }
            return Err(reason.into());
        }
    };
    run.settled(f);
    let mut texts = vec![
        f.category.clone(),
        f.key.clone(),
        candidate.statement.clone(),
    ];
    texts.extend(candidate.steps.iter().cloned());
    texts.extend(candidate.commands.iter().map(|c| c.cmd.clone()));
    texts.extend(refs.iter().cloned());
    for text in &texts {
        if crate::core::context_admission::carrier::admit_for_store_under(text, None)
            .ok()
            .is_none_or(|admitted| admitted != *text)
        {
            return Err(ReasonCode::Admission.into());
        }
    }
    run.screen
        .check(&texts.join("\n"))
        .map_err(|b| ReasonCode::of_screen(&b))?;
    let key = dedupe::exact_key(&run.pk_id, candidate.node_type, &candidate.statement);
    let known = dedupe::Known {
        node_type: candidate.node_type,
        signature: dedupe::signature(&run.pk_id, &candidate.statement),
        refs: refs.clone(),
    };
    match index.verdict(&key, &known) {
        dedupe::Verdict::Duplicate => Err(ReasonCode::Duplicate.into()),
        dedupe::Verdict::Merge { index: i, refs } => Err(Rejected::Merge { index: i, refs }),
        dedupe::Verdict::New => {
            index.insert(key, known);
            Ok(Captured {
                candidate,
                refs,
                anchors: shaped.anchors,
            })
        }
    }
}

/// A fact that passed the gate.
struct Captured {
    candidate: capture::CaptureCandidate,
    /// Its valid references.
    refs: Vec<String>,
    /// The anchors that go with it.
    anchors: Vec<crate::core::knowledge::anchors::CodeAnchor>,
}

/// Project-knowledge category for a Hive repo's facts.
pub fn knowledge_category(ns: &str, name: &str) -> String {
    format!("hive/{ns}/{name}")
}

/// `imported_from` of every fact pulled from a Hive vault starts with this.
/// It is stored in the project knowledge, so it keeps its pre-VA name: an
/// older engine (downgrade) still recognizes and replaces these facts.
pub(crate) const IMPORT_TAG_SCHEME: &str = "hive-repo:";
/// The vault spelling, read as well (written by development builds).
const VAULT_IMPORT_TAG_SCHEME: &str = "hive-vault:";

/// A Hive import tag without its scheme (`@ns/name@rN`), `None` for every
/// other origin.
pub(crate) fn import_tag_source(origin: &str) -> Option<&str> {
    origin
        .strip_prefix(IMPORT_TAG_SCHEME)
        .or_else(|| origin.strip_prefix(VAULT_IMPORT_TAG_SCHEME))
}

pub(crate) fn import_tag_prefix(ns: &str, name: &str) -> String {
    format!("@{ns}/{name}@r")
}

/// The project-knowledge key of a pulled fact: its Hive id when that is a
/// strict token (`[A-Za-z0-9][A-Za-z0-9._-]{0,79}`: every id engines and
/// Hive make, `lc-…`, `hive.…`, ULIDs), otherwise a digest of it.
/// Renderers show keys outside the X5 fence, so a key carries no spaces,
/// punctuation or length to phrase anything in; J2 reports the key back as
/// the Hive id, so real ids stay as they are.
fn pulled_fact_key(id: &str) -> String {
    let bytes = id.as_bytes();
    let strict = !bytes.is_empty()
        && bytes.len() <= 80
        && bytes[0].is_ascii_alphanumeric()
        && bytes
            .iter()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'));
    if strict {
        id.to_string()
    } else {
        format!("fact-{}", short_hash(&[id]))
    }
}

/// Whether the vault `@ns/name` is delivered from a local C0 2.0 replica
/// (SPEC-ENGINE E2, V4.10): its revisions are then no longer copied into
/// the project knowledge.
pub fn served_by_replica(ns: &str, name: &str) -> bool {
    let Ok(store) = crate::core::hive::HiveStore::open() else {
        return false;
    };
    crate::core::hive::vault::VaultMap::load(store.data_dir())
        .vaults
        .values()
        .find(|r| r.ns == ns && r.name == name)
        .is_some_and(|r| {
            crate::core::hive::replica::store::place_dir(store.data_dir(), &r.vault_id)
                .join("replica.db")
                .exists()
        })
}

/// Remove the facts copied in from `@ns/name` revisions. Returns how many.
pub fn remove_pulled(project_root: &str, ns: &str, name: &str) -> Result<usize, String> {
    let prefix = import_tag_prefix(ns, name);
    let (_, removed) = ProjectKnowledge::mutate_locked(project_root, |k| {
        let before = k.facts.len();
        k.facts.retain(|f| {
            !f.imported_from
                .as_deref()
                .and_then(import_tag_source)
                .is_some_and(|source| source.starts_with(&prefix))
        });
        let removed = before - k.facts.len();
        if removed > 0 {
            k.rebuild_index();
        }
        removed
    })?;
    Ok(removed)
}

/// Replace this repo's facts in the project knowledge store with `doc`.
/// Returns `(loaded, removed)`. A vault served by a replica only loses its
/// old copies (its knowledge is delivered from the replica, §3.5).
pub fn sync_into_knowledge(
    project_root: &str,
    ns: &str,
    name: &str,
    revision: u64,
    doc: &ResearchContextDocumentV1,
) -> Result<(usize, usize), String> {
    if served_by_replica(ns, name) {
        return Ok((0, remove_pulled(project_root, ns, name)?));
    }
    let policy = crate::core::memory_policy::MemoryPolicy::default();
    let prefix = import_tag_prefix(ns, name);
    let tag = format!("{IMPORT_TAG_SCHEME}{prefix}{revision}");
    let category = knowledge_category(ns, name);
    let (_, counts) = ProjectKnowledge::mutate_locked(project_root, |k| {
        let before = k.facts.len();
        k.facts.retain(|f| {
            !f.imported_from
                .as_deref()
                .and_then(import_tag_source)
                .is_some_and(|source| source.starts_with(&prefix))
        });
        let removed = before - k.facts.len();
        if removed > 0 {
            k.rebuild_index();
        }
        let mut loaded = 0usize;
        for fact in &doc.facts {
            // X5: other members' text is stored fenced, so every renderer
            // and every consumer of the store only ever sees it wrapped; the
            // key stands outside the fence and is only ever a strict slug.
            let key = pulled_fact_key(&fact.id);
            let value = crate::core::hive::untrusted::wrap(
                crate::core::hive::untrusted::UntrustedKind::Knowledge,
                &tag,
                &fact.statement,
            );
            k.remember(&category, &key, &value, &tag, 0.8, &policy);
            if let Some(last) = k
                .facts
                .iter_mut()
                .rev()
                .find(|f| f.category == category && f.key == key && f.is_current())
            {
                last.imported_from = Some(tag.clone());
                // J2: the code anchors travel with the fact, so recall marks
                // it "to verify" once the anchored code changes here.
                last.anchors.clone_from(&fact.anchors);
                loaded += 1;
            }
        }
        (loaded, removed)
    })?;
    Ok(counts)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_and_kinds_are_stable() {
        assert_eq!(
            fact_id("decision", "db-choice", "Postgres"),
            fact_id("decision", "db-choice", "Postgres")
        );
        assert_ne!(
            fact_id("decision", "db-choice", "Postgres"),
            fact_id("decision", "db-choice", "MySQL")
        );
        assert!(fact_id("decision", "DB Choice!", "x").starts_with("lc-db-choice-"));
        assert_eq!(kind_of(NodeType::Decision), "decision");
        assert_eq!(kind_of(NodeType::Finding), "observation");
    }

    /// V4.10: once a replica serves the vault, the facts its 1.x revisions
    /// copied in are removed; own facts and other vaults' copies stay.
    #[test]
    fn pulled_copies_of_a_vault_are_removed_and_nothing_else() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_string_lossy().into_owned();
        let doc = |statement: &str| ResearchContextDocumentV1 {
            facts: vec![FactV1 {
                id: "f1".into(),
                statement: statement.into(),
                kind: "observation".into(),
                applies_to: vec![],
                sources: vec![],
                from_contributions: vec![],
                anchors: vec![],
            }],
            ..ResearchContextDocumentV1::default()
        };
        sync_into_knowledge(&root, "acme", "pay", 3, &doc("Pay copies this claim.")).unwrap();
        sync_into_knowledge(&root, "acme", "other", 1, &doc("Other vault claim here.")).unwrap();
        let policy = crate::core::memory_policy::MemoryPolicy::default();
        ProjectKnowledge::mutate_locked(&root, |k| {
            k.remember("arch", "own", "My own claim stays here.", "s", 0.9, &policy);
        })
        .unwrap();

        assert_eq!(remove_pulled(&root, "acme", "pay").unwrap(), 1);
        let k = ProjectKnowledge::load(&root).unwrap();
        let values: Vec<&str> = k.facts.iter().map(|f| f.value.as_str()).collect();
        assert!(values.iter().any(|v| v.contains("Other vault claim")));
        assert!(values.contains(&"My own claim stays here."));
        assert!(!values.iter().any(|v| v.contains("Pay copies")));
        assert_eq!(remove_pulled(&root, "acme", "pay").unwrap(), 0);
    }
}
