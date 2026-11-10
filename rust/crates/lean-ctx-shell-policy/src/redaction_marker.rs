// SPDX-License-Identifier: Apache-2.0
//! The redaction-marker guard (24.10.).
//!
//! Redaction applies only to what an agent is shown, never to what runs. An
//! agent that copies a `[REDACTED:<label>]` marker out of a redacted view into
//! a command or a file writes the marker itself, not the value (a remote
//! config with a marker in it crash-looped a server on 24.10.). So a marker
//! never runs and is never written, whatever the shell security mode, with
//! one narrow exception: source code whose text must hold the marker literal
//! (a redaction test, a marker constant). See [`marker_literal_allowed_in`].

use std::path::{Component, Path};

use crate::ShellError;

use super::lexer::{lex, remove_ranges};

/// Where the allow-path for marker literals is documented.
const DOC: &str = "docs/reference/13-security-and-governance.md, section 2.4 Redaction markers";

fn first_marker(text: &str) -> Option<regex::Match<'_>> {
    static MARKER: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    MARKER
        .get_or_init(|| {
            regex::Regex::new(r"\[REDACTED:[^\]\n]{1,64}\]").expect("BUG: invalid redaction marker")
        })
        .find(text)
}

fn blocked(marker: &str) -> ShellError {
    ShellError::from(format!(
        "[BLOCKED] the command contains the redaction marker `{marker}`. It stands for a \
         value lean-ctx hid from your view; running it would write the marker itself, \
         not the value. Do not retype the marker, and do not retry this command with it.\n\
         To use the real value, let the command derive it:\n\
         - It came from a command's output: run that command inside this one and pipe \
         its output on (`producer | consumer`), so the value never passes through \
         your view.\n\
         - It lives in an env var or a secrets file: read it there inside the command \
         (`\"$DB_URL\"`, `--password-file`).\n\
         Source or test code that must contain the marker literal (Rust files, test \
         files) is the one exception: write it with a quoted heredoc into that file \
         (`cat > src/x_tests.rs <<'EOF' … EOF`) or with your file-editing tool. \
         Config and data files never take a marker. See {DOC}."
    ))
}

/// Refuses `code` that contains a redaction marker such as
/// `[REDACTED:Database credentials]`, with no exception: for code that runs
/// as given (an interpreter snippet, a `lean-ctx` invocation, file content
/// whose destination is unknown). Shell commands go through
/// [`refuse_redaction_marker_in_command`], files through
/// [`refuse_redaction_marker_in_file`].
pub fn refuse_redaction_marker(code: &str) -> Result<(), ShellError> {
    match first_marker(code) {
        None => Ok(()),
        Some(found) => Err(blocked(found.as_str())),
    }
}

/// [`refuse_redaction_marker`] for file content written to `path` in the
/// project at `root`: a marker is allowed only where
/// [`marker_literal_allowed_in`] says so for `path` relative to `root`. Only
/// the part below the root counts, so a project that itself sits in a
/// `test`/`tests` folder (`/home/u/test/app`) gains no exemption for its
/// config; a path outside the root never takes a marker.
pub fn refuse_redaction_marker_in_file(
    root: &Path,
    path: &Path,
    content: &str,
) -> Result<(), ShellError> {
    if path_below_root(root, path).is_some_and(|relative| marker_literal_allowed_in(&relative)) {
        return Ok(());
    }
    refuse_redaction_marker(content)
}

/// `path` relative to `root`, comparing both as given and canonicalized (a
/// jailed path is canonical, a configured root may not be: `/tmp` vs
/// `/private/tmp`). `None` when `path` is not strictly below `root`. A
/// relative `path` is taken as already relative to the root.
fn path_below_root(root: &Path, path: &Path) -> Option<std::path::PathBuf> {
    if path.is_relative() {
        return Some(path.to_path_buf());
    }
    let roots = [Some(root.to_path_buf()), std::fs::canonicalize(root).ok()];
    let paths = [Some(path.to_path_buf()), std::fs::canonicalize(path).ok()];
    roots
        .iter()
        .flatten()
        .filter(|r| r.is_absolute())
        .find_map(|r| {
            paths
                .iter()
                .flatten()
                .find_map(|p| p.strip_prefix(r).ok().map(Path::to_path_buf))
        })
        .filter(|relative| relative.components().next().is_some())
}

/// [`refuse_redaction_marker`] for a shell command, with one allow-path: the
/// whole command is a single `cat > FILE <<'EOF'` (or `cat >> FILE`,
/// `tee [-a] FILE`) whose markers all sit in the body of that one quoted
/// heredoc, and `FILE` is source code per [`marker_literal_allowed_in`]. A
/// quoted heredoc body is literal stdin, never executed or expanded; the
/// operator line holds nothing else (no pipe, `;`, substitution, variable,
/// second command or redirect), so the body can only land in that file.
pub fn refuse_redaction_marker_in_command(command: &str) -> Result<(), ShellError> {
    let Some(found) = first_marker(command) else {
        return Ok(());
    };
    if heredoc_into_source_file(command) {
        return Ok(());
    }
    Err(blocked(found.as_str()))
}

