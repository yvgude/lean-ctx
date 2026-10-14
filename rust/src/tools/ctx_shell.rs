use crate::tools::CrpMode;

const MAX_COMMAND_BYTES: usize = 8192;

/// Validates a shell command before execution. Returns Some(error_message) if
/// the command should be rejected, None if it's safe to run.
pub fn validate_command(command: &str) -> Option<String> {
    let write_allow_paths = crate::core::config::default_shell_write_allow_paths();
    let project_root = crate::core::config::Config::find_project_root();
    validate_command_with_write_allow_paths(command, &write_allow_paths, project_root.as_deref())
}

/// Validate without knowing where the command will run.
///
/// A relative redirect target cannot be placed, so it is refused — the
/// conservative reading. Callers that know the working directory should use
/// [`validate_command_in_cwd`] instead, which resolves the destination first.
pub(crate) fn validate_command_with_write_allow_paths(
    command: &str,
    write_allow_paths: &[String],
    project_root: Option<&str>,
) -> Option<String> {
    validate_command_in_cwd(command, write_allow_paths, project_root, None)
}

/// The capture rule as the guard applies it, shared by the redirect and `tee`
/// refusals so the two cannot drift apart. #1946: both used to call the rule
/// "output capture into a project path", but a destination outside the
/// project — and outside every root — is refused just the same. A caller who
/// took that wording at its word tried another non-project path and was
/// refused again with the identical text.
const CAPTURE_RULE: &str = "The rule: output capture (`>`, `>>`, `| tee`) may go to the \
     session's project, a directory in `allow_paths` / `extra_roots`, a directory listed in \
     `write_allow_paths` in config.toml, or a scratch path (/tmp, /var/tmp, $TMPDIR). Every \
     other destination is refused. Commands that do not capture output are not restricted.";

/// Directories ctx_shell output capture may write besides `write_allow_paths`
/// and the scratch paths: the session's project, its host-declared roots and
/// the jail's explicit allow entries — the places lean-ctx's own edit tools may
/// write (owner decision 2026-10-08: refusing `> out.txt` inside the project
/// while `allow_paths` was ignored read as a bug). Broad roots (`/`, `~`, temp
/// parents) and anything under `read_only_roots` are never included.
pub(crate) fn capture_roots(
    project_root: &str,
    session_extra_roots: &[String],
    cfg: &crate::core::config::Config,
) -> Vec<String> {
    std::iter::once(project_root.to_string())
        .chain(session_extra_roots.iter().cloned())
        .chain(cfg.allow_paths.iter().cloned())
        .chain(cfg.extra_roots.iter().cloned())
        .filter(|raw| !raw.trim().is_empty())
        .map(|raw| crate::core::pathjail::expand_user_path(&raw))
        .filter(|path| {
            let contains_home = dirs::home_dir().is_some_and(|home| home.starts_with(path));
            path.is_absolute()
                && !contains_home
                && !crate::core::pathutil::is_broad_or_unsafe_root(path)
                && !crate::core::pathjail::is_read_only_path(path)
        })
        .map(|path| path.to_string_lossy().into_owned())
        .collect()
}

/// Why the capture rule exists, shared by every write refusal (#1992). The
/// refusals used to say ctx_shell's compression could land in the file as
/// markers. That stopped being true with #1303 (v3.9.14): the child shell
/// performs the redirect, so the file receives the command's own bytes and
/// only the MCP return channel is compressed. A caller who knew their output
/// was tiny plain text read the old rationale as a risk that did not apply to
/// them and retried with `raw=true` or a smaller command — neither changes
/// the verdict, because the reason is the write path, not the bytes.
const WRITE_REASON: &str = "Files you keep are written with ctx_patch or the native Write/Edit \
     tool, so the read-before-write gate and the edit checks apply to them; ctx_shell does not \
     write them through output capture. This is unconditional: the redirect itself is not \
     compressed, so a smaller command or `raw=true` does not change the verdict.";

