use std::path::Path;

/// Finds the outermost project root by walking up from `file_path`.
/// For monorepos with nested `.git` dirs (e.g. `mono/backend/.git` + `mono/frontend/.git`),
/// returns the outermost ancestor containing `.git`, a workspace marker, or a known
/// monorepo config file — so the whole monorepo is treated as one project.
pub fn detect_project_root(file_path: &str) -> Option<String> {
    let mut dir = Path::new(file_path).parent()?;
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

pub fn detect_project_root_or_cwd(file_path: &str) -> String {
    detect_project_root(file_path).unwrap_or_else(|| {
        let p = Path::new(file_path);
        if p.exists() {
            if p.is_dir() {
                return file_path.to_string();
            }
            if let Some(parent) = p.parent() {
                return parent.to_string_lossy().to_string();
            }
            return file_path.to_string();
        }
        std::env::current_dir()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|_| ".".to_string())
    })
}

pub fn shorten_path(path: &str) -> String {
    let p = Path::new(path);
    if let Some(name) = p.file_name() {
        return name.to_string_lossy().to_string();
    }
    path.to_string()
}

pub fn format_savings(original: usize, compressed: usize) -> String {
    let saved = original.saturating_sub(compressed);
    if original == 0 {
        return "0 tok saved".to_string();
    }
    let pct = (saved as f64 / original as f64 * 100.0).round() as usize;
    format!("[{saved} tok saved ({pct}%)]")
}

/// Compresses tool output text based on density level.
/// - Normal: no changes
/// - Terse: strip blank lines, strip comment-only lines, remove banners
/// - Ultra: additionally abbreviate common words
pub fn compress_output(text: &str, density: &super::config::OutputDensity) -> String {
    use super::config::OutputDensity;
    match density {
        OutputDensity::Normal => text.to_string(),
        OutputDensity::Terse => compress_terse(text),
        OutputDensity::Ultra => compress_ultra(text),
    }
}

fn compress_terse(text: &str) -> String {
    text.lines()
        .filter(|line| {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                return false;
            }
            if is_comment_only(trimmed) {
                return false;
            }
            if is_banner_line(trimmed) {
                return false;
            }
            true
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn compress_ultra(text: &str) -> String {
    let terse = compress_terse(text);
    let mut result = terse;
    for (long, short) in ABBREVIATIONS {
        result = result.replace(long, short);
    }
    result
}

const ABBREVIATIONS: &[(&str, &str)] = &[
    ("function", "fn"),
    ("configuration", "cfg"),
    ("implementation", "impl"),
    ("dependencies", "deps"),
    ("dependency", "dep"),
    ("request", "req"),
    ("response", "res"),
    ("context", "ctx"),
    ("error", "err"),
    ("return", "ret"),
    ("argument", "arg"),
    ("value", "val"),
    ("module", "mod"),
    ("package", "pkg"),
    ("directory", "dir"),
    ("parameter", "param"),
    ("variable", "var"),
];

fn is_comment_only(line: &str) -> bool {
    line.starts_with("//")
        || line.starts_with('#')
        || line.starts_with("--")
        || (line.starts_with("/*") && line.ends_with("*/"))
}

fn is_banner_line(line: &str) -> bool {
    if line.len() < 4 {
        return false;
    }
    let chars: Vec<char> = line.chars().collect();
    let first = chars[0];
    if matches!(first, '=' | '-' | '*' | '─' | '━' | '▀' | '▄') {
        let same_count = chars.iter().filter(|c| **c == first).count();
        return same_count as f64 / chars.len() as f64 > 0.7;
    }
    false
}

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
            root_path.canonicalize().ok(),
            base.canonicalize().ok(),
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
}
