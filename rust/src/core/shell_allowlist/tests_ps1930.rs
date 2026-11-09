// SPDX-License-Identifier: Apache-2.0
//! #1930: PowerShell statements, expressions and script blocks.

use super::ps_statements::HostGuard;
use super::{check_all_segments, tests::allow};

const POWERSHELL: bool = true;
const POSIX: bool = false;

fn passes(cmd: &str, powershell: bool) {
    let _host = HostGuard::powershell(powershell);
    let list = allow(&["git", "echo", "sleep", "true", "ls"]);
    if let Err(err) = check_all_segments(cmd, &list) {
        panic!("should pass (powershell host: {powershell}): {cmd:?}\n{err}");
    }
}

fn blocked(cmd: &str, powershell: bool) -> String {
    let _host = HostGuard::powershell(powershell);
    let list = allow(&["git", "echo", "sleep", "true", "ls"]);
    match check_all_segments(cmd, &list) {
        Ok(()) => panic!("must be blocked (powershell host: {powershell}): {cmd:?}"),
        Err(err) => err.to_string(),
    }
}

/// Every row of the report — ordinary PowerShell 5.1 statement lines that
/// were rejected as mis-splits.
#[test]
fn reported_statement_lines_pass_under_powershell() {
    for cmd in [
        "$m = @(Select-String -Path x.log -Pattern err)",
        "$t = Get-ScheduledTask -TaskName x\nif ($t) {\n  Write-Output yes\n}",
        "$t.Actions | ForEach-Object { $_.Execute }",
        "try {\n  Get-Date\n} catch {\n  Write-Output $_\n}",
        "$path = [Environment]::GetEnvironmentVariable('Path','User')",
        "$sw = [Diagnostics.Stopwatch]::StartNew()",
        "$p.WaitForExit()",
        "while (-not $p.HasExited) { Start-Sleep -Milliseconds 200 }",
    ] {
        passes(cmd, POWERSHELL);
    }
}

#[test]
fn control_flow_and_expressions_pass_under_powershell() {
    for cmd in [
        "if (Test-Path x) { Get-Item x } elseif ($y -eq 2) { Get-Date } else { Write-Output no }",
        "try { Get-Date } catch [System.IO.IOException], [TimeoutException] { Write-Output x } finally { Get-Date }",
        "do { Start-Sleep 1 } while (-not $p.HasExited)",
        "do { Start-Sleep 1 } until ($p.HasExited)",
        "foreach ($f in (Get-ChildItem *.log)) { Get-Content $f }",
        "for ($i = 0; $i -lt 3; $i++) { Write-Output $i }",
        "$n = ($items | Measure-Object).Count",
        "$n = $n + 1",
        "$sw.Elapsed.TotalSeconds.ToString('N1')",
        "if ([int]$x -gt 3) { Get-Date }",
        "if ($LASTEXITCODE -ne 0) { Write-Output failed }",
        "$files[0].Name.Trim().ToLower()",
        "Get-ChildItem | Where-Object { $_.Length -gt 100 } | Select-Object @{n='kb'; e={ $_.Length / 1kb }}",
        "Get-ChildItem | % { $_.Name }",
        "if ($x) { $n = $n + 1 }",
        "foreach ($f in $files) { $total += $f.Length }",
        "for ($i = 0; $i -lt 3; $i++) { $i++ }",
        "Get-ChildItem | ForEach-Object { $_.Name -replace 'a', 'b' }",
        "try { $sw = [Diagnostics.Stopwatch]::StartNew(); Get-Date } catch { $err = $_ }",
    ] {
        passes(cmd, POWERSHELL);
    }
}

/// Under a POSIX shell the keyword forms and single operands still work —
/// the commands inside them are what gets checked.
#[test]
fn single_operand_forms_pass_under_posix() {
    for cmd in [
        "$m = @(Select-String -Path x.log -Pattern err)",
        "$t = Get-ScheduledTask -TaskName x\nif ($t) {\n  Write-Output yes\n}",
        "$t.Actions | ForEach-Object { $_.Execute }",
        "try {\n  Get-Date\n} catch {\n  Write-Output $_\n}",
        "$path = [Environment]::GetEnvironmentVariable('Path','User')",
        "$p.WaitForExit()",
    ] {
        passes(cmd, POSIX);
    }
}

