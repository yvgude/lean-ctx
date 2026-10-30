// SPDX-License-Identifier: Apache-2.0

//! Governor rules for native Bash results (R1.bash, R2, R3 shadow).
//!
//! Claude Code's Bash result is `{stdout, stderr, interrupted, isImage}`; a
//! replacement mirrors that object and swaps only `stdout`. Anything else is
//! an unknown shape and passes through.

use super::diagnostics::has_diagnostics;
use super::{Decision, MIN_SAVED_TOKENS, Policy, RuleMode};
use crate::core::patterns::lossless_collapse;
use crate::core::tokens::count_tokens;
use crate::hook_handlers::read_dedup;

/// Below this size an R1 stub saves too little to be worth the risk.
const R1_MIN_BYTES: usize = 512;
/// R2 trigger and minimum saving (design §2 R2).
const R2_MIN_TOKENS: usize = 2000;
const R2_MIN_SAVED_TOKENS: usize = 500;
const R2_MIN_SAVED_SHARE: f64 = 0.25;
/// R3: this many consecutive non-blank lines identical to an earlier result.
const R3_WINDOW: usize = 20;
/// R3 window store cap per session; past it no new windows are added.
const R3_MAX_STORE_BYTES: u64 = 1024 * 1024;
/// Longest command echoed in a stub (deterministic truncation, #498).
const STUB_LABEL_CHARS: usize = 120;

const EPOCH_FILE: &str = "edit.epoch";
const R3_FILE: &str = "r3.win";

pub(super) fn decide(v: &serde_json::Value, policy: &Policy, session_id: &str) -> Decision {
    let args = crate::hook_handlers::payload::resolve_tool_args(v);
    let Some(cmd) = crate::hook_handlers::payload::resolve_command(v, args.as_ref()) else {
        return Decision::default();
    };
    let viewer = is_viewer_command(&cmd);
    if !viewer {
        // Anything that is not a pure viewer may have changed files.
        bump_epoch(session_id);
    }
    let Some(resp) = v.get("tool_response").and_then(|r| r.as_object()) else {
        return Decision::default();
    };
    let Some(stdout) = resp.get("stdout").and_then(|s| s.as_str()) else {
        return Decision::default();
    };
    let stderr = resp.get("stderr").and_then(|s| s.as_str()).unwrap_or("");
    let flag = |k: &str| resp.get(k).and_then(serde_json::Value::as_bool) == Some(true);
    if flag("interrupted") || flag("isImage") {
        return Decision::default();
    }
    if has_diagnostics(stdout) || has_diagnostics(stderr) {
        return Decision::default();
    }
    let agent_id = v.get("agent_id").and_then(|s| s.as_str()).unwrap_or("");
    let cwd = v.get("cwd").and_then(|s| s.as_str()).unwrap_or("");
    let tool_use_id = v.get("tool_use_id").and_then(|s| s.as_str()).unwrap_or("");

    if viewer && policy.r1_bash != RuleMode::Off {
        let key = R1Key {
            session_id,
            agent_id,
            cwd,
            cmd: &cmd,
        };
        if let Some(d) = r1(v, policy.r1_bash, &key, stdout, stderr, tool_use_id) {
            return d;
        }
    }
    let class = crate::shell::output_policy::classify(
        &cmd,
        &crate::core::config::Config::load().excluded_commands,
    );
    if class.is_protected() {
        return Decision::default();
    }
    if policy.r2 != RuleMode::Off
        && let Some(d) = r2(v, policy.r2, stdout, session_id)
    {
        return d;
    }
    if policy.r3 != RuleMode::Off {
        return r3(stdout, session_id);
    }
    Decision::default()
}

struct R1Key<'a> {
    session_id: &'a str,
    agent_id: &'a str,
    cwd: &'a str,
    cmd: &'a str,
}

