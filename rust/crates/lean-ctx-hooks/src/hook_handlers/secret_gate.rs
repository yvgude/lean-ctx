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
//! The same funnel applies the Hive organisation policy (ent2-3):
//! `security.readable_roots` and `security.never_send` refuse a target
//! outside the roots or matching a never-send pattern, and a damaged or
//! missing-but-required policy refuses every read (fail closed). Neither the
//! role nor the warn mode loosens them. While `never_send` is set, a native
//! search over a directory is refused too, since its matches cannot be
//! filtered here; `ctx_search` leaves those files out.
//!
//! Scope: the call's explicit targets (file, search root, file globs). A
//! directory-wide search can still match a secret file the host does not
//! ignore, and shell reads (`cat .env`) are not covered here.
//!
//! [`io_boundary::check_secret_path_for_tool`]: lean_ctx::core::io_boundary::check_secret_path_for_tool

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
    let explicit = TARGET_FIELDS
        .iter()
        .filter_map(|field| args.get(*field))
        .flat_map(strings)
        .flat_map(|target| candidates(&target))
        .find_map(|target| refuse(tool, &target));
    if explicit.is_some() || tool != "Grep" {
        return explicit;
    }
    search_scope_refusal(args)
}

/// Fields that name a native search's root directory.
const SCOPE_FIELDS: &[&str] = &["path", "dir_path", "paths"];

/// The Hive I/O rules for a native search over a directory (ent2-3): the
/// root (the working directory when the call names none) must be readable,
/// and while the policy sets `never_send` a directory-wide native search is
/// refused — the host would return matches from those files, which this gate
/// cannot filter; `ctx_search` applies `never_send` to every file it scans.
fn search_scope_refusal(args: &Value) -> Option<String> {
    let mut roots: Vec<String> = SCOPE_FIELDS
        .iter()
        .filter_map(|field| args.get(*field))
        .flat_map(strings)
        .collect();
    if roots.is_empty() {
        let cwd = std::env::current_dir().ok()?;
        let cwd = cwd.to_string_lossy().into_owned();
        if let Some(msg) = refuse("Grep", &cwd) {
            return Some(msg);
        }
        roots.push(cwd);
    }
    if !roots.iter().any(|root| absolute(root).is_dir()) {
        return None;
    }
    let reason = match lean_ctx::core::io_boundary::hive_io_rules(None) {
        Ok(Some(rules)) if rules.has_never_send() => {
            "the Hive policy sets never_send, and a native search over a directory can return \
             those files; use ctx_search, which leaves them out"
                .to_string()
        }
        Ok(_) => return None,
        Err(e) => format!("{e}; reads stay refused until a valid Hive policy is installed"),
    };
    Some(format!("lean-ctx refused the native Grep: {reason}."))
}

/// `target` as an absolute path: a relative one is taken from the working
/// directory, where the host runs the native tool.
fn absolute(target: &str) -> std::path::PathBuf {
    let path = Path::new(target);
    if path.is_absolute() {
        return path.to_path_buf();
    }
    std::env::current_dir().map_or_else(|_| path.to_path_buf(), |cwd| cwd.join(path))
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
        lean_ctx::core::io_boundary::check_secret_path_for_tool(&format!("native {tool}"), path)
            .err()
    };
    let path = absolute(target);
    let path = path.as_path();
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
        let _iso = lean_ctx::core::data_dir::isolated_data_dir();
        lean_ctx::test_env::remove_var("LEAN_CTX_IO_BOUNDARY_MODE");
        let coder = lean_ctx::core::roles::load_role("coder").expect("builtin coder role");
        lean_ctx::core::roles::with_test_active_role(coder, f)
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

    /// Install a Hive policy floor with these `security` members in the
    /// isolated data directory.
    fn install_floor(security: &Value) {
        use lean_ctx::core::policy::hive_floor;
        let dir = hive_floor::floor_dir().expect("data dir");
        std::fs::create_dir_all(&dir).expect("floor dir");
        let floor = json!({
            "schema": hive_floor::FLOOR_SCHEMA, "version": 2,
            "digest": format!("sha256:{}", "a".repeat(64)),
            "not_after": "2099-01-01T00:00:00Z",
            "policy": {"effective": {
                "schema": "leanctx.hive.policy/v1",
                "security": security, "context": {}, "sharing": {}
            }, "vaults": []}
        });
        std::fs::write(dir.join(hive_floor::FLOOR_FILE), floor.to_string()).expect("floor");
        std::fs::write(dir.join(hive_floor::REQUIRED_FILE), r#"{"version":2}"#).expect("required");
    }

    #[cfg(unix)]
    #[test]
    fn hive_readable_roots_and_never_send_refuse_native_reads() {
        as_coder(|| {
            install_floor(&json!({
                "readable_roots": ["{project}"],
                "never_send": ["*.pem"]
            }));
            let cwd = std::env::current_dir().expect("cwd");
            let inside = cwd.join("src").join("lib.rs");
            let args = json!({ "file_path": inside.to_string_lossy() });
            assert_eq!(refusal("Read", Some(&args)), None, "inside the roots");

            let args = json!({ "file_path": "/elsewhere/notes.md" });
            let msg = refusal("Read", Some(&args)).expect("outside the roots");
            assert!(msg.contains("readable root"), "{msg}");

            let args = json!({ "file_path": "tls/server.pem" });
            let msg = refusal("Read", Some(&args)).expect("never_send hit");
            assert!(msg.contains("never_send"), "{msg}");

            let args = json!({ "pattern": "KEY" });
            let msg = refusal("Grep", Some(&args)).expect("directory-wide search");
            assert!(msg.contains("ctx_search"), "{msg}");
        });
    }

    #[test]
    fn a_damaged_hive_policy_refuses_native_reads() {
        as_coder(|| {
            install_floor(&json!({}));
            let dir = lean_ctx::core::policy::hive_floor::floor_dir().expect("data dir");
            std::fs::write(
                dir.join(lean_ctx::core::policy::hive_floor::FLOOR_FILE),
                "{",
            )
            .expect("damage");
            let args = json!({ "file_path": "/p/src/main.rs" });
            let msg = refusal("Read", Some(&args)).expect("fail closed");
            assert!(msg.contains("Hive policy"), "{msg}");
        });
    }
}
