// SPDX-License-Identifier: Apache-2.0

//! Template collapse for the governor's R4 rule.
//!
//! [`super::lossless_collapse`] only folds byte-identical lines, so real logs,
//! lint output and exports, whose lines differ in a timestamp, a sequence
//! number or an id, never shrink. Here every line is reduced to a *template*:
//! its variable fields become a placeholder. Variable fields are maximal ASCII
//! alphanumeric words that are
//! - a number (`2026`, `750`, the `03` of `2026-03-14`), except a short
//!   enumeration suffix of a name (`worker-2`, `shard_7`: at most
//!   [`MAX_NAME_SUFFIX_DIGITS`] digits after `<letter>-`/`<letter>_`),
//! - a number with a short unit or separator letter (`750ms`, `14T10`,
//!   `411Z`, `0x1f`: starts with a digit, at most three letters),
//! - a hex id of at least [`MIN_HEX_ID`] characters with a digit
//!   (`fc56b260`), or a UUID.
//!
//! Decimals, IP addresses, durations and dates follow from these rules.
//! Words such as lint codes (`W501`, `E201`), hosts, paths and messages are
//! not variable, so two lines with different text never share a template.
//!
//! Lines sharing one template at least [`MIN_GROUP`] times are shown once,
//! where the first one was, as `[N× like] <first line> … last: <changed part
//! of the last line>`. Everything else stays verbatim and in order. This is
//! lossy for the reader; the caller keeps the full output in the archive and
//! names it. The result is a pure function of the input (#498): no clock, no
//! hash-map order reaches the output.
//!
//! Safety classes:
//! - Input with a diff hunk is refused whole: every diff line matters.
//! - Protected lines are never grouped: test-runner lines, Python traceback
//!   blocks (header, indented frames, exception line) and indented
//!   continuation lines right after a severity line.
//! - JSON records (lines opening with `{`, or a `[` line that parses as
//!   JSON) are never grouped: in an export the numbers are the data.
//! - Severity lines (error, warning, failure, panic, exception words) stay
//!   verbatim while all of them together fit in [`SEVERITY_VERBATIM_BYTES`].
//!   Past that, each distinct normalized message is kept once with its count,
//!   grouped only with lines of the same severity template, never with plain
//!   lines. A rare error therefore stays visible: its text differs from the
//!   surrounding retries, so its template has fewer than [`MIN_GROUP`]
//!   members and the line stays verbatim.

use std::collections::HashMap;

/// Fewest lines sharing a template that are collapsed. A group line costs
/// about two lines (first line plus the changed tail of the last), so eight
/// members save at least 75 % of them; smaller sets are usually distinct
/// events the reader wants to see one by one.
pub const MIN_GROUP: usize = 8;

/// All severity lines stay verbatim while their total size stays below this
/// (about 1,000 tokens); past it each distinct message is kept once.
pub const SEVERITY_VERBATIM_BYTES: usize = 4096;

/// Larger input is left alone (bounded runtime; the archive keeps 10 MB).
pub const MAX_INPUT_BYTES: usize = 8 * 1024 * 1024;

/// Longer lines are never grouped (bounded per-line work, rarely a log line).
const MAX_TEMPLATE_LINE_BYTES: usize = 4096;

/// At most this many indented lines after a severity line are protected.
const MAX_CONTINUATION: usize = 64;

/// Digits after `<letter>-` or `<letter>_` up to this length name a member
/// of a small set (`worker-2`) and stay literal.
pub const MAX_NAME_SUFFIX_DIGITS: usize = 2;

/// Shortest hex word (with a digit) treated as an id.
pub const MIN_HEX_ID: usize = 6;

/// Placeholder byte for a variable field inside a template key.
const HOLE: u8 = 0x01;

/// The collapse result: the rendered body and its counts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TemplateCollapsed {
    pub body: String,
    pub lines_before: usize,
    pub lines_after: usize,
    /// Number of `[N× like]` group lines.
    pub groups: usize,
    /// Lines folded into a group line (its first line included).
    pub grouped_lines: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Class {
    Plain,
    Severity,
    Protected,
}

