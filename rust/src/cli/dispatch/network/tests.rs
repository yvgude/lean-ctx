//! Tests for the network dispatch (help guard, proxy parsing, open-mode fallback).

use super::wants_help;

fn args(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| (*s).to_string()).collect()
}

// GH #393: `daemon enable --help` executed instead of showing help.
// The guard must catch help flags at any position, for any verb.
#[test]
fn help_flag_detected_after_verb() {
    assert!(wants_help(&args(&["enable", "--help"])));
    assert!(wants_help(&args(&["disable", "-h"])));
    assert!(wants_help(&args(&["restart", "--help"])));
    assert!(wants_help(&args(&["help"])));
    assert!(wants_help(&args(&["--help"])));
}

// #603/#616: the Codex ChatGPT-subscription opt-in must be bridged from the
// shell env into config.toml (the managed proxy / env-less setup passes never
// see the env var), but only once and never overriding an explicit config.
#[cfg(feature = "http-server")]
#[test]
fn codex_chatgpt_optin_persists_only_when_env_set_and_not_already_on() {
    use super::should_persist_codex_chatgpt_optin as persist;
    // Env opt-in present and config has not enabled it yet → persist.
    assert!(persist(true, None));
    assert!(persist(true, Some(false)));
    // Already enabled in config → idempotent no-op.
    assert!(!persist(true, Some(true)));
    // Env absent → never touch config; config stays the source of truth.
    assert!(!persist(false, None));
    assert!(!persist(false, Some(false)));
    assert!(!persist(false, Some(true)));
}

// The durable `proxy codex-chatgpt <arg>` switch: on/off (+ synonyms) mutate,
// `status`/no-arg is read-only, anything else is rejected (never a silent flip).
#[cfg(feature = "http-server")]
#[test]
fn codex_chatgpt_action_parsing_is_explicit() {
    use super::CodexChatgptAction::{Off, On, Status, Unknown};
    use super::parse_codex_chatgpt_action as parse;
    assert_eq!(parse(Some("on")), On);
    assert_eq!(parse(Some("enable")), On);
    assert_eq!(parse(Some("true")), On);
    assert_eq!(parse(Some("off")), Off);
    assert_eq!(parse(Some("disable")), Off);
    assert_eq!(parse(Some("false")), Off);
    assert_eq!(parse(Some("status")), Status);
    assert_eq!(parse(None), Status, "bare call must be read-only status");
    assert_eq!(parse(Some("nonsense")), Unknown);
}

#[test]
fn normal_verbs_do_not_trigger_help() {
    assert!(!wants_help(&args(&["enable"])));
    assert!(!wants_help(&args(&["status"])));
    assert!(!wants_help(&args(&[])));
    // Values that merely contain "help" as a substring must not match.
    assert!(!wants_help(&args(&["--helper"])));
}

// GH #587: `--open=vscode` must never launch the external browser. The
// vscode-intent fallback resolves to the guidance mode ("vscode") or, with
// --no-open, to silent ("none") — but NEVER "browser" (the #424 contract).
#[test]
fn vscode_intent_never_falls_back_to_browser() {
    assert_eq!(super::vscode_fallback_open_mode(false), "vscode");
    assert_eq!(super::vscode_fallback_open_mode(true), "none");
    assert_ne!(super::vscode_fallback_open_mode(false), "browser");
    assert_ne!(super::vscode_fallback_open_mode(true), "browser");
}

/// `lean-ctx cockpit --port N` and `--port=N` pick the same port; a value
/// that is not a port is not taken silently from the next flag.
#[test]
fn cockpit_port_accepts_both_flag_forms() {
    assert_eq!(
        super::port_flag(&args(&["--no-open", "--port", "4100"])),
        Some(4100)
    );
    assert_eq!(super::port_flag(&args(&["--port=4101"])), Some(4101));
    assert_eq!(super::port_flag(&args(&["-p", "4102"])), Some(4102));
    assert_eq!(super::port_flag(&args(&["--port", "--no-open"])), None);
    assert_eq!(super::port_flag(&args(&["--no-open"])), None);
}

#[test]
fn task_authority_policy_reader_is_bounded() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("authority.json");
    std::fs::write(
        &path,
        vec![b'x'; (crate::core::a2a::task::policy_file::MAX_BYTES + 1) as usize],
    )
    .expect("write oversized policy");
    assert!(super::read_a2a_task_authority(&path).is_err());
}

#[cfg(unix)]
#[test]
fn task_authority_policy_reader_rejects_symlinks() {
    use std::os::unix::fs::symlink;

    let dir = tempfile::tempdir().expect("tempdir");
    let target = dir.path().join("target.json");
    let link = dir.path().join("authority.json");
    std::fs::write(&target, "{}").expect("write target");
    symlink(&target, &link).expect("symlink");
    assert!(super::read_a2a_task_authority(&link).is_err());
}

