use std::path::Path;

// ── Shared types moved here from tools/ to break reverse-dependency ──

/// Context Reduction Protocol mode controlling output verbosity.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CrpMode {
    Off,
    Compact,
    Tdd,
}

impl CrpMode {
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_lowercase().as_str() {
            "off" => Some(Self::Off),
            "compact" => Some(Self::Compact),
            "tdd" => Some(Self::Tdd),
            _ => None,
        }
    }
}

/// Recorded metrics for a single MCP tool invocation.
#[derive(Clone, Debug)]
pub struct ToolCallRecord {
    pub tool: String,
    pub original_tokens: usize,
    pub saved_tokens: usize,
    pub mode: Option<String>,
    pub duration_ms: u64,
    pub timestamp: String,
}

/// Finds the outermost project root by walking up from `file_path`.
/// For monorepos with nested `.git` dirs (e.g. `mono/backend/.git` + `mono/frontend/.git`),
/// returns the outermost ancestor containing `.git`, a workspace marker, or a known
/// monorepo config file — so the whole monorepo is treated as one project.
#[must_use]
pub fn detect_project_root(file_path: &str) -> Option<String> {
    let start = Path::new(file_path);
    let mut dir = if start.is_dir() {
        start
    } else {
        start.parent()?
    };
    let mut best: Option<String> = None;

    loop {
        if is_project_root_marker(dir) {
            best = Some(dir.to_string_lossy().to_string());
        }
        match dir.parent() {
            Some(parent) if parent != dir => dir = parent,
            _ => break,
        }
    }
    best
}

/// Checks if a directory looks like a project root (has `.git`, workspace config, etc.).
fn is_project_root_marker(dir: &Path) -> bool {
    const MARKERS: &[&str] = &[
        ".git",
        "Cargo.toml",
        "package.json",
        "go.work",
        "pnpm-workspace.yaml",
        "lerna.json",
        "nx.json",
        "turbo.json",
        ".projectile",
        "pyproject.toml",
        "setup.py",
        "Makefile",
        "CMakeLists.txt",
        "BUILD.bazel",
    ];
    MARKERS.iter().any(|m| dir.join(m).exists())
}

/// Returns the project root for `file_path`, falling back to cwd if none found.
/// Checks `LEAN_CTX_PROJECT_ROOT` env var and config.toml `project_root` first.
/// Logs a warning when the fallback is a broad directory (home, root).
pub fn detect_project_root_or_cwd(file_path: &str) -> String {
    if let Ok(env_root) = std::env::var("LEAN_CTX_PROJECT_ROOT")
        && !env_root.is_empty()
    {
        return env_root;
    }
    let cfg = crate::core::config::Config::load();
    if let Some(ref cfg_root) = cfg.project_root
        && !cfg_root.is_empty()
    {
        return cfg_root.clone();
    }
    if let Some(ide_root) = resolve_ide_path(&cfg, file_path) {
        return ide_root;
    }
    if let Some(root) = detect_project_root(file_path) {
        return root;
    }

    let fallback = {
        let p = Path::new(file_path);
        if p.exists() {
            if p.is_dir() {
                file_path.to_string()
            } else {
                p.parent().map_or_else(
                    || file_path.to_string(),
                    |pp| pp.to_string_lossy().to_string(),
                )
            }
        } else {
            std::env::current_dir()
                .map_or_else(|_| ".".to_string(), |p| p.to_string_lossy().to_string())
        }
    };

    if is_broad_directory(&fallback) {
        use std::sync::Once;
        static WARN_ONCE: Once = Once::new();
        WARN_ONCE.call_once(|| {
            tracing::warn!(
                "[protocol: no project detected — current directory is {fallback} which is not a project root.\n  \
                 To fix: run from inside a project (with .git, Cargo.toml, package.json, etc.)\n  \
                 Or set: export LEAN_CTX_PROJECT_ROOT=/path/to/your/project]"
            );
        });
    }

    fallback
}

