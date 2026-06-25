//! Append-only, SHA-256 hash-chained JSONL store for [`SavingsEvent`]s.
//!
//! Appends are serialised across processes with an advisory file lock (`fs2`), so the
//! MCP server and CLI can both write to one correct chain. The last hash is read from
//! the file tail under the lock (O(1) per append), not cached, to stay correct under
//! concurrent writers. Cryptographic signing of batches is a later phase (G5/G6).

use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use fs2::FileExt;

use super::event::{SavingsEvent, compute_hash};

pub const GENESIS: &str = "genesis";
const TAIL_READ_BYTES: u64 = 8192;

/// Default ledger location: `<data_dir>/savings/ledger.jsonl`. Local only.
#[must_use]
pub fn default_path() -> Option<PathBuf> {
    let dir = crate::core::data_dir::lean_ctx_data_dir().ok()?;
    let sub = dir.join("savings");
    fs::create_dir_all(&sub).ok()?;
    Some(sub.join("ledger.jsonl"))
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
    #[must_use]
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
    /// (`model_id`, `saved_tokens`, `saved_usd`), descending by tokens.
    pub by_model: Vec<(String, u64, f64)>,
    /// (YYYY-MM-DD, `saved_tokens`, `saved_usd`), ascending by day.
    pub by_day: Vec<(String, u64, f64)>,
    /// (tool, `saved_tokens`), descending by tokens.
    pub by_tool: Vec<(String, u64)>,
}

impl LedgerSummary {
    /// Net saved tokens = gross savings minus bounce.
    #[must_use]
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

/// Streams the ledger and aggregates totals sliceable by model / day / tool.
pub fn summarize(path: &Path) -> LedgerSummary {
    use std::collections::HashMap;
    let Ok(file) = fs::File::open(path) else {
        return LedgerSummary::default();
    };

    let mut s = LedgerSummary::default();
    let mut by_model: HashMap<String, (u64, f64)> = HashMap::new();
    let mut by_day: HashMap<String, (u64, f64)> = HashMap::new();
    let mut by_tool: HashMap<String, u64> = HashMap::new();
    let mut tokenizers: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();

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

            *by_tool.entry(ev.tool.clone()).or_default() += ev.saved_tokens;
        }
    }

    s.by_model = by_model.into_iter().map(|(k, (t, u))| (k, t, u)).collect();
    s.by_model.sort_by_key(|row| std::cmp::Reverse(row.1));

    s.by_day = by_day.into_iter().map(|(k, (t, u))| (k, t, u)).collect();
    s.by_day.sort_by(|a, b| a.0.cmp(&b.0));

    s.by_tool = by_tool.into_iter().collect();
    s.by_tool.sort_by_key(|row| std::cmp::Reverse(row.1));

    s.tokenizers = tokenizers.into_iter().collect();
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

#[cfg(test)]
mod tests {
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
}
