// SPDX-License-Identifier: Apache-2.0
//! #2267: a variable assigned literal text around a `$(…)` was bound to
//! lean-ctx's state directory alone, dropping the literal text, so a `cp` to
//! `"$backup"` (`backup=".git/backups/x.$(date …).bak"`) was refused as a
//! state write. Literal text now stays literal where every substitution in
//! the value is a `date +FORMAT` whose output holds no `/`, `.` or `\` and the
//! literal prefix is a relative path inside the working directory; the
//! substitution is then judged as a glob (`x.*.bak`). Every other run-time
//! value keeps the fail-closed reading, and its refusal names the word the
//! command wrote.

use super::*;

fn list() -> Allowlist {
    allow(&["cp", "mv", "rm", "touch", "date", "echo", "cat"])
}

/// The issue's table rows that must run, and its real-world form.
const ALLOWED: &[&str] = &[
    "b=\"x.$(date +%Y).bak\" && cp -p f \"$b\"",
    "backup=\".git/backups/reassessment-20261104.md.$(date -u +%Y%m%dT%H%M%SZ).$$.bak\" && cp -p --no-clobber reassessment-20261104.md \"$backup\"",
    "cp -p f \"x.$(date +%Y).bak\"",
    "b=\"$(date)\" && cp -p f b2",
    "b=\"x.$RANDOM.bak\" && cp -p f \"$b\"",
    "ts=\"$(date +%Y)\"; b=\"x.$ts.bak\"; cp -p f \"$b\"",
    "b=\"build/out-$(date --utc +%s)\" && mv f \"$b\"",
    "b=\"x-$(date +%Y)-$(date +%m).log\"; touch \"$b\"",
];

/// Values whose run-time part may name lean-ctx's state: refused.
const REFUSED: &[&str] = &[
    // Nothing but a substitution: its output may be an absolute path.
    "b=\"$(date +%Y)\" && cp -p f \"$b\"",
    "b=\"$(echo fixed)\" && cp -p f \"$b\"",
    "b=\"$(date +%Y)$(date +%m)\" && cp -p f \"$b\"",
    "b=\"${x:-$(date +%Y)}\" && cp -p f \"$b\"",
    "b=\"`date +%Y`\" && cp -p f \"$b\"",
    // Backticks keep the opaque reading, also after literal text.
    "b=\"x.`date +%Y`.bak\" && cp -p f \"$b\"",
    // A substitution with unbounded output may hold `../` or `/`.
    "b=\"build/$(echo ../../x)\" && rm -rf \"$b\"",
    "b=\"x.$(cat name).bak\" && cp -p f \"$b\"",
    "b=\"x.$(date +%Y/%m).bak\" && cp -p f \"$b\"",
    "b=\"x.$(date +%Y.%m).bak\" && cp -p f \"$b\"",
    "b=\"x.$(date +%a).bak\" && cp -p f \"$b\"",
    "b=\"x.$(date -d @0 +%s).bak\" && cp -p f \"$b\"",
    // A prefix that leaves the working directory or is absolute.
    "b=\"../$(date +%s)\" && rm -rf \"$b\"",
    "b=\"x/../../$(date +%s)\" && rm -rf \"$b\"",
    "b=\"/tmp/$(date +%s)\" && rm -rf \"$b\"",
    "b=\"$HOME/.config/lean-ctx/$(date +%s)\" && rm -rf \"$b\"",
    "b=\"${XDG_CONFIG_HOME}/lean-ctx/$(date +%s)\" && rm -rf \"$b\"",
    "b=~/.config/lean-ctx/x$(date +%s) && rm -rf \"$b\"",
    "b=\"$d/$(date +%s)\" && rm -rf \"$b\"",
    // `date` redefined on the line.
    "date() { echo ../../x; }; b=\"x.$(date +%s)\" && rm -rf \"$b\"",
    "PATH=/tmp/bin:$PATH; b=\"x.$(date +%s)\" && rm -rf \"$b\"",
    "alias date=cat; b=\"x.$(date +%s)\" && rm -rf \"$b\"",
    // Appended values are not followed.
    "b=x; b+=\".$(date +%s)\" && rm -rf \"$b\"",
];

