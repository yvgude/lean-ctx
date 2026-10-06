use std::path::Path;
use std::process::Command;

pub(crate) fn seatbelt_profile(allowed_read_paths: &[&Path], interpreter_path: &str) -> String {
    let mut profile = String::from("(version 1)\n(deny default)\n");
    profile.push_str("(allow process-exec)\n");
    profile.push_str("(allow process-fork)\n");
    profile.push_str("(allow sysctl-read)\n");
    profile.push_str("(allow mach-lookup)\n");
    // macOS aborts even /usr/bin/true without access to the root directory
    // itself. `literal` permits that directory, never its descendant files.
    profile.push_str("(allow file-read* (literal \"/\"))\n");

    profile.push_str("(allow file-read* (subpath \"/usr/lib\"))\n");
    profile.push_str("(allow file-read* (subpath \"/usr/share\"))\n");
    profile.push_str("(allow file-read* (subpath \"/System\"))\n");
    profile.push_str("(allow file-read* (subpath \"/Library/Frameworks\"))\n");
    profile.push_str("(allow file-read* (subpath \"/Applications/Xcode.app\"))\n");

    profile.push_str(&format!(
        "(allow file-read* (literal {}))\n",
        profile_quote(interpreter_path)
    ));

    for path in allowed_read_paths {
        let p = profile_quote(&path.to_string_lossy());
        profile.push_str(&format!("(allow file-read* (subpath {p}))\n"));
    }

    profile.push_str("(allow file-read* file-write* (subpath \"/tmp\"))\n");
    profile.push_str("(allow file-read* file-write* (subpath \"/private/tmp\"))\n");
    let sandbox_tmp = std::env::temp_dir().join("lean-ctx-sandbox");
    profile.push_str(&format!(
        "(allow file-read* file-write* (subpath {}))\n",
        profile_quote(&sandbox_tmp.to_string_lossy())
    ));

    profile.push_str("(allow file-read* (literal \"/dev/null\"))\n");
    profile.push_str("(allow file-read* (literal \"/dev/urandom\"))\n");
    profile.push_str("(allow file-write* (literal \"/dev/null\"))\n");

    profile
}

fn profile_quote(value: &str) -> String {
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}

fn sandbox_command(profile: &str) -> Command {
    // Pass immutable profile bytes at exec. A shared child-writable profile
    // file allowed another process to replace the rules before sandbox startup.
    let mut command = Command::new("/usr/bin/sandbox-exec");
    command.args(["-p", profile]);
    command
}

/// `ctx_execute`'s own narrower read/write/network boundary.
///
/// In a protected session this process runs *inside* the whole-MCP Seatbelt
/// profile, where macOS refuses a nested `sandbox_apply` outright — composing
/// the store denials here would only produce a profile the kernel never
/// applies. The request therefore goes to the launcher's supervisor, which
/// composes this profile with the pinned launch denials (appended last, so a
/// caller-selected read root cannot re-open a control file or the store) and
/// starts the child from outside the sandbox. The caller cannot ask for a
/// weaker boundary: the supervisor constructs the profile from typed paths.
pub(crate) fn execute_sandboxed(
    interpreter: &str,
    args: &[&str],
    allowed_read_paths: &[&Path],
    env: &[(String, String)],
    timeout_secs: u64,
    cwd: Option<&Path>,
) -> Result<(String, String, i32), String> {
    let profile = seatbelt_profile(allowed_read_paths, interpreter);
    #[cfg(unix)]
    {
        use crate::core::protected_execution::{Mode, launch_mode};
        match launch_mode()? {
            Mode::Brokered(client) => {
                return client.program(
                    allowed_read_paths,
                    interpreter,
                    args,
                    env,
                    timeout_secs,
                    cwd,
                );
            }
            // Only LaunchProfile::compose may construct a supervisor profile.
            Mode::Sandboxed(_) => {
                return Err("program execution must use the supervised program channel".into());
            }
            Mode::Direct => {}
        }
    }
    execute_with_profile(&profile, interpreter, args, env, timeout_secs, cwd, None)
}

