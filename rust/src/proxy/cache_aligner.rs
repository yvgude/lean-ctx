//! Cache-aligner (#940 detect, #974 relocate) — Headroom "cache aligner" port.
//!
//! A stable system prompt is the largest prefix a provider can cache, but a
//! single turn-to-turn-varying token inside it (today's date, a fresh UUID, a
//! git SHA) shifts the bytes and busts the cache on every request. Two opt-in
//! stages address this, both Anthropic-only:
//!
//! 1. **Detect** (`cache_aligner`, #940): a deterministic scan counts the
//!    volatile fields in an *unanchored* system prompt and surfaces the leak on
//!    `/status` — pure measurement, the body is never mutated.
//! 2. **Relocate** (`cache_align_relocate`, #974): rewrites `system` into a
//!    stable block (volatile values replaced by constant placeholders) carrying
//!    the cache breakpoint, plus an *uncached* tail block that re-states the
//!    relocated values. The cacheable prefix then stays byte-stable turn-to-turn
//!    and finally caches; only the small, reprocessed tail changes. Follows the
//!    same stable-first ordering as
//!    `crate::core::neural::cache_alignment::CacheAlignedOutput`.
//!
//! ## Determinism (#498) & cache-safety (#448)
//! Both stages are pure functions of the text: matches come from the
//! `merged_spans` helper (collected, sorted, overlaps merged), and the
//! relocate's placeholders + tail
//! header are byte-constants, so identical input yields byte-identical output and
//! the rewritten prefix is stable across turns. The relocate is idempotent (a
//! second pass sees only placeholders) and only ever fires when the client
//! anchored nothing itself, so it never rewrites a client-cached prefix.

use std::sync::LazyLock;

use regex::Regex;
use serde_json::{Map, Value};

use crate::core::tokens::count_tokens;

/// A type of text that makes an otherwise-stable provider cache prefix vary.
///
/// Positions in [`VolatileReport::volatile_positions`] are UTF-8 byte offsets
/// into the scanned string. Every finding is disjoint and listed in source
/// order, so callers can safely use the offsets for diagnostics without
/// needing to merge overlapping date/timestamp matches themselves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VolatileKind {
    Uuid,
    Timestamp,
    Jwt,
    Hash,
}

/// Measurement-only cache-alignment report for arbitrary text.
///
/// `alignment_score` starts at 100 and loses ten points per finding, floored at
/// zero because the public wire representation is an unsigned byte. Detection
/// never alters the supplied text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VolatileReport {
    pub uuid_count: u32,
    pub timestamp_count: u32,
    pub jwt_count: u32,
    pub hash_count: u32,
    pub total_volatile: u32,
    pub alignment_score: u8,
    pub volatile_positions: Vec<(usize, VolatileKind)>,
}

