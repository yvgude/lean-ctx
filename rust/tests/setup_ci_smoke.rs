use std::process::Command;

use lean_ctx::core::setup_report::SetupReport;
use lean_ctx::status::StatusReport;
use lean_ctx::token_report::TokenReport;

fn run_json(bin: &str, args: &[&str], envs: &[(&str, &str)]) -> (i32, String) {
    let mut cmd = Command::new(bin);
    cmd.args(args);
    for (k, v) in envs {
        cmd.env(k, v);
    }
    let out = cmd.output().expect("process start");
    let code = out.status.code().unwrap_or(1);
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    (code, stdout)
}

fn write_exe(path: &std::path::Path, content: &str) {
    std::fs::write(path, content).expect("write");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(path).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(path, perms).unwrap();
    }
}

#[test]
fn setup_bootstrap_doctor_status_json_smoke() {
    let bin = env!("CARGO_BIN_EXE_lean-ctx");

    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    let data_dir = tmp.path().join("data");
    std::fs::create_dir_all(&data_dir).unwrap();
    let bin_dir = tmp.path().join("bin");
    std::fs::create_dir_all(&bin_dir).unwrap();

    let home_str = home.to_string_lossy().to_string();
    let data_str = data_dir.to_string_lossy().to_string();

    // Fake claude binary so we can verify `claude mcp add-json` integration.
    // It writes stdin JSON to $HOME/claude-mcp.json and exits 0.
    let claude_path = bin_dir.join(if cfg!(windows) {
        "claude.cmd"
    } else {
        "claude"
    });
    if cfg!(windows) {
        write_exe(
            &claude_path,
            "@echo off\r\nsetlocal\r\nset \"OUT=%USERPROFILE%\\claude-mcp.json\"\r\nmore > \"%OUT%\"\r\nexit /b %errorlevel%\r\n",
        );
    } else {
        write_exe(
            &claude_path,
            "#!/bin/sh\nset -eu\nOUT=\"$HOME/claude-mcp.json\"\ncat > \"$OUT\"\nexit 0\n",
        );
    }

    let mut envs = vec![
        ("HOME", home_str.as_str()),
        ("LEAN_CTX_DATA_DIR", data_str.as_str()),
        ("LEAN_CTX_ACTIVE", "1"),
        ("LEAN_CTX_DISABLED", "1"),
    ];

    #[cfg(not(windows))]
    {
        envs.push(("SHELL", "/bin/bash"));
    }
    #[cfg(windows)]
    {
        envs.push(("USERPROFILE", home_str.as_str()));
    }

    // Prefer our fake claude first in PATH.
    let old_path = std::env::var("PATH").unwrap_or_default();
    let sep = if cfg!(windows) { ";" } else { ":" };
    let new_path = format!("{}{sep}{}", bin_dir.to_string_lossy(), old_path);
    envs.push(("PATH", new_path.as_str()));

    // bootstrap --json returns clean JSON (SetupReport)
    let (code, out) = run_json(bin, &["bootstrap", "--json"], &envs);
    assert_eq!(code, 0, "bootstrap exit code");
    let setup: SetupReport = serde_json::from_str(&out).expect("bootstrap JSON parse");
    assert_eq!(setup.schema_version, 1);

    // bootstrap should create env.sh in LEAN_CTX_DATA_DIR for Docker/CI shells.
    // env.sh is Unix-only (shell script); skip assertion on Windows.
    #[cfg(not(windows))]
    {
        let env_sh = data_dir.join("env.sh");
        let env_sh_content = std::fs::read_to_string(&env_sh).expect("env.sh exists");
        assert!(
            env_sh_content.contains("lean-ctx docker self-heal"),
            "env.sh missing docker self-heal snippet"
        );
    }

    // init --agent claude should prefer `claude mcp add-json` when available.
    let out = Command::new(bin)
        .args(["init", "--agent", "claude", "--global"])
        .envs(envs.iter().copied())
        .output()
        .expect("init --agent claude");
    assert!(out.status.success(), "init --agent claude exit");
    let saved = std::fs::read_to_string(home.join("claude-mcp.json")).expect("claude-mcp.json");
    let v: serde_json::Value = serde_json::from_str(&saved).expect("claude json parse");
    assert!(
        v.get("command").is_some(),
        "claude input should be server entry json"
    );

    // doctor --fix --json returns clean JSON (SetupReport shape)
    // Exit code may be 1 if doctor finds unfixable issues (e.g. no real shell profile in CI)
    let (_code, out) = run_json(bin, &["doctor", "--fix", "--json"], &envs);
    let doctor_report: SetupReport = serde_json::from_str(&out).expect("doctor JSON parse");
    assert_eq!(doctor_report.schema_version, 1);

    // status --json returns clean JSON
    let (code, out) = run_json(bin, &["status", "--json"], &envs);
    assert_eq!(code, 0, "status exit code");
    let status: StatusReport = serde_json::from_str(&out).expect("status JSON parse");
    assert_eq!(status.schema_version, 1);

    // token-report --json returns clean JSON
    let (code, out) = run_json(bin, &["token-report", "--json"], &envs);
    assert_eq!(code, 0, "token-report exit code");
    let report: TokenReport = serde_json::from_str(&out).expect("token-report JSON parse");
    assert_eq!(report.schema_version, 1);
}

