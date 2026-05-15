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
/// Checks LEAN_CTX_PROJECT_ROOT env var and config.toml `project_root` first.
/// Logs a warning when the fallback is a broad directory (home, root).
pub fn detect_project_root_or_cwd(file_path: &str) -> String {
    if let Ok(env_root) = std::env::var("LEAN_CTX_PROJECT_ROOT") {
        if !env_root.is_empty() {
            return env_root;
        }
    }
    if let Some(ref cfg_root) = crate::core::config::Config::load().project_root {
        if !cfg_root.is_empty() {
            return cfg_root.clone();
        }
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
                "[protocol: no project markers found — falling back to broad directory {fallback}. \
                 Set LEAN_CTX_PROJECT_ROOT to override]"
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

/// Returns the file name component of a path for compact display.
pub fn shorten_path(path: &str) -> String {
    let p = Path::new(path);
    if let Some(name) = p.file_name() {
        return name.to_string_lossy().to_string();
    }
    path.to_string()
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
/// Suppressed when `LEAN_CTX_QUIET=1` (production use, e.g. Codex with minimal verbosity).
pub fn savings_footer_visible() -> bool {
    if matches!(std::env::var("LEAN_CTX_QUIET"), Ok(v) if v.trim() == "1") {
        return false;
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
pub fn meta_visible() -> bool {
    if matches!(std::env::var("LEAN_CTX_QUIET"), Ok(v) if v.trim() == "1") {
        return false;
    }
    matches!(std::env::var("LEAN_CTX_META"), Ok(v) if v.trim() == "1")
        || matches!(std::env::var("LEAN_CTX_DIAGNOSTICS"), Ok(v) if v.trim() == "1")
}

/// Formats a unified token savings footer like `[lean-ctx: 100→50 tok, -50%]`.
///
/// Returns an empty string when savings footers are suppressed (MCP context in `auto` mode,
/// or `savings_footer = "never"`).
pub fn format_savings(original: usize, compressed: usize) -> String {
    if !savings_footer_visible() {
        return String::new();
    }
    if original == 0 {
        return String::new();
    }
    let saved = original.saturating_sub(compressed);
    if saved == 0 {
        return String::new();
    }
    let pct = (saved as f64 / original as f64 * 100.0).round() as usize;
    format!("[lean-ctx: {original}\u{2192}{compressed} tok, -{pct}%]")
}

/// Appends a savings footer to `output` with a newline separator, but only if the footer is non-empty.
pub fn append_savings(output: &str, original: usize, compressed: usize) -> String {
    let footer = format_savings(original, compressed);
    if footer.is_empty() {
        output.to_string()
    } else {
        format!("{output}\n{footer}")
    }
}

/// A terse instruction code and its human-readable expansion.
pub struct InstructionTemplate {
    pub code: &'static str,
    pub full: &'static str,
}

const TEMPLATES: &[InstructionTemplate] = &[
    InstructionTemplate {
        code: "ACT1",
        full: "Act immediately, 1-line result",
    },
    InstructionTemplate {
        code: "BRIEF",
        full: "1-2 line approach, then act",
    },
    InstructionTemplate {
        code: "FULL",
        full: "Outline+edge cases, then act",
    },
    InstructionTemplate {
        code: "DELTA",
        full: "Changed lines only",
    },
    InstructionTemplate {
        code: "NOREPEAT",
        full: "No repeat, use Fn refs",
    },
    InstructionTemplate {
        code: "STRUCT",
        full: "+/-/~ notation",
    },
    InstructionTemplate {
        code: "1LINE",
        full: "1 line per action",
    },
    InstructionTemplate {
        code: "NODOC",
        full: "No narration comments",
    },
    InstructionTemplate {
        code: "ACTFIRST",
        full: "Tool calls first, no narration",
    },
    InstructionTemplate {
        code: "QUALITY",
        full: "Never skip edge cases",
    },
    InstructionTemplate {
        code: "NOMOCK",
        full: "No mock/placeholder data",
    },
    InstructionTemplate {
        code: "FREF",
        full: "Fn refs only, no full paths",
    },
    InstructionTemplate {
        code: "DIFF",
        full: "Diff lines only",
    },
    InstructionTemplate {
        code: "ABBREV",
        full: "fn,cfg,impl,deps,req,res,ctx,err",
    },
    InstructionTemplate {
        code: "SYMBOLS",
        full: "+=add -=rm ~=mod ->=ret",
    },
];

/// Generates the INSTRUCTION CODES block for agent system prompts.
pub fn instruction_decoder_block() -> String {
    let pairs: Vec<String> = TEMPLATES
        .iter()
        .map(|t| format!("{}={}", t.code, t.full))
        .collect();
    format!("INSTRUCTION CODES:\n  {}", pairs.join(" | "))
}

/// Encode an instruction suffix using short codes with budget hints.
/// Response budget is dynamic based on task complexity to shape LLM output length.
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
        let block = instruction_decoder_block();
        for t in TEMPLATES {
            assert!(
                block.contains(t.code),
                "decoder should contain code {}",
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
    fn format_savings_returns_bracket_when_always() {
        super::MCP_CONTEXT.store(false, std::sync::atomic::Ordering::Relaxed);
        std::env::set_var("LEAN_CTX_SAVINGS_FOOTER", "always");
        let s = super::format_savings(100, 50);
        assert!(
            s.contains("100\u{2192}50 tok"),
            "expected unified format, got: {s}"
        );
        assert!(s.contains("-50%"), "expected percentage, got: {s}");
    }

    #[test]
    fn format_savings_returns_empty_when_never() {
        std::env::set_var("LEAN_CTX_SAVINGS_FOOTER", "never");
        let s = super::format_savings(100, 50);
        assert!(
            s.is_empty(),
            "expected empty string with never mode, got: {s}"
        );
    }

    #[test]
    fn format_savings_suppressed_in_mcp_auto_mode() {
        super::MCP_CONTEXT.store(true, std::sync::atomic::Ordering::Relaxed);
        std::env::set_var("LEAN_CTX_SAVINGS_FOOTER", "auto");
        let s = super::format_savings(100, 50);
        assert!(s.is_empty(), "expected empty in MCP+auto, got: {s}");
        super::MCP_CONTEXT.store(false, std::sync::atomic::Ordering::Relaxed);
    }

    #[test]
    fn append_savings_no_trailing_newline_when_suppressed() {
        std::env::set_var("LEAN_CTX_SAVINGS_FOOTER", "never");
        let result = super::append_savings("hello", 100, 50);
        assert_eq!(result, "hello");
    }
}
