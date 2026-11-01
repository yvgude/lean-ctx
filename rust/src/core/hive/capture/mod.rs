// SPDX-License-Identifier: Apache-2.0
//! Capture quality gate (Hive v4 V1, SPEC-ENGINE §3.1).
//!
//! Every item that leaves the machine for a vault is one typed claim:
//! a single complete sentence of 24–300 characters, a node type, at least
//! one valid reference, an evidence level, deduplicated against what the
//! place holds, admitted and screened. The gate is rule-based: it rejects
//! with a stable [`ReasonCode`], it never rewrites or splits (E1). A fact
//! that fails stays in the local knowledge store unchanged.

pub mod dedupe;
pub mod derived;
pub mod emit;
pub mod episode;
pub mod lifecycle;
pub mod refs;
pub mod rules;

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::core::knowledge::{
    EvidenceLevel, FactOrigin, KnowledgeArchetype, KnowledgeFact, NodeType, ProcedureCommand,
};
use crate::core::sensitivity::SensitivityLevel;

use self::refs::{Checkout, RefState};

/// Version of the gate rules, carried with every count and event.
pub const GATE_VERSION: u32 = 1;
/// References a claim may carry (G10).
pub const MAX_REFERENCES: usize = 10;
/// A candidate whose only reference is an unpublished commit waits this
/// long before it is rejected finally (G10).
pub const PENDING_DAYS: i64 = 14;

/// Why the gate refused a candidate. Stable snake_case strings: counted,
/// shown in `hive status`, reported to the place as counts only.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReasonCode {
    NoType,
    TooShort,
    TooLong,
    MultiLine,
    MultiClaim,
    NotSentence,
    LogText,
    Truncated,
    NoReference,
    TooManyReferences,
    Sensitivity,
    ProcedureShape,
    EpisodeShape,
    ScreenSecret,
    ScreenPii,
    ScreenRedaction,
    ScreenForbidden,
    ScreenLocalPath,
    Duplicate,
    NearDuplicateMerged,
    BudgetBatchFull,
    Admission,
    PurgedContent,
    UnpublishedRef,
}

impl ReasonCode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NoType => "no_type",
            Self::TooShort => "too_short",
            Self::TooLong => "too_long",
            Self::MultiLine => "multi_line",
            Self::MultiClaim => "multi_claim",
            Self::NotSentence => "not_sentence",
            Self::LogText => "log_text",
            Self::Truncated => "truncated",
            Self::NoReference => "no_reference",
            Self::TooManyReferences => "too_many_references",
            Self::Sensitivity => "sensitivity",
            Self::ProcedureShape => "procedure_shape",
            Self::EpisodeShape => "episode_shape",
            Self::ScreenSecret => "screen_secret",
            Self::ScreenPii => "screen_pii",
            Self::ScreenRedaction => "screen_redaction",
            Self::ScreenForbidden => "screen_forbidden",
            Self::ScreenLocalPath => "screen_local_path",
            Self::Duplicate => "duplicate",
            Self::NearDuplicateMerged => "near_duplicate_merged",
            Self::BudgetBatchFull => "budget_batch_full",
            Self::Admission => "admission",
            Self::PurgedContent => "purged_content",
            Self::UnpublishedRef => "unpublished_ref",
        }
    }

    /// Why a fact cannot be shared as is, in plain words, with how to fix
    /// it where a person can (`knowledge remember`, the web's "can't be
    /// shared as is").
    pub fn plain(self) -> &'static str {
        match self {
            Self::NoType => "lean-ctx cannot tell what kind of knowledge it is",
            Self::TooShort => "it is shorter than 24 characters; write one full sentence",
            Self::TooLong => "it is longer than 300 characters; keep it to one sentence",
            Self::MultiLine => "it spans several lines; write it as one sentence",
            Self::MultiClaim => "it says several things at once; remember one fact per sentence",
            Self::NotSentence => {
                "it is not a full sentence; start with a capital letter and end with a period"
            }
            Self::LogText => "it looks like log or tool output, not a statement",
            Self::Truncated => "it looks cut off; finish the sentence",
            Self::NoReference => {
                "it names no source others can check; add --ref file:<path> (or sym:, test:, commit:, https://)"
            }
            Self::TooManyReferences => "it names more than 10 sources",
            Self::Sensitivity => "it is marked as more sensitive than this vault allows",
            Self::ProcedureShape => "the steps or commands of this procedure are not complete",
            Self::EpisodeShape => "the episode summary is not complete",
            Self::ScreenSecret => "it contains what looks like a secret",
            Self::ScreenPii => "it contains personal data",
            Self::ScreenRedaction => "it contains text your filters redact",
            Self::ScreenForbidden => "your workspace's policy does not allow sharing it",
            Self::ScreenLocalPath => "it contains a path that only exists on this machine",
            Self::Duplicate => "the vault already has it",
            Self::NearDuplicateMerged => "the vault already has almost the same fact",
            Self::BudgetBatchFull => "this sync's batch is full; it goes with the next one",
            Self::Admission => "this vault does not take knowledge from this machine",
            Self::PurgedContent => "it was removed from the vault before",
            Self::UnpublishedRef => {
                "its only source is a commit that is not pushed yet; it shares once it is"
            }
        }
    }

    /// The screening reason (1:1 to [`Blocked`](crate::core::hive::knowledge::screening::Blocked)).
    pub fn of_screen(blocked: &crate::core::hive::knowledge::screening::Blocked) -> Self {
        use crate::core::hive::knowledge::screening::Blocked;
        match blocked {
            Blocked::Secret(_) => Self::ScreenSecret,
            Blocked::Pii(_) => Self::ScreenPii,
            Blocked::Redaction => Self::ScreenRedaction,
            // A broken local rule refuses like a forbidden pattern (fails closed).
            Blocked::Forbidden | Blocked::BrokenRules(_) => Self::ScreenForbidden,
            Blocked::LocalPath => Self::ScreenLocalPath,
        }
    }
}

