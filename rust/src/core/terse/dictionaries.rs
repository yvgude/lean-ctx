//! Domain-specific abbreviation dictionaries for terse compression.
//!
//! Each dictionary provides whole-word-matching abbreviations for a specific
//! domain (git, cargo, npm, general). Unlike the legacy ABBREVIATIONS list
//! (18 blind substring replacements), these use word-boundary-aware matching.

/// A single abbreviation rule: replaces `long` with `short` at word boundaries.
pub struct Abbreviation {
    pub long: &'static str,
    pub short: &'static str,
}

pub const GENERAL: &[Abbreviation] = &[
    Abbreviation {
        long: "function",
        short: "fn",
    },
    Abbreviation {
        long: "configuration",
        short: "cfg",
    },
    Abbreviation {
        long: "implementation",
        short: "impl",
    },
    Abbreviation {
        long: "dependencies",
        short: "deps",
    },
    Abbreviation {
        long: "dependency",
        short: "dep",
    },
    Abbreviation {
        long: "request",
        short: "req",
    },
    Abbreviation {
        long: "response",
        short: "res",
    },
    Abbreviation {
        long: "context",
        short: "ctx",
    },
    Abbreviation {
        long: "error",
        short: "err",
    },
    Abbreviation {
        long: "return",
        short: "ret",
    },
    Abbreviation {
        long: "argument",
        short: "arg",
    },
    Abbreviation {
        long: "value",
        short: "val",
    },
    Abbreviation {
        long: "module",
        short: "mod",
    },
    Abbreviation {
        long: "package",
        short: "pkg",
    },
    Abbreviation {
        long: "directory",
        short: "dir",
    },
    Abbreviation {
        long: "parameter",
        short: "param",
    },
    Abbreviation {
        long: "variable",
        short: "var",
    },
    Abbreviation {
        long: "information",
        short: "info",
    },
    Abbreviation {
        long: "application",
        short: "app",
    },
    Abbreviation {
        long: "environment",
        short: "env",
    },
    Abbreviation {
        long: "repository",
        short: "repo",
    },
    Abbreviation {
        long: "authentication",
        short: "auth",
    },
    Abbreviation {
        long: "authorization",
        short: "authz",
    },
    Abbreviation {
        long: "description",
        short: "desc",
    },
    Abbreviation {
        long: "development",
        short: "dev",
    },
    Abbreviation {
        long: "production",
        short: "prod",
    },
    Abbreviation {
        long: "connection",
        short: "conn",
    },
    Abbreviation {
        long: "database",
        short: "db",
    },
    Abbreviation {
        long: "temporary",
        short: "tmp",
    },
    Abbreviation {
        long: "document",
        short: "doc",
    },
    Abbreviation {
        long: "maximum",
        short: "max",
    },
    Abbreviation {
        long: "minimum",
        short: "min",
    },
    Abbreviation {
        long: "number",
        short: "num",
    },
    Abbreviation {
        long: "reference",
        short: "ref",
    },
    Abbreviation {
        long: "string",
        short: "str",
    },
    Abbreviation {
        long: "message",
        short: "msg",
    },
    Abbreviation {
        long: "command",
        short: "cmd",
    },
    Abbreviation {
        long: "expression",
        short: "expr",
    },
    Abbreviation {
        long: "iteration",
        short: "iter",
    },
    Abbreviation {
        long: "previous",
        short: "prev",
    },
    Abbreviation {
        long: "current",
        short: "cur",
    },
    Abbreviation {
        long: "original",
        short: "orig",
    },
    Abbreviation {
        long: "destination",
        short: "dst",
    },
    Abbreviation {
        long: "source",
        short: "src",
    },
    Abbreviation {
        long: "attribute",
        short: "attr",
    },
    Abbreviation {
        long: "allocation",
        short: "alloc",
    },
    Abbreviation {
        long: "generation",
        short: "gen",
    },
    Abbreviation {
        long: "specification",
        short: "spec",
    },
    Abbreviation {
        long: "initialization",
        short: "init",
    },
    Abbreviation {
        long: "operation",
        short: "op",
    },
    Abbreviation {
        long: "optional",
        short: "opt",
    },
    Abbreviation {
        long: "utility",
        short: "util",
    },
    Abbreviation {
        long: "execution",
        short: "exec",
    },
    Abbreviation {
        long: "property",
        short: "prop",
    },
    Abbreviation {
        long: "statistics",
        short: "stats",
    },
    Abbreviation {
        long: "accumulator",
        short: "acc",
    },
    Abbreviation {
        long: "synchronize",
        short: "sync",
    },
    Abbreviation {
        long: "asynchronous",
        short: "async",
    },
    Abbreviation {
        long: "certificate",
        short: "cert",
    },
    Abbreviation {
        long: "identifier",
        short: "id",
    },
];

