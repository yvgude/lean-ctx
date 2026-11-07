use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, ErrorKind, Seek, SeekFrom, Write};
use std::path::PathBuf;

use fs2::FileExt;

use super::types::{OclaError, OclaRequestContext, OclaResult};
use crate::core::savings_ledger::SavingsEvent;

/// Unified P5 savings event combining the legacy chain fields with
/// cross-capability attribution and analysis metadata.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct UnifiedSavingsEventV2 {
    pub tool_name: String,
    pub mode: String,
    pub original_tokens: u64,
    pub compressed_tokens: u64,
    pub saved_tokens: u64,
    pub content_hash: String,
    pub timestamp_epoch_ms: u64,
    pub prev_hash: String,
    pub event_hash: String,
    pub intent: Option<String>,
    pub outcome: Option<String>,
    pub routing_decision: Option<String>,
    pub agent_id: Option<String>,
    pub efficiency_etpao: Option<u64>,
    pub attribution_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trace_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quality_ref: Option<String>,
}

/// Comparison of the legacy and unified savings ledgers.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ReconciliationReport {
    pub matched: usize,
    pub unmatched_legacy: usize,
    pub unmatched_unified: usize,
    pub token_drift: i64,
    pub double_bookings: Vec<String>,
}

/// Formats a reconciliation report for human-readable CLI output.
pub fn format_reconciliation_report(report: &ReconciliationReport) -> String {
    let mut out = String::new();
    out.push_str(&format!("Matched events: {}\n", report.matched));
    out.push_str(&format!("Unmatched legacy: {}\n", report.unmatched_legacy));
    out.push_str(&format!(
        "Unmatched unified: {}\n",
        report.unmatched_unified
    ));
    out.push_str(&format!("Token drift: {}\n", report.token_drift));
    out.push_str(&format!(
        "Double bookings: {}\n",
        report.double_bookings.len()
    ));
    if report.token_drift == 0 && report.double_bookings.is_empty() {
        out.push_str("Status: PASS\n");
    } else {
        out.push_str("Status: FAIL\n");
    }
    out
}

/// Unified ledger contract for P5 migration and eventual legacy replacement.
///
/// Migration plan:
/// - Phase 1: introduce this schema alongside the legacy schema (dual-write).
/// - Phase 2: migrate existing events into unified events.
/// - Phase 3: deactivate the legacy schema after migration verification.
pub trait UnifiedLedger: Send + Sync {
    fn record_unified(&self, event: UnifiedSavingsEventV2) -> OclaResult<String>;
    fn verify_chain(&self) -> OclaResult<bool>;
    fn query_by_attribution(&self, id: &str) -> OclaResult<Option<UnifiedSavingsEventV2>>;
}

/// File-backed implementation used during the P5 dual-write migration.
/// File-backed implementation of the unified savings ledger.
pub struct FileUnifiedLedger {
    path: PathBuf,
}

impl FileUnifiedLedger {
    pub fn from_data_dir() -> OclaResult<Self> {
        let data_dir = crate::core::data_dir::lean_ctx_data_dir()
            .map_err(|error| OclaError::InvalidRequest(error.clone()))?;
        Ok(Self::new(data_dir.join("savings/unified_ledger.jsonl")))
    }

    pub(crate) fn new(path: PathBuf) -> Self {
        Self { path }
    }

    fn io_error(error: impl std::fmt::Display) -> OclaError {
        OclaError::InvalidRequest(format!("unified ledger I/O failed: {error}"))
    }

    fn read_events(&self) -> OclaResult<Vec<UnifiedSavingsEventV2>> {
        let file = match File::open(&self.path) {
            Ok(file) => file,
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(Self::io_error(error)),
        };
        file.lock_shared().map_err(Self::io_error)?;
        let result = BufReader::new(&file)
            .lines()
            .map(|line| {
                let line = line.map_err(Self::io_error)?;
                serde_json::from_str(&line).map_err(Self::io_error)
            })
            .collect();
        let _ = file.unlock();
        result
    }