/// Validate against the directory the command will actually run in (#1811).
///
/// The write guard classifies the *destination*, so a relative redirect target
/// has to be placed before it can be judged. Without a `cwd` it cannot be, and
/// every relative target was refused — which made `cwd=<scratch> … > probe.txt`
/// fail while the identical absolute path succeeded, under a message promising
/// that the destination decides.
pub(crate) fn validate_command_in_cwd(
    command: &str,
    write_allow_paths: &[String],
    project_root: Option<&str>,
    cwd: Option<&str>,
) -> Option<String> {
    if command.len() > MAX_COMMAND_BYTES {
        return Some(format!(
            "ERROR: Command too large ({} bytes, limit {}). \
             If you're writing file content, use the native Write/Edit tool instead. \
             ctx_shell is for reading command output only (git, cargo, npm, etc.).",
            command.len(),
            MAX_COMMAND_BYTES
        ));
    }

    // #931: strip heredoc bodies before the redirect scanner — a `>` inside a
    // heredoc body is opaque data, not a file-write redirect.
    let cmd_no_heredoc = crate::core::shell_allowlist::strip_all_heredoc_bodies(command);
    // #1850: a relative target lands where its own command runs, and a `cd`
    // earlier in the line moves that. Judging every target against the call's
    // `cwd` blocked `cd /tmp && echo x > out.txt` and let `cd <project> && echo
    // x > f` from a scratch cwd write straight into the project. Each segment
    // is judged against its own directory; where that cannot be known, a
    // relative target is refused.
    let segments = segments_in_cwd(&cmd_no_heredoc, cwd);
    // #1768: the rule is the destination, not the size of the payload. The
    // refusal used to justify itself with "MCP protocol corruption on large
    // payloads" while the guard allows a megabyte into /tmp and blocks two
    // bytes into a project file — so the stated reason argued *against* the
    // verdict in both directions, and steered callers to retry smaller (no
    // effect) or to avoid the sanctioned scratch capture. #1992: the
    // compression-marker rationale that replaced it was stale too — see
    // WRITE_REASON for the reason that holds.
    if let Some(target) = segments.iter().find_map(|(segment, here)| {
        disallowed_write_redirect_target(segment, write_allow_paths, project_root, here.as_deref())
    }) {
        return Some(format!(
            "ERROR: ctx_shell refuses the redirect into `{target}` — the destination decides, \
             not the size or content of the output. {WRITE_REASON} {CAPTURE_RULE} \
             Write the file with ctx_patch or the native Write tool, or capture to a scratch \
             path.{}",
            relative_target_note(&target)
        ));
    }

    // #989: tee detection must run on heredoc-stripped text to avoid false
    // positives when the word "tee" appears in heredoc/quoted payloads.
    //
    // #1671: the rule is the destination, not the pipe. The message used to say
    // "tee without pipe" and then state that piped tee is allowed — so it
    // rejected a command and, in the next sentence, described that exact
    // command as permitted. A reviewer read it, concluded the block was correct
    // because an alternative was offered, and closed a review with no findings.
    // Naming the pipe also sent callers off to restructure their pipeline,
    // which cannot help: `… | tee FILE | wc -l` is judged identically.
    if let Some(target) = segments.iter().find_map(|(segment, here)| {
        disallowed_tee_target(segment, write_allow_paths, project_root, here.as_deref())
    }) {
        return Some(format!(
            "ERROR: ctx_shell refuses `tee {target}`. Piping makes no difference: the \
             destination decides. {WRITE_REASON} {CAPTURE_RULE} \
             Write the file with ctx_patch or the native Write tool, or tee to a scratch \
             path.{}",
            relative_target_note(&target)
        ));
    }

    if is_heredoc_file_write(command, &segments, write_allow_paths, project_root) {
        return Some(format!(
            "ERROR: ctx_shell detected a heredoc writing to a file. {WRITE_REASON} \
             Create the file with ctx_patch (op=create) or the native Write tool. \
             Note: heredocs for input piping (e.g. psql <<EOF) are allowed."
        ));
    }

    // #1672: on the heredoc-stripped text, for the same reason as #931 and
    // #989 above. A download flag inside a heredoc *body* is opaque data — a
    // commit message, a doc, a fixture — and refusing the call for it blocks
    // writing about the guard at all. This detector was simply never switched
    // over when the other two were.
    if let Some(reason) = download_to_file_reason(&cmd_no_heredoc) {
        return Some(format!(
            "ERROR: ctx_shell detected a file download/write ({reason}). \
             Download flags write a file without a redirect, so the capture rule applies \
             to them as well (GH #391). \
             For text, fetch to stdout: curl <url> / wget -qO- <url>. \
             For a binary (image, PDF, archive) neither stdout nor the editor's Write \
             tool can carry the bytes — download to an absolute scratch path instead, \
             e.g. curl -sL -o /tmp/shot.png <url>, which is permitted (GH #1661). \
             The target must be a literal path, $TMPDIR, or a variable this same command \
             set to a literal earlier (D=/tmp/x; curl -o $D/f); any other variable cannot \
             be resolved here and is refused."
        ));
    }

    None
}