#[test]
fn claude_config_dir_fallback_writes_dot_claude_json() {
    let bin = env!("CARGO_BIN_EXE_lean-ctx");

    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    let data_dir = tmp.path().join("data");
    std::fs::create_dir_all(&data_dir).unwrap();
    let bin_dir = tmp.path().join("bin");
    std::fs::create_dir_all(&bin_dir).unwrap();

    let claude_cfg = tmp.path().join("claude-cfg");
    std::fs::create_dir_all(&claude_cfg).unwrap();

    let home_str = home.to_string_lossy().to_string();
    let data_str = data_dir.to_string_lossy().to_string();
    let claude_cfg_str = claude_cfg.to_string_lossy().to_string();

    // Fake claude that fails (forces lean-ctx to fallback to file merge/write).
    let claude_path = bin_dir.join(if cfg!(windows) {
        "claude.cmd"
    } else {
        "claude"
    });
    if cfg!(windows) {
        write_exe(&claude_path, "@echo off\r\nexit /b 1\r\n");
    } else {
        write_exe(&claude_path, "#!/bin/sh\nexit 1\n");
    }

    let mut envs = vec![
        ("HOME", home_str.as_str()),
        ("LEAN_CTX_DATA_DIR", data_str.as_str()),
        ("LEAN_CTX_ACTIVE", "1"),
        ("LEAN_CTX_DISABLED", "1"),
        ("CLAUDE_CONFIG_DIR", claude_cfg_str.as_str()),
    ];

    #[cfg(not(windows))]
    {
        envs.push(("SHELL", "/bin/bash"));
    }
    #[cfg(windows)]
    {
        envs.push(("USERPROFILE", home_str.as_str()));
    }

    let old_path = std::env::var("PATH").unwrap_or_default();
    let sep = if cfg!(windows) { ";" } else { ":" };
    let new_path = format!("{}{sep}{}", bin_dir.to_string_lossy(), old_path);
    envs.push(("PATH", new_path.as_str()));

    let out = Command::new(bin)
        .args(["init", "--agent", "claude", "--global"])
        .envs(envs.iter().copied())
        .output()
        .expect("init --agent claude");
    assert!(out.status.success(), "init --agent claude exit");

    let cfg_path = claude_cfg.join(".claude.json");
    let content = std::fs::read_to_string(&cfg_path).expect(".claude.json exists");
    assert!(
        content.contains("\"mcpServers\""),
        "must contain mcpServers"
    );
    assert!(content.contains("lean-ctx"), "must contain lean-ctx entry");

    let out = Command::new(bin)
        .args(["doctor"])
        .envs(envs.iter().copied())
        .output()
        .expect("doctor");
    assert!(out.status.success(), "doctor exit");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("MCP config") && stdout.contains("lean-ctx found"),
        "doctor should report lean-ctx found in MCP config; got:\n{stdout}"
    );
}