    fn read_legacy_events(&self) -> Vec<SavingsEvent> {
        let events_path = self.path.with_file_name("events.jsonl");
        if events_path.exists() {
            crate::core::savings_ledger::store::load(&events_path)
        } else {
            crate::core::savings_ledger::store::load(&self.path.with_file_name("ledger.jsonl"))
        }
    }

    /// Compares legacy and unified entries by hash and reports accounting drift.
    pub fn reconcile(&self) -> OclaResult<ReconciliationReport> {
        let legacy = self.read_legacy_events();
        let unified = self.read_events()?;
        let mut counts: BTreeMap<String, (usize, usize)> = BTreeMap::new();

        let legacy_tokens: u64 = legacy.iter().map(|event| event.saved_tokens).sum();
        for event in &legacy {
            counts.entry(event.entry_hash.clone()).or_default().0 += 1;
        }

        let unified_tokens: u64 = unified.iter().map(|event| event.saved_tokens).sum();
        for event in &unified {
            counts.entry(event.event_hash.clone()).or_default().1 += 1;
        }

        let matched = counts
            .values()
            .map(|(legacy_count, unified_count)| (*legacy_count).min(*unified_count))
            .sum();
        let double_bookings = counts
            .into_iter()
            .filter_map(|(hash, (legacy_count, unified_count))| {
                (legacy_count > 1 || unified_count > 1).then_some(hash)
            })
            .collect();

        let token_delta = i128::from(unified_tokens) - i128::from(legacy_tokens);
        let token_drift = i64::try_from(token_delta).unwrap_or_else(|_| {
            if token_delta.is_negative() {
                i64::MIN
            } else {
                i64::MAX
            }
        });

        Ok(ReconciliationReport {
            matched,
            unmatched_legacy: legacy.len() - matched,
            unmatched_unified: unified.len() - matched,
            token_drift,
            double_bookings,
        })
    }

    /// Returns unified events associated with the supplied trace identifier.
    ///
    /// Consumed by the P5 unified-ledger query surface in E14 phase 3.
    pub(crate) fn query_by_trace(&self, trace_id: &str) -> Vec<UnifiedSavingsEventV2> {
        self.read_events()
            .unwrap_or_default()
            .into_iter()
            .filter(|event| event.trace_id.as_deref() == Some(trace_id))
            .collect()
    }

    /// Strict reconciliation for CI gates: returns an error on drift or double-booking.
    pub fn reconcile_strict(&self) -> OclaResult<ReconciliationReport> {
        let report = self.reconcile()?;
        if report.token_drift != 0 || !report.double_bookings.is_empty() {
            return Err(OclaError::InvalidRequest(format!(
                "reconciliation drift: {} tokens, {} double-bookings",
                report.token_drift,
                report.double_bookings.len()
            )));
        }
        Ok(report)
    }

    /// Returns the percentage of legacy events that have a matching unified event.
    pub fn reconciliation_coverage(&self) -> f64 {
        let Ok(report) = self.reconcile() else {
            return 0.0;
        };
        let total = report.matched + report.unmatched_legacy;
        if total == 0 {
            return 100.0;
        }
        (report.matched as f64 / total as f64) * 100.0
    }

    pub(crate) fn from_savings_event(event: &SavingsEvent) -> OclaResult<UnifiedSavingsEventV2> {
        let timestamp_epoch_ms = chrono::DateTime::parse_from_rfc3339(&event.ts)
            .map_err(Self::io_error)?
            .timestamp_millis();
        let timestamp_epoch_ms = u64::try_from(timestamp_epoch_ms)
            .map_err(|error| Self::io_error(format!("invalid event timestamp: {error}")))?;

        Ok(UnifiedSavingsEventV2 {
            tool_name: event.tool.clone(),
            mode: event.mechanism.clone(),
            original_tokens: event.baseline_tokens,
            compressed_tokens: event.actual_tokens,
            saved_tokens: event.saved_tokens,
            content_hash: event.repo_hash.clone(),
            timestamp_epoch_ms,
            prev_hash: event.prev_hash.clone(),
            event_hash: event.entry_hash.clone(),
            intent: event.intent_tag.clone(),
            outcome: event.outcome.clone(),
            routing_decision: event.model_routed.clone(),
            agent_id: Some(event.agent_id.clone()),
            efficiency_etpao: None,
            attribution_id: event
                .attribution_id
                .clone()
                .unwrap_or_else(|| event.repo_hash.clone()),
            trace_id: OclaRequestContext::current_trace_id(),
            request_id: OclaRequestContext::current_request_id(),
            session_id: OclaRequestContext::current_session_id(),
            quality_ref: None,
        })
    }
}

