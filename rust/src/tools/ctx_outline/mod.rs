//! `ctx_outline` — fast, syntax-aware code outline (a "table of contents").
//!
//! Backed by tree-sitter (primary, via `crate::core::signatures_ts`) with a
//! conservative regex fallback. Three navigation questions, one primitive:
//! - a single **file** → its shape,
//! - a **directory** → the folder surface (per-file symbols),
//! - a `match`/`kind`-filtered slice → focused detail.
//!
//! Output can be the compact text outline (default) or deterministic JSON
//! (`format=json`, byte-stable per #498) that labels the extraction `backend`
//! per file, so the "syntax-aware" claim is verifiable rather than asserted
//! (gitlab #981).

mod dir;
mod json;
#[cfg(test)]
mod tests;

use crate::core::signatures::{SigBackend, Signature, extract_signatures_with_backend};
use crate::core::tokens::count_tokens;
use crate::tools::CrpMode;

/// Knobs for an outline run. `kind` filters by symbol kind (`fn|struct|class|…`
/// or `all`), `name_match` keeps only symbols whose name contains the substring
/// (case-insensitive), `as_json` switches to the deterministic JSON renderer.
#[derive(Debug, Clone, Default)]
pub struct OutlineOpts<'a> {
    pub kind: Option<&'a str>,
    pub name_match: Option<&'a str>,
    pub as_json: bool,
}

/// Tool identity the sources are read for (path jail, role rules, audit).
const TOOL: &str = "ctx_outline";

/// Per-file extracted + filtered symbols, shared by the directory text and JSON
/// renderers. `rel` is the path relative to the outlined directory (forward
/// slashes for deterministic, OS-independent output).
struct FileSymbols {
    rel: String,
    ext: String,
    backend: SigBackend,
    sigs: Vec<Signature>,
}

/// Outline a file or directory. Returns `(rendered_output, original_tokens)`,
/// where `original_tokens` is the full-read baseline used for savings reporting.
///
/// Test entry point: the fixture's own directory is the explicit root.
/// Production callers name the call's project root ([`run_rooted`]); no root
/// is ever derived from the path there.
#[cfg(test)]
#[must_use]
pub fn run(path: &str, opts: &OutlineOpts) -> (String, usize) {
    let root = crate::tools::admitted_sources::fixture_root(path);
    run_rooted(path, &root, opts)
}

/// Outline under an explicit root. Every source is acquired through the common
/// rooted admitted reader (G01): symbols are extracted from the ADMITTED text,
/// so a name or signature a rule masks is masked in the outline, and a source
/// the policy withholds contributes no symbol — it is reported as withheld,
/// never as a file without symbols.
#[must_use]
pub fn run_rooted(path: &str, root: &str, opts: &OutlineOpts) -> (String, usize) {
    // Path containment is enforced upstream by the resolution layer
    // (`require_resolved_path` → `resolve_path`), the sole caller of `run` on the
    // live MCP path: an escaping path (absolute, `..`, or a symlink whose target
    // leaves the project root) is rejected before we are reached. An in-tree
    // symlink therefore arrives already resolved to its real, in-jail target and
    // is outlined like any other file — so we deliberately do not second-guess it
    // here with a misleading "skipped for security" message that never fires on
    // the live path. `metadata()` (not `symlink_metadata()`) follows the link.
    let p = std::path::Path::new(path);
    match p.metadata() {
        Ok(m) if m.is_dir() => dir::outline_dir(path, root, opts),
        Ok(_) => outline_file(path, root, opts),
        Err(e) => (format!("ERROR: Cannot read {path}: {e}"), 0),
    }
}

fn outline_file(path: &str, root: &str, opts: &OutlineOpts) -> (String, usize) {
    use crate::tools::admitted_sources::SourceRead;
    let mut remaining = crate::core::limits::max_read_bytes();
    let content = match crate::tools::admitted_sources::read(path, root, TOOL, &mut remaining) {
        SourceRead::Admitted(read) => read.content,
        SourceRead::Withheld(reason) | SourceRead::Unavailable(reason) => {
            return (format!("ERROR: Cannot read {path}: {reason}"), 0);
        }
    };
    let full_tokens = count_tokens(&content);
    let ext = ext_of(path);
    let (sigs, backend) = extract_signatures_with_backend(&content, ext);
    let filtered = filter_signatures(&sigs, opts);

    if opts.as_json {
        return (json::file_json(path, ext, backend, &filtered), full_tokens);
    }

    if filtered.is_empty() {
        return (no_match_message(path, opts), 0);
    }

    let crp = CrpMode::effective();
    let mut outline = filtered
        .iter()
        .map(|s| render_one(s, crp))
        .collect::<Vec<_>>()
        .join("\n");
    if crp.is_tdd() {
        let legend = crate::core::signatures::tdd_legend(&filtered);
        if !legend.is_empty() {
            outline = format!("{legend}\n{outline}");
        }
    }
    // Located symbols are addressable as stable handles (#607): one self-
    // describing hint, not a per-line handle that would just repeat name+span.
    outline.push('\n');
    outline.push_str(crate::core::handle::USAGE_HINT);

    let sent = count_tokens(&outline);
    let savings = crate::core::protocol::format_savings(full_tokens, sent);
    (format!("{outline}\n{savings}"), full_tokens)
}

/// Render one signature for the text outline. Navigation modes earn their line
/// span (`@Lstart-end`): the whole point of an outline is to locate the next
/// read, so unlike the compression-first renderers we always include it.
fn render_one(s: &Signature, crp: CrpMode) -> String {
    if crp.is_tdd() {
        s.to_tdd_located()
    } else {
        s.to_compact_located()
    }
}

/// Apply the `kind` then `name_match` filters, preserving source order.
fn filter_signatures<'a>(sigs: &'a [Signature], opts: &OutlineOpts) -> Vec<&'a Signature> {
    let kind = opts.kind.map(str::to_lowercase);
    let name = opts.name_match.map(str::to_lowercase);
    sigs.iter()
        .filter(|s| match &kind {
            None => true,
            Some(k) if k == "all" => true,
            Some(k) => s.kind.eq_ignore_ascii_case(k),
        })
        .filter(|s| match &name {
            None => true,
            Some(n) => s.name.to_lowercase().contains(n.as_str()),
        })
        .collect()
}

fn no_match_message(path: &str, opts: &OutlineOpts) -> String {
    match (opts.kind, opts.name_match) {
        (_, Some(m)) => format!("No symbols matching '{m}' in {path}"),
        (Some(k), None) if !k.eq_ignore_ascii_case("all") => format!("No '{k}' symbols in {path}"),
        _ => format!("No symbols found in {path}"),
    }
}

fn ext_of(path: &str) -> &str {
    std::path::Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
}
