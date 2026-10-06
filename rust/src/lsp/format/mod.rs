//! Formatter routing for `ctx_refactor action=reformat`: pick a formatter by
//! file extension, using built-in routing per extension.

/// The formatter selected for a file: either the IDE HTTP backend or an external
/// shell command (template with a `{file}` placeholder).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Formatter {
    Jetbrains,
    Command(String),
}

/// Pick the formatter for `abs_path` using built-in defaults per extension.
/// Extension match is case-insensitive; no extension or an unknown extension → `Jetbrains`.
pub fn resolve_formatter(abs_path: &str) -> Formatter {
    let ext = std::path::Path::new(abs_path)
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    builtin_default(&ext)
}

/// Built-in routing when the config has no entry for this extension.
fn builtin_default(ext: &str) -> Formatter {
    match ext {
        "rs" => Formatter::Command("rustfmt {file}".to_string()),
        _ => Formatter::Jetbrains,
    }
}

/// The binary name of a command template, for the `via <name>` output label.
pub fn command_label(template: &str) -> &str {
    template.split_whitespace().next().unwrap_or("formatter")
}

/// Split a command template into argv, substituting the `{file}` placeholder with
/// `abs_path`. `{file}` may be a standalone token or embedded in a token. If no
/// placeholder is present, `abs_path` is appended as the final argument. The path
/// is always a single argv element (spaces in the path are preserved).
pub fn build_argv(template: &str, abs_path: &str) -> Vec<String> {
    let mut argv: Vec<String> = Vec::new();
    let mut saw_placeholder = false;
    for tok in template.split_whitespace() {
        if tok == "{file}" {
            argv.push(abs_path.to_string());
            saw_placeholder = true;
        } else if tok.contains("{file}") {
            argv.push(tok.replace("{file}", abs_path));
            saw_placeholder = true;
        } else {
            argv.push(tok.to_string());
        }
    }
    if !saw_placeholder {
        argv.push(abs_path.to_string());
    }
    argv
}

/// Run an external formatter command on `abs_path` with cwd `project_root` (so
/// tool config like `rustfmt.toml` is discovered). Returns `Err` with a clear
/// message if the binary is missing or the command exits non-zero.
///
/// Formatter commands are trusted local executables. LeanCTX terminates the
/// formatter and ordinary descendants through a Unix process group or Windows
/// Job Object; a deliberately detached Unix process (`setsid`) is outside this
/// cooperative cleanup boundary.
pub fn run_command_formatter(
    template: &str,
    abs_path: &str,
    project_root: &str,
) -> Result<(), String> {
    run_command_formatter_with_timeout(
        template,
        abs_path,
        project_root,
        std::time::Duration::from_secs(30),
    )
}

fn run_command_formatter_with_timeout(
    template: &str,
    abs_path: &str,
    project_root: &str,
    timeout: std::time::Duration,
) -> Result<(), String> {
    wait_for_command_formatter(
        spawn_command_formatter(template, abs_path, project_root)?,
        timeout,
    )
}

fn spawn_command_formatter(
    template: &str,
    abs_path: &str,
    project_root: &str,
) -> Result<crate::core::process_capture::CapturedChild, String> {
    use std::process::{Command, Stdio};

    let argv = build_argv(template, abs_path);
    let (bin, rest) = argv
        .split_first()
        .ok_or_else(|| "INVALID_TARGET: empty formatter template".to_string())?;
    let mut command = Command::new(bin);
    command
        .args(rest)
        .current_dir(project_root)
        .stdin(Stdio::null());
    crate::core::process_capture::spawn_capture(&mut command)
}

fn wait_for_command_formatter(
    child: crate::core::process_capture::CapturedChild,
    timeout: std::time::Duration,
) -> Result<(), String> {
    let bin = child.program().to_owned();
    let captured = crate::core::process_capture::wait_for_capture(child, timeout)?;
    if captured.timed_out {
        return Err(format!(
            "formatter '{bin}' timed out after {}s",
            timeout.as_secs()
        ));
    }
    if !captured.output.status.success() {
        let code = captured
            .output
            .status
            .code()
            .map_or_else(|| "signal".to_string(), |code| code.to_string());
        let stderr = String::from_utf8_lossy(&captured.output.stderr);
        return Err(format!("{bin} exited {code}: {}", stderr.trim()));
    }
    Ok(())
}

