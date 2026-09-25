//! Search and directory-listing command rewriting.
//!
//! Extracted from `hook_handlers/mod.rs` (#660 LOC gate) to keep the main
//! module under the 1500-line budget. All `rewrite_*` functions plus shared
//! helpers (`shell_tokenize`, `shell_quote`) live here.

use super::file_rewrite::is_outside_project_path;

/// Rewrites `grep`/`egrep`/`fgrep`/`rg` (and PowerShell `Select-String`/`sls`,
/// #561) to `lean-ctx grep <pattern> [path]` when the invocation is simple enough
/// to map losslessly; complex flag combos fall through to the `lean-ctx -c` wrap.
pub(super) fn rewrite_search_command(cmd: &str, binary: &str) -> Option<String> {
    let parts = shell_tokenize(cmd);
    #[allow(clippy::match_same_arms)]
    match parts.first().map(String::as_str) {
        // fgrep uses fixed-string matching; lean-ctx grep is regex-only → always -c wrap
        Some("fgrep") => None,
        Some("grep") => rewrite_grep(&parts, binary, PatternDialect::Basic),
        Some("egrep") => rewrite_grep(&parts, binary, PatternDialect::Extended),
        Some("rg") => rewrite_rg(&parts, binary),
        Some("Select-String" | "sls") => rewrite_select_string(&parts, binary),
        _ => None,
    }
}

/// Flags that are purely cosmetic and safe to strip: lean-ctx grep always shows
/// line numbers, filenames, and searches recursively. Flags that change search
/// semantics (-i, -w, -l, -c, -F, --include/--exclude) are NOT here — they
/// cause fall-through to `lean-ctx -c` for correct native grep behavior.
const GREP_SAFE_FLAGS: &[&str] = &[
    "-n",
    "--line-number",
    "-r",
    "-R",
    "--recursive",
    "-H",
    "--with-filename",
    "-s",
    "--no-messages",
    "--color=auto",
    "--color=always",
    "--color=never",
    "--color",
];

/// Flags that take a value argument (next token is consumed as value).
/// Only context flags are here — they cause fall-through to `-c` wrap.
/// All other value-carrying flags (--include, -m, etc.) are unknown → fall-through.
const GREP_VALUE_FLAGS: &[&str] = &[
    "-A",
    "--after-context",
    "-B",
    "--before-context",
    "-C",
    "--context",
];

/// Which regex dialect the invoked binary applies to its pattern.
///
/// `lean-ctx grep` compiles patterns with the Rust `regex` crate, whose syntax
/// matches POSIX *extended* regular expressions closely enough to hand a
/// pattern over verbatim. POSIX *basic* regular expressions — what plain
/// `grep` uses — invert the meaning of seven metacharacters, so the same
/// pattern text means something different on each side.
#[derive(Clone, Copy, PartialEq, Eq)]
enum PatternDialect {
    /// Plain `grep`: POSIX BRE, where `\|` alternates and a bare `|` is literal.
    Basic,
    /// `egrep`: POSIX ERE, which the Rust `regex` crate accepts as-is.
    Extended,
}

/// Metacharacters whose meaning flips between POSIX BRE and the Rust `regex`
/// crate — in *both* directions.
///
/// In BRE the escaped form is the operator and the bare form is a literal;
/// `regex` reads it the other way round. Handing such a pattern to
/// `lean-ctx grep` verbatim therefore does not fail loudly, it silently
/// answers a different question (#1827):
///
/// * `grep 'a\|b'`  — BRE alternation, read by `regex` as the literal `a|b`
///   → a real match is reported as `0 matches` (the reported symptom).
/// * `grep 'a|b'`   — BRE literal `a|b`, read by `regex` as alternation
///   → lines that native grep never matches are reported as hits.
///
/// Both are wrong answers that look like right ones, which is worse than an
/// error. Declining the rewrite sends the command through the `lean-ctx -c`
/// wrap instead, where the platform's own grep resolves the pattern — the same
/// escape valve `fgrep` and the semantic flags (`-i`, `-w`, `-F`, …) already use.
const BRE_AMBIGUOUS_METACHARS: &[char] = &['|', '+', '?', '(', ')', '{', '}'];

/// True when `pattern` cannot be handed to `lean-ctx grep` without changing
/// what it means.
fn dialect_is_ambiguous(pattern: &str, dialect: PatternDialect) -> bool {
    // ERE and the `regex` crate agree on these seven, escaped and bare alike.
    if dialect == PatternDialect::Extended {
        return false;
    }
    pattern
        .chars()
        .any(|c| BRE_AMBIGUOUS_METACHARS.contains(&c))
}

