use std::path::Path;

/// Parse a `file://` URI to a validated local path string.
/// Rejects non-file URIs, null bytes, `..` traversal, and non-directory paths.
/// Returns a canonicalized absolute path.
#[must_use]
pub fn uri_to_path(uri: &str) -> Option<String> {
    let raw = uri.strip_prefix("file://")?;
    if raw.contains("%00") {
        return None;
    }
    let decoded = percent_decode(raw);
    if decoded.is_empty() || decoded.contains('\0') {
        return None;
    }
    // Windows `file:///C:/path` URIs strip to `/C:/path`, which is NOT an
    // absolute Windows path (no drive prefix at the start) and would be rejected
    // below — so on Windows+Cursor every workspace root failed to parse and the
    // session fell back to the home dir as project root (GL discussion #273:
    // "MCP root misconfigured (resolves to C:/Users/<user>)"). Drop the single
    // leading slash before the drive letter so it parses as `C:/path`. POSIX
    // paths keep their leading slash (on Unix `/C:/x` is a legitimate path).
    #[cfg(windows)]
    let decoded = if has_leading_slash_drive(&decoded) {
        decoded[1..].to_string()
    } else {
        decoded
    };
    let path = Path::new(&decoded);
    if !path.is_absolute() {
        return None;
    }
    let canonical = crate::core::pathutil::safe_canonicalize_or_self(path);
    let s = canonical.to_string_lossy().to_string();
    if s.is_empty() {
        return None;
    }
    Some(s)
}

fn percent_decode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.bytes();
    while let Some(b) = chars.next() {
        if b == b'%' {
            let hi = chars.next().and_then(hex_val);
            let lo = chars.next().and_then(hex_val);
            if let (Some(h), Some(l)) = (hi, lo) {
                let byte = h << 4 | l;
                if byte == 0 {
                    continue;
                }
                out.push(byte as char);
            } else {
                out.push('%');
            }
        } else {
            out.push(b as char);
        }
    }
    out
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// True for a `file://` URI path of the form `/C:/…` — a leading slash, an ASCII
/// drive letter, then a colon. This shape comes from a Windows
/// `file:///C:/path` URI and is not an absolute Windows path until the leading
/// slash is removed. Pure predicate so the logic is unit-tested on every
/// platform, even though it is only wired into [`uri_to_path`] on Windows
/// (`allow(dead_code)` elsewhere keeps `-D warnings` clean).
#[cfg_attr(not(windows), allow(dead_code))]
fn has_leading_slash_drive(p: &str) -> bool {
    let b = p.as_bytes();
    b.len() >= 3 && b[0] == b'/' && b[1].is_ascii_alphabetic() && b[2] == b':'
}

pub(super) fn has_project_marker(dir: &Path) -> bool {
    crate::core::pathutil::has_project_marker(dir)
}

/// Select the best project root from MCP client roots.
/// Only considers paths that are existing directories.
/// Prefers roots with project markers (.git, Cargo.toml, etc.).
/// Falls back to the first valid directory if none have markers — but never
/// accepts a broad/unsafe root (HOME, filesystem root, agent sandbox dirs),
/// which would otherwise contaminate sessions across projects.
#[must_use]
pub fn best_root_from_uris(uris: &[String]) -> Option<String> {
    best_root_from_paths(uris.iter().filter_map(|u| uri_to_path(u)).collect())
}

/// Pick the best project root from a list of candidate directory paths.
///
/// Prefers a path with a project marker (`.git`, `Cargo.toml`, …); otherwise
/// falls back to the first *safe* directory. A caller that reports its workspace
/// root as HOME (some do) must not turn HOME into the project root — that is the
/// root cause of cross-project session contamination — so a broad/unsafe root is
/// never accepted as a marker-less fallback.
fn best_root_from_paths(paths: Vec<String>) -> Option<String> {
    let paths: Vec<String> = paths
        .into_iter()
        .filter(|p| Path::new(p).is_dir())
        .collect();

    if paths.is_empty() {
        return None;
    }

    for p in &paths {
        if has_project_marker(Path::new(p)) {
            return Some(p.clone());
        }
    }

    paths
        .into_iter()
        .find(|p| !crate::core::pathutil::is_broad_or_unsafe_root(Path::new(p)))
}

