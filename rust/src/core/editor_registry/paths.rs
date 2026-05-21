use std::path::{Path, PathBuf};

pub fn zed_settings_path(home: &std::path::Path) -> PathBuf {
    if cfg!(target_os = "macos") {
        home.join("Library/Application Support/Zed/settings.json")
    } else {
        home.join(".config/zed/settings.json")
    }
}

pub fn zed_config_dir(home: &std::path::Path) -> PathBuf {
    if cfg!(target_os = "macos") {
        home.join("Library/Application Support/Zed")
    } else {
        home.join(".config/zed")
    }
}

pub fn vscode_mcp_path() -> PathBuf {
    if let Some(home) = dirs::home_dir() {
        #[cfg(target_os = "macos")]
        {
            return home.join("Library/Application Support/Code/User/mcp.json");
        }
        #[cfg(target_os = "linux")]
        {
            return home.join(".config/Code/User/mcp.json");
        }
        #[cfg(target_os = "windows")]
        {
            if let Ok(appdata) = std::env::var("APPDATA") {
                return PathBuf::from(appdata).join("Code/User/mcp.json");
            }
        }
        #[allow(unreachable_code)]
        home.join(".config/Code/User/mcp.json")
    } else {
        PathBuf::from("/nonexistent")
    }
}

pub fn qoder_mcp_path(home: &Path) -> PathBuf {
    #[cfg(target_os = "windows")]
    {
        if let Ok(appdata) = std::env::var("APPDATA") {
            return PathBuf::from(appdata)
                .join("Qoder")
                .join("SharedClientCache")
                .join("mcp.json");
        }
    }
    home.join(".qoder").join("mcp.json")
}

#[cfg(target_os = "macos")]
pub fn qoder_mcp_paths(home: &Path) -> Vec<PathBuf> {
    let mut paths = vec![qoder_mcp_path(home)];
    paths.push(home.join("Library/Application Support/Qoder/User/mcp.json"));
    paths.push(home.join("Library/Application Support/Qoder/SharedClientCache/mcp.json"));
    paths
}

#[cfg(not(target_os = "macos"))]
pub fn qoder_mcp_paths(home: &Path) -> Vec<PathBuf> {
    vec![qoder_mcp_path(home)]
}

#[allow(unreachable_code)]
pub fn cline_mcp_path() -> PathBuf {
    #[cfg(target_os = "windows")]
    {
        if let Ok(appdata) = std::env::var("APPDATA") {
            return PathBuf::from(appdata).join(
                "Code/User/globalStorage/saoudrizwan.claude-dev/settings/cline_mcp_settings.json",
            );
        }
        return PathBuf::from("/nonexistent");
    }

    let Some(home) = dirs::home_dir() else {
        return PathBuf::from("/nonexistent");
    };
    #[cfg(target_os = "macos")]
    {
        return home.join("Library/Application Support/Code/User/globalStorage/saoudrizwan.claude-dev/settings/cline_mcp_settings.json");
    }
    #[cfg(target_os = "linux")]
    {
        return home.join(".config/Code/User/globalStorage/saoudrizwan.claude-dev/settings/cline_mcp_settings.json");
    }
    PathBuf::from("/nonexistent")
}

#[allow(unreachable_code)]
pub fn roo_mcp_path() -> PathBuf {
    #[cfg(target_os = "windows")]
    {
        if let Ok(appdata) = std::env::var("APPDATA") {
            return PathBuf::from(appdata)
                .join("Code/User/globalStorage/rooveterinaryinc.roo-cline/settings/cline_mcp_settings.json");
        }
        return PathBuf::from("/nonexistent");
    }

    let Some(home) = dirs::home_dir() else {
        return PathBuf::from("/nonexistent");
    };
    #[cfg(target_os = "macos")]
    {
        return home.join("Library/Application Support/Code/User/globalStorage/rooveterinaryinc.roo-cline/settings/cline_mcp_settings.json");
    }
    #[cfg(target_os = "linux")]
    {
        return home.join(".config/Code/User/globalStorage/rooveterinaryinc.roo-cline/settings/cline_mcp_settings.json");
    }
    PathBuf::from("/nonexistent")
}

pub fn qoder_settings_path(home: &Path) -> PathBuf {
    #[cfg(target_os = "windows")]
    {
        if let Ok(appdata) = std::env::var("APPDATA") {
            return PathBuf::from(appdata)
                .join("Qoder")
                .join("SharedClientCache")
                .join("mcp.json");
        }
    }
    home.join(".qoder/mcp.json")
}

