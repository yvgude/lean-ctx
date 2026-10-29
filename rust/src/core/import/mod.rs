//! Import facts from local AI coding-session histories, and from the
//! project's own instruction files, documentation and git history (K1).

mod bounded;
pub mod claude_code;
pub mod codex;
pub mod cursor;
pub(crate) mod database;
pub mod git_history;
pub mod markdown;
pub mod opencode;
mod staging;

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use chrono::Utc;
use serde_json::Value;

use crate::core::knowledge::KnowledgeFact;
use staging::{Gateway, Origin, Rules, joined_leaves, mask_strings};

const MAX_FACT_VALUE_CHARS: usize = 500;
const MAX_ERRORS: usize = 100;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportSource {
    ClaudeCode,
    Codex,
    Cursor,
    OpenCode,
    /// The project's instruction files and `docs/` ([`markdown`]).
    Docs,
    /// The project's local git history ([`git_history`]).
    Git,
}

impl ImportSource {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ClaudeCode => "claude-code",
            Self::Codex => "codex",
            Self::Cursor => "cursor",
            Self::OpenCode => "opencode",
            Self::Docs => "docs",
            Self::Git => "git",
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct ImportResult {
    pub sessions_found: usize,
    pub facts_extracted: usize,
    pub files_touched: usize,
    pub errors: Vec<String>,
    /// Facts remain available to the CLI until it either persists or displays them.
    pub facts: Vec<KnowledgeFact>,
    /// Facts that were found but not kept: admission withholds their value
    /// or permits it for a reader only (E3). A count — the values are gone.
    pub facts_skipped: usize,
    /// History sources that were found but not read to the end: above the
    /// total cap of their kind, holding one record above the per-record
    /// bound, or not readable. Never read in part.
    pub sources_skipped: usize,
    /// History sources that were not opened or not used: outside every
    /// named root, withheld by a path rule, the policy or the gateway, or
    /// changed between their verification and the read.
    pub sources_refused: usize,
    /// What a repository-source run read (`import docs`): facts of a file
    /// it read that were not found again lost their section.
    pub repo_scope: Option<RepoScope>,
}

/// The files a docs import read, relative to the project root, and whether
/// it covered the whole default set (then a file that is gone lost all its
/// facts).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RepoScope {
    pub files: Vec<String>,
    pub whole: bool,
}

/// Why a history source (a transcript file, a history database) was not
/// read. Typed and content-free: no path, no byte of the source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceRefusal {
    /// It does not resolve inside any named root (symlinks resolved).
    OutsideRoot,
    /// A path rule, the project policy or the gateway withholds it, or it
    /// is delivered for a reader only and may not feed a store (E3).
    Withheld,
    /// It is larger than the total cap of its kind
    /// (`bounded::MAX_TRANSCRIPT_BYTES`, `database::MAX_HISTORY_DATABASE_BYTES`;
    /// the read cap for a single JSON document, which is one record). A
    /// source is never read in part.
    TooLarge,
    /// One record of a transcript is larger than the per-record bound (the
    /// read cap). A record is never cut and never passed uninspected, so the
    /// transcript is not imported.
    RecordTooLarge,
    /// A transcript above the read cap cannot be admitted piece by piece
    /// without being judged more weakly than as a whole: a rule that is
    /// active now can match across lines without a bound (a policy-pack or
    /// configured pattern not proven single-line, a semantic detector), or
    /// a piece ends inside such a match of a built-in rule. It is not read —
    /// as it was not before transcripts above the cap were read at all.
    UnitAdmissionUnsupported,
    /// One transcript or database yields more distinct facts than may be held
    /// pending until it is read to its end (`staging::MAX_PENDING_BYTES`). It
    /// is not imported in part.
    TooManyFacts,
    /// It is no longer the file that was verified, or it was written while
    /// it was read: exchanged at its path, or changed in size or time.
    Changed,
    /// It cannot be read as text (missing, binary, I/O error).
    Unreadable,
    /// A verified database that the engine cannot open, or whose rows
    /// cannot be stepped or decoded to their end. It is not imported in part.
    DatabaseUnreadable,
    /// A database without the tables and columns its importer reads.
    UnsupportedSchema,
}
// The fixed sentence of each refusal (`Display`) lives in `staging`.

/// A history source that resolves inside a named root: both paths canonical.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HistorySource {
    pub root: PathBuf,
    pub path: PathBuf,
}

/// Resolve a history source inside a NAMED root — the one rule for every
/// importer (Claude Code, Codex, Cursor, OpenCode; transcripts and
/// databases alike).
///
/// The named roots are the request's project, when one is bound, and
/// `history_root`: the directory the importer was started for. In product
/// code that is a fixed location under the home or platform data directory
/// (`~/.claude/projects`, `~/.codex`, `~/.cursor/projects`,
/// `<data dir>/opencode`), never caller input. No authority is derived from
/// `path` itself — not its directory, not a project found above it. `path`
/// is accepted only if, with every symlink resolved, it lies inside one of
/// the named roots; anything else is [`SourceRefusal::OutsideRoot`] and is
/// not opened.
///
/// This is root containment and nothing more. It does not decide whether
/// the path may be read (the path jail and secret-path rule do: the rooted
/// admitted reader for a transcript, [`admit_history_database`] for a
/// database) and it says nothing about content (the reader's admission of
/// the file, [`AdmittedValue::admit`] for a database value).
pub(crate) fn resolve_history_source(
    history_root: &Path,
    path: &Path,
) -> Result<HistorySource, SourceRefusal> {
    let request = crate::tools::admitted_sources::request_root();
    let named = [PathBuf::from(request), history_root.to_path_buf()];
    let path = path.canonicalize().map_err(|_| SourceRefusal::Unreadable)?;
    named
        .iter()
        .filter(|root| !root.as_os_str().is_empty())
        .filter_map(|root| root.canonicalize().ok())
        .find(|root| path.starts_with(root))
        .map(|root| HistorySource { root, path })
        .ok_or(SourceRefusal::OutsideRoot)
}