/// How a relative target is placed — said when one is refused, because the
/// answer is usually to say where it should go (#1850).
fn relative_target_note(target: &str) -> &'static str {
    let t = target
        .trim_start_matches(['>', '&', '|'])
        .trim_matches(['"', '\'']);
    if t.starts_with('/') || std::path::Path::new(t).is_absolute() {
        return "";
    }
    " A relative target is placed in the directory its own command runs in: the call's \
     `cwd`, moved by a plain `cd <dir> &&` before it. Where that directory cannot be \
     known — a `cd` through a variable, `pushd`, a subshell, a `cd` that may not have \
     run — a relative target is refused; give an absolute path."
}

/// Each segment of the heredoc-stripped command with the directory it runs in,
/// as a string for the path checks. An empty `cwd` is no `cwd`.
fn segments_in_cwd(command: &str, cwd: Option<&str>) -> Vec<(String, Option<String>)> {
    let base = cwd
        .filter(|c| !c.trim().is_empty())
        .map(std::path::Path::new);
    crate::core::command_cwd::segments_with_cwd(command, base)
        .into_iter()
        .map(|s| {
            let here = s.cwd.map(|p| p.to_string_lossy().into_owned());
            (s.segment, here)
        })
        .collect()
}

/// Well-known Unix scratch prefixes that agents use on every platform.
/// On Windows, `/tmp/foo` is not a real absolute path, but Git Bash, WSL,
/// and agent-generated commands routinely target it.  (#1467)
fn is_unix_scratch_prefix(path: &str) -> bool {
    for prefix in ["/tmp", "/var/tmp", "/private/tmp", "/dev/null"] {
        if path == prefix || path.starts_with(&format!("{prefix}/")) {
            return true;
        }
    }
    false
}

/// Detects download/copy tools writing directly to files via their own flags.
/// Returns true when a path targets a scratch/temp location outside the
/// project, where file downloads are safe (#1021).
fn is_scratch_path(path: &str) -> bool {
    if is_unix_scratch_prefix(path) {
        return true;
    }
    if let Ok(tmpdir) = std::env::var("TMPDIR")
        && !tmpdir.is_empty()
        && std::path::Path::new(path).starts_with(tmpdir.as_str())
    {
        return true;
    }
    // Windows: %TEMP% / std::env::temp_dir()
    let tmp = std::env::temp_dir();
    if std::path::Path::new(path).starts_with(&tmp) {
        return true;
    }
    false
}

