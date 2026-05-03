use serde_json::Value;
use std::path::PathBuf;

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
fn claude_mcp_add_json_used_when_available() {
    if cfg!(windows) {
        return;
    }
    let _g = lean_ctx::core::data_dir::test_env_lock();
    let tmp = tempfile::tempdir().expect("tempdir");
    let home = tmp.path().join("home");
    let bin_dir = tmp.path().join("bin");
    std::fs::create_dir_all(&home).expect("mkdir home");
    std::fs::create_dir_all(&bin_dir).expect("mkdir bin");

    // Fake claude binary that accepts: claude mcp add-json --scope user lean-ctx
    // It writes the stdin JSON to a file so we can assert it was used.
    let claude = bin_dir.join(if cfg!(windows) {
        "claude.cmd"
    } else {
        "claude"
    });
    if cfg!(windows) {
        write_exe(
            &claude,
            "@echo off\r\nset OUT=%HOME%\\claude-mcp.json\r\nmore > \"%OUT%\"\r\nexit /b 0\r\n",
        );
    } else {
        write_exe(
            &claude,
            "#!/bin/sh\nset -eu\nOUT=\"$HOME/claude-mcp.json\"\ncat > \"$OUT\"\nexit 0\n",
        );
    }

    let old_home = std::env::var("HOME").ok();
    std::env::set_var("HOME", &home);
    let old_path = std::env::var("PATH").unwrap_or_default();
    let new_path = format!("{}:{}", bin_dir.to_string_lossy(), old_path);
    std::env::set_var("PATH", new_path);

    let targets = lean_ctx::core::editor_registry::detect::build_targets(&home);
    let claude_target = targets
        .iter()
        .find(|t| t.agent_key == "claude")
        .expect("claude target");

    let bin = std::env::current_exe()
        .unwrap_or_else(|_| PathBuf::from("lean-ctx"))
        .to_string_lossy()
        .to_string();

    let res =
        lean_ctx::core::editor_registry::writers::write_config(claude_target, &bin).expect("write");
    assert!(
        res.note.as_deref() == Some("via claude mcp add-json"),
        "{res:?}"
    );

    let saved = std::fs::read_to_string(home.join("claude-mcp.json")).expect("saved json");
    let v: Value = serde_json::from_str(&saved).expect("parse");
    assert!(v.get("command").is_some(), "must be server entry json");

    // Restore env
    if let Some(h) = old_home {
        std::env::set_var("HOME", h);
    }
    std::env::set_var("PATH", old_path);
}
