use std::io::{self, IsTerminal};

/// Sets `LC_CTYPE=C.UTF-8` when no UTF-8 locale is inherited from the parent
/// process. Without this, commands treat bytes >127 as non-printable (C locale),
/// mangling Cyrillic, CJK, emoji, etc.
pub(crate) fn apply_utf8_locale(cmd: &mut std::process::Command) {
    let has_utf8 = std::env::var("LC_ALL")
        .or_else(|_| std::env::var("LC_CTYPE"))
        .or_else(|_| std::env::var("LANG"))
        .is_ok_and(|v| v.to_ascii_lowercase().contains("utf"));

    if !has_utf8 {
        cmd.env("LC_CTYPE", "C.UTF-8");
    }
}

pub fn decode_output(bytes: &[u8]) -> String {
    match String::from_utf8(bytes.to_vec()) {
        Ok(s) => s,
        Err(_) => {
            #[cfg(windows)]
            {
                decode_windows_output(bytes)
            }
            #[cfg(not(windows))]
            {
                String::from_utf8_lossy(bytes).into_owned()
            }
        }
    }
}

#[cfg(windows)]
fn decode_windows_output(bytes: &[u8]) -> String {
    use std::os::windows::ffi::OsStringExt;

    let lossy = String::from_utf8_lossy(bytes);
    let replacement_count = lossy.chars().filter(|&c| c == '\u{FFFD}').count();
    if replacement_count == 0 {
        return lossy.into_owned();
    }

    unsafe extern "system" {
        fn GetACP() -> u32;
        fn MultiByteToWideChar(
            cp: u32,
            flags: u32,
            src: *const u8,
            srclen: i32,
            dst: *mut u16,
            dstlen: i32,
        ) -> i32;
    }

    // SAFETY: `GetACP` takes no arguments and only returns the active code
    // page; it cannot fail or cause undefined behaviour.
    let codepage = unsafe { GetACP() };
    // SAFETY: called with a null destination and length 0 to measure the
    // required buffer size; `bytes` is a live slice and every pointer/length
    // argument is valid.
    let wide_len = unsafe {
        MultiByteToWideChar(
            codepage,
            0,
            bytes.as_ptr(),
            bytes.len() as i32,
            std::ptr::null_mut(),
            0,
        )
    };
    if wide_len <= 0 {
        return lossy.into_owned();
    }
    let mut wide: Vec<u16> = vec![0u16; wide_len as usize];
    // SAFETY: `wide` is sized to the previously measured length and `bytes` is
    // a live slice; the source and destination pointers/lengths are valid and
    // do not overlap.
    unsafe {
        MultiByteToWideChar(
            codepage,
            0,
            bytes.as_ptr(),
            bytes.len() as i32,
            wide.as_mut_ptr(),
            wide_len,
        );
    }
    std::ffi::OsString::from_wide(&wide)
        .to_string_lossy()
        .into_owned()
}

#[cfg(windows)]
pub(super) fn set_console_utf8() {
    unsafe extern "system" {
        fn SetConsoleOutputCP(id: u32) -> i32;
    }
    // SAFETY: `SetConsoleOutputCP` takes a code-page id (65001 = UTF-8) by
    // value; it cannot cause undefined behaviour.
    unsafe {
        SetConsoleOutputCP(65001);
    }
}

/// Detects if the current process runs inside a Docker/container environment.
pub fn is_container() -> bool {
    #[cfg(unix)]
    {
        if std::path::Path::new("/.dockerenv").exists() {
            return true;
        }
        if let Ok(cgroup) = std::fs::read_to_string("/proc/1/cgroup")
            && (cgroup.contains("/docker/") || cgroup.contains("/lxc/"))
        {
            return true;
        }
        if let Ok(mounts) = std::fs::read_to_string("/proc/self/mountinfo")
            && mounts.contains("/docker/containers/")
        {
            return true;
        }
        false
    }
    #[cfg(not(unix))]
    {
        false
    }
}

