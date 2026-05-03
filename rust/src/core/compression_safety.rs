use std::fmt;

/// How aggressively a command's output may be compressed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SafetyLevel {
    Verbatim,
    Minimal,
    Standard,
    Aggressive,
}

impl fmt::Display for SafetyLevel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SafetyLevel::Verbatim => write!(f, "verbatim"),
            SafetyLevel::Minimal => write!(f, "minimal"),
            SafetyLevel::Standard => write!(f, "standard"),
            SafetyLevel::Aggressive => write!(f, "aggressive"),
        }
    }
}

/// Maps a CLI command to its compression safety level and rationale.
pub struct CommandSafety {
    pub command: &'static str,
    pub level: SafetyLevel,
    pub description: &'static str,
}

/// Static lookup table of per-command compression safety levels.
pub const COMMAND_SAFETY_TABLE: &[CommandSafety] = &[
    // --- Verbatim: output passes through unchanged ---
    CommandSafety {
        command: "df",
        level: SafetyLevel::Verbatim,
        description: "Disk usage — root filesystem must never be hidden",
    },
    CommandSafety {
        command: "git status",
        level: SafetyLevel::Verbatim,
        description: "DETACHED HEAD, staged/unstaged lists preserved verbatim",
    },
    CommandSafety {
        command: "git stash",
        level: SafetyLevel::Verbatim,
        description: "Stash save/pop/list output preserved verbatim",
    },
    CommandSafety {
        command: "ls",
        level: SafetyLevel::Verbatim,
        description: "All files shown including .env, dotfiles",
    },
    CommandSafety {
        command: "find",
        level: SafetyLevel::Verbatim,
        description: "Full absolute paths preserved",
    },
    CommandSafety {
        command: "wc",
        level: SafetyLevel::Verbatim,
        description: "Pipe/stdin input handled correctly",
    },
    CommandSafety {
        command: "env/printenv",
        level: SafetyLevel::Verbatim,
        description: "Environment variables preserved (values filtered)",
    },
    // --- Minimal: light formatting, all critical data preserved ---
    CommandSafety {
        command: "git diff",
        level: SafetyLevel::Minimal,
        description: "All +/- lines preserved, only index headers and excess context trimmed",
    },
    CommandSafety {
        command: "git log",
        level: SafetyLevel::Minimal,
        description: "Up to 50 entries, respects --max-count/-n, shows truncation notice",
    },
    CommandSafety {
        command: "git blame",
        level: SafetyLevel::Minimal,
        description: "Verbatim up to 100 lines, then author/line-range summary",
    },
    CommandSafety {
        command: "docker ps",
        level: SafetyLevel::Minimal,
        description: "Header-parsed columns; (unhealthy), Exited status always preserved",
    },
    CommandSafety {
        command: "grep/rg",
        level: SafetyLevel::Minimal,
        description: "Verbatim up to 100 lines, then grouped by file with line numbers",
    },
    CommandSafety {
        command: "ruff check",
        level: SafetyLevel::Minimal,
        description: "Verbatim up to 30 issues (file:line:col preserved), then summary",
    },
    CommandSafety {
        command: "npm audit",
        level: SafetyLevel::Minimal,
        description: "CVE IDs, severity, package names, fix recommendations preserved",
    },
    CommandSafety {
        command: "pip list",
        level: SafetyLevel::Minimal,
        description: "All packages shown (no truncation)",
    },
    CommandSafety {
        command: "pip uninstall",
        level: SafetyLevel::Minimal,
        description: "All removed package names listed",
    },
    CommandSafety {
        command: "pytest",
        level: SafetyLevel::Minimal,
        description: "passed/failed/skipped/xfailed/xpassed/warnings all counted",
    },
    CommandSafety {
        command: "docker logs",
        level: SafetyLevel::Minimal,
        description: "Dedup + safety-needle scan preserves FATAL/ERROR/CRITICAL lines",
    },
    CommandSafety {
        command: "cat (logs)",
        level: SafetyLevel::Minimal,
        description: "Log dedup preserves all severity levels including CRITICAL",
    },
    // --- Standard: structured compression, key info preserved ---
    CommandSafety {
        command: "cargo build/test",
        level: SafetyLevel::Standard,
        description: "Errors and warnings preserved, progress lines removed",
    },
    CommandSafety {
        command: "npm install",
        level: SafetyLevel::Standard,
        description: "Package count, vulnerability summary preserved",
    },
    CommandSafety {
        command: "docker build",
        level: SafetyLevel::Standard,
        description: "Step count, errors preserved, intermediate output removed",
    },
    CommandSafety {
        command: "git commit",
        level: SafetyLevel::Standard,
        description: "Branch, hash, change stats preserved; hook output kept",
    },
    CommandSafety {
        command: "git push/pull",
        level: SafetyLevel::Standard,
        description: "Remote, branch, conflict info preserved",
    },
    CommandSafety {
        command: "eslint/biome",
        level: SafetyLevel::Standard,
        description: "Error/warning counts, file references preserved",
    },
    CommandSafety {
        command: "tsc",
        level: SafetyLevel::Standard,
        description: "Type errors with file:line preserved",
    },
    CommandSafety {
        command: "curl (JSON)",
        level: SafetyLevel::Standard,
        description: "Schema extraction; sensitive keys (token/password/secret) REDACTED",
    },
    // --- Aggressive: heavy compression for verbose output ---
    CommandSafety {
        command: "kubectl describe",
        level: SafetyLevel::Aggressive,
        description: "Key fields extracted, verbose event history trimmed",
    },
    CommandSafety {
        command: "aws CLI",
        level: SafetyLevel::Aggressive,
        description: "JSON schema extraction for large API responses",
    },
    CommandSafety {
        command: "terraform plan",
        level: SafetyLevel::Aggressive,
        description: "Resource changes summarized, full plan truncated",
    },
    CommandSafety {
        command: "docker images",
        level: SafetyLevel::Aggressive,
        description: "Compressed to name:tag (size) list",
    },
];