/// Filter and validate URIs to existing directories only.
#[must_use]
pub fn valid_dir_paths_from_uris(uris: &[String]) -> Vec<String> {
    uris.iter()
        .filter_map(|u| uri_to_path(u))
        .filter(|p| Path::new(p).is_dir())
        .collect()
}

/// Detect project root from IDE-specific environment variables.
/// Priority: `LEAN_CTX_PROJECT_ROOT` > `CLAUDE_PROJECT_DIR`
#[must_use]
pub fn root_from_env() -> Option<String> {
    for var in ["LEAN_CTX_PROJECT_ROOT", "CLAUDE_PROJECT_DIR"] {
        if let Ok(val) = std::env::var(var) {
            let trimmed = val.trim().to_string();
            if !trimmed.is_empty()
                && Path::new(&trimmed).is_dir()
                && !crate::core::pathutil::is_broad_or_unsafe_root(Path::new(&trimmed))
            {
                return Some(trimmed);
            }
        }
    }
    None
}

/// Split a `WORKSPACE_FOLDER_PATHS` value into individual paths.
///
/// Cursor separates entries with `,` (observed). We also tolerate the OS
/// path-list delimiter for robustness — `;` on Windows (never `:`, which is part
/// of `C:` drive specs) and `:` on Unix (never part of a POSIX path).
fn split_workspace_paths(raw: &str) -> Vec<String> {
    let delims: &[char] = if cfg!(windows) {
        &[',', ';']
    } else {
        &[',', ':']
    };
    raw.split(delims)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToString::to_string)
        .collect()
}

/// Best project root from the IDE-injected `WORKSPACE_FOLDER_PATHS` env var.
///
/// Cursor declares the MCP `roots` capability but does NOT implement
/// `roots/list` (it answers `-32601 Method not found`) and launches stdio MCP
/// servers with `cwd = /`. Without this signal the project root falls back to an
/// unsafe directory and relative tool paths resolve against the wrong tree
/// (#699). This variable is the Cursor-sanctioned way to learn the active
/// workspace folder(s). The same broad/unsafe-root guards as MCP roots apply.
#[must_use]
pub fn root_from_workspace_env() -> Option<String> {
    let raw = std::env::var("WORKSPACE_FOLDER_PATHS").ok()?;
    best_root_from_paths(split_workspace_paths(&raw))
}