/// Source code that may hold a redaction marker literal: any Rust file
/// (compiled, never read as configuration at run time) and test files of the
/// common languages (a `tests`/`test`/`__tests__` directory, or a test file
/// name such as `test_x.py`, `x_test.go`, `x.test.ts`, `x.spec.js`,
/// `x_tests.rs`). Everything else is refused: configuration and data
/// (`.env`, `.toml`, `.json`, `.yaml`, `.ini`, `.conf`, …), shell scripts
/// (they run), documents, and source outside a test location in languages
/// whose source doubles as configuration (`settings.py`).
///
/// `path` is relative to the project root. An absolute path never takes a
/// marker: its parts above the project (`/home/u/test/app/settings.py`)
/// would otherwise pass for a test directory.
#[must_use]
pub fn marker_literal_allowed_in(path: &Path) -> bool {
    // Judged on the path as written: only plain names. A root or drive prefix
    // reaches above the project, and a `..` or `.` part could walk out of the
    // test folder it names (`tests/../settings.py`).
    if !path.components().all(|c| matches!(c, Component::Normal(_))) {
        return false;
    }
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
        return false;
    };
    let Some((stem, ext)) = name.rsplit_once('.') else {
        return false;
    };
    if stem.is_empty() {
        return false;
    }
    if ext == "rs" {
        return true;
    }
    const TEST_SOURCE: &[&str] = &[
        "py", "ts", "tsx", "js", "jsx", "mjs", "cjs", "go", "java", "kt", "swift", "c", "cc",
        "cpp", "h", "hpp", "cs", "rb",
    ];
    if !TEST_SOURCE.contains(&ext) {
        return false;
    }
    let in_test_dir = path
        .parent()
        .into_iter()
        .flat_map(Path::components)
        .any(|c| matches!(c.as_os_str().to_str(), Some("tests" | "test" | "__tests__")));
    let test_name = stem.starts_with("test_")
        || stem.ends_with("_test")
        || stem.ends_with("_tests")
        || stem
            .rsplit_once('.')
            .is_some_and(|(_, kind)| kind == "test" || kind == "spec")
        || stem.ends_with("Test")
        || stem.ends_with("Tests");
    in_test_dir || test_name
}

/// True when `command` is exactly one `cat`/`tee` writing one quoted heredoc
/// into a file [`marker_literal_allowed_in`] admits, and no marker sits
/// outside that heredoc's body.
fn heredoc_into_source_file(command: &str) -> bool {
    if !command.contains("<<") {
        return false;
    }
    let lexed = lex(command);
    if lexed.problem.is_some()
        || lexed.too_deep
        || !lexed.expansions.is_empty()
        || !lexed.continuations.is_empty()
        || lexed.heredocs.len() != 1
        || !lexed.heredocs[0].quoted
    {
        return false;
    }
    let operator_line = remove_ranges(command, vec![lexed.heredocs[0].body.clone()]);
    let operator_line = operator_line.trim();
    // Only plain words, paths, the redirect and the quoted delimiter: no
    // `$`, backtick, `;`, `|`, `&`, `(`, a second line or a glob.
    if operator_line.is_empty()
        || !operator_line.bytes().all(|b| {
            b.is_ascii_alphanumeric()
                || matches!(
                    b,
                    b' ' | b'.' | b'_' | b'-' | b'/' | b'+' | b'<' | b'>' | b'\'' | b'"'
                )
        })
    {
        return false;
    }
    match file_written_by(operator_line) {
        Some(target) => marker_literal_allowed_in(Path::new(target)),
        None => false,
    }
}

/// The one file `cat > F <<'D'`, `cat <<'D' >> F` or `tee [-a] F <<'D'`
/// writes; `None` for any other shape.
fn file_written_by(line: &str) -> Option<&str> {
    let mut words = line.split(' ').filter(|w| !w.is_empty());
    let program = words.next()?;
    let mut heredoc = false;
    let mut target: Option<&str> = None;
    let mut redirect_pending = false;
    let mut append_flag = false;
    while let Some(word) = words.next() {
        if redirect_pending {
            target = Some(plain_path(word)?);
            redirect_pending = false;
            continue;
        }
        if let Some(delim) = word.strip_prefix("<<") {
            if heredoc {
                return None;
            }
            heredoc = true;
            let delim = delim.strip_prefix('-').unwrap_or(delim);
            let delim = if delim.is_empty() {
                words.next()?
            } else {
                delim
            };
            if !is_quoted_delimiter(delim) {
                return None;
            }
            continue;
        }
        if word.contains(['\'', '"', '<']) {
            return None;
        }
        if let Some(rest) = word.strip_prefix(">>").or_else(|| word.strip_prefix('>')) {
            if program != "cat" || target.is_some() {
                return None;
            }
            if rest.is_empty() {
                redirect_pending = true;
            } else {
                target = Some(plain_path(rest)?);
            }
            continue;
        }
        if program == "tee" && word == "-a" && !append_flag && target.is_none() {
            append_flag = true;
            continue;
        }
        if program == "tee" && target.is_none() {
            target = Some(plain_path(word)?);
            continue;
        }
        return None;
    }
    if !heredoc || redirect_pending || !matches!(program, "cat" | "tee") {
        return None;
    }
    target
}

fn is_quoted_delimiter(word: &str) -> bool {
    let inner = word
        .strip_prefix('\'')
        .and_then(|w| w.strip_suffix('\''))
        .or_else(|| word.strip_prefix('"').and_then(|w| w.strip_suffix('"')));
    inner
        .is_some_and(|d| !d.is_empty() && d.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_'))
}

/// A path word without quotes, options or redirect characters.
fn plain_path(word: &str) -> Option<&str> {
    (!word.is_empty() && !word.starts_with('-') && !word.contains(['\'', '"', '<', '>']))
        .then_some(word)
}
