// SPDX-License-Identifier: Apache-2.0
//! #1442: PowerShell cmdlet coverage — verb-prefix matching integration tests.

use super::{check_all_segments, tests::allow};

/// Safe PowerShell cmdlets pass through check_all_segments without being in
/// the explicit allowlist.
#[test]
fn powershell_safe_cmdlets_pass_enforcement() {
    let _host = super::super::ps_statements::HostGuard::powershell(true);
    let list = allow(&["git", "cargo"]);
    for cmd in [
        "Get-Date",
        "Get-ChildItem C:\\Temp",
        "Test-Path C:\\Temp",
        "Get-Content file.txt",
        "Write-Output hello",
        "Measure-Object",
        "Select-Object Name,Length",
        "Format-Table",
        "ConvertTo-Json",
        "ConvertFrom-Csv",
        "Sort-Object Name",
        "Where-Object Length -gt 100",
        "ForEach-Object { $_.Name }",
        "Compare-Object $a $b",
        "Join-Path C:\\Users test",
        "Split-Path C:\\Users\\test",
        "Resolve-Path .",
    ] {
        assert!(
            check_all_segments(cmd, &list).is_ok(),
            "PowerShell cmdlet should pass: {cmd}"
        );
    }
}

/// Noun-level safe exceptions pass despite blocked verb prefix.
#[test]
fn powershell_safe_exceptions_pass() {
    let list = allow(&["git"]);
    for cmd in [
        "Set-Location C:\\Users",
        "New-Guid",
        "New-TimeSpan -Hours 1",
        "Start-Sleep -Seconds 5",
        "Clear-Host",
        "Out-String",
        "Out-Null",
        "Out-Host",
        "Import-Csv data.csv",
        "Import-Clixml backup.xml",
        "Export-Csv results.csv",
        "Read-Host",
    ] {
        assert!(
            check_all_segments(cmd, &list).is_ok(),
            "Safe exception should pass: {cmd}"
        );
    }
}

/// New-Object is an execution primitive (download cradles, COM objects) and must
/// be blocked despite being a "constructor".
#[test]
fn powershell_new_object_blocked() {
    let list = allow(&["git"]);
    assert!(check_all_segments("New-Object System.IO.MemoryStream", &list).is_err());
    assert!(check_all_segments("New-Object Net.WebClient", &list).is_err());
    assert!(check_all_segments("New-Object -ComObject WScript.Shell", &list).is_err());
}

/// Import-Module is blocked (can execute arbitrary code), but Import-Csv is safe.
#[test]
fn powershell_import_module_blocked_import_csv_safe() {
    let list = allow(&["git"]);
    assert!(check_all_segments("Import-Csv data.csv", &list).is_ok());
    assert!(check_all_segments("Import-Clixml backup.xml", &list).is_ok());
    assert!(check_all_segments("Import-Module ./evil.psm1", &list).is_err());
    assert!(check_all_segments("Import-PSSession $s", &list).is_err());
}

/// Out-File is blocked (writes to disk), but Out-String/Out-Null are safe.
#[test]
fn powershell_out_file_blocked_out_string_safe() {
    let list = allow(&["git"]);
    assert!(check_all_segments("Out-String", &list).is_ok());
    assert!(check_all_segments("Out-Null", &list).is_ok());
    assert!(check_all_segments("Out-Host", &list).is_ok());
    assert!(check_all_segments("Out-File report.txt", &list).is_err());
    assert!(check_all_segments("Out-GridView", &list).is_err());
}

/// Destructive PowerShell cmdlets are blocked.
#[test]
fn powershell_destructive_cmdlets_blocked() {
    let list = allow(&["git", "cargo"]);
    for cmd in [
        "Remove-Item file.txt",
        "Set-Content file.txt -Value hello",
        "Stop-Process -Name notepad",
        "New-Item -Path C:\\test",
        "Start-Process cmd.exe",
        "Invoke-Expression ls",
        "Invoke-Command { rm -rf / }",
        "Clear-Content file.txt",
        "Restart-Service nginx",
    ] {
        let result = check_all_segments(cmd, &list);
        assert!(
            result.is_err(),
            "PowerShell destructive cmdlet should be blocked: {cmd}"
        );
        let err = result.unwrap_err();
        assert!(
            err.contains("destructive verb"),
            "error must mention destructive verb: {err}"
        );
    }
}