// `serve --a2a-signing-key <key>` put the key in the process list; the key
// file keeps it out and drops only the trailing line break.
#[cfg(feature = "http-server")]
#[test]
fn a2a_signing_key_file_is_read_without_its_line_break_and_refused_when_empty() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("a2a.key");
    std::fs::write(&path, " key with spaces \r\n").expect("write key");
    assert_eq!(
        super::read_secret_file(&path).as_deref(),
        Ok(" key with spaces ")
    );
    std::fs::write(&path, "\n").expect("write empty key");
    assert!(super::read_secret_file(&path).is_err());
    assert!(super::read_secret_file(&dir.path().join("missing")).is_err());
    assert!(super::A2A_SIGNING_KEY_ARGV_WARNING.contains("--a2a-signing-key-file"));
}

// `serve --auth-token <token>` put the Bearer token in the process list; the
// token file keeps it out and the warning names the replacements.
#[cfg(feature = "http-server")]
#[test]
fn auth_token_file_is_read_without_its_line_break_and_the_argv_form_warns() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("http.token");
    std::fs::write(&path, "tok-123\n").expect("write token");
    assert_eq!(super::read_secret_file(&path).as_deref(), Ok("tok-123"));
    std::fs::write(&path, "  \r\n").expect("write blank token");
    assert!(super::read_secret_file(&path).is_err());
    assert!(super::AUTH_TOKEN_ARGV_WARNING.contains("--auth-token-file"));
    assert!(super::AUTH_TOKEN_ARGV_WARNING.contains("LEAN_CTX_HTTP_TOKEN"));
}

// `cockpit --auth-token=<token>` put the Bearer token in the process list;
// the token file (flag or env) keeps it out, the argv form still works.
#[test]
fn cockpit_auth_token_reads_the_file_and_keeps_the_argv_form() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("cockpit.token");
    std::fs::write(&path, "tok-file\n").expect("write token");
    let path_str = path.to_str().expect("utf-8 path");
    let no_env = |_: &str| None;
    let token = |list: &[&str]| super::cockpit_auth_token(&args(list), no_env);

    assert_eq!(
        token(&[&format!("--auth-token-file={path_str}")]),
        Ok(Some("tok-file".into()))
    );
    assert_eq!(
        token(&["--auth-token-file", path_str]),
        Ok(Some("tok-file".into()))
    );
    assert_eq!(
        token(&["--auth-token=tok-argv"]),
        Ok(Some("tok-argv".into()))
    );
    assert_eq!(token(&["--token=tok-argv"]), Ok(Some("tok-argv".into())));
    assert_eq!(token(&[]), Ok(None));
    assert!(token(&["--auth-token=a", "--auth-token-file", path_str]).is_err());
    assert!(token(&["--auth-token-file"]).is_err());
    assert!(token(&["--auth-token-file", "--no-auth"]).is_err());
    let missing = token(&["--auth-token-file=/nonexistent/lean-ctx.token"]).unwrap_err();
    assert!(missing.contains("Invalid auth token file"), "{missing}");

    // LEAN_CTX_HTTP_TOKEN_FILE applies only while LEAN_CTX_HTTP_TOKEN is unset.
    let file_env = |name: &str| (name == "LEAN_CTX_HTTP_TOKEN_FILE").then(|| path_str.to_string());
    assert_eq!(
        super::cockpit_auth_token(&[], file_env),
        Ok(Some("tok-file".into()))
    );
    let both_env = |name: &str| match name {
        "LEAN_CTX_HTTP_TOKEN_FILE" => Some(path_str.to_string()),
        "LEAN_CTX_HTTP_TOKEN" => Some("tok-env".to_string()),
        _ => None,
    };
    assert_eq!(super::cockpit_auth_token(&[], both_env), Ok(None));
    assert!(super::COCKPIT_AUTH_TOKEN_ARGV_WARNING.contains("--auth-token-file"));
}

// The owner's `lean-ctx cockpit --project <dir>` was silently ignored.
#[test]
fn cockpit_project_flag_takes_both_forms_and_refuses_a_missing_dir() {
    let flag = |list: &[&str]| super::project_flag(&args(list));
    assert_eq!(
        flag(&["--project=/work/repo"]),
        Ok(Some("/work/repo".into()))
    );
    assert_eq!(
        flag(&["--project", "/work/repo", "--port=3334"]),
        Ok(Some("/work/repo".into()))
    );
    assert_eq!(flag(&["--port=3334"]), Ok(None));
    for refused in [
        &["--project"][..],
        &["--project="][..],
        &["--project", "--port", "3334"][..],
    ] {
        assert!(flag(refused).is_err(), "{refused:?}");
    }
}