/// The commands inside PowerShell syntax are what gets validated: a
/// destructive cmdlet anywhere in the statement blocks it, on either host.
#[test]
fn commands_inside_powershell_syntax_are_still_checked() {
    for cmd in [
        "$m = @(Remove-Item x)",
        "$m = $(Remove-Item x)",
        "if (Remove-Item x) { Get-Date }",
        "if ($t) { Remove-Item y }",
        "if ($t) { Get-Date } else { Remove-Item y }",
        "try { Remove-Item z } catch { }",
        "try { Get-Date } catch { Remove-Item z }",
        "try { Get-Date } finally { Remove-Item z }",
        "while ($true) { Stop-Process -Name x }",
        "do { Remove-Item x } while ($true)",
        "$n = (Remove-Item x).Count",
        "$x = \"a$(Remove-Item y)\"",
        "if ($x) { $n = (Remove-Item y) }",
        "Get-ChildItem | ForEach-Object { $n = $_; Remove-Item $n }",
    ] {
        blocked(cmd, POWERSHELL);
        blocked(cmd, POSIX);
    }
}

/// These all ran unchecked before #1930: the loop header was skipped as a
/// POSIX `for` header, and script-block bodies were never looked into.
#[test]
fn script_block_bodies_are_validated() {
    for cmd in [
        "foreach ($f in $files) { Remove-Item $f }",
        "Get-Date; for ($i = 0; $i -lt 3; $i++) { Remove-Item x }",
        "Get-ChildItem | ForEach-Object { Remove-Item $_ }",
        "Get-ChildItem | % { Remove-Item $_ }",
        "Get-ChildItem | Where-Object { Remove-Item $_ }",
        "Get-ChildItem | ? { Remove-Item $_ }",
        "Get-ChildItem | foreach { Remove-Item $_ }",
        "Get-ChildItem | Select-Object @{n='x'; e={ Remove-Item $_ }}",
    ] {
        let err = blocked(cmd, POWERSHELL);
        assert!(err.contains("destructive verb"), "{cmd:?}: {err}");
        // Under POSIX a strict loop header may block first; blocked either way.
        blocked(cmd, POSIX);
    }
}

/// Only read-only .NET members are inert; anything that writes, starts a
/// process, compiles code or calls through the engine stays blocked.
#[test]
fn executing_dotnet_members_stay_blocked() {
    for cmd in [
        "[Diagnostics.Process]::Start('cmd.exe')",
        "$p = [System.Diagnostics.Process]::Start('cmd.exe')",
        "[IO.File]::WriteAllText('x', 'y')",
        "[IO.File]::Delete('x')",
        "if ($x) { [IO.File]::Delete('y') }",
        "[scriptblock]::Create('Remove-Item x')",
        "$sb = [scriptblock]'Remove-Item x'",
        "$ExecutionContext.InvokeCommand.InvokeScript('Remove-Item x')",
        "$Host.Runspace.CreateNestedPipeline()",
        "$p.Kill()",
        "$items.Where({ Remove-Item $_ })",
        "(Get-Item 'C:\\Temp\\victim.txt').Delete()",
        "$x > out.txt",
        "$env:PATH = 'C:\\attacker'",
        "if ($x) { $env:PATH = 'C:\\attacker' }",
    ] {
        blocked(cmd, POWERSHELL);
        blocked(cmd, POSIX);
    }
}