/// The path-and-policy decision for a history DATABASE, made before any
/// `Connection::open*`: the same decision a file read gets before its file
/// is opened (`core::io_boundary::jail_and_check_path`, the call the rooted
/// admitted reader makes first), under the named root the path resolved in.
///
/// It decides on the PATH: the path jail (containment under the root or an
/// allow-listed path), the protected-store rule, and the secret-like-path
/// rule of the active role (refused in boundary mode `enforce`). It cannot
/// classify content it has not read — the values that leave the database
/// are admitted one by one ([`AdmittedValue::admit`]). Root containment
/// alone ([`resolve_history_source`]) is NOT this decision.
///
/// A decision on a path does not bind the file that is opened later. The
/// importers therefore never open the path this returns: they go through
/// [`database::snapshot_history_database`], which makes this decision, opens
/// the file once, checks it on its descriptor and reads from that descriptor.
pub(crate) fn admit_history_database(
    history_root: &Path,
    path: &Path,
) -> Result<HistorySource, SourceRefusal> {
    let source = resolve_history_source(history_root, path)?;
    crate::core::io_boundary::jail_and_check_path("ctx_knowledge", &source.path, &source.root)
        .map_err(|_| SourceRefusal::Withheld)?;
    Ok(source)
}

/// Why an original value yields nothing. Content-free.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ValueRefusal {
    /// The project policy or the gateway withholds the value.
    Withheld,
    /// It is delivered for a reader only (restricted, E3): nothing derived
    /// from it may be stored — a clean-looking part of it included.
    NotStorable,
}

/// An ORIGINAL value of a history source after admission — the verdict that
/// travels with every derivation (G01: admission before selection).
///
/// Selecting a snippet can cut a classification marker or a banner away, so
/// the decision is made on the whole original and inherited by whatever is
/// taken from it. This type is that inheritance: the fact builder
/// ([`process_admitted`]) and the selectors in front of it take an
/// `AdmittedValue` and nothing else, and one exists only
///
/// - for a database column value that [`Self::admit`] admitted as a whole:
///   withheld or not storable → no `AdmittedValue`, so nothing is derived
///   and nothing stored; masked → the value held here IS the masked one;
/// - for a value parsed from the text of a transcript the rooted admitted
///   reader delivered as storable ([`Self::of_admitted_source`]);
/// - for a value derived from one of these ([`Self::derive`]).
///
/// The admission of the derived fact in `push_fact` stays as a second
/// check. It can only refuse; it never upgrades this verdict.
#[derive(Debug, Clone)]
pub(crate) struct AdmittedValue(Value);

impl AdmittedValue {
    /// Admit one original column value as a whole.
    ///
    /// The admitted unit is the VALUE: every decoded text leaf of it — each
    /// object key, each string, each number in its JSON spelling — in
    /// document order, one per line, as ONE text through the admission a
    /// carrier text gets (project policy on the original, then the gateway;
    /// classification, storability). A marker lies inside one leaf, and a
    /// leaf is never divided: leaves are only joined, each starting its own
    /// line, so a marker cannot be split across units and a marker in any
    /// leaf decides for the whole value.
    ///
    /// When admission masks something, the strings are masked one by one in
    /// place (a key or a number cannot be masked: that refuses the value),
    /// and the masked value must then pass as a whole unchanged — otherwise
    /// it is refused. Derivation never sees the raw original.
    pub(crate) fn admit(original: Value, rules: Rules<'_>) -> Result<Self, ValueRefusal> {
        let joined = joined_leaves(&original);
        if joined.is_empty() {
            return Ok(Self(original));
        }
        if admitted_text(&joined, rules)? == joined {
            return Ok(Self(original));
        }
        let mut masked = original;
        mask_strings(&mut masked, rules)?;
        let rejoined = joined_leaves(&masked);
        if admitted_text(&rejoined, rules)? != rejoined {
            return Err(ValueRefusal::Withheld);
        }
        Ok(Self(masked))
    }

    /// A value parsed from text the rooted admitted reader delivered as
    /// storable: the whole file was admitted before it was parsed, and
    /// `read_transcript` hands out nothing else.
    fn of_admitted_source(value: Value) -> Self {
        Self(value)
    }

    /// A value selected or rebuilt from this one. It inherits the verdict:
    /// the receiver is the witness that an admitted original exists.
    #[allow(clippy::unused_self)]
    pub(crate) fn derive(&self, value: Value) -> Self {
        Self(value)
    }

    pub(crate) fn value(&self) -> &Value {
        &self.0
    }
}

/// The admitted form of `text` when it may feed a store, under `rules`.
fn admitted_text(text: &str, rules: Rules<'_>) -> Result<String, ValueRefusal> {
    let admission = crate::core::context_admission::carrier::admit_carrier_text_under(text, rules)
        .map_err(|_| ValueRefusal::Withheld)?;
    if admission.storable {
        Ok(admission.text)
    } else {
        Err(ValueRefusal::NotStorable)
    }
}

/// Test-only: admit one original value under the rules configured now. A
/// database reader counts into its pending set (`staging::Pending::admit`).
#[cfg(test)]
pub(crate) fn admit_column(result: &mut ImportResult, original: Value) -> Option<AdmittedValue> {
    match AdmittedValue::admit(original, None) {
        Ok(value) => Some(value),
        Err(ValueRefusal::Withheld | ValueRefusal::NotStorable) => {
            result.facts_skipped += 1;
            None
        }
    }
}

impl ImportResult {
    pub(crate) fn merge(&mut self, mut other: Self) {
        self.sessions_found += other.sessions_found;
        self.facts_extracted += other.facts_extracted;
        self.files_touched += other.files_touched;
        self.facts_skipped += other.facts_skipped;
        self.sources_skipped += other.sources_skipped;
        self.sources_refused += other.sources_refused;
        self.errors.append(&mut other.errors);
        self.facts.append(&mut other.facts);
    }

    /// Count a source that was not read. Nothing about it is reported
    /// beyond the count — no path, no reason text with content.
    pub(crate) fn refuse(&mut self, refusal: SourceRefusal) {
        match refusal {
            SourceRefusal::OutsideRoot | SourceRefusal::Withheld | SourceRefusal::Changed => {
                self.sources_refused += 1;
            }
            SourceRefusal::TooLarge
            | SourceRefusal::RecordTooLarge
            | SourceRefusal::UnitAdmissionUnsupported
            | SourceRefusal::TooManyFacts
            | SourceRefusal::DatabaseUnreadable
            | SourceRefusal::UnsupportedSchema
            | SourceRefusal::Unreadable => self.sources_skipped += 1,
        }
    }
}

pub(crate) fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

/// lean-ctx's own data directory: where an import keeps its private scratch
/// files (the spool of a transcript read in units, the snapshot of a history
/// database). Never a directory of the history that is being read.
fn scratch_parent() -> std::io::Result<PathBuf> {
    let directory = crate::core::data_dir::lean_ctx_data_dir().map_err(std::io::Error::other)?;
    std::fs::create_dir_all(&directory)?;
    Ok(directory)
}