fn severity_re() -> &'static regex::Regex {
    static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| {
        regex::Regex::new(concat!(
            r"(?i:\b(errors?|critical|fatal|panic(ked|king|s)?|exceptions?|fail(ed|ure|ures|ing|s)?",
            r"|warn(ing|ings)?|traceback|segfault|abort(ed)?|denied|assert(ion|ions|ed)?)\b)",
            r"|[a-z](Error|Exception)\b",
        ))
        .expect("BUG: invalid template_collapse severity regex")
    })
}

fn test_runner_re() -> &'static regex::Regex {
    static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| {
        regex::Regex::new(concat!(
            r"(?i:\btest result:|\brunning \d+ tests?\b|\b\d+ (passed|skipped|tests?)\b)",
            r"|^(PASS|FAIL|ok|not ok|OK|FAILED)\b",
        ))
        .expect("BUG: invalid template_collapse test-runner regex")
    })
}

fn is_diff_line(line: &str) -> bool {
    line.starts_with("diff --git ") || (line.starts_with("@@ -") && line[4..].contains(" @@"))
}

fn is_indented(line: &str) -> bool {
    line.starts_with([' ', '\t']) && !line.trim().is_empty()
}

/// A JSON record (owner decision 2026-10-31): its numbers are data, so an
/// export stays countable. Every line opening with `{` counts; one opening
/// with `[` only when it parses as JSON, so bracketed log prefixes such as
/// `[2026-03-14 10:00:00] INFO …` still group.
fn is_data_record(line: &str) -> bool {
    let t = line.trim();
    t.starts_with('{')
        || (t.starts_with('[')
            && t.ends_with(']')
            && serde_json::from_str::<serde::de::IgnoredAny>(t).is_ok())
}

/// Per-line class. Severity lines become protected when they all fit in the
/// verbatim budget.
fn classify(lines: &[&str]) -> Vec<Class> {
    let mut class = vec![Class::Plain; lines.len()];
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        if line.contains("Traceback (most recent call last)") {
            class[i] = Class::Protected;
            let mut j = i + 1;
            while j < lines.len() && (is_indented(lines[j]) || lines[j].trim().is_empty()) {
                class[j] = Class::Protected;
                j += 1;
            }
            if j < lines.len() {
                // The exception line closing the block.
                class[j] = Class::Protected;
                j += 1;
            }
            i = j;
            continue;
        }
        if test_runner_re().is_match(line) {
            class[i] = Class::Protected;
        } else if severity_re().is_match(line) {
            class[i] = Class::Severity;
            let mut j = i + 1;
            while j < lines.len() && j - i <= MAX_CONTINUATION && is_indented(lines[j]) {
                class[j] = Class::Protected;
                j += 1;
            }
            i = j;
            continue;
        }
        i += 1;
    }
    let severity_bytes: usize = lines
        .iter()
        .zip(&class)
        .filter(|(_, c)| **c == Class::Severity)
        .map(|(l, _)| l.len() + 1)
        .sum();
    if severity_bytes <= SEVERITY_VERBATIM_BYTES {
        for c in &mut class {
            if *c == Class::Severity {
                *c = Class::Protected;
            }
        }
    }
    class
}

fn is_uuid_at(b: &[u8]) -> bool {
    const SHAPE: [usize; 5] = [8, 4, 4, 4, 12];
    let mut pos = 0;
    for (k, len) in SHAPE.iter().enumerate() {
        if k > 0 {
            if b.get(pos) != Some(&b'-') {
                return false;
            }
            pos += 1;
        }
        let Some(part) = b.get(pos..pos + len) else {
            return false;
        };
        if !part.iter().all(u8::is_ascii_hexdigit) {
            return false;
        }
        pos += len;
    }
    !b.get(pos).is_some_and(u8::is_ascii_alphanumeric)
}

