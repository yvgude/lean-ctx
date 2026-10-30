// SPDX-License-Identifier: Apache-2.0
//! Secret-path gate for a host's native read and search tools.
//!
//! `ctx_read` and `ctx_search` refuse secret-like paths through
//! [`io_boundary::check_secret_path_for_tool`], but a native call never
//! reached that rule: Claude Code keeps the Read redirect off (#637), and
//! `should_passthrough` hands `.env` to the native tool on every host. The
//! documented default `io.allow_secret_paths = false` therefore held only on
//! lean-ctx's own tools. This gate applies the same rule — same detector, same
//! role switch, same warn/enforce mode, same policy event — to the native call
//! before it runs, on the path as written and on its symlink target.
//!
//! Scope: the call's explicit targets (file, search root, file globs). A
//! directory-wide search can still match a secret file the host does not
//! ignore, and shell reads (`cat .env`) are not covered here.
//!
//! [`io_boundary::check_secret_path_for_tool`]: crate::core::io_boundary::check_secret_path_for_tool

use std::path::Path;

use serde_json::Value;

/// Native read tools across hosts (Claude Code/CodeBuddy matcher, Cursor,
/// Copilot CLI `view`, Gemini CLI `read_file`/`read_many_files`).
const READ_TOOLS: &[&str] = &[
    "Read",
    "read",
    "ReadFile",
    "read_file",
    "View",
    "view",
    "read_many_files",
];

/// Native content-search tools (Gemini CLI: `search_file_content`).
const SEARCH_TOOLS: &[&str] = &[
    "Grep",
    "grep",
    "Search",
    "search",
    "ripgrep",
    "rg",
    "search_file_content",
];

/// Argument fields that name a file, a search root or a file glob, across the
/// hosts above. A string or an array of strings.
const TARGET_FIELDS: &[&str] = &[
    "file_path",
    "path",
    "target_file",
    "absolute_path",
    "dir_path",
    "paths",
    "glob",
    "include",
];

/// Upper bound on brace-expanded glob alternatives checked per pattern.
const MAX_GLOB_ALTERNATIVES: usize = 64;

/// Whether the gate inspects this native tool.
pub(super) fn inspects(tool_name: &str) -> bool {
    READ_TOOLS.contains(&tool_name) || SEARCH_TOOLS.contains(&tool_name)
}

/// The refusal for a native read or search whose target is a secret-like path
/// the active role may not read. `None` lets the call proceed.
pub(super) fn refusal(tool_name: &str, tool_args: Option<&Value>) -> Option<String> {
    let tool = if READ_TOOLS.contains(&tool_name) {
        "Read"
    } else if SEARCH_TOOLS.contains(&tool_name) {
        "Grep"
    } else {
        return None;
    };
    let args = tool_args?;
    TARGET_FIELDS
        .iter()
        .filter_map(|field| args.get(*field))
        .flat_map(strings)
        .flat_map(|target| candidates(&target))
        .find_map(|target| refuse(tool, &target))
}