impl Default for VolatileReport {
    fn default() -> Self {
        Self {
            uuid_count: 0,
            timestamp_count: 0,
            jwt_count: 0,
            hash_count: 0,
            total_volatile: 0,
            alignment_score: 100,
            volatile_positions: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct VolatileMatch {
    start: usize,
    end: usize,
    kind: VolatileKind,
}

/// Detect values that are likely to change between requests and invalidate a
/// provider's KV-cache prefix. This is deliberately regex-free: the scan works
/// on ASCII syntax inside UTF-8 text and has no hidden regex-engine state.
///
/// The result is diagnostic only; this function never rewrites, redacts, or
/// otherwise changes `text`.
pub fn detect_volatile_content(text: &str) -> VolatileReport {
    let bytes = text.as_bytes();
    let mut matches = Vec::new();
    matches.extend(uuid_matches(bytes));
    matches.extend(timestamp_matches(bytes));
    matches.extend(jwt_matches(bytes));
    matches.extend(hash_matches(bytes));

    // Prefer the longest match at one offset. This makes an ISO timestamp win
    // over its date prefix, then discards every later overlap deterministically.
    matches.sort_unstable_by(|left, right| {
        left.start
            .cmp(&right.start)
            .then_with(|| right.end.cmp(&left.end))
            .then_with(|| volatile_kind_rank(left.kind).cmp(&volatile_kind_rank(right.kind)))
    });

    let mut report = VolatileReport::default();
    let mut last_end = 0;
    for found in matches {
        if found.start < last_end {
            continue;
        }
        last_end = found.end;
        match found.kind {
            VolatileKind::Uuid => report.uuid_count += 1,
            VolatileKind::Timestamp => report.timestamp_count += 1,
            VolatileKind::Jwt => report.jwt_count += 1,
            VolatileKind::Hash => report.hash_count += 1,
        }
        report.volatile_positions.push((found.start, found.kind));
    }
    report.total_volatile = report
        .uuid_count
        .saturating_add(report.timestamp_count)
        .saturating_add(report.jwt_count)
        .saturating_add(report.hash_count);
    report.alignment_score = 100u8
        .saturating_sub(u8::try_from(report.total_volatile.saturating_mul(10)).unwrap_or(u8::MAX));
    report
}

fn volatile_kind_rank(kind: VolatileKind) -> u8 {
    match kind {
        VolatileKind::Uuid => 0,
        VolatileKind::Timestamp => 1,
        VolatileKind::Jwt => 2,
        VolatileKind::Hash => 3,
    }
}

fn is_hex(byte: u8) -> bool {
    byte.is_ascii_hexdigit()
}

fn is_token_char(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

fn bounded_by_non_tokens(bytes: &[u8], start: usize, end: usize) -> bool {
    (start == 0 || !is_token_char(bytes[start - 1]))
        && (end == bytes.len() || !is_token_char(bytes[end]))
}

fn uuid_matches(bytes: &[u8]) -> Vec<VolatileMatch> {
    let mut matches = Vec::new();
    if bytes.len() < 36 {
        return matches;
    }
    for start in 0..=bytes.len().saturating_sub(36) {
        let end = start + 36;
        let valid = bytes[start..end].iter().enumerate().all(|(offset, byte)| {
            matches!(offset, 8 | 13 | 18 | 23) && *byte == b'-'
                || !matches!(offset, 8 | 13 | 18 | 23) && is_hex(*byte)
        });
        if valid && bounded_by_non_tokens(bytes, start, end) {
            matches.push(VolatileMatch {
                start,
                end,
                kind: VolatileKind::Uuid,
            });
        }
    }
    matches
}

fn timestamp_matches(bytes: &[u8]) -> Vec<VolatileMatch> {
    let mut matches = Vec::new();
    if bytes.len() < 10 {
        return matches;
    }
    for start in 0..=bytes.len().saturating_sub(10) {
        let Some(date_end) = date_end(bytes, start) else {
            continue;
        };
        let end = datetime_end(bytes, start).unwrap_or(date_end);
        if bounded_by_non_tokens(bytes, start, end) {
            matches.push(VolatileMatch {
                start,
                end,
                kind: VolatileKind::Timestamp,
            });
        }
    }
    matches
}

fn date_end(bytes: &[u8], start: usize) -> Option<usize> {
    let end = start.checked_add(10)?;
    let slice = bytes.get(start..end)?;
    if !slice[0..4].iter().all(u8::is_ascii_digit)
        || slice[4] != b'-'
        || !slice[5..7].iter().all(u8::is_ascii_digit)
        || slice[7] != b'-'
        || !slice[8..10].iter().all(u8::is_ascii_digit)
    {
        return None;
    }
    let month = two_digits(slice[5], slice[6]);
    let day = two_digits(slice[8], slice[9]);
    ((1..=12).contains(&month) && (1..=31).contains(&day)).then_some(end)
}

fn datetime_end(bytes: &[u8], start: usize) -> Option<usize> {
    date_end(bytes, start)?;
    let time_start = start.checked_add(11)?;
    if !matches!(bytes.get(start + 10), Some(b'T' | b' ')) {
        return None;
    }
    let mut end = time_start.checked_add(5)?;
    let time = bytes.get(time_start..end)?;
    if !time[0..2].iter().all(u8::is_ascii_digit)
        || time[2] != b':'
        || !time[3..5].iter().all(u8::is_ascii_digit)
        || two_digits(time[0], time[1]) > 23
        || two_digits(time[3], time[4]) > 59
    {
        return None;
    }
    if bytes.get(end) == Some(&b':') {
        let seconds_end = end.checked_add(3)?;
        let seconds = bytes.get(end + 1..seconds_end)?;
        if !seconds.iter().all(u8::is_ascii_digit) || two_digits(seconds[0], seconds[1]) > 59 {
            return None;
        }
        end = seconds_end;
    }
    if bytes.get(end) == Some(&b'.') {
        let fraction_start = end + 1;
        end = fraction_start;
        while bytes.get(end).is_some_and(u8::is_ascii_digit) {
            end += 1;
        }
        if end == fraction_start {
            return None;
        }
    }
    match bytes.get(end) {
        Some(b'Z') => end += 1,
        Some(b'+' | b'-') => {
            let zone_start = end + 1;
            let zone_end = zone_start.checked_add(5)?;
            let zone = bytes.get(zone_start..zone_end)?;
            if !zone[0..2].iter().all(u8::is_ascii_digit)
                || zone[2] != b':'
                || !zone[3..5].iter().all(u8::is_ascii_digit)
                || two_digits(zone[0], zone[1]) > 23
                || two_digits(zone[3], zone[4]) > 59
            {
                return None;
            }
            end = zone_end;
        }
        _ => {}
    }
    Some(end)
}

fn two_digits(tens: u8, units: u8) -> u8 {
    (tens - b'0') * 10 + (units - b'0')
}

fn is_base64url(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_')
}

fn jwt_matches(bytes: &[u8]) -> Vec<VolatileMatch> {
    let mut matches = Vec::new();
    for start in 0..bytes.len() {
        if start > 0 && (is_base64url(bytes[start - 1]) || bytes[start - 1] == b'.') {
            continue;
        }
        let Some(end) = jwt_end(bytes, start) else {
            continue;
        };
        if end == bytes.len() || (!is_base64url(bytes[end]) && bytes[end] != b'.') {
            matches.push(VolatileMatch {
                start,
                end,
                kind: VolatileKind::Jwt,
            });
        }
    }
    matches
}

fn jwt_end(bytes: &[u8], start: usize) -> Option<usize> {
    let mut cursor = start;
    for segment in 0..3 {
        let segment_start = cursor;
        while bytes.get(cursor).is_some_and(|byte| is_base64url(*byte)) {
            cursor += 1;
        }
        if cursor == segment_start {
            return None;
        }
        if segment < 2 {
            if bytes.get(cursor) != Some(&b'.') {
                return None;
            }
            cursor += 1;
        }
    }
    Some(cursor)
}

fn hash_matches(bytes: &[u8]) -> Vec<VolatileMatch> {
    let mut matches = Vec::new();
    let mut start = 0;
    while start < bytes.len() {
        if !is_hex(bytes[start]) {
            start += 1;
            continue;
        }
        let end = bytes[start..]
            .iter()
            .position(|byte| !is_hex(*byte))
            .map_or(bytes.len(), |offset| start + offset);
        if matches!(end - start, 32 | 40 | 64) && bounded_by_non_tokens(bytes, start, end) {
            matches.push(VolatileMatch {
                start,
                end,
                kind: VolatileKind::Hash,
            });
        }
        start = end;
    }
    matches
}

/// Volatile substrings that change turn-to-turn and so bust an otherwise-stable
/// system-prompt prefix. Deliberately precise (ISO dates/datetimes, UUIDs, full
/// git SHAs) rather than broad, so a stable identifier is never miscounted as
/// volatile. Datetimes are matched alongside bare dates; the span merge below
/// collapses the overlap so a full timestamp counts once.
static VOLATILE_PATTERNS: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    [
        // ISO-8601 datetime: date + time, optional seconds/fraction/zone.
        r"\d{4}-\d{2}-\d{2}[T ]\d{2}:\d{2}(?::\d{2})?(?:\.\d+)?(?:Z|[+-]\d{2}:?\d{2})?",
        // ISO-8601 date.
        r"\d{4}-\d{2}-\d{2}",
        // RFC-4122 UUID.
        r"[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}",
        // git SHA-1 (40 lowercase hex), a common volatile "current commit" field.
        r"\b[0-9a-f]{40}\b",
    ]
    .iter()
    .filter_map(|p| Regex::new(p).ok())
    .collect()
});

/// Result of scanning a system prompt for volatile, cache-busting fields.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct VolatileScan {
    /// Number of distinct (overlap-merged) volatile spans found.
    pub fields: usize,
    /// Total bytes covered by those spans — how much of the prefix is volatile.
    pub volatile_bytes: usize,
}

/// Deterministically collect the volatile spans in `text`, merging overlapping
/// matches (e.g. a datetime and the bare date inside it) so each counts once.
/// Shared by the detector ([`scan_volatile`]) and the relocate
/// ([`relocate_volatile`]) so both see exactly the same fields.
fn merged_spans(text: &str) -> Vec<(usize, usize)> {
    let mut spans: Vec<(usize, usize)> = Vec::new();
    for re in VOLATILE_PATTERNS.iter() {
        spans.extend(re.find_iter(text).map(|m| (m.start(), m.end())));
    }
    if spans.is_empty() {
        return spans;
    }
    spans.sort_unstable();
    let mut merged: Vec<(usize, usize)> = Vec::with_capacity(spans.len());
    for (start, end) in spans {
        match merged.last_mut() {
            Some(last) if start <= last.1 => last.1 = last.1.max(end),
            _ => merged.push((start, end)),
        }
    }
    merged
}

/// Deterministically scan `text` for volatile fields (measurement-only, #940).
pub(crate) fn scan_volatile(text: &str) -> VolatileScan {
    let merged = merged_spans(text);
    VolatileScan {
        fields: merged.len(),
        volatile_bytes: merged.iter().map(|(s, e)| e - s).sum(),
    }
}

/// The plain text of an Anthropic `system` field — a bare string, or every text
/// block of a block array joined with newlines. `None` for any other shape.
pub(crate) fn system_text(system: &Value) -> Option<String> {
    match system {
        Value::String(s) => Some(s.clone()),
        Value::Array(blocks) => {
            let joined = blocks
                .iter()
                .filter_map(|b| b.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("\n");
            (!joined.is_empty()).then_some(joined)
        }
        _ => None,
    }
}

/// Anthropic ignores a cache breakpoint whose prefix is under its minimum
/// cacheable size; relocating below it just churns bytes for no cache win, so the
/// relocate is gated on the same floor as `cache_breakpoint` (#939).
const MIN_STABLE_TOKENS: usize = 1024;

/// Constant header introducing the relocated tail block. Byte-constant so it
/// never perturbs the prefix (#498).
const TAIL_HEADER: &str = "Volatile context (relocated to keep the prompt-cache prefix stable):";

/// The constant placeholder that replaces the `n`-th relocated value in the
/// stable block. Numbered by appearance so the model can map it to the tail and
/// so the rewrite is deterministic; carries no volatile pattern, which is what
/// makes [`relocate_volatile`] idempotent.
fn placeholder(n: usize) -> String {
    format!("[ctx#{n}]")
}

/// A system prompt split for cache alignment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RelocateResult {
    /// System text with every volatile value replaced by a constant placeholder
    /// — byte-stable turn-to-turn, so it is the part that caches.
    pub stable: String,
    /// The relocated volatile values, re-stated in order under a constant header.
    /// Belongs in an *uncached* trailing block.
    pub tail: String,
    /// Number of volatile fields relocated.
    pub fields: usize,
}

/// Split `text` into a byte-stable `stable` part (volatile values → placeholders)
/// and a `tail` that re-states those values. `None` when there is nothing
/// volatile to move, so callers stay a strict no-op.
pub(crate) fn relocate_volatile(text: &str) -> Option<RelocateResult> {
    let spans = merged_spans(text);
    if spans.is_empty() {
        return None;
    }
    let mut stable = String::with_capacity(text.len());
    let mut values: Vec<&str> = Vec::with_capacity(spans.len());
    let mut cursor = 0usize;
    for (start, end) in &spans {
        stable.push_str(&text[cursor..*start]);
        stable.push_str(&placeholder(values.len() + 1));
        values.push(&text[*start..*end]);
        cursor = *end;
    }
    stable.push_str(&text[cursor..]);

    let mut tail = String::from(TAIL_HEADER);
    for (i, value) in values.iter().enumerate() {
        tail.push('\n');
        tail.push_str(&placeholder(i + 1));
        tail.push_str(" = ");
        tail.push_str(value);
    }
    Some(RelocateResult {
        stable,
        tail,
        fields: values.len(),
    })
}

/// A plain `{"type":"text","text":…}` system block.
fn text_block(text: String) -> Map<String, Value> {
    let mut block = Map::new();
    block.insert("type".into(), Value::String("text".into()));
    block.insert("text".into(), Value::String(text));
    block
}

/// The stable block plus the ephemeral cache breakpoint that anchors the prefix.
fn stable_block(text: String) -> Value {
    let mut block = text_block(text);
    block.insert(
        "cache_control".into(),
        serde_json::json!({ "type": "ephemeral" }),
    );
    Value::Object(block)
}

/// Rewrite the Anthropic `system` field in place so volatile values live in an
/// uncached tail block and the stable prefix carries the cache breakpoint.
/// Returns the number of fields relocated (`0` = left untouched).
///
/// Handles a plain string or an array of pure text blocks that carry no
/// `cache_control` of their own (the caller already guards anchored prefixes).
/// Any other shape, or a stable part below [`MIN_STABLE_TOKENS`], is a no-op.
pub(crate) fn apply_anthropic_relocate(doc: &mut Value) -> usize {
    let Some(system) = doc.get_mut("system") else {
        return 0;
    };
    let text = match system {
        Value::String(s) => s.clone(),
        Value::Array(blocks) => {
            let all_plain_text = !blocks.is_empty()
                && blocks.iter().all(|b| {
                    b.get("type").and_then(Value::as_str) == Some("text")
                        && b.get("text").is_some_and(Value::is_string)
                        && b.get("cache_control").is_none()
                });
            if !all_plain_text {
                return 0;
            }
            blocks
                .iter()
                .filter_map(|b| b.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("\n")
        }
        _ => return 0,
    };
    let Some(result) = relocate_volatile(&text) else {
        return 0;
    };
    if count_tokens(&result.stable) < MIN_STABLE_TOKENS {
        return 0;
    }
    *system = Value::Array(vec![
        stable_block(result.stable),
        Value::Object(text_block(result.tail)),
    ]);
    result.fields
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_each_volatile_kind_once() {
        let text = "Today is 2026-06-22. Session 550e8400-e29b-41d4-a716-446655440000 \
                    at commit da39a3ee5e6b4b0d3255bfef95601890afd80709.";
        let scan = scan_volatile(text);
        assert_eq!(scan.fields, 3, "one date, one UUID, one git SHA");
        assert!(scan.volatile_bytes > 0);
    }

    #[test]
    fn datetime_and_inner_date_merge_to_one_span() {
        // The datetime pattern and the bare-date pattern both match the date part;
        // the merge must collapse them so a full timestamp counts exactly once.
        let scan = scan_volatile("Generated at 2026-06-22T15:04:05Z by the agent.");
        assert_eq!(
            scan.fields, 1,
            "overlapping datetime/date spans merge to one"
        );
    }

    #[test]
    fn stable_prompt_has_no_volatile_fields() {
        let scan = scan_volatile("You are a careful senior engineer. Prefer small diffs.");
        assert_eq!(scan, VolatileScan::default());
    }

    #[test]
    fn scan_is_deterministic() {
        let text = "v1 2026-06-22 id 550e8400-e29b-41d4-a716-446655440000 and 2025-01-01";
        assert_eq!(scan_volatile(text), scan_volatile(text));
    }

    #[test]
    fn report_detects_uuid_at_its_byte_position() {
        let text = "run 550e8400-e29b-41d4-a716-446655440000 now";
        assert_eq!(
            detect_volatile_content(text),
            VolatileReport {
                uuid_count: 1,
                total_volatile: 1,
                alignment_score: 90,
                volatile_positions: vec![(4, VolatileKind::Uuid)],
                ..VolatileReport::default()
            }
        );
    }

    #[test]
    fn report_detects_iso_timestamp_once() {
        let text = "built 2026-06-22T15:04:05Z";
        assert_eq!(
            detect_volatile_content(text),
            VolatileReport {
                timestamp_count: 1,
                total_volatile: 1,
                alignment_score: 90,
                volatile_positions: vec![(6, VolatileKind::Timestamp)],
                ..VolatileReport::default()
            }
        );
    }

    #[test]
    fn report_detects_timestamp_fraction_and_zone() {
        let text = "built 2026-06-22T15:04:05.123+02:00";
        let report = detect_volatile_content(text);
        assert_eq!(report.timestamp_count, 1);
        assert_eq!(report.alignment_score, 90);
    }

    #[test]
    fn report_detects_jwt() {
        let jwt = "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.c2lnbmF0dXJl";
        assert_eq!(
            detect_volatile_content(jwt),
            VolatileReport {
                jwt_count: 1,
                total_volatile: 1,
                alignment_score: 90,
                volatile_positions: vec![(0, VolatileKind::Jwt)],
                ..VolatileReport::default()
            }
        );
    }

    #[test]
    fn report_scores_mixed_volatile_content() {
        let text = concat!(
            "2026-06-22 ",
            "550e8400-e29b-41d4-a716-446655440000 ",
            "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.c2lnbmF0dXJl ",
            "da39a3ee5e6b4b0d3255bfef95601890afd80709"
        );
        let report = detect_volatile_content(text);
        assert_eq!(report.timestamp_count, 1);
        assert_eq!(report.uuid_count, 1);
        assert_eq!(report.jwt_count, 1);
        assert_eq!(report.hash_count, 1);
        assert_eq!(report.total_volatile, 4);
        assert_eq!(report.alignment_score, 60);
        assert_eq!(report.volatile_positions.len(), 4);
    }

    #[test]
    fn system_text_reads_string_and_block_array() {
        assert_eq!(
            system_text(&Value::String("hi".into())).as_deref(),
            Some("hi")
        );
        let arr = serde_json::json!([
            {"type": "text", "text": "alpha"},
            {"type": "text", "text": "beta"}
        ]);
        assert_eq!(system_text(&arr).as_deref(), Some("alpha\nbeta"));
        assert_eq!(system_text(&serde_json::json!(42)), None);
    }

    // A system prompt comfortably over MIN_STABLE_TOKENS, with one volatile date.
    fn big_system_with_date() -> String {
        format!(
            "You are a meticulous senior engineer. Today is 2026-06-27. {}",
            "Prefer small, well-tested diffs. ".repeat(400)
        )
    }

    #[test]
    fn relocate_moves_volatiles_to_tail_and_leaves_placeholders() {
        let result =
            relocate_volatile("Date 2026-06-27, id 550e8400-e29b-41d4-a716-446655440000.").unwrap();
        assert_eq!(result.fields, 2);
        assert!(!result.stable.contains("2026-06-27"), "value left prefix");
        assert!(result.stable.contains("[ctx#1]") && result.stable.contains("[ctx#2]"));
        assert!(result.tail.contains("[ctx#1] = 2026-06-27"));
        assert!(
            result
                .tail
                .contains("[ctx#2] = 550e8400-e29b-41d4-a716-446655440000")
        );
    }

    #[test]
    fn relocate_is_noop_without_volatile_fields() {
        assert!(relocate_volatile("You are a careful engineer.").is_none());
    }

    #[test]
    fn relocate_is_idempotent() {
        let once = relocate_volatile("Built at 2026-06-27 ok").unwrap();
        assert!(
            relocate_volatile(&once.stable).is_none(),
            "placeholders carry no volatile pattern, so a second pass is a no-op"
        );
    }

    #[test]
    fn relocate_is_deterministic() {
        let text = "v 2026-06-27 id 550e8400-e29b-41d4-a716-446655440000 sha \
                    da39a3ee5e6b4b0d3255bfef95601890afd80709";
        assert_eq!(relocate_volatile(text), relocate_volatile(text));
    }

    #[test]
    fn apply_rewrites_string_system_into_stable_plus_tail() {
        let mut doc = serde_json::json!({ "system": big_system_with_date(), "messages": [] });
        assert_eq!(apply_anthropic_relocate(&mut doc), 1);
        let system = &doc["system"];
        assert!(system.is_array(), "string system becomes a block array");
        assert_eq!(system[0]["cache_control"]["type"], "ephemeral");
        assert!(
            !system[0]["text"].as_str().unwrap().contains("2026-06-27"),
            "the date left the cacheable prefix"
        );
        assert!(
            system[1].get("cache_control").is_none(),
            "the tail block stays uncached"
        );
        assert!(
            system[1]["text"].as_str().unwrap().contains("2026-06-27"),
            "the date was relocated to the tail"
        );
    }

    #[test]
    fn apply_skips_small_system_and_clean_system() {
        let mut small = serde_json::json!({ "system": "Today is 2026-06-27", "messages": [] });
        assert_eq!(
            apply_anthropic_relocate(&mut small),
            0,
            "below the cacheable floor → no churn"
        );
        let mut clean =
            serde_json::json!({ "system": "You are precise. ".repeat(400), "messages": [] });
        assert_eq!(
            apply_anthropic_relocate(&mut clean),
            0,
            "no volatile fields → strict no-op"
        );
    }

    #[test]
    fn apply_skips_array_with_existing_breakpoint() {
        let mut doc = serde_json::json!({
            "system": [{
                "type": "text",
                "text": big_system_with_date(),
                "cache_control": { "type": "ephemeral" }
            }],
            "messages": []
        });
        assert_eq!(
            apply_anthropic_relocate(&mut doc),
            0,
            "a client-anchored array must be left untouched"
        );
    }

    #[test]
    fn apply_is_deterministic() {
        let mk = || serde_json::json!({ "system": big_system_with_date(), "messages": [] });
        let (mut a, mut b) = (mk(), mk());
        assert_eq!(apply_anthropic_relocate(&mut a), 1);
        assert_eq!(apply_anthropic_relocate(&mut b), 1);
        assert_eq!(a, b, "identical input → byte-identical output (#498)");
    }
}