/// Rewrites `grep [-nirlcwHRs] [--include=...] <pattern> [path...]` to
/// `lean-ctx grep <pattern> [path]`. Complex invocations (pipes as stdin,
/// unsupported flags, multiple paths) fall through to the `lean-ctx -c` wrap
/// via the `is_rewritable` fallback.
fn rewrite_grep(parts: &[String], binary: &str, dialect: PatternDialect) -> Option<String> {
    let mut pattern: Option<String> = None;
    let mut path: Option<String> = None;
    let mut has_context_flags = false;
    let mut i = 1;

    while i < parts.len() {
        let arg = &parts[i];

        if arg == "--" {
            i += 1;
            continue;
        }

        // Combined short flags like -rn: validate each char is safe to strip
        if arg.starts_with('-') && !arg.starts_with("--") && arg.len() > 2 {
            let chars = &arg[1..];
            if chars.chars().all(|c| "nrRHs".contains(c)) {
                i += 1;
                continue;
            }
            return None;
        }

        // --flag=value style
        if arg.starts_with("--") && arg.contains('=') {
            let flag_name = arg.split('=').next().unwrap_or("");
            if GREP_SAFE_FLAGS.contains(&flag_name) || GREP_VALUE_FLAGS.contains(&flag_name) {
                i += 1;
                continue;
            }
            return None;
        }

        // Known flags (with or without value)
        if arg.starts_with('-') {
            if GREP_VALUE_FLAGS.contains(&arg.as_str()) {
                has_context_flags |= matches!(
                    arg.as_str(),
                    "-A" | "-B" | "-C" | "--after-context" | "--before-context" | "--context"
                );
                i += 2;
                continue;
            }
            if GREP_SAFE_FLAGS.contains(&arg.as_str()) {
                i += 1;
                continue;
            }
            return None;
        }

        if pattern.is_none() {
            pattern = Some(arg.clone());
        } else if path.is_none() {
            path = Some(arg.clone());
        } else {
            return None;
        }
        i += 1;
    }

    let pattern = pattern?;

    // A BRE pattern whose meaning would change under the Rust `regex` crate
    // must not be handed to `lean-ctx grep` — see `BRE_AMBIGUOUS_METACHARS`.
    if dialect_is_ambiguous(&pattern, dialect) {
        return None;
    }

    if has_context_flags {
        return None;
    }

    match &path {
        Some(p) if is_outside_project_path(p) => None,
        Some(p) => Some(format!(
            "{binary} grep {} {}",
            shell_quote(&pattern),
            shell_quote(p)
        )),
        None => Some(format!("{binary} grep {}", shell_quote(&pattern))),
    }
}

/// Rewrites `rg [flags] <pattern> [path]` to `lean-ctx grep <pattern> [path]`.
/// Supports common flags that don't alter the fundamental search semantics.
fn rewrite_rg(parts: &[String], binary: &str) -> Option<String> {
    if parts.len() < 2 {
        return None;
    }

    const RG_SAFE_SHORT: &str = "nsSHu";
    const RG_SAFE_LONG: &[&str] = &[
        "--line-number",
        "--no-ignore",
        "--hidden",
        "--no-heading",
        "--with-filename",
        "--follow",
        "--unrestricted",
        "--color=auto",
        "--color=always",
        "--color=never",
        "--color=ansi",
        "--no-line-number",
    ];
    const RG_VALUE_FLAGS: &[&str] = &[
        "-A",
        "--after-context",
        "-B",
        "--before-context",
        "-C",
        "--context",
    ];

    let mut pattern: Option<String> = None;
    let mut path: Option<String> = None;
    let mut has_context_flags = false;
    let mut i = 1;

    while i < parts.len() {
        let arg = &parts[i];

        if arg == "--" {
            i += 1;
            continue;
        }

        if arg.starts_with("--") && arg.contains('=') {
            let flag_name = arg.split('=').next().unwrap_or("");
            if RG_SAFE_LONG.contains(&arg.as_str())
                || RG_SAFE_LONG.contains(&flag_name)
                || RG_VALUE_FLAGS.contains(&flag_name)
            {
                i += 1;
                continue;
            }
            return None;
        }

        if arg.starts_with("--") {
            if RG_SAFE_LONG.contains(&arg.as_str()) {
                i += 1;
                continue;
            }
            if RG_VALUE_FLAGS.contains(&arg.as_str()) {
                has_context_flags |= matches!(
                    arg.as_str(),
                    "--after-context" | "--before-context" | "--context"
                );
                i += 2;
                continue;
            }
            return None;
        }

        if arg.starts_with('-') && arg.len() >= 2 {
            let flag_str = &arg[..2];
            if RG_VALUE_FLAGS.contains(&flag_str) {
                has_context_flags |= matches!(flag_str, "-A" | "-B" | "-C");
                if arg.len() > 2 {
                    i += 1;
                } else {
                    i += 2;
                }
                continue;
            }
            let chars = &arg[1..];
            if chars.chars().all(|c| RG_SAFE_SHORT.contains(c)) {
                i += 1;
                continue;
            }
            return None;
        }

        if pattern.is_none() {
            pattern = Some(arg.clone());
        } else if path.is_none() {
            path = Some(arg.clone());
        } else {
            return None;
        }
        i += 1;
    }

    let pattern = pattern?;

    if has_context_flags {
        return None;
    }

    match &path {
        Some(p) if is_outside_project_path(p) => None,
        Some(p) => Some(format!(
            "{binary} grep {} {}",
            shell_quote(&pattern),
            shell_quote(p)
        )),
        None => Some(format!("{binary} grep {}", shell_quote(&pattern))),
    }
}