impl std::fmt::Display for ReasonCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A local fact shaped for the gate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptureCandidate {
    pub node_type: NodeType,
    /// The claim; the title of a procedure.
    pub statement: String,
    pub steps: Vec<String>,
    pub commands: Vec<ProcedureCommand>,
    /// Raw references: the fact's `refs`, its anchors as `file:`/`sym:`, and
    /// its import source.
    pub refs: Vec<String>,
    pub evidence: EvidenceLevel,
    pub sensitivity: SensitivityLevel,
}

/// A fact the gate let through, with its valid references and where it
/// came from in the local store (for `own_sources`, never shared).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapturedFact {
    pub candidate: CaptureCandidate,
    pub refs: Vec<String>,
    pub category: String,
    pub key: String,
    pub value: String,
    /// Live vault nodes the fact names itself (`node:<id>` references,
    /// [`derived::link`]); each becomes a `derived_from` edge next to the
    /// fact's `node.add` when the claim body carries the reference.
    pub derived_from: Vec<String>,
}

/// Category tokens that never make a shareable node (G1).
const DENY_TOKENS: [&str; 5] = ["log", "trace", "debug", "output", "temp"];

/// Node type of a fact (SPEC-ENGINE §3.1.3), `Err(NoType)` for a category
/// in the deny map.
pub fn node_type_of(f: &KnowledgeFact) -> Result<NodeType, ReasonCode> {
    if let Some(t) = f.capture.node_type {
        return Ok(t);
    }
    match f.archetype {
        KnowledgeArchetype::Decision => return Ok(NodeType::Decision),
        KnowledgeArchetype::Gotcha => return Ok(NodeType::Gotcha),
        KnowledgeArchetype::Workflow => {
            return Ok(if f.capture.steps.is_empty() {
                NodeType::Fact
            } else {
                NodeType::Procedure
            });
        }
        _ => {}
    }
    let category = f.category.to_lowercase();
    let tokens: Vec<&str> = category
        .split(['-', '_', ':', '/', ' '])
        .filter(|t| !t.is_empty())
        .collect();
    let has = |words: &[&str]| tokens.iter().any(|t| words.contains(t));
    if has(&DENY_TOKENS) {
        Err(ReasonCode::NoType)
    } else if has(&["decision", "adr"]) {
        Ok(NodeType::Decision)
    } else if has(&["gotcha", "pitfall", "warning"]) {
        Ok(NodeType::Gotcha)
    } else if has(&["procedure", "howto", "runbook"]) && !f.capture.steps.is_empty() {
        Ok(NodeType::Procedure)
    } else {
        Ok(NodeType::Fact)
    }
}

