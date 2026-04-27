use std::path::{Path, PathBuf};

/// Canonicalize a path and strip the Windows verbatim/extended-length prefix (`\\?\`)
/// that `std::fs::canonicalize` adds on Windows. This prefix breaks many tools and
/// string-based path comparisons.
///
/// On non-Windows platforms this is equivalent to `std::fs::canonicalize`.
pub fn safe_canonicalize(path: &Path) -> std::io::Result<PathBuf> {
    let canon = std::fs::canonicalize(path)?;
    Ok(strip_verbatim(canon))
}

/// Like `safe_canonicalize` but returns the original path on failure.
pub fn safe_canonicalize_or_self(path: &Path) -> PathBuf {
    safe_canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// Remove the `\\?\` / `//?/` verbatim prefix from a `PathBuf`.
/// Handles both regular verbatim (`\\?\C:\...`) and UNC verbatim (`\\?\UNC\...`).
pub fn strip_verbatim(path: PathBuf) -> PathBuf {
    let s = path.to_string_lossy();
    if let Some(stripped) = strip_verbatim_str(&s) {
        PathBuf::from(stripped)
    } else {
        path
    }
}

/// Remove the `\\?\` / `//?/` verbatim prefix from a path string.
/// Returns `Some(cleaned)` if a prefix was found, `None` otherwise.
pub fn strip_verbatim_str(path: &str) -> Option<String> {
    let normalized = path.replace('\\', "/");

    if let Some(rest) = normalized.strip_prefix("//?/UNC/") {
        Some(format!("//{rest}"))
    } else {
        normalized
            .strip_prefix("//?/")
            .map(std::string::ToString::to_string)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_regular_verbatim() {
        let p = PathBuf::from(r"\\?\C:\Users\dev\project");
        let result = strip_verbatim(p);
        assert_eq!(result, PathBuf::from("C:/Users/dev/project"));
    }

    #[test]
    fn strip_unc_verbatim() {
        let p = PathBuf::from(r"\\?\UNC\server\share\dir");
        let result = strip_verbatim(p);
        assert_eq!(result, PathBuf::from("//server/share/dir"));
    }

    #[test]
    fn no_prefix_unchanged() {
        let p = PathBuf::from("/home/user/project");
        let result = strip_verbatim(p.clone());
        assert_eq!(result, p);
    }

    #[test]
    fn windows_drive_unchanged() {
        let p = PathBuf::from("C:/Users/dev");
        let result = strip_verbatim(p.clone());
        assert_eq!(result, p);
    }

    #[test]
    fn strip_str_regular() {
        assert_eq!(
            strip_verbatim_str(r"\\?\E:\code\lean-ctx"),
            Some("E:/code/lean-ctx".to_string())
        );
    }

    #[test]
    fn strip_str_unc() {
        assert_eq!(
            strip_verbatim_str(r"\\?\UNC\myserver\data"),
            Some("//myserver/data".to_string())
        );
    }

    #[test]
    fn strip_str_forward_slash_variant() {
        assert_eq!(
            strip_verbatim_str("//?/C:/Users/dev"),
            Some("C:/Users/dev".to_string())
        );
    }

    #[test]
    fn strip_str_no_prefix() {
        assert_eq!(strip_verbatim_str("/home/user"), None);
    }

    #[test]
    fn safe_canonicalize_or_self_nonexistent() {
        let p = Path::new("/this/path/should/not/exist/xyzzy");
        let result = safe_canonicalize_or_self(p);
        assert_eq!(result, p.to_path_buf());
    }
}
