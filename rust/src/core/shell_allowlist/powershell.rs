//! PowerShell cmdlet allowlist — verb-prefix + noun-level matching.
//!
//! PowerShell cmdlets follow a strict `Verb-Noun` naming convention.
//! This module classifies cmdlets into safe/blocked based on their verb prefix,
//! with noun-level exceptions for verbs that are safe in some contexts but
//! dangerous in others (e.g. `Out-String` is safe, `Out-File` is not).
//!
//! Alias resolution is split into two categories:
//! - **PS-only aliases** (no native binary collision): checked on ALL platforms
//!   because pwsh keeps them on Linux/macOS. This prevents `iex` (= Invoke-Expression)
//!   from bypassing the verb gate.
//! - **POSIX-colliding aliases** (conflict with native binary): only checked on
//!   Windows. On Unix, pwsh removes these so the native binary runs instead.
//!
//! See GH #1442 and follow-up reviews from @tr3lane.
//! Reference: <https://learn.microsoft.com/powershell/scripting/whats-new/unix-support>

/// Unconditionally safe verb prefixes — truly read-only / formatting
/// operations regardless of the noun.
const SAFE_VERB_PREFIXES: &[&str] = &[
    "Get-",
    "Test-",
    "Measure-",
    "Select-",
    "Where-",
    "ForEach-",
    "Sort-",
    "Group-",
    "Compare-",
    "Format-",
    "Write-",
    "ConvertTo-",
    "ConvertFrom-",
    "Join-",
    "Split-",
    "Resolve-",
    "Convert-",
    "Find-",
    "Search-",
    "Show-",
    "Wait-",
    "Watch-",
    "Trace-",
    "Debug-",
    "Assert-",
    "Confirm-",
];

/// Explicitly blocked verb prefixes — destructive, execution-capable, or
/// can write to disk depending on the noun.
const BLOCKED_VERB_PREFIXES: &[&str] = &[
    "Remove-",
    "Clear-",
    "Stop-",
    "Set-",
    "New-",
    "Start-",
    "Invoke-",
    "Enable-",
    "Disable-",
    "Uninstall-",
    "Restart-",
    "Suspend-",
    "Resume-",
    "Reset-",
    "Revoke-",
    "Deny-",
    "Block-",
    "Unblock-",
    "Disconnect-",
    "Dismount-",
    "Unregister-",
    "Import-",
    "Export-",
    "Out-",
    "Read-",
];

/// Noun-level safe exceptions for otherwise-blocked verb prefixes.
/// These specific cmdlets are safe despite their verb being in BLOCKED_VERB_PREFIXES.
const SAFE_CMDLET_EXCEPTIONS: &[&str] = &[
    // Set-Location = cd, the single most common PS command
    "Set-Location",
    // Pure constructors with no side effects
    "New-Guid",
    "New-TimeSpan",
    // NOTE: New-Object deliberately excluded — it's an execution primitive:
    //   (New-Object Net.WebClient).DownloadString(...)
    //   New-Object -ComObject WScript.Shell  →  .Run(...)
    // Start-Sleep = wait, no mutation (same as the `sleep` alias)
    "Start-Sleep",
    // Clear-Host = cls, display-only (clears terminal, not data)
    "Clear-Host",
    // Out-String / Out-Null / Out-Host = formatting to stdout, no file I/O
    "Out-String",
    "Out-Null",
    "Out-Host",
    "Out-Default",
    // Import-Csv / Import-Clixml = reads data from file (doesn't execute code)
    "Import-Csv",
    "Import-Clixml",
    // Export-Csv / Export-Clixml — structured data output
    "Export-Csv",
    "Export-Clixml",
    "Export-FormatData",
    // Read-Host — safe (prompts for input), though it blocks headless agents
    // on stdin timeout. Not a security issue, an availability one.
    "Read-Host",
];

/// PowerShell-only aliases that do NOT collide with any native Unix binary.
/// These must be intercepted on ALL platforms (pwsh keeps them on Linux/macOS).
/// Source: <https://learn.microsoft.com/powershell/scripting/whats-new/unix-support>
const PS_ONLY_DANGEROUS_ALIASES: &[(&str, &str)] = &[
    ("iex", "Invoke-Expression"),
    ("ihy", "Invoke-History"),
    ("icm", "Invoke-Command"),
    ("irm", "Invoke-RestMethod"),
    ("iwr", "Invoke-WebRequest"),
    ("del", "Remove-Item"),
    ("ri", "Remove-Item"),
    ("ni", "New-Item"),
    ("ndr", "New-PSDrive"),
    ("mi", "Move-Item"),
    ("cpi", "Copy-Item"),
    ("si", "Set-Item"),
    ("sp", "Set-ItemProperty"),
    ("sc", "Set-Content"),
    ("spps", "Stop-Process"),
    ("saps", "Start-Process"),
    ("sajb", "Start-Job"),
];

