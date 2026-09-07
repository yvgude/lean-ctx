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
            return resolve_vscode_global_storage(&home, "User/mcp.json");
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

/// VS Code **Insiders** user-scope MCP config. Insiders keeps a fully
/// separate profile dir (`Code - Insiders`), so a server registered in
/// stable's `Code/User/mcp.json` simply does not exist there — an Insiders
/// user then sees an empty `MCP: Open User Configuration` even after
/// `lean-ctx setup` succeeded (GH #694). On Linux `vscode_mcp_path()` can
/// already resolve to Insiders via the shared fallback chain when it is the
/// only install; this path is the *dedicated* Insiders location for the
/// distinct-target case.
pub fn vscode_insiders_mcp_path() -> PathBuf {
    let Some(home) = dirs::home_dir() else {
        return PathBuf::from("/nonexistent");
    };
    #[cfg(target_os = "macos")]
    {
        home.join("Library/Application Support/Code - Insiders/User/mcp.json")
    }
    #[cfg(target_os = "windows")]
    {
        if let Ok(appdata) = std::env::var("APPDATA") {
            return PathBuf::from(appdata).join("Code - Insiders/User/mcp.json");
        }
        home.join("AppData/Roaming/Code - Insiders/User/mcp.json")
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        home.join(".config/Code - Insiders/User/mcp.json")
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

/// Cline CLI's MCP settings file. Unified across the Cline IDE extension,
/// CLI, and SDK since 2026.7 (`~/.cline/data/settings/cline_mcp_settings.json`),
/// separate from the VS Code extension's own globalStorage copy
/// (`cline_mcp_path`, below) that predates the unification. Honors the same
/// overrides as Cline's own `resolveMcpSettingsPath()`: `CLINE_MCP_SETTINGS_PATH`
/// wins outright, otherwise `CLINE_DATA_DIR` replaces the `~/.cline` root.
pub fn cline_cli_mcp_settings_path() -> PathBuf {
    if let Ok(explicit) = std::env::var("CLINE_MCP_SETTINGS_PATH") {
        let trimmed = explicit.trim();
        if !trimmed.is_empty() {
            return PathBuf::from(trimmed);
        }
    }
    let data_dir = std::env::var("CLINE_DATA_DIR")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|home| home.join(".cline")))
        .unwrap_or_else(|| PathBuf::from("/nonexistent"));
    data_dir.join("data/settings/cline_mcp_settings.json")
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
        let suffix = "User/globalStorage/saoudrizwan.claude-dev/settings/cline_mcp_settings.json";
        return resolve_vscode_global_storage(&home, suffix);
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
        let suffix =
            "User/globalStorage/rooveterinaryinc.roo-cline/settings/cline_mcp_settings.json";
        return resolve_vscode_global_storage(&home, suffix);
    }
    PathBuf::from("/nonexistent")
}