fn is_broad_directory(path: &str) -> bool {
    if path == "/" || path == "\\" || path == "." {
        return true;
    }
    if let Some(home) = dirs::home_dir() {
        let home_str = home.to_string_lossy();
        if path == home_str.as_ref() || path == format!("{home_str}/") {
            return true;
        }
    }
    false
}

/// Resolves per-IDE allowed paths from config. If the active agent has
/// `ide_paths` configured, returns the first path that contains `file_path`.
fn resolve_ide_path(cfg: &crate::core::config::Config, file_path: &str) -> Option<String> {
    if cfg.ide_paths.is_empty() {
        return None;
    }
    let agent = std::env::var("LEAN_CTX_AGENT").ok()?;
    let agent_lower = agent.to_lowercase();
    let paths = cfg.ide_paths.get(&agent_lower)?;
    let fp = Path::new(file_path);
    for allowed in paths {
        let ap = Path::new(allowed.as_str());
        if fp.starts_with(ap) {
            return Some(allowed.clone());
        }
    }
    // file_path is outside all allowed paths — return first allowed path as root
    paths.first().cloned()
}

/// Returns the file name component of a path for compact display.
/// Normalize a path for display by converting Windows backslashes to forward
/// slashes. Forward slashes are valid path separators on Windows, and unlike
/// backslashes they are never misinterpreted as escape sequences by the JSON,
/// markdown, or terminal layers of MCP clients — which corrupted Windows paths
/// in tool output (e.g. `C:\Users\…` rendered as `CUsers…`). See issue #324.
#[must_use]
pub fn display_path(path: &str) -> String {
    path.replace('\\', "/")
}

#[must_use]
pub fn shorten_path(path: &str) -> String {
    let normalized = display_path(path);
    let p = Path::new(&normalized);
    if let Some(name) = p.file_name() {
        return name.to_string_lossy().to_string();
    }
    normalized
}

/// Returns a path relative to `root` for disambiguated display, always with
/// forward slashes. Falls back to the basename if stripping fails.
///
/// Relativization is done on slash-normalized strings so it works regardless of
/// the separator style the client sent (Windows backslashes, mixed separators).
/// A component boundary is required so that root `a/b` never matches `a/bc`.
#[must_use]
pub fn shorten_path_relative(path: &str, root: &str) -> String {
    let norm_path = display_path(path);
    let norm_root = display_path(root);
    let norm_root = norm_root.strip_suffix('/').unwrap_or(&norm_root);
    if let Some(rest) = norm_path.strip_prefix(norm_root)
        && let Some(rel) = rest.strip_prefix('/')
        && !rel.is_empty()
    {
        return rel.to_string();
    }
    shorten_path(&norm_path)
}

/// Whether savings footers should be suppressed in tool output.
///
/// Default config is `never` to keep CLI output quiet; `auto` remains available for
/// legacy compatibility and still follows transport context when explicitly enabled.
static MCP_CONTEXT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Mark the current process as serving MCP tool calls (suppresses savings footers in `auto` mode).
pub fn set_mcp_context(active: bool) {
    MCP_CONTEXT.store(active, std::sync::atomic::Ordering::Relaxed);
}

/// Returns true if savings footers should be shown based on config + transport context.
///
/// Suppressed when `LEAN_CTX_QUIET=1`, `LEAN_CTX_SHOW_SAVINGS=0`, or compression is `Max` (ultra).
pub fn savings_footer_visible() -> bool {
    if matches!(std::env::var("LEAN_CTX_QUIET"), Ok(v) if v.trim() == "1") {
        return false;
    }
    if matches!(std::env::var("LEAN_CTX_SHOW_SAVINGS"), Ok(v) if v.trim() == "0") {
        return false;
    }
    if matches!(std::env::var("LEAN_CTX_SHOW_SAVINGS"), Ok(v) if v.trim() == "1") {
        return true;
    }
    let mode = super::config::SavingsFooter::effective();
    match mode {
        super::config::SavingsFooter::Always => true,
        super::config::SavingsFooter::Never => false,
        super::config::SavingsFooter::Auto => {
            !MCP_CONTEXT.load(std::sync::atomic::Ordering::Relaxed)
        }
    }
}

