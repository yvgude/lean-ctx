// SPDX-License-Identifier: Apache-2.0

//! Code Mode: shape what the model reads, never what a script reads.
//!
//! In Claude Code's Code Mode the model writes a script for the `REPL` tool;
//! the script calls Bash, Read and the other tools itself (`tool_use_id`
//! `repl_…`) and only the script's result reaches the model. A PostToolUse
//! replacement of an inner result is what the *script* receives, so a
//! collapsed Bash output or a file outline would corrupt any script that
//! parses, counts or searches it. Hence:
//!
//! - **Inner call** ([`inner`]): passes through unchanged and leaves no R1/R3
//!   record (the model has not seen it). R2, R4 (Bash) and R5 (Read, `cat`)
//!   still decide; an applied view is remembered next to the native text,
//!   pending for the eval that made the call. An inner read that gets no view
//!   (R5's escape: the file was outlined before) drops a pending view of the
//!   same text, so the eval hands it back in full.
//! - **REPL result** ([`shape`]): wherever the result carries a pending
//!   native text verbatim — plain, as Read's numbered lines, JSON-escaped, or
//!   inside a chain of JS string literals as `util.inspect` prints it — that
//!   text is replaced by its view. Anything the script cut, filtered or
//!   reformatted does not match and stays as it is. A finished eval consumes
//!   every pending view, applied or not: a view never outlives the eval that
//!   made it, so a later read of the same file (R5's escape) or an equal
//!   computed value reaches the model as it is.
//!
//! The replacement goes through `updatedToolOutput` of the REPL's own
//! PostToolUse, which the host delivers in place of the eval result inside
//! its event envelope. An async eval (the REPL reports only "(dispatched #N)")
//! settles through the `Poll` tool; [`shape_poll`] shapes the `<result>` and
//! `<log>` segments of that envelope the same way. The dispatch keeps the
//! eval's script until its result arrives, so the Poll knows which files the
//! script named ([`first_reads`]).

use super::bash::{self, Scope};
use super::{Decision, MIN_SAVED_TOKENS, Policy, RuleMode, outline, read_dedup};
use lean_ctx::core::tokens::count_tokens;

/// The Code Mode tool whose result the model reads.
pub(super) const REPL_TOOL: &str = "REPL";
/// The tool that delivers an async REPL eval's settled result.
pub(super) const POLL_TOOL: &str = "Poll";
/// `tool_use_id` prefix the host gives calls made from a REPL script.
const INNER_ID_PREFIX: &str = "repl_";
/// Session subdirectory of the pending views and dispatched scripts.
const VIEW_DIR: &str = "repl";
/// Extension of a pending view.
const VIEW_EXT: &str = "json";
/// Extension of a dispatched eval's script (`eval-<N>.code`).
const CODE_EXT: &str = "code";
/// Larger native texts are not remembered (bounded disk and matching time).
const MAX_NATIVE_BYTES: usize = 2 * 1024 * 1024;
/// Pending views (and dispatched scripts) per session; the oldest go first.
const MAX_VIEWS: usize = 64;