pub fn qoder_all_mcp_paths(home: &Path) -> Vec<PathBuf> {
    let paths = vec![qoder_settings_path(home)];
    #[cfg(target_os = "macos")]
    let paths = {
        let mut paths = paths;
        paths.push(home.join("Library/Application Support/Qoder/User/mcp.json"));
        paths.push(home.join("Library/Application Support/Qoder/SharedClientCache/mcp.json"));
        paths
    };
    paths
}

pub fn qoderwork_mcp_path(home: &Path) -> PathBuf {
    home.join(".qoderwork/mcp.json")
}

pub fn claude_mcp_json_path(home: &Path) -> PathBuf {
    if let Ok(dir) = std::env::var("CLAUDE_CONFIG_DIR") {
        let dir = dir.trim();
        if !dir.is_empty() {
            return PathBuf::from(dir).join(".claude.json");
        }
    }
    home.join(".claude.json")
}

pub fn claude_state_dir(home: &Path) -> PathBuf {
    if let Ok(dir) = std::env::var("CLAUDE_CONFIG_DIR") {
        let dir = dir.trim();
        if !dir.is_empty() {
            return PathBuf::from(dir);
        }
    }
    home.join(".claude")
}

pub fn claude_rules_dir(home: &Path) -> PathBuf {
    claude_state_dir(home).join("rules")
}

pub fn augment_cli_settings_path(home: &Path) -> PathBuf {
    home.join(".augment/settings.json")
}

/// MCP server list for the Augment VS Code extension.
///
/// The extension persists registered MCP servers as a top-level JSON array in
/// its globalStorage directory. Confirmed empirically against
/// `augment.vscode-augment` build shipped on 2026-05-21 (see PR description).
///
/// On Windows the User dir lives under `%APPDATA%/Code` rather than the
/// user's home, so we honour that when the env var is set; we fall back to
/// the home-relative path for tests and unusual setups.
pub fn augment_vscode_mcp_path(home: &Path) -> PathBuf {
    const TAIL: &str = "globalStorage/augment.vscode-augment/augment-global-state/mcpServers.json";

    #[cfg(target_os = "macos")]
    {
        return home
            .join("Library/Application Support/Code/User")
            .join(TAIL);
    }
    #[cfg(target_os = "linux")]
    {
        return home.join(".config/Code/User").join(TAIL);
    }
    #[cfg(target_os = "windows")]
    {
        if let Ok(appdata) = std::env::var("APPDATA") {
            return PathBuf::from(appdata).join("Code/User").join(TAIL);
        }
    }
    #[allow(unreachable_code)]
    home.join(".config/Code/User").join(TAIL)
}

#[cfg(test)]
mod augment_tests {
    use super::*;

    #[test]
    fn augment_cli_settings_path_is_under_dot_augment() {
        let home = Path::new("/home/tester");
        assert_eq!(
            augment_cli_settings_path(home),
            home.join(".augment").join("settings.json")
        );
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn augment_vscode_mcp_path_uses_linux_globalstorage() {
        let home = Path::new("/home/tester");
        assert_eq!(
            augment_vscode_mcp_path(home),
            home.join(".config/Code/User/globalStorage/augment.vscode-augment/augment-global-state/mcpServers.json")
        );
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn augment_vscode_mcp_path_uses_macos_application_support() {
        let home = Path::new("/Users/tester");
        assert_eq!(
            augment_vscode_mcp_path(home),
            home.join("Library/Application Support/Code/User/globalStorage/augment.vscode-augment/augment-global-state/mcpServers.json")
        );
    }

    /// On Windows we honour `%APPDATA%` when set, falling back to a
    /// home-relative path only when it is missing. We can't reliably mutate
    /// process-wide env vars in a parallel test runner, so this test only
    /// asserts the invariant tail (which is platform-agnostic) and that the
    /// final segment is the expected file name. Both branches share that tail.
    #[test]
    #[cfg(target_os = "windows")]
    fn augment_vscode_mcp_path_ends_with_globalstorage_tail() {
        let home = Path::new("C:/Users/tester");
        let path = augment_vscode_mcp_path(home);
        let s = path.to_string_lossy().replace('\\', "/");
        assert!(
            s.ends_with(
                "Code/User/globalStorage/augment.vscode-augment/augment-global-state/mcpServers.json"
            ),
            "unexpected windows path: {s}"
        );
    }
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;

    #[test]
    #[cfg(target_os = "macos")]
    fn qoder_mcp_paths_include_macos_user_and_shared_cache_locations() {
        let home = Path::new("/Users/tester");
        let paths = qoder_mcp_paths(home);

        assert_eq!(
            paths,
            vec![
                home.join(".qoder/mcp.json"),
                home.join("Library/Application Support/Qoder/User/mcp.json"),
                home.join("Library/Application Support/Qoder/SharedClientCache/mcp.json"),
            ]
        );
    }
}
