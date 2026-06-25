//! Offline trace mining: distill recurring error signatures from a directory of
//! `.jsonl` transcript/log files (e.g. agent transcripts, CI logs).
//!
//! The live loop ([`record_shell_outcome`](super::record_shell_outcome)) learns
//! from commands run *through* lean-ctx. This module bootstraps that loop from
//! history: it scans past transcripts for the same unambiguous error markers the
//! shell detector recognizes (`error[E####]`, `error TS####`, `Traceback`,
//! `npm ERR!`, `panicked at`, …) and ranks the signatures that recur across the
//! most sessions — the project's persistent pain points.
//!
//! Deterministic and high-precision by design: only structured error markers
//! match (never free prose), each file counts as one "session", and the output
//! is read-only — it surfaces signatures for review, it never mutates state.

use std::collections::BTreeMap;
use std::path::Path;

use regex::Regex;

use super::normalize_error_signature;

/// A recurring error signature distilled from mined transcripts.
#[derive(Debug, Clone, PartialEq)]
pub struct MinedSignature {
    pub signature: String,
    /// Total matches across all files.
    pub occurrences: usize,
    /// Distinct files (sessions) the signature appeared in.
    pub sessions: usize,
}

/// High-precision, command-agnostic error markers. Anchored to tokens that do
/// not occur in ordinary prose, so matching transcript text never false-positives
/// on a discussion *about* errors.
fn signature_patterns() -> Vec<Regex> {
    [
        r"error\[E\d{4}\][^\n]*",         // Rust / rustc
        r"error TS\d{4}[^\n]*",           // TypeScript / tsc
        r"panicked at [^\n]*",            // Rust panic
        r"npm ERR![^\n]*",                // npm
        r"ModuleNotFoundError[^\n]*",     // Python
        r"ImportError: [^\n]*",           // Python
        r"undefined: [A-Za-z_][\w]*",     // Go
        r"undefined reference to [^\n]*", // C/C++ linker
    ]
    .iter()
    .filter_map(|p| Regex::new(p).ok())
    .collect()
}

/// Extract normalized error signatures from a single blob of text. Pure and
/// deterministic; returns one entry per match (callers aggregate counts).
#[must_use]
pub fn extract_error_signatures(text: &str) -> Vec<String> {
    extract_with(text, &signature_patterns())
}

fn extract_with(text: &str, patterns: &[Regex]) -> Vec<String> {
    let mut out = Vec::new();
    for re in patterns {
        for m in re.find_iter(text) {
            let sig = normalize_error_signature(m.as_str());
            if !sig.is_empty() {
                out.push(sig);
            }
        }
    }
    out
}

/// Collect every string value in a parsed JSON line, so mining works regardless
/// of the transcript's exact schema (message text lives under different keys
/// across tools). Deterministic depth-first traversal.
fn collect_strings(value: &serde_json::Value, out: &mut String) {
    match value {
        serde_json::Value::String(s) => {
            out.push_str(s);
            out.push('\n');
        }
        serde_json::Value::Array(arr) => {
            for v in arr {
                collect_strings(v, out);
            }
        }
        serde_json::Value::Object(map) => {
            for v in map.values() {
                collect_strings(v, out);
            }
        }
        _ => {}
    }
}

/// Mine all `.jsonl` files in `dir` for recurring error signatures. Each file is
/// treated as one session; signatures are ranked by session reach, then total
/// occurrences, then lexically — fully deterministic.
#[must_use]
pub fn mine_jsonl_dir(dir: &Path) -> Vec<MinedSignature> {
    let patterns = signature_patterns();

    // Stable file order so traversal is deterministic regardless of FS ordering.
    let mut files: Vec<std::path::PathBuf> = match std::fs::read_dir(dir) {
        Ok(rd) => rd
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|e| e == "jsonl"))
            .collect(),
        Err(_) => return Vec::new(),
    };
    files.sort();

    // signature -> (total occurrences, set of file indices it appeared in)
    let mut occ: BTreeMap<String, usize> = BTreeMap::new();
    let mut sess: BTreeMap<String, usize> = BTreeMap::new();

    for path in &files {
        let Ok(content) = std::fs::read_to_string(path) else {
            continue;
        };
        let mut seen_in_file: std::collections::BTreeSet<String> =
            std::collections::BTreeSet::new();
        for line in content.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            // Pull text out of structured JSON when possible; fall back to the
            // raw line so plain-text logs still mine.
            let text = match serde_json::from_str::<serde_json::Value>(line) {
                Ok(v) => {
                    let mut buf = String::new();
                    collect_strings(&v, &mut buf);
                    buf
                }
                Err(_) => line.to_string(),
            };
            for sig in extract_with(&text, &patterns) {
                *occ.entry(sig.clone()).or_insert(0) += 1;
                seen_in_file.insert(sig);
            }
        }
        for sig in seen_in_file {
            *sess.entry(sig).or_insert(0) += 1;
        }
    }

    let mut result: Vec<MinedSignature> = occ
        .into_iter()
        .map(|(signature, occurrences)| {
            let sessions = sess.get(&signature).copied().unwrap_or(0);
            MinedSignature {
                signature,
                occurrences,
                sessions,
            }
        })
        .collect();

    result.sort_by(|a, b| {
        b.sessions
            .cmp(&a.sessions)
            .then_with(|| b.occurrences.cmp(&a.occurrences))
            .then_with(|| a.signature.cmp(&b.signature))
    });
    result
}