/// PowerShell aliases (gci, gc, sls, etc.) pass as builtins.
#[test]
fn powershell_aliases_pass_enforcement() {
    let list = allow(&["git"]);
    for cmd in [
        "gci",
        "gc file.txt",
        "sls pattern file.txt",
        "dir",
        "cls",
        "sleep",
        "sl C:\\Users",
        "pwd",
        "echo hello",
        "sort",
        "measure",
    ] {
        assert!(
            check_all_segments(cmd, &list).is_ok(),
            "PowerShell alias should pass: {cmd}"
        );
    }
}

/// PS-only dangerous aliases (iex, del, ni, etc.) must be blocked on ALL platforms.
/// These don't collide with POSIX binaries, so pwsh keeps them on Linux/macOS.
#[test]
fn powershell_ps_only_aliases_blocked_everywhere() {
    let list = allow(&["git", "cargo"]);
    // iex is the critical one — Invoke-Expression
    for cmd in ["iex $code", "del file.txt", "ni newfile", "si prop"] {
        let result = check_all_segments(cmd, &list);
        assert!(
            result.is_err(),
            "PS-only dangerous alias should be blocked: {cmd}"
        );
    }
}

/// On Windows, POSIX-colliding aliases (rm, kill, mv, cp) are also blocked.
#[test]
#[cfg(windows)]
fn powershell_posix_colliding_aliases_blocked_on_windows() {
    let list = allow(&["git", "cargo"]);
    for cmd in [
        "rm file.txt",
        "rmdir folder",
        "kill 1234",
        "mv a b",
        "cp a b",
    ] {
        let result = check_all_segments(cmd, &list);
        assert!(
            result.is_err(),
            "POSIX-colliding alias should be blocked on Windows: {cmd}"
        );
    }
}

/// On Unix, rm/kill/mv/cp are real POSIX binaries and pass the standard allowlist.
#[test]
#[cfg(not(windows))]
fn posix_binaries_not_blocked_on_unix() {
    let list = allow(&["git", "rm", "kill", "mv", "cp"]);
    for cmd in ["rm file.txt", "kill 1234", "mv a b", "cp a b"] {
        assert!(
            check_all_segments(cmd, &list).is_ok(),
            "POSIX binary should pass on Unix: {cmd}"
        );
    }
}

/// PowerShell cmdlets in pipelines: all segments must pass.
#[test]
fn powershell_pipeline_all_safe_passes() {
    let list = allow(&["git"]);
    assert!(
        check_all_segments(
            "Get-ChildItem | Where-Object Length -gt 100 | Sort-Object Name | Format-Table",
            &list
        )
        .is_ok()
    );
}

/// Mixed pipeline: safe cmdlet piped to destructive = blocked.
#[test]
fn powershell_pipeline_with_destructive_blocked() {
    let list = allow(&["git"]);
    let result = check_all_segments("Get-ChildItem | Remove-Item", &list);
    assert!(result.is_err());
}