pub(super) fn is_inner_call(v: &serde_json::Value) -> bool {
    v.get("tool_use_id")
        .and_then(|s| s.as_str())
        .is_some_and(|id| id.starts_with(INNER_ID_PREFIX))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
enum Kind {
    Bash,
    Read,
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct View {
    kind: Kind,
    native: String,
    view: String,
}

/// A tool call from a REPL script: always passes through.
pub(super) fn inner(
    v: &serde_json::Value,
    tool: Option<&str>,
    policy: &Policy,
    session_id: &str,
) -> Decision {
    match tool {
        Some("Read") => {
            let view = outline::decide(v, policy, session_id).and_then(|d| d.view);
            let resp = v.get("tool_response");
            let native = resp.and_then(|r| {
                let slot = read_dedup::locate_content(r)?;
                read_dedup::slot_text(r, &slot)
            });
            if let Some(native) = native {
                remember(session_id, Kind::Read, native, view);
            }
        }
        Some("Bash") => {
            let decision = bash::decide(v, policy, session_id, Scope::Script);
            let stdout = v
                .get("tool_response")
                .and_then(|r| r.get("stdout"))
                .and_then(|s| s.as_str());
            if let Some(native) = stdout {
                remember(session_id, Kind::Bash, native, decision.view);
            }
        }
        Some("Edit" | "Write" | "MultiEdit" | "NotebookEdit") => bash::bump_epoch(session_id),
        _ => {}
    }
    Decision::default()
}

/// Keep `view` pending for `native`; without a view (an inner read R5 let
/// through as its escape), drop a pending view of the same text.
fn remember(session_id: &str, kind: Kind, native: &str, view: Option<String>) {
    if native.len() > MAX_NATIVE_BYTES || native.trim().is_empty() {
        return;
    }
    let Some(dir) = view_dir(session_id) else {
        return;
    };
    let name = format!(
        "{}.{VIEW_EXT}",
        &blake3::hash(native.as_bytes()).to_hex()[..16]
    );
    let Some(view) = view else {
        let _ = std::fs::remove_file(dir.join(name));
        return;
    };
    let record = View {
        kind,
        native: native.to_string(),
        view,
    };
    let Ok(bytes) = serde_json::to_vec(&record) else {
        return;
    };
    let _ = read_dedup::write_atomic(&dir.join(name), &bytes);
    prune(&dir, VIEW_EXT);
}

fn view_dir(session_id: &str) -> Option<std::path::PathBuf> {
    let dir = read_dedup::session_dir(session_id)?.join(VIEW_DIR);
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir)
}

fn prune(dir: &std::path::Path, ext: &str) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut files: Vec<(std::time::SystemTime, std::path::PathBuf)> = entries
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == ext))
        .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
        .collect();
    if files.len() <= MAX_VIEWS {
        return;
    }
    files.sort();
    for (_, path) in &files[..files.len() - MAX_VIEWS] {
        let _ = std::fs::remove_file(path);
    }
}

/// Pending views in name order, so the result is deterministic (#498);
/// `consume` deletes them: the eval they belong to has finished.
fn pending_views(session_id: &str, consume: bool) -> Vec<View> {
    let Some(dir) = view_dir(session_id) else {
        return Vec::new();
    };
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut paths: Vec<std::path::PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == VIEW_EXT))
        .collect();
    paths.sort();
    paths
        .iter()
        .filter_map(|p| {
            let bytes = std::fs::read(p).ok();
            if consume {
                let _ = std::fs::remove_file(p);
            }
            serde_json::from_slice(&bytes?).ok()
        })
        .collect()
}

/// The eval number of a REPL result that only reports its dispatch
/// (`asyncDispatched` with `evalId`, or the text "(dispatched #N)").
fn dispatched_eval(resp: &serde_json::Map<String, serde_json::Value>) -> Option<u64> {
    if resp
        .get("asyncDispatched")
        .and_then(serde_json::Value::as_bool)
        == Some(true)
        && let Some(id) = resp.get("evalId").and_then(serde_json::Value::as_u64)
    {
        return Some(id);
    }
    resp.get("result")?
        .as_str()?
        .trim()
        .strip_prefix("(dispatched #")?
        .strip_suffix(')')?
        .parse()
        .ok()
}

fn eval_code_path(dir: &std::path::Path, eval: u64) -> std::path::PathBuf {
    dir.join(format!("eval-{eval}.{CODE_EXT}"))
}

/// The eval numbers of the events in a Poll envelope (`<event … eval="N">`),
/// sorted. Body text is XML-escaped, so every `<event` is a real tag.
fn envelope_evals(content: &str) -> Vec<u64> {
    let mut evals: Vec<u64> = content
        .match_indices("<event ")
        .filter_map(|(at, _)| {
            let tag = &content[at..at + content[at..].find('>')?];
            let value = &tag[tag.find(" eval=\"")? + 7..];
            value[..value.find('"')?].parse().ok()
        })
        .collect();
    evals.sort_unstable();
    evals.dedup();
    evals
}

/// The scripts of the envelope's dispatched evals, joined; `consume`
/// deletes them.
fn dispatched_code(session_id: &str, evals: &[u64], consume: bool) -> String {
    let Some(dir) = view_dir(session_id) else {
        return String::new();
    };
    let mut code = String::new();
    for eval in evals {
        let path = eval_code_path(&dir, *eval);
        if let Ok(text) = std::fs::read_to_string(&path) {
            code.push_str(&text);
            code.push('\n');
        }
        if consume {
            let _ = std::fs::remove_file(&path);
        }
    }
    code
}

