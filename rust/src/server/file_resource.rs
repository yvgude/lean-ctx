//! Serve `file://` URIs through `resources/read` by reading the local file.
//!
//! Agents (especially Claude Code) sometimes call `readMcpResource` with a
//! `file://` URI instead of `ctx_read`.  Rather than returning a confusing
//! "Unknown resource" error, we resolve the path, validate it against the
//! session's PathJail roots, and return the file content as a text resource.
//!
//! GH #1418

use rmcp::model::ResourceContents;
use std::path::Path;

const MAX_FILE_BYTES: u64 = 2 * 1024 * 1024; // 2 MiB

/// Why a `file://` resource request could not be served.
#[derive(Debug)]
pub(super) enum FileResourceError {
    /// URI is not a file path — caller should fall through to the default error.
    NotAFilePath,
    /// Path resolved but the file does not exist.
    NotFound(String),
    /// File exceeds the size guard.
    TooLarge(String, u64),
    /// File is not valid UTF-8.
    NotUtf8(String),
    /// I/O error while reading.
    Io(String, std::io::Error),
    /// Path rejected by PathJail (outside project root / extra roots).
    OutsideJail(String),
}

impl std::fmt::Display for FileResourceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotAFilePath => write!(f, "Not a file path"),
            Self::NotFound(p) => write!(f, "File not found: {p}"),
            Self::TooLarge(p, sz) => write!(
                f,
                "File too large for resource read ({sz} bytes, limit {MAX_FILE_BYTES}): {p}",
            ),
            Self::NotUtf8(p) => write!(
                f,
                "File is not valid UTF-8 (binary?): {p}. Use ctx_read for binary-safe reads."
            ),
            Self::Io(p, e) => write!(f, "I/O error reading {p}: {e}"),
            Self::OutsideJail(p) => write!(
                f,
                "Path outside project root: {p}. \
                 Add the parent directory to extra_roots in config.toml if needed."
            ),
        }
    }
}

/// Try to serve a `file://` URI (or raw absolute path) as a text resource.
///
/// Returns `Err(NotAFilePath)` when the URI is not a recognisable file path,
/// signalling the caller to fall through to the default resource-not-found path.
pub(super) fn read_file_resource(
    uri: &str,
    project_root: Option<&str>,
    extra_roots: &[String],
) -> Result<Vec<ResourceContents>, FileResourceError> {
    let path_str = resolve_path(uri)?;
    let path = Path::new(&path_str);

    if let Some(root) = project_root {
        validate_jail(path, root, extra_roots)?;
    }

    if !path.is_file() {
        return Err(FileResourceError::NotFound(path_str));
    }

    let meta = std::fs::metadata(path).map_err(|e| FileResourceError::Io(path_str.clone(), e))?;
    if meta.len() > MAX_FILE_BYTES {
        return Err(FileResourceError::TooLarge(path_str, meta.len()));
    }

    let content = std::fs::read_to_string(path).map_err(|e| {
        if e.kind() == std::io::ErrorKind::InvalidData {
            FileResourceError::NotUtf8(path_str.clone())
        } else {
            FileResourceError::Io(path_str.clone(), e)
        }
    })?;

    Ok(vec![super::resources::text_resource(&content, uri)])
}

/// Parse `file://` URIs and raw absolute paths to a local path string.
fn resolve_path(uri: &str) -> Result<String, FileResourceError> {
    if uri.starts_with("file://") {
        super::roots::uri_to_path(uri).ok_or(FileResourceError::NotAFilePath)
    } else if is_absolute_path(uri) {
        Ok(uri.to_string())
    } else {
        Err(FileResourceError::NotAFilePath)
    }
}

fn is_absolute_path(s: &str) -> bool {
    s.starts_with('/')
        || (cfg!(windows)
            && s.len() > 2
            && s.as_bytes()[0].is_ascii_alphabetic()
            && s.as_bytes()[1] == b':')
}

fn validate_jail(
    path: &Path,
    project_root: &str,
    extra_roots: &[String],
) -> Result<(), FileResourceError> {
    // Intentionally NOT using jail_path_with_roots here: the full PathJail
    // widens access via global config (allow_paths, state_dir, IDE dirs,
    // env vars). For resources/read (a fallback for agents that use the
    // wrong API), we enforce strict containment in session-explicit roots
    // only — no implicit widening.
    let canon = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let root_canon = Path::new(project_root)
        .canonicalize()
        .unwrap_or_else(|_| std::path::PathBuf::from(project_root));

    if canon.starts_with(&root_canon) {
        return Ok(());
    }

    for extra in extra_roots {
        if let Ok(extra_canon) = Path::new(extra.as_str()).canonicalize() {
            if canon.starts_with(&extra_canon) {
                return Ok(());
            }
        }
    }

    Err(FileResourceError::OutsideJail(
        path.to_string_lossy().to_string(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn path_to_file_uri(p: &std::path::Path) -> String {
        let s = p.to_string_lossy().replace('\\', "/");
        if s.starts_with('/') {
            format!("file://{s}")
        } else {
            format!("file:///{s}")
        }
    }

    #[cfg(unix)]
    #[test]
    fn resolve_file_uri() {
        let path = resolve_path("file:///tmp/test.txt").unwrap();
        assert_eq!(path, "/tmp/test.txt");
    }

    #[cfg(unix)]
    #[test]
    fn resolve_raw_absolute_path() {
        let path = resolve_path("/home/user/file.rs").unwrap();
        assert_eq!(path, "/home/user/file.rs");
    }

    #[test]
    fn resolve_relative_path_returns_not_a_file() {
        assert!(matches!(
            resolve_path("relative/file.rs"),
            Err(FileResourceError::NotAFilePath)
        ));
    }

    #[test]
    fn resolve_lean_ctx_uri_returns_not_a_file() {
        assert!(matches!(
            resolve_path("lean-ctx://context/summary"),
            Err(FileResourceError::NotAFilePath)
        ));
    }

    #[test]
    fn read_nonexistent_file() {
        let dir = tempfile::tempdir().unwrap();
        let nonexistent = dir.path().join("does_not_exist.txt");
        let uri = path_to_file_uri(&nonexistent);
        let result = read_file_resource(&uri, None, &[]);
        assert!(matches!(result, Err(FileResourceError::NotFound(_))));
    }

    #[test]
    fn read_existing_file() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), "hello world").unwrap();
        let uri = path_to_file_uri(tmp.path());
        let result = read_file_resource(&uri, None, &[]).unwrap();
        assert_eq!(result.len(), 1);
    }

    #[test]
    fn read_file_outside_jail() {
        let jail = tempfile::tempdir().unwrap();
        let outside = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(outside.path(), "secret").unwrap();
        let uri = path_to_file_uri(outside.path());
        let result = read_file_resource(&uri, Some(&jail.path().to_string_lossy()), &[]);
        assert!(matches!(result, Err(FileResourceError::OutsideJail(_))));
    }

    #[test]
    fn read_file_inside_jail() {
        let jail = tempfile::tempdir().unwrap();
        let file = jail.path().join("test.txt");
        std::fs::write(&file, "allowed").unwrap();
        let uri = path_to_file_uri(&file);
        let result = read_file_resource(&uri, Some(&jail.path().to_string_lossy()), &[]);
        assert!(result.is_ok());
    }

    #[test]
    fn non_file_uri_is_not_a_file_path() {
        assert!(matches!(
            read_file_resource("https://example.com", None, &[]),
            Err(FileResourceError::NotAFilePath)
        ));
    }
}