fn r1(
    v: &serde_json::Value,
    mode: RuleMode,
    key: &R1Key<'_>,
    stdout: &str,
    stderr: &str,
    tool_use_id: &str,
) -> Option<Decision> {
    let dir = read_dedup::session_dir(key.session_id)?;
    let normalized = key.cmd.split_whitespace().collect::<Vec<_>>().join(" ");
    let record_key =
        blake3::hash(format!("{}\u{0}{}\u{0}{normalized}", key.agent_id, key.cwd).as_bytes())
            .to_hex()[..16]
            .to_string();
    let store = dir.join(format!("{record_key}.gvb"));
    let mut hasher = blake3::Hasher::new();
    hasher.update(stdout.as_bytes());
    hasher.update(b"\0");
    hasher.update(stderr.as_bytes());
    let out_hash = hasher.finalize().to_hex()[..32].to_string();
    let epoch = read_epoch(&dir);
    let current = format!("{out_hash}:{epoch}");

    let prev = std::fs::read_to_string(&store).ok();
    let _ = read_dedup::write_atomic(&store, format!("{current} {tool_use_id}").as_bytes());
    let (prev_state, prev_tool_use) = read_dedup::parse_record(&prev?)?;
    // Same tool_use_id = a duplicate fire of the same event, not a re-run.
    if prev_state != current
        || (!tool_use_id.is_empty() && prev_tool_use == tool_use_id)
        || !stderr.is_empty()
        || stdout.len() < R1_MIN_BYTES
    {
        return None;
    }
    let stub = render_bash_stub(key.cmd, stdout.lines().count());
    let (baseline, actual) = (count_tokens(stdout), count_tokens(&stub));
    if baseline.saturating_sub(actual) < MIN_SAVED_TOKENS {
        return None;
    }
    let output = replace_stdout(v, &stub)?;
    Some(Decision::from_rule(
        mode,
        output,
        "R1.bash",
        baseline,
        actual,
        key.session_id,
    ))
}

fn r2(v: &serde_json::Value, mode: RuleMode, stdout: &str, session_id: &str) -> Option<Decision> {
    let baseline = count_tokens(stdout);
    if baseline < R2_MIN_TOKENS {
        return None;
    }
    let collapsed = lossless_collapse::collapse(stdout)?;
    let actual = count_tokens(&collapsed.text);
    let saved = baseline.saturating_sub(actual);
    if saved < R2_MIN_SAVED_TOKENS || (saved as f64) < baseline as f64 * R2_MIN_SAVED_SHARE {
        return None;
    }
    let output = replace_stdout(v, &collapsed.text)?;
    Some(Decision::from_rule(
        mode, output, "R2", baseline, actual, session_id,
    ))
}

/// R3 (shadow only): count the lines of this output that sit in a run of at
/// least [`R3_WINDOW`] non-blank lines identical to an earlier result of this
/// session, then remember this output's windows.
fn r3(stdout: &str, session_id: &str) -> Decision {
    let lines: Vec<&str> = stdout.lines().filter(|l| !l.trim().is_empty()).collect();
    if lines.len() < R3_WINDOW {
        return Decision::default();
    }
    let Some(dir) = read_dedup::session_dir(session_id) else {
        return Decision::default();
    };
    let store = dir.join(R3_FILE);
    let known: std::collections::HashSet<String> = std::fs::read_to_string(&store)
        .unwrap_or_default()
        .lines()
        .map(str::to_owned)
        .collect();
    let windows: Vec<String> = lines
        .windows(R3_WINDOW)
        .map(|w| blake3::hash(w.join("\n").as_bytes()).to_hex()[..16].to_string())
        .collect();
    let mut covered = vec![false; lines.len()];
    for (i, w) in windows.iter().enumerate() {
        if known.contains(w) {
            covered[i..i + R3_WINDOW].iter_mut().for_each(|c| *c = true);
        }
    }
    let within_cap = !std::fs::metadata(&store).is_ok_and(|m| m.len() >= R3_MAX_STORE_BYTES);
    if within_cap {
        let fresh: Vec<&String> = windows.iter().filter(|w| !known.contains(*w)).collect();
        if !fresh.is_empty() {
            use std::io::Write;
            let mut buf = String::new();
            for w in fresh {
                buf.push_str(w);
                buf.push('\n');
            }
            // Append: concurrent appends of whole small lines are safe on
            // O_APPEND, and a lost window only costs a shadow observation.
            let _ = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&store)
                .and_then(|mut f| f.write_all(buf.as_bytes()));
        }
    }
    let repeated: Vec<&str> = lines
        .iter()
        .zip(&covered)
        .filter_map(|(l, c)| c.then_some(*l))
        .collect();
    if repeated.is_empty() {
        return Decision::default();
    }
    let would_save = count_tokens(&repeated.join("\n"));
    let baseline = count_tokens(stdout);
    // v1: R3 is log-only whatever the policy says.
    Decision::from_rule(
        RuleMode::Shadow,
        String::new(),
        "R3",
        baseline,
        baseline.saturating_sub(would_save),
        session_id,
    )
}

