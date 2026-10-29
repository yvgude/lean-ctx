// SPDX-License-Identifier: Apache-2.0

//! What one admitted value yields, and the all-or-nothing take-over of one
//! history source — a transcript or a history database.
//!
//! The selectors ([`select`]) only FIND: file paths, decisions, errors. What
//! is found becomes part of the import in one place ([`take_over`]): the
//! dedupe sets are consulted and filled, the fact is admitted for its store
//! and counted. Nothing a source yields is taken over as it is found:
//! everything its lines or rows yield is held in a [`Pending`] set that
//! touches neither the result nor the dedupe sets nor a counter, and is taken
//! over only after the source was read and parsed to its end without an
//! error. A source that fails — at any point, for any reason — contributes
//! nothing but its one refusal count. One transcript file is one source; one
//! history database is one source ([`read_database`]), whatever number of
//! tables and sessions it holds.
//!
//! The importer writes to no store itself: its whole output is the
//! `ImportResult` and the two dedupe sets of one run, so dropping the pending
//! set IS the rollback. Taking it over cannot fail (a fact that admission
//! refuses is a counted outcome, not an error — as is an original database
//! value that admission refuses, [`Pending::admit`]).

use std::collections::HashSet;
use std::path::Path;

use serde_json::Value;

use super::{
    AdmittedValue, ImportResult, ImportSource, MAX_ERRORS, SourceRefusal, ValueRefusal,
    admitted_text, collect_file_paths, collect_strings, contains_assistant_message, is_decision,
    is_error, matching_snippets, pinned, push_error, push_fact, resolve_history_source, truncate,
};
use crate::core::context_admission::carrier::gateway_snapshot;
use crate::core::context_admission::{AdmissionPolicy, stores::StoreAdmission};

/// The gateway rules one original value is admitted under: the snapshot of
/// its source, or `None` for the rules configured at that moment.
pub(super) type Rules<'a> = Option<&'a AdmissionPolicy>;

/// The ONE gateway snapshot of a source (`carrier::gateway_snapshot`): its
/// reader, its label, its values and the facts built from it are all judged
/// under it, and it is compared with the configured rules before take-over.
pub(super) type Gateway = StoreAdmission;

/// Where a fact comes from, and the rules of that source's snapshot under
/// which the fact is admitted for its store when it is built.
#[derive(Clone, Copy)]
pub(super) struct Origin<'a> {
    pub(super) source: ImportSource,
    pub(super) session: &'a str,
    pub(super) rules: Rules<'a>,
}

impl<'a> Origin<'a> {
    pub(super) fn new(source: ImportSource, session: &'a str, rules: Rules<'a>) -> Self {
        Self {
            source,
            session,
            rules,
        }
    }
}

/// Replace every string of `value` by its admitted form.
pub(super) fn mask_strings(value: &mut Value, rules: Rules<'_>) -> Result<(), ValueRefusal> {
    match value {
        Value::String(text) => {
            if !text.is_empty() {
                *text = admitted_text(text, rules)?;
            }
        }
        Value::Array(items) => {
            for item in items {
                mask_strings(item, rules)?;
            }
        }
        Value::Object(map) => {
            for item in map.values_mut() {
                mask_strings(item, rules)?;
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
    Ok(())
}

/// The fixed, content-free sentence of each refusal: no path, no byte of the
/// source, no engine or driver message.
impl std::fmt::Display for SourceRefusal {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::OutsideRoot => "history source is outside every named root; not opened",
            Self::Withheld => "history source withheld by policy; not imported",
            Self::TooLarge => "history source exceeds the read limit; skipped",
            Self::RecordTooLarge => "history source holds an oversized record; skipped",
            Self::UnitAdmissionUnsupported => {
                "history source exceeds the read limit and the active rules cannot be \
                 applied to it in parts; skipped"
            }
            Self::TooManyFacts => "history source yields too many facts; skipped",
            Self::Changed => "history source changed after it was verified; not imported",
            Self::Unreadable => "history source is not readable as text; skipped",
            Self::DatabaseUnreadable => "history database could not be read; skipped",
            Self::UnsupportedSchema => "history database has an unsupported schema; skipped",
        })
    }
}

/// The line an importer reports for a database it found, started to read and
/// did not import: the label the importer already prints for that database
/// and the fixed sentence of the typed refusal — nothing else.
pub(super) fn not_imported(label: &str, refusal: SourceRefusal) -> String {
    format!("{label}: {refusal}")
}

/// Every text leaf of `value` in document order, one per line.
pub(super) fn joined_leaves(value: &Value) -> String {
    fn collect(value: &Value, out: &mut Vec<String>) {
        match value {
            Value::String(text) => out.push(text.clone()),
            Value::Number(number) => out.push(number.to_string()),
            Value::Array(items) => {
                for item in items {
                    collect(item, out);
                }
            }
            Value::Object(map) => {
                for (key, item) in map {
                    out.push(key.clone());
                    collect(item, out);
                }
            }
            Value::Null | Value::Bool(_) => {}
        }
    }
    let mut leaves = Vec::new();
    collect(value, &mut leaves);
    leaves.join("\n")
}