/// PostToolUse of the REPL tool: a dispatch keeps the eval's script for its
/// Poll; a finished eval consumes the pending views and replays them into
/// its result.
pub(super) fn shape(v: &serde_json::Value, policy: &Policy, session_id: &str) -> Decision {
    let Some(resp) = v.get("tool_response").and_then(|r| r.as_object()) else {
        return Decision::default();
    };
    let code = super::super::payload::resolve_tool_args(v)
        .and_then(|a| a.get("code").and_then(|c| c.as_str()).map(str::to_owned))
        .unwrap_or_default();
    if let Some(eval) = dispatched_eval(resp) {
        if let Some(dir) = view_dir(session_id) {
            let _ = read_dedup::write_atomic(&eval_code_path(&dir, eval), code.as_bytes());
            prune(&dir, CODE_EXT);
        }
        return Decision::default();
    }
    let mut views = pending_views(session_id, true);
    if [policy.r2, policy.r4, policy.r5]
        .iter()
        .all(|m| *m != RuleMode::On)
    {
        return Decision::default();
    }
    let fields = ["result", "stdout"];
    if !fields.iter().any(|f| {
        resp.get(*f)
            .and_then(|s| s.as_str())
            .is_some_and(|s| !s.is_empty())
    }) {
        return Decision::default();
    }
    let texts: Vec<&str> = fields
        .iter()
        .filter_map(|f| resp.get(*f).and_then(|s| s.as_str()))
        .collect();
    views.extend(first_reads(v, &code, &texts, &views, policy, session_id));
    if views.is_empty() {
        return Decision::default();
    }
    let subs = Substitutions::new(&views);
    let mut updated = serde_json::Value::Object(resp.clone());
    let (mut baseline, mut actual) = (0, 0);
    for field in fields {
        let Some(text) = resp.get(field).and_then(|s| s.as_str()) else {
            continue;
        };
        let shaped = subs.apply(text);
        baseline += count_tokens(text);
        actual += count_tokens(&shaped);
        if shaped != text {
            updated[field] = serde_json::Value::String(shaped);
        }
    }
    if baseline.saturating_sub(actual) < MIN_SAVED_TOKENS {
        return Decision::default();
    }
    let output = read_dedup::post_tool_use_output(&updated);
    Decision::from_rule(RuleMode::On, output, "REPL", baseline, actual, session_id)
}

/// PostToolUse of the Poll tool: an async REPL eval (the REPL itself only
/// reports "(dispatched #N)") settles into an event envelope whose
/// `<result>…</result>` and `<log>…</log>` segments carry the eval result and
/// its console output, XML-escaped (`&`, `<`, `>`). Each segment is
/// unescaped, shaped like a REPL result and re-escaped; a segment that does
/// not survive the escape round trip unchanged is left alone. Paths for
/// [`first_reads`] come from the scripts the dispatches kept. An envelope
/// with a `<result>` finishes its evals: it consumes the pending views and
/// those scripts.
pub(super) fn shape_poll(v: &serde_json::Value, policy: &Policy, session_id: &str) -> Decision {
    let Some(resp) = v.get("tool_response").and_then(|r| r.as_object()) else {
        return Decision::default();
    };
    let Some(content) = resp.get("content").and_then(|s| s.as_str()) else {
        return Decision::default();
    };
    if !POLL_SEGMENTS.iter().any(|(open, _)| content.contains(open)) {
        return Decision::default();
    }
    let finished = content.contains("<result");
    let mut views = pending_views(session_id, finished);
    let code = dispatched_code(session_id, &envelope_evals(content), finished);
    if [policy.r2, policy.r4, policy.r5]
        .iter()
        .all(|m| *m != RuleMode::On)
    {
        return Decision::default();
    }
    let unescaped = xml_unescape(content);
    views.extend(first_reads(
        v,
        &code,
        &[&unescaped],
        &views,
        policy,
        session_id,
    ));
    if views.is_empty() {
        return Decision::default();
    }
    let subs = Substitutions::new(&views);
    let (mut baseline, mut actual) = (0, 0);
    let mut out = content.to_string();
    for (open, close) in POLL_SEGMENTS {
        out = shape_segments(&out, open, close, &subs, &mut baseline, &mut actual);
    }
    if baseline.saturating_sub(actual) < MIN_SAVED_TOKENS {
        return Decision::default();
    }
    let mut updated = serde_json::Value::Object(resp.clone());
    updated["content"] = serde_json::Value::String(out);
    let output = read_dedup::post_tool_use_output(&updated);
    Decision::from_rule(RuleMode::On, output, "REPL", baseline, actual, session_id)
}