/// Renders the full safety table as a human-readable report.
pub fn format_safety_table() -> String {
    let mut out = String::new();
    out.push_str("Command Compression Safety Levels\n");
    out.push_str(&"=".repeat(72));
    out.push('\n');
    out.push('\n');

    for level in &[
        SafetyLevel::Verbatim,
        SafetyLevel::Minimal,
        SafetyLevel::Standard,
        SafetyLevel::Aggressive,
    ] {
        let label = match level {
            SafetyLevel::Verbatim => "VERBATIM — output passes through unchanged",
            SafetyLevel::Minimal => {
                "MINIMAL — light formatting, all safety-critical data preserved"
            }
            SafetyLevel::Standard => "STANDARD — structured compression, key info preserved",
            SafetyLevel::Aggressive => "AGGRESSIVE — heavy compression for verbose output",
        };
        out.push_str(&format!("[{label}]\n"));

        for entry in COMMAND_SAFETY_TABLE.iter().filter(|e| e.level == *level) {
            out.push_str(&format!("  {:<20} {}\n", entry.command, entry.description));
        }
        out.push('\n');
    }

    out.push_str("Safety features active on ALL commands:\n");
    out.push_str("  • Safety-needle scan: CRITICAL/FATAL/panic/ERROR/CVE lines preserved\n");
    out.push_str("  • Safeguard ratio: >95% compression on >100 tokens triggers fallback\n");
    out.push_str("  • Auth flow detection: login/OAuth prompts never compressed\n");
    out.push_str("  • Minimum token threshold: outputs <50 tokens pass through unchanged\n");
    out.push('\n');
    out.push_str("Use `lean-ctx bypass \"command\"` to run any command with zero compression.\n");

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safety_table_has_entries() {
        assert!(COMMAND_SAFETY_TABLE.len() > 20);
    }

    #[test]
    fn format_table_contains_all_levels() {
        let table = format_safety_table();
        assert!(table.contains("VERBATIM"));
        assert!(table.contains("MINIMAL"));
        assert!(table.contains("STANDARD"));
        assert!(table.contains("AGGRESSIVE"));
    }

    #[test]
    fn df_is_verbatim() {
        let df = COMMAND_SAFETY_TABLE
            .iter()
            .find(|e| e.command == "df")
            .unwrap();
        assert_eq!(df.level, SafetyLevel::Verbatim);
    }

    #[test]
    fn git_diff_is_minimal() {
        let diff = COMMAND_SAFETY_TABLE
            .iter()
            .find(|e| e.command == "git diff")
            .unwrap();
        assert_eq!(diff.level, SafetyLevel::Minimal);
    }
}