#[test]
fn init_agent_preserves_agents_md_and_is_idempotent() {
    let bin = env!("CARGO_BIN_EXE_lean-ctx");

    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    let data_dir = tmp.path().join("data");
    std::fs::create_dir_all(&data_dir).unwrap();
    let bin_dir = tmp.path().join("bin");
    std::fs::create_dir_all(&bin_dir).unwrap();
    let project = tmp.path().join("project");
    std::fs::create_dir_all(&project).unwrap();

    // Create a git repo so project files are generated.
    std::fs::create_dir_all(project.join(".git")).unwrap();

    // Existing user AGENTS.md should be preserved.
    let agents_path = project.join("AGENTS.md");
    std::fs::write(&agents_path, "# My Agents\n\nDo not overwrite.\n").unwrap();

    let home_str = home.to_string_lossy().to_string();
    let data_str = data_dir.to_string_lossy().to_string();

    // Fake claude (success) so init --agent claude prefers `claude mcp add-json`.
    let claude_path = bin_dir.join(if cfg!(windows) {
        "claude.cmd"
    } else {
        "claude"
    });
    if cfg!(windows) {
        write_exe(&claude_path, "@echo off\r\nrem succeed\r\nexit /b 0\r\n");
    } else {
        write_exe(&claude_path, "#!/bin/sh\nexit 0\n");
    }

    let mut envs = vec![
        ("HOME", home_str.as_str()),
        ("LEAN_CTX_DATA_DIR", data_str.as_str()),
        ("LEAN_CTX_ACTIVE", "1"),
        ("LEAN_CTX_DISABLED", "1"),
    ];
    #[cfg(not(windows))]
    {
        envs.push(("SHELL", "/bin/bash"));
    }
    #[cfg(windows)]
    {
        envs.push(("USERPROFILE", home_str.as_str()));
    }

    let old_path = std::env::var("PATH").unwrap_or_default();
    let sep = if cfg!(windows) { ";" } else { ":" };
    let new_path = format!("{}{sep}{}", bin_dir.to_string_lossy(), old_path);
    envs.push(("PATH", new_path.as_str()));

    for _ in 0..2 {
        let out = Command::new(bin)
            .args(["init", "--agent", "claude"])
            .current_dir(&project)
            .envs(envs.iter().copied())
            .output()
            .expect("init --agent claude");
        assert!(out.status.success(), "init --agent claude exit");
    }

    let agents = std::fs::read_to_string(&agents_path).unwrap();
    assert!(agents.contains("# My Agents"), "must preserve user header");
    assert!(
        agents.contains("Do not overwrite."),
        "must preserve user content"
    );
    assert!(
        agents.contains("<!-- lean-ctx -->") && agents.contains("@LEAN-CTX.md"),
        "must add lean-ctx reference block"
    );
    assert_eq!(
        agents.matches("<!-- lean-ctx -->").count(),
        1,
        "must not duplicate marker block"
    );

    let lean_ctx_md = project.join("LEAN-CTX.md");
    let lean_ctx_content = std::fs::read_to_string(&lean_ctx_md).expect("LEAN-CTX.md exists");
    assert!(
        lean_ctx_content.contains("lean-ctx — Context Engineering Layer"),
        "LEAN-CTX.md must contain rules"
    );
}

/// On Windows, `dirs::home_dir()` uses the Win32 API (`SHGetKnownFolderPath`)
/// rather than `USERPROFILE`, so env-var overrides in the subprocess don't
/// control where files land. We can only verify file creation on Unix.
#[test]
#[cfg_attr(windows, ignore)]
fn init_claude_installs_dedicated_rules_file_without_claude_md() {
    let bin = env!("CARGO_BIN_EXE_lean-ctx");

    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    let data_dir = tmp.path().join("data");
    std::fs::create_dir_all(&data_dir).unwrap();
    let project = tmp.path().join("project");
    std::fs::create_dir_all(&project).unwrap();

    let home_str = home.to_string_lossy().to_string();
    let data_str = data_dir.to_string_lossy().to_string();

    let mut envs = vec![
        ("HOME", home_str.as_str()),
        ("LEAN_CTX_DATA_DIR", data_str.as_str()),
        ("LEAN_CTX_ACTIVE", "1"),
        ("LEAN_CTX_DISABLED", "1"),
    ];
    #[cfg(not(windows))]
    {
        envs.push(("SHELL", "/bin/bash"));
    }
    #[cfg(windows)]
    {
        envs.push(("USERPROFILE", home_str.as_str()));
    }

    let out = Command::new(bin)
        .args(["init", "--agent", "claude", "--global"])
        .current_dir(&project)
        .envs(envs.iter().copied())
        .output()
        .expect("init --agent claude --global");
    assert!(out.status.success(), "init --agent claude --global exit");

    let claude_md_path = home.join(".claude/CLAUDE.md");
    assert!(
        claude_md_path.exists(),
        "must create ~/.claude/CLAUDE.md with lean-ctx block"
    );
    let claude_md = std::fs::read_to_string(&claude_md_path).expect("CLAUDE.md readable");
    assert!(
        claude_md.contains("<!-- lean-ctx -->"),
        "CLAUDE.md must contain lean-ctx marker block"
    );
    assert!(
        claude_md.contains("@rules/lean-ctx.md"),
        "CLAUDE.md must import rules file"
    );

    assert!(
        !project.join("CLAUDE.md").exists(),
        "must not create project CLAUDE.md"
    );

    let rules_path = home.join(".claude/rules/lean-ctx.md");
    assert!(
        rules_path.exists(),
        "must create dedicated Claude rules file"
    );
    let content = std::fs::read_to_string(&rules_path).expect("rules readable");
    assert!(
        content.contains("lean-ctx-rules-"),
        "rules must contain marker"
    );
}