/// Cap on the text one source may hold pending (paths and fact values,
/// counted once; the dedupe keys hold a second copy).
///
/// A pending value is at most 500 bytes and only DISTINCT paths, decisions
/// and error lines are held, so 64 MiB is at least 128,000 distinct facts
/// from one session file or database, several hundred thousand at ordinary
/// lengths. A source beyond that is refused whole
/// ([`SourceRefusal::TooManyFacts`]); taken over, the same text would sit in
/// the result's facts in any case.
pub(super) const MAX_PENDING_BYTES: usize = 64 * 1024 * 1024;

/// One thing the selectors found in an admitted value.
pub(super) enum Found {
    /// A file path the value names.
    Touched(String),
    /// A candidate fact: a decision or an observed error.
    Fact {
        category: &'static str,
        value: String,
        confidence: f32,
    },
}

/// Select file paths, decisions and errors from an admitted value, in the
/// order the importer has always used: the paths, then per string its
/// decisions (assistant messages only) and its errors. Selection happens
/// AFTER the admission of the original — the parameter type is the proof.
pub(super) fn select(value: &AdmittedValue, mut found: impl FnMut(Found)) {
    let value = value.value();
    let mut paths = Vec::new();
    collect_file_paths(value, &mut paths);
    for path in paths {
        found(Found::Touched(path));
    }

    let mut strings = Vec::new();
    collect_strings(value, &mut strings);
    let assistant = contains_assistant_message(value);
    for text in strings {
        if assistant {
            for decision in matching_snippets(&text, is_decision) {
                found(Found::Fact {
                    category: "imported-decision",
                    value: decision,
                    confidence: 0.65,
                });
            }
        }
        for error in matching_snippets(&text, is_error) {
            found(Found::Fact {
                category: "imported-observation",
                value: format!("Observed error: {error}"),
                confidence: 0.55,
            });
        }
    }
}

/// Make one found thing part of the import: a path not touched before in
/// this run becomes an observation, a candidate becomes a fact unless the
/// run has seen it (`push_fact` dedupes, admits and counts).
pub(super) fn take_over(
    found: Found,
    origin: Origin<'_>,
    result: &mut ImportResult,
    touched: &mut HashSet<String>,
    seen: &mut HashSet<(String, String)>,
) {
    match found {
        Found::Touched(path) => {
            if touched.insert(path.clone()) {
                push_fact(
                    result,
                    seen,
                    origin,
                    "imported-observation",
                    &format!("Touched file: {path}"),
                    0.6,
                );
            }
        }
        Found::Fact {
            category,
            value,
            confidence,
        } => push_fact(result, seen, origin, category, &value, confidence),
    }
}

/// Everything one source yields, held back until the source was read to its
/// end.
///
/// Repeats inside the source are dropped here with the keys the run's own
/// dedupe uses (the path; the category and the truncated value), so taking
/// the rest over in order does to the result, the dedupe sets and the
/// counters exactly what taking each thing over as it was found would have
/// done.
#[derive(Default)]
pub(super) struct Pending {
    /// What was found, each with the session it belongs to: `0` is the
    /// session [`Self::settle`] is given, `n` is `labels[n - 1]`.
    found: Vec<(usize, Found)>,
    labels: Vec<String>,
    touched: HashSet<String>,
    seen: HashSet<(&'static str, String)>,
    errors: Vec<String>,
    bytes: usize,
    values_refused: usize,
    /// The ONE gateway snapshot every value of a database is admitted under
    /// ([`read_database`]). `None` for a transcript: its text was admitted by
    /// the reader before a line was parsed.
    gateway: Option<StoreAdmission>,
}

impl Pending {
    /// The pending set of a source that is judged under `gateway` throughout.
    pub(super) fn under(gateway: Gateway) -> Self {
        Self {
            gateway: Some(gateway),
            ..Self::default()
        }
    }

    /// The rules of this source's snapshot.
    pub(super) fn rules(&self) -> Rules<'_> {
        self.gateway.as_ref().map(StoreAdmission::admission_policy)
    }

    /// What is staged from here on belongs to the session labelled `session`
    /// (a database holds several sessions; a transcript is one and never
    /// calls this).
    pub(super) fn label(&mut self, session: String) {
        self.labels.push(session);
    }