/// Aliases that collide with POSIX binaries — PowerShell removes them on
/// Linux/macOS so the native binary runs instead. Only gate on Windows.
/// Source: <https://learn.microsoft.com/powershell/scripting/whats-new/unix-support>
const POSIX_COLLIDING_DANGEROUS_ALIASES: &[(&str, &str)] = &[
    ("rm", "Remove-Item"),
    ("rmdir", "Remove-Item"),
    ("kill", "Stop-Process"),
    ("mv", "Move-Item"),
    ("cp", "Copy-Item"),
    ("start", "Start-Process"),
];

/// Safe PowerShell aliases and builtins that agents use frequently.
/// These bypass the allowlist like POSIX SHELL_BUILTINS.
const POWERSHELL_BUILTINS: &[&str] = &[
    // Aliases for Get-* (read-only)
    "gci", "gi", "gp", "gpv", "gc", "gl", "gm", "gps", "gsv", "gwmi",
    // Aliases for formatting/output
    "fl", "ft", "fw", "oh",      // Aliases for filtering/selection
    "sls",     // Measure-Object
    "measure", // ForEach-Object / Where-Object
    "%", "?",     // Clear-Host (display only)
    "cls",   // Get-ChildItem
    "dir",   // Get-Content
    "type",  // pagination
    "more",  // Compare-Object
    "diff",  // Start-Sleep (wait, no mutation)
    "sleep", // Set-Location (= cd, navigation only)
    "sl", "cd", "chdir", // Write-Output
    "echo",  // Get-History
    "h", "history", // Get-Location
    "pwd",     // Sort-Object
    "sort",    // Where-Object
    "where",   // ForEach-Object
    "foreach",
];

// NOTE on removed builtins vs previous version:
// - `ogv` removed: Out-GridView is now blocked (writes GUI window / doesn't
//   exist on Linux/macOS). Alias must agree with canonical form.
// - `tee` removed: Tee-Object -FilePath writes arbitrary files on Windows.
//   On Unix `tee` is the native binary (handled by standard allowlist).

/// Check a command token against the PowerShell allowlist.
///
/// Returns:
/// - `Some(true)`: safe PowerShell cmdlet / alias / builtin
/// - `Some(false)`: destructive cmdlet (blocked)
/// - `None`: not recognized as PowerShell syntax; fall through to standard allowlist
pub fn check_powershell_cmdlet(base: &str) -> Option<bool> {
    // Safe builtins/aliases (fast path)
    if POWERSHELL_BUILTINS
        .iter()
        .any(|&b| b.eq_ignore_ascii_case(base))
    {
        return Some(true);
    }

    // PS-only dangerous aliases — checked on ALL platforms (pwsh keeps them on Unix)
    if let Some(&(_, target)) = PS_ONLY_DANGEROUS_ALIASES
        .iter()
        .find(|&&(alias, _)| alias.eq_ignore_ascii_case(base))
    {
        if is_safe_exception(target) {
            return Some(true);
        }
        return Some(false);
    }

    // POSIX-colliding dangerous aliases — only on Windows where pwsh defines them
    if cfg!(windows) {
        if let Some(&(_, target)) = POSIX_COLLIDING_DANGEROUS_ALIASES
            .iter()
            .find(|&&(alias, _)| alias.eq_ignore_ascii_case(base))
        {
            if is_safe_exception(target) {
                return Some(true);
            }
            return Some(false);
        }
    }

    // Not a Verb-Noun cmdlet? Let the standard allowlist handle it.
    if !looks_like_cmdlet(base) {
        return None;
    }

    // Exact safe exception takes priority over verb-prefix block
    if is_safe_exception(base) {
        return Some(true);
    }

    if is_safe_verb(base) {
        return Some(true);
    }

    if is_blocked_verb(base) {
        return Some(false);
    }

    // Unknown verb with Verb-Noun shape — fall through to standard allowlist
    None
}

/// #1930: is `base` a `Verb-Noun` cmdlet with a verb this module knows? Such
/// a command runs the script blocks it receives, so their bodies are
/// validated. `docker-compose` and other hyphenated binaries do not qualify.
pub fn is_known_cmdlet(base: &str) -> bool {
    looks_like_cmdlet(base)
        && (is_safe_exception(base) || is_safe_verb(base) || is_blocked_verb(base))
}

