use std::io::{self, IsTerminal};

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

    extern "system" {
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

    let codepage = unsafe { GetACP() };
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
        return String::from_utf8_lossy(bytes).into_owned();
    }
    let mut wide: Vec<u16> = vec![0u16; wide_len as usize];
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
    extern "system" {
        fn SetConsoleOutputCP(id: u32) -> i32;
    }
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
        if let Ok(cgroup) = std::fs::read_to_string("/proc/1/cgroup") {
            if cgroup.contains("/docker/") || cgroup.contains("/lxc/") {
                return true;
            }
        }
        if let Ok(mounts) = std::fs::read_to_string("/proc/self/mountinfo") {
            if mounts.contains("/docker/containers/") {
                return true;
            }
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
    if is_running_in_powershell() {
        if let Ok(pwsh) = which_powershell() {
            return pwsh;
        }
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
fn is_running_in_powershell() -> bool {
    if is_running_in_msys_or_gitbash() {
        return false;
    }
    std::env::var("PSModulePath").is_ok()
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

fn join_command_for(args: &[String], shell_flag: &str) -> String {
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