    /// Admit one original column value of a database. A refused value yields
    /// nothing and is counted once in `facts_skipped` — when the source is
    /// taken over, not before: the facts it might have held are never
    /// derived, so they cannot be counted one by one. Every value of the
    /// source is admitted under the source's one gateway snapshot.
    pub(super) fn admit(&mut self, original: Value) -> Option<AdmittedValue> {
        match AdmittedValue::admit(original, self.rules()) {
            Ok(value) => Some(value),
            Err(ValueRefusal::Withheld | ValueRefusal::NotStorable) => {
                self.values_refused += 1;
                None
            }
        }
    }

    pub(super) fn stage(&mut self, found: Found) {
        if self.bytes > MAX_PENDING_BYTES {
            return;
        }
        let found = match found {
            Found::Touched(path) => {
                if !self.touched.insert(path.clone()) {
                    return;
                }
                self.bytes += path.len();
                Found::Touched(path)
            }
            Found::Fact {
                category,
                value,
                confidence,
            } => {
                let value = truncate(&value);
                if value.is_empty() || !self.seen.insert((category, value.clone())) {
                    return;
                }
                self.bytes += value.len();
                Found::Fact {
                    category,
                    value,
                    confidence,
                }
            }
        };
        self.found.push((self.labels.len(), found));
    }

    /// A message about a line or a row that could not be parsed. Only the
    /// first `MAX_ERRORS` of a run are ever kept, so no more are held.
    pub(super) fn error(&mut self, error: String) {
        if self.errors.len() < MAX_ERRORS {
            self.errors.push(error);
        }
    }

    /// Take the source over — it was read to its end (`read` is `Ok`) and is
    /// within the pending cap — or count its one refusal and nothing else.
    /// `Err` is the refusal that was counted.
    pub(super) fn settle(
        self,
        read: Result<(), SourceRefusal>,
        source: ImportSource,
        session: &str,
        result: &mut ImportResult,
        touched: &mut HashSet<String>,
        seen: &mut HashSet<(String, String)>,
    ) -> Result<(), SourceRefusal> {
        // The take-over check, for every kind of source: the rules configured
        // NOW must still be the snapshot the source was judged under.
        let checked = if !self.gateway_unchanged() {
            Err(SourceRefusal::Withheld)
        } else if self.bytes > MAX_PENDING_BYTES {
            Err(SourceRefusal::TooManyFacts)
        } else {
            Ok(())
        };
        if let Err(refusal) = read.and(checked) {
            result.refuse(refusal);
            return Err(refusal);
        }
        result.facts_skipped += self.values_refused;
        // The second check of every built fact runs under the snapshot the
        // source was read under — no value of it meets the rules of another
        // moment.
        let rules = self.gateway.as_ref().map(StoreAdmission::admission_policy);
        for (label, found) in self.found {
            let session = label
                .checked_sub(1)
                .and_then(|index| self.labels.get(index))
                .map_or(session, String::as_str);
            let origin = Origin::new(source, session, rules);
            take_over(found, origin, result, touched, seen);
        }
        for error in self.errors {
            push_error(result, error);
        }
        Ok(())
    }

    /// Whether the gateway rules configured NOW are still the snapshot every
    /// value of this source was admitted under.
    fn gateway_unchanged(&self) -> bool {
        self.gateway
            .as_ref()
            .is_none_or(|taken| gateway_snapshot().digest() == taken.digest())
    }
}

/// Read one history DATABASE as ONE source, all or nothing.
///
/// `rows` reads every table and row it is going to read and stages what the
/// admitted values yield into the pending set it is handed; it returns `Err`
/// for ANY failure of the read — the snapshot does not open, a statement does
/// not prepare, a row cannot be stepped or decoded. The caller settles the
/// returned pair ([`Pending::settle`]): on `Err` the database contributes its
/// one refusal and nothing else — no fact of an earlier row, no dedupe key,
/// no touched path, no counter.
///
/// The whole read runs under ONE policy snapshot, like a transcript read in
/// units: inside one source view of the root the database resolved in
/// (`super::pinned` — the pack and the role are pinned for every value
/// admission and compared again when the view ends), and under ONE gateway
/// snapshot (`carrier::gateway_snapshot`, the function the unit reader
/// uses), taken here before the first row: [`Pending::admit`] hands it into
/// every value admission, and the rules configured after the last row must
/// still have its digest. A change of either, or a policy that cannot be
/// resolved, withholds the database; `Ok` is the point immediately before
/// take-over.
pub(super) fn read_database(
    history_root: &Path,
    path: &Path,
    rows: impl FnOnce(&mut Pending) -> Result<(), SourceRefusal>,
) -> (Pending, Result<(), SourceRefusal>) {
    let mut pending = Pending::under(gateway_snapshot());
    let read = resolve_history_source(history_root, path).and_then(|source| {
        // The gateway side is compared in `Pending::settle`, the last thing
        // before take-over; the pack side when this view ends.
        pinned(&source, || rows(&mut pending))
    });
    (pending, read)
}