/// (`curl -o`, `wget` default mode, `dd of=`) — the redirect-free equivalent of
/// `> file`, reported as a `validate_command` bypass in GH #391.
fn download_to_file_reason(command: &str) -> Option<String> {
    // #1661: the scratch carve-out is written in absolute paths, but agents
    // reach a scratch directory the way a person would — `cd <scratchpad> &&
    // curl -o shot.png …`. Judged as the bare string `shot.png`, a target that
    // lands squarely inside the sanctioned directory looked like a project
    // write and was blocked, leaving no in-tool way to fetch a binary at all.
    // Segments are paired with the directory they actually run in so a relative
    // target is judged where it lands.
    //
    // #1953: likewise a target spelled through a variable the same command set
    // to a literal (`D=/tmp/x; curl -o $D/f`) is judged where it lands; any
    // other variable stays unresolved and is refused as before.
    let segments = crate::core::command_cwd::segments_with_cwd(command, None);
    let known_vars = literal_assignments_before_each_segment(command, segments.len());
    for (index, seg_cwd) in segments.into_iter().enumerate() {
        let seg = seg_cwd.segment;
        let here = seg_cwd.cwd;
        let vars = &known_vars[index];
        let resolve = |path: &str| -> String {
            let expanded = expand_leading_variable(path, vars);
            let path = expanded.as_deref().unwrap_or(path);
            let p = std::path::Path::new(path);
            // `starts_with('/')`: shell text, so a Unix-absolute target is
            // absolute on Windows too, where `is_absolute` disagrees (#1467).
            if p.is_absolute() || path.starts_with('/') {
                return path.to_string();
            }
            // Without a known directory the target stays unresolved, which the
            // scratch check then rejects — the guard errs closed.
            match here.as_deref() {
                Some(dir) => crate::core::command_cwd::join_in(dir, path)
                    .to_string_lossy()
                    .into_owned(),
                None => path.to_string(),
            }
        };
        let tokens = crate::core::shell_allowlist::shell_tokenize(seg.trim());
        let Some(first) = tokens.first() else {
            continue;
        };
        let base = first.rsplit('/').next().unwrap_or(first);
        match base {
            "curl" => {
                let tokens_slice = &tokens[1..];
                for (i, tok) in tokens_slice.iter().enumerate() {
                    let target: Option<&str> = if tok == "--output" {
                        tokens_slice.get(i + 1).map(String::as_str)
                    } else if let Some(val) = tok.strip_prefix("--output=") {
                        Some(val)
                    } else if tok == "--output-dir" {
                        tokens_slice.get(i + 1).map(String::as_str)
                    } else if let Some(val) = tok.strip_prefix("--output-dir=") {
                        Some(val)
                    } else if tok.starts_with('-')
                        && !tok.starts_with("--")
                        && tok[1..].contains('o')
                    {
                        // -o <file>: next token is the path
                        tokens_slice.get(i + 1).map(String::as_str)
                    } else if tok == "--remote-name"
                        || tok == "--remote-name-all"
                        || (tok.starts_with('-')
                            && !tok.starts_with("--")
                            && tok[1..].contains('O'))
                    {
                        Some(".")
                    } else {
                        None
                    };
                    if let Some(path) = target {
                        if is_scratch_path(&resolve(path)) {
                            continue;
                        }
                        return Some(format!("curl {tok}"));
                    }
                }
            }
            "wget" => {
                // wget writes a file BY DEFAULT; only stdout/no-download modes pass.
                let to_stdout = tokens[1..].iter().enumerate().any(|(i, tok)| {
                    tok == "--output-document=-"
                        || tok == "-O-"
                        || (tok.starts_with('-') && !tok.starts_with("--") && tok.ends_with("O-"))
                        || ((tok == "-O" || tok == "--output-document")
                            && tokens.get(i + 2).map(std::string::String::as_str) == Some("-"))
                        || tok == "--spider"
                });
                if !to_stdout {
                    return Some(
                        "wget downloads to a file by default; use wget -qO- <url> for stdout"
                            .to_string(),
                    );
                }
            }
            "dd" => {
                for tok in &tokens[1..] {
                    if tok.starts_with("of=") && !tok.starts_with("of=/dev/null") {
                        return Some(format!("dd {tok}"));
                    }
                }
            }
            _ => {}
        }
    }
    None
}

/// For each segment, the variables known to hold a literal value when that
/// segment starts (#1953).
///
/// Only a standalone `NAME=value` / `export NAME=value` segment that certainly
/// runs in the current shell counts: not behind `&&`/`||` (it may be skipped),
/// not in a pipeline or the background (a subshell, so nothing propagates).
/// A value is literal only without `$`, a backtick or a leading `~`. Any
/// segment that can rebind variables some other way (`read`, `for`, `eval`,
/// `source`, …) forgets everything known so far. Where this cannot follow the
/// shell it knows less, and an unknown variable leaves the target refused.
fn literal_assignments_before_each_segment(
    command: &str,
    segment_count: usize,
) -> Vec<std::collections::HashMap<String, String>> {
    use crate::core::shell_allowlist::Separator;

    let separated = crate::core::shell_allowlist::segments_with_separators(command);
    if separated.len() != segment_count {
        // Both come from the same scanner; if they ever disagree, resolve nothing.
        return vec![std::collections::HashMap::new(); segment_count];
    }
    let mut known = std::collections::HashMap::new();
    let mut prev: Option<Separator> = None;
    let mut before_each = Vec::with_capacity(segment_count);
    for (segment, sep) in separated {
        before_each.push(known.clone());
        let tokens = crate::core::shell_allowlist::shell_tokenize(&segment);
        let words: &[String] = match tokens.first().map(String::as_str) {
            Some("export") => &tokens[1..],
            _ => &tokens,
        };
        let assignments: Option<Vec<(&str, &str)>> = words
            .iter()
            .map(|word| {
                let (name, value) = word.split_once('=')?;
                let valid_name = name
                    .chars()
                    .next()
                    .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
                    && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
                valid_name.then_some((name, value))
            })
            .collect();
        match assignments {
            Some(pairs) if !pairs.is_empty() => {
                let in_current_shell = matches!(prev, None | Some(Separator::Sequence))
                    && matches!(sep, None | Some(Separator::Sequence | Separator::And));
                for (name, value) in pairs {
                    let literal = !value.contains(['$', '`']) && !value.starts_with('~');
                    if in_current_shell && literal {
                        known.insert(name.to_string(), value.to_string());
                    } else {
                        known.remove(name);
                    }
                }
            }
            _ => {
                let rebinds = matches!(
                    tokens.first().map(String::as_str),
                    Some(
                        "read"
                            | "for"
                            | "select"
                            | "eval"
                            | "source"
                            | "."
                            | "unset"
                            | "declare"
                            | "typeset"
                            | "local"
                            | "readonly"
                            | "export"
                            | "mapfile"
                            | "readarray"
                            | "getopts"
                            | "printf"
                    )
                );
                if rebinds {
                    known.clear();
                }
            }
        }
        prev = sep;
    }
    before_each
}

