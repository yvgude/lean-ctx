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