pub const GIT: &[Abbreviation] = &[
    Abbreviation {
        long: "modified",
        short: "M",
    },
    Abbreviation {
        long: "deleted",
        short: "D",
    },
    Abbreviation {
        long: "untracked",
        short: "?",
    },
    Abbreviation {
        long: "renamed",
        short: "R",
    },
    Abbreviation {
        long: "copied",
        short: "C",
    },
    Abbreviation {
        long: "insertion",
        short: "+",
    },
    Abbreviation {
        long: "deletion",
        short: "-",
    },
    Abbreviation {
        long: "upstream",
        short: "u/",
    },
    Abbreviation {
        long: "origin",
        short: "o/",
    },
    Abbreviation {
        long: "detached",
        short: "det",
    },
    Abbreviation {
        long: "conflict",
        short: "!!",
    },
    Abbreviation {
        long: "changes not staged for commit",
        short: "unstaged",
    },
    Abbreviation {
        long: "Changes to be committed",
        short: "staged",
    },
    Abbreviation {
        long: "nothing to commit, working tree clean",
        short: "clean",
    },
];

pub const CARGO: &[Abbreviation] = &[
    Abbreviation {
        long: "Compiling",
        short: "CC",
    },
    Abbreviation {
        long: "Downloading",
        short: "DL",
    },
    Abbreviation {
        long: "Downloaded",
        short: "DL'd",
    },
    Abbreviation {
        long: "Finished",
        short: "OK",
    },
    Abbreviation {
        long: "warning",
        short: "W",
    },
    Abbreviation {
        long: "test result: ok",
        short: "PASS",
    },
    Abbreviation {
        long: "test result: FAILED",
        short: "FAIL",
    },
    Abbreviation {
        long: "running",
        short: "run",
    },
    Abbreviation {
        long: "Blocking waiting for file lock on package cache",
        short: "LOCK",
    },
    Abbreviation {
        long: "Updating crates.io index",
        short: "IDX",
    },
    Abbreviation {
        long: "target/debug",
        short: "t/d",
    },
    Abbreviation {
        long: "target/release",
        short: "t/r",
    },
];

pub const NPM: &[Abbreviation] = &[
    Abbreviation {
        long: "added",
        short: "+",
    },
    Abbreviation {
        long: "removed",
        short: "-",
    },
    Abbreviation {
        long: "packages",
        short: "pkgs",
    },
    Abbreviation {
        long: "vulnerabilities",
        short: "vulns",
    },
    Abbreviation {
        long: "deprecated",
        short: "depr",
    },
    Abbreviation {
        long: "node_modules",
        short: "n_m",
    },
    Abbreviation {
        long: "devDependencies",
        short: "devDeps",
    },
    Abbreviation {
        long: "peerDependencies",
        short: "peerDeps",
    },
    Abbreviation {
        long: "optionalDependencies",
        short: "optDeps",
    },
    Abbreviation {
        long: "npm warn",
        short: "W",
    },
    Abbreviation {
        long: "npm error",
        short: "E",
    },
];

