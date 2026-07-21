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
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();

    let json_str = extract_json(&stdout).unwrap_or_else(|| {
        eprintln!(
            "--- run_json debug ({} {}) ---\nexit={code}\nstdout[{}]={stdout}\nstderr[{}]={stderr}\n---",
            bin,
            args.join(" "),
            out.stdout.len(),
            out.stderr.len(),
        );
        stdout.clone()
    });
    (code, json_str)
}

fn extract_json(s: &str) -> Option<String> {
    let start = s.find('{')?;
    let end = s.rfind('}')?;
    if end >= start {
        Some(s[start..=end].to_string())
    } else {
        None
    }
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
#[cfg_attr(
    windows,
    ignore = "Windows handle inheritance causes subprocess hangs in CI"
)]
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
            "@echo off\r\nsetlocal\r\nset \"OUT=%USERPROFILE%\\claude-mcp.json\"\r\npowershell -NoProfile -Command \"[Console]::In.ReadToEnd() | Set-Content -Path $env:OUT -NoNewline\"\r\nexit /b 0\r\n",
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
    envs.push(("LEAN_CTX_TRUST_CLAUDE_PATH", "1"));

    // bootstrap --json returns clean JSON (SetupReport)
    let (code, out) = run_json(bin, &["bootstrap", "--json"], &envs);
    assert_eq!(code, 0, "bootstrap exit code");
    let setup: SetupReport = serde_json::from_str(&out).unwrap_or_else(|e| {
        panic!(
            "bootstrap JSON parse: {e}\nstdout[{}]=<<<{out}>>>",
            out.len()
        )
    });
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

    // init --agent claude --mode mcp should prefer `claude mcp add-json` when available.
    let out = Command::new(bin)
        .args(["init", "--agent", "claude", "--global", "--mode", "mcp"])
        .envs(envs.iter().copied())
        .output()
        .expect("init --agent claude --mode mcp");
    assert!(out.status.success(), "init --agent claude --mode mcp exit");
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
        .args(["init", "--agent", "claude", "--global", "--mode", "mcp"])
        .envs(envs.iter().copied())
        .output()
        .expect("init --agent claude --mode mcp");
    assert!(out.status.success(), "init --agent claude --mode mcp exit");

    let cfg_path = claude_cfg.join(".claude.json");
    let content = std::fs::read_to_string(&cfg_path).expect(".claude.json exists");
    assert!(
        content.contains("\"mcpServers\""),
        "must contain mcpServers"
    );
    assert!(content.contains("lean-ctx"), "must contain lean-ctx entry");

    // `doctor` is a health gate (#1046): it exits non-zero whenever *any* check
    // needs attention, and this sandbox legitimately fails several unrelated
    // ones (no lean-ctx on PATH, no shell profile, a fake `claude` that exits 1)
    // — the same reason the bootstrap smoke test tolerates a non-zero code. The
    // contract under test is the `.claude.json` fallback *detection*, which is
    // printed to stdout regardless of the gate, so assert on the report and only
    // guard the exit code against a hard error (panic/crash → not 0/1).
    let out = Command::new(bin)
        .args(["doctor"])
        .envs(envs.iter().copied())
        .output()
        .expect("doctor");
    let code = out.status.code().unwrap_or(-1);
    assert!(
        code == 0 || code == 1,
        "doctor must run as a health gate (exit 0 or 1), got {code}"
    );
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
    // v3 (GL #555): the injected block is a compact pointer. It references
    // LEAN-CTX.md *without* an `@` prefix on purpose — agents expand `@`
    // imports inline at session start, which would defeat the footprint cut
    // ("open on demand — do not auto-load").
    assert!(
        agents.contains("<!-- lean-ctx -->") && agents.contains("LEAN-CTX.md"),
        "must add compact lean-ctx pointer block"
    );
    assert!(
        !agents.contains("@LEAN-CTX.md"),
        "pointer must not use @-import (inline expansion defeats #555)"
    );
    assert_eq!(
        agents.matches("<!-- lean-ctx -->").count(),
        1,
        "must not duplicate marker block"
    );

    let lean_ctx_md = project.join("LEAN-CTX.md");
    let lean_ctx_content = std::fs::read_to_string(&lean_ctx_md).expect("LEAN-CTX.md exists");
    assert!(
        lean_ctx_content.contains("<!-- lean-ctx-rules -->")
            && (lean_ctx_content.contains("Tool selection by intent")
                || lean_ctx_content.contains("auto-route")),
        "LEAN-CTX.md must contain the canonical lean-ctx ruleset"
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
        .args(["init", "--agent", "claude", "--global", "--mode", "mcp"])
        .current_dir(&project)
        .envs(envs.iter().copied())
        .output()
        .expect("init --agent claude --global --mode mcp");
    assert!(
        out.status.success(),
        "init --agent claude --global --mode mcp exit"
    );

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

    // v3 (GL #555): the block is self-contained — Claude Code expands `@`
    // imports inline at launch, so the old `@rules/lean-ctx.md` pointer
    // silently multiplied the per-session footprint. Detail docs moved to
    // the on-demand skill; the CLAUDE.md block itself must stay compact.
    // v4 (GH #637 / GL #1138): MCP-aware — ctx_* guidance is conditional on
    // the tools actually existing in the session, and native Read → Edit is
    // documented as a guard-safe editing path (read-before-write gate).
    // v5 (#1008 / GL #1144): edits route through anchored ctx_patch, ctx_edit
    // is a legacy power-profile mention; v4's guard semantics stay documented.
    // v6: block content updated; version bump propagated here.
    // v7 (#1091): Replace-mode write guidance corrected; shared tag bumped.
    assert!(
        claude_md.contains("lean-ctx-claude-v7"),
        "CLAUDE.md must carry the v7 block version"
    );
    assert!(
        claude_md.contains("ctx_patch"),
        "v6 block must route edits to the anchored editor (#1008)"
    );
    assert!(
        claude_md.contains("prior native Read"),
        "v6 block must document Claude Code's path-keyed read-before-write gate"
    );
    assert!(
        !claude_md.contains("@rules/"),
        "v6 block must not @-import rules (inline expansion defeats #555)"
    );
    assert!(
        claude_md.len() < 2048,
        "compact block contract: CLAUDE.md footprint must stay <2 KB, got {}",
        claude_md.len()
    );

    assert!(
        !project.join("CLAUDE.md").exists(),
        "must not create project CLAUDE.md"
    );

    let skill_path = home.join(".claude/skills/lean-ctx/SKILL.md");
    assert!(
        skill_path.exists(),
        "must install the on-demand lean-ctx skill (detail docs live there)"
    );
}