/// Maps `Select-String`/`sls` to `lean-ctx grep`, honoring `-Pattern` and
/// `-Path`/`-LiteralPath` plus the positional `<pattern> [path]` form.
fn rewrite_select_string(parts: &[String], binary: &str) -> Option<String> {
    let mut pattern: Option<String> = None;
    let mut path: Option<String> = None;
    let mut i = 1;
    while i < parts.len() {
        if let Some(flag) = parts[i].strip_prefix('-') {
            let value = parts.get(i + 1);
            match flag.to_ascii_lowercase().as_str() {
                "pattern" => pattern = Some(value?.clone()),
                "path" | "literalpath" => path = Some(value?.clone()),
                _ => return None,
            }
            i += 2;
        } else if pattern.is_none() {
            pattern = Some(parts[i].clone());
            i += 1;
        } else if path.is_none() {
            path = Some(parts[i].clone());
            i += 1;
        } else {
            return None;
        }
    }
    let pattern = shell_quote(&pattern?);
    match path {
        Some(p) if is_outside_project_path(&p) => None,
        Some(p) => Some(format!("{binary} grep {pattern} {}", shell_quote(&p))),
        None => Some(format!("{binary} grep {pattern}")),
    }
}

/// Rewrites simple `ls [path]` (and PowerShell `Get-ChildItem`/`gci`, #561) to
/// `lean-ctx ls [path]`.
pub(super) fn rewrite_dir_list_command(cmd: &str, binary: &str) -> Option<String> {
    let parts = shell_tokenize(cmd);
    match parts.first().map(String::as_str) {
        Some("ls") => match parts.len() {
            1 => Some(format!("{binary} ls")),
            2 if !parts[1].starts_with('-') => {
                Some(format!("{binary} ls {}", shell_quote(&parts[1])))
            }
            _ => None,
        },
        Some("Get-ChildItem" | "gci") => rewrite_get_childitem(&parts, binary),
        _ => None,
    }
}

fn rewrite_get_childitem(parts: &[String], binary: &str) -> Option<String> {
    let mut path: Option<String> = None;
    let mut i = 1;
    while i < parts.len() {
        if let Some(flag) = parts[i].strip_prefix('-') {
            let value = parts.get(i + 1);
            match flag.to_ascii_lowercase().as_str() {
                "path" | "literalpath" => path = Some(value?.clone()),
                _ => return None,
            }
            i += 2;
        } else if path.is_none() {
            path = Some(parts[i].clone());
            i += 1;
        } else {
            return None;
        }
    }
    match path {
        Some(p) => Some(format!("{binary} ls {}", shell_quote(&p))),
        None => Some(format!("{binary} ls")),
    }
}

/// Tokenize a shell command respecting single/double quotes and backslash escapes.
pub fn shell_tokenize(input: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut chars = input.chars().peekable();
    let mut in_single = false;
    let mut in_double = false;

    while let Some(c) = chars.next() {
        match c {
            '\'' if !in_double => in_single = !in_single,
            '"' if !in_single => in_double = !in_double,
            '\\' if !in_single => {
                if let Some(&next) = chars.peek() {
                    if in_double {
                        // POSIX: inside double quotes only \\ \" \$ \` \newline
                        // are special. Everything else (including Windows path
                        // separators like \U \f) keeps the backslash (#1038).
                        if matches!(next, '\\' | '"' | '$' | '`' | '\n') {
                            chars.next();
                            current.push(next);
                        } else {
                            current.push('\\');
                        }
                    } else {
                        chars.next();
                        current.push(next);
                    }
                }
            }
            c if c.is_whitespace() && !in_single && !in_double => {
                if !current.is_empty() {
                    tokens.push(std::mem::take(&mut current));
                }
            }
            _ => current.push(c),
        }
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    tokens
}

/// Quote a path/arg for the POSIX shell that runs the rewritten command if it
/// contains spaces or special chars.
///
/// Single quotes, never double: the word reaches us already unquoted, so a
/// literal `$HOME` or `` `id` `` the agent single-quoted must stay literal.
/// Double quotes would let the calling shell expand or execute it (#1862).
pub fn shell_quote(s: &str) -> String {
    if s.contains(|c: char| {
        c.is_whitespace()
            || matches!(
                c,
                '\'' | '"'
                    | '\\'
                    | '|'
                    | '&'
                    | ';'
                    | '$'
                    | '`'
                    | '('
                    | ')'
                    | '*'
                    | '?'
                    | '>'
                    | '<'
                    | '#'
                    | '!'
                    | '['
                    | ']'
                    | '{'
                    | '}'
                    | '~'
            )
    }) {
        format!("'{}'", s.replace('\'', "'\\''"))
    } else {
        s.to_string()
    }
}
