//! Golden-path contract (zero-config excellence): `lean-ctx onboard --yes` on a
//! fresh machine with detected-but-unconfigured agents must leave `doctor` fully
//! green — no "run: lean-ctx setup" follow-ups, no dead loops.
//!
//! This is the Rust twin of the manual fresh-install E2E journey: it caught
//! three real bugs (SKILL.md never installed on the golden path, pipe-guard
//! false positive under LEAN_CTX_CONFIG_DIR, Claude state-dir detection
//! mismatch between doctor and setup).

use std::process::Command;

/// Stops the daemon `onboard` started (scoped to the test's data dir), even
/// when an assertion panics mid-test.
struct DaemonStop<'a> {
    bin: &'a str,
    envs: Vec<(String, String)>,
}

impl Drop for DaemonStop<'_> {
    fn drop(&mut self) {
        let _ = Command::new(self.bin)
            .arg("stop")
            .env_clear()
            .envs(self.envs.iter().map(|(k, v)| (k.as_str(), v.as_str())))
            .output();
    }
}

#[test]
#[cfg_attr(
    windows,
    ignore = "HOME-override isolation is Unix-only (dirs::home_dir uses the Win32 API)"
)]
fn onboard_yes_leaves_doctor_fully_green() {
    let bin = env!("CARGO_BIN_EXE_lean-ctx");

    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let proj = tmp.path().join("proj");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&proj).unwrap();
    std::fs::create_dir_all(proj.join(".git")).unwrap();

    // Agents that exist but were never wired to lean-ctx. A bare state dir is
    // exactly what doctor/rules/skills treat as "installed" — setup must agree.
    std::fs::create_dir_all(home.join(".cursor")).unwrap();
    std::fs::create_dir_all(home.join(".claude")).unwrap();
    std::fs::create_dir_all(home.join(".codex")).unwrap();

    let bin_dir = std::path::Path::new(bin).parent().unwrap();
    let envs: Vec<(String, String)> = vec![
        ("HOME".into(), home.to_string_lossy().into_owned()),
        (
            "PATH".into(),
            format!("{}:/usr/bin:/bin", bin_dir.to_string_lossy()),
        ),
        (
            "LEAN_CTX_CONFIG_DIR".into(),
            home.join("cfg").to_string_lossy().into_owned(),
        ),
        (
            "LEAN_CTX_DATA_DIR".into(),
            home.join("data").to_string_lossy().into_owned(),
        ),
        (
            "CODEX_HOME".into(),
            home.join(".codex").to_string_lossy().into_owned(),
        ),
        ("SHELL".into(), "/bin/bash".into()),
        ("NO_COLOR".into(), "1".into()),
        ("TERM".into(), "dumb".into()),
        // Hermetic: doctor must not try to fetch embedding models in CI.
        ("LEAN_CTX_EMBEDDINGS_AUTO_DOWNLOAD".into(), "0".into()),
    ];
    let _stop = DaemonStop {
        bin,
        envs: envs.clone(),
    };

    let onboard = Command::new(bin)
        .args(["onboard", "--yes"])
        .current_dir(&proj)
        .env_clear()
        .envs(envs.iter().map(|(k, v)| (k.as_str(), v.as_str())))
        .output()
        .expect("onboard spawn");
    assert!(
        onboard.status.success(),
        "onboard --yes must exit 0; stderr:\n{}",
        String::from_utf8_lossy(&onboard.stderr)
    );

    let doctor = Command::new(bin)
        .arg("doctor")
        .current_dir(&proj)
        .env_clear()
        .envs(envs.iter().map(|(k, v)| (k.as_str(), v.as_str())))
        .output()
        .expect("doctor spawn");
    let stdout = String::from_utf8_lossy(&doctor.stdout);

    // The two checks that historically dead-looped ("run: lean-ctx setup"
    // right after onboard already ran): Claude plan mode + CLAUDE.md block.
    assert!(
        stdout.contains("permissions present"),
        "Claude plan-mode permissions must be wired by onboard; doctor said:\n{stdout}"
    );
    assert!(
        !stdout.contains("run: lean-ctx setup"),
        "doctor must never point back to setup right after onboard ran; doctor said:\n{stdout}"
    );

    // doctor is a health gate (#1046): exit 0 == every counted check green.
    assert!(
        doctor.status.success(),
        "doctor must be fully green right after onboard --yes; output:\n{stdout}"
    );
}
