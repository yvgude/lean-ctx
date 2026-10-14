//! Append-only, SHA-256 hash-chained JSONL store for [`SavingsEvent`]s.
//!
//! Appends are serialised across processes with an advisory file lock (`fs2`), so the
//! MCP server and CLI can both write to one correct chain. The last hash is read from
//! the file tail under the lock (O(1) per append), not cached, to stay correct under
//! concurrent writers. Cryptographic signing of batches is a later phase (G5/G6).

use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;

use fs2::FileExt;

use super::event::{
    CustomerApproval, EvidenceClass, MeasurementMethod, SavingsEvent, SettlementStatus,
    compute_hash,
};
use super::evidence_projection::{
    LedgerProjectionErrorV2, MAX_LEDGER_SNAPSHOT_BYTES_V2, VerifiedLedgerSnapshotV2,
};

pub const GENESIS: &str = "genesis";
const TAIL_READ_BYTES: u64 = 8192;

/// Default ledger location: `<data_dir>/savings/ledger.jsonl`. Local only.
pub fn default_path() -> Option<PathBuf> {
    let dir = crate::core::data_dir::lean_ctx_data_dir().ok()?;
    let sub = dir.join("savings");
    fs::create_dir_all(&sub).ok()?;
    Some(sub.join("ledger.jsonl"))
}

/// Read, parse and verify one bounded ledger snapshot from a single locked file handle.
///
/// This is the file-backed entry point for evidence projection. It refuses symlinks and
/// non-regular files, takes the same advisory lock used by ledger writers, reads at most
/// 4 MiB, and validates chain plus event semantics before returning an opaque verified type.
pub fn read_verified_snapshot_v2(
    path: &Path,
) -> Result<VerifiedLedgerSnapshotV2, LedgerSnapshotReadErrorV2> {
    let link_metadata = fs::symlink_metadata(path)
        .map_err(|error| LedgerSnapshotReadErrorV2::Io(error.to_string()))?;
    if link_metadata.file_type().is_symlink() || !link_metadata.file_type().is_file() {
        return Err(LedgerSnapshotReadErrorV2::NotRegular);
    }

    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    options.custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK);
    let mut file = options
        .open(path)
        .map_err(|error| LedgerSnapshotReadErrorV2::Io(error.to_string()))?;
    FileExt::lock_shared(&file)
        .map_err(|error| LedgerSnapshotReadErrorV2::Io(error.to_string()))?;

    let result = (|| {
        let before = file
            .metadata()
            .map_err(|error| LedgerSnapshotReadErrorV2::Io(error.to_string()))?;
        if !before.file_type().is_file() {
            return Err(LedgerSnapshotReadErrorV2::NotRegular);
        }
        if before.len() > MAX_LEDGER_SNAPSHOT_BYTES_V2 {
            return Err(LedgerSnapshotReadErrorV2::TooLarge);
        }

        let mut bytes = Vec::with_capacity(before.len() as usize);
        (&mut file)
            .take(MAX_LEDGER_SNAPSHOT_BYTES_V2 + 1)
            .read_to_end(&mut bytes)
            .map_err(|error| LedgerSnapshotReadErrorV2::Io(error.to_string()))?;
        if bytes.len() as u64 > MAX_LEDGER_SNAPSHOT_BYTES_V2 {
            return Err(LedgerSnapshotReadErrorV2::TooLarge);
        }

        let after = file
            .metadata()
            .map_err(|error| LedgerSnapshotReadErrorV2::Io(error.to_string()))?;
        if before.len() != after.len()
            || before
                .modified()
                .ok()
                .zip(after.modified().ok())
                .is_some_and(|(before_modified, after_modified)| before_modified != after_modified)
        {
            return Err(LedgerSnapshotReadErrorV2::ChangedDuringRead);
        }

        let text = std::str::from_utf8(&bytes).map_err(|_| LedgerSnapshotReadErrorV2::Utf8)?;
        let mut events = Vec::new();
        for (line_index, line) in text.lines().enumerate() {
            if line.is_empty() {
                return Err(LedgerSnapshotReadErrorV2::MalformedJson { line_index });
            }
            let event = serde_json::from_str::<SavingsEvent>(line)
                .map_err(|_| LedgerSnapshotReadErrorV2::MalformedJson { line_index })?;
            events.push(event);
        }
        VerifiedLedgerSnapshotV2::try_from_events(events).map_err(LedgerSnapshotReadErrorV2::from)
    })();

    let _ = FileExt::unlock(&file);
    result
}

#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum LedgerSnapshotReadErrorV2 {
    #[error("ledger snapshot I/O failed: {0}")]
    Io(String),
    #[error("ledger snapshot is not a regular file")]
    NotRegular,
    #[error("ledger snapshot exceeds byte bound")]
    TooLarge,
    #[error("ledger snapshot is not UTF-8")]
    Utf8,
    #[error("ledger snapshot contains malformed JSON at line {line_index}")]
    MalformedJson { line_index: usize },
    #[error("ledger snapshot changed while it was read")]
    ChangedDuringRead,
    #[error(transparent)]
    Projection(#[from] LedgerProjectionErrorV2),
}