/// Expand a target that starts with `$NAME` or `${NAME}` from `known`, or from
/// the environment for `$TMPDIR` — the scratch variable the guard itself
/// honours. `None` when the target starts with no variable or an unknown one.
fn expand_leading_variable(
    path: &str,
    known: &std::collections::HashMap<String, String>,
) -> Option<String> {
    let rest = path.strip_prefix('$')?;
    let (name, tail) = if let Some(braced) = rest.strip_prefix('{') {
        let (name, tail) = braced.split_once('}')?;
        (name, tail)
    } else {
        let end = rest
            .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
            .unwrap_or(rest.len());
        rest.split_at(end)
    };
    if name.is_empty() || !(tail.is_empty() || tail.starts_with('/')) {
        return None;
    }
    let value = match known.get(name) {
        Some(value) => value.clone(),
        None if name == "TMPDIR" => std::env::var("TMPDIR").ok().filter(|v| !v.is_empty())?,
        None => return None,
    };
    Some(format!("{value}{tail}"))
}

/// Returns true only for heredocs that redirect to files (the dangerous pattern).
/// Legitimate heredoc uses (input piping, inline scripts) are allowed through.
/// `segments` is the heredoc-stripped command with each segment's directory.
fn is_heredoc_file_write(
    command: &str,
    segments: &[(String, Option<String>)],
    write_allow_paths: &[String],
    project_root: Option<&str>,
) -> bool {
    let has_heredoc = command.contains("<<");
    if !has_heredoc {
        return false;
    }
    let cmd_lower = command.to_lowercase();
    let heredoc_patterns = ["<<eof", "<<'eof'", "<<\"eof\"", "<<end", "<<'end'"];
    let has_known_heredoc = heredoc_patterns.iter().any(|p| cmd_lower.contains(p));
    if !has_known_heredoc {
        return false;
    }
    // #931: the segments come from the heredoc-stripped text, so `>` / `>>`
    // inside the body are not mistaken for file-write redirects.
    segments.iter().any(|(segment, here)| {
        disallowed_write_redirect_target(segment, write_allow_paths, project_root, here.as_deref())
            .is_some()
    })
}

/// Detects shell redirect operators (`>` or `>>`) that write to files.
/// Ignores `>` inside quotes, after a backslash escape (`\"` must not toggle
/// quote state, `\>` is a literal), `2>` (stderr), `/dev/null`, and
/// comparison operators.
/// #848: temp directory targets are read-back, not persistent writes.
/// #848/#989: targets that are NOT persistent project-file writes.
/// Redirecting to temp dirs, /dev/* devices, or paths containing shell
/// variables (which we cannot resolve at parse time) is output capture,
/// not file authoring.
pub fn is_temp_redirect_target(target: &str) -> bool {
    let write_allow_paths = crate::core::config::default_shell_write_allow_paths();
    // No cwd here: this public helper judges a target in isolation, so a
    // relative one stays unplaceable and therefore not allowed (#1811).
    is_write_allowed_redirect_target(target, &write_allow_paths, None, None)
}

