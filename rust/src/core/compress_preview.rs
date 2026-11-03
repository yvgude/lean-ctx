//! compress_preview — a read-only "what would compression do to this?" inspector
//! (#984, Headroom #1267).
//!
//! Returns the original alongside the exact bytes lean-ctx would emit, with token
//! and byte accounting plus the line diff. It deliberately calls the **production**
//! compressors — [`compressor::aggressive_compress`] for the file-read path and
//! the shell engine's `compress_if_beneficial` for command output — so the
//! preview is always what the agent would actually receive, never a re-derivation
//! that could drift from the real pipeline (#498).

use crate::core::compressor;
use crate::core::tokens::count_tokens;

/// Which production compressor a preview runs through.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pipeline {
    /// The `ctx_read` aggressive arm: strips comments/blank lines and losslessly
    /// compacts JSON/CSV. The file extension selects the language-specific rules.
    Read,
    /// The `ctx_shell` output pass: the ~95-pattern beneficial compressor.
    Shell,
}

impl Pipeline {
    /// Stable label used in [`Preview::render`] (no spaces that would break a
    /// machine split on the header line).
    pub fn label(self) -> &'static str {
        match self {
            Pipeline::Read => "read/aggressive",
            Pipeline::Shell => "shell",
        }
    }
}

/// A computed preview: the original, the compressed form lean-ctx would emit, and
/// the token/byte accounting for both.
pub struct Preview {
    pub pipeline: Pipeline,
    pub original: String,
    pub compressed: String,
    pub original_tokens: usize,
    pub compressed_tokens: usize,
}

impl Preview {
    pub fn original_bytes(&self) -> usize {
        self.original.len()
    }

    pub fn compressed_bytes(&self) -> usize {
        self.compressed.len()
    }

    /// Tokens removed. Saturating: a pipeline never inflates in practice, but the
    /// accounting stays honest (never negative) if a pathological input did.
    pub fn saved_tokens(&self) -> usize {
        self.original_tokens.saturating_sub(self.compressed_tokens)
    }

    /// Compressed-to-original token ratio in `[0.0, 1.0]` (1.0 = no change, also
    /// the empty-input convention). Lower is better.
    pub(crate) fn token_ratio(&self) -> f64 {
        if self.original_tokens == 0 {
            return 1.0;
        }
        self.compressed_tokens as f64 / self.original_tokens as f64
    }

    /// Percent of tokens saved, rounded to one decimal (derived from the ratio).
    pub fn saved_pct(&self) -> f64 {
        ((1.0 - self.token_ratio()) * 1000.0).round() / 10.0
    }

    /// Line-level diff original→compressed via the shared differ.
    pub fn diff(&self) -> String {
        compressor::diff_content(&self.original, &self.compressed)
    }

    /// Human-readable report: an accounting header followed by the diff.
    /// Deterministic (no timestamps/counters) so the output is cache-stable (#498).
    pub fn render(&self) -> String {
        let mut out = String::new();
        out.push_str("compress preview — pipeline: ");
        out.push_str(self.pipeline.label());
        out.push('\n');
        out.push_str(&format!(
            "tokens: {} -> {}  (-{}, {:.1}% saved)\n",
            self.original_tokens,
            self.compressed_tokens,
            self.saved_tokens(),
            self.saved_pct(),
        ));
        out.push_str(&format!(
            "bytes:  {} -> {}\n",
            self.original_bytes(),
            self.compressed_bytes(),
        ));
        out.push_str("-- diff (original -> compressed) --\n");
        out.push_str(&self.diff());
        if !out.ends_with('\n') {
            out.push('\n');
        }
        out
    }
}

/// Preview the read/aggressive pipeline for `content`, with an optional file
/// extension that drives language-specific comment stripping and the JSON/CSV
/// crushers.
pub fn preview_read(content: &str, ext: Option<&str>) -> Preview {
    let compressed = compressor::aggressive_compress(content, ext);
    finish(Pipeline::Read, content, compressed)
}

/// Preview the shell pipeline for `output` produced by `command` (the command
/// steers build/test-aware verbatim preservation).
pub fn preview_shell(command: &str, output: &str) -> Preview {
    let compressed = crate::shell::compress::engine::compress_if_beneficial_pub(command, output);
    finish(Pipeline::Shell, output, compressed)
}

fn finish(pipeline: Pipeline, original: &str, compressed: String) -> Preview {
    Preview {
        pipeline,
        original_tokens: count_tokens(original),
        compressed_tokens: count_tokens(&compressed),
        original: original.to_string(),
        compressed,
    }
}

/// Lowercased extension (no leading dot) of `path`, for [`preview_read`].
pub fn ext_of(path: &str) -> Option<String> {
    std::path::Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_preview_strips_comments_and_saves_tokens() {
        let src = "// a leading comment\nfn main() {\n    // inner note\n    let x = 1;\n}\n";
        let p = preview_read(src, Some("rs"));
        assert_eq!(p.pipeline, Pipeline::Read);
        assert!(
            !p.compressed.contains("leading comment") && !p.compressed.contains("inner note"),
            "comments must be stripped: {}",
            p.compressed
        );
        assert!(p.compressed_tokens < p.original_tokens, "tokens must drop");
        assert!(p.saved_tokens() > 0);
        assert!(p.token_ratio() < 1.0);
        assert!(p.saved_pct() > 0.0);
    }

    #[test]
    fn read_preview_compacts_pretty_json() {
        let json =
            "{\n  \"name\": \"lean-ctx\",\n  \"nested\": {\n    \"a\": 1,\n    \"b\": 2\n  }\n}\n";
        let p = preview_read(json, Some("json"));
        // Structured compaction strips insignificant whitespace losslessly.
        assert!(!p.compressed.contains("\n  "), "json must be compacted");
        assert!(p.compressed_tokens < p.original_tokens);
    }

    #[test]
    fn no_change_input_reports_no_savings_and_clean_diff() {
        let src = "let x = 1;";
        let p = preview_read(src, Some("rs"));
        assert_eq!(
            p.compressed, p.original,
            "already-minimal input is unchanged"
        );
        assert_eq!(p.saved_tokens(), 0);
        assert!((p.token_ratio() - 1.0).abs() < f64::EPSILON);
        assert_eq!(p.saved_pct(), 0.0);
        assert_eq!(p.diff(), "(no changes)");
    }

    #[test]
    fn shell_preview_compresses_repetitive_output() {
        let output = "Compiling foo\n".repeat(40);
        let p = preview_shell("cargo build", &output);
        assert_eq!(p.pipeline, Pipeline::Shell);
        assert!(
            p.compressed_tokens <= p.original_tokens,
            "shell compression never inflates"
        );
    }

    #[test]
    fn render_is_deterministic_and_self_describing() {
        let src = "// c\nfn a() {}\n";
        let p = preview_read(src, Some("rs"));
        let a = p.render();
        let b = p.render();
        assert_eq!(a, b, "render must be deterministic (#498)");
        assert!(a.contains("pipeline: read/aggressive"));
        assert!(a.contains("tokens:"));
        assert!(a.contains("bytes:"));
    }

    #[test]
    fn ext_of_extracts_lowercased_extension() {
        assert_eq!(ext_of("/a/b/File.RS").as_deref(), Some("rs"));
        assert_eq!(ext_of("data.JSON").as_deref(), Some("json"));
        assert_eq!(ext_of("Makefile"), None);
        assert_eq!(ext_of("-"), None);
    }
}