/// Whether non-essential meta lines (cache refs, budget warnings, repetition hints) should be shown.
///
/// Default is false to keep tool outputs clean for agents; opt-in via env var.
#[must_use]
pub fn meta_visible() -> bool {
    if matches!(std::env::var("LEAN_CTX_QUIET"), Ok(v) if v.trim() == "1") {
        return false;
    }
    matches!(std::env::var("LEAN_CTX_META"), Ok(v) if v.trim() == "1")
        || matches!(std::env::var("LEAN_CTX_DIAGNOSTICS"), Ok(v) if v.trim() == "1")
}

/// Formats a token savings footer with box-drawing delimiters.
///
/// Output: `─── 4,200 → 840 tok (↓80%) ───`
///
/// Returns an empty string when savings footers are suppressed.
#[must_use]
pub fn format_savings(original: usize, compressed: usize) -> String {
    super::savings_footer::format_footer_basic(original, compressed)
}

/// Formats a savings footer with mode and optional detail context.
///
/// Output: `─── 4,200 → 840 tok (↓80%) | mode: map ───`
#[must_use]
pub fn format_savings_with_info(
    original: usize,
    compressed: usize,
    mode: Option<&str>,
    detail: Option<&str>,
) -> String {
    super::savings_footer::format_footer(&super::savings_footer::SavingsInfo {
        original,
        compressed,
        mode,
        detail,
    })
}

/// Appends a savings footer to `output` with a newline separator, but only if the footer is non-empty.
#[must_use]
pub fn append_savings(output: &str, original: usize, compressed: usize) -> String {
    super::savings_footer::append_footer_basic(output, original, compressed)
}

/// Appends a savings footer with mode/detail context.
#[must_use]
pub fn append_savings_with_info(
    output: &str,
    original: usize,
    compressed: usize,
    mode: Option<&str>,
    detail: Option<&str>,
) -> String {
    super::savings_footer::append_footer(
        output,
        &super::savings_footer::SavingsInfo {
            original,
            compressed,
            mode,
            detail,
        },
    )
}

/// Removes a single trailing savings footer line, if present.
///
/// The compression funnel appends at most one footer as the final line — either
/// the box-drawing form (`─── 4,200 → 840 tok (↓80%) ───`) or the verbatim
/// truncation form (`[lean-ctx: 4200→840 tok, verbatim truncated]`). The
/// `/v1/compress` contract surfaces savings in a structured `stats` field, so
/// message bodies must stay footer-free and byte-stable for prompt caching
/// (#498). This strips that trailing line regardless of the ambient
/// `savings_footer` setting; content without a footer is returned untouched.
#[must_use]
pub fn strip_trailing_savings_footer(output: &str) -> &str {
    let body = output.trim_end_matches('\n');
    let (head, last_line) = match body.rfind('\n') {
        Some(nl) => (&body[..nl], &body[nl + 1..]),
        None => ("", body),
    };
    if is_savings_footer_line(last_line) {
        head
    } else {
        output
    }
}

fn is_savings_footer_line(line: &str) -> bool {
    let l = line.trim();
    (l.starts_with("\u{2500}\u{2500}\u{2500} ") && l.ends_with(" \u{2500}\u{2500}\u{2500}"))
        || (l.starts_with("[lean-ctx: ") && l.ends_with(']'))
}

/// A terse instruction code and its human-readable expansion.
pub struct InstructionTemplate {
    pub code: &'static str,
    pub full: &'static str,
}