/// GH #1514: PowerShell assignment and member-expression wrappers are syntax,
/// not command names. The wrapped cmdlet still owns the security decision.
#[test]
fn powershell_assignment_and_member_wrappers_validate_inner_cmdlets() {
    let _host = super::super::ps_statements::HostGuard::powershell(true);
    let list = allow(&["git"]);

    assert!(
        check_all_segments(
            "$d = Get-Content 'C:\\Users\\Max\\notes.json' -Raw | ConvertFrom-Json -AsHashtable",
            &list,
        )
        .is_ok()
    );
    assert!(check_all_segments("$d['items'] | ConvertTo-Json -Depth 6", &list).is_ok());
    assert!(check_all_segments("$d = (Get-Item 'C:\\Temp').Name", &list).is_ok());
    assert!(
        check_all_segments(
            "(Get-Item 'C:\\Users\\Max\\.cargo\\bin\\lean-ctx.exe').VersionInfo | Format-List *",
            &list,
        )
        .is_ok()
    );

    assert!(check_all_segments("$d = Remove-Item file.txt", &list).is_err());
    assert!(check_all_segments("(Invoke-Expression 'Get-Process').Length", &list).is_err());
    assert!(
        check_all_segments("(Get-Item 'C:\\Temp\\victim.txt').Delete()", &list).is_err(),
        "method calls must not be mistaken for safe property access"
    );
    assert!(
        check_all_segments("$d = (Get-Item 'C:\\Temp\\victim.txt').Delete()", &list).is_err(),
        "assignment wrappers must not hide method calls"
    );
    assert!(
        check_all_segments("$env:PATH = 'C:\\attacker'", &list).is_err(),
        "environment mutation must not be normalized into a safe local assignment"
    );
    assert!(
        check_all_segments("$env:PATH='C:\\attacker'", &list).is_err(),
        "compact scoped assignments must not be consumed as POSIX environment prefixes"
    );
}

/// Case insensitivity: PowerShell cmdlets are case-insensitive.
#[test]
fn powershell_case_insensitive_enforcement() {
    let list = allow(&["git"]);
    assert!(check_all_segments("get-date", &list).is_ok());
    assert!(check_all_segments("GET-CHILDITEM", &list).is_ok());
    assert!(check_all_segments("test-path C:\\Temp", &list).is_ok());
    assert!(check_all_segments("set-location C:\\Temp", &list).is_ok());
    assert!(check_all_segments("new-guid", &list).is_ok());
    let result = check_all_segments("remove-item file.txt", &list);
    assert!(result.is_err());
}

/// Start-Sleep is safe (same as `sleep` builtin), but Start-Process is blocked.
#[test]
fn powershell_start_sleep_safe_start_process_blocked() {
    let list = allow(&["git"]);
    assert!(check_all_segments("Start-Sleep -Seconds 5", &list).is_ok());
    assert!(check_all_segments("Start-Sleep 10", &list).is_ok());
    assert!(check_all_segments("Start-Process cmd.exe", &list).is_err());
    assert!(check_all_segments("Start-Job { heavy }", &list).is_err());
}

/// ogv/tee are no longer PS builtins (canonical forms are blocked/unresolved).
#[test]
fn powershell_ogv_tee_removed_from_builtins() {
    let list = allow(&["git"]);
    // ogv → Out-GridView is blocked (doesn't exist on Linux, hangs headless on Windows)
    let ogv_result = check_all_segments("ogv", &list);
    assert!(
        ogv_result.is_err(),
        "ogv should not pass as builtin anymore"
    );
    // tee → On Unix it's handled by standard allowlist, not PS builtins
    // If tee is in the standard allowlist it passes, otherwise it's blocked
    let tee_list = allow(&["git", "tee"]);
    assert!(check_all_segments("tee output.log", &tee_list).is_ok());
}

/// When a PS-only alias (like `iex`) is explicitly in the allowlist (e.g. Elixir's
/// `iex` REPL), the allowlist override takes precedence over PS alias blocking.
#[test]
fn powershell_explicit_allowlist_overrides_ps_alias_block() {
    // With iex in allowlist (Elixir REPL use-case) → should pass
    let list_with_iex = allow(&["git", "iex"]);
    assert!(
        check_all_segments("iex", &list_with_iex).is_ok(),
        "iex should pass when explicitly allowlisted (Elixir REPL)"
    );
    // Without iex in allowlist → should be blocked (PS Invoke-Expression)
    let list_without = allow(&["git", "cargo"]);
    assert!(
        check_all_segments("iex", &list_without).is_err(),
        "iex should be blocked when not in allowlist"
    );
}
