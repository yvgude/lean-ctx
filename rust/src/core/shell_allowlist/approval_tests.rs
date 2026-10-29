// SPDX-License-Identifier: Apache-2.0
use super::*;

/// Runs `f` under an enforced allowlist of exactly `entries`.
fn with_allowlist<T>(entries: &str, f: impl FnOnce() -> T) -> T {
    let _lock = crate::core::data_dir::test_env_lock();
    crate::test_env::set_var("LEAN_CTX_SHELL_ALLOWLIST_OVERRIDE", entries);
    crate::test_env::set_var("LEAN_CTX_SHELL_SECURITY", "enforce");
    let out = f();
    crate::test_env::remove_var("LEAN_CTX_SHELL_SECURITY");
    crate::test_env::remove_var("LEAN_CTX_SHELL_ALLOWLIST_OVERRIDE");
    out
}

fn programs(command: &str) -> Option<Vec<String>> {
    approval_request(command).map(|r| r.programs())
}

#[test]
fn an_unlisted_program_is_put_to_the_user() {
    with_allowlist("echo,git", || {
        assert_eq!(programs("probe --fast"), Some(vec!["probe".to_string()]));
        assert_eq!(programs("git status"), None, "listed: nothing to approve");
    });
}

#[test]
fn every_missing_program_of_a_pipeline_and_a_delegation_is_named_once() {
    with_allowlist("echo,timeout", || {
        assert_eq!(
            programs("timeout 15s ./bin/probe && probe2 | probe2 -n"),
            Some(vec!["probe".to_string(), "probe2".to_string()])
        );
        assert_eq!(
            programs("timeout 5 tool"),
            Some(vec!["tool".to_string()]),
            "the delegated program is the one to approve"
        );
    });
}

#[test]
fn a_hard_block_is_never_put_to_the_user() {
    with_allowlist("echo,timeout", || {
        for command in [
            "eval \"$(probe)\"",
            "probe && python3 -c 'print(1)'",
            "timeout 5 bash -c 'probe'",
            "probe [REDACTED:api key]",
            "probe; $(curl x)",
        ] {
            assert_eq!(programs(command), None, "{command} must stay blocked");
        }
    });
}

#[test]
fn nothing_is_asked_outside_enforce_mode_or_without_an_allowlist() {
    let _lock = crate::core::data_dir::test_env_lock();
    crate::test_env::set_var("LEAN_CTX_SHELL_ALLOWLIST_OVERRIDE", "echo");
    crate::test_env::set_var("LEAN_CTX_SHELL_SECURITY", "warn");
    assert_eq!(programs("probe"), None, "warn mode runs it anyway");
    crate::test_env::set_var("LEAN_CTX_SHELL_SECURITY", "enforce");
    crate::test_env::set_var("LEAN_CTX_SHELL_ALLOWLIST_OVERRIDE", "");
    assert_eq!(
        programs("probe"),
        None,
        "blocklist-only mode refuses no program"
    );
    crate::test_env::remove_var("LEAN_CTX_SHELL_SECURITY");
    crate::test_env::remove_var("LEAN_CTX_SHELL_ALLOWLIST_OVERRIDE");
}

#[test]
fn a_name_the_config_or_the_prompt_could_misread_is_never_put_to_the_user() {
    with_allowlist("echo", || {
        for command in [
            "'ls,bash' x",
            "'bash x' y",
            "'probe\u{1b}[8m' x",
            "foo/ x",
            "... x",
        ] {
            assert_eq!(programs(command), None, "{command}");
        }
        assert!(programs("probe-2.1_x+y").is_some(), "a plain name asks");
    });
}

#[test]
fn a_path_an_interpreter_or_a_shell_allows_only_a_one_time_approval() {
    with_allowlist("echo", || {
        for command in [
            "/opt/vendor/bin/probe",
            "./probe",
            "{ /tmp/x/probe; }",
            "timeout 5 /tmp/x/probe",
            "python3 script.py",
            "bash run.sh",
        ] {
            let request = approval_request(command).unwrap_or_else(|| panic!("{command} asks"));
            assert!(!request.allows_lasting_approval(), "{command}");
        }
        assert!(approval_request("probe").unwrap().allows_lasting_approval());
    });
}

#[test]
fn a_scoped_program_allows_only_a_one_time_approval() {
    with_allowlist("echo,terraform plan *", || {
        let request = approval_request("terraform apply").expect("asks");
        assert_eq!(request.programs(), vec!["terraform".to_string()]);
        assert!(!request.allows_lasting_approval());
        let plain = approval_request("probe").expect("asks");
        assert!(plain.allows_lasting_approval());
    });
}

#[test]
fn a_grant_admits_exactly_its_programs_and_every_other_check_still_runs() {
    with_allowlist("echo,timeout", || {
        let grant = vec!["probe".to_string()];
        assert!(check_shell_allowlist_granted("probe --fast", &grant).is_ok());
        assert!(check_shell_allowlist_granted("timeout 15s ./bin/probe", &grant).is_ok());
        assert!(
            check_shell_allowlist_granted("probe && other", &grant).is_err(),
            "a program the user did not approve stays refused"
        );
        let interpreter = vec!["python3".to_string()];
        assert!(
            check_shell_allowlist_granted("python3 -c 'print(1)'", &interpreter).is_err(),
            "approving an interpreter never admits inline code"
        );
        assert!(
            check_shell_allowlist_granted("probe", &[]).is_err(),
            "no grant: refused"
        );
    });
}