/// Reads the most recent `entry_hash` by scanning only the file tail. Returns
/// [`GENESIS`] for an empty/new file.
fn read_last_hash_from_tail(file: &mut fs::File) -> std::io::Result<String> {
    let len = file.seek(SeekFrom::End(0))?;
    if len == 0 {
        return Ok(GENESIS.to_string());
    }
    let read_size = len.min(TAIL_READ_BYTES);
    file.seek(SeekFrom::End(-(read_size as i64)))?;
    let mut buf = vec![0u8; read_size as usize];
    file.read_exact(&mut buf)?;
    let text = String::from_utf8_lossy(&buf);
    for line in text.lines().rev() {
        if let Ok(ev) = serde_json::from_str::<SavingsEvent>(line) {
            return Ok(ev.entry_hash);
        }
    }
    Ok(GENESIS.to_string())
}

/// Appends one event, filling `prev_hash`/`entry_hash` under an exclusive lock.
/// Returns the finalised event. Best-effort on serialise failure (no write, no error).
pub fn append(path: &Path, mut ev: SavingsEvent) -> std::io::Result<SavingsEvent> {
    if ev.measurement_method.is_none() {
        ev.measurement_method = Some(match ev.mechanism.as_str() {
            "compression" => MeasurementMethod::DirectCount,
            "routing" => MeasurementMethod::BaselineEstimate,
            "caching" => MeasurementMethod::ProviderReconciled,
            _ => MeasurementMethod::Unknown,
        });
    }
    if ev.evidence_class.is_none() {
        ev.evidence_class = Some(match ev.mechanism.as_str() {
            "compression" | "caching" => EvidenceClass::Measured,
            "routing" => EvidenceClass::Approximated,
            _ => EvidenceClass::Unclassified,
        });
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut file = OpenOptions::new()
        .create(true)
        .read(true)
        .append(true)
        .open(path)?;
    file.lock_exclusive()?;
    let result = append_locked(&mut file, &mut ev);
    let _ = FileExt::unlock(&file);
    result.map(|()| ev)
}

fn append_locked(file: &mut fs::File, ev: &mut SavingsEvent) -> std::io::Result<()> {
    let prev = read_last_hash_from_tail(file)?;
    ev.entry_hash = compute_hash(&prev, &ev.canonical_content());
    ev.prev_hash = prev;
    if let Ok(line) = serde_json::to_string(ev) {
        let (line, _) = crate::core::secret_detection::scan_and_redact_from_config(&line);
        file.seek(SeekFrom::End(0))?;
        writeln!(file, "{line}")?;
    }
    Ok(())
}

/// Loads every event (whole file). Callers that only need totals should prefer
/// [`summarize`], which streams.
pub fn load(path: &Path) -> Vec<SavingsEvent> {
    let Ok(file) = fs::File::open(path) else {
        return Vec::new();
    };
    BufReader::new(file)
        .lines()
        .map_while(Result::ok)
        .filter_map(|l| serde_json::from_str(&l).ok())
        .collect()
}

#[derive(Debug, Clone)]
pub struct VerifyResult {
    pub total: usize,
    pub valid: bool,
    pub first_invalid_at: Option<usize>,
}

impl VerifyResult {
    pub fn empty() -> Self {
        Self {
            total: 0,
            valid: true,
            first_invalid_at: None,
        }
    }

    fn invalid_at(total: usize) -> Self {
        Self {
            total,
            valid: false,
            first_invalid_at: Some(total),
        }
    }
}

/// Re-walks the chain from genesis, recomputing each hash. Detects any edited,
/// reordered, inserted, or removed entry.
pub fn verify(path: &Path) -> VerifyResult {
    let Ok(file) = fs::File::open(path) else {
        return VerifyResult::empty();
    };
    let mut prev = GENESIS.to_string();
    let mut total = 0usize;
    for line in BufReader::new(file).lines().map_while(Result::ok) {
        let ev: SavingsEvent = match serde_json::from_str(&line) {
            Ok(e) => e,
            Err(_) => return VerifyResult::invalid_at(total),
        };
        if ev.prev_hash != prev {
            return VerifyResult::invalid_at(total);
        }
        // Accept the v2 (integer micro-USD) hash or the legacy v1 (`{:.6}`) hash, so clean
        // pre-v2 ledgers keep verifying while new appends use the round-trip-stable scheme.
        if !ev.hash_matches(&prev) {
            return VerifyResult::invalid_at(total);
        }
        prev = ev.entry_hash;
        total += 1;
    }
    VerifyResult {
        total,
        valid: true,
        first_invalid_at: None,
    }
}

/// Re-hashes the whole ledger under the current (v2) canonical scheme, rewriting the file in
/// place. Repairs a chain that broke purely from the legacy `{:.6}` float round-trip bug (not
/// tampering): the stored event *content* is preserved verbatim, only `prev_hash`/`entry_hash`
/// are recomputed. Returns the number of re-chained events.
///
/// The rewrite happens under the same exclusive lock as [`append`] and truncates in place
/// (the inode is kept), so a concurrent appender that is blocked on the lock resumes correctly
/// against the migrated tail instead of writing to an orphaned file.
pub fn rechain(path: &Path) -> std::io::Result<usize> {
    let mut file = OpenOptions::new().read(true).write(true).open(path)?;
    file.lock_exclusive()?;
    let result = rechain_locked(&mut file);
    let _ = FileExt::unlock(&file);
    result
}

fn rechain_locked(file: &mut fs::File) -> std::io::Result<usize> {
    file.seek(SeekFrom::Start(0))?;
    let mut content = String::new();
    file.read_to_string(&mut content)?;

    let mut prev = GENESIS.to_string();
    let mut out = String::with_capacity(content.len() + 64);
    let mut count = 0usize;
    for line in content.lines() {
        let Ok(mut ev) = serde_json::from_str::<SavingsEvent>(line) else {
            continue;
        };
        ev.prev_hash = prev.clone();
        ev.entry_hash = compute_hash(&prev, &ev.canonical_content());
        prev.clone_from(&ev.entry_hash);
        if let Ok(serialized) = serde_json::to_string(&ev) {
            out.push_str(&serialized);
            out.push('\n');
            count += 1;
        }
    }

    file.set_len(0)?;
    file.seek(SeekFrom::Start(0))?;
    file.write_all(out.as_bytes())?;
    file.flush()?;
    Ok(count)
}

#[derive(Debug, Clone, Default)]
pub struct LedgerSummary {
    pub total_events: usize,
    /// Gross saved tokens (read events only; bounce events do not inflate this).
    pub saved_tokens: u64,
    /// Net USD: read savings minus bounce events (which carry negative `saved_usd`).
    pub saved_usd: f64,
    /// Tokens later wasted by a compressed->full re-read (sum of `bounce_adjustment`).
    pub bounce_tokens: u64,
    /// Number of recorded bounce events.
    pub bounce_events: usize,
    /// Distinct tokenizers that produced the recorded counts (usually just `o200k_base`).
    pub tokenizers: Vec<String>,
    /// (model_id, saved_tokens, saved_usd), descending by tokens.
    pub by_model: Vec<(String, u64, f64)>,
    /// (YYYY-MM-DD, saved_tokens, saved_usd, baseline_tokens), ascending by day.
    pub by_day: Vec<(String, u64, f64, u64)>,
    /// (tool, saved_tokens), descending by tokens.
    pub by_tool: Vec<(String, u64)>,
    /// (mechanism, saved_tokens, saved_usd), descending by USD — the
    /// attribution slice (enterprise#19): compression | routing | caching.
    pub by_mechanism: Vec<(String, u64, f64)>,
    /// G8 Token-Stream USD Attribution (#1191): (stream_name, saved_tokens, saved_usd).
    /// Streams: "first_inject" (cache_write rate) and "re_read" (cache_read rate).
    pub by_stream: Vec<(String, u64, f64)>,
}

impl LedgerSummary {
    /// Net saved tokens = gross savings minus bounce.
    pub fn net_saved_tokens(&self) -> u64 {
        self.saved_tokens.saturating_sub(self.bounce_tokens)
    }
}

/// Per-day learning trend: `(YYYY-MM-DD, bounce_events, read_events)`,
/// ascending by day, limited to the last `days` calendar days (by event ts).
/// `read_events` counts savings-bearing read events (`ctx_read`) — the
/// denominator the bounce rate is honest against, since bounces invalidate
/// exactly those compressed reads (#507).
pub fn daily_bounce_trend(path: &Path, days: u32) -> Vec<(String, u64, u64)> {
    use std::collections::BTreeMap;
    let Ok(file) = fs::File::open(path) else {
        return Vec::new();
    };
    let cutoff = chrono::Utc::now() - chrono::Duration::days(i64::from(days));

    let mut by_day: BTreeMap<String, (u64, u64)> = BTreeMap::new();
    for line in BufReader::new(file).lines().map_while(Result::ok) {
        let Ok(ev) = serde_json::from_str::<SavingsEvent>(&line) else {
            continue;
        };
        let Ok(ts) = chrono::DateTime::parse_from_rfc3339(&ev.ts) else {
            continue;
        };
        if ts.with_timezone(&chrono::Utc) < cutoff {
            continue;
        }
        let day = ev.ts.get(..10).unwrap_or("").to_string();
        if day.is_empty() {
            continue;
        }
        let entry = by_day.entry(day).or_default();
        if ev.tool == "bounce" {
            entry.0 += 1;
        } else if ev.tool == "ctx_read" {
            entry.1 += 1;
        }
    }
    by_day.into_iter().map(|(d, (b, r))| (d, b, r)).collect()
}

/// In-memory summary cache: `(file_len, LedgerSummary)`. Invalidated when the
/// ledger grows (append-only). Avoids re-parsing the entire file on every
/// dashboard refresh — critical for large ledgers (40MB+, 70k+ events).
static SUMMARY_CACHE: std::sync::Mutex<Option<(u64, LedgerSummary)>> = std::sync::Mutex::new(None);

/// Returns a cached summary if the ledger hasn't grown since the last read.
/// Falls back to a full re-parse on size change or cache miss.
pub fn summarize(path: &Path) -> LedgerSummary {
    let file_len = fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    if let Ok(guard) = SUMMARY_CACHE.lock() {
        if let Some((cached_len, ref summary)) = *guard {
            if cached_len == file_len {
                return summary.clone();
            }
        }
    }
    let result = summarize_uncached(path);
    if let Ok(mut guard) = SUMMARY_CACHE.lock() {
        *guard = Some((file_len, result.clone()));
    }
    result
}

/// Full re-parse of the ledger file (expensive for large files).
fn summarize_uncached(path: &Path) -> LedgerSummary {
    use std::collections::HashMap;
    let Ok(file) = fs::File::open(path) else {
        return LedgerSummary::default();
    };

    let mut s = LedgerSummary::default();
    let mut by_model: HashMap<String, (u64, f64)> = HashMap::new();
    let mut by_day: HashMap<String, (u64, f64, u64)> = HashMap::new();
    let mut by_tool: HashMap<String, u64> = HashMap::new();
    let mut by_mechanism: HashMap<String, (u64, f64)> = HashMap::new();
    let mut tokenizers: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    let mut first_inject: (u64, f64) = (0, 0.0);
    let mut re_read: (u64, f64) = (0, 0.0);

    for line in BufReader::new(file).lines().map_while(Result::ok) {
        let Ok(ev) = serde_json::from_str::<SavingsEvent>(&line) else {
            continue;
        };
        s.total_events += 1;
        s.saved_tokens = s.saved_tokens.saturating_add(ev.saved_tokens);
        s.saved_usd += ev.saved_usd;
        s.bounce_tokens = s.bounce_tokens.saturating_add(ev.bounce_adjustment);
        if ev.bounce_adjustment > 0 {
            s.bounce_events += 1;
        }
        if !ev.tokenizer.is_empty() {
            tokenizers.insert(ev.tokenizer.clone());
        }

        // Attribution slice (enterprise#19): value-bearing events by mechanism.
        // Routing/caching events carry USD at zero saved_tokens, so the filter
        // is on value, not tokens; bounces stay out (they net the headline).
        if ev.saved_tokens > 0 || (ev.saved_usd != 0.0 && ev.bounce_adjustment == 0) {
            let mech = by_mechanism.entry(ev.mechanism.clone()).or_default();
            mech.0 = mech.0.saturating_add(ev.saved_tokens);
            mech.1 += ev.saved_usd;
        }

        // Breakdowns describe *savings* — bounce events (saved_tokens == 0, negative USD)
        // are netted into the headline totals above but kept out of the slices below.
        if ev.saved_tokens > 0 {
            let m = by_model.entry(ev.model_id.clone()).or_default();
            m.0 = m.0.saturating_add(ev.saved_tokens);
            m.1 += ev.saved_usd;

            let day = ev.ts.get(..10).unwrap_or("").to_string();
            let d = by_day.entry(day).or_default();
            d.0 = d.0.saturating_add(ev.saved_tokens);
            d.1 += ev.saved_usd;
            d.2 = d.2.saturating_add(ev.baseline_tokens);

            *by_tool.entry(ev.tool.clone()).or_default() += ev.saved_tokens;
        }

        // G8 Token-Stream Attribution (#1191): classify by first-inject vs re-read.
        // For events with the G8 field, use it directly. For pre-G8 events,
        // estimate based on cache_read rate vs input rate.
        if ev.saved_tokens > 0 {
            let is_first = ev.is_first_inject.unwrap_or(true);
            let stream_usd = if is_first {
                let rate = ev
                    .cache_write_per_m_usd
                    .unwrap_or(ev.unit_price_per_m_usd * 1.25);
                ev.saved_tokens as f64 / 1_000_000.0 * rate
            } else {
                let rate = ev
                    .cache_read_per_m_usd
                    .unwrap_or(ev.unit_price_per_m_usd * 0.1);
                ev.saved_tokens as f64 / 1_000_000.0 * rate
            };
            if is_first {
                first_inject.0 = first_inject.0.saturating_add(ev.saved_tokens);
                first_inject.1 += stream_usd;
            } else {
                re_read.0 = re_read.0.saturating_add(ev.saved_tokens);
                re_read.1 += stream_usd;
            }
        }
    }

    s.by_model = by_model.into_iter().map(|(k, (t, u))| (k, t, u)).collect();
    s.by_model.sort_by_key(|row| std::cmp::Reverse(row.1));

    s.by_day = by_day
        .into_iter()
        .map(|(k, (t, u, b))| (k, t, u, b))
        .collect();
    s.by_day.sort_by(|a, b| a.0.cmp(&b.0));

    s.by_tool = by_tool.into_iter().collect();
    s.by_tool.sort_by_key(|row| std::cmp::Reverse(row.1));

    s.by_mechanism = by_mechanism
        .into_iter()
        .map(|(k, (t, u))| (k, t, u))
        .collect();
    s.by_mechanism
        .sort_by(|a, b| b.2.partial_cmp(&a.2).unwrap_or(std::cmp::Ordering::Equal));

    s.tokenizers = tokenizers.into_iter().collect();

    // G8 Token-Stream Attribution (#1191)
    let mut streams = Vec::new();
    if first_inject.0 > 0 {
        streams.push(("first_inject".to_string(), first_inject.0, first_inject.1));
    }
    if re_read.0 > 0 {
        streams.push(("re_read".to_string(), re_read.0, re_read.1));
    }
    s.by_stream = streams;
    s
}

/// Sums `bounce_adjustment` over the ledger, optionally limited to events within the last
/// `days` (by RFC3339 timestamp). `None` = all time.
pub fn bounce_tokens_since(path: &Path, days: Option<u32>) -> u64 {
    let Ok(file) = fs::File::open(path) else {
        return 0;
    };
    let cutoff = days.map(|d| chrono::Utc::now() - chrono::Duration::days(i64::from(d)));
    let mut total = 0u64;
    for line in BufReader::new(file).lines().map_while(Result::ok) {
        let Ok(ev) = serde_json::from_str::<SavingsEvent>(&line) else {
            continue;
        };
        if ev.bounce_adjustment == 0 {
            continue;
        }
        if let Some(cut) = cutoff {
            match chrono::DateTime::parse_from_rfc3339(&ev.ts) {
                Ok(t) if t.with_timezone(&chrono::Utc) < cut => continue,
                _ => {}
            }
        }
        total = total.saturating_add(ev.bounce_adjustment);
    }
    total
}

/// Aggregated savings for one ledger mechanism.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MechanismSummary {
    pub count: usize,
    pub saved_tokens: u64,
    pub saved_usd: f64,
}