fn looks_like_cmdlet(s: &str) -> bool {
    let Some(hyphen_pos) = s.find('-') else {
        return false;
    };
    if hyphen_pos == 0 || hyphen_pos == s.len() - 1 {
        return false;
    }
    let prefix = &s[..hyphen_pos];
    let suffix = &s[hyphen_pos + 1..];
    prefix.chars().all(|c| c.is_ascii_alphabetic())
        && suffix
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic())
}

fn is_safe_verb(base: &str) -> bool {
    SAFE_VERB_PREFIXES
        .iter()
        .any(|prefix| starts_with_ignore_case(base, prefix))
}

fn is_blocked_verb(base: &str) -> bool {
    BLOCKED_VERB_PREFIXES
        .iter()
        .any(|prefix| starts_with_ignore_case(base, prefix))
}

fn is_safe_exception(base: &str) -> bool {
    SAFE_CMDLET_EXCEPTIONS
        .iter()
        .any(|&exc| exc.eq_ignore_ascii_case(base))
}

fn starts_with_ignore_case(s: &str, prefix: &str) -> bool {
    s.len() >= prefix.len() && s[..prefix.len()].eq_ignore_ascii_case(prefix)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_cmdlets_pass() {
        for cmd in [
            "Get-Date",
            "Get-ChildItem",
            "Test-Path",
            "Measure-Object",
            "Select-Object",
            "Format-Table",
            "Write-Output",
            "ConvertTo-Json",
            "ConvertFrom-Csv",
            "Compare-Object",
            "Sort-Object",
            "Where-Object",
            "ForEach-Object",
            "Join-Path",
            "Split-Path",
            "Resolve-Path",
            "Find-Module",
        ] {
            assert_eq!(
                check_powershell_cmdlet(cmd),
                Some(true),
                "{cmd} should be safe"
            );
        }
    }

    #[test]
    fn blocked_cmdlets_rejected() {
        for cmd in [
            "Remove-Item",
            "Clear-Content",
            "Stop-Process",
            "Set-Content",
            "New-Item",
            "Start-Process",
            "Invoke-Expression",
            "Invoke-Command",
            "Enable-PSRemoting",
            "Disable-PSBreakpoint",
            "Restart-Service",
            "Import-Module",
            "Out-File",
            "Export-ModuleMember",
            "New-Object",
            "Read-Eval",
        ] {
            assert_eq!(
                check_powershell_cmdlet(cmd),
                Some(false),
                "{cmd} should be blocked"
            );
        }
    }

    #[test]
    fn safe_exceptions_override_blocked_verb() {
        for cmd in [
            "Set-Location",
            "New-Guid",
            "New-TimeSpan",
            "Start-Sleep",
            "Clear-Host",
            "Out-String",
            "Out-Null",
            "Out-Host",
            "Import-Csv",
            "Import-Clixml",
            "Export-Csv",
            "Read-Host",
        ] {
            assert_eq!(
                check_powershell_cmdlet(cmd),
                Some(true),
                "{cmd} should be safe (exception)"
            );
        }
    }

    #[test]
    fn new_object_is_blocked() {
        // New-Object is an execution primitive, not a pure constructor
        assert_eq!(check_powershell_cmdlet("New-Object"), Some(false));
        assert_eq!(check_powershell_cmdlet("new-object"), Some(false));
    }

    #[test]
    fn ps_only_dangerous_aliases_blocked_on_all_platforms() {
        // These don't collide with POSIX binaries → blocked everywhere
        for alias in [
            "iex", "icm", "irm", "iwr", "del", "ri", "ni", "si", "sp", "sc",
        ] {
            assert_eq!(
                check_powershell_cmdlet(alias),
                Some(false),
                "{alias} is a PS-only dangerous alias, must be blocked on all platforms"
            );
        }
    }

    #[test]
    #[cfg(windows)]
    fn posix_colliding_aliases_blocked_on_windows() {
        for alias in ["rm", "rmdir", "kill", "mv", "cp", "start"] {
            assert_eq!(
                check_powershell_cmdlet(alias),
                Some(false),
                "{alias} is a POSIX-colliding alias, must be blocked on Windows"
            );
        }
    }

    #[test]
    #[cfg(not(windows))]
    fn posix_colliding_aliases_fall_through_on_unix() {
        // On Unix, rm/kill/mv/cp are real POSIX binaries — PS removes these aliases
        for alias in ["rm", "rmdir", "kill", "mv", "cp"] {
            assert_eq!(
                check_powershell_cmdlet(alias),
                None,
                "{alias} should fall through to standard allowlist on Unix"
            );
        }
    }

    #[test]
    fn sl_is_safe_builtin() {
        // sl is in POWERSHELL_BUILTINS (Set-Location = cd)
        assert_eq!(check_powershell_cmdlet("sl"), Some(true));
    }

    #[test]
    fn non_cmdlets_return_none() {
        for cmd in ["git", "cargo", "ls", "npm", "docker", "curl"] {
            assert_eq!(check_powershell_cmdlet(cmd), None, "{cmd} is not a cmdlet");
        }
    }

    #[test]
    fn powershell_builtins_pass() {
        for cmd in [
            "gci", "gc", "dir", "type", "sls", "%", "?", "cls", "sleep", "measure", "echo", "pwd",
            "sort", "where", "foreach",
        ] {
            assert_eq!(
                check_powershell_cmdlet(cmd),
                Some(true),
                "{cmd} is a PS builtin/alias"
            );
        }
    }

    #[test]
    fn ogv_not_a_builtin_anymore() {
        // ogv (Out-GridView) removed from builtins — canonical form is blocked
        assert_eq!(check_powershell_cmdlet("ogv"), None);
        assert_eq!(check_powershell_cmdlet("Out-GridView"), Some(false));
    }

    #[test]
    fn tee_not_a_ps_builtin_anymore() {
        // tee removed from PS builtins — on Unix it's a native binary handled
        // by the standard allowlist; on Windows Tee-Object -FilePath writes files
        assert_eq!(check_powershell_cmdlet("tee"), None);
    }

    #[test]
    fn case_insensitive_cmdlet_matching() {
        assert_eq!(check_powershell_cmdlet("get-date"), Some(true));
        assert_eq!(check_powershell_cmdlet("GET-DATE"), Some(true));
        assert_eq!(check_powershell_cmdlet("remove-item"), Some(false));
        assert_eq!(check_powershell_cmdlet("INVOKE-Expression"), Some(false));
        assert_eq!(check_powershell_cmdlet("set-location"), Some(true));
        assert_eq!(check_powershell_cmdlet("NEW-GUID"), Some(true));
    }

    #[test]
    fn case_insensitive_ps_only_alias() {
        assert_eq!(check_powershell_cmdlet("IEX"), Some(false));
        assert_eq!(check_powershell_cmdlet("Iex"), Some(false));
        assert_eq!(check_powershell_cmdlet("DEL"), Some(false));
    }

    #[test]
    fn edge_cases() {
        assert_eq!(check_powershell_cmdlet("-"), None);
        assert_eq!(check_powershell_cmdlet("Get-"), None);
        assert_eq!(check_powershell_cmdlet("-Item"), None);
        assert_eq!(check_powershell_cmdlet(""), None);
        assert_eq!(check_powershell_cmdlet("Get"), None);
    }

    #[test]
    fn unknown_verb_returns_none() {
        assert_eq!(check_powershell_cmdlet("Custom-Thing"), None);
        assert_eq!(check_powershell_cmdlet("Foo-Bar"), None);
    }

    #[test]
    fn import_module_blocked_import_csv_safe() {
        assert_eq!(check_powershell_cmdlet("Import-Module"), Some(false));
        assert_eq!(check_powershell_cmdlet("Import-Csv"), Some(true));
        assert_eq!(check_powershell_cmdlet("Import-Clixml"), Some(true));
    }

    #[test]
    fn out_file_blocked_out_string_safe() {
        assert_eq!(check_powershell_cmdlet("Out-File"), Some(false));
        assert_eq!(check_powershell_cmdlet("Out-String"), Some(true));
        assert_eq!(check_powershell_cmdlet("Out-Null"), Some(true));
        assert_eq!(check_powershell_cmdlet("Out-Host"), Some(true));
        assert_eq!(check_powershell_cmdlet("Out-GridView"), Some(false));
    }

    #[test]
    fn start_sleep_safe_start_process_blocked() {
        assert_eq!(check_powershell_cmdlet("Start-Sleep"), Some(true));
        assert_eq!(check_powershell_cmdlet("Start-Process"), Some(false));
        assert_eq!(check_powershell_cmdlet("Start-Job"), Some(false));
    }

    #[test]
    fn read_host_is_safe_exception() {
        assert_eq!(check_powershell_cmdlet("Read-Host"), Some(true));
    }

    #[test]
    fn read_verb_blocked_by_default_except_read_host() {
        // Read- is in BLOCKED_VERB_PREFIXES, only Read-Host is excepted
        assert_eq!(check_powershell_cmdlet("Read-Host"), Some(true));
        assert_eq!(check_powershell_cmdlet("Read-Eval"), Some(false));
    }
}