/// One transcript, at most the read cap, as text that may feed a derived
/// store.
///
/// The file is resolved inside a named root ([`resolve_history_source`]) and
/// then read through the rooted admitted reader under that root — path jail,
/// secret-path rule, project policy and gateway decided on the original. A
/// transcript above the read cap is [`SourceRefusal::TooLarge`] here and
/// never read in part ([`for_each_transcript_line`] reads it in admitted
/// units instead); one that is delivered but restricted or incompletely
/// decoded feeds no store (E3). The gateway rules are those of `gateway`, the
/// ONE snapshot of this source.
fn read_transcript(root: &Path, path: &Path, gateway: &Gateway) -> Result<String, SourceRefusal> {
    use crate::tools::admitted_sources::{self, SourceRead};
    let source = resolve_history_source(root, path)?;
    let size = std::fs::metadata(&source.path)
        .map_err(|_| SourceRefusal::Unreadable)?
        .len();
    if size > crate::core::limits::max_read_bytes() as u64 {
        return Err(SourceRefusal::TooLarge);
    }
    match admitted_sources::read_with_admission(
        &source.root.to_string_lossy(),
        &source.path.to_string_lossy(),
        "ctx_knowledge",
        gateway,
    ) {
        read @ SourceRead::Admitted(_) => read
            .storable()
            .map(str::to_owned)
            .ok_or(SourceRefusal::Withheld),
        SourceRead::Withheld(_) => Err(SourceRefusal::Withheld),
        SourceRead::Unavailable(_) => Err(SourceRefusal::Unreadable),
    }
}

/// Hand every line of one JSONL transcript — with its 1-based number — to
/// `each`, after admission and never before.
///
/// Up to the read cap the file is admitted as ONE text ([`read_transcript`]).
/// Above it, it is admitted in units of complete records ([`bounded::admit`]):
/// every unit passes the same admission on its original bytes, and `each`
/// sees the first line only after the last unit was admitted. A refusal is a
/// refusal of the whole source: on `Err` the caller takes over nothing of it
/// (above the cap `each` may have seen lines before a late error).
///
/// Above the cap the whole read runs inside ONE source view ([`pinned`]).
fn for_each_transcript_line(
    root: &Path,
    path: &Path,
    gateway: &Gateway,
    mut each: impl FnMut(usize, &str),
) -> Result<(), SourceRefusal> {
    match read_transcript(root, path, gateway) {
        Ok(content) => {
            for (index, line) in content.lines().enumerate() {
                each(index + 1, line);
            }
            Ok(())
        }
        Err(SourceRefusal::TooLarge) => {
            let source = resolve_history_source(root, path)?;
            let mut line_number = 0;
            pinned(&source, || {
                bounded::admit(&source, gateway)?.for_each_unit(|unit| {
                    for line in unit.lines() {
                        line_number += 1;
                        each(line_number, line);
                    }
                })
            })
        }
        Err(refusal) => Err(refusal),
    }
}

/// Run the read of one source in admitted units under ONE policy snapshot.
///
/// `policy::runtime::with_project_source_view` pins the pack and the role
/// for the whole operation — every `policy::runtime::active()` inside it,
/// the unit-safety inspection as much as each admission, is that one
/// snapshot — and compares the pack fingerprint and the role fingerprint
/// again when the operation ends. The gateway rules are one snapshot held by
/// the unit reader and compared when the last unit was read. A change of
/// either, or a policy that cannot be resolved, withholds the source; `Ok`
/// is the point immediately before its pending set is taken over.
fn pinned<T>(
    source: &HistorySource,
    read: impl FnOnce() -> Result<T, SourceRefusal>,
) -> Result<T, SourceRefusal> {
    crate::core::policy::runtime::with_project_source_view(&source.root.to_string_lossy(), read)
        .unwrap_or(Err(SourceRefusal::Withheld))
}

pub(crate) fn read_jsonl_files(
    source: ImportSource,
    root: &Path,
    files: Vec<PathBuf>,
) -> ImportResult {
    let mut result = ImportResult {
        sessions_found: files.len(),
        ..ImportResult::default()
    };
    let mut touched = HashSet::new();
    let mut seen = HashSet::new();

    for path in files {
        // All or nothing per source, under ONE gateway snapshot (label, read,
        // built facts): the lines fill a pending set; result, dedupe sets and
        // counters are touched only when the source was read to its end.
        let gateway = crate::core::context_admission::carrier::gateway_snapshot();
        let mut pending = staging::Pending::under(gateway.clone());
        let session = session_id(source, &path, pending.rules());
        let read = for_each_transcript_line(root, &path, &gateway, |line_number, line| {
            if line.trim().is_empty() {
                return;
            }
            match serde_json::from_str::<Value>(line) {
                Ok(value) => staging::select(&AdmittedValue::of_admitted_source(value), |found| {
                    pending.stage(found);
                }),
                Err(error) => pending.error(format!(
                    "{}:{line_number}: invalid JSONL: {error}",
                    path.display()
                )),
            }
        });
        let _ = pending.settle(read, source, &session, &mut result, &mut touched, &mut seen);
    }

    result.files_touched = touched.len();
    result.facts_extracted = result.facts.len();
    result
}

pub(crate) fn read_json_file(source: ImportSource, root: &Path, path: &Path) -> ImportResult {
    let mut result = ImportResult {
        sessions_found: 1,
        ..ImportResult::default()
    };
    let mut touched = HashSet::new();
    let mut seen = HashSet::new();
    let gateway = crate::core::context_admission::carrier::gateway_snapshot();
    let rules = Some(gateway.admission_policy());
    let session = session_id(source, path, rules);
    match read_transcript(root, path, &gateway) {
        Err(refusal) => result.refuse(refusal),
        Ok(content) => match serde_json::from_str::<Value>(&content) {
            Ok(value) => process_admitted(
                Origin::new(source, &session, rules),
                &AdmittedValue::of_admitted_source(value),
                &mut result,
                &mut touched,
                &mut seen,
            ),
            Err(error) => push_error(
                &mut result,
                format!("{}: invalid JSON: {error}", path.display()),
            ),
        },
    }

    result.files_touched = touched.len();
    result.facts_extracted = result.facts.len();
    result
}

/// Test-only entry with the former signature: admits `value` as an original
/// and then builds facts from it. Product code has only
/// [`process_admitted`].
#[cfg(test)]
pub(crate) fn process_value(
    source: ImportSource,
    session: &str,
    value: &Value,
    result: &mut ImportResult,
    touched: &mut HashSet<String>,
    seen: &mut HashSet<(String, String)>,
) {
    if let Some(admitted) = admit_column(result, value.clone()) {
        let origin = Origin::new(source, session, None);
        process_admitted(origin, &admitted, result, touched, seen);
    }
}