/// Exactly the codes `encode_instructions` can emit — the decoder block rides
/// in every tdd-mode session, so codes that are never emitted (NODOC,
/// ACTFIRST, NOMOCK) or already explained inline by the CRP suffix (ABBREV,
/// SYMBOLS) must not be re-defined here (#579).
const TEMPLATES: &[InstructionTemplate] = &[
    InstructionTemplate {
        code: "ACT1",
        full: "act now, 1-line result",
    },
    InstructionTemplate {
        code: "BRIEF",
        full: "1-2 line approach, then act",
    },
    InstructionTemplate {
        code: "FULL",
        full: "outline+edge cases first",
    },
    InstructionTemplate {
        code: "DELTA",
        full: "changed lines only",
    },
    InstructionTemplate {
        code: "NOREPEAT",
        full: "use Fn refs",
    },
    InstructionTemplate {
        code: "STRUCT",
        full: "+/-/~",
    },
    InstructionTemplate {
        code: "1LINE",
        full: "1 line/action",
    },
    InstructionTemplate {
        code: "QUALITY",
        full: "keep edge cases",
    },
    InstructionTemplate {
        code: "FREF",
        full: "Fn refs, no paths",
    },
    InstructionTemplate {
        code: "DIFF",
        full: "diff lines only",
    },
];

/// Generates the INSTRUCTION CODES block for agent system prompts.
/// Only emits content when the instructions being built are in Tdd CRP mode
/// (otherwise returns empty — the codes are only emitted in tdd outputs, so
/// defining them would waste ~60 tokens per MCP instructions payload, #579).
#[must_use]
pub fn instruction_decoder_block(tdd_active: bool) -> String {
    if !tdd_active {
        return String::new();
    }
    let pairs: Vec<String> = TEMPLATES
        .iter()
        .map(|t| format!("{}={}", t.code, t.full))
        .collect();
    format!("INSTRUCTION CODES:\n  {}", pairs.join(" | "))
}

/// Encode an instruction suffix using short codes with budget hints.
/// Response budget is dynamic based on task complexity to shape LLM output length.
#[must_use]
pub fn encode_instructions(complexity: &str) -> String {
    match complexity {
        "mechanical" => "MODE: ACT1 DELTA 1LINE | BUDGET: <=50 tokens, 1 line answer".to_string(),
        "simple" => "MODE: BRIEF DELTA 1LINE | BUDGET: <=100 tokens, structured".to_string(),
        "standard" => "MODE: BRIEF DELTA NOREPEAT STRUCT | BUDGET: <=200 tokens".to_string(),
        "complex" => {
            "MODE: FULL QUALITY NOREPEAT STRUCT FREF DIFF | BUDGET: <=500 tokens".to_string()
        }
        "architectural" => {
            "MODE: FULL QUALITY NOREPEAT STRUCT FREF | BUDGET: unlimited".to_string()
        }
        _ => "MODE: BRIEF | BUDGET: <=200 tokens".to_string(),
    }
}