/// Returns true if stdin is NOT a terminal (pipe, /dev/null, etc.)
pub fn is_non_interactive() -> bool {
    !io::stdin().is_terminal()
}

/// Returns `true` when `shell_path` points to a PowerShell executable.
pub(crate) fn is_powershell(shell_path: &str) -> bool {
    let name = std::path::Path::new(shell_path)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    name.contains("powershell") || name.contains("pwsh")
}

/// Path to the current-user PowerShell profile (`$PROFILE.CurrentUserCurrentHost`).
///
/// Windows PowerShell stores it under `Documents\PowerShell\…`, but **PowerShell
/// (pwsh) on macOS/Linux reads `~/.config/powershell/…` instead** — and stat-ing
/// anything inside `~/Documents` on macOS pops a TCC privacy prompt ("lean-ctx
/// would like to access files in your Documents folder", #356). Resolving the
/// profile per-OS keeps pwsh support everywhere while never touching `~/Documents`
/// on non-Windows hosts.
pub(crate) fn powershell_profile_path(home: &std::path::Path) -> std::path::PathBuf {
    const PROFILE_FILE: &str = "Microsoft.PowerShell_profile.ps1";
    if cfg!(windows) {
        home.join("Documents").join("PowerShell").join(PROFILE_FILE)
    } else {
        home.join(".config").join("powershell").join(PROFILE_FILE)
    }
}

/// Windows only: argument that passes one command string to the shell binary.
/// `exe_basename` must already be ASCII-lowercase (e.g. `bash.exe`, `cmd.exe`).
fn windows_shell_flag_for_exe_basename(exe_basename: &str) -> &'static str {
    if exe_basename.contains("powershell") || exe_basename.contains("pwsh") {
        "-Command"
    } else if exe_basename == "cmd.exe" || exe_basename == "cmd" {
        "/C"
    } else {
        "-c"
    }
}

pub fn shell_and_flag() -> (String, String) {
    let shell = detect_shell();
    let flag = if cfg!(windows) {
        let name = std::path::Path::new(&shell)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        windows_shell_flag_for_exe_basename(&name).to_string()
    } else {
        "-c".to_string()
    };
    (shell, flag)
}

/// Returns a short, human-readable shell name (e.g. "bash", "zsh", "powershell", "cmd").
pub fn shell_name() -> String {
    let shell = detect_shell();
    let basename = std::path::Path::new(&shell)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("sh")
        .to_ascii_lowercase();
    basename
        .strip_suffix(".exe")
        .unwrap_or(&basename)
        .to_string()
}

pub(super) fn detect_shell() -> String {
    if let Ok(shell) = std::env::var("LEAN_CTX_SHELL") {
        return shell;
    }

    if let Ok(shell) = std::env::var("SHELL") {
        let bin = std::path::Path::new(&shell)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("sh");

        if bin == "lean-ctx" {
            return find_real_shell();
        }
        return shell;
    }

    find_real_shell()
}

#[cfg(unix)]
fn find_real_shell() -> String {
    for shell in &["/bin/zsh", "/bin/bash", "/bin/sh"] {
        if std::path::Path::new(shell).exists() {
            return shell.to_string();
        }
    }
    "/bin/sh".to_string()
}

#[cfg(windows)]
fn find_real_shell() -> String {
    if is_running_in_msys_or_gitbash() {
        for candidate in &["bash.exe", "sh.exe"] {
            if let Ok(output) = std::process::Command::new("where").arg(candidate).output() {
                if output.status.success() {
                    if let Ok(path) = String::from_utf8(output.stdout) {
                        if let Some(first_line) = path.lines().next() {
                            let trimmed = first_line.trim();
                            if !trimmed.is_empty() {
                                return trimmed.to_string();
                            }
                        }
                    }
                }
            }
        }
    }
    if let Ok(pwsh) = which_powershell() {
        return pwsh;
    }
    if let Ok(comspec) = std::env::var("COMSPEC") {
        return comspec;
    }
    "cmd.exe".to_string()
}