/// Whether the alphanumeric `word`, starting at `start` of `line`, is a
/// variable field (see the module docs).
fn is_variable(line: &[u8], start: usize, word: &[u8]) -> bool {
    let digits = word.iter().filter(|b| b.is_ascii_digit()).count();
    if digits == 0 {
        return false;
    }
    if digits == word.len() {
        let name_suffix = start >= 2
            && matches!(line[start - 1], b'-' | b'_')
            && line[start - 2].is_ascii_alphabetic();
        return !(name_suffix && word.len() <= MAX_NAME_SUFFIX_DIGITS);
    }
    if word[0].is_ascii_digit() && word.len() - digits <= 3 {
        return true;
    }
    word.len() >= MIN_HEX_ID && word.iter().all(u8::is_ascii_hexdigit)
}

/// The template key of `line` and the byte spans of its variable fields.
fn template(line: &str) -> (Vec<u8>, Vec<(usize, usize)>) {
    let b = line.as_bytes();
    let mut key = Vec::with_capacity(b.len());
    let mut spans = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if !b[i].is_ascii_alphanumeric() {
            key.push(b[i]);
            i += 1;
            continue;
        }
        let start = i;
        let word_start_ok = start == 0 || !b[start - 1].is_ascii_alphanumeric();
        if word_start_ok && is_uuid_at(&b[start..]) {
            i = start + 36;
            key.push(HOLE);
            spans.push((start, i));
            continue;
        }
        while i < b.len() && b[i].is_ascii_alphanumeric() {
            i += 1;
        }
        if is_variable(b, start, &b[start..i]) {
            key.push(HOLE);
            spans.push((start, i));
        } else {
            key.extend_from_slice(&b[start..i]);
        }
    }
    (key, spans)
}

struct Group {
    first: usize,
    last: usize,
    count: usize,
}

/// Collapse lines that differ only in variable fields. `None` when nothing
/// collapses, the input is too large, or it carries a diff.
#[must_use]
pub fn collapse(input: &str) -> Option<TemplateCollapsed> {
    if input.len() > MAX_INPUT_BYTES {
        return None;
    }
    let lines: Vec<&str> = input.split('\n').collect();
    if lines.iter().any(|l| is_diff_line(l)) {
        return None;
    }
    let class = classify(&lines);
    let mut index: HashMap<(bool, Vec<u8>), usize> = HashMap::new();
    let mut groups: Vec<Group> = Vec::new();
    let mut spans: Vec<Vec<(usize, usize)>> = vec![Vec::new(); lines.len()];
    let mut member: Vec<Option<usize>> = vec![None; lines.len()];
    for (i, line) in lines.iter().enumerate() {
        if class[i] == Class::Protected
            || line.trim().is_empty()
            || line.len() > MAX_TEMPLATE_LINE_BYTES
            || is_data_record(line)
        {
            continue;
        }
        let (key, line_spans) = template(line);
        if line_spans.is_empty() {
            // Without a variable field only identical lines share the
            // template: that is R2's job, and a text line stays verbatim.
            continue;
        }
        spans[i] = line_spans;
        let next = groups.len();
        let g = *index
            .entry((class[i] == Class::Severity, key))
            .or_insert(next);
        if g == next {
            groups.push(Group {
                first: i,
                last: i,
                count: 0,
            });
        }
        groups[g].last = i;
        groups[g].count += 1;
        member[i] = Some(g);
    }
    let mut out: Vec<String> = Vec::with_capacity(lines.len());
    let (mut group_lines, mut grouped) = (0usize, 0usize);
    for (i, line) in lines.iter().enumerate() {
        match member[i].map(|g| &groups[g]) {
            Some(g) if g.count >= MIN_GROUP => {
                if g.first == i {
                    out.push(group_line(&lines, &spans, g));
                    group_lines += 1;
                    grouped += g.count;
                }
            }
            _ => out.push((*line).to_string()),
        }
    }
    if group_lines == 0 {
        return None;
    }
    Some(TemplateCollapsed {
        body: out.join("\n"),
        lines_before: lines.len(),
        lines_after: out.len(),
        groups: group_lines,
        grouped_lines: grouped,
    })
}

