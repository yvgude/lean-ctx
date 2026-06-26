//! Cross-channel rule deduplication policy (#684).
//!
//! lean-ctx publishes its guidance through several "channels":
//!   * per-client global rule files (`~/.cursor/rules/lean-ctx.mdc`, …),
//!   * the shared project `AGENTS.md` (Cursor, Codex and other agents all
//!     auto-load it),
//!   * the MCP server `instructions` block (sent on every `initialize`).
//!
//! Several agents read more than one channel, so the *same* guidance can be
//! billed two or three times per session. This module centralises the policy
//! that decides, per client, which channel is the single canonical carrier — so
//! the writers (`compression` inject, hooks), the repair command
//! (`lean-ctx rules dedup`) and the honest accounting (`doctor overhead`) all
//! agree on one source of truth.

use std::path::Path;

/// Markers of the heavy compression / output-style block — the per-turn payload
/// that actually drives cross-channel duplication. Defined in `rules_canonical`
/// (the single marker source of truth) and re-exported here so the coverage/dedup
/// readers and the `render()` writer can never disagree (#548).
pub use crate::core::rules_canonical::{COMPRESSION_BLOCK_END, COMPRESSION_BLOCK_START};

/// The agents that auto-load the shared project `AGENTS.md`. Kept in sync with
/// `core::rules_overhead::collect_rules_files`, which attributes `AGENTS.md` to
/// the same set.
pub const AGENTS_MD_READERS: &[&str] = &["cursor", "codex"];

/// True when `content` carries a *full* lean-ctx payload — the canonical rule
/// set (the `RULES_MARKER` header) or the compression/output-style block —
/// rather than just the lightweight `<!-- lean-ctx -->` cross-reference pointer.
///
/// A pointer-only file (a thinned `AGENTS.md` / `.cursorrules` that merely says
/// "the full rules live in the canonical file") does not duplicate guidance and
/// must not be counted as a second source for its client.
pub fn carries_full_rules(content: &str) -> bool {
    content.contains(crate::core::rules_canonical::START_MARK)
        || content.contains(COMPRESSION_BLOCK_START)
}

/// True when `content` contains a lean-ctx block but only the lightweight
/// pointer (no canonical rules, no compression payload).
pub fn is_pointer_only(content: &str) -> bool {
    content.contains("<!-- lean-ctx") && !carries_full_rules(content)
}

fn file_has_compression(path: &Path) -> bool {
    std::fs::read_to_string(path).is_ok_and(|c| c.contains(COMPRESSION_BLOCK_START))
}

/// Cursor auto-loads `~/.cursor/rules/lean-ctx.mdc`; it is "covered" for the
/// compression payload once that canonical file carries the block.
pub fn cursor_compression_covered(home: &Path) -> bool {
    file_has_compression(&home.join(".cursor/rules/lean-ctx.mdc"))
}

/// Codex's per-user config dir (`~/.codex`, or `$CODEX_HOME`).
fn codex_dir(home: &Path) -> std::path::PathBuf {
    crate::core::home::resolve_codex_dir().unwrap_or_else(|| home.join(".codex"))
}

/// Codex is present on this machine when its config dir exists.
pub fn codex_present(home: &Path) -> bool {
    codex_dir(home).exists()
}

/// Codex auto-loads `~/.codex/AGENTS.md`; covered once it carries the block.
pub fn codex_compression_covered(home: &Path) -> bool {
    file_has_compression(&codex_dir(home).join("AGENTS.md"))
}

/// Decide whether the shared project `AGENTS.md` may drop its compression block
/// (keeping only the `<!-- lean-ctx -->` pointer). Safe ⇔ EVERY `AGENTS.md`
/// reader present on this machine already receives the compression payload from
/// its own canonical file.
///
/// Conservative by construction (#684, "thin only if covered"): if any reader
/// would lose the guidance, `AGENTS.md` stays the full carrier.
pub fn agents_md_can_thin(home: &Path) -> bool {
    if !cursor_compression_covered(home) {
        return false;
    }
    if codex_present(home) && !codex_compression_covered(home) {
        return false;
    }
    true
}