/// Evidence level of a fact: explicit; `imported` for imports and provider
/// or derived origins; `observed` for a local fact with an anchor; else
/// `stated`.
pub fn evidence_of(f: &KnowledgeFact, imported: bool) -> EvidenceLevel {
    if let Some(e) = f.capture.evidence {
        return e;
    }
    match &f.origin {
        _ if imported => EvidenceLevel::Imported,
        FactOrigin::Provider(_) | FactOrigin::Derived(_) => EvidenceLevel::Imported,
        FactOrigin::Local if !f.anchors.is_empty() => EvidenceLevel::Observed,
        FactOrigin::Local | FactOrigin::Unverified => EvidenceLevel::Stated,
    }
}

/// Shape a fact into a candidate (G1). `import_ref` is the reference its
/// import source names (`file:<path>` or `commit:<sha>`).
pub fn shape(
    f: &KnowledgeFact,
    import_ref: Option<String>,
) -> Result<CaptureCandidate, ReasonCode> {
    let node_type = if import_ref.is_some() {
        NodeType::Fact
    } else {
        node_type_of(f)?
    };
    let mut refs = f.capture.refs.clone();
    for anchor in &f.anchors {
        refs.push(match &anchor.symbol {
            Some(symbol) => format!("sym:{}::{symbol}", anchor.path),
            None => format!("file:{}", anchor.path),
        });
    }
    refs.extend(import_ref);
    let mut seen = std::collections::HashSet::new();
    refs.retain(|r| seen.insert(r.clone()));
    Ok(CaptureCandidate {
        node_type,
        statement: f.value.trim().to_string(),
        steps: f.capture.steps.clone(),
        commands: f.capture.commands.clone(),
        refs,
        evidence: evidence_of(f, f.imported_from.is_some()),
        sensitivity: f.sensitivity,
    })
}

/// Procedure shape (SPEC-ENGINE §3.1.4): title, 1–20 steps, 0–10 commands.
fn check_procedure(c: &CaptureCandidate) -> Result<(), ReasonCode> {
    let title = &c.statement;
    rules::check_length(title, 8, 120).map_err(|_| ReasonCode::NotSentence)?;
    rules::check_single_line(title).map_err(|_| ReasonCode::NotSentence)?;
    rules::check_sentence(title, false)?;
    let shape = |ok: bool| {
        if ok {
            Ok(())
        } else {
            Err(ReasonCode::ProcedureShape)
        }
    };
    shape((1..=20).contains(&c.steps.len()))?;
    for step in &c.steps {
        shape(
            rules::check_length(step, 8, 300).is_ok()
                && rules::check_single_line(step).is_ok()
                && rules::check_log(step).is_ok()
                && rules::check_truncated(step).is_ok(),
        )?;
    }
    shape(c.commands.len() <= 10)?;
    for cmd in &c.commands {
        shape(
            matches!(cmd.shell.as_str(), "sh" | "pwsh" | "cmd")
                && !cmd.cmd.trim().is_empty()
                && cmd.cmd.chars().count() <= 500
                && cmd
                    .cwd_hint
                    .as_ref()
                    .is_none_or(|h| h.chars().count() <= 120),
        )?;
    }
    Ok(())
}

/// The rules G2–G11 on a shaped candidate. Returns the valid references.
pub fn gate(
    c: &CaptureCandidate,
    checkout: &mut Checkout,
    sealed: bool,
) -> Result<Vec<String>, ReasonCode> {
    if c.node_type == NodeType::Procedure {
        check_procedure(c)?;
    } else {
        rules::check_statement(&c.statement)?;
    }
    let mut valid = Vec::new();
    let mut unpublished = false;
    for raw in &c.refs {
        let Some(parsed) = refs::parse(raw) else {
            continue;
        };
        match checkout.check(&parsed) {
            RefState::Valid => {
                let r = parsed.to_contract();
                if !valid.contains(&r) {
                    valid.push(r);
                }
            }
            RefState::Unpublished => unpublished = true,
            RefState::Invalid => {}
        }
    }
    if valid.is_empty() {
        return Err(if unpublished {
            ReasonCode::UnpublishedRef
        } else {
            ReasonCode::NoReference
        });
    }
    if valid.len() > MAX_REFERENCES {
        return Err(ReasonCode::TooManyReferences);
    }
    let allowed = if sealed {
        c.sensitivity <= SensitivityLevel::Internal
    } else {
        c.sensitivity == SensitivityLevel::Public
    };
    if !allowed {
        return Err(ReasonCode::Sensitivity);
    }
    Ok(valid)
}