#[cfg(windows)]
fn is_running_in_msys_or_gitbash() -> bool {
    std::env::var("MSYSTEM").is_ok() || std::env::var("MINGW_PREFIX").is_ok()
}

#[cfg(windows)]
fn which_powershell() -> Result<String, ()> {
    for candidate in &["pwsh.exe", "powershell.exe"] {
        if let Ok(output) = std::process::Command::new("where").arg(candidate).output() {
            if output.status.success() {
                if let Ok(path) = String::from_utf8(output.stdout) {
                    if let Some(first_line) = path.lines().next() {
                        let trimmed = first_line.trim();
                        if !trimmed.is_empty() {
                            return Ok(trimmed.to_string());
                        }
                    }
                }
            }
        }
    }
    Err(())
}

/// Join multiple CLI arguments into a single command string, using quoting
/// conventions appropriate for the detected shell.
///
/// On Unix, this always produces POSIX-compatible quoting.
/// On Windows, the quoting adapts to the actual shell (PowerShell, cmd.exe,
/// or Git Bash / MSYS).
pub fn join_command(args: &[String]) -> String {
    let (_, flag) = shell_and_flag();
    join_command_for(args, &flag)
}

pub fn join_command_for(args: &[String], shell_flag: &str) -> String {
    match shell_flag {
        "-Command" => join_powershell(args),
        "/C" => join_cmd(args),
        _ => join_posix(args),
    }
}

fn join_posix(args: &[String]) -> String {
    args.iter()
        .map(|a| quote_posix(a))
        .collect::<Vec<_>>()
        .join(" ")
}

fn join_powershell(args: &[String]) -> String {
    if args.len() == 1 && args[0].contains(' ') {
        return args[0].clone();
    }
    let quoted: Vec<String> = args.iter().map(|a| quote_powershell(a)).collect();
    format!("& {}", quoted.join(" "))
}

fn join_cmd(args: &[String]) -> String {
    args.iter()
        .map(|a| quote_cmd(a))
        .collect::<Vec<_>>()
        .join(" ")
}

fn quote_posix(s: &str) -> String {
    if s.is_empty() {
        return "''".to_string();
    }
    if s.bytes()
        .all(|b| b.is_ascii_alphanumeric() || b"-_./=:@,+%^".contains(&b))
    {
        return s.to_string();
    }
    format!("'{}'", s.replace('\'', "'\\''"))
}

fn quote_powershell(s: &str) -> String {
    if s.is_empty() {
        return "''".to_string();
    }
    if s.bytes()
        .all(|b| b.is_ascii_alphanumeric() || b"-_./=:@,+%^".contains(&b))
    {
        return s.to_string();
    }
    format!("'{}'", s.replace('\'', "''"))
}

fn quote_cmd(s: &str) -> String {
    if s.is_empty() {
        return "\"\"".to_string();
    }
    if s.bytes()
        .all(|b| b.is_ascii_alphanumeric() || b"-_./=:@,+%^\\".contains(&b))
    {
        return s.to_string();
    }
    format!("\"{}\"", s.replace('"', "\\\""))
}

#[cfg(test)]
mod join_command_tests {
    use super::*;

    #[test]
    fn posix_simple_args() {
        let args: Vec<String> = vec!["git".into(), "status".into()];
        assert_eq!(join_command_for(&args, "-c"), "git status");
    }

    #[test]
    fn posix_path_with_spaces() {
        let args: Vec<String> = vec!["/usr/local/my app/bin".into(), "--help".into()];
        assert_eq!(
            join_command_for(&args, "-c"),
            "'/usr/local/my app/bin' --help"
        );
    }

    #[test]
    fn posix_single_quotes_escaped() {
        let args: Vec<String> = vec!["echo".into(), "it's".into()];
        assert_eq!(join_command_for(&args, "-c"), "echo 'it'\\''s'");
    }

    #[test]
    fn posix_empty_arg() {
        let args: Vec<String> = vec!["cmd".into(), String::new()];
        assert_eq!(join_command_for(&args, "-c"), "cmd ''");
    }