#[test]
fn gh2267_literal_text_stays_literal_without_an_allowlist() {
    let _lock = lean_ctx::core::data_dir::test_env_lock();
    for cmd in ALLOWED {
        let verdict = check_unconditional_blocked_only(cmd);
        assert!(verdict.is_ok(), "must pass: {cmd}: {verdict:?}");
    }
}

#[test]
fn gh2267_literal_text_stays_literal_with_an_allowlist() {
    let _lock = lean_ctx::core::data_dir::test_env_lock();
    for cmd in ALLOWED {
        let verdict = check_all_segments(cmd, &list());
        assert!(verdict.is_ok(), "must pass: {cmd}: {verdict:?}");
    }
}

#[test]
fn gh2267_unbounded_run_time_values_stay_refused() {
    let _lock = lean_ctx::core::data_dir::test_env_lock();
    for cmd in REFUSED {
        assert!(
            check_unconditional_blocked_only(cmd).is_err(),
            "must refuse without an allowlist: {cmd}"
        );
        assert!(
            check_all_segments(cmd, &list()).is_err(),
            "must refuse with an allowlist: {cmd}"
        );
    }
}

/// The literal prefix is judged where the command runs: inside the state
/// directory, or next to it with a prefix of its name, the glob reaches it.
#[test]
fn gh2267_prefix_reaching_the_state_is_refused() {
    let _lock = lean_ctx::core::data_dir::test_env_lock();
    let state = lean_ctx::core::paths::data_dir_read_only().expect("data dir");
    let name = state
        .file_name()
        .expect("named data dir")
        .to_string_lossy()
        .to_string();
    let parent = state.parent().expect("data dir parent").to_path_buf();
    let inside = "b=\"x.$(date +%s).bak\" && cp -p f \"$b\"";
    let next_to = format!("b=\"{}$(date +%s)\" && rm -rf \"$b\"", &name[..3]);
    for (cwd, cmd) in [(state.clone(), inside.to_string()), (parent, next_to)] {
        let (bare, listed) = super::with_run_cwd(Some(cwd), || {
            (
                check_unconditional_blocked_only(&cmd),
                check_all_segments(&cmd, &list()),
            )
        });
        assert!(bare.is_err(), "must refuse without an allowlist: {cmd}");
        assert!(listed.is_err(), "must refuse with an allowlist: {cmd}");
    }
}

/// A PowerShell host reads `$(…)` as its own subexpression: not followed.
#[test]
fn gh2267_powershell_host_keeps_the_opaque_reading() {
    use super::ps_statements::HostGuard;
    let _lock = lean_ctx::core::data_dir::test_env_lock();
    let _host = HostGuard::powershell(true);
    let cmd = "b=\"x.$(date +%Y).bak\"; cp -p f \"$b\"";
    assert!(check_unconditional_blocked_only(cmd).is_err(), "{cmd}");
}

/// The refusal names the word the command wrote, not the state directory
/// that stands in for its run-time value, and gives no read hint for a copy.
#[test]
fn gh2267_refusal_names_the_written_word() {
    let _lock = lean_ctx::core::data_dir::test_env_lock();
    let err = check_unconditional_blocked_only("b=\"$(date +%Y)\" && cp -p f \"$b\"")
        .expect_err("pure substitution stays refused")
        .to_string();
    assert!(err.contains("`cp -p f $b`"), "{err}");
    let state = lean_ctx::core::paths::data_dir_read_only().expect("data dir");
    assert!(!err.contains(&state.display().to_string()), "{err}");
    assert!(!err.contains("To read the file"), "{err}");
}