/// Build facts from an admitted value: select file paths, decisions and
/// errors from it. Selection happens here, AFTER the admission of the
/// original — the parameter type is the proof.
fn process_admitted(
    origin: Origin<'_>,
    value: &AdmittedValue,
    result: &mut ImportResult,
    touched: &mut HashSet<String>,
    seen: &mut HashSet<(String, String)>,
) {
    // Found and taken over at once: one parsed JSON document has no failure
    // point after its read. Transcripts and databases go through `Pending`.
    staging::select(value, |found| {
        staging::take_over(found, origin, result, touched, seen);
    });
}

pub(crate) fn finish_result(result: &mut ImportResult, touched: &HashSet<String>) {
    result.files_touched = touched.len();
    result.facts_extracted = result.facts.len();
}

pub(crate) fn push_error(result: &mut ImportResult, error: String) {
    if result.errors.len() < MAX_ERRORS {
        result.errors.push(error);
    }
}

pub(crate) fn session_id(source: ImportSource, path: &Path, rules: Rules<'_>) -> String {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("unknown-session");
    session_label(source, name, rules)
}

/// The `source_session` label stored with every fact of one session:
/// `<importer>:<name>`. `name` is a file name or a database's session id —
/// text that lean-ctx did not choose and that is persisted with the fact. It
/// is kept only if it passes admission for a store unchanged (E3) under
/// `rules`, the snapshot of its source; otherwise `<importer>:unnamed-session`.
pub(crate) fn session_label(source: ImportSource, name: &str, rules: Rules<'_>) -> String {
    let kept = crate::core::context_admission::carrier::admit_for_store_under(name, rules)
        .is_ok_and(|admitted| admitted == name);
    let name = if kept { name } else { "unnamed-session" };
    format!("{}:{name}", source.as_str())
}

fn push_fact(
    result: &mut ImportResult,
    seen: &mut HashSet<(String, String)>,
    Origin {
        source,
        session,
        rules,
    }: Origin<'_>,
    category: &str,
    value: &str,
    confidence: f32,
) {
    let value = truncate(value);
    if value.is_empty() || !seen.insert((category.to_owned(), value.clone())) {
        return;
    }
    // Every fact is built here, whatever it was mined from (a transcript
    // line, a database row), so this is where it is admitted for the store
    // it is headed to (E3): a value the gateway masks is kept masked, a
    // withheld or restricted one is not kept at all — it is counted, and
    // nothing of it is reported. The fact carries no source binding. The
    // rules are those of its source's ONE snapshot (`origin.rules`).
    let admitted = crate::core::context_admission::carrier::admit_for_store_under(&value, rules);
    let Ok(value) = admitted else {
        result.facts_skipped += 1;
        return;
    };
    if value.is_empty() {
        return;
    }

    let now = Utc::now();
    result.facts.push(KnowledgeFact {
        anchors: Vec::new(),
        review: None,
        capture: Default::default(),
        origin: crate::core::knowledge::FactOrigin::Unverified,
        category: category.to_owned(),
        key: String::new(),
        value,
        source_session: session.to_owned(),
        confidence,
        created_at: now,
        last_confirmed: now,
        retrieval_count: 0,
        last_retrieved: None,
        valid_from: None,
        valid_until: None,
        supersedes: None,
        confirmation_count: 0,
        feedback_up: 0,
        feedback_down: 0,
        last_feedback: None,
        privacy: Default::default(),
        sensitivity: Default::default(),
        imported_from: Some(source.as_str().to_owned()),
        archetype: Default::default(),
        fidelity: None,
        revision_count: 0,
        status: None,
        validity: None,
    });
}

/// One fact a repository source ([`markdown`], [`git_history`]) yields, before
/// it is admitted for its store.
pub(crate) struct RepoFact {
    pub category: &'static str,
    /// Where it comes from inside the project: `<file>#<section>/<n>` or
    /// `git:<commit>`. Content-addressed anchors only, never a value.
    pub key: String,
    pub value: String,
    pub confidence: f32,
    pub validity: Option<crate::core::knowledge::FactValidity>,
    pub valid_from: Option<chrono::DateTime<Utc>>,
    /// The section (or commit) it is anchored to, when one could be captured.
    pub anchors: Vec<crate::core::knowledge::anchors::CodeAnchor>,
}

/// Build the fact of a repository source: admitted for its store (E3) under
/// `rules`, the one snapshot of the source, exactly as [`push_fact`] does for
/// a history — masked stays masked, withheld or restricted is counted in
/// `facts_skipped` and dropped. Deduplicated on `(category, value)`.
pub(crate) fn push_repo_fact(
    result: &mut ImportResult,
    seen: &mut HashSet<(String, String)>,
    source: ImportSource,
    session: &str,
    rules: Rules<'_>,
    fact: RepoFact,
) {
    let value = truncate(&fact.value);
    if value.is_empty() || !seen.insert((fact.category.to_owned(), value.clone())) {
        return;
    }
    let Ok(value) = crate::core::context_admission::carrier::admit_for_store_under(&value, rules)
    else {
        result.facts_skipped += 1;
        return;
    };
    if value.is_empty() {
        return;
    }
    let now = Utc::now();
    result.facts.push(KnowledgeFact {
        origin: crate::core::knowledge::FactOrigin::Unverified,
        category: fact.category.to_owned(),
        key: fact.key,
        value,
        source_session: session.to_owned(),
        confidence: fact.confidence,
        created_at: now,
        last_confirmed: now,
        retrieval_count: 0,
        last_retrieved: None,
        valid_from: fact.valid_from,
        valid_until: None,
        supersedes: None,
        confirmation_count: 0,
        feedback_up: 0,
        feedback_down: 0,
        last_feedback: None,
        privacy: Default::default(),
        sensitivity: Default::default(),
        imported_from: Some(source.as_str().to_owned()),
        archetype: crate::core::knowledge::KnowledgeArchetype::infer_from_category(fact.category),
        fidelity: None,
        revision_count: 0,
        status: None,
        validity: fact.validity,
        anchors: fact.anchors,
        review: None,
        capture: Default::default(),
    });
}