    #[test]
    fn powershell_simple_args() {
        let args: Vec<String> = vec!["npm".into(), "install".into()];
        assert_eq!(join_command_for(&args, "-Command"), "& npm install");
    }

    #[test]
    fn powershell_path_with_spaces() {
        let args: Vec<String> = vec![
            "C:\\Program Files\\nodejs\\npm.cmd".into(),
            "install".into(),
        ];
        assert_eq!(
            join_command_for(&args, "-Command"),
            "& 'C:\\Program Files\\nodejs\\npm.cmd' install"
        );
    }

    #[test]
    fn powershell_single_quotes_escaped() {
        let args: Vec<String> = vec!["echo".into(), "it's done".into()];
        assert_eq!(join_command_for(&args, "-Command"), "& echo 'it''s done'");
    }

    #[test]
    fn cmd_simple_args() {
        let args: Vec<String> = vec!["npm.cmd".into(), "install".into()];
        assert_eq!(join_command_for(&args, "/C"), "npm.cmd install");
    }

    #[test]
    fn cmd_path_with_spaces() {
        let args: Vec<String> = vec![
            "C:\\Program Files\\nodejs\\npm.cmd".into(),
            "install".into(),
        ];
        assert_eq!(
            join_command_for(&args, "/C"),
            "\"C:\\Program Files\\nodejs\\npm.cmd\" install"
        );
    }

    #[test]
    fn cmd_double_quotes_escaped() {
        let args: Vec<String> = vec!["echo".into(), "say \"hello\"".into()];
        assert_eq!(join_command_for(&args, "/C"), "echo \"say \\\"hello\\\"\"");
    }

    #[test]
    fn unknown_flag_uses_posix() {
        let args: Vec<String> = vec!["ls".into(), "-la".into()];
        assert_eq!(join_command_for(&args, "--exec"), "ls -la");
    }

    #[test]
    fn powershell_single_full_command_not_quoted() {
        let args: Vec<String> = vec!["git commit -m \"feat: add feature\"".into()];
        let result = join_command_for(&args, "-Command");
        assert_eq!(result, "git commit -m \"feat: add feature\"");
        assert!(
            !result.starts_with("& '"),
            "must not wrap full command in & '...'"
        );
    }

    #[test]
    fn powershell_single_no_spaces_still_uses_call_operator() {
        let args: Vec<String> = vec!["git".into()];
        assert_eq!(join_command_for(&args, "-Command"), "& git");
    }
}

#[cfg(test)]
mod is_powershell_tests {
    use super::is_powershell;

    #[test]
    fn detects_pwsh_exe() {
        assert!(is_powershell("pwsh.exe"));
    }

    #[test]
    fn detects_powershell_exe() {
        assert!(is_powershell("powershell.exe"));
    }

    #[test]
    fn rejects_cmd() {
        assert!(!is_powershell("cmd.exe"));
    }

    #[test]
    fn rejects_bash() {
        assert!(!is_powershell("/usr/bin/bash"));
    }

    #[test]
    fn case_insensitive() {
        assert!(is_powershell("PWSH.EXE"));
        assert!(is_powershell("PowerShell.exe"));
    }

    #[test]
    fn full_path_with_pwsh() {
        assert!(is_powershell(
            "C:\\Windows\\System32\\WindowsPowerShell\\v1.0\\powershell.exe"
        ));
        assert!(is_powershell("/usr/local/bin/pwsh"));
    }
}

#[cfg(test)]
mod powershell_profile_tests {
    use super::powershell_profile_path;
    use std::path::Path;

    #[test]
    fn always_ends_with_profile_file() {
        let p = powershell_profile_path(Path::new("/home/u"));
        assert!(p.ends_with("Microsoft.PowerShell_profile.ps1"));
    }