fn is_write_allowed_redirect_target(
    target: &str,
    write_allow_paths: &[String],
    project_root: Option<&str>,
    cwd: Option<&str>,
) -> bool {
    // `>|` is the noclobber-override form of `>`; the `|` is not part of the path.
    let t = target.trim_start_matches(['>', '&', '|']);
    // #1142: agents quote scratch paths (`> "$TMPDIR/x.log"`, `> "/private/tmp/x"`);
    // strip quotes so quoted and unquoted targets are judged identically.
    let t = t.trim_matches(['"', '\'']);
    if t.starts_with('$') || t.starts_with("${") {
        // Preserve #989's escape hatch for harness-provided scratch paths.
        return true;
    }

    // On Windows, agents often use Unix-style /tmp paths (Git Bash, WSL,
    // or Claude generating Unix commands).  `/tmp/foo` is not absolute per
    // std::path on Windows, so check well-known scratch prefixes before
    // the is_absolute gate.  (#1467)
    if is_unix_scratch_prefix(t) {
        return true;
    }

    // #1811: a relative target is a destination like any other — it just needs
    // placing first. The guard used to refuse every relative target outright,
    // so `cwd=<scratch> … > probe.txt` was blocked while the identical
    // `> <scratch>/probe.txt` was allowed, and the refusal said "the
    // destination decides" about a destination it had not resolved.
    //
    // Resolving narrows as often as it widens: a relative target under a
    // project cwd now resolves *into* the project and is refused on the same
    // rule as an absolute one, instead of by accident of its spelling.
    //
    // `cwd` is the directory the target's own command runs in — the call's
    // `cwd` moved by any `cd` before it (#1850, `command_cwd`), or `None` when
    // that cannot be known, which refuses a relative target below.
    let path = std::path::Path::new(t);
    let resolved_input;
    let path = if path.is_absolute() {
        path
    } else if let Some(base) = cwd.filter(|c| !c.trim().is_empty()) {
        resolved_input = std::path::Path::new(base).join(path);
        resolved_input.as_path()
    } else {
        return false;
    };
    let resolved = resolve_path_for_comparison(path);
    if project_root.is_some_and(|root| {
        resolved.starts_with(resolve_path_for_comparison(std::path::Path::new(root)))
    }) {
        return false;
    }
    write_allow_paths.iter().any(|allowed| {
        resolved.starts_with(resolve_path_for_comparison(std::path::Path::new(allowed)))
    })
}

fn resolve_path_for_comparison(path: &std::path::Path) -> std::path::PathBuf {
    use std::path::{Component, PathBuf};

    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            other => normalized.push(other.as_os_str()),
        }
    }

    let mut unresolved = Vec::new();
    let mut existing = normalized.clone();
    while !existing.exists() {
        let Some(name) = existing.file_name() else {
            break;
        };
        unresolved.push(name.to_os_string());
        if !existing.pop() {
            break;
        }
    }
    let mut resolved = crate::core::pathutil::canonicalize_secure_or_self(&existing);
    for component in unresolved.iter().rev() {
        resolved.push(component);
    }
    resolved
}

fn tee_targets(command: &str) -> Vec<String> {
    crate::core::shell_allowlist::extract_all_commands_pub(command)
        .into_iter()
        .filter_map(|segment| {
            let tokens = crate::core::shell_allowlist::shell_tokenize(segment.trim());
            let first = tokens.first()?;
            if first.rsplit('/').next().unwrap_or(first) != "tee" {
                return None;
            }
            let mut after_separator = false;
            Some(
                tokens
                    .iter()
                    .skip(1)
                    .find(|token| {
                        if *token == "--" {
                            after_separator = true;
                            return false;
                        }
                        after_separator || !token.starts_with('-')
                    })
                    .cloned()
                    .unwrap_or_default(),
            )
        })
        .collect()
}

/// The first `tee` destination that is not a permitted write target.
///
/// Returns the path so the refusal can name it (#1671). The rule is entirely
/// about the destination — whether the `tee` is piped makes no difference — and
/// a message that cannot name the destination ends up describing some other
/// rule instead.
fn disallowed_tee_target(
    command: &str,
    write_allow_paths: &[String],
    project_root: Option<&str>,
    cwd: Option<&str>,
) -> Option<String> {
    tee_targets(command).into_iter().find(|target| {
        !target.is_empty()
            && !is_write_allowed_redirect_target(target, write_allow_paths, project_root, cwd)
    })
}

/// True when the command redirects into a path it may not write.
///
/// Test-only convenience: every production caller needs the offending target
/// for its message and goes to [`disallowed_write_redirect_target`] directly
/// (#1768). The assertions here only care about the verdict, and reading
/// `.is_some()` into every one of them would obscure what they pin.
#[cfg(test)]
fn has_file_write_redirect(
    command: &str,
    write_allow_paths: &[String],
    project_root: Option<&str>,
) -> bool {
    // No cwd: these assertions pin the target-spelling rules, where a relative
    // target is unplaceable and therefore refused (#1811). The cwd-resolution
    // behaviour has its own tests that call the public entry point.
    disallowed_write_redirect_target(command, write_allow_paths, project_root, None).is_some()
}