fn strings(value: &Value) -> Vec<String> {
    match value {
        Value::String(s) if !s.is_empty() => vec![s.clone()],
        Value::Array(items) => items
            .iter()
            .filter_map(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect(),
        _ => Vec::new(),
    }
}

/// A target and the file names a glob form of it stands for: brace groups
/// expanded (`*.{pem,key}`), trailing wildcards stripped (`.env*` → `.env`).
fn candidates(target: &str) -> Vec<String> {
    let mut out = Vec::new();
    for alt in expand_braces(target) {
        let stripped = alt.trim_end_matches(['*', '?']).to_string();
        if !stripped.is_empty() && stripped != alt {
            out.push(stripped);
        }
        out.push(alt);
    }
    out
}

fn expand_braces(pattern: &str) -> Vec<String> {
    let mut done = Vec::new();
    let mut todo = vec![pattern.to_string()];
    while let Some(p) = todo.pop() {
        if done.len() + todo.len() >= MAX_GLOB_ALTERNATIVES {
            done.push(p);
            continue;
        }
        let group = p
            .find('{')
            .and_then(|open| p[open..].find('}').map(|close| (open, open + close)));
        match group {
            Some((open, close)) => {
                for alt in p[open + 1..close].split(',') {
                    todo.push(format!("{}{alt}{}", &p[..open], &p[close + 1..]));
                }
            }
            None => done.push(p),
        }
    }
    done
}

fn refuse(tool: &str, target: &str) -> Option<String> {
    let check = |path: &Path| {
        crate::core::io_boundary::check_secret_path_for_tool(&format!("native {tool}"), path).err()
    };
    let path = Path::new(target);
    // `ctx_read` checks the jailed, canonical path; a symlink named
    // `config.txt` pointing at `.env` must not pass here either.
    let reason = check(path).or_else(|| {
        std::fs::canonicalize(path)
            .ok()
            .filter(|resolved| resolved != path)
            .and_then(|resolved| check(&resolved))
    })?;
    Some(format!(
        "lean-ctx refused the native {tool}: {}",
        reason.trim_start_matches("ERROR: ")
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn as_coder<T>(f: impl FnOnce() -> T) -> T {
        let _iso = crate::core::data_dir::isolated_data_dir();
        crate::test_env::remove_var("LEAN_CTX_IO_BOUNDARY_MODE");
        let coder = crate::core::roles::load_role("coder").expect("builtin coder role");
        crate::core::roles::with_test_active_role(coder, f)
    }

    #[test]
    fn secret_targets_are_refused_on_every_host_spelling() {
        as_coder(|| {
            for (tool, args) in [
                ("Read", json!({ "file_path": "/p/.env" })),
                ("view", json!({ "path": "/home/u/.ssh/id_ed25519" })),
                (
                    "read_file",
                    json!({ "absolute_path": "/p/.env.production" }),
                ),
                (
                    "read_many_files",
                    json!({ "paths": ["src/a.rs", "tls/key.pem"] }),
                ),
                ("Grep", json!({ "pattern": "KEY", "path": "/p/.env" })),
                ("Grep", json!({ "pattern": "KEY", "glob": ".env*" })),
                ("Grep", json!({ "pattern": "KEY", "glob": "**/*.{rs,pem}" })),
                (
                    "search_file_content",
                    json!({ "pattern": "KEY", "include": "*.key" }),
                ),
            ] {
                let msg = refusal(tool, Some(&args))
                    .unwrap_or_else(|| panic!("{tool} {args} must be refused"));
                assert!(msg.contains("allow_secret_paths"), "{msg}");
            }
        });
    }

    #[test]
    fn a_symlink_to_a_secret_is_refused() {
        as_coder(|| {
            let dir = tempfile::tempdir().expect("tempdir");
            let secret = dir.path().join(".env");
            std::fs::write(&secret, "TOKEN=x\n").expect("write secret");
            let alias = dir.path().join("config.txt");
            #[cfg(unix)]
            std::os::unix::fs::symlink(&secret, &alias).expect("symlink");
            #[cfg(windows)]
            if std::os::windows::fs::symlink_file(&secret, &alias).is_err() {
                return; // symlinks need a privilege on Windows
            }
            let args = json!({ "file_path": alias.to_string_lossy() });
            assert!(refusal("Read", Some(&args)).is_some());
        });
    }

    #[test]
    fn ordinary_files_and_directory_searches_pass() {
        as_coder(|| {
            for (tool, args) in [
                ("Read", json!({ "file_path": "/p/src/environment.ts" })),
                ("Read", json!({ "file_path": "/p/.env.example" })),
                (
                    "Grep",
                    json!({ "pattern": "KEY", "path": "/p/src", "glob": "*.{rs,ts}" }),
                ),
                ("Glob", json!({ "pattern": "**/.env" })),
            ] {
                assert_eq!(refusal(tool, Some(&args)), None, "{tool} {args}");
            }
        });
    }
}