/// Resolves the correct VS Code-family base directory on Linux.
/// Checks VSCodium, Code - OSS, Code, Code - Insiders, then the dev-container
/// server dir (in that order) — returns the first existing path, falling back
/// to the standard `Code` path for fresh installs. Most-specific first: a
/// VSCodium user with a leftover `~/.config/Code` (one accidental Code launch
/// is enough) must keep resolving to VSCodium. Dev containers use
/// `.vscode-server/data` instead of `.config`, so we check both.
#[cfg(target_os = "linux")]
fn resolve_vscode_global_storage(home: &Path, suffix: &str) -> PathBuf {
    const CANDIDATES: &[&str] = &[
        ".config/VSCodium",
        ".config/Code - OSS",
        ".config/Code",
        ".config/Code - Insiders",
        ".vscode-server/data",
    ];
    for base in CANDIDATES {
        let path = home.join(base);
        if path.exists() {
            return path.join(suffix);
        }
    }
    home.join(".config/Code").join(suffix)
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

/// Oh My Pi's user-level agent directory.
///
/// OMP keeps its native MCP config (`mcp.json`) and its user instruction file
/// (`AGENTS.md`) together under `~/.omp/agent`. `PI_CODING_AGENT_DIR` is OMP's
/// documented *full* override of that directory: when it is set, OMP itself
/// reads from there, so lean-ctx must write to the very same place instead of
/// re-deriving a path under `$HOME`.
pub fn omp_agent_dir(home: &Path) -> PathBuf {
    omp_agent_dir_from(home, std::env::var("PI_CODING_AGENT_DIR").ok().as_deref())
}

/// Env-free core of [`omp_agent_dir`] so the override semantics stay testable
/// without mutating process-global environment state. A blank/whitespace-only
/// override is not a usable directory and falls back to the default layout.
pub(crate) fn omp_agent_dir_from(home: &Path, override_dir: Option<&str>) -> PathBuf {
    if let Some(explicit) = override_dir.map(str::trim).filter(|dir| !dir.is_empty()) {
        return PathBuf::from(explicit);
    }
    home.join(".omp/agent")
}

pub fn omp_mcp_path(home: &Path) -> PathBuf {
    omp_agent_dir(home).join("mcp.json")
}

pub fn omp_agents_path(home: &Path) -> PathBuf {
    omp_agent_dir(home).join("AGENTS.md")
}

/// Qoder CLI stores user-scoped MCP servers in the shared Qoder settings file.
/// This is intentionally separate from Qoder IDE's `mcp.json` locations: the
/// two applications use the same `.qoder` state directory but do not consume
/// the same MCP configuration file.
pub fn qodercli_settings_path(home: &Path) -> PathBuf {
    home.join(".qoder/settings.json")
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

pub fn codebuddy_mcp_json_path(home: &Path) -> PathBuf {
    codebuddy_state_dir(home).join("mcp.json")
}

pub fn codebuddy_state_dir(home: &Path) -> PathBuf {
    if let Ok(dir) = std::env::var("CODEBUDDY_CONFIG_DIR") {
        let dir = dir.trim();
        if !dir.is_empty() {
            return PathBuf::from(dir);
        }
    }
    home.join(".codebuddy")
}

pub fn codebuddy_rules_dir(home: &Path) -> PathBuf {
    codebuddy_state_dir(home).join("rules")
}

/// CodeWhale's user-level MCP config (GH #1402).
///
/// Upstream resolution order (CodeWhale `docs/MCP.md`, verified 2026-09-07):
///   1. `DEEPSEEK_MCP_CONFIG` — explicit override, still carrying the
///      pre-rename env var name.
///   2. `~/.codewhale/mcp.json` — current path.
///   3. `~/.deepseek/mcp.json` — legacy path, read only while the CodeWhale
///      file is absent.
///
/// We mirror that order exactly so `init`/`setup`/`doctor`/`uninstall` all
/// touch the one file CodeWhale actually reads. Writing both would leave a
/// lean-ctx entry in the shadowed file that the user never sees loaded and
/// that a later `uninstall` of the other path would not explain.
pub fn codewhale_mcp_json_path(home: &Path) -> PathBuf {
    resolve_codewhale_mcp_path(home, std::env::var("DEEPSEEK_MCP_CONFIG").ok().as_deref())
}

/// Pure resolver behind [`codewhale_mcp_json_path`] — the env lookup is lifted
/// to the caller so tests can cover the override without mutating process env
/// (which races under the parallel test harness).
fn resolve_codewhale_mcp_path(home: &Path, explicit_override: Option<&str>) -> PathBuf {
    if let Some(explicit) = explicit_override {
        let explicit = explicit.trim();
        if !explicit.is_empty() {
            return PathBuf::from(explicit);
        }
    }
    let current = codewhale_dir(home).join("mcp.json");
    if current.exists() {
        return current;
    }
    let legacy = codewhale_legacy_dir(home).join("mcp.json");
    if legacy.exists() {
        return legacy;
    }
    current
}

/// `~/.codewhale` — CodeWhale's current config dir (`config.toml`, `mcp.json`).
pub fn codewhale_dir(home: &Path) -> PathBuf {
    home.join(".codewhale")
}

/// `~/.deepseek` — CodeWhale's pre-rename config dir, still honoured upstream
/// as a read-only fallback.
pub fn codewhale_legacy_dir(home: &Path) -> PathBuf {
    home.join(".deepseek")
}

#[cfg(test)]
mod codewhale_tests {
    use super::*;

    #[test]
    fn defaults_to_codewhale_dir_when_nothing_exists() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let home = tmp.path();
        assert_eq!(
            resolve_codewhale_mcp_path(home, None),
            home.join(".codewhale").join("mcp.json")
        );
    }

    #[test]
    fn prefers_existing_codewhale_config_over_legacy() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let home = tmp.path();
        for dir in [".codewhale", ".deepseek"] {
            std::fs::create_dir_all(home.join(dir)).expect("create dir");
            std::fs::write(home.join(dir).join("mcp.json"), "{}").expect("write cfg");
        }
        assert_eq!(
            resolve_codewhale_mcp_path(home, None),
            home.join(".codewhale").join("mcp.json"),
            "current path must win so we never write into the shadowed legacy file"
        );
    }

    #[test]
    fn falls_back_to_legacy_deepseek_config_when_only_it_exists() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let home = tmp.path();
        std::fs::create_dir_all(home.join(".deepseek")).expect("create dir");
        std::fs::write(home.join(".deepseek").join("mcp.json"), "{}").expect("write cfg");
        assert_eq!(
            resolve_codewhale_mcp_path(home, None),
            home.join(".deepseek").join("mcp.json")
        );
    }

    #[test]
    fn explicit_override_wins_and_blank_is_ignored() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let home = tmp.path();
        let explicit = home.join("custom").join("mcp.json");
        assert_eq!(
            resolve_codewhale_mcp_path(home, Some(&explicit.to_string_lossy())),
            explicit
        );
        assert_eq!(
            resolve_codewhale_mcp_path(home, Some("   ")),
            home.join(".codewhale").join("mcp.json")
        );
    }

    #[test]
    fn detect_path_accepts_either_config_dir() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let home = tmp.path();
        assert_eq!(codewhale_detect_path(home), home.join(".codewhale"));
        std::fs::create_dir_all(home.join(".deepseek")).expect("create dir");
        assert_eq!(codewhale_detect_path(home), home.join(".deepseek"));
        std::fs::create_dir_all(home.join(".codewhale")).expect("create dir");
        assert_eq!(codewhale_detect_path(home), home.join(".codewhale"));
    }
}