/// Returns events whose mechanism exactly matches `mechanism`.
pub fn query_by_mechanism(path: &Path, mechanism: &str) -> Vec<SavingsEvent> {
    load(path)
        .into_iter()
        .filter(|event| event.mechanism == mechanism)
        .collect()
}

/// Returns events whose evidence class exactly matches `class`.
pub fn query_by_evidence_class(path: &Path, class: &EvidenceClass) -> Vec<SavingsEvent> {
    load(path)
        .into_iter()
        .filter(|event| event.evidence_class.as_ref() == Some(class))
        .collect()
}

/// Returns events whose attribution group exactly matches `group`.
pub fn query_by_attribution_group(path: &Path, group: &str) -> Vec<SavingsEvent> {
    load(path)
        .into_iter()
        .filter(|event| event.attribution_group.as_deref() == Some(group))
        .collect()
}

fn update_event<F>(path: &Path, entry_hash: &str, update: F) -> std::io::Result<SavingsEvent>
where
    F: FnOnce(&mut SavingsEvent),
{
    let mut file = OpenOptions::new().read(true).write(true).open(path)?;
    file.lock_exclusive()?;
    let result = (|| {
        file.seek(SeekFrom::Start(0))?;
        let mut content = String::new();
        file.read_to_string(&mut content)?;

        let mut events: Vec<SavingsEvent> = content
            .lines()
            .map(|line| {
                serde_json::from_str(line)
                    .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))
            })
            .collect::<Result<_, _>>()?;
        let index = events
            .iter()
            .position(|event| event.entry_hash == entry_hash)
            .ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    format!("ledger entry not found: {entry_hash}"),
                )
            })?;
        update(&mut events[index]);

        let mut rewritten = String::new();
        for event in &events {
            let line = serde_json::to_string(event)
                .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
            rewritten.push_str(&line);
            rewritten.push('\n');
        }
        file.set_len(0)?;
        file.seek(SeekFrom::Start(0))?;
        file.write_all(rewritten.as_bytes())?;
        file.flush()?;
        rechain_locked(&mut file)?;

        file.seek(SeekFrom::Start(0))?;
        let mut final_content = String::new();
        file.read_to_string(&mut final_content)?;
        final_content
            .lines()
            .nth(index)
            .and_then(|line| serde_json::from_str(line).ok())
            .ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "updated ledger entry could not be read",
                )
            })
    })();
    let _ = FileExt::unlock(&file);
    result
}