/// Encode instructions with SNR metric for context quality awareness.
#[must_use]
pub fn encode_instructions_with_snr(complexity: &str, compression_pct: f64) -> String {
    let snr = if compression_pct > 0.0 {
        1.0 - (compression_pct / 100.0)
    } else {
        1.0
    };
    let base = encode_instructions(complexity);
    format!("{base} | SNR: {snr:.2}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_trailing_savings_footer_handles_both_styles() {
        // Box-drawing footer.
        let boxed = "body line one\nbody line two\n\u{2500}\u{2500}\u{2500} 4,200 \u{2192} 840 tok (\u{2193}80%) \u{2500}\u{2500}\u{2500}";
        assert_eq!(
            strip_trailing_savings_footer(boxed),
            "body line one\nbody line two"
        );
        // Verbatim-truncation footer.
        let verbatim = "out\n[lean-ctx: 4200\u{2192}840 tok, verbatim truncated]";
        assert_eq!(strip_trailing_savings_footer(verbatim), "out");
        // No footer → untouched (including trailing newline).
        assert_eq!(
            strip_trailing_savings_footer("plain body\n"),
            "plain body\n"
        );
        // A footer-only string collapses to empty.
        assert_eq!(
            strip_trailing_savings_footer("[lean-ctx: 10\u{2192}5 tok, verbatim truncated]"),
            ""
        );
        // A body line that merely mentions the marker mid-text is preserved.
        assert_eq!(
            strip_trailing_savings_footer("see [lean-ctx: docs] for details"),
            "see [lean-ctx: docs] for details"
        );
    }

    #[test]
    fn display_path_normalizes_windows_separators() {
        // Issue #324: backslashes were dropped/escaped by client render layers.
        assert_eq!(
            display_path(r"C:\Users\zir\AppData\Local\Temp\win-build-log.txt"),
            "C:/Users/zir/AppData/Local/Temp/win-build-log.txt"
        );
        assert_eq!(display_path("src/main.rs"), "src/main.rs");
    }

    #[test]
    fn shorten_path_basename_for_windows_abs_path() {
        assert_eq!(
            shorten_path(r"D:\Temp\win-build-raw.log"),
            "win-build-raw.log"
        );
        assert_eq!(shorten_path("a/b/c.txt"), "c.txt");
    }

    #[test]
    fn shorten_path_relative_handles_windows_separators() {
        // Relative display keeps forward slashes regardless of input style.
        assert_eq!(
            shorten_path_relative(r"C:\proj\src\app\main.rs", r"C:\proj"),
            "src/app/main.rs"
        );
        // Mixed separators between path and root still relativize.
        assert_eq!(
            shorten_path_relative(r"C:\proj\src\main.rs", "C:/proj/"),
            "src/main.rs"
        );
        // A non-prefix abs path falls back to a clean basename, never a
        // separator-stripped blob like "CUserszir…".
        assert_eq!(
            shorten_path_relative(r"C:\Users\zir\Temp\build.log", r"D:\proj"),
            "build.log"
        );
    }

    #[test]
    fn shorten_path_relative_requires_component_boundary() {
        // Root "a/b" must not match sibling "a/bc".
        assert_eq!(shorten_path_relative("a/bc/d.rs", "a/b"), "d.rs");
        assert_eq!(shorten_path_relative("a/b/d.rs", "a/b"), "d.rs");
    }

    #[test]
    fn is_project_root_marker_detects_git() {
        let tmp = std::env::temp_dir().join("lean-ctx-test-root-marker");
        let _ = std::fs::create_dir_all(&tmp);
        let git_dir = tmp.join(".git");
        let _ = std::fs::create_dir_all(&git_dir);
        assert!(is_project_root_marker(&tmp));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn is_project_root_marker_detects_cargo_toml() {
        let tmp = std::env::temp_dir().join("lean-ctx-test-cargo-marker");
        let _ = std::fs::create_dir_all(&tmp);
        let _ = std::fs::write(tmp.join("Cargo.toml"), "[package]");
        assert!(is_project_root_marker(&tmp));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn detect_project_root_finds_outermost() {
        let base = std::env::temp_dir().join("lean-ctx-test-monorepo");
        let inner = base.join("packages").join("app");
        let _ = std::fs::create_dir_all(&inner);
        let _ = std::fs::create_dir_all(base.join(".git"));
        let _ = std::fs::create_dir_all(inner.join(".git"));

        let test_file = inner.join("main.rs");
        let _ = std::fs::write(&test_file, "fn main() {}");

        let root = detect_project_root(test_file.to_str().unwrap());
        assert!(root.is_some(), "should find a project root for nested .git");
        let root_path = std::path::PathBuf::from(root.unwrap());
        assert_eq!(
            crate::core::pathutil::safe_canonicalize(&root_path).ok(),
            crate::core::pathutil::safe_canonicalize(&base).ok(),
            "should return outermost .git, not inner"
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn decoder_block_contains_all_codes() {
        let block = instruction_decoder_block(true);
        for t in TEMPLATES {
            assert!(
                block.contains(t.code),
                "decoder should contain code {}",
                t.code
            );
        }
    }

    #[test]
    fn decoder_block_empty_outside_tdd() {
        assert!(instruction_decoder_block(false).is_empty());
    }

    #[test]
    fn decoder_codes_match_what_encode_can_emit() {
        // Every defined code must appear in at least one encode_instructions
        // output — dead definitions tax every tdd session (#579).
        let all_modes: Vec<String> = [
            "mechanical",
            "simple",
            "standard",
            "complex",
            "architectural",
            "unknown",
        ]
        .iter()
        .map(|c| encode_instructions(c))
        .collect();
        for t in TEMPLATES {
            assert!(
                all_modes.iter().any(|m| m.contains(t.code)),
                "code {} is defined but never emitted",
                t.code
            );
        }
    }

    #[test]
    fn encoded_instructions_are_compact() {
        use super::super::tokens::count_tokens;
        let full = "TASK COMPLEXITY: mechanical\nMinimal reasoning needed. Act immediately, report result in one line. Show only changed lines, not full files.";
        let encoded = encode_instructions("mechanical");
        assert!(
            count_tokens(&encoded) <= count_tokens(full),
            "encoded ({}) should be <= full ({})",
            count_tokens(&encoded),
            count_tokens(full)
        );
    }

    #[test]
    fn all_complexity_levels_encode() {
        for level in &["mechanical", "standard", "architectural"] {
            let encoded = encode_instructions(level);
            assert!(encoded.starts_with("MODE:"), "should start with MODE:");
        }
    }

    #[test]
    fn savings_footer_env_gated_tests() {
        let _lock = crate::core::data_dir::test_env_lock();

        // Test: always mode shows box-drawing format
        super::MCP_CONTEXT.store(false, std::sync::atomic::Ordering::Relaxed);
        crate::test_env::set_var("LEAN_CTX_SAVINGS_FOOTER", "always");
        crate::test_env::set_var("LEAN_CTX_SHOW_SAVINGS", "1");
        crate::test_env::remove_var("LEAN_CTX_QUIET");

        let s = super::format_savings(100, 50);
        assert!(s.contains("\u{2192}"), "expected arrow: {s}");
        assert!(s.contains("\u{2193}50%"), "expected pct: {s}");
        assert!(
            s.starts_with("\u{2500}\u{2500}\u{2500}"),
            "expected box-drawing: {s}"
        );

        // Test: mode info included
        let s = super::format_savings_with_info(4200, 840, Some("map"), None);
        assert!(s.contains("mode: map"), "expected mode: {s}");
        assert!(s.contains("\u{2193}80%"), "expected 80%: {s}");

        // Test: never mode suppresses
        crate::test_env::set_var("LEAN_CTX_SAVINGS_FOOTER", "never");
        crate::test_env::set_var("LEAN_CTX_SHOW_SAVINGS", "0");
        let s = super::format_savings(100, 50);
        assert!(s.is_empty(), "expected empty with never: {s}");

        let result = super::append_savings("hello", 100, 50);
        assert_eq!(result, "hello");

        // Test: MCP auto mode suppresses
        super::MCP_CONTEXT.store(true, std::sync::atomic::Ordering::Relaxed);
        crate::test_env::set_var("LEAN_CTX_SAVINGS_FOOTER", "auto");
        crate::test_env::remove_var("LEAN_CTX_SHOW_SAVINGS");
        let s = super::format_savings(100, 50);
        assert!(s.is_empty(), "expected empty in MCP+auto: {s}");
        super::MCP_CONTEXT.store(false, std::sync::atomic::Ordering::Relaxed);

        // Test: SHOW_SAVINGS overrides config
        crate::test_env::set_var("LEAN_CTX_SAVINGS_FOOTER", "never");
        crate::test_env::set_var("LEAN_CTX_SHOW_SAVINGS", "1");
        assert!(super::savings_footer_visible());
        crate::test_env::set_var("LEAN_CTX_SHOW_SAVINGS", "0");
        assert!(!super::savings_footer_visible());

        // Restore ALL touched env — leaking LEAN_CTX_SAVINGS_FOOTER made
        // footers visible in unrelated tests (GL #556 flakiness).
        crate::test_env::remove_var("LEAN_CTX_SHOW_SAVINGS");
        crate::test_env::remove_var("LEAN_CTX_SAVINGS_FOOTER");
    }
}