    #[cfg(not(windows))]
    #[test]
    fn non_windows_uses_config_powershell_never_documents() {
        // #356: stat-ing anything under ~/Documents pops a macOS TCC prompt, so the
        // non-Windows profile path must live under ~/.config/powershell instead.
        let p = powershell_profile_path(Path::new("/Users/jane"));
        assert_eq!(
            p,
            Path::new("/Users/jane/.config/powershell/Microsoft.PowerShell_profile.ps1")
        );
        assert!(
            !p.to_string_lossy().contains("Documents"),
            "macOS/Linux PowerShell profile must never touch ~/Documents (#356)"
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_uses_documents_powershell() {
        let p = powershell_profile_path(Path::new("C:\\Users\\jane"));
        assert!(p.ends_with("Documents\\PowerShell\\Microsoft.PowerShell_profile.ps1"));
    }
}

#[cfg(test)]
mod windows_shell_flag_tests {
    use super::windows_shell_flag_for_exe_basename;

    #[test]
    fn cmd_uses_slash_c() {
        assert_eq!(windows_shell_flag_for_exe_basename("cmd.exe"), "/C");
        assert_eq!(windows_shell_flag_for_exe_basename("cmd"), "/C");
    }

    #[test]
    fn powershell_uses_command() {
        assert_eq!(
            windows_shell_flag_for_exe_basename("powershell.exe"),
            "-Command"
        );
        assert_eq!(windows_shell_flag_for_exe_basename("pwsh.exe"), "-Command");
    }

    #[test]
    fn posix_shells_use_dash_c() {
        assert_eq!(windows_shell_flag_for_exe_basename("bash.exe"), "-c");
        assert_eq!(windows_shell_flag_for_exe_basename("bash"), "-c");
        assert_eq!(windows_shell_flag_for_exe_basename("sh.exe"), "-c");
        assert_eq!(windows_shell_flag_for_exe_basename("zsh.exe"), "-c");
        assert_eq!(windows_shell_flag_for_exe_basename("fish.exe"), "-c");
    }
}

#[cfg(test)]
mod platform_tests {
    #[test]
    fn is_container_returns_bool() {
        let _ = super::is_container();
    }

    #[test]
    fn is_non_interactive_returns_bool() {
        let _ = super::is_non_interactive();
    }

    #[test]
    fn join_command_preserves_structure() {
        let args = vec![
            "git".to_string(),
            "commit".to_string(),
            "-m".to_string(),
            "my message".to_string(),
        ];
        let joined = super::join_command(&args);
        assert!(joined.contains("git"));
        assert!(joined.contains("commit"));
        assert!(joined.contains("my message") || joined.contains("'my message'"));
    }

    #[test]
    fn quote_posix_handles_em_dash() {
        let result = super::quote_posix("closing — see #407");
        assert!(
            result.starts_with('\''),
            "em-dash args must be single-quoted: {result}"
        );
    }

    #[test]
    fn quote_posix_handles_nested_single_quotes() {
        let result = super::quote_posix("it's a test");
        assert!(
            result.contains("\\'"),
            "single quotes must be escaped: {result}"
        );
    }

    #[test]
    fn quote_posix_safe_chars_unquoted() {
        let result = super::quote_posix("simple_word");
        assert_eq!(result, "simple_word");
    }

    #[test]
    fn quote_posix_empty_string() {
        let result = super::quote_posix("");
        assert_eq!(result, "''");
    }

    #[test]
    fn quote_posix_dollar_expansion_protected() {
        let result = super::quote_posix("$HOME/test");
        assert!(
            result.starts_with('\''),
            "dollar signs must be single-quoted: {result}"
        );
    }

    #[test]
    fn quote_posix_backtick_protected() {
        let result = super::quote_posix("echo `date`");
        assert!(
            result.starts_with('\''),
            "backticks must be single-quoted: {result}"
        );
    }

    #[test]
    fn quote_posix_double_quotes_protected() {
        let result = super::quote_posix(r#"he said "hello""#);
        assert!(
            result.starts_with('\''),
            "double quotes must be single-quoted: {result}"
        );
    }
}