/// Marks one ledger event with the customer's approval state and re-chains the ledger.
pub fn approve_event(
    path: &Path,
    entry_hash: &str,
    approval: &CustomerApproval,
) -> std::io::Result<SavingsEvent> {
    update_event(path, entry_hash, |event| {
        event.customer_approval = Some(approval.clone());
    })
}

/// Marks one ledger event with its settlement state and re-chains the ledger.
pub fn settle_event(
    path: &Path,
    entry_hash: &str,
    status: &SettlementStatus,
) -> std::io::Result<SavingsEvent> {
    update_event(path, entry_hash, |event| {
        event.settlement_status = Some(status.clone());
    })
}

/// Returns positive-savings events that have not received customer approval.
pub fn query_pending_approval(path: &Path) -> Vec<SavingsEvent> {
    load(path)
        .into_iter()
        .filter(|event| event.customer_approval.is_none() && event.saved_usd > 0.0)
        .collect()
}

/// Aggregates event count, saved tokens, and saved USD by mechanism.
pub fn summarize_by_mechanism(path: &Path) -> BTreeMap<String, MechanismSummary> {
    let mut summaries: BTreeMap<String, MechanismSummary> = BTreeMap::new();
    for event in load(path) {
        let summary = summaries.entry(event.mechanism).or_default();
        summary.count += 1;
        summary.saved_tokens = summary.saved_tokens.saturating_add(event.saved_tokens);
        summary.saved_usd += event.saved_usd;
    }
    summaries
}