/// For the MCP `instructions` block: does `client_name` already auto-load the
/// compression payload from a rule file? If so, repeating the output-style
/// block in the per-session instructions is pure cross-channel duplication and
/// can be dropped (the file copy governs).
pub fn client_autoloads_compression(client_name: &str, home: &Path) -> bool {
    let lower = client_name.to_lowercase();
    if lower.is_empty() {
        return false;
    }
    if lower.contains("cursor") {
        return cursor_compression_covered(home);
    }
    if lower.contains("codex") {
        return codex_present(home) && codex_compression_covered(home);
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    const FULL_HEADER: &str = crate::core::rules_canonical::START_MARK;

    fn compression_block() -> String {
        format!("{COMPRESSION_BLOCK_START}\nOUTPUT STYLE\n{COMPRESSION_BLOCK_END}\n")
    }

    fn pointer_block() -> String {
        format!(
            "{}\n## lean-ctx\nFull rules: ~/.cursor/rules/lean-ctx.mdc\n{}\n",
            crate::core::rules_canonical::AGENTS_BLOCK_START,
            crate::core::rules_canonical::AGENTS_BLOCK_END,
        )
    }

    #[test]
    fn full_rules_detected_for_canonical_header_and_compression() {
        let comp = compression_block();
        let ptr = pointer_block();
        assert!(carries_full_rules(&format!("{FULL_HEADER}\nbody\n")));
        assert!(carries_full_rules(&comp));
        assert!(carries_full_rules(&format!("{ptr}{comp}")));
    }

    #[test]
    fn pointer_only_block_is_not_full() {
        let ptr = pointer_block();
        assert!(!carries_full_rules(&ptr));
        assert!(is_pointer_only(&ptr));
    }

    #[test]
    fn plain_user_content_is_neither_full_nor_pointer() {
        let user = "# My project rules\njust some notes\n";
        assert!(!carries_full_rules(user));
        assert!(!is_pointer_only(user));
    }

    #[test]
    fn cursor_coverage_follows_mdc_block() {
        let comp = compression_block();
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        assert!(!cursor_compression_covered(home));

        std::fs::create_dir_all(home.join(".cursor/rules")).unwrap();
        std::fs::write(
            home.join(".cursor/rules/lean-ctx.mdc"),
            format!("{FULL_HEADER}\n{comp}"),
        )
        .unwrap();
        assert!(cursor_compression_covered(home));
    }

    #[test]
    fn agents_md_thins_only_when_cursor_covered_and_no_uncovered_codex() {
        let comp = compression_block();
        // Serialize CODEX_HOME mutation (tests share the process environment).
        let _guard = crate::core::data_dir::test_env_lock();
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();

        // No canonical mdc yet → AGENTS.md must stay the carrier.
        crate::test_env::set_var("CODEX_HOME", home.join(".codex"));
        assert!(!agents_md_can_thin(home));

        // Cursor covered, codex absent → safe to thin (the common case).
        // CODEX_HOME points at this isolated home so a real `~/.codex` on the
        // test machine cannot leak in.
        std::fs::create_dir_all(home.join(".cursor/rules")).unwrap();
        std::fs::write(
            home.join(".cursor/rules/lean-ctx.mdc"),
            format!("{FULL_HEADER}\n{comp}"),
        )
        .unwrap();
        assert!(agents_md_can_thin(home));

        // Codex present but uncovered → must NOT thin (codex would lose it).
        std::fs::create_dir_all(home.join(".codex")).unwrap();
        assert!(codex_present(home));
        assert!(!agents_md_can_thin(home));

        // Codex now covered by its own global AGENTS.md → safe to thin again.
        std::fs::write(home.join(".codex/AGENTS.md"), &comp).unwrap();
        assert!(agents_md_can_thin(home));
        crate::test_env::remove_var("CODEX_HOME");
    }

    #[test]
    fn client_autoloads_compression_is_client_aware() {
        let comp = compression_block();
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        std::fs::create_dir_all(home.join(".cursor/rules")).unwrap();
        std::fs::write(
            home.join(".cursor/rules/lean-ctx.mdc"),
            format!("{FULL_HEADER}\n{comp}"),
        )
        .unwrap();

        assert!(client_autoloads_compression("Cursor", home));
        assert!(client_autoloads_compression("cursor-vscode", home));
        // Empty / unknown clients never auto-load a file copy.
        assert!(!client_autoloads_compression("", home));
        assert!(!client_autoloads_compression("some-other-agent", home));
    }

    #[test]
    fn render_output_is_detected_as_compression_coverage() {
        // The slice's core guarantee (#548 B2): the bytes the writer (`render`)
        // emits into a carrier file are recognised by the coverage detection the
        // MCP cross-channel dedup depends on. Before the unified marker model,
        // render embedded the prompt inline (no markers) so this was always
        // false → Cursor was billed for the compression block twice (rule file +
        // every MCP session).
        use crate::core::config::CompressionLevel;
        use crate::core::rules_canonical::{Wrapper, render};

        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        assert!(!cursor_compression_covered(home));

        // Exactly what `rules_content`/inject writes to the Cursor mdc (frontmatter
        // is irrelevant to substring detection).
        let block = render(false, Wrapper::Dedicated, CompressionLevel::Standard);
        std::fs::create_dir_all(home.join(".cursor/rules")).unwrap();
        std::fs::write(home.join(".cursor/rules/lean-ctx.mdc"), &block).unwrap();

        assert!(cursor_compression_covered(home));
        assert!(client_autoloads_compression("cursor", home));

        // An Off render carries no payload, so it must NOT count as coverage.
        let off = render(false, Wrapper::Dedicated, CompressionLevel::Off);
        std::fs::write(home.join(".cursor/rules/lean-ctx.mdc"), &off).unwrap();
        assert!(!cursor_compression_covered(home));
    }
}