/// `[N× like] <first> … last: <changed span of the last line>`.
fn group_line(lines: &[&str], spans: &[Vec<(usize, usize)>], g: &Group) -> String {
    fn field(line: &str, s: (usize, usize)) -> &str {
        &line[s.0..s.1]
    }
    let (first, last) = (lines[g.first], lines[g.last]);
    let (fs, ls) = (&spans[g.first], &spans[g.last]);
    let changed: Vec<usize> = (0..fs.len().min(ls.len()))
        .filter(|&k| field(first, fs[k]) != field(last, ls[k]))
        .collect();
    match (changed.first(), changed.last()) {
        (Some(&a), Some(&z)) => format!(
            "[{}× like] {first} … last: {}",
            g.count,
            &last[ls[a].0..ls[z].1]
        ),
        _ => format!("[{}× identical] {first}", g.count),
    }
}

/// The text a reader gets: one header naming the archived original, then
/// the body. `recovery` is the caller's pointer to the full output.
#[must_use]
pub fn render(c: &TemplateCollapsed, recovery: &str) -> String {
    format!(
        "[lean-ctx: {} lines shown as {}; {} lines that differ only in numbers, ids or timestamps \
         are grouped as \"[N× like] first … last: …\"; every distinct error message is kept; {recovery}]\n{}",
        c.lines_before, c.lines_after, c.grouped_lines, c.body
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn heartbeat_log(n: usize) -> String {
        (0..n).fold(String::new(), |mut s, i| {
            let ms = i * 997;
            s.push_str(&format!(
                "2026-03-14T10:{:02}:{:02}.{:03}Z INFO heartbeat ok worker-2 seq={i}\n",
                (ms / 60_000) % 60,
                (ms / 1000) % 60,
                ms % 1000
            ));
            s
        })
    }

    #[test]
    fn variable_field_table() {
        // (line, template with HOLE shown as '#')
        let cases = [
            (
                "2026-03-14T10:00:00.001Z INFO heartbeat ok worker-2 seq=0",
                "#-#-#:#:#.# INFO heartbeat ok worker-2 seq=#",
            ),
            ("delivery dlv-fc56b260 took 750ms", "delivery dlv-# took #"),
            (
                "batch b-006992 delivered 487/500",
                "batch b-# delivered #/#",
            ),
            ("from 10.0.12.7:8443", "from #.#.#.#:#"),
            ("id 550e8400-e29b-41d4-a716-446655440000 ok", "id # ok"),
            (
                "relayd/schema.py:47:101: W501 line too long (128 > 100)",
                "relayd/schema.py:#:#: W501 line too long (# > #)",
            ),
            ("shard_7 v2 E201 cafe", "shard_7 v2 E201 cafe"),
        ];
        for (line, want) in cases {
            let (key, _) = template(line);
            let shown: String = key
                .iter()
                .map(|&b| if b == HOLE { '#' } else { b as char })
                .collect();
            assert_eq!(shown, want, "{line}");
        }
    }

    #[test]
    fn heartbeat_log_collapses_by_at_least_eighty_percent() {
        let log = heartbeat_log(1500);
        let c = collapse(&log).expect("collapses");
        assert!(
            c.body.len() * 5 <= log.len(),
            "{} vs {}",
            c.body.len(),
            log.len()
        );
        assert!(c.body.starts_with(
            "[1500× like] 2026-03-14T10:00:00.000Z INFO heartbeat ok worker-2 seq=0 … last: "
        ));
        assert!(
            c.body.contains("seq=1499"),
            "last instance named: {}",
            c.body
        );
        assert_eq!((c.groups, c.grouped_lines), (1, 1500));
    }

    #[test]
    fn rare_error_among_thousands_of_retries_stays_verbatim() {
        let mut log = String::new();
        let rare = "2026-03-14T03:12:09.114Z ERROR worker-2 batch b-006992 stopped: ValueError: invalid duration '5x' in parse_duration";
        for i in 0..3000 {
            if i == 1777 {
                log.push_str(rare);
                log.push('\n');
            }
            log.push_str(&format!(
                "2026-03-14T03:{:02}:{:02}.{:03}Z WARN worker-1 delivery dlv-{:08x} to https://a.example/x failed: 503 Service Unavailable (attempt {}/5, retrying in 500ms)\n",
                (i / 60) % 60, i % 60, i % 1000, i * 7919 + 0x1000_0000, i % 5 + 1
            ));
        }
        let c = collapse(&log).expect("collapses");
        assert!(c.body.lines().any(|l| l == rare), "rare error verbatim");
        assert!(c.body.len() * 10 < log.len());
        // The retries are one severity message, kept once with its count.
        assert!(c.body.contains("[3000× like] "));
    }

    #[test]
    fn small_severity_sets_stay_verbatim_and_never_mix_with_plain_lines() {
        let mut log = heartbeat_log(200);
        for i in 0..20 {
            log.push_str(&format!(
                "2026-03-14T11:00:{i:02}.000Z ERROR worker-3 timeout after {i}s\n"
            ));
        }
        let c = collapse(&log).unwrap();
        for i in 0..20 {
            let line = format!("2026-03-14T11:00:{i:02}.000Z ERROR worker-3 timeout after {i}s");
            assert!(c.body.lines().any(|l| l == line), "{line}");
        }
    }

    #[test]
    fn tracebacks_test_output_and_diffs_stay_untouched() {
        let mut log = heartbeat_log(100);
        let tb = "Traceback (most recent call last):\n  File \"relayd/worker.py\", line 88, in run\n    d = parse_duration(v)\n  File \"relayd/config.py\", line 12, in parse_duration\n    raise ValueError(v)\nValueError: 5x";
        let tb_lines: Vec<String> = tb.lines().map(str::to_owned).collect();
        for _ in 0..10 {
            log.push_str(tb);
            log.push('\n');
        }
        for i in 0..12 {
            log.push_str(&format!("ok {i} - case_{i}\n"));
        }
        log.push_str("Ran 12 tests in 0.004s\n");
        let c = collapse(&log).unwrap();
        for l in &tb_lines {
            assert_eq!(c.body.lines().filter(|b| b == l).count(), 10, "{l}");
        }
        assert_eq!(c.body.lines().filter(|l| l.starts_with("ok ")).count(), 12);
        assert!(c.body.contains("Ran 12 tests in 0.004s"));

        let diff = format!(
            "{}diff --git a/x b/x\n@@ -1,2 +1,2 @@\n",
            heartbeat_log(100)
        );
        assert!(collapse(&diff).is_none(), "diffs are refused whole");
    }

    #[test]
    fn output_is_deterministic_and_order_preserving() {
        let mut log = String::from("start\n");
        log.push_str(&heartbeat_log(40));
        log.push_str("middle marker\n");
        log.push_str(&heartbeat_log(40));
        log.push_str("end\n");
        let a = collapse(&log).unwrap();
        assert_eq!(a, collapse(&log).unwrap());
        let body: Vec<&str> = a.body.lines().collect();
        assert_eq!(body[0], "start");
        assert!(body[1].starts_with("[80× like] "));
        assert_eq!(&body[2..], &["middle marker", "end"]);
        assert_eq!(render(&a, "full: X"), render(&a, "full: X"));
    }

    #[test]
    fn nothing_to_group_returns_none() {
        assert!(collapse("a\nb\nc").is_none());
        assert!(collapse(&heartbeat_log(MIN_GROUP - 1)).is_none());
        assert!(
            collapse(&"plain text line\n".repeat(50)).is_none(),
            "identical text is R2's"
        );
        // JSON records are data: never grouped. A bracketed log prefix is not.
        let records = |open: &str, close: &str| {
            (0..50).fold(String::new(), |mut s, i| {
                s.push_str(&format!(
                    "{open}\"id\":{i},\"status\":{}{close}\n",
                    200 + i % 3
                ));
                s
            })
        };
        assert!(collapse(&records("{", "}")).is_none());
        assert!(collapse(&records("[{", "}]")).is_none());
        let bracketed = (0..50).fold(String::new(), |mut s, i| {
            s.push_str(&format!(
                "[2026-03-14 10:00:{:02}] INFO heartbeat seq={i}\n",
                i % 60
            ));
            s
        });
        assert!(collapse(&bracketed).is_some());
    }
}