#[cfg(test)]
mod tests {
    use super::super::event::MECHANISM_COMPRESSION;
    use super::*;

    fn temp_path(tag: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        std::env::temp_dir().join(format!(
            "lean-ctx-ledger-{}-{}-{nanos}.jsonl",
            std::process::id(),
            tag
        ))
    }

    fn sample(saved: u64) -> SavingsEvent {
        SavingsEvent {
            ts: "2026-06-01T12:00:00+00:00".into(),
            tool: "ctx_read".into(),
            mechanism: MECHANISM_COMPRESSION.into(),
            model_id: "claude-3.5-sonnet".into(),
            tokenizer: "o200k_base".into(),
            baseline_tokens: saved + 100,
            actual_tokens: 100,
            saved_tokens: saved,
            bounce_adjustment: 0,
            unit_price_per_m_usd: 3.0,
            saved_usd: saved as f64 / 1_000_000.0 * 3.0,
            repo_hash: "repo".into(),
            agent_id: "local".into(),
            prev_hash: String::new(),
            entry_hash: String::new(),
            version: "3.9.0".into(),
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
        }
    }

    #[test]
    fn daily_bounce_trend_counts_bounces_against_reads_per_day() {
        let p = temp_path("trend");
        let today = chrono::Utc::now().format("%Y-%m-%d").to_string();

        // Two reads + one bounce today; an old event outside the window.
        let mut read1 = sample(500);
        read1.ts = format!("{today}T08:00:00+00:00");
        let mut read2 = sample(300);
        read2.ts = format!("{today}T09:00:00+00:00");
        let mut bounce = sample(0);
        bounce.tool = "bounce".into();
        bounce.bounce_adjustment = 200;
        bounce.ts = format!("{today}T10:00:00+00:00");
        let mut ancient = sample(100);
        ancient.ts = "2020-01-01T00:00:00+00:00".into();

        for ev in [read1, read2, bounce, ancient] {
            append(&p, ev).unwrap();
        }

        let trend = daily_bounce_trend(&p, 14);
        assert_eq!(trend.len(), 1, "only today is inside the window");
        assert_eq!(trend[0].0, today);
        assert_eq!(trend[0].1, 1, "one bounce event");
        assert_eq!(trend[0].2, 2, "two ctx_read events");

        let _ = fs::remove_file(&p);
    }

