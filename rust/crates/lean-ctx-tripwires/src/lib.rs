// SPDX-License-Identifier: Apache-2.0
//! P4 tripwires (W1 pz8): a cheap, deterministic diff analyzer that flags the
//! documented reward-hacking tricks in one diff `base..commit`.
//!
//! A hit never blocks honest work by itself and never lets work through: the
//! caller routes a task with hits to a person (process-v1 minor 1.1, the G6
//! record's `p4`). The crate is pure: no I/O, no clock, no environment, no
//! network. The caller (lean-ctx-hive, hardened git) supplies the file pairs and
//! the Instance's write counts; [`analyze`] returns a [`TripwireReportV1`].
//!
//! There is no suppression mechanism: comments such as `// tripwire: allow`,
//! attributes such as `#[allow(..)]`, or a `.tripwires.toml` in the repo change
//! nothing. Configuration comes only from the consts of this crate.
//!
//! Line numbers: `line` is 1-based, in the candidate for something the candidate
//! has and in the base for something the candidate removed; `0` is a hit on the
//! whole file or crate (a touched test path, a dropped test count,
//! `edit_cycles`, `unparsable`).
//!
//! Callers should pass every changed file and, as context, the base copies of the
//! test and fixture files of the touched crates (with `base == candidate`).
//! Context files contribute base test literals and names but never hits.

#![forbid(unsafe_code)]

mod diff;
mod model;
mod paths;
mod rules;
mod rust_model;
mod script_model;

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

pub use paths::is_test_or_grader_path;

/// Schema id of [`TripwireReportV1`] (process-v1 minor 1.1, `tripwires.schema.json`).
pub const SCHEMA: &str = "leanctx.process.tripwires/v1";

/// More writes than this to one file under one claim is an `edit_cycles` hit
/// (design value, v6 §11).
pub const EDIT_CYCLES_MAX: u32 = 8;

/// A changed file larger than this (either side) is not analysed: `unparsable`.
pub const MAX_FILE_BYTES: usize = 2 * 1024 * 1024;

/// Exit code of `lean-ctx tasks tripwires` without hits.
pub const EXIT_CLEAN: i32 = 0;
/// Exit code of `lean-ctx tasks tripwires` on an error (no report).
pub const EXIT_ERROR: i32 = 2;
/// Exit code of `lean-ctx tasks tripwires` with hits.
pub const EXIT_HITS: i32 = 3;

/// The exact rule ids, in their sort order.
pub const RULES: [&str; 12] = [
    rules::ASSERT_REMOVED,
    rules::ASSERT_WEAKENED,
    rules::BRANCH_ON_TEST_INPUT,
    rules::EDIT_CYCLES,
    rules::ERROR_SWALLOWED,
    rules::FIXTURE_CONSTANT,
    rules::MOCK_ONLY_TEST,
    rules::MONKEY_PATCH,
    rules::SKIP_ADDED,
    rules::TEST_COUNT_DROPPED,
    rules::TEST_OR_GRADER_TOUCHED,
    rules::UNPARSABLE,
];

/// One file of the diff. `base: None` = added, `candidate: None` = deleted,
/// `base == candidate` = unchanged context (never a hit).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileInputV1 {
    /// Repo-relative path with `/` separators.
    pub path: String,
    /// The bytes at the base commit.
    pub base: Option<Vec<u8>>,
    /// The bytes at the judged commit.
    pub candidate: Option<Vec<u8>>,
}

impl FileInputV1 {
    /// A file pair from its two sides.
    pub fn new(path: impl Into<String>, base: Option<&[u8]>, candidate: Option<&[u8]>) -> Self {
        Self {
            path: path.into(),
            base: base.map(<[u8]>::to_vec),
            candidate: candidate.map(<[u8]>::to_vec),
        }
    }
}

/// The analyzer's whole input. The base test literals are derived inside from
/// the base sides of `files`; nothing is taken from the worker.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiffInputV1 {
    /// The base commit (claim start for S, `spec_commit` for M/H); empty
    /// when the caller passes only files.
    #[serde(default, deserialize_with = "null_as_default")]
    pub base: String,
    /// The judged commit; empty when the caller passes only files.
    #[serde(default, deserialize_with = "null_as_default")]
    pub commit: String,
    /// Every changed file, plus optional unchanged context files.
    pub files: Vec<FileInputV1>,
    /// Writes per path under this claim, from the Instance's write log
    /// (LeanCTX write tools and hook-observed native writes), never from the
    /// worker's report. Absent or `null` means no writes were logged.
    #[serde(default, deserialize_with = "null_as_default")]
    pub edit_counts: BTreeMap<String, u32>,
}

/// Reads `null` like an absent field (the field's default).
fn null_as_default<'de, D, T>(de: D) -> Result<T, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Default + Deserialize<'de>,
{
    Ok(Option::<T>::deserialize(de)?.unwrap_or_default())
}

/// One tripwire hit.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct TripwireHitV1 {
    /// One of [`RULES`].
    pub rule: String,
    /// Repo-relative path.
    pub path: String,
    /// 1-based line; 0 for the whole file.
    pub line: u32,
    /// Human-readable reason (at most 2000 chars).
    pub detail: String,
}

/// Test items in the changed files, per crate by test name.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TripwireCountsV1 {
    pub tests_base: u32,
    pub tests_candidate: u32,
}

/// The report: `leanctx.process.tripwires/v1`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TripwireReportV1 {
    pub schema: String,
    pub base: String,
    pub commit: String,
    /// `true` exactly when `hits` is empty.
    pub ok: bool,
    /// Sorted by `(rule, path, line)`, each key once.
    pub hits: Vec<TripwireHitV1>,
    pub counts: TripwireCountsV1,
}

impl TripwireReportV1 {
    /// The canonical serialization (compact JSON, field order fixed): the same
    /// input gives byte-identical output.
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }

    /// The rule ids that hit, sorted and each once.
    pub fn rules_hit(&self) -> Vec<&str> {
        let mut out: Vec<&str> = self.hits.iter().map(|h| h.rule.as_str()).collect();
        out.dedup();
        out
    }
}

/// The CLI's exit code for a report: [`EXIT_CLEAN`] without hits, [`EXIT_HITS`]
/// with hits. The gate never reads it: it calls [`analyze`] and reads the report.
pub fn exit_code(report: &TripwireReportV1) -> i32 {
    if report.hits.is_empty() {
        EXIT_CLEAN
    } else {
        EXIT_HITS
    }
}

/// Run every rule over one diff.
pub fn analyze(input: &DiffInputV1) -> TripwireReportV1 {
    let (hits, counts) = rules::run(input);
    TripwireReportV1 {
        schema: SCHEMA.to_string(),
        base: input.base.clone(),
        commit: input.commit.clone(),
        ok: hits.is_empty(),
        hits,
        counts,
    }
}