/// The first redirect target that is **not** write-allowed, or `None` when the
/// command writes nowhere it may not.
///
/// Returns the target rather than a bare verdict so the refusal can name what
/// tripped it (#1768) — the same reason [`disallowed_tee_target`] exists. A
/// guard that states a rule the caller cannot map onto their own command gets
/// read as something else (there, "piping"; here, "large payloads"), and every
/// such misreading sends the caller somewhere that cannot help.
fn disallowed_write_redirect_target(
    command: &str,
    write_allow_paths: &[String],
    project_root: Option<&str>,
    cwd: Option<&str>,
) -> Option<String> {
    let bytes = command.as_bytes();
    let len = bytes.len();
    let mut i = 0;
    let mut in_single_quote = false;
    let mut in_double_quote = false;

    while i < len {
        let c = bytes[i];
        if c == b'\\' && !in_single_quote {
            // A backslash escapes the next byte (POSIX: outside quotes and
            // inside double quotes; inside single quotes it is literal).
            // Without this, an escaped quote like `\"` toggled the quote
            // state and literal `>` in quoted prose (e.g. `(root: <root>)`
            // in a gh --body string) read as a redirect (#903).
            i += 2;
            continue;
        }
        if c == b'\'' && !in_double_quote {
            in_single_quote = !in_single_quote;
        } else if c == b'"' && !in_single_quote {
            in_double_quote = !in_double_quote;
        } else if c == b'>' && !in_single_quote && !in_double_quote {
            if i > 0 && bytes[i - 1] == b'2' {
                i += 1;
                continue;
            }
            let target_start = if i + 1 < len && bytes[i + 1] == b'>' {
                i + 2
            } else {
                i + 1
            };
            // The redirect *operator* can carry one more character: `>|`
            // overrides noclobber and `>&` duplicates a descriptor. Neither
            // belongs to the target word, so strip it before reading the word
            // and re-attach `&` for the fd check below.
            let rest = command[target_start..].trim_start();
            let (rest, fd_dup) = match rest.strip_prefix('|') {
                Some(r) => (r, false),
                None => match rest.strip_prefix('&') {
                    Some(r) => (r, true),
                    None => (rest, false),
                },
            };
            // The word ends at whitespace *or* at a shell metacharacter
            // (#1659). Stopping only at whitespace made the verdict depend on
            // where the redirect sat: `echo a 1>/dev/null` read the target as
            // `/dev/null` and was allowed, while `echo a 1>/dev/null; echo b`
            // read `/dev/null;` — semicolon included, since it is not
            // whitespace — missed the /dev/null exemption, and was blocked as a
            // file write. `&&` happened to work only because a space precedes
            // it. Same redirect, same shell semantics, opposite verdicts.
            //
            // `(` is deliberately not a terminator: it would reduce the
            // process-substitution target `>(cmd)` to an empty word, and an
            // empty word falls through unblocked.
            let word: String = rest
                .chars()
                .take_while(|c| !c.is_whitespace() && !matches!(c, ';' | '&' | '|' | ')' | '<'))
                .collect();
            let target = if fd_dup { format!("&{word}") } else { word };
            if target == "/dev/null" || target == "/dev/stdout" || target == "/dev/stderr" {
                i += 1;
                continue;
            }
            // #1142: `>&1` / `>&2` duplicate a file descriptor — no file involved.
            // (`2>&1` is already skipped by the `2>` case above.)
            if let Some(fd) = target.strip_prefix('&')
                && !fd.is_empty()
                && (fd == "-" || fd.chars().all(|c| c.is_ascii_digit()))
            {
                i += 1;
                continue;
            }
            // #848: allow redirects to temp directories — agents capture
            // build output for grepping, not writing persistent files.
            if is_write_allowed_redirect_target(&target, write_allow_paths, project_root, cwd) {
                i += 1;
                continue;
            }
            if !target.is_empty() {
                return Some(target);
            }
        }
        i += 1;
    }
    None
}

/// On Windows cmd.exe, `;` is not a valid command separator.
/// Convert `cmd1; cmd2` to `cmd1 && cmd2` when running under cmd.exe.
pub fn normalize_command_for_shell(command: &str) -> String {
    if !cfg!(windows) {
        return command.to_string();
    }
    let (_, flag) = crate::shell::shell_and_flag();
    if flag != "/C" {
        return command.to_string();
    }
    let bytes = command.as_bytes();
    let mut result = Vec::with_capacity(bytes.len() + 16);
    let mut in_single = false;
    let mut in_double = false;
    for (i, &b) in bytes.iter().enumerate() {
        if b == b'\'' && !in_double {
            in_single = !in_single;
        } else if b == b'"' && !in_single {
            in_double = !in_double;
        } else if b == b';' && !in_single && !in_double {
            result.extend_from_slice(b" && ");
            continue;
        }
        result.push(b);
        let _ = i;
    }
    String::from_utf8(result).unwrap_or_else(|_| command.to_string())
}