    #[test]
    fn append_builds_a_valid_chain() {
        let p = temp_path("chain");
        let e1 = append(&p, sample(500)).unwrap();
        let e2 = append(&p, sample(300)).unwrap();

        assert_eq!(e1.prev_hash, GENESIS);
        assert_eq!(e2.prev_hash, e1.entry_hash, "chain must link entries");

        let loaded = load(&p);
        assert_eq!(loaded.len(), 2);

        let v = verify(&p);
        assert!(v.valid, "freshly built chain must verify");
        assert_eq!(v.total, 2);

        let _ = fs::remove_file(&p);
    }

    #[test]
    fn append_fills_p5_evidence_fields_from_mechanism() {
        let p = temp_path("evidence-fallback");
        let cases = [
            (
                "compression",
                MeasurementMethod::DirectCount,
                EvidenceClass::Measured,
            ),
            (
                "routing",
                MeasurementMethod::BaselineEstimate,
                EvidenceClass::Approximated,
            ),
            (
                "caching",
                MeasurementMethod::ProviderReconciled,
                EvidenceClass::Measured,
            ),
            (
                "other",
                MeasurementMethod::Unknown,
                EvidenceClass::Unclassified,
            ),
        ];

        for (mechanism, method, evidence) in cases {
            let mut event = sample(1);
            event.mechanism = mechanism.to_string();
            let appended = append(&p, event).unwrap();
            assert_eq!(appended.measurement_method, Some(method));
            assert_eq!(appended.evidence_class, Some(evidence));
        }

        let mut explicit = sample(1);
        explicit.measurement_method = Some(MeasurementMethod::Holdout);
        explicit.evidence_class = Some(EvidenceClass::Statistical);
        let appended = append(&p, explicit).unwrap();
        assert_eq!(
            appended.measurement_method,
            Some(MeasurementMethod::Holdout)
        );
        assert_eq!(appended.evidence_class, Some(EvidenceClass::Statistical));

        let _ = fs::remove_file(&p);
    }

    /// Regression: appending an event whose USD value lands on a half-micro tie (7831 tokens
    /// @ $2.5/M = 19577.5 µ$) and then verifying must succeed. This exercises the *real* append
    /// and verify call sites (not a single in-line recompute), which is where the tie previously
    /// broke an untampered chain.
    #[test]
    fn append_then_verify_survives_half_micro_tie() {
        let p = temp_path("tie");
        let mut tie = sample(0);
        tie.saved_tokens = 7831;
        tie.baseline_tokens = 8228;
        tie.actual_tokens = 397;
        tie.unit_price_per_m_usd = 2.5;
        // Same computation order as the production recorder.
        tie.saved_usd = tie.saved_tokens as f64 / 1_000_000.0 * tie.unit_price_per_m_usd;

        append(&p, sample(500)).unwrap();
        append(&p, tie).unwrap();
        append(&p, sample(300)).unwrap();

        let v = verify(&p);
        assert!(
            v.valid,
            "a fresh chain with a half-micro tie value must verify"
        );
        assert_eq!(v.total, 3);

        let _ = fs::remove_file(&p);
    }