/// The prefix of a ledger file this process already validated record by
/// record (#2196): appending used to parse the whole file under the exclusive
/// lock on every tool call, which on a 166 MiB ledger stalled each read.
///
/// The prefix is trusted only while the file is the same file (device and
/// inode on Unix), has not shrunk, and still ends its validated part with the
/// exact bytes this process last validated or wrote there. Otherwise — a
/// rotation, truncation, rewrite or another process changing the tip — the
/// whole file is scanned again. Records appended by other processes after the
/// validated prefix are always parsed, so corruption there is still refused.
/// `verify_chain` keeps validating the full file.
#[derive(Clone, Debug)]
struct ValidatedTip {
    path: PathBuf,
    file_id: Option<(u64, u64)>,
    len: u64,
    last_hash: Option<String>,
    /// The last validated record's bytes, including its newline.
    last_line: Vec<u8>,
}

/// Ledgers whose tip this process tracks; a few paths at most (the data-dir
/// ledger, plus test ledgers), bounded anyway.
const MAX_VALIDATED_TIPS: usize = 8;

static VALIDATED_TIPS: std::sync::Mutex<Vec<ValidatedTip>> = std::sync::Mutex::new(Vec::new());

fn validated_tips() -> std::sync::MutexGuard<'static, Vec<ValidatedTip>> {
    VALIDATED_TIPS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn remember_validated_tip(tip: ValidatedTip) {
    let mut tips = validated_tips();
    tips.retain(|known| known.path != tip.path);
    if tips.len() >= MAX_VALIDATED_TIPS {
        tips.remove(0);
    }
    tips.push(tip);
}

fn forget_validated_tip(path: &std::path::Path) {
    validated_tips().retain(|known| known.path != path);
}

#[cfg(unix)]
fn file_id(file: &File) -> Option<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    file.metadata().ok().map(|m| (m.dev(), m.ino()))
}

#[cfg(not(unix))]
fn file_id(_file: &File) -> Option<(u64, u64)> {
    None
}

/// The chain tip found by a scan: the last record's hash, the scanned length,
/// and whether the file ends with a newline.
struct ScannedTip {
    last_hash: Option<String>,
    len: u64,
    terminated: bool,
}