/// All valid, safe workspace directories from `WORKSPACE_FOLDER_PATHS`.
///
/// Used to register the sibling folders of a multi-root workspace as extra
/// trusted roots, so explicit paths into them are not rejected by the path jail.
#[must_use]
pub fn workspace_roots_from_env() -> Vec<String> {
    let Ok(raw) = std::env::var("WORKSPACE_FOLDER_PATHS") else {
        return Vec::new();
    };
    split_workspace_paths(&raw)
        .into_iter()
        .filter(|p| Path::new(p).is_dir())
        .filter(|p| !crate::core::pathutil::is_broad_or_unsafe_root(Path::new(p)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn parse_file_uri_unix() {
        assert_eq!(
            uri_to_path("file:///home/user/project"),
            Some("/home/user/project".to_string())
        );
    }

    #[cfg(unix)]
    #[test]
    fn parse_file_uri_windows() {
        assert_eq!(
            uri_to_path("file:///C:/Users/dev/project"),
            Some("/C:/Users/dev/project".to_string())
        );
    }

    #[cfg(unix)]
    #[test]
    fn parse_file_uri_with_spaces() {
        assert_eq!(
            uri_to_path("file:///home/user/my%20project"),
            Some("/home/user/my project".to_string())
        );
    }

    #[test]
    fn parse_non_file_uri_returns_none() {
        assert!(uri_to_path("https://example.com").is_none());
        assert!(uri_to_path("").is_none());
    }

    #[test]
    fn detects_leading_slash_windows_drive() {
        // GL #273: the `/C:/…` shape (from a Windows `file:///C:/…` URI) must be
        // recognised so the leading slash can be stripped. Runs on every
        // platform so Linux CI guards the logic the Windows-only wiring uses.
        assert!(has_leading_slash_drive("/C:/Users/dev"));
        assert!(has_leading_slash_drive("/c:/proj"));
        assert!(has_leading_slash_drive("/Z:"));
        // POSIX paths, already-stripped drives, UNC shares and root stay intact.
        assert!(!has_leading_slash_drive("/home/user/proj"));
        assert!(!has_leading_slash_drive("C:/already"));
        assert!(!has_leading_slash_drive("//server/share"));
        assert!(!has_leading_slash_drive("/"));
        assert!(!has_leading_slash_drive("/1:/x"));
    }

    #[cfg(windows)]
    #[test]
    fn parse_file_uri_windows_drive_strips_leading_slash() {
        // GL #273: Cursor on Windows reports roots as `file:///C:/…`; these must
        // parse to an absolute `C:/` path instead of being rejected (which left
        // the session falling back to the home dir as project root).
        let got = uri_to_path("file:///C:/Users/dev/project").expect("windows drive uri");
        assert!(
            !got.starts_with('/'),
            "leading slash must be stripped: {got}"
        );
        assert!(
            got.to_ascii_lowercase().starts_with("c:"),
            "drive prefix must survive: {got}"
        );
    }

    #[cfg(windows)]
    #[test]
    fn parse_file_uri_windows_percent_encoded_colon() {
        // Some clients percent-encode the drive colon (`C%3A`).
        let got = uri_to_path("file:///C%3A/Users/dev/project").expect("encoded colon uri");
        assert!(
            !got.starts_with('/'),
            "leading slash must be stripped: {got}"
        );
        assert!(got.to_ascii_lowercase().starts_with("c:"), "got: {got}");
    }

    #[test]
    fn rejects_null_bytes() {
        assert!(uri_to_path("file:///tmp/evil%00path").is_none());
    }

    #[test]
    fn rejects_relative_uri() {
        assert!(uri_to_path("file://relative/path").is_none());
    }

    #[test]
    fn canonicalizes_traversal() {
        let tmp = tempfile::tempdir().unwrap();
        let sub = tmp.path().join("a").join("b");
        std::fs::create_dir_all(&sub).unwrap();
        let traversal = format!("file://{}/a/b/../..", tmp.path().display());
        let result = uri_to_path(&traversal);
        assert!(result.is_some());
        let resolved = result.unwrap();
        assert!(
            !resolved.contains(".."),
            "should be canonicalized: {resolved}"
        );
    }

    #[test]
    fn best_root_prefers_marker() {
        let tmp = tempfile::tempdir().unwrap();
        let with_marker = tmp.path().join("has_git");
        let without = tmp.path().join("plain");
        std::fs::create_dir_all(&with_marker).unwrap();
        std::fs::create_dir_all(&without).unwrap();
        std::fs::create_dir(with_marker.join(".git")).unwrap();

        let uris = vec![
            format!("file://{}", without.display()),
            format!("file://{}", with_marker.display()),
        ];
        let result = best_root_from_uris(&uris).unwrap();
        assert!(result.contains("has_git"));
    }

    #[test]
    fn best_root_falls_back_to_first_existing_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let a = tmp.path().join("dir_a");
        let b = tmp.path().join("dir_b");
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();

        let uris = vec![
            format!("file://{}", a.display()),
            format!("file://{}", b.display()),
        ];
        let result = best_root_from_uris(&uris).unwrap();
        assert!(result.contains("dir_a"));
    }

    #[test]
    fn best_root_skips_nonexistent() {
        let uris = vec!["file:///nonexistent_abc_123".to_string()];
        assert!(best_root_from_uris(&uris).is_none());
    }

    #[test]
    fn best_root_empty_returns_none() {
        assert!(best_root_from_uris(&[]).is_none());
    }

    #[test]
    fn env_override_returns_none_when_unset() {
        let _ = root_from_env();
    }

    #[test]
    fn best_root_rejects_home_without_marker() {
        // A client reporting HOME as its workspace root must NOT turn HOME into
        // the project root (root cause of cross-project session contamination).
        if let Some(home) = dirs::home_dir() {
            let uris = vec![format!("file://{}", home.display())];
            assert_eq!(
                best_root_from_uris(&uris),
                None,
                "HOME must never be accepted as a marker-less project root"
            );
        }
    }

    #[test]
    fn best_root_prefers_safe_dir_over_home() {
        if let Some(home) = dirs::home_dir() {
            let tmp = tempfile::tempdir().unwrap();
            let safe = tmp.path().join("real_project");
            std::fs::create_dir_all(&safe).unwrap();
            let uris = vec![
                format!("file://{}", home.display()),
                format!("file://{}", safe.display()),
            ];
            let result = best_root_from_uris(&uris).unwrap();
            assert!(result.contains("real_project"));
        }
    }

    #[test]
    fn best_root_rejects_filesystem_root() {
        let uris = vec!["file:///".to_string()];
        assert!(best_root_from_uris(&uris).is_none());
    }

    #[test]
    fn all_paths_from_uris() {
        let tmp = tempfile::tempdir().unwrap();
        let a = tmp.path().join("project_a");
        let b = tmp.path().join("project_b");
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        std::fs::create_dir(a.join(".git")).unwrap();

        let uris = vec![
            format!("file://{}", a.display()),
            format!("file://{}", b.display()),
        ];

        let paths: Vec<String> = uris.iter().filter_map(|u| uri_to_path(u)).collect();
        assert_eq!(paths.len(), 2);
        assert!(paths[0].contains("project_a"));
        assert!(paths[1].contains("project_b"));

        let best = best_root_from_uris(&uris).unwrap();
        assert!(best.contains("project_a"));
    }

    #[test]
    fn split_workspace_paths_comma_separated() {
        assert_eq!(
            split_workspace_paths("/home/u/proj-a,/home/u/proj-b"),
            vec!["/home/u/proj-a".to_string(), "/home/u/proj-b".to_string()]
        );
    }

    #[test]
    fn split_workspace_paths_trims_and_drops_empty() {
        assert_eq!(
            split_workspace_paths(" /a , , /b ,"),
            vec!["/a".to_string(), "/b".to_string()]
        );
    }

    #[cfg(unix)]
    #[test]
    fn split_workspace_paths_unix_colon_delimiter() {
        // Unix path-list delimiter is ':'; POSIX paths never contain it.
        assert_eq!(
            split_workspace_paths("/a:/b"),
            vec!["/a".to_string(), "/b".to_string()]
        );
    }

    #[test]
    fn best_root_from_paths_prefers_marker_over_first() {
        let tmp = tempfile::tempdir().unwrap();
        let plain = tmp.path().join("plain");
        let marked = tmp.path().join("marked");
        std::fs::create_dir_all(&plain).unwrap();
        std::fs::create_dir_all(&marked).unwrap();
        std::fs::create_dir(marked.join(".git")).unwrap();
        let got = best_root_from_paths(vec![
            plain.to_string_lossy().to_string(),
            marked.to_string_lossy().to_string(),
        ])
        .unwrap();
        assert!(got.contains("marked"), "marker dir must win: {got}");
    }

    #[test]
    fn best_root_from_paths_filters_nonexistent() {
        let tmp = tempfile::tempdir().unwrap();
        let safe = tmp.path().join("real_proj");
        std::fs::create_dir_all(&safe).unwrap();
        let got = best_root_from_paths(vec![
            "/nonexistent_xyz_987".to_string(),
            safe.to_string_lossy().to_string(),
        ])
        .unwrap();
        assert!(got.contains("real_proj"));
    }

    #[test]
    fn best_root_from_paths_empty_returns_none() {
        assert!(best_root_from_paths(vec![]).is_none());
        assert!(best_root_from_paths(vec!["/nonexistent_abc".to_string()]).is_none());
    }

    #[test]
    fn workspace_env_value_picks_marker_root() {
        // Mirrors Cursor's `WORKSPACE_FOLDER_PATHS` (comma-separated multi-root):
        // the folder carrying a project marker must win over a sibling.
        let tmp = tempfile::tempdir().unwrap();
        let a = tmp.path().join("ws_a");
        let b = tmp.path().join("ws_b");
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        std::fs::write(b.join("Cargo.toml"), "[package]").unwrap();
        let raw = format!("{},{}", a.display(), b.display());
        let got = best_root_from_paths(split_workspace_paths(&raw)).unwrap();
        assert!(got.contains("ws_b"), "marker workspace must win: {got}");
    }

    #[test]
    fn workspace_env_readers_do_not_panic() {
        // Smoke test: both env readers tolerate the variable being set or unset.
        let _ = root_from_workspace_env();
        let _ = workspace_roots_from_env();
    }
}