    #[test]
    fn verify_detects_tampering() {
        let p = temp_path("tamper");
        append(&p, sample(500)).unwrap();
        append(&p, sample(300)).unwrap();

        // Tamper: rewrite the first line with an inflated saved_tokens.
        let content = fs::read_to_string(&p).unwrap();
        let mut lines: Vec<String> = content.lines().map(String::from).collect();
        lines[0] = lines[0].replace("\"saved_tokens\":500", "\"saved_tokens\":999999");
        fs::write(&p, lines.join("\n") + "\n").unwrap();

        let v = verify(&p);
        assert!(!v.valid, "edited entry must fail verification");
        assert_eq!(v.first_invalid_at, Some(0));

        let _ = fs::remove_file(&p);
    }

    #[test]
    fn rechain_repairs_broken_chain_and_preserves_content() {
        let p = temp_path("rechain");
        // Simulate a ledger whose chain hashes are invalid (e.g. broken by the legacy float
        // round-trip bug): the event *content* is intact, only the links are wrong.
        let mut lines = String::new();
        for saved in [500u64, 300, 700] {
            let mut e = sample(saved);
            e.prev_hash = "deadbeef".into();
            e.entry_hash = "deadbeef".into();
            lines.push_str(&serde_json::to_string(&e).unwrap());
            lines.push('\n');
        }
        fs::write(&p, &lines).unwrap();
        assert!(!verify(&p).valid, "broken chain must fail before rechain");

        let n = rechain(&p).unwrap();
        assert_eq!(n, 3, "all events re-chained");

        let v = verify(&p);
        assert!(v.valid, "rechain must produce a valid chain");
        assert_eq!(v.total, 3);

        // Only the chain hashes are recomputed; the saved-token content is preserved.
        assert_eq!(summarize(&p).saved_tokens, 1500);

        let _ = fs::remove_file(&p);
    }

    #[test]
    fn summarize_aggregates_totals_and_slices() {
        let p = temp_path("sum");
        append(&p, sample(500)).unwrap();
        append(&p, sample(300)).unwrap();

        let s = summarize(&p);
        assert_eq!(s.total_events, 2);
        assert_eq!(s.saved_tokens, 800);
        assert!((s.saved_usd - 800.0 / 1_000_000.0 * 3.0).abs() < 1e-9);
        assert_eq!(s.by_model.len(), 1);
        assert_eq!(s.by_model[0].1, 800);
        assert_eq!(s.by_tool[0], ("ctx_read".to_string(), 800));
        assert_eq!(s.by_mechanism.len(), 1);
        assert_eq!(s.by_mechanism[0].0, MECHANISM_COMPRESSION);
        assert_eq!(s.by_mechanism[0].1, 800);

        let _ = fs::remove_file(&p);
    }

    #[test]
    fn summarize_attributes_mechanisms_separately() {
        // enterprise#19: a routing event (USD at zero saved tokens) and a
        // compression event must land in distinct attribution rows; a bounce
        // must stay out of the slice while netting the headline.
        let p = temp_path("mech");
        append(&p, sample(500)).unwrap();

        let mut route = sample(0);
        route.tool = "proxy_route".into();
        route.mechanism = "routing".into();
        route.baseline_tokens = 10_000;
        route.actual_tokens = 10_000;
        route.saved_usd = 0.048_75; // 10k tokens × (5.00−0.125)/MTok
        append(&p, route).unwrap();

        let mut bounce = sample(0);
        bounce.tool = "bounce".into();
        bounce.baseline_tokens = 50;
        bounce.actual_tokens = 50;
        bounce.bounce_adjustment = 50;
        bounce.saved_usd = -0.00015;
        append(&p, bounce).unwrap();

        let s = summarize(&p);
        assert_eq!(s.by_mechanism.len(), 2, "compression + routing rows");
        let routing = s
            .by_mechanism
            .iter()
            .find(|(m, _, _)| m == "routing")
            .expect("routing row");
        assert_eq!(routing.1, 0, "routing saves USD, not tokens");
        assert!((routing.2 - 0.048_75).abs() < 1e-9);
        assert!(
            !s.by_mechanism.iter().any(|(m, _, _)| m == "bounce"),
            "bounce is a correction, not an attribution mechanism"
        );
        assert!(verify(&p).valid, "v3 chain must verify");

        let _ = fs::remove_file(&p);
    }

    #[test]
    fn verify_empty_is_valid() {
        let p = temp_path("empty");
        let v = verify(&p);
        assert!(v.valid);
        assert_eq!(v.total, 0);
    }

    fn bounce(wasted: u64) -> SavingsEvent {
        let mut e = sample(0);
        e.tool = "bounce".into();
        e.baseline_tokens = wasted;
        e.actual_tokens = wasted;
        e.saved_tokens = 0;
        e.bounce_adjustment = wasted;
        e.saved_usd = -(wasted as f64 / 1_000_000.0 * 3.0);
        e
    }