#[cfg(test)]
thread_local! {
    static RECORDS_PARSED: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

impl FileUnifiedLedger {
    /// The chain tip of the locked ledger `file`, parsing only what this
    /// process has not validated yet. Fails on any malformed record parsed.
    fn validated_tip(&self, file: &File) -> OclaResult<ScannedTip> {
        let len = file.metadata().map_err(Self::io_error)?.len();
        let known = validated_tips()
            .iter()
            .find(|known| known.path == self.path)
            .cloned();
        if let Some(known) = known
            && known.file_id == file_id(file)
            && known.len <= len
            && Self::tail_matches(file, &known)?
        {
            return Self::scan_from(file, known.len, known.last_hash, true);
        }
        forget_validated_tip(&self.path);
        Self::scan_from(file, 0, None, true)
    }

    fn tail_matches(mut file: &File, known: &ValidatedTip) -> OclaResult<bool> {
        let Some(start) = known.len.checked_sub(known.last_line.len() as u64) else {
            return Ok(false);
        };
        let mut tail = vec![0u8; known.last_line.len()];
        file.seek(SeekFrom::Start(start)).map_err(Self::io_error)?;
        match std::io::Read::read_exact(&mut file, &mut tail) {
            Ok(()) => Ok(tail == known.last_line),
            Err(error) if error.kind() == ErrorKind::UnexpectedEof => Ok(false),
            Err(error) => Err(Self::io_error(error)),
        }
    }

    /// Parse every record from byte `start` to the end of `file`.
    fn scan_from(
        mut file: &File,
        start: u64,
        mut last_hash: Option<String>,
        mut terminated: bool,
    ) -> OclaResult<ScannedTip> {
        file.seek(SeekFrom::Start(start)).map_err(Self::io_error)?;
        let mut reader = BufReader::new(&mut file);
        let mut len = start;
        let mut line = Vec::new();
        loop {
            line.clear();
            let read = reader
                .read_until(b'\n', &mut line)
                .map_err(Self::io_error)?;
            if read == 0 {
                break;
            }
            len += read as u64;
            terminated = line.ends_with(b"\n");
            let mut record = line.as_slice();
            record = record.strip_suffix(b"\n").unwrap_or(record);
            record = record.strip_suffix(b"\r").unwrap_or(record);
            let previous: UnifiedSavingsEventV2 =
                serde_json::from_slice(record).map_err(Self::io_error)?;
            #[cfg(test)]
            RECORDS_PARSED.with(|parsed| parsed.set(parsed.get() + 1));
            last_hash = Some(previous.event_hash);
        }
        Ok(ScannedTip {
            last_hash,
            len,
            terminated,
        })
    }
}

impl UnifiedLedger for FileUnifiedLedger {
    fn record_unified(&self, event: UnifiedSavingsEventV2) -> OclaResult<String> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent).map_err(Self::io_error)?;
        }
        let mut file = OpenOptions::new()
            .create(true)
            .read(true)
            .append(true)
            .open(&self.path)
            .map_err(Self::io_error)?;
        file.lock_exclusive().map_err(Self::io_error)?;
        let result = (|| {
            let tip = self.validated_tip(&file)?;
            let last_hash = tip.last_hash.as_deref().unwrap_or("genesis");
            let mut record = event;
            if record.prev_hash != last_hash {
                // Self-heal: when the unified ledger is empty or its tip
                // diverged from the savings chain (file deleted, reset, or
                // concurrent truncation), re-anchor the incoming event as a
                // new genesis rather than permanently rejecting all future
                // writes. The per-session ledger remains the source of truth;
                // the unified ledger is a best-effort mirror for OCLA.
                record.prev_hash = last_hash.to_string();
            }
            let mut line = serde_json::to_vec(&record).map_err(Self::io_error)?;
            line.push(b'\n');
            // A last record without its newline still counts (it parsed);
            // terminate it so the new record starts on its own line.
            let separator: &[u8] = if tip.terminated { b"" } else { b"\n" };
            file.seek(SeekFrom::End(0)).map_err(Self::io_error)?;
            file.write_all(&[separator, &line].concat())
                .map_err(Self::io_error)?;
            remember_validated_tip(ValidatedTip {
                path: self.path.clone(),
                file_id: file_id(&file),
                len: tip.len + (separator.len() + line.len()) as u64,
                last_hash: Some(record.event_hash.clone()),
                last_line: line,
            });
            Ok(record.event_hash)
        })();
        let _ = file.unlock();
        result
    }

    fn verify_chain(&self) -> OclaResult<bool> {
        let events = self.read_events()?;
        Ok(events.iter().enumerate().all(|(index, event)| {
            event.prev_hash
                == if index == 0 {
                    "genesis"
                } else {
                    events[index - 1].event_hash.as_str()
                }
        }))
    }

    fn query_by_attribution(&self, id: &str) -> OclaResult<Option<UnifiedSavingsEventV2>> {
        Ok(self
            .read_events()?
            .into_iter()
            .find(|event| event.attribution_id == id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn savings_event() -> SavingsEvent {
        SavingsEvent {
            ts: "2026-01-01T00:00:00Z".into(),
            tool: "ctx_read".into(),
            mechanism: "compression".into(),
            model_id: "model".into(),
            tokenizer: "tokenizer".into(),
            baseline_tokens: 100,
            actual_tokens: 40,
            saved_tokens: 60,
            bounce_adjustment: 0,
            unit_price_per_m_usd: 1.0,
            saved_usd: 0.00006,
            repo_hash: "repo".into(),
            agent_id: "agent".into(),
            prev_hash: "genesis".into(),
            entry_hash: "event".into(),
            version: "version".into(),
            intent_tag: None,
            outcome: None,
            model_original: None,
            model_routed: None,
            routing_savings: None,
            response_original_tokens: None,
            response_delivered_tokens: None,
            agent_chain_id: None,
            chain_depth: None,
            measurement_method: None,
            evidence_class: None,
            confidence: None,
            request_id: None,
            session_id: None,
            trace_id: None,
            solution_decision: None,
            loc_added: None,
            loc_removed: None,
            path: None,
            lines_added: None,
            lines_removed: None,
            net: None,
            quality_signal: None,
            attribution_group: None,
            attribution_id: None,
            baseline_ref: None,
            price_version: None,
            customer_approval: None,
            settlement_status: None,
            is_first_inject: None,
            cache_read_per_m_usd: None,
            cache_write_per_m_usd: None,
            governor: None,
        }
    }

    #[test]
    fn schema_instantiates_legacy_and_p5_fields() {
        let event = UnifiedSavingsEventV2 {
            tool_name: "context_read".into(),
            mode: "compressed".into(),
            original_tokens: 1_000,
            compressed_tokens: 400,
            saved_tokens: 600,
            content_hash: "blake3:content".into(),
            timestamp_epoch_ms: 1_700_000_000_000,
            prev_hash: "blake3:previous".into(),
            event_hash: "blake3:event".into(),
            intent: Some("summarize".into()),
            outcome: Some("accepted".into()),
            routing_decision: Some("local".into()),
            agent_id: Some("agent-test".into()),
            efficiency_etpao: Some(750),
            attribution_id: "attribution:test".into(),
            trace_id: Some("tr-test".into()),
            request_id: Some("request-test".into()),
            session_id: Some("session-test".into()),
            quality_ref: None,
        };

        assert_eq!(event.saved_tokens, 600);
        assert_eq!(event.attribution_id, "attribution:test");
        assert_eq!(event.intent.as_deref(), Some("summarize"));
    }

    fn request_context() -> OclaRequestContext {
        OclaRequestContext {
            request_id: "request".into(),
            session_id: "session".into(),
            agent_id: "agent".into(),
            content_ref: "content".into(),
            tenant_id: None,
            trace_id: "tr-request".into(),
            task_id: None,
            parent_task_id: None,
        }
    }

    #[test]
    fn request_context_trace_id_reaches_unified_event() {
        let context = request_context();
        let unified = context.scope(|| {
            FileUnifiedLedger::from_savings_event(&savings_event()).expect("legacy event converts")
        });
        assert_eq!(unified.trace_id.as_deref(), Some("tr-request"));
    }

    #[test]
    fn test_unified_event_carries_request_id() {
        let context = request_context();
        let unified = context.scope(|| {
            FileUnifiedLedger::from_savings_event(&savings_event()).expect("legacy event converts")
        });
        assert_eq!(unified.request_id.as_deref(), Some("request"));
    }

    #[test]
    fn test_unified_event_carries_session_id() {
        let context = request_context();
        let unified = context.scope(|| {
            FileUnifiedLedger::from_savings_event(&savings_event()).expect("legacy event converts")
        });
        assert_eq!(unified.session_id.as_deref(), Some("session"));
    }

    fn trace_event(trace_id: &str) -> UnifiedSavingsEventV2 {
        UnifiedSavingsEventV2 {
            tool_name: "ctx_read".into(),
            mode: "compression".into(),
            original_tokens: 100,
            compressed_tokens: 40,
            saved_tokens: 60,
            content_hash: "repo".into(),
            timestamp_epoch_ms: 1,
            prev_hash: "genesis".into(),
            event_hash: "event-1".into(),
            intent: None,
            outcome: None,
            routing_decision: None,
            agent_id: Some("agent".into()),
            efficiency_etpao: None,
            attribution_id: "attr".into(),
            trace_id: Some(trace_id.into()),
            request_id: None,
            session_id: None,
            quality_ref: None,
        }
    }

    #[test]
    fn test_query_by_trace_returns_matching() {
        let dir = tempfile::tempdir().expect("temporary directory");
        let ledger = FileUnifiedLedger::new(dir.path().join("unified.jsonl"));
        ledger
            .record_unified(trace_event("trace-match"))
            .expect("event records");

        assert_eq!(ledger.query_by_trace("trace-match").len(), 1);
    }

    #[test]
    fn test_query_by_trace_empty_on_mismatch() {
        let dir = tempfile::tempdir().expect("temporary directory");
        let ledger = FileUnifiedLedger::new(dir.path().join("unified.jsonl"));
        ledger
            .record_unified(trace_event("trace-match"))
            .expect("event records");

        {
            let found = &ledger.query_by_trace("trace-missing");
            assert!(found.is_empty(), "{found:?}");
        }
    }

    #[test]
    fn file_ledger_records_verifies_and_queries_events() {
        let path = std::env::temp_dir().join(format!(
            "lean-ctx-unified-ledger-{}.jsonl",
            std::process::id()
        ));
        let _ = fs::remove_file(&path);
        let ledger = FileUnifiedLedger::new(path.clone());
        let event = UnifiedSavingsEventV2 {
            tool_name: "ctx_read".into(),
            mode: "compression".into(),
            original_tokens: 100,
            compressed_tokens: 40,
            saved_tokens: 60,
            content_hash: "repo".into(),
            timestamp_epoch_ms: 1,
            prev_hash: "genesis".into(),
            event_hash: "event-1".into(),
            intent: None,
            outcome: None,
            routing_decision: None,
            agent_id: Some("agent".into()),
            efficiency_etpao: None,
            attribution_id: "attr".into(),
            trace_id: None,
            request_id: None,
            session_id: None,
            quality_ref: None,
        };
        assert_eq!(ledger.record_unified(event).unwrap(), "event-1");
        assert!(ledger.verify_chain().unwrap());
        assert_eq!(
            ledger
                .query_by_attribution("attr")
                .unwrap()
                .unwrap()
                .saved_tokens,
            60
        );
        let _ = fs::remove_file(path);
    }

    fn chained(event_hash: &str, prev_hash: &str) -> UnifiedSavingsEventV2 {
        UnifiedSavingsEventV2 {
            tool_name: "ctx_read".into(),
            mode: "compression".into(),
            original_tokens: 100,
            compressed_tokens: 40,
            saved_tokens: 60,
            content_hash: "repo".into(),
            timestamp_epoch_ms: 1,
            prev_hash: prev_hash.into(),
            event_hash: event_hash.into(),
            intent: None,
            outcome: None,
            routing_decision: None,
            agent_id: None,
            efficiency_etpao: None,
            attribution_id: event_hash.into(),
            trace_id: None,
            request_id: None,
            session_id: None,
            quality_ref: None,
        }
    }

    fn records_parsed_by(record: impl FnOnce()) -> u64 {
        let before = RECORDS_PARSED.with(std::cell::Cell::get);
        record();
        RECORDS_PARSED.with(std::cell::Cell::get) - before
    }

    fn append_raw(path: &std::path::Path, bytes: &[u8]) {
        let mut file = OpenOptions::new().append(true).open(path).unwrap();
        file.write_all(bytes).unwrap();
    }

    /// #2196: appending parses only the records this process has not
    /// validated yet, instead of the whole ledger on every tool call, while
    /// records other writers append — and malformed ones among them — are
    /// still parsed, and a replaced or shrunk file is scanned in full again.
    #[test]
    fn append_parses_only_records_past_the_validated_tip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("unified_ledger.jsonl");
        let ledger = FileUnifiedLedger::new(path.clone());

        let mut prev = "genesis".to_string();
        for index in 0..50 {
            let hash = format!("event-{index}");
            ledger.record_unified(chained(&hash, &prev)).unwrap();
            prev = hash;
        }
        // Every append after the first scan parses nothing old.
        let parsed = records_parsed_by(|| {
            ledger.record_unified(chained("event-50", &prev)).unwrap();
        });
        assert_eq!(parsed, 0, "the validated prefix was parsed again");

        // Another process appends a record: it is parsed and chained onto.
        let mut foreign = serde_json::to_vec(&chained("foreign", "event-50")).unwrap();
        foreign.push(b'\n');
        append_raw(&path, &foreign);
        let parsed = records_parsed_by(|| {
            assert_eq!(
                ledger
                    .record_unified(chained("event-51", "foreign"))
                    .unwrap(),
                "event-51"
            );
        });
        assert_eq!(parsed, 1);
        assert!(ledger.verify_chain().unwrap());

        // A malformed record past the tip is still refused.
        append_raw(&path, b"{not json\n");
        assert!(
            ledger
                .record_unified(chained("event-52", "event-51"))
                .is_err()
        );

        // A replaced, shorter file is scanned in full and re-anchored onto.
        let replacement = path.with_extension("new");
        let mut first = serde_json::to_vec(&chained("fresh", "genesis")).unwrap();
        first.push(b'\n');
        fs::write(&replacement, &first).unwrap();
        fs::rename(&replacement, &path).unwrap();
        let parsed = records_parsed_by(|| {
            ledger.record_unified(chained("after", "event-51")).unwrap();
        });
        assert_eq!(parsed, 1);
        let events = ledger.read_events().unwrap();
        assert_eq!(events.last().unwrap().prev_hash, "fresh");
        assert!(ledger.verify_chain().unwrap());
    }

    #[test]
    fn a_malformed_earlier_record_is_refused_on_the_first_scan() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("unified_ledger.jsonl");
        let mut good = serde_json::to_vec(&chained("event-0", "genesis")).unwrap();
        good.push(b'\n');
        fs::write(&path, [b"{broken\n".as_slice(), &good].concat()).unwrap();
        let ledger = FileUnifiedLedger::new(path);
        assert!(
            ledger
                .record_unified(chained("event-1", "event-0"))
                .is_err()
        );
    }

    fn legacy_event(saved_tokens: u64) -> SavingsEvent {
        serde_json::from_value(serde_json::json!({
            "ts": "2026-06-01T00:00:00+00:00",
            "tool": "ctx_read",
            "mechanism": "compression",
            "model_id": "test-model",
            "tokenizer": "o200k_base",
            "baseline_tokens": 100,
            "actual_tokens": 100 - saved_tokens,
            "saved_tokens": saved_tokens,
            "bounce_adjustment": 0,
            "unit_price_per_m_usd": 2.5,
            "saved_usd": saved_tokens as f64 * 2.5 / 1_000_000.0,
            "repo_hash": "repo",
            "agent_id": "agent",
            "prev_hash": "",
            "entry_hash": "",
            "version": "test"
        }))
        .unwrap()
    }

    #[test]
    fn reconcile_matches_legacy_and_unified_events() {
        let dir = tempfile::tempdir().unwrap();
        let savings = dir.path().join("savings");
        let legacy_path = savings.join("events.jsonl");
        let unified_path = savings.join("unified_ledger.jsonl");
        let legacy =
            crate::core::savings_ledger::store::append(&legacy_path, legacy_event(60)).unwrap();
        let ledger = FileUnifiedLedger::new(unified_path);
        ledger
            .record_unified(FileUnifiedLedger::from_savings_event(&legacy).unwrap())
            .unwrap();

        assert_eq!(
            ledger.reconcile().unwrap(),
            ReconciliationReport {
                matched: 1,
                unmatched_legacy: 0,
                unmatched_unified: 0,
                token_drift: 0,
                double_bookings: Vec::new(),
            }
        );
    }

    #[test]
    fn reconcile_reports_drift_and_double_bookings() {
        let dir = tempfile::tempdir().unwrap();
        let savings = dir.path().join("savings");
        let legacy_path = savings.join("events.jsonl");
        let unified_path = savings.join("unified_ledger.jsonl");
        let legacy =
            crate::core::savings_ledger::store::append(&legacy_path, legacy_event(60)).unwrap();
        let ledger = FileUnifiedLedger::new(unified_path);
        let unified = FileUnifiedLedger::from_savings_event(&legacy).unwrap();
        ledger.record_unified(unified.clone()).unwrap();
        let mut duplicate = unified;
        duplicate.prev_hash = duplicate.event_hash.clone();
        ledger.record_unified(duplicate).unwrap();

        let report = ledger.reconcile().unwrap();
        assert_eq!(report.matched, 1);
        assert_eq!(report.unmatched_legacy, 0);
        assert_eq!(report.unmatched_unified, 1);
        assert_eq!(report.token_drift, 60);
        assert_eq!(report.double_bookings, vec![legacy.entry_hash]);
    }

    #[test]
    fn test_reconcile_strict_passes_on_clean_dual_write() {
        let dir = tempfile::tempdir().unwrap();
        let savings = dir.path().join("savings");
        let legacy_path = savings.join("events.jsonl");
        let unified_path = savings.join("unified_ledger.jsonl");
        let legacy =
            crate::core::savings_ledger::store::append(&legacy_path, legacy_event(60)).unwrap();
        let ledger = FileUnifiedLedger::new(unified_path);
        ledger
            .record_unified(FileUnifiedLedger::from_savings_event(&legacy).unwrap())
            .unwrap();

        assert!(ledger.reconcile_strict().is_ok());
    }

    #[test]
    fn test_reconcile_strict_fails_on_drift() {
        let dir = tempfile::tempdir().unwrap();
        let savings = dir.path().join("savings");
        let legacy_path = savings.join("events.jsonl");
        let unified_path = savings.join("unified_ledger.jsonl");
        let legacy =
            crate::core::savings_ledger::store::append(&legacy_path, legacy_event(60)).unwrap();
        let ledger = FileUnifiedLedger::new(unified_path);
        let mut unified = FileUnifiedLedger::from_savings_event(&legacy).unwrap();
        unified.saved_tokens = 59;
        ledger.record_unified(unified).unwrap();

        assert!(ledger.reconcile_strict().is_err());
    }

    #[test]
    fn test_format_reconciliation_report_pass() {
        let report = ReconciliationReport {
            matched: 1,
            unmatched_legacy: 0,
            unmatched_unified: 0,
            token_drift: 0,
            double_bookings: Vec::new(),
        };

        assert!(format_reconciliation_report(&report).contains("Status: PASS"));
    }

    #[test]
    fn test_format_reconciliation_report_fail() {
        let report = ReconciliationReport {
            matched: 1,
            unmatched_legacy: 2,
            unmatched_unified: 3,
            token_drift: 4,
            double_bookings: vec!["event".into()],
        };
        let formatted = format_reconciliation_report(&report);

        assert!(formatted.contains("Unmatched legacy: 2"));
        assert!(formatted.contains("Unmatched unified: 3"));
        assert!(formatted.contains("Token drift: 4"));
        assert!(formatted.contains("Double bookings: 1"));
        assert!(formatted.contains("Status: FAIL"));
    }

    #[test]
    fn test_reconciliation_coverage_100_on_match() {
        let dir = tempfile::tempdir().unwrap();
        let savings = dir.path().join("savings");
        let legacy_path = savings.join("events.jsonl");
        let unified_path = savings.join("unified_ledger.jsonl");
        let legacy =
            crate::core::savings_ledger::store::append(&legacy_path, legacy_event(60)).unwrap();
        let ledger = FileUnifiedLedger::new(unified_path);
        ledger
            .record_unified(FileUnifiedLedger::from_savings_event(&legacy).unwrap())
            .unwrap();

        assert_eq!(ledger.reconciliation_coverage(), 100.0);
    }

    #[test]
    fn test_reconciliation_coverage_0_on_empty() {
        let dir = tempfile::tempdir().unwrap();
        let ledger = FileUnifiedLedger::new(dir.path().join("savings/unified_ledger.jsonl"));

        assert_eq!(ledger.reconciliation_coverage(), 100.0);
    }
}
