//! Read a file from disk and render it in the requested mode.
//!
//! # Invariants
//!
//! - [`read`] is **pure**: its output is a deterministic function of
//!   (file content on disk, mode, crp_mode, task). No side effects, no cache.
//! - [`ReadMode`] is validated at parse time (in `registered/ctx_read.rs`).
//!   Once constructed, all variants are valid and need no re-checking.
//! - Line ranges only apply to [`ReadMode::Full`]. The enum and schema prevent
//!   passing a range to Signatures/Map/Diff.

use std::path::Path;

use crate::core::deps;
use crate::core::protocol;
use crate::core::signatures;
use crate::core::tokens::count_tokens;
use crate::tools::CrpMode;

pub(crate) mod render;
pub(crate) use render::*;
#[cfg(test)]
mod tests;

// ── Mode label constants ──
pub const MODE_FULL: &str = "full";
pub const MODE_SIGNATURES: &str = "signatures";
pub const MODE_MAP: &str = "map";
pub const MODE_DIFF: &str = "diff";

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// 1-based inclusive line range.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LineRange {
    pub start: usize,
    pub end: usize,
}

impl LineRange {
    /// Panics if `start < 1` or `end < start`.
    pub fn new(start: usize, end: usize) -> Self {
        assert!(start >= 1, "LineRange::start must be ≥ 1, got {start}");
        assert!(
            end >= start,
            "LineRange::end ({end}) must be ≥ start ({start})"
        );
        Self { start, end }
    }
}

/// Read mode — validated at parse time in the MCP handler.
///
/// `Full(r)`: `r` is an optional line range.
/// All other modes ignore line ranges.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReadMode {
    Full(Option<LineRange>),
    Signatures,
    Map,
    Diff,
}

impl ReadMode {
    pub fn supports_range(&self) -> bool {
        matches!(self, ReadMode::Full(_))
    }

    pub fn label(&self) -> &'static str {
        match self {
            ReadMode::Full(_) => MODE_FULL,
            ReadMode::Signatures => MODE_SIGNATURES,
            ReadMode::Map => MODE_MAP,
            ReadMode::Diff => MODE_DIFF,
        }
    }
}