// End-to-end: `lean-ctx init --agent augment` must drive setup.rs's
// `"augment" =>` match arm through configure_agent_mcp → the McpJson writer,
// landing at ~/.augment/settings.json with the standard mcpServers shape.
//
// Same Unix-only caveat as init_claude_installs_dedicated_rules_file_without_claude_md:
// on Windows `dirs::home_dir()` uses SHGetKnownFolderPath, not %USERPROFILE%,
// so env-var overrides don't reach the subprocess's HOME resolution.
#[test]
#[cfg_attr(windows, ignore)]
fn init_augment_installs_lean_ctx_mcp_into_dot_augment_settings() {
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

    let envs = [
        ("HOME", home_str.as_str()),
        ("LEAN_CTX_DATA_DIR", data_str.as_str()),
        ("LEAN_CTX_ACTIVE", "1"),
        ("LEAN_CTX_DISABLED", "1"),
        ("SHELL", "/bin/bash"),
    ];

    let out = Command::new(bin)
        .args(["init", "--agent", "augment", "--global", "--mode", "mcp"])
        .current_dir(&project)
        .envs(envs.iter().copied())
        .output()
        .expect("init --agent augment --global --mode mcp");
    assert!(
        out.status.success(),
        "init --agent augment exit; stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );

    let settings_path = home.join(".augment/settings.json");
    assert!(
        settings_path.exists(),
        "must create ~/.augment/settings.json"
    );
    let raw = std::fs::read_to_string(&settings_path).expect("settings.json readable");
    let json: serde_json::Value = serde_json::from_str(&raw).expect("settings.json is JSON");
    let lean_ctx = json
        .get("mcpServers")
        .and_then(|m| m.get("lean-ctx"))
        .expect("mcpServers.lean-ctx must be present");
    assert!(
        lean_ctx.get("command").and_then(|c| c.as_str()).is_some(),
        "lean-ctx entry must carry a command string"
    );

    // Rules injection (ticket #3): the per-agent rules file must land at
    // ~/.augment/rules/lean-ctx.md so Auggie actually knows lean-ctx exists.
    let rules_path = home.join(".augment/rules/lean-ctx.md");
    assert!(
        rules_path.exists(),
        "must create ~/.augment/rules/lean-ctx.md"
    );
    let rules = std::fs::read_to_string(&rules_path).expect("rules readable");
    assert!(
        rules.contains("<!-- lean-ctx-rules -->") && rules.contains("<!-- version: "),
        "rules file must carry the canonical marker + version comment"
    );

    // Ticket #2: the VS Code extension surface (augment.vscode-augment) keeps
    // its MCP server list as a top-level JSON array in globalStorage. The
    // exact path is OS-specific; we only assert on Linux/macOS here because
    // those are the platforms this test runs on (Windows is #[ignore]'d).
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        let vscode_path = {
            #[cfg(target_os = "linux")]
            {
                let user_dirs = [
                    home.join(".config/Code/User"),
                    home.join(".config/Code - Insiders/User"),
                    home.join(".vscode-server/data/User"),
                ];
                let user_dir = user_dirs
                    .iter()
                    .find(|p| p.exists())
                    .cloned()
                    .unwrap_or_else(|| user_dirs[0].clone());
                user_dir.join(
                    "globalStorage/augment.vscode-augment/augment-global-state/mcpServers.json",
                )
            }
            #[cfg(target_os = "macos")]
            {
                home.join("Library/Application Support/Code/User/globalStorage/augment.vscode-augment/augment-global-state/mcpServers.json")
            }
        };
        assert!(
            vscode_path.exists(),
            "must create augment vscode mcpServers.json at {}",
            vscode_path.display()
        );
        let raw = std::fs::read_to_string(&vscode_path).expect("vscode mcpServers.json readable");
        let arr: serde_json::Value =
            serde_json::from_str(&raw).expect("vscode mcpServers.json is JSON");
        let entries = arr.as_array().expect("vscode mcpServers.json is an array");
        let lean_ctx = entries
            .iter()
            .find(|e| e.get("name").and_then(|n| n.as_str()) == Some("lean-ctx"))
            .expect("array must contain a lean-ctx entry");
        assert_eq!(
            lean_ctx.get("type").and_then(|t| t.as_str()),
            Some("stdio"),
            "augment vscode entry must declare stdio transport"
        );
        assert!(
            lean_ctx.get("id").and_then(|i| i.as_str()).is_some(),
            "augment vscode entry must carry a stable id"
        );
        assert!(
            lean_ctx.get("command").and_then(|c| c.as_str()).is_some(),
            "augment vscode entry must carry a command string"
        );
        assert!(
            lean_ctx.get("env").is_none(),
            "data dir is auto-detected at runtime, not pinned into the config (GH #408)"
        );
    }
}
