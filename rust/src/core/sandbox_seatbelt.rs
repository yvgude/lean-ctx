use std::path::Path;
use std::process::Command;

#[must_use]
pub fn seatbelt_profile(allowed_read_paths: &[&Path], interpreter_path: &str) -> String {
    let mut profile = String::from("(version 1)\n(deny default)\n");
    profile.push_str("(allow process-exec)\n");
    profile.push_str("(allow process-fork)\n");
    profile.push_str("(allow sysctl-read)\n");
    profile.push_str("(allow mach-lookup)\n");

    profile.push_str("(allow file-read* (subpath \"/usr/lib\"))\n");
    profile.push_str("(allow file-read* (subpath \"/usr/share\"))\n");
    profile.push_str("(allow file-read* (subpath \"/System\"))\n");
    profile.push_str("(allow file-read* (subpath \"/Library/Frameworks\"))\n");
    profile.push_str("(allow file-read* (subpath \"/Applications/Xcode.app\"))\n");

    profile.push_str(&format!(
        "(allow file-read* (literal \"{interpreter_path}\"))\n"
    ));

    for path in allowed_read_paths {
        let p = path.display();
        profile.push_str(&format!("(allow file-read* (subpath \"{p}\"))\n"));
    }

    profile.push_str("(allow file-read* file-write* (subpath \"/tmp\"))\n");
    profile.push_str("(allow file-read* file-write* (subpath \"/private/tmp\"))\n");
    let sandbox_tmp = std::env::temp_dir().join("lean-ctx-sandbox");
    profile.push_str(&format!(
        "(allow file-read* file-write* (subpath \"{}\"))\n",
        sandbox_tmp.display()
    ));

    profile.push_str("(allow file-read* (literal \"/dev/null\"))\n");
    profile.push_str("(allow file-read* (literal \"/dev/urandom\"))\n");
    profile.push_str("(allow file-write* (literal \"/dev/null\"))\n");

    profile
}

pub fn wrap_with_seatbelt(profile: &str) -> Result<std::path::PathBuf, String> {
    let tmp = std::env::temp_dir()
        .join("lean-ctx-sandbox")
        .join("profile.sb");
    let parent = tmp
        .parent()
        .ok_or_else(|| "sandbox profile path has no parent directory".to_string())?;
    std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    std::fs::write(&tmp, profile).map_err(|e| e.to_string())?;
    Ok(tmp)
}

pub fn execute_sandboxed(
    interpreter: &str,
    args: &[&str],
    allowed_read_paths: &[&Path],
    env: &[(String, String)],
    timeout_secs: u64,
) -> Result<(String, String, i32), String> {
    let profile = seatbelt_profile(allowed_read_paths, interpreter);
    let profile_path = wrap_with_seatbelt(&profile)?;

    let mut cmd = Command::new("sandbox-exec");
    cmd.args(["-f", &profile_path.to_string_lossy()]);
    cmd.arg(interpreter);
    cmd.args(args);

    cmd.env_clear();
    cmd.env("PATH", "/usr/bin:/bin:/usr/local/bin");
    cmd.env("HOME", std::env::var("HOME").unwrap_or_default());
    cmd.env("LEAN_CTX_SANDBOX", "1");
    for (k, v) in env {
        cmd.env(k, v);
    }

    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());

    let child = cmd
        .spawn()
        .map_err(|e| format!("sandbox-exec spawn failed: {e}"))?;

    let output = wait_with_timeout(child, timeout_secs)?;

    Ok((
        String::from_utf8_lossy(&output.stdout).to_string(),
        String::from_utf8_lossy(&output.stderr).to_string(),
        output.status.code().unwrap_or(1),
    ))
}

fn wait_with_timeout(
    mut child: std::process::Child,
    timeout_secs: u64,
) -> Result<std::process::Output, String> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(timeout_secs);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return child.wait_with_output().map_err(|e| e.to_string()),
            Ok(None) => {
                if std::time::Instant::now() > deadline {
                    let _ = child.kill();
                    return Err(format!("Execution timed out after {timeout_secs}s"));
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            Err(e) => return Err(e.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

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
        let result = execute_sandboxed("/bin/echo", &["hello"], &[], &[], 5);
        assert!(result.is_ok());
        let (stdout, _, code) = result.unwrap();
        assert_eq!(code, 0);
        assert!(stdout.contains("hello"));
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
        );
        if let Ok((_, stderr, code)) = result {
            assert_ne!(code, 0, "curl should fail under sandbox: {stderr}");
        }
    }
}