/// Pure read result.
#[derive(Debug, Clone)]
pub struct ReadOutput {
    pub content: String,
    pub mode: ReadMode,
    pub original_tokens: usize,
    pub output_tokens: usize,
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Read a file from disk and render it in `mode`.
///
/// identical (disk content, mode, crp_mode, task) → byte-identical output.
pub fn read(
    path: &str,
    mode: &ReadMode,
    crp_mode: CrpMode,
    task: Option<&str>,
) -> Result<ReadOutput, ReadError> {
    let content = read_file_lossy(path)?;
    Ok(render_content(&content, path, mode, crp_mode, task))
}

/// Render already-loaded content in `mode` — no disk I/O.
///
/// Pure: output is a deterministic function of inputs.
pub fn render_content(
    content: &str,
    path: &str,
    mode: &ReadMode,
    crp_mode: CrpMode,
    task: Option<&str>,
) -> ReadOutput {
    let ext = Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("");
    let original_tokens = count_tokens(content);
    let short = protocol::shorten_path(path);

    let (body, _resolved_label) = match mode {
        ReadMode::Diff => {
            // Reflects current working-tree state.
            let output = std::process::Command::new("git")
                .args(["diff", "HEAD", "--", path])
                .output();
            let body = match output {
                Ok(out) if out.status.success() && !out.stdout.is_empty() => {
                    let diff_text = String::from_utf8_lossy(&out.stdout);
                    let sent = count_tokens(&diff_text);
                    let savings = protocol::format_savings(original_tokens, sent);
                    format!("{short} [git diff HEAD]\n{diff_text}\n{savings}")
                }
                Ok(_) => format!("{short} [no uncommitted changes against HEAD]"),
                Err(e) => format!("{short} [git diff failed: {e}]"),
            };
            let sent = count_tokens(&body);
            return ReadOutput {
                content: body,
                mode: ReadMode::Diff,
                original_tokens,
                output_tokens: sent,
            };
        }

        ReadMode::Full(range) => {
            let ranged = apply_range(content, *range);
            let line_count = ranged.lines().count();
            let (framed, _) =
                format_full_output("", &short, ext, &ranged, original_tokens, line_count, task);
            (framed, MODE_FULL)
        }

        ReadMode::Signatures => {
            let (out, _sent) = render::render_signatures(
                content,
                &short,
                ext,
                original_tokens,
                crp_mode,
                path,
                task,
            );
            (out, MODE_SIGNATURES)
        }

        ReadMode::Map => {
            let (out, _sent) =
                render::render_map(content, &short, ext, original_tokens, crp_mode, path, task);
            (out, MODE_MAP)
        }
    };

    let output_tokens = count_tokens(&body);
    ReadOutput {
        content: body,
        mode: mode.clone(),
        original_tokens,
        output_tokens,
    }
}

/// Check if `path` is an instruction/rules file.
pub fn is_instruction_file(path: &str) -> bool {
    let lower = path.to_lowercase();
    let filename = std::path::Path::new(&lower)
        .file_name()
        .and_then(|f| f.to_str())
        .unwrap_or("");

    matches!(
        filename,
        "skill.md"
            | "agents.md"
            | "rules.md"
            | ".cursorrules"
            | ".clinerules"
            | "lean-ctx.md"
            | "lean-ctx.mdc"
    ) || lower.contains("/skills/")
        || lower.contains("/.cursor/rules/")
        || lower.contains("/.claude/rules/")
        || lower.contains("/agents.md")
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Apply an optional line range to content. Range is assumed valid.
fn apply_range(content: &str, range: Option<LineRange>) -> String {
    let Some(r) = range else {
        return content.to_string();
    };
    let lines: Vec<&str> = content.lines().collect();
    let total = lines.len();
    let start = (r.start - 1).min(total);
    let end = r.end.min(total);
    if start < end {
        lines[start..end].join("\n")
    } else {
        String::new()
    }
}

// ---------------------------------------------------------------------------
// File I/O
// ---------------------------------------------------------------------------

/// Read a file as UTF-8 with lossy fallback.
pub fn read_file_lossy(path: &str) -> Result<String, ReadError> {
    if crate::core::binary_detect::is_binary_file(path) {
        let msg = crate::core::binary_detect::binary_file_message(path);
        return Err(ReadError::Binary(msg));
    }

    let cap = crate::core::limits::max_read_bytes() as u64;
    let file = open_with_retry(path)?;
    let meta = file
        .metadata()
        .map_err(|e| ReadError::Io(std::io::Error::other(format!("cannot stat open fd: {e}"))))?;
    if meta.len() > cap {
        return Err(ReadError::TooLarge {
            size: meta.len(),
            limit: cap,
        });
    }

    use std::io::Read;
    let mut bytes = Vec::with_capacity(meta.len() as usize);
    std::io::BufReader::new(file).read_to_end(&mut bytes)?;
    match String::from_utf8(bytes) {
        Ok(s) => Ok(s),
        Err(e) => Ok(String::from_utf8_lossy(e.as_bytes()).into_owned()),
    }
}

fn open_with_retry(path: &str) -> Result<std::fs::File, ReadError> {
    match open_nofollow(path) {
        Ok(f) => Ok(f),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            std::thread::sleep(std::time::Duration::from_millis(50));
            open_nofollow(path).map_err(|_| ReadError::NotFound(path.to_string()))
        }
        Err(e) => Err(ReadError::Io(e)),
    }
}

#[cfg(unix)]
fn open_nofollow(path: &str) -> Result<std::fs::File, std::io::Error> {
    use std::os::unix::fs::OpenOptionsExt;
    let p = Path::new(path);
    if let (Some(parent), Some(filename)) = (p.parent(), p.file_name())
        && parent.exists()
    {
        let canonical_parent = crate::core::pathutil::safe_canonicalize_bounded(parent, 2000);
        let canonical_path = canonical_parent.join(filename);
        std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&canonical_path)
    } else {
        std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(path)
    }
}

#[cfg(not(unix))]
fn open_nofollow(path: &str) -> Result<std::fs::File, std::io::Error> {
    std::fs::File::open(path)
}

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub enum ReadError {
    Binary(String),
    NotFound(String),
    TooLarge { size: u64, limit: u64 },
    Io(std::io::Error),
}

impl std::fmt::Display for ReadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ReadError::Binary(msg) => write!(f, "{msg}"),
            ReadError::NotFound(p) => write!(f, "file not found: {p}"),
            ReadError::TooLarge { size, limit } => write!(
                f,
                "file too large ({size} bytes, limit {limit} bytes via LCTX_MAX_READ_BYTES). \
                 Use offset=1, limit=100 for partial reads."
            ),
            ReadError::Io(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for ReadError {}

impl From<std::io::Error> for ReadError {
    fn from(e: std::io::Error) -> Self {
        ReadError::Io(e)
    }
}

const COMPRESSED_HINT: &str =
    "[lean-ctx: compact view \u{2014} nothing lost, full source on request]";

/// Append a hint that the agent can recover the full source.
pub fn append_compressed_hint(output: &str, file_path: &str) -> String {
    if !crate::core::profiles::active_profile()
        .output_hints
        .compressed_hint()
    {
        return output.to_string();
    }
    format!(
        "{output}\n{COMPRESSED_HINT}\n  {MODE_FULL}: ctx_read(\"{file_path}\", mode=\"{MODE_FULL}\")  ·  recover: ctx_retrieve(\"{file_path}\")"
    )
}