/// Shape every `open…close` segment of `content`; token counts of the
/// changed segments accumulate into `baseline` and `actual`.
fn shape_segments(
    content: &str,
    open_tag: &str,
    close_tag: &str,
    subs: &Substitutions,
    baseline: &mut usize,
    actual: &mut usize,
) -> String {
    let mut out = String::with_capacity(content.len());
    let mut rest = content;
    while let Some(open) = rest.find(open_tag) {
        let body_start = open + open_tag.len();
        let Some(len) = rest[body_start..].find(close_tag) else {
            break;
        };
        out.push_str(&rest[..body_start]);
        let seg = &rest[body_start..body_start + len];
        let text = xml_unescape(seg);
        let shaped = if xml_escape(&text) == seg {
            subs.apply(&text)
        } else {
            text.clone()
        };
        if shaped == text {
            out.push_str(seg);
        } else {
            *baseline += count_tokens(seg);
            let escaped = xml_escape(&shaped);
            *actual += count_tokens(&escaped);
            out.push_str(&escaped);
        }
        rest = &rest[body_start + len..];
    }
    out.push_str(rest);
    out
}

/// Script-read files the result carries whole, at most this many per eval.
const MAX_FIRST_READS: usize = 16;
/// Path-like tokens of a script tried per eval (each costs a stat and a read).
const MAX_PATH_TOKENS: usize = 64;

/// R5 for reads no hook sees: a script reads a file through a REPL builtin
/// (`file(p).text()`, `` $`cat p` ``) without an inner Read, so nothing was
/// remembered. Every path the script names that resolves to a file inside
/// the session's project and whose whole content the result carries
/// verbatim gets R5's outline as its view — the same rule, record and escape
/// as a native Read (a second whole read of the file passes in full). Paths
/// come from the script text (`code`) only; a file not named literally stays
/// as it is.
fn first_reads(
    v: &serde_json::Value,
    code: &str,
    texts: &[&str],
    known: &[View],
    policy: &Policy,
    session_id: &str,
) -> Vec<View> {
    if policy.r5 != RuleMode::On || code.trim().is_empty() {
        return Vec::new();
    }
    let Some(root) = v
        .get("cwd")
        .and_then(|c| c.as_str())
        .filter(|c| !c.trim().is_empty())
        .and_then(|c| std::fs::canonicalize(c).ok())
    else {
        return Vec::new();
    };
    let agent_id = v.get("agent_id").and_then(|s| s.as_str()).unwrap_or("");
    let tool_use_id = v.get("tool_use_id").and_then(|s| s.as_str()).unwrap_or("");
    let mut out = Vec::new();
    for name in path_tokens(code).into_iter().take(MAX_PATH_TOKENS) {
        if out.len() >= MAX_FIRST_READS {
            break;
        }
        let Some((path, content)) = project_file(&root, &name) else {
            continue;
        };
        let native = content.trim_end_matches('\n');
        if native.is_empty()
            || known
                .iter()
                .any(|k| k.native.trim_end_matches('\n') == native)
        {
            continue;
        }
        // Probe first: the record is written only for a file the model reads.
        let probe = View {
            kind: Kind::Read,
            native: content.clone(),
            view: String::from("\u{0}"),
        };
        let subs = Substitutions::new(std::slice::from_ref(&probe));
        if texts.iter().all(|t| subs.apply(t) == *t) {
            continue;
        }
        let total = content.lines().count();
        let shown = path.to_string_lossy();
        if let Some((view, _, _)) =
            super::outline::first_view(&shown, &content, total, agent_id, tool_use_id, session_id)
        {
            out.push(View {
                kind: Kind::Read,
                native: content,
                view,
            });
        }
    }
    out
}