/// Counts of capture runs (SPEC-ENGINE §3.1.2 step 7): never any text.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CaptureCounts {
    pub gate_version: u32,
    pub window_from: Option<i64>,
    pub window_to: Option<i64>,
    pub considered: u32,
    pub captured: u32,
    pub rejected: BTreeMap<ReasonCode, u32>,
}

impl CaptureCounts {
    pub fn reject(&mut self, reason: ReasonCode) {
        *self.rejected.entry(reason).or_default() += 1;
    }

    /// Add a run's counts to these (the window grows to cover both).
    pub fn absorb(&mut self, run: &CaptureCounts) {
        self.gate_version = GATE_VERSION;
        self.window_from = match (self.window_from, run.window_from) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
        self.window_to = self.window_to.max(run.window_to);
        self.considered += run.considered;
        self.captured += run.captured;
        for (reason, n) in &run.rejected {
            *self.rejected.entry(*reason).or_default() += n;
        }
    }

    /// `considered / captured / rejected by reason`, one line.
    pub fn summary(&self) -> String {
        let mut line = format!("{} considered, {} captured", self.considered, self.captured);
        if !self.rejected.is_empty() {
            let reasons: Vec<String> = self
                .rejected
                .iter()
                .map(|(r, n)| format!("{r} {n}"))
                .collect();
            line.push_str(&format!(", rejected: {}", reasons.join(", ")));
        }
        line
    }
}

/// Unsent counts per vault, `<data>/hive/capture/counts.json`.
pub type CountsFile = BTreeMap<String, CaptureCounts>;

fn capture_dir(data_dir: &Path) -> std::path::PathBuf {
    data_dir.join("capture")
}

pub fn load_counts(data_dir: &Path) -> CountsFile {
    std::fs::read(capture_dir(data_dir).join("counts.json"))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

/// Add one run's counts to the vault's unsent counts.
pub fn record_counts(
    data_dir: &Path,
    vault: &str,
    run: &CaptureCounts,
) -> Result<(), crate::core::hive::HiveError> {
    let mut all = load_counts(data_dir);
    all.entry(vault.to_string()).or_default().absorb(run);
    let bytes =
        serde_json::to_vec(&all).map_err(|e| crate::core::hive::HiveError::Local(e.to_string()))?;
    crate::core::hive::state::write_private(&capture_dir(data_dir), "counts.json", &bytes)
}

/// A candidate waiting for its commit to reach a remote-tracking branch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pending {
    pub fact_key_hash: String,
    pub first_seen_ms: i64,
}

fn pending_file(vault: &str) -> String {
    format!("{vault}.pending.json")
}

pub fn load_pending(data_dir: &Path, vault: &str) -> Vec<Pending> {
    std::fs::read(capture_dir(data_dir).join(pending_file(vault)))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

pub fn save_pending(
    data_dir: &Path,
    vault: &str,
    pending: &[Pending],
) -> Result<(), crate::core::hive::HiveError> {
    let bytes = serde_json::to_vec(pending)
        .map_err(|e| crate::core::hive::HiveError::Local(e.to_string()))?;
    crate::core::hive::state::write_private(&capture_dir(data_dir), &pending_file(vault), &bytes)
}

/// `hex(blake3_keyed(pk_id, category || 0x00 || key))`: names a local fact
/// in the pending file without its slug.
pub fn fact_key_hash(pk_id: &[u8; 32], f: &KnowledgeFact) -> String {
    let mut h = blake3::Hasher::new_keyed(pk_id);
    h.update(f.category.as_bytes());
    h.update(&[0]);
    h.update(f.key.as_bytes());
    h.finalize().to_hex().to_string()
}

/// One capture run for one place: the gate's inputs and what it counted.
pub struct CaptureRun<'a> {
    pub screen: &'a mut crate::core::hive::knowledge::screening::Screen,
    pub checkout: Checkout,
    pub pk_id: [u8; 32],
    pub sealed: bool,
    pub counts: CaptureCounts,
    pub pending: Vec<Pending>,
    pub now_ms: i64,
}