/// PowerShell's call operator `&` runs its target with the arguments after
/// it. Only a literal program name or path meets the allowlist; the target
/// was read as an inert expression before, so any program ran.
#[test]
fn call_operator_target_meets_the_allowlist() {
    for cmd in [
        "& git status",
        "& 'C:/Program Files/Git/cmd/git' status",
        "& { git status }",
        // PowerShell needs no blank inside a script block's braces.
        "&{git status}",
        "& {git status}",
        "& { & { & { & { git status } } } }",
        "git status 2>&1",
        "git fetch & git status",
    ] {
        passes(cmd, POWERSHELL);
    }
    for cmd in [
        "& 'evil_binary'",
        "&'evil_binary'",
        "git status | & evil_binary",
        "git status && & evil_binary",
        "& git status & 'evil_binary'",
        // A cmdlet target runs its script blocks like the plain cmdlet does.
        "1 | & % { evil_binary }",
        "& ForEach-Object { evil_binary }",
        "& Where-Object { evil_binary }",
        "& Select-Object @{e={evil_binary}}",
        // The nesting bound refuses a block; it never reads it as a command.
        // Five levels already reach the bound every compound form shares
        // (five nested `if` blocks are refused the same way).
        "& { & { & { & { & { git status } } } } }",
        "if ($a) { if ($b) { if ($c) { if ($d) { if ($e) { git status } } } } }",
        "& { & { & { & { & { & { echo x; evil_binary } } } } } }",
        // Conservative: PowerShell reports a missing expression after a bare
        // `&` at the end of a line, but the next line must not pass as inert.
        "&\n'evil_binary'",
        "&\r\n'evil_binary'",
        // A `$` outside single quotes names the program at run time.
        "& \"$env:TEMP/git\" status",
        "& 'cmd' ,'/c','evil_binary'",
        "& $c",
        "& $env:ComSpec",
        "& (Get-Command evil_binary)",
        "& @('evil_binary')",
        "& { git status; evil_binary }",
        "& { git status } evil_binary",
        "git status & 'evil_binary'",
        "(& 'evil_binary')",
        "if ($x) { & 'evil_binary' }",
        "Get-Command evil_binary | ForEach-Object { & $_ }",
        "$p = & 'evil_binary'",
        // Dot-sourcing is refused before any reader sees it.
        ". ./evil.ps1",
        ". 'evil.ps1'",
    ] {
        blocked(cmd, POWERSHELL);
    }
    // POSIX: `&` stays a background operator; the next command is checked.
    blocked("sleep 1 & evil_binary", POSIX);
    passes("sleep 1 & git status", POSIX);
}

/// A call operator inside a substitution keeps its target a command: the
/// substitution body used to be split without it, and the quoted target
/// then read as an inert string.
#[test]
fn call_operator_inside_substitution_meets_the_allowlist() {
    let _lock = crate::core::data_dir::test_env_lock();
    let _allowlist = crate::test_env::VarGuard::set(
        "LEAN_CTX_SHELL_ALLOWLIST_OVERRIDE",
        "git,echo,Write-Output",
    );
    let _host = HostGuard::powershell(POWERSHELL);
    for cmd in [
        "Write-Output $(& 'evil_binary')",
        "echo \"$(& 'evil_binary')\"",
        "git log --format=$(& 'cmd' ,'/c','evil_binary')",
    ] {
        assert!(
            super::check_substitution_in_args(cmd).is_err(),
            "must be blocked: {cmd:?}"
        );
    }
    let allowed = "Write-Output $(& git rev-parse HEAD)";
    let result = super::check_substitution_in_args(allowed);
    assert!(result.is_ok(), "{allowed:?}: {result:?}");
    // A bare `&` before `;` inside a substitution is refused like one
    // before a line break.
    assert!(super::check_substitution_in_args("Write-Output $(& ;evil_binary)").is_err());
    // `$(&)` names no target; PowerShell rejects it as a parse error, so
    // nothing runs and there is nothing to check.
    let empty = "Write-Output $(&)";
    assert!(
        super::check_substitution_in_args(empty).is_ok(),
        "{empty:?}"
    );
    passes(empty, POWERSHELL);
}

/// A literal call target that runs a script block (`& % { … }`) counts
/// against the same depth bound as the plain `% { … }`, so the walk cannot
/// recurse without limit through nested targets.
#[test]
fn call_operator_targets_share_the_depth_bound() {
    passes("& % { & % { & % { & % { git status } } } }", POWERSHELL);
    let six = "& % { & % { & % { & % { & % { & % { git status } } } } } }";
    blocked(six, POWERSHELL);
    let deep = format!("{}git status{}", "& % { ".repeat(2000), " }".repeat(2000));
    blocked(&deep, POWERSHELL);
}

/// A trailing redirection binds a `& { … }` block's output, as it does for
/// a plain brace group; anything else after the `}` stays refused.
#[test]
fn call_operator_block_accepts_a_redirection() {
    for cmd in ["& { git status } > out", "&{git status} 2>&1"] {
        passes(cmd, POWERSHELL);
    }
    for cmd in [
        "& { git status } evil_binary",
        "& { git status } > out evil_binary",
        "& { evil_binary } > out",
    ] {
        blocked(cmd, POWERSHELL);
    }
}