/// Path-like tokens of a script, deduplicated in sorted order (#498): runs of
/// path characters with a file extension, as string literals and shell
/// commands spell them.
fn path_tokens(code: &str) -> Vec<String> {
    let is_path_char =
        |c: char| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | '/' | '@' | '+' | '~');
    let mut tokens: Vec<String> = code
        .split(|c: char| !is_path_char(c))
        .filter(|t| {
            std::path::Path::new(t)
                .extension()
                .is_some_and(|e| !e.is_empty() && e.len() <= 10)
        })
        .map(str::to_owned)
        .collect();
    tokens.sort();
    tokens.dedup();
    tokens
}

/// `name` resolved against the project root: an existing regular file whose
/// canonical path stays inside `root`, read as UTF-8 within the size bound.
pub(super) fn project_file(
    root: &std::path::Path,
    name: &str,
) -> Option<(std::path::PathBuf, String)> {
    let joined = if std::path::Path::new(name).is_absolute() {
        std::path::PathBuf::from(name)
    } else {
        root.join(name)
    };
    let path = std::fs::canonicalize(joined).ok()?;
    if !path.starts_with(root) {
        return None;
    }
    let meta = std::fs::metadata(&path).ok()?;
    if !meta.is_file() || meta.len() as usize > MAX_NATIVE_BYTES {
        return None;
    }
    let content = std::fs::read_to_string(&path).ok()?;
    Some((path, content))
}

/// Poll envelope segments that carry eval output: the result and the
/// console log (`self.log`, `console.log`).
const POLL_SEGMENTS: [(&str, &str); 2] = [("<result>", "</result>"), ("<log>", "</log>")];

/// The host's envelope escaping: `&`, `<`, `>` only.
fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn xml_unescape(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}

/// (native, view) pairs in every spelling a script result can carry them.
struct Substitutions {
    /// Spellings that appear as decoded text: plain and numbered.
    plain: Vec<(String, String)>,
    /// Spellings inside a JSON string (`JSON.stringify` output).
    json: Vec<(String, String)>,
}

impl Substitutions {
    fn new(views: &[View]) -> Self {
        let mut plain = Vec::new();
        let mut json = Vec::new();
        for v in views {
            let native = v.native.trim_end_matches('\n');
            let view = v.view.trim_end_matches('\n');
            if native.is_empty() || native == view {
                continue;
            }
            let mut spellings = vec![native.to_string()];
            if v.kind == Kind::Read {
                spellings.push(numbered(native));
            }
            for s in spellings {
                if let (Some(js), Some(jv)) = (json_inner(&s), json_inner(view))
                    && js != s
                {
                    json.push((js, jv));
                }
                plain.push((s, view.to_string()));
            }
        }
        Self { plain, json }
    }

    fn apply(&self, text: &str) -> String {
        let mut out = text.to_string();
        for (native, view) in self.plain.iter().chain(&self.json) {
            if out.contains(native.as_str()) {
                out = out.replace(native.as_str(), view);
            }
        }
        rewrite_literal_chains(&out, |decoded| {
            let mut hit = false;
            let mut d = decoded.to_string();
            for (native, view) in &self.plain {
                if d.contains(native.as_str()) {
                    d = d.replace(native.as_str(), view);
                    hit = true;
                }
            }
            hit.then_some(d)
        })
    }
}

/// Read's text as the host hands it to a script: `cat -n` numbering.
fn numbered(content: &str) -> String {
    content
        .lines()
        .enumerate()
        .map(|(i, l)| format!("{:>6}\t{l}", i + 1))
        .collect::<Vec<_>>()
        .join("\n")
}

/// `s` as it reads inside a JSON string literal (without the quotes).
fn json_inner(s: &str) -> Option<String> {
    let quoted = serde_json::to_string(s).ok()?;
    Some(quoted[1..quoted.len() - 1].to_string())
}

