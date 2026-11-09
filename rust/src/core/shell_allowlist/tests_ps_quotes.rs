// SPDX-License-Identifier: Apache-2.0
//! PowerShell quoting and hashtable literals through the whole gate, as
//! `ctx_shell` runs it on a PowerShell host.

use super::ps_statements::HostGuard;

fn gate(cmd: &str, powershell: bool) -> Result<(), String> {
    let _lock = crate::core::data_dir::test_env_lock();
    let _allowlist = crate::test_env::VarGuard::set(
        "LEAN_CTX_SHELL_ALLOWLIST_OVERRIDE",
        "git,echo,sleep,true,ls",
    );
    let _host = HostGuard::powershell(powershell);
    super::enforce_shell_allowlist(cmd).map_err(|err| err.to_string())
}

fn blocked(cmds: &[&str], powershell: bool) {
    for cmd in cmds {
        assert!(
            gate(cmd, powershell).is_err(),
            "must be blocked (powershell host: {powershell}): {cmd:?}"
        );
    }
}

fn passes(cmds: &[&str], powershell: bool) {
    for cmd in cmds {
        if let Err(err) = gate(cmd, powershell) {
            panic!("should pass (powershell host: {powershell}): {cmd:?}\n{err}");
        }
    }
}

/// PowerShell reads `‘ ’ ‚ ‛` as `'` and `“ ” „` as `"`. Read as word
/// characters, they let a quote close where the gate saw none, so a command
/// after `;` ran unchecked. The hook mapped them; `ctx_shell` did not.
#[test]
fn typographic_quotes_are_read_as_powershell_reads_them() {
    blocked(
        &[
            "echo 'a\u{2019}; evil_binary; \u{2018}b'",
            "echo 'a\u{2018}; evil_binary; \u{201B}b'",
            "echo 'a\u{201A}; evil_binary; \u{2019}b'",
            "echo \"a\u{201D}; evil_binary; \u{201C}b\"",
            "echo \"a\u{201E}; evil_binary; \u{201C}b\"",
            "echo \u{201C}x\u{201D}; evil_binary",
            "echo \u{2018}x\u{2019}; evil_binary",
        ],
        true,
    );
    passes(
        &[
            "echo \u{201C}hello world\u{201D}",
            "echo \u{2018}a; b\u{2019}",
            "git commit -m \u{201C}it\u{2019}s done\u{201D}",
        ],
        true,
    );
}

/// A backslash is an ordinary character to PowerShell, not an escape, so
/// `"a\"` is a closed string there.
#[test]
fn backslash_does_not_escape_a_powershell_quote() {
    blocked(&["echo \"a\\\"; evil_binary; \\\"b\""], true);
    passes(&["echo C:\\Users\\x", "git -C \"C:\\repo\\\" status"], true);
}

/// The values of a hashtable literal `@{ k = v }` are statements: PowerShell
/// runs a bare word there as a command.
#[test]
fn hashtable_values_meet_the_allowlist() {
    blocked(
        &[
            "echo @{a=evil_binary}",
            "echo @{a=(evil_binary)}",
            "$h = @{a=evil_binary}",
            "$h = @{ a = 1; b = evil_binary }",
            "echo @{a=1\nb=evil_binary}",
            "echo @{a=@{b=evil_binary}}",
            "echo @{e={ evil_binary }}",
            "echo @{ (evil_binary) = 1 }",
            "echo @{ $(evil_binary) = 1 }",
            "echo @{ \"$(evil_binary)\" = 1 }",
            "echo @{ a = 1; (evil_binary) = 2 }",
            "echo @{a=1\n(evil_binary)=2}",
        ],
        true,
    );
    passes(
        &[
            "echo @{ a = 1; b = 'two'; c = \"three\" }",
            "echo @{ 'a b' = 1; \"c\" = 2; 3 = 4; Name-x.y = 5 }",
            "echo @{ $x = 1 }",
            "echo @{ (git rev-parse HEAD) = 1 }",
            "echo @{ a = $x }",
            "echo @{ a = $x; b = 2 }",
            "echo @{a=(git rev-parse HEAD)}",
            "echo @{e={ $_.Length / 1kb }}",
            "echo @{e={}}",
            "echo @{}",
        ],
        true,
    );
}

/// POSIX hosts keep their own reading.
#[test]
fn posix_hosts_are_unchanged() {
    passes(
        &[
            "echo 'it\u{2019}s'",
            "echo \"a\\\"b\"",
            "echo @{a=evil_binary}",
        ],
        false,
    );
}