/// Call-operator forms the walker refuses conservatively or reads loosely.
#[test]
fn call_operator_edge_forms() {
    for cmd in [
        // A bare `&` before a line break, after a pipe, with a lone CR or a
        // block comment between.
        "git status | &\n'evil_binary'",
        "&\r'evil_binary'",
        "git status | &\r'evil_binary'",
        "& <# #>\n'evil_binary'",
        // A `$` outside single quotes anywhere in the first word.
        "& 'git'$x",
        "& \"a`\"$x\"",
    ] {
        blocked(cmd, POWERSHELL);
    }
    // Tokenizing folds `'g''it'` to `git`; PowerShell would run a program
    // named `g'it`, so one of that name on PATH is the only exposure.
    passes("& 'g''it' status", POWERSHELL);
}

/// The whole gate under a PowerShell host, as `ctx_shell` and the hooks run
/// it: comment stripping and the substitution scan happen before the walk.
fn gate(cmd: &str) -> Result<(), String> {
    let _lock = crate::core::data_dir::test_env_lock();
    let _allowlist = crate::test_env::VarGuard::set(
        "LEAN_CTX_SHELL_ALLOWLIST_OVERRIDE",
        "git,echo,sleep,true,ls",
    );
    let _host = HostGuard::powershell(POWERSHELL);
    super::enforce_shell_allowlist(cmd).map_err(|err| err.to_string())
}

/// In argument mode PowerShell runs a `( … )` group as a pipeline and passes
/// its output: `echo (cmd)` runs `cmd`. The group's commands used to read as
/// inert arguments, so any program ran.
#[test]
fn argument_groups_meet_the_allowlist() {
    for cmd in [
        "Write-Output (evil_binary)",
        "echo (evil_binary)",
        "echo x (evil_binary)",
        "echo x# (evil_binary)",
        "git status (evil_binary)",
        "& git status (evil_binary)",
        "git log --format=(evil_binary)",
        "echo ('x' + (evil_binary))",
        "echo (git status; evil_binary)",
        "echo (git status | evil_binary)",
        "echo \"a`\"b\" (evil_binary)",
        "if ($x) { echo (evil_binary) }",
        "Get-ChildItem | ForEach-Object { echo (evil_binary) }",
        // Unclosed: the rest cannot be read.
        "echo (git status",
        "echo 'x (evil_binary)",
    ] {
        assert!(gate(cmd).is_err(), "must be blocked: {cmd:?}");
    }
    let deep = format!("echo {}git status{}", "(echo ".repeat(6), ")".repeat(6));
    assert!(gate(&deep).is_err(), "nesting bound");
    let huge = format!("echo {}git status{}", "(".repeat(5000), ")".repeat(5000));
    assert!(gate(&huge).is_err(), "nesting bound");
    for cmd in [
        "echo (git rev-parse HEAD)",
        "echo x (git status) y",
        "echo (echo (git status))",
        "if ($x -eq 1) { echo (git rev-parse HEAD) }",
        "foreach ($f in (Get-ChildItem *.log)) { Get-Content $f }",
        "$n = ($items | Measure-Object).Count",
        "echo '(not a command)'",
        "echo \"(still text)\"",
        "git log --format='%h (%s)'",
    ] {
        if let Err(err) = gate(cmd) {
            panic!("should pass: {cmd:?}\n{err}");
        }
    }
    // POSIX shells refuse `echo (x)` as a syntax error; nothing to check there.
    passes("echo x", POSIX);
}

/// A function body's commands run when the function is called, so a `( … )`
/// group in their arguments meets the allowlist like a top-level one.
#[test]
fn argument_groups_in_function_bodies_meet_the_allowlist() {
    for cmd in [
        "function f { echo (evil_binary) }; f",
        "function f { git log --format=(evil_binary) }",
        "f() { echo (evil_binary); }; f",
    ] {
        assert!(gate(cmd).is_err(), "must be blocked: {cmd:?}");
    }
    for cmd in [
        "function f { echo (git rev-parse HEAD) }; f",
        "function f { echo '(evil_binary)' }; f",
    ] {
        if let Err(err) = gate(cmd) {
            panic!("should pass: {cmd:?}\n{err}");
        }
    }
}