fn collect_file_paths(value: &Value, out: &mut Vec<String>) {
    match value {
        Value::Array(values) => {
            for value in values {
                collect_file_paths(value, out);
            }
        }
        Value::Object(map) => {
            for (key, value) in map {
                if matches!(
                    key.as_str(),
                    "file_path" | "filePath" | "path" | "filename" | "file_name"
                ) {
                    if let Some(path) = value
                        .as_str()
                        .map(str::trim)
                        .filter(|path| !path.is_empty())
                    {
                        out.push(path.to_owned());
                    }
                }
                collect_file_paths(value, out);
            }
        }
        _ => {}
    }
}

fn collect_strings(value: &Value, out: &mut Vec<String>) {
    match value {
        Value::String(text) => out.push(text.to_owned()),
        Value::Array(values) => {
            for value in values {
                collect_strings(value, out);
            }
        }
        Value::Object(map) => {
            for value in map.values() {
                collect_strings(value, out);
            }
        }
        _ => {}
    }
}

fn contains_assistant_message(value: &Value) -> bool {
    match value {
        Value::Array(values) => values.iter().any(contains_assistant_message),
        Value::Object(map) => {
            map.get("role").and_then(Value::as_str) == Some("assistant")
                || matches!(
                    map.get("type").and_then(Value::as_str),
                    Some("assistant" | "assistant_message")
                )
                || map.values().any(contains_assistant_message)
        }
        _ => false,
    }
}

fn matching_snippets(text: &str, predicate: fn(&str) -> bool) -> Vec<String> {
    text.split(['\n', '.', '!', '?'])
        .map(str::trim)
        .filter(|snippet| snippet.len() >= 4 && predicate(snippet))
        .map(str::to_owned)
        .collect()
}

fn is_decision(text: &str) -> bool {
    let text = text.to_ascii_lowercase();
    [
        "decision",
        "decided",
        "we chose",
        "i chose",
        "chosen",
        "will use",
        "we'll use",
        "we will use",
        "going with",
        "approach is",
    ]
    .iter()
    .any(|marker| text.contains(marker))
}

fn is_error(text: &str) -> bool {
    let text = text.to_ascii_lowercase();
    [
        "error",
        "failed",
        "failure",
        "panic",
        "unable to",
        "cannot ",
        "can't ",
    ]
    .iter()
    .any(|marker| text.contains(marker))
}