/// Compresses shell command output using the unified compression pipeline.
/// Delegates to the same exit-code-aware logic used by the CLI, so a failed
/// command (`exit_code != 0`) is preserved verbatim and successful output is
/// compressed consistently (excluded_commands, structural routing, terse). #810.
pub fn handle(command: &str, output: &str, exit_code: i32, _crp_mode: CrpMode) -> String {
    crate::shell::compress::engine::compress_for_outcome(command, output, exit_code)
}

pub fn handle_with_context(
    command: &str,
    output: &str,
    exit_code: i32,
    crp_mode: CrpMode,
    project_root: Option<&str>,
) -> String {
    let mut result = handle(command, output, exit_code, crp_mode);

    {
        if let Some(root) = project_root {
            let estimated_tokens = result.len() / 4;
            if estimated_tokens > 500 {
                let kernel_budget = 100;
                if let Some(enrichment) =
                    crate::core::context_kernel::bridge::kernel_enrich(command, root, kernel_budget)
                    && !enrichment.blocks.is_empty()
                {
                    result.push_str("\n--- kernel context ---\n");
                    result.push_str(&enrichment.blocks);
                }
            }
        }
    }

    result
}

#[cfg(test)]
mod kernel_tests {
    use super::handle_with_context;
    use crate::tools::CrpMode;

    #[test]
    fn handle_with_context_does_not_panic_on_short_output() {
        let result = handle_with_context("ls", "file.txt", 0, CrpMode::Tdd, Some("/tmp"));
        assert!(!result.is_empty(), "`result` is empty");
    }
}

#[cfg(test)]
fn is_search_command(command: &str) -> bool {
    let cmd = command.trim_start();
    cmd.starts_with("grep ")
        || cmd.starts_with("rg ")
        || cmd.starts_with("find ")
        || cmd.starts_with("fd ")
        || cmd.starts_with("ag ")
        || cmd.starts_with("ack ")
}

#[cfg(test)]
fn generic_compress(output: &str) -> String {
    let output = crate::core::compressor::strip_ansi(output);
    let lines: Vec<&str> = output
        .lines()
        .filter(|l| {
            let t = l.trim();
            !t.is_empty()
        })
        .collect();

    if lines.len() <= 20 {
        return lines.join("\n");
    }

    let show_count = (lines.len() / 3).min(30);
    let half = show_count / 2;
    let first = &lines[..half];
    let last = &lines[lines.len() - half..];
    let omitted = lines.len() - (half * 2);
    format!(
        "{}\n[truncated: showing {}/{} lines, {} omitted. Use raw=true for full output.]\n{}",
        first.join("\n"),
        half * 2,
        lines.len(),
        omitted,
        last.join("\n")
    )
}

/// Detects OAuth device code flow output that must not be compressed.
/// Uses a two-tier approach: strong signals match alone (very specific to
/// device code flows), weak signals require a URL/domain in the same output.
pub fn contains_auth_flow(output: &str) -> bool {
    let lower = output.to_lowercase();

    const STRONG_SIGNALS: &[&str] = &[
        "devicelogin",
        "deviceauth",
        "device_code",
        "device code",
        "device-code",
        "verification_uri",
        "user_code",
        "one-time code",
    ];

    if STRONG_SIGNALS.iter().any(|s| lower.contains(s)) {
        return true;
    }

    const WEAK_SIGNALS: &[&str] = &[
        "enter the code",
        "enter this code",
        "enter code:",
        "use the code",
        "use a web browser to open",
        "open the page",
        "authenticate by visiting",
        "sign in with the code",
        "sign in using a code",
        "verification code",
        "authorize this device",
        "waiting for authentication",
        "waiting for login",
        "waiting for you to authenticate",
        "open your browser",
        "open in your browser",
    ];

    let has_weak_signal = WEAK_SIGNALS.iter().any(|s| lower.contains(s));
    if !has_weak_signal {
        return false;
    }

    lower.contains("http://") || lower.contains("https://")
}

#[cfg(test)]
mod tests;