impl<'a> CaptureRun<'a> {
    pub fn new(
        screen: &'a mut crate::core::hive::knowledge::screening::Screen,
        root: &Path,
        pk_id: [u8; 32],
        sealed: bool,
        pending: Vec<Pending>,
        now_ms: i64,
    ) -> Self {
        Self {
            screen,
            checkout: Checkout::new(root),
            pk_id,
            sealed,
            counts: CaptureCounts {
                gate_version: GATE_VERSION,
                window_from: Some(now_ms),
                window_to: Some(now_ms),
                ..CaptureCounts::default()
            },
            pending,
            now_ms,
        }
    }

    /// Whether `f` waits for its commit (it is re-checked on every run).
    pub fn is_pending(&self, f: &KnowledgeFact) -> bool {
        let hash = fact_key_hash(&self.pk_id, f);
        self.pending.iter().any(|p| p.fact_key_hash == hash)
    }

    /// Record an `unpublished_ref` verdict: pending for [`PENDING_DAYS`],
    /// then rejected finally (the caller counts it once per run either way).
    pub fn unpublished(&mut self, f: &KnowledgeFact) {
        let hash = fact_key_hash(&self.pk_id, f);
        let limit = PENDING_DAYS * 86_400_000;
        match self.pending.iter().position(|p| p.fact_key_hash == hash) {
            Some(i) if self.now_ms - self.pending[i].first_seen_ms >= limit => {
                self.pending.remove(i);
            }
            Some(_) => {}
            None => self.pending.push(Pending {
                fact_key_hash: hash,
                first_seen_ms: self.now_ms,
            }),
        }
    }

    /// `f` passed (or failed for another reason): it no longer waits.
    pub fn settled(&mut self, f: &KnowledgeFact) {
        let hash = fact_key_hash(&self.pk_id, f);
        self.pending.retain(|p| p.fact_key_hash != hash);
    }
}

/// Open a capture run for the project `stable` at `root`: the vault it is
/// bound to (or the project itself before it is), its pending candidates.
/// Returns the run and the place name its counts are kept under.
pub fn open_run<'a>(
    data_dir: &Path,
    stable: &str,
    root: &Path,
    screen: &'a mut crate::core::hive::knowledge::screening::Screen,
    now_ms: i64,
) -> (CaptureRun<'a>, String) {
    let record = crate::core::hive::vault::VaultMap::load(data_dir)
        .vaults
        .get(stable)
        .cloned();
    let place = record
        .as_ref()
        .map_or_else(|| stable.to_string(), |r| r.vault_id.clone());
    let sealed = record.is_some_and(|r| r.sealed);
    let pending = load_pending(data_dir, &place);
    let run = CaptureRun::new(screen, root, local_pk_id(&place), sealed, pending, now_ms);
    (run, place)
}

/// Keep what a run found: its counts and the candidates still pending.
pub fn finish_run(
    data_dir: &Path,
    place: &str,
    run: &CaptureRun<'_>,
) -> Result<(), crate::core::hive::HiveError> {
    if run.counts.considered == 0 && run.counts.rejected.is_empty() {
        return Ok(());
    }
    record_counts(data_dir, place, &run.counts)?;
    save_pending(data_dir, place, &run.pending)
}

/// The place id key of a vault until the replica delivers the server's
/// `id_salt` (SPEC-ENGINE §3.1.5).
// lean-ctx: derived from the vault id, so local-only; replace with the
// snapshot's `id_salt` / sealed `place_secret` when the replica lands (V4).
pub fn local_pk_id(vault: &str) -> [u8; 32] {
    blake3::derive_key("leanctx.hive.capture.pk-local.v1", vault.as_bytes())
}

#[cfg(test)]
pub(crate) mod tests;