/// Run one child under exactly the profile given — no composition, no lookup.
/// The supervisor passes the profile it composed itself; the community path
/// passes the local one.
#[allow(clippy::too_many_arguments)]
pub(crate) fn execute_with_profile(
    profile: &str,
    interpreter: &str,
    args: &[&str],
    env: &[(String, String)],
    timeout_secs: u64,
    cwd: Option<&Path>,
    cancel: Option<&std::sync::atomic::AtomicBool>,
) -> Result<(String, String, i32), String> {
    let mut cmd = sandbox_command(profile);
    // #1666: the caller's working directory, when it named one. The profile
    // above already grants read access to it.
    if let Some(dir) = cwd {
        if !dir.is_dir() {
            return Err(format!(
                "working directory does not exist: {}",
                dir.display()
            ));
        }
        cmd.current_dir(dir);
    }
    cmd.arg(interpreter);
    cmd.args(args);

    cmd.env_clear();
    cmd.env("PATH", "/usr/bin:/bin:/usr/local/bin");
    cmd.env("HOME", std::env::var("HOME").unwrap_or_default());
    cmd.env("LEAN_CTX_SANDBOX", "1");
    for (k, v) in env {
        cmd.env(k, v);
    }

    cmd.stdin(std::process::Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    // Reuse bounded private capture: waiting before draining pipes deadlocks
    // once a program fills an OS pipe, and inherited pipes can outlive it.
    let cap = crate::core::limits::max_shell_bytes().min(1024 * 1024);
    let capture = crate::core::process_capture::run_with_output_limits_cancellable(
        &mut cmd,
        std::time::Duration::from_secs(timeout_secs.min(4 * 60 * 60)),
        cap,
        cap,
        || cancel.is_some_and(|flag| flag.load(std::sync::atomic::Ordering::Acquire)),
    )?;
    let mut output = capture.output;
    if capture.timed_out {
        output.stderr.extend_from_slice(
            format!("\n[lean-ctx] Execution timed out after {timeout_secs}s").as_bytes(),
        );
    } else if capture.cancelled {
        output
            .stderr
            .extend_from_slice(b"\n[lean-ctx] Execution cancelled");
    }

    Ok((
        String::from_utf8_lossy(&output.stdout).to_string(),
        String::from_utf8_lossy(&output.stderr).to_string(),
        crate::shell::exit_status::exit_code(output.status),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn sandbox_profile_is_passed_inline_without_a_replaceable_file() {
        let profile = seatbelt_profile(&[], "/bin/echo");
        let command = sandbox_command(&profile);
        assert_eq!(command.get_program(), "/usr/bin/sandbox-exec");
        assert_eq!(
            command.get_args().collect::<Vec<_>>(),
            vec![std::ffi::OsStr::new("-p"), std::ffi::OsStr::new(&profile)]
        );
    }

    #[test]
    fn profile_contains_deny_default() {
        let profile = seatbelt_profile(&[], "/usr/bin/python3");
        assert!(profile.contains("(deny default)"));
        assert!(profile.contains("(version 1)"));
    }

    #[test]
    fn profile_includes_interpreter() {
        let profile = seatbelt_profile(&[], "/usr/bin/python3");
        assert!(profile.contains("(allow file-read* (literal \"/usr/bin/python3\"))"));
    }

    #[test]
    fn profile_includes_allowed_paths() {
        let p = PathBuf::from("/home/user/project");
        let profile = seatbelt_profile(&[p.as_path()], "/usr/bin/python3");
        assert!(profile.contains("(allow file-read* (subpath \"/home/user/project\"))"));
    }

    #[test]
    fn profile_allows_tmp() {
        let profile = seatbelt_profile(&[], "/usr/bin/python3");
        assert!(profile.contains("(subpath \"/tmp\")"));
        assert!(profile.contains("(subpath \"/private/tmp\")"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "sandbox-exec behavior varies by macOS version; run manually"]
    fn seatbelt_exec_echo() {
        let result = execute_sandboxed("/bin/echo", &["hello"], &[], &[], 5, None);
        assert!(result.is_ok());
        let (stdout, _, code) = result.unwrap();
        assert_eq!(code, 0);
        assert!(stdout.contains("hello"));
    }

    /// A community session builds and applies its own profile locally: there is
    /// no supervisor and nothing is appended to it.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_community_session_applies_exactly_the_local_profile() {
        let _lock = crate::core::data_dir::test_env_lock();
        crate::cli::unpin_synthetic_session();
        let profile = seatbelt_profile(&[], "/usr/bin/true");
        assert!(matches!(
            crate::core::protected_execution::launch_mode().unwrap(),
            crate::core::protected_execution::Mode::Direct
        ));
        assert!(
            Command::new("/usr/bin/sandbox-exec")
                .args(["-p", &profile])
                .arg("/usr/bin/true")
                .status()
                .unwrap()
                .success(),
            "the local profile must compile and run: {profile}"
        );
    }

    /// The store boundary is no longer composed here. macOS refuses a nested
    /// `sandbox_apply` under the whole-MCP profile, so a protected session
    /// hands the request to the launcher's supervisor — and a protected process
    /// that has no channel runs nothing at all rather than an unsupervised
    /// child. The composition itself is covered in `protected_execution`.
    #[test]
    fn a_protected_session_without_a_supervisor_runs_nothing() {
        let _lock = crate::core::data_dir::test_env_lock();
        let isolation = crate::core::data_dir::isolated_data_dir();
        let fact = isolation.path().join("knowledge/knowledge.json");
        std::fs::create_dir_all(fact.parent().unwrap()).unwrap();
        std::fs::write(&fact, "synthetic-provider-fact").unwrap();
        let _session = crate::cli::pin_synthetic_session(isolation.path()).expect("pinned rules");

        let refused = execute_sandboxed(
            "/bin/cat",
            &[fact.to_str().unwrap()],
            &[isolation.path()],
            &[],
            5,
            None,
        );
        assert!(
            refused.is_err(),
            "a protected session must not start an unsupervised child"
        );
        assert_eq!(
            std::fs::read_to_string(&fact).unwrap(),
            "synthetic-provider-fact"
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn seatbelt_denies_network() {
        let result = execute_sandboxed(
            "/usr/bin/curl",
            &["-s", "--max-time", "2", "https://example.com"],
            &[],
            &[],
            5,
            None,
        );
        if let Ok((_, stderr, code)) = result {
            assert_ne!(code, 0, "curl should fail under sandbox: {stderr}");
        }
    }
}