/// PowerShell's block comment `<# … #>` ends at `#>` and may span lines. Read
/// as a POSIX `#` comment, its closing `#>` hid the rest of the line, so a
/// command after `; ` or a substitution after it went unchecked.
#[test]
fn block_comments_hide_no_commands() {
    for cmd in [
        "echo x <# c #> ; evil_binary",
        "echo <# a #> <# b #>; evil_binary",
        "echo x <# c #>\nevil_binary",
        "echo <# #> $(evil_binary)",
        "echo <# #> (evil_binary)",
        "echo <# multi\nline #>; evil_binary",
        // `<#>` opens a comment; it does not close one.
        "echo <#> ; evil_binary #>; evil_binary",
        // Unterminated, or attached to a word: refused.
        "echo <# c ; evil_binary",
        "echo x<# c #> ; git status",
    ] {
        assert!(gate(cmd).is_err(), "must be blocked: {cmd:?}");
    }
    for cmd in [
        "echo x <# c #>",
        "echo x <# c #> ; git status",
        "<# note #> git status",
        // The comment is a blank: the line break inside it ends no statement.
        "echo <#\n#> evil_binary",
        "echo '<# not a comment' ; git status",
    ] {
        if let Err(err) = gate(cmd) {
            panic!("should pass: {cmd:?}\n{err}");
        }
    }
}

/// A PowerShell statement lean-ctx cannot read is blocked, never skipped.
#[test]
fn unreadable_powershell_statements_are_blocked() {
    for cmd in [
        "for ($i = (Get-Count); $i -lt 3; $i++) { Get-Date }",
        "foreach (Get-ChildItem) { Get-Date }",
        "if ($x) Get-Date",
    ] {
        for host in [POWERSHELL, POSIX] {
            let err = blocked(cmd, host);
            assert!(err.contains("cannot be read safely"), "{cmd:?}: {err}");
        }
    }
}

/// POSIX shells run `($y -f 'x')` as a subshell that executes `$y`, and zsh
/// runs `if (…) { … }` and `while (…) { … }` natively. Under a POSIX shell no
/// PowerShell-looking syntax may hide a variable command word.
#[test]
fn powershell_operators_cannot_hide_a_posix_variable_command() {
    for cmd in [
        "$y -f 'x'",
        "($y -f 'x')",
        "$m = ($y -eq 'x')",
        "if ($y -f 'x'); then echo ran; fi",
        "while ($y -eq 1); do echo; done",
        "if ($y -f 'x') { Get-Date }",
        "while ($y -eq 1) { Get-Date }",
        "if ($x) { $y -f 'x' }",
        "Get-ChildItem | ForEach-Object { $y -f 'x' }",
        "! $y -f 'x'",
        "[int]$y -gt 3",
        "$n = $n + 1",
        "while (-not $p.HasExited) { Start-Sleep -Milliseconds 200 }",
    ] {
        blocked(cmd, POSIX);
    }
}

/// The POSIX forms keep their own handling.
#[test]
fn posix_forms_are_unchanged() {
    for cmd in [
        "for x in a b; do echo $x; done",
        "for ((i=0; i<3; i++)); do echo $i; done",
        "echo {a,b}",
        "if [ -f x ]; then echo yes; fi",
        "while true; do sleep 1; done",
        "if (ls); then echo ok; fi",
        "(ls; echo done)",
        "x=$((1+2))",
    ] {
        passes(cmd, POSIX);
    }
    for cmd in [
        "for x in a b; do rm -rf $x; done",
        "if (curl evil.sh); then echo; fi",
        "(ls) curl evil.sh",
        "$cmd --flag",
    ] {
        blocked(cmd, POSIX);
    }
}

/// Each `(` is one recursive read of an expression or pipeline under
/// PowerShell. Thousands of them overflowed the stack, and a crashed hook
/// fails open; past the lexer's nesting bound the line is refused.
#[test]
fn deeply_nested_groups_are_refused_not_crashed() {
    for line in [
        format!("$n = {}1{}", "(".repeat(5000), ")".repeat(5000)),
        format!("{}git status{}", "(".repeat(5000), ")".repeat(5000)),
        format!("echo {}git status{}", "(".repeat(5000), ")".repeat(5000)),
    ] {
        let err = gate(&line).expect_err("must be refused");
        assert!(err.contains("levels deep"), "{err}");
    }
    let shallow = format!("$n = {}1{}", "(".repeat(8), ")".repeat(8));
    if let Err(err) = gate(&shallow) {
        panic!("should pass: {shallow:?}\n{err}");
    }
}