    #[test]
    fn bounce_events_net_out_usd_and_track_tokens() {
        let p = temp_path("bounce");
        append(&p, sample(1000)).unwrap();
        append(&p, bounce(200)).unwrap();

        let s = summarize(&p);
        assert_eq!(s.saved_tokens, 1000, "gross savings excludes bounce events");
        assert_eq!(s.bounce_tokens, 200);
        assert_eq!(s.bounce_events, 1);
        assert_eq!(s.net_saved_tokens(), 800);
        // 1000 saved - 200 wasted, both at $3/M.
        assert!((s.saved_usd - 800.0 / 1_000_000.0 * 3.0).abs() < 1e-9);
        assert_eq!(s.tokenizers, vec!["o200k_base".to_string()]);
        assert!(verify(&p).valid, "chain stays valid across event kinds");

        assert_eq!(bounce_tokens_since(&p, None), 200);
        let _ = fs::remove_file(&p);
    }

    #[test]
    fn query_by_mechanism_returns_only_matching_events() {
        let p = temp_path("query-mechanism");
        append(&p, sample(100)).unwrap();
        let mut routing = sample(25);
        routing.mechanism = "routing".into();
        append(&p, routing).unwrap();

        let result = query_by_mechanism(&p, "routing");
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].saved_tokens, 25);
        {
            let found = &query_by_mechanism(&p, "missing");
            assert!(found.is_empty(), "{found:?}");
        }
        let _ = fs::remove_file(&p);
    }

    #[test]
    fn query_by_evidence_class_returns_only_matching_events() {
        let p = temp_path("query-evidence");
        let mut measured = sample(100);
        measured.evidence_class = Some(EvidenceClass::Measured);
        append(&p, measured).unwrap();
        let mut declared = sample(25);
        declared.evidence_class = Some(EvidenceClass::Declared);
        append(&p, declared).unwrap();

        let result = query_by_evidence_class(&p, &EvidenceClass::Measured);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].saved_tokens, 100);
        {
            let found = &query_by_evidence_class(&p, &EvidenceClass::Statistical);
            assert!(found.is_empty(), "{found:?}");
        }
        let _ = fs::remove_file(&p);
    }

    #[test]
    fn query_by_attribution_group_returns_only_matching_events() {
        let p = temp_path("query-attribution");
        let mut first = sample(100);
        first.attribution_group = Some("team-a".into());
        append(&p, first).unwrap();
        let mut second = sample(25);
        second.attribution_group = Some("team-b".into());
        append(&p, second).unwrap();

        let result = query_by_attribution_group(&p, "team-a");
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].saved_tokens, 100);
        {
            let found = &query_by_attribution_group(&p, "team-c");
            assert!(found.is_empty(), "{found:?}");
        }
        let _ = fs::remove_file(&p);
    }

    #[test]
    fn summarize_by_mechanism_aggregates_count_tokens_and_usd() {
        let p = temp_path("summary-mechanism");
        append(&p, sample(100)).unwrap();
        append(&p, sample(25)).unwrap();
        let mut routing = sample(40);
        routing.mechanism = "routing".into();
        append(&p, routing).unwrap();

        let result = summarize_by_mechanism(&p);
        let compression = result.get(MECHANISM_COMPRESSION).unwrap();
        assert_eq!(compression.count, 2);
        assert_eq!(compression.saved_tokens, 125);
        assert!((compression.saved_usd - 125.0 / 1_000_000.0 * 3.0).abs() < 1e-9);
        assert_eq!(result.get("routing").unwrap().saved_tokens, 40);
        let _ = fs::remove_file(&p);
    }

    #[test]
    fn approve_event_updates_state_and_rechains() {
        let p = temp_path("approve");
        let original = append(&p, sample(100)).unwrap();

        let updated = approve_event(&p, &original.entry_hash, &CustomerApproval::Approved).unwrap();

        assert_eq!(updated.customer_approval, Some(CustomerApproval::Approved));
        assert!(verify(&p).valid);
        assert_eq!(
            load(&p)[0].customer_approval,
            Some(CustomerApproval::Approved)
        );
        let _ = fs::remove_file(&p);
    }

    #[test]
    fn settle_event_updates_state_and_rechains() {
        let p = temp_path("settle");
        let original = append(&p, sample(100)).unwrap();

        let updated = settle_event(&p, &original.entry_hash, &SettlementStatus::Settled).unwrap();

        assert_eq!(updated.settlement_status, Some(SettlementStatus::Settled));
        assert!(verify(&p).valid);
        assert_eq!(
            load(&p)[0].settlement_status,
            Some(SettlementStatus::Settled)
        );
        let _ = fs::remove_file(&p);
    }

    #[test]
    fn query_pending_approval_filters_unapproved_positive_savings() {
        let p = temp_path("pending-approval");
        append(&p, sample(100)).unwrap();
        append(&p, sample(0)).unwrap();
        let approved = append(&p, sample(50)).unwrap();
        approve_event(&p, &approved.entry_hash, &CustomerApproval::Approved).unwrap();

        let pending = query_pending_approval(&p);
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].saved_tokens, 100);
        assert!(pending[0].customer_approval.is_none());
        let _ = fs::remove_file(&p);
    }
}