fn truncate(value: &str) -> String {
    // The first char boundary *at or past* the cap can land one byte over it:
    // with 2-byte characters starting at an odd offset the boundaries are
    // 11, 13, … 499, 501, so a 500-byte cap yielded a 501-byte slice. Take the
    // last boundary at or below the cap instead — the same idiom already used
    // in `ctx_search` and `ctx_preload`.
    value[..value.floor_char_boundary(MAX_FACT_VALUE_CHARS)]
        .trim()
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncates_multibyte_facts_without_panicking() {
        let value = format!("We decided {}", "é".repeat(300));
        let truncated = truncate(&value);
        assert!(truncated.len() <= MAX_FACT_VALUE_CHARS);
        assert!(truncated.is_char_boundary(truncated.len()));
    }

    #[test]
    fn extracts_touches_decisions_and_errors() {
        let value: Value = serde_json::json!({
            "role": "assistant",
            "content": "We decided to use a cache. Build failed because input is invalid.",
            "tool_input": { "file_path": "rust/src/lib.rs" }
        });
        let mut result = ImportResult::default();
        let mut touched = HashSet::new();
        let mut seen = HashSet::new();

        process_value(
            ImportSource::Codex,
            "codex:test",
            &value,
            &mut result,
            &mut touched,
            &mut seen,
        );
        finish_result(&mut result, &touched);

        assert_eq!(result.files_touched, 1);
        assert!(
            result
                .facts
                .iter()
                .any(|fact| fact.category == "imported-decision")
        );
        assert!(
            result
                .facts
                .iter()
                .any(|fact| fact.value.starts_with("Observed error:"))
        );
    }

    const AWS_KEY: &str = concat!("AK", "IAIOSFODNN7EXAMPLE");

    /// A history root with one Claude-Code-shaped session directory, as
    /// canonical paths (the rooted reader compares resolved paths).
    fn history() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let dir = tempfile::tempdir().expect("history dir");
        let root = dir.path().canonicalize().expect("history root");
        let sessions = root.join("project-a");
        std::fs::create_dir_all(&sessions).expect("session dir");
        (dir, root, sessions)
    }

    fn assistant_line(text: &str) -> String {
        serde_json::json!({ "message": { "role": "assistant", "content": text } }).to_string()
    }

    fn decisions(result: &ImportResult) -> Vec<&str> {
        result
            .facts
            .iter()
            .filter(|fact| fact.category == "imported-decision")
            .map(|fact| fact.value.as_str())
            .collect()
    }

    /// E3, for every caller: facts were built from transcript text as it
    /// was and only the CLI admitted them before storing. Each value is now
    /// admitted where the fact is built — a clean value keeps its bytes, a
    /// key-shaped value is never handed out raw.
    #[test]
    fn a_transcript_is_read_admitted_and_its_facts_are_admitted_where_they_are_built() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
        let (_dir, root, sessions) = history();
        std::fs::write(
            sessions.join("session.jsonl"),
            format!(
                "{}\n{}\n",
                assistant_line("We decided to use JWT RS256 for the session tokens"),
                assistant_line(&format!(
                    "We decided to deploy with the key {AWS_KEY} today"
                )),
            ),
        )
        .expect("transcript");

        let result = claude_code::import_from_root(&root);
        assert!(result.errors.is_empty(), "{:?}", result.errors);
        assert_eq!(result.sessions_found, 1);
        let decisions = decisions(&result);
        assert!(
            decisions.contains(&"We decided to use JWT RS256 for the session tokens"),
            "a clean fact keeps its bytes: {decisions:?}"
        );
        assert!(
            result
                .facts
                .iter()
                .all(|fact| !fact.value.contains(AWS_KEY)),
            "stored masked, or skipped — never raw"
        );
        // Two decisions were found: each is a fact or a counted skip.
        assert_eq!(decisions.len() + result.facts_skipped, 2);
        assert_eq!(result.facts_extracted, result.facts.len());
    }

    /// A value the current policy withholds or restricts is not kept: it is
    /// counted, and neither the facts nor the errors carry anything of it.
    #[test]
    fn a_restricted_value_is_skipped_and_counted() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let policy = crate::core::policy::load(
            "name = \"import-admission-test\"\nversion = \"1.0.0\"\ndescription = \"test\"\n\
             [filters]\nclassification = \"block\"\nblocked_labels = [\"CONFIDENTIAL\"]\n",
        )
        .expect("policy");
        let _policy = crate::core::policy::runtime::TestPolicyOverride::set(Some(policy));
        let mut result = ImportResult::default();
        let mut seen = HashSet::new();

        for value in [
            "We decided to use a cache",
            "CONFIDENTIAL\nWe decided to ship roadmap-marker in May",
            // The same withheld value again: one distinct value, one count.
            "CONFIDENTIAL\nWe decided to ship roadmap-marker in May",
        ] {
            push_fact(
                &mut result,
                &mut seen,
                Origin::new(ImportSource::Codex, "codex:test", None),
                "imported-decision",
                value,
                0.65,
            );
        }

        assert_eq!(decisions(&result), vec!["We decided to use a cache"]);
        assert_eq!(result.facts_skipped, 1);
        assert!(result.errors.is_empty(), "nothing is reported about it");

        let mut merged = ImportResult::default();
        merged.merge(result);
        assert_eq!(merged.facts_skipped, 1, "the count survives a merge");
    }

    /// The root rule: a transcript that resolves outside the named history
    /// root is not read — no root is derived from where it really lives.
    #[cfg(unix)]
    #[test]
    fn a_transcript_outside_every_root_is_not_read() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
        let (_dir, root, sessions) = history();
        let elsewhere = tempfile::tempdir().expect("outside dir");
        let outside = elsewhere
            .path()
            .canonicalize()
            .expect("outside root")
            .join("outside.jsonl");
        std::fs::write(
            &outside,
            format!(
                "{}\n",
                assistant_line("We decided to keep outside-marker secret")
            ),
        )
        .expect("outside transcript");
        std::os::unix::fs::symlink(&outside, sessions.join("linked.jsonl")).expect("symlink");

        let result = claude_code::import_from_root(&root);
        assert!(result.facts.is_empty(), "{:?}", decisions(&result));
        assert_eq!(result.facts_skipped, 0, "not read, so nothing was judged");
        assert_eq!(result.sources_refused, 1, "counted as refused");
        assert!(
            result.errors.is_empty(),
            "no path, no text: {:?}",
            result.errors
        );

        // The one rule every importer uses, decided on resolved paths, with
        // a typed, content-free reason.
        let inside = sessions.join("inside.jsonl");
        std::fs::write(&inside, "{}\n").expect("inside transcript");
        assert_eq!(
            resolve_history_source(&root, &inside),
            Ok(HistorySource {
                root: root.clone(),
                path: inside,
            })
        );
        assert_eq!(
            resolve_history_source(&root, &outside),
            Err(SourceRefusal::OutsideRoot)
        );
        assert_eq!(
            resolve_history_source(&root, &sessions.join("linked.jsonl")),
            Err(SourceRefusal::OutsideRoot),
            "a link inside the root that points outside"
        );
        assert_eq!(
            resolve_history_source(&root, &root.join("missing.db")),
            Err(SourceRefusal::Unreadable)
        );
        assert!(
            !SourceRefusal::OutsideRoot
                .to_string()
                .contains("outside.jsonl")
        );
    }

    /// The named history roots stay usable: a hidden directory under the
    /// home directory is a root the rooted admitted reader accepts, and the
    /// `.jsonl` transcripts inside it are read. (Fixtures shaped like
    /// `~/.claude/projects` and `~/.codex`.)
    #[test]
    fn history_files_inside_their_named_roots_are_read() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
        let home = tempfile::tempdir().expect("home");
        let home = home.path().canonicalize().expect("home root");

        let claude_root = home.join(".claude").join("projects");
        let claude_sessions = claude_root.join("-Users-dev-project");
        std::fs::create_dir_all(&claude_sessions).expect("claude dir");
        std::fs::write(
            claude_sessions.join("0b1c.jsonl"),
            format!(
                "{}\n",
                assistant_line("We decided to keep the claude marker")
            ),
        )
        .expect("claude transcript");
        let claude = claude_code::import_from_root(&claude_root);
        assert_eq!((claude.sources_refused, claude.sources_skipped), (0, 0));
        assert!(claude.errors.is_empty(), "{:?}", claude.errors);
        assert_eq!(
            decisions(&claude),
            vec!["We decided to keep the claude marker"]
        );

        let codex_root = home.join(".codex");
        let codex_sessions = codex_root.join("sessions").join("2026");
        std::fs::create_dir_all(&codex_sessions).expect("codex dir");
        std::fs::write(
            codex_sessions.join("rollout-1.jsonl"),
            format!(
                "{}\n",
                assistant_line("We decided to keep the codex marker")
            ),
        )
        .expect("codex transcript");
        let codex = codex::import_from_root(&codex_root);
        assert_eq!((codex.sources_refused, codex.sources_skipped), (0, 0));
        assert!(codex.errors.is_empty(), "{:?}", codex.errors);
        assert_eq!(
            decisions(&codex),
            vec!["We decided to keep the codex marker"]
        );
    }

    /// A transcript above the read cap whose second record is itself above
    /// the per-record bound is skipped whole and counted — it is never read
    /// in part and treated as complete, and the record is never cut: the
    /// typed reason is the oversized record, not the size of the file.
    #[test]
    fn a_transcript_above_the_read_cap_is_skipped_and_counted() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
        let (_dir, root, sessions) = history();
        let first = assistant_line("We decided to keep the early marker");
        let filler = assistant_line(&"x".repeat(4096));
        std::fs::write(sessions.join("large.jsonl"), format!("{first}\n{filler}\n"))
            .expect("large transcript");

        crate::test_env::set_var("LCTX_MAX_READ_BYTES", "1024");
        let result = claude_code::import_from_root(&root);
        let rules = crate::core::context_admission::carrier::gateway_snapshot();
        let reason = resolve_history_source(&root, &sessions.join("large.jsonl"))
            .and_then(|source| pinned(&source, || bounded::admit(&source, &rules).map(|_| ())));
        crate::test_env::remove_var("LCTX_MAX_READ_BYTES");

        assert_eq!(reason, Err(SourceRefusal::RecordTooLarge));
        assert_eq!(result.sources_skipped, 1);
        assert_eq!(result.sources_refused, 0);
        assert!(result.facts.is_empty(), "not even the part below the cap");
        assert!(result.errors.is_empty(), "{:?}", result.errors);
    }

    /// A session label is persisted with every fact: a name that admission
    /// would change is not kept.
    #[test]
    fn a_session_label_is_kept_only_if_it_is_admitted_unchanged() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
        assert_eq!(
            session_label(ImportSource::OpenCode, "ses_01HZX", None),
            "opencode:ses_01HZX"
        );
        let label = session_label(
            ImportSource::Codex,
            &format!("rollout {AWS_KEY}.jsonl"),
            None,
        );
        assert_eq!(label, "codex:unnamed-session");
    }

    const BLOCK_CONFIDENTIAL: &str = "name = \"import-original-test\"\nversion = \"1.0.0\"\n\
         description = \"test\"\n[filters]\nclassification = \"block\"\n\
         blocked_labels = [\"CONFIDENTIAL\"]\n";

    fn block_confidential() -> crate::core::policy::runtime::TestPolicyOverride {
        let policy = crate::core::policy::load(BLOCK_CONFIDENTIAL).expect("policy");
        crate::core::policy::runtime::TestPolicyOverride::set(Some(policy))
    }

    /// A Codex-shaped history database with one table of TEXT rows.
    fn codex_database(path: &Path, rows: &[&str]) {
        let db = rusqlite::Connection::open(path).expect("database");
        db.execute_batch("CREATE TABLE history (id INTEGER PRIMARY KEY, body TEXT NOT NULL);")
            .expect("schema");
        for row in rows {
            db.execute("INSERT INTO history (body) VALUES (?1)", [row])
                .expect("row");
        }
    }

    /// G01, the order: an original value is admitted as a WHOLE before
    /// anything is selected from it. A marker in one leaf decides for the
    /// value — the clean, importable sentence in another leaf is not a fact.
    #[test]
    fn a_marker_anywhere_in_an_original_value_refuses_the_whole_value() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let _policy = block_confidential();
        let marked = serde_json::json!({
            "role": "assistant",
            "content": "We decided to keep the marked sentence",
            "banner": "CONFIDENTIAL",
        });
        assert!(AdmittedValue::admit(marked.clone(), None).is_err());

        let mut result = ImportResult::default();
        assert!(admit_column(&mut result, marked).is_none());
        assert_eq!(result.facts_skipped, 1, "counted once per original value");

        let clean = serde_json::json!({
            "role": "assistant",
            "content": "We decided to use a bounded cache",
        });
        let admitted = AdmittedValue::admit(clean.clone(), None).expect("a clean value");
        assert_eq!(admitted.value(), &clean, "a clean value keeps its bytes");
        let derived = admitted.derive(serde_json::json!({ "role": "assistant" }));
        assert_eq!(derived.value()["role"], "assistant");
    }

    /// A value the gateway masks is derived from in its MASKED form only.
    #[test]
    fn a_masked_original_is_only_ever_seen_masked() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
        let keyed = serde_json::json!({
            "role": "assistant",
            "content": format!("We decided to deploy today. The key is {AWS_KEY}"),
        });
        match AdmittedValue::admit(keyed, None) {
            Ok(admitted) => {
                let text = admitted.value().to_string();
                assert!(!text.contains(AWS_KEY), "{text}");
                assert!(text.contains("We decided to deploy today"), "{text}");
            }
            // Not permitted for a store at all: nothing is derived.
            Err(refusal) => assert!(
                matches!(refusal, ValueRefusal::NotStorable | ValueRefusal::Withheld),
                "{refusal:?}"
            ),
        }
    }

    /// The same order for a Codex TEXT column: the row with a marker next to
    /// an importable sentence yields no fact; the clean row does.
    #[test]
    fn a_marked_codex_column_value_yields_no_fact() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let (_dir, root, _sessions) = history();
        codex_database(
            &root.join("history.sqlite"),
            &[
                r#"{"role":"assistant","content":"We decided to keep the marked sentence","banner":"CONFIDENTIAL"}"#,
                r#"{"role":"assistant","content":"We decided to use a bounded cache"}"#,
            ],
        );

        let _policy = block_confidential();
        let result = codex::import_from_root(&root);
        assert_eq!(
            decisions(&result),
            vec!["We decided to use a bounded cache"]
        );
        assert_eq!(result.facts_skipped, 1);
        assert_eq!((result.sources_refused, result.sources_skipped), (0, 0));
        assert!(
            result
                .facts
                .iter()
                .all(|fact| !fact.value.contains("marked sentence"))
        );

        // All or nothing per database: a second database whose SECOND row
        // cannot be stepped (a generated column over a body that is not
        // JSON) adds its one skip and one fixed sentence — nothing of its
        // first row, no session, no counter.
        let broken = rusqlite::Connection::open(root.join("broken.sqlite")).expect("database");
        broken
            .execute_batch(
                "CREATE TABLE history (id INTEGER PRIMARY KEY, body TEXT NOT NULL); \
                 INSERT INTO history (body) VALUES \
                 ('{\"role\":\"assistant\",\"content\":\"We decided to keep the early row\"}'), \
                 ('not json'); \
                 ALTER TABLE history ADD COLUMN kind TEXT \
                 GENERATED ALWAYS AS (json_extract(body, '$.role')) VIRTUAL;",
            )
            .expect("fixture");
        drop(broken);
        let again = codex::import_from_root(&root);
        assert_eq!(decisions(&again), decisions(&result));
        assert_eq!((again.sources_skipped, again.sources_refused), (1, 0));
        assert_eq!(
            (again.sessions_found, again.facts_skipped),
            (result.sessions_found, 1)
        );
        assert_eq!(again.errors.len(), 1, "{:?}", again.errors);
        assert!(again.errors[0].ends_with(": history database could not be read; skipped"));
    }

    /// A key-shaped secret next to an importable sentence in a database
    /// column: whatever is stored comes from the masked value.
    #[test]
    fn a_keyed_codex_column_value_never_yields_the_key() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
        let (_dir, root, _sessions) = history();
        let row = serde_json::json!({
            "role": "assistant",
            "content": format!("We decided to deploy today. The key is {AWS_KEY}"),
        })
        .to_string();
        codex_database(&root.join("history.sqlite"), &[&row]);

        let result = codex::import_from_root(&root);
        assert_eq!(result.sources_refused, 0);
        assert!(
            result
                .facts
                .iter()
                .all(|fact| !fact.value.contains(AWS_KEY))
        );
        if result.facts_skipped == 0 {
            assert!(
                decisions(&result).contains(&"We decided to deploy today"),
                "the clean sentence of the masked value is imported: {:?}",
                decisions(&result)
            );
        }
    }

    /// The same order for a transcript: the rooted admitted reader admits
    /// the whole file before a line is parsed, so a marker in one place
    /// withholds the importable sentence in another.
    #[test]
    fn a_marked_transcript_yields_no_fact() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let (_dir, root, sessions) = history();
        std::fs::write(
            sessions.join("session.jsonl"),
            format!(
                "CONFIDENTIAL\n{}\n",
                assistant_line("We decided to keep the marked sentence")
            ),
        )
        .expect("transcript");

        let _policy = block_confidential();
        let result = claude_code::import_from_root(&root);
        assert!(result.facts.is_empty(), "{:?}", decisions(&result));
        assert_eq!(result.sources_refused, 1);
        assert_eq!(result.facts_skipped, 0, "the file was refused as a whole");
        assert!(result.errors.is_empty(), "{:?}", result.errors);
    }

    /// The path decision before a database is opened: a secret-like path
    /// under boundary mode `enforce` is refused like a file read would be —
    /// not opened, counted.
    #[test]
    fn a_database_at_a_refused_path_is_not_opened() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
        let (_dir, root, _sessions) = history();
        let secret_dir = root.join(".aws");
        std::fs::create_dir_all(&secret_dir).expect("secret-like dir");
        let database = secret_dir.join("history.sqlite");
        codex_database(
            &database,
            &[r#"{"role":"assistant","content":"We decided to use a bounded cache"}"#],
        );

        crate::test_env::set_var("LEAN_CTX_IO_BOUNDARY_MODE", "enforce");
        let decision = admit_history_database(&root, &database);
        let result = codex::import_from_root(&root);
        crate::test_env::remove_var("LEAN_CTX_IO_BOUNDARY_MODE");

        assert_eq!(decision, Err(SourceRefusal::Withheld));
        assert_eq!(result.sources_refused, 1);
        assert_eq!(result.facts_skipped, 0);
        assert!(result.facts.is_empty(), "{:?}", decisions(&result));

        // In boundary mode `warn` the same path is a readable source. The
        // mode is named: a role's default mode is `enforce`
        // (`roles::default_boundary_mode`), so "no variable" is not `warn`.
        crate::test_env::set_var("LEAN_CTX_IO_BOUNDARY_MODE", "warn");
        let warned = admit_history_database(&root, &database);
        crate::test_env::remove_var("LEAN_CTX_IO_BOUNDARY_MODE");
        assert!(warned.is_ok());
    }

    /// A transcript above the read cap is imported: read in units of complete
    /// records, each unit admitted on its original bytes before a line is
    /// parsed. The units cover the file — every record is a fact, in order,
    /// multi-byte text intact — and the source rule holds across units: a
    /// marking in the LAST unit withholds the records of the first.
    ///
    /// A pattern that could match across lines without a bound is never
    /// taken for a single-line one (it would refuse unit admission).
    #[test]
    fn a_transcript_above_the_read_cap_is_imported_in_admitted_units() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let policy = crate::core::policy::load(&format!("{BLOCK_CONFIDENTIAL}pii = \"block\"\n"))
            .expect("policy");
        let _policy = crate::core::policy::runtime::TestPolicyOverride::set(Some(policy));
        let _cap = crate::setup::EnvVarGuard::set("LCTX_MAX_READ_BYTES", "1024");
        let (_dir, root, sessions) = history();
        // Digit-free records of one length: the unit boundaries are known.
        let expected: Vec<String> = ('a'..='c')
            .flat_map(|x| ('a'..='t').map(move |y| (x, y)))
            .map(|(x, y)| format!("We decided to keep the unit marker é {x}{y}"))
            .collect();
        let records: Vec<String> = expected.iter().map(|text| assistant_line(text)).collect();
        let write = |records: &[String], end: &str| {
            std::fs::write(sessions.join("long.jsonl"), records.join("\n") + "\n" + end)
                .expect("transcript");
        };
        let width = records[0].len();
        let per_unit = bounded::unit_bytes() / (width + 1);
        assert!(records.len() > 3 * per_unit, "several units");

        write(&records, "");
        let result = claude_code::import_from_root(&root);
        assert_eq!((result.sources_skipped, result.sources_refused), (0, 0));
        assert!(result.errors.is_empty(), "{:?}", result.errors);
        assert_eq!(decisions(&result), expected);

        write(&records, "CONFIDENTIAL\n");
        let result = claude_code::import_from_root(&root);
        assert!(result.facts.is_empty(), "{:?}", decisions(&result));
        assert_eq!((result.sources_skipped, result.sources_refused), (0, 1));

        use crate::core::context_admission::cross_line::pattern_is_single_line;
        assert!(pattern_is_single_line(r"EMP-\d{4}") && !pattern_is_single_line(r"left\s+right"));

        // All or nothing per source: the read fails after the first unit was
        // parsed. Nothing of that source is taken over — no fact, no counter
        // but its one skip, and no dedupe state: the decision it shares with
        // the NEXT source is still imported from there.
        write(&records, "");
        std::fs::write(sessions.join("next.jsonl"), format!("{}\n", records[0])).expect("next");
        bounded::FAIL_BEFORE_UNIT.with(|fault| fault.set(Some(1)));
        let result = claude_code::import_from_root(&root);
        bounded::FAIL_BEFORE_UNIT.with(|fault| fault.set(None));
        assert_eq!(decisions(&result), vec![expected[0].as_str()]);
        assert_eq!(
            (
                result.sources_skipped,
                result.sources_refused,
                result.facts_skipped
            ),
            (1, 0, 0)
        );
        assert_eq!((result.facts_extracted, result.errors.len()), (1, 0));
    }

    /// The database is bound to the descriptor it was verified on. A file
    /// exchanged at the path after the verification — renamed over, or
    /// replaced by a link — is refused with a typed reason: nothing is read,
    /// neither from the exchanged file nor from the verified one.
    #[cfg(unix)]
    #[test]
    fn a_database_exchanged_after_verification_is_refused() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
        let (_dir, root, sessions) = history();
        let database = root.join("history.sqlite");
        let other = sessions.join("other.sqlite");
        let row = r#"{"role":"assistant","content":"We decided to keep the verified marker"}"#;
        codex_database(&database, &[row]);
        codex_database(&other, &[row]);

        let verified = database::verify_history_database(&root, &database).expect("verified");
        std::fs::rename(&other, &database).expect("rename over");
        assert_eq!(verified.into_snapshot().err(), Some(SourceRefusal::Changed));

        let verified = database::verify_history_database(&root, &database).expect("verified");
        std::fs::rename(&database, &other).expect("move away");
        std::os::unix::fs::symlink(&other, &database).expect("link in its place");
        assert_eq!(verified.into_snapshot().err(), Some(SourceRefusal::Changed));
    }
}