fn replace_stdout(v: &serde_json::Value, stdout: &str) -> Option<String> {
    let mut updated = v.get("tool_response")?.clone();
    *updated.get_mut("stdout")? = serde_json::Value::String(stdout.to_string());
    Some(read_dedup::post_tool_use_output(&updated))
}

/// Deterministic (#498) stub for an identical re-run.
fn render_bash_stub(cmd: &str, line_count: usize) -> String {
    let label: String = cmd.chars().take(STUB_LABEL_CHARS).collect();
    let ellipsis = if cmd.chars().count() > STUB_LABEL_CHARS {
        "…"
    } else {
        ""
    };
    format!(
        "{label}{ellipsis} [unchanged {line_count}L · lean-ctx governor]\nSame output as your last run of this command in this session, with no edit in between — the full output is already above. It is shown again after any edit or other command changes state."
    )
}

fn read_epoch(dir: &std::path::Path) -> String {
    std::fs::read_to_string(dir.join(EPOCH_FILE))
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|_| "0".to_string())
}

/// Invalidate every R1.bash record and R3 window of the session: an edit or
/// a command with possible side effects ran. The epoch is a fresh unique
/// token, so racing bumps can never restore an old value.
pub(super) fn bump_epoch(session_id: &str) {
    let Some(dir) = read_dedup::session_dir(session_id) else {
        return;
    };
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let token = blake3::hash(format!("{}\u{0}{nanos}", std::process::id()).as_bytes()).to_hex()
        [..16]
        .to_string();
    let _ = read_dedup::write_atomic(&dir.join(EPOCH_FILE), token.as_bytes());
    let _ = std::fs::remove_file(dir.join(R3_FILE));
}

/// A pipeline of read-only viewer commands with no redirection or command
/// substitution. Conservative: unknown means "may have side effects".
pub(super) fn is_viewer_command(cmd: &str) -> bool {
    let cleaned = cmd.replace("2>&1", "").replace("2>/dev/null", "");
    if cleaned.contains(['>', '<', '`']) || cleaned.contains("$(") || cleaned.contains("<<") {
        return false;
    }
    let segments = crate::compound_lexer::split_compound(&cleaned);
    if segments.is_empty() {
        return false;
    }
    segments.iter().all(|s| match s {
        crate::compound_lexer::Segment::Operator(op) => op == "|",
        crate::compound_lexer::Segment::Command(c) => segment_is_viewer(c),
    })
}

fn segment_is_viewer(segment: &str) -> bool {
    let tokens = crate::hook_handlers::shell_tokenize(segment.trim());
    let Some(first) = tokens.first() else {
        return false;
    };
    let program = first.rsplit('/').next().unwrap_or(first);
    let has = |flags: &[&str]| tokens.iter().any(|t| flags.contains(&t.as_str()));
    match program {
        "cat" | "nl" | "head" | "grep" | "egrep" | "fgrep" | "rg" | "ls" | "wc" | "tree"
        | "stat" | "file" | "cut" | "pwd" => true,
        "tail" => !has(&["-f", "-F", "--follow"]),
        "sed" => {
            has(&["-n"])
                && !tokens
                    .iter()
                    .any(|t| t.starts_with("-i") || t == "--in-place")
        }
        "find" => !has(&[
            "-exec", "-execdir", "-delete", "-ok", "-okdir", "-fprint", "-fprintf", "-fls",
        ]),
        "git" => matches!(
            tokens.get(1).map(String::as_str),
            Some("status" | "diff" | "log" | "show" | "blame" | "ls-files")
        ),
        _ => false,
    }
}