/// Detection marker for CodeWhale: either config dir counts as "installed".
pub fn codewhale_detect_path(home: &Path) -> PathBuf {
    let current = codewhale_dir(home);
    if current.exists() {
        return current;
    }
    let legacy = codewhale_legacy_dir(home);
    if legacy.exists() {
        return legacy;
    }
    current
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
        for path in [
            ".config/Code/User",
            ".config/Code - Insiders/User",
            ".vscode-server/data/User",
        ] {
            let full_path = home.join(path).join(TAIL);
            if full_path.exists() {
                return full_path;
            }
        }
        // Fall back to primary path if none exist
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

pub fn vibe_config_path(home: &Path) -> PathBuf {
    home.join(".vibe/config.toml")
}

pub fn detect_vibe_path(home: &Path) -> PathBuf {
    let vibe_dir = home.join(".vibe");
    if vibe_dir.exists() {
        return vibe_dir;
    }
    PathBuf::from("/nonexistent")
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

#[cfg(test)]
mod qodercli_tests {
    use super::*;

    #[test]
    fn qodercli_settings_path_uses_shared_qoder_settings_file() {
        let home = Path::new("/home/tester");
        assert_eq!(
            qodercli_settings_path(home),
            home.join(".qoder/settings.json")
        );
    }

    #[test]
    fn qodercli_settings_path_is_distinct_from_qoder_ide_mcp_path() {
        let home = Path::new("/home/tester");
        assert_ne!(qodercli_settings_path(home), qoder_mcp_path(home));
    }

    #[test]
    fn qodercli_settings_path_is_distinct_from_qoderwork_path() {
        let home = Path::new("/home/tester");
        assert_ne!(qodercli_settings_path(home), qoderwork_mcp_path(home));
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

#[cfg(test)]
mod omp_path_tests {
    use super::{omp_agent_dir_from, omp_agents_path, omp_mcp_path};
    use std::path::{Path, PathBuf};

    #[test]
    fn omp_defaults_to_the_native_agent_dir() {
        assert_eq!(
            omp_agent_dir_from(Path::new("/home/tester"), None),
            PathBuf::from("/home/tester/.omp/agent")
        );
    }

    #[test]
    fn pi_coding_agent_dir_is_a_full_override() {
        assert_eq!(
            omp_agent_dir_from(Path::new("/home/tester"), Some("/elsewhere/omp-agent")),
            PathBuf::from("/elsewhere/omp-agent")
        );
    }

    #[test]
    fn blank_override_falls_back_to_the_default_layout() {
        let home = Path::new("/home/tester");
        assert_eq!(
            omp_agent_dir_from(home, Some("   ")),
            omp_agent_dir_from(home, None)
        );
    }

    #[test]
    fn omp_config_and_rules_share_one_agent_dir() {
        // Env-robust: compares the two paths against each other, so it holds
        // with or without a PI_CODING_AGENT_DIR override in the environment.
        let home = Path::new("/home/tester");
        assert_eq!(omp_mcp_path(home).parent(), omp_agents_path(home).parent());
        assert_eq!(omp_mcp_path(home).file_name().unwrap(), "mcp.json");
        assert_eq!(omp_agents_path(home).file_name().unwrap(), "AGENTS.md");
    }
}