/// Applies whole-word abbreviations from the given dictionaries to the text.
/// Uses a single scan: first checks which patterns exist, then applies only matches.
#[must_use]
pub fn apply_dictionaries(text: &str, level: DictLevel) -> String {
    let dicts: Vec<&[Abbreviation]> = match level {
        DictLevel::General => vec![GENERAL],
        DictLevel::Full => vec![GENERAL, GIT, CARGO, NPM],
    };

    let mut result = text.to_string();
    for dict in dicts {
        for abbr in dict {
            result = replace_whole_word(&result, abbr.long, abbr.short);
        }
    }
    result
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum DictLevel {
    General,
    Full,
}

fn is_word_boundary(b: u8) -> bool {
    !b.is_ascii_alphanumeric() && b != b'-' && b != b'_' && b != b'\'' && b != b'"'
}

fn replace_whole_word(text: &str, pattern: &str, replacement: &str) -> String {
    if pattern.is_empty() {
        return text.to_string();
    }

    let pattern_lower = pattern.to_lowercase();
    let text_lower = text.to_lowercase();

    if !text_lower.contains(&pattern_lower) {
        return text.to_string();
    }

    let mut result = String::with_capacity(text.len());
    let mut start = 0;

    while let Some(pos) = text_lower[start..].find(&pattern_lower) {
        let abs_pos = start + pos;
        let end_pos = abs_pos + pattern.len();

        let before_ok = abs_pos == 0 || is_word_boundary(text.as_bytes()[abs_pos - 1]);
        let after_ok = end_pos >= text.len() || is_word_boundary(text.as_bytes()[end_pos]);

        result.push_str(&text[start..abs_pos]);

        if before_ok && after_ok {
            result.push_str(replacement);
        } else {
            result.push_str(&text[start + pos..end_pos]);
        }
        start = end_pos;
    }
    result.push_str(&text[start..]);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn whole_word_replaces_standalone() {
        let r = replace_whole_word("the function works", "function", "fn");
        assert_eq!(r, "the fn works");
    }

    #[test]
    fn whole_word_skips_substring() {
        let r = replace_whole_word("dysfunction", "function", "fn");
        assert_eq!(r, "dysfunction");
    }

    #[test]
    fn whole_word_at_start() {
        let r = replace_whole_word("function call", "function", "fn");
        assert_eq!(r, "fn call");
    }

    #[test]
    fn whole_word_at_end() {
        let r = replace_whole_word("call function", "function", "fn");
        assert_eq!(r, "call fn");
    }

    #[test]
    fn whole_word_with_punctuation() {
        let r = replace_whole_word("function(arg)", "function", "fn");
        assert_eq!(r, "fn(arg)");
    }

    #[test]
    fn general_dict_applies() {
        let r = apply_dictionaries("the configuration directory", DictLevel::General);
        assert!(r.contains("cfg"));
        assert!(r.contains("dir"));
    }

    #[test]
    fn full_dict_includes_domain() {
        let r = apply_dictionaries("Compiling lean-ctx", DictLevel::Full);
        assert!(r.contains("CC"), "cargo abbreviation should apply: {r}");
    }

    #[test]
    fn dict_count_general() {
        assert!(
            GENERAL.len() >= 60,
            "should have 60+ general abbreviations, got {}",
            GENERAL.len()
        );
    }

    #[test]
    fn dict_count_git() {
        assert!(
            GIT.len() >= 9,
            "should have 9+ git abbreviations, got {}",
            GIT.len()
        );
    }

    #[test]
    fn git_dict_never_abbreviates_subcommands() {
        let git_subcommands = [
            "commit", "branch", "checkout", "merge", "stash", "rebase", "push", "pull", "fetch",
            "clone", "tag", "reset", "bisect", "log", "diff", "show", "status", "add",
        ];
        for abbr in GIT {
            assert!(
                !git_subcommands.contains(&abbr.long),
                "GIT dictionary must NOT abbreviate git subcommand '{}' (→ '{}'). \
                 Agents will misinterpret abbreviated output as valid commands.",
                abbr.long,
                abbr.short
            );
        }
    }

    #[test]
    fn commit_word_survives_full_dict() {
        let text = "commit abc1234 on branch main";
        let result = apply_dictionaries(text, DictLevel::Full);
        assert!(
            result.contains("commit"),
            "word 'commit' must not be abbreviated in output: {result}"
        );
    }

    #[test]
    fn branch_word_survives_full_dict() {
        let text = "Your branch is ahead of 'origin/main' by 2 commits";
        let result = apply_dictionaries(text, DictLevel::Full);
        assert!(
            result.contains("branch"),
            "word 'branch' must not be abbreviated in output: {result}"
        );
    }
}