/// Hex BLAKE3 of the file content, for honest before/after change detection.
pub fn blake3_of(abs_path: &str) -> Result<String, String> {
    let bytes = std::fs::read(abs_path).map_err(|e| format!("FILE_NOT_FOUND: {abs_path}: {e}"))?;
    Ok(crate::core::hasher::hash_hex(&bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wait_for_path(path: &std::path::Path, timeout: std::time::Duration) -> bool {
        let deadline = std::time::Instant::now() + timeout;
        while !path.exists() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        path.exists()
    }

    fn assert_descendant_stopped(release: &std::path::Path, survived: &std::path::Path) {
        std::fs::write(release, "release").unwrap();
        assert!(
            !wait_for_path(survived, std::time::Duration::from_secs(2)),
            "formatter descendant performed work after cleanup"
        );
    }

    /// Spawn a formatter script the test has just written. When another test
    /// thread forks while that write handle is still open, the child inherits
    /// it and `exec` fails with ETXTBSY until the child execs or exits. Retry
    /// only that error; any other spawn failure is returned at once.
    #[cfg(unix)]
    fn spawn_fresh_script(
        template: &str,
        abs_path: &str,
        project_root: &str,
    ) -> Result<crate::core::process_capture::CapturedChild, String> {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            match spawn_command_formatter(template, abs_path, project_root) {
                Err(error)
                    if error.contains("Text file busy") && std::time::Instant::now() < deadline =>
                {
                    std::thread::sleep(std::time::Duration::from_millis(20));
                }
                result => return result,
            }
        }
    }

    #[test]
    fn rs_defaults_to_rustfmt() {
        let f = resolve_formatter("/x/a.rs");
        assert!(matches!(f, Formatter::Command(ref t) if t == "rustfmt {file}"));
    }

    #[test]
    fn md_and_unknown_and_no_ext_default_to_jetbrains() {
        assert!(matches!(resolve_formatter("/x/a.md"), Formatter::Jetbrains));
        assert!(matches!(
            resolve_formatter("/x/a.txt"),
            Formatter::Jetbrains
        ));
        assert!(matches!(
            resolve_formatter("/x/README"),
            Formatter::Jetbrains
        ));
    }

    #[test]
    fn extension_is_case_insensitive() {
        assert!(matches!(
            resolve_formatter("/x/A.RS"),
            Formatter::Command(_)
        ));
    }

    #[test]
    fn command_label_is_first_token() {
        assert_eq!(command_label("rustfmt {file}"), "rustfmt");
        assert_eq!(command_label("ruff format {file}"), "ruff");
        assert_eq!(command_label(""), "formatter");
    }

    #[test]
    fn argv_substitutes_placeholder() {
        assert_eq!(
            build_argv("rustfmt {file}", "/x/a.rs"),
            vec!["rustfmt".to_string(), "/x/a.rs".to_string()]
        );
        assert_eq!(
            build_argv("ruff format {file}", "/x/a.py"),
            vec![
                "ruff".to_string(),
                "format".to_string(),
                "/x/a.py".to_string()
            ]
        );
    }

    #[test]
    fn argv_appends_path_when_no_placeholder() {
        assert_eq!(
            build_argv("gofmt -w", "/x/a.go"),
            vec!["gofmt".to_string(), "-w".to_string(), "/x/a.go".to_string()]
        );
    }

    #[test]
    fn argv_path_with_spaces_stays_one_arg() {
        let argv = build_argv("rustfmt {file}", "/x/my dir/a.rs");
        assert_eq!(
            argv,
            vec!["rustfmt".to_string(), "/x/my dir/a.rs".to_string()]
        );
    }

    #[test]
    fn blake3_detects_change() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("a.txt");
        std::fs::write(&f, "one").unwrap();
        let p = f.to_str().unwrap();
        let h1 = blake3_of(p).unwrap();
        let h2 = blake3_of(p).unwrap();
        assert_eq!(h1, h2, "same content → same hash");
        std::fs::write(&f, "two").unwrap();
        assert_ne!(
            h1,
            blake3_of(p).unwrap(),
            "changed content → different hash"
        );
    }

    #[test]
    fn blake3_missing_file_errors() {
        assert!(blake3_of("/no/such/file.xyz").is_err());
    }

    #[test]
    fn run_command_missing_binary_errors() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("a.rs");
        std::fs::write(&f, "fn x(){}\n").unwrap();
        let err = run_command_formatter(
            "definitely-not-a-formatter-binary {file}",
            f.to_str().unwrap(),
            dir.path().to_str().unwrap(),
        )
        .unwrap_err();
        assert!(err.contains("not found"), "got: {err}");
    }

    #[test]
    fn run_command_nonzero_exit_errors() {
        let dir = tempfile::tempdir().unwrap();
        #[cfg(unix)]
        let (template, formatter) = {
            let formatter = dir.path().join("fail-formatter");
            std::fs::write(&formatter, "#!/bin/sh\nexit 7\n").unwrap();
            ("sh {file}", formatter)
        };
        #[cfg(windows)]
        let (template, formatter) = {
            let formatter = dir.path().join("fail-formatter.cmd");
            std::fs::write(&formatter, "@echo off\r\nexit /B 7\r\n").unwrap();
            ("cmd.exe /D /S /C {file}", formatter)
        };
        let err = run_command_formatter(
            template,
            formatter.to_str().unwrap(),
            dir.path().to_str().unwrap(),
        )
        .unwrap_err();
        assert!(err.contains("exited"), "got: {err}");
    }

    #[cfg(windows)]
    #[test]
    fn command_formatter_timeout_closes_descendant_pipes() {
        use std::time::Duration;

        let dir = tempfile::tempdir().unwrap();
        let formatter = dir.path().join("stall-formatter.ps1");
        let descendant = dir.path().join("descendant.ps1");
        let ready = dir.path().join("descendant.ready");
        let release = dir.path().join("descendant.release");
        let survived = dir.path().join("descendant.survived");
        let ps_quote = |path: &std::path::Path| path.display().to_string().replace('\'', "''");
        std::fs::write(
            &descendant,
            format!(
                "$ErrorActionPreference = 'Stop'\r\nSet-Content -LiteralPath '{}' -Value ready\r\nwhile (-not (Test-Path -LiteralPath '{}')) {{ Start-Sleep -Milliseconds 10 }}\r\nSet-Content -LiteralPath '{}' -Value survived\r\n",
                ps_quote(&ready),
                ps_quote(&release),
                ps_quote(&survived)
            ),
        )
        .unwrap();
        std::fs::write(
            &formatter,
            format!(
                "$ErrorActionPreference = 'Stop'\r\nStart-Process powershell.exe -NoNewWindow -ArgumentList @('-NoProfile','-ExecutionPolicy','Bypass','-File','{}')\r\nwhile ($true) {{ Start-Sleep -Seconds 1 }}\r\n",
                ps_quote(&descendant)
            ),
        )
        .unwrap();
        let process = spawn_command_formatter(
            "powershell.exe -NoProfile -ExecutionPolicy Bypass -File {file}",
            formatter.to_str().unwrap(),
            dir.path().to_str().unwrap(),
        )
        .expect("formatter process must spawn");
        // #1536: PowerShell cold-start on a loaded Windows runner can exceed
        // 5 s. The poll is bounded either way, so the generous ceiling only
        // costs wall-clock time in the case where the test is about to fail.
        assert!(
            wait_for_path(&ready, Duration::from_secs(30)),
            "formatter descendant must start"
        );
        let error = wait_for_command_formatter(process, Duration::from_millis(250))
            .expect_err("stalled formatter tree must fail closed");

        assert!(error.contains("timed out"), "got: {error}");
        assert_descendant_stopped(&release, &survived);
    }

    #[cfg(unix)]
    #[test]
    fn command_formatter_times_out_and_reaps_a_stalled_child() {
        use std::os::unix::fs::PermissionsExt;
        use std::time::Duration;

        let dir = tempfile::tempdir().unwrap();
        let formatter = dir.path().join("stall-formatter");
        std::fs::write(
            &formatter,
            "#!/bin/sh\n(printf ready > \"${0}.ready\"; while [ ! -e \"${0}.release\" ]; do sleep 0.01; done; printf survived > \"${0}.survived\") &\nwait\n",
        )
        .unwrap();
        let mut permissions = std::fs::metadata(&formatter).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&formatter, permissions).unwrap();
        let source = dir.path().join("a.rs");
        std::fs::write(&source, "fn x() {}\n").unwrap();
        let ready = formatter.with_file_name("stall-formatter.ready");
        let release = formatter.with_file_name("stall-formatter.release");
        let survived = formatter.with_file_name("stall-formatter.survived");
        let template = format!("{} {{file}}", formatter.display());
        let source_path = source.to_str().unwrap().to_owned();
        let project_root = dir.path().to_str().unwrap().to_owned();
        let process = spawn_fresh_script(&template, &source_path, &project_root)
            .expect("formatter process must spawn");

        assert!(
            wait_for_path(&ready, Duration::from_secs(10)),
            "formatter descendant must start"
        );
        let error = wait_for_command_formatter(process, Duration::from_millis(250))
            .expect_err("stalled formatter must fail closed");

        assert!(error.contains("timed out"), "unexpected error: {error}");
        assert_descendant_stopped(&release, &survived);
    }

    #[cfg(unix)]
    #[test]
    fn command_formatter_cleans_descendant_after_parent_exits() {
        use std::os::unix::fs::PermissionsExt;
        use std::time::Duration;

        let dir = tempfile::tempdir().unwrap();
        let formatter = dir.path().join("background-formatter");
        std::fs::write(
            &formatter,
            "#!/bin/sh\n(printf ready > \"${0}.ready\"; while [ ! -e \"${0}.release\" ]; do sleep 0.01; done; printf survived > \"${0}.survived\") &\nexit 0\n",
        )
        .unwrap();
        let mut permissions = std::fs::metadata(&formatter).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&formatter, permissions).unwrap();
        let source = dir.path().join("a.rs");
        std::fs::write(&source, "fn x() {}\n").unwrap();

        let process = spawn_fresh_script(
            &format!("{} {{file}}", formatter.display()),
            source.to_str().unwrap(),
            dir.path().to_str().unwrap(),
        )
        .expect("formatter process must spawn");
        let ready = formatter.with_file_name("background-formatter.ready");
        let release = formatter.with_file_name("background-formatter.release");
        let survived = formatter.with_file_name("background-formatter.survived");
        assert!(
            wait_for_path(&ready, Duration::from_secs(10)),
            "formatter descendant must start"
        );

        wait_for_command_formatter(process, Duration::from_secs(1))
            .expect("successful formatter must clean up its background process");
        assert_descendant_stopped(&release, &survived);
    }

    #[test]
    fn run_rustfmt_formats_and_reports_change() {
        // Gated: only runs when rustfmt is installed.
        if std::process::Command::new("rustfmt")
            .arg("--version")
            .output()
            .is_err()
        {
            eprintln!("SKIP: rustfmt not in PATH");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("a.rs");
        std::fs::write(&f, "fn   x( ){let y=1;}\n").unwrap(); // deliberate drift
        let p = f.to_str().unwrap();
        let before = blake3_of(p).unwrap();
        run_command_formatter("rustfmt {file}", p, dir.path().to_str().unwrap()).unwrap();
        let after = blake3_of(p).unwrap();
        assert_ne!(
            before, after,
            "rustfmt should have changed the drifted file"
        );

        // A second run is a no-op (already conformant).
        let before2 = blake3_of(p).unwrap();
        run_command_formatter("rustfmt {file}", p, dir.path().to_str().unwrap()).unwrap();
        assert_eq!(
            before2,
            blake3_of(p).unwrap(),
            "second run should be unchanged"
        );
    }
}