/// Find every chain of JS string literals joined by `+` (the multi-line
/// string form of `util.inspect`), decode it, and let `f` rewrite the decoded
/// text; a rewritten chain is re-encoded in the same style. Text that is not
/// a well-formed chain is copied unchanged.
fn rewrite_literal_chains(text: &str, f: impl Fn(&str) -> Option<String>) -> String {
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut copied = 0;
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        if !matches!(c, b'\'' | b'"' | b'`') || !literal_may_start(bytes, i) {
            i += 1;
            continue;
        }
        let Some(chain) = parse_chain(text, i) else {
            i += 1;
            continue;
        };
        if let Some(rewritten) = f(&chain.decoded) {
            out.push_str(&text[copied..i]);
            out.push_str(&encode_chain(&rewritten, &chain.joiner));
            copied = chain.end;
        }
        i = chain.end;
    }
    out.push_str(&text[copied..]);
    out
}

/// A literal starts a value: at the start, after whitespace or after one of
/// `:[(,{=`.
fn literal_may_start(bytes: &[u8], i: usize) -> bool {
    i == 0
        || matches!(
            bytes[i - 1],
            b' ' | b'\n' | b'\t' | b':' | b'[' | b'(' | b',' | b'{' | b'='
        )
}

pub(super) struct Chain {
    pub(super) decoded: String,
    end: usize,
    /// The text between `+` and the next literal (`"\n    "` by default).
    joiner: String,
}

pub(super) fn parse_chain(text: &str, start: usize) -> Option<Chain> {
    let bytes = text.as_bytes();
    let (mut decoded, mut end) = parse_literal(text, start)?;
    let mut joiner = None;
    loop {
        let mut j = end;
        while j < bytes.len() && matches!(bytes[j], b' ' | b'\n' | b'\t') {
            j += 1;
        }
        if bytes.get(j) != Some(&b'+') {
            break;
        }
        let mut k = j + 1;
        while k < bytes.len() && matches!(bytes[k], b' ' | b'\n' | b'\t') {
            k += 1;
        }
        let Some((next, next_end)) = parse_literal(text, k) else {
            break;
        };
        joiner.get_or_insert_with(|| text[j + 1..k].to_string());
        decoded.push_str(&next);
        end = next_end;
    }
    // A literal must end a value, or it was prose with a stray quote.
    let after = text[end..].trim_start_matches([' ', '\t']);
    if !(after.is_empty() || after.starts_with([',', '}', ']', ')', '\n', ';'])) {
        return None;
    }
    Some(Chain {
        decoded,
        end,
        joiner: joiner.unwrap_or_else(|| "\n    ".to_string()),
    })
}

/// One JS string literal at `start`: its decoded value and the byte after it.
fn parse_literal(text: &str, start: usize) -> Option<(String, usize)> {
    let mut chars = text[start..].char_indices();
    let (_, quote) = chars.next()?;
    if !matches!(quote, '\'' | '"' | '`') {
        return None;
    }
    let mut out = String::new();
    while let Some((at, c)) = chars.next() {
        match c {
            _ if c == quote => return Some((out, start + at + c.len_utf8())),
            '\n' if quote != '`' => return None,
            '$' if quote == '`' && text[start + at..].starts_with("${") => return None,
            '\\' => {
                let (_, e) = chars.next()?;
                match e {
                    'n' => out.push('\n'),
                    't' => out.push('\t'),
                    'r' => out.push('\r'),
                    'b' => out.push('\u{8}'),
                    'f' => out.push('\u{c}'),
                    'v' => out.push('\u{b}'),
                    '0' => out.push('\0'),
                    'x' => {
                        let h: String = (0..2)
                            .filter_map(|_| chars.next().map(|(_, c)| c))
                            .collect();
                        out.push(char::from_u32(u32::from_str_radix(&h, 16).ok()?)?);
                    }
                    'u' => {
                        let h: String = (0..4)
                            .filter_map(|_| chars.next().map(|(_, c)| c))
                            .collect();
                        out.push(char::from_u32(u32::from_str_radix(&h, 16).ok()?)?);
                    }
                    other => out.push(other),
                }
            }
            _ => out.push(c),
        }
    }
    None
}

/// `s` as single-quoted literals, one per line, joined by `+` and `joiner`.
fn encode_chain(s: &str, joiner: &str) -> String {
    let parts: Vec<String> = s.split_inclusive('\n').map(encode_literal).collect();
    if parts.is_empty() {
        return "''".to_string();
    }
    parts.join(&format!(" +{joiner}"))
}

fn encode_literal(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for c in s.chars() {
        match c {
            '\'' => out.push_str("\\'"),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\x{:02X}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('\'');
    out
}