/// Render a mining report. `min_sessions` hides one-off signatures so only
/// genuinely recurring pain points show.
#[must_use]
pub fn format_mining_report(mined: &[MinedSignature], min_sessions: usize) -> String {
    let recurring: Vec<&MinedSignature> = mined
        .iter()
        .filter(|m| m.sessions >= min_sessions)
        .collect();

    if recurring.is_empty() {
        return "No recurring error signatures found in the mined transcripts.".to_string();
    }

    let mut out = format!(
        "=== Recurring errors across sessions ({} signature(s)) ===\n",
        recurring.len()
    );
    for m in recurring {
        out.push_str(&format!(
            "  [{} sessions, {}x] {}\n",
            m.sessions, m.occurrences, m.signature
        ));
    }
    out.push_str("\nThese recur across past sessions — capture fixes with ctx_knowledge or address the root cause.\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_known_error_markers() {
        let text = "thread 'main' panicked at src/x.rs:1:1\n\
                    error[E0382]: borrow of moved value: `x`\n\
                    src/a.ts:3:1 - error TS2304: Cannot find name 'foo'.\n\
                    ModuleNotFoundError: No module named 'flask'";
        let sigs = extract_error_signatures(text);
        assert!(sigs.iter().any(|s| s.contains("E0382")));
        assert!(sigs.iter().any(|s| s.contains("TS2304")));
        assert!(sigs.iter().any(|s| s.contains("panicked at")));
        assert!(sigs.iter().any(|s| s.contains("ModuleNotFoundError")));
    }

    #[test]
    fn ignores_prose_without_markers() {
        let text = "We discussed the error handling strategy and how to fix the failed build.";
        assert!(
            extract_error_signatures(text).is_empty(),
            "discussion about errors must not match"
        );
    }

    #[test]
    fn mines_recurring_signature_across_files() {
        let dir = tempfile::tempdir().expect("tempdir");
        // Two separate "sessions" both hitting the same compile error.
        std::fs::write(
            dir.path().join("s1.jsonl"),
            "{\"role\":\"assistant\",\"content\":\"error[E0382]: borrow of moved value: x\"}\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("s2.jsonl"),
            "{\"role\":\"user\",\"text\":\"error[E0382]: borrow of moved value: x\"}\n",
        )
        .unwrap();
        // A non-jsonl file must be ignored.
        std::fs::write(dir.path().join("notes.txt"), "error[E9999]: ignore me").unwrap();

        let mined = mine_jsonl_dir(dir.path());
        let top = mined
            .iter()
            .find(|m| m.signature.contains("E0382"))
            .expect("E0382 mined");
        assert_eq!(top.sessions, 2, "seen across both transcript files");
        assert_eq!(top.occurrences, 2);
        assert!(
            !mined.iter().any(|m| m.signature.contains("E9999")),
            "non-.jsonl files are not mined"
        );

        let report = format_mining_report(&mined, 2);
        assert!(report.contains("E0382"));
        assert!(report.contains("2 sessions"));
    }

    #[test]
    fn report_hides_one_off_signatures() {
        let mined = vec![MinedSignature {
            signature: "error[E0001]: once".to_string(),
            occurrences: 1,
            sessions: 1,
        }];
        let report = format_mining_report(&mined, 2);
        assert!(report.contains("No recurring error signatures"));
    }

    #[test]
    fn mining_is_deterministic() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join("a.jsonl"),
            "{\"content\":\"error[E0382]: moved\"}\n{\"content\":\"error TS2304: missing\"}\n",
        )
        .unwrap();
        assert_eq!(mine_jsonl_dir(dir.path()), mine_jsonl_dir(dir.path()));
    }
}
