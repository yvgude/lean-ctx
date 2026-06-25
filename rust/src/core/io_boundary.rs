use std::path::{Path, PathBuf};

use crate::core::{events, pathjail, roles, secret_detection};

/// Reads a file without following symlinks (TOCTOU protection).
/// Falls back to regular read on non-Unix platforms.
#[cfg(unix)]
pub fn read_file_nofollow(path: &str) -> Result<String, std::io::Error> {
    use std::os::unix::fs::OpenOptionsExt;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path);
    match file {
        Ok(mut f) => {
            use std::io::Read;
            let mut buf = Vec::new();
            f.read_to_end(&mut buf)?;
            Ok(String::from_utf8_lossy(&buf).into_owned())
        }
        Err(e) if e.raw_os_error() == Some(libc::ELOOP) => Err(std::io::Error::other(format!(
            "Symlink detected at {path} — refusing to follow (TOCTOU protection)"
        ))),
        Err(e) => Err(e),
    }
}

/// Windows parity (GL#442): no O_NOFOLLOW exists, so lstat first and refuse
/// symlinks *and* NTFS junctions/reparse points before opening. Small TOCTOU
/// window remains between the check and the open (documented in SECURITY.md).
#[cfg(not(unix))]
pub fn read_file_nofollow(path: &str) -> Result<String, std::io::Error> {
    if let Ok(meta) = std::fs::symlink_metadata(path) {
        if crate::core::pathutil::is_symlink_or_reparse(&meta) {
            return Err(std::io::Error::other(format!(
                "Symlink detected at {path} — refusing to follow (TOCTOU protection)"
            )));
        }
    }
    std::fs::read_to_string(path)
}

/// Reads a file as lossy UTF-8, rejecting binary files.
/// Uses `O_NOFOLLOW` on Unix to prevent TOCTOU symlink attacks.
pub fn read_file_lossy(path: &str) -> Result<String, std::io::Error> {
    if crate::core::binary_detect::is_binary_file(path) {
        let msg = crate::core::binary_detect::binary_file_message(path);
        return Err(std::io::Error::other(msg));
    }
    read_file_nofollow(path)
}

/// Result of a file read with secret scanning applied.
pub struct ScannedRead {
    pub content: String,
    pub secret_matches: Vec<secret_detection::SecretMatch>,
    pub was_redacted: bool,
}

/// Reads a file and applies secret detection/redaction per config.
///
/// - `enabled=true, redact=false`: returns original content + warnings in `secret_matches`
/// - `enabled=true, redact=true`: returns redacted content + `was_redacted=true`
/// - `enabled=false`: returns original content, no scanning
pub fn read_file_scanned(path: &str) -> Result<ScannedRead, std::io::Error> {
    let raw = read_file_lossy(path)?;
    let cfg = crate::core::config::Config::load();
    let sd = &cfg.secret_detection;

    if !sd.enabled {
        return Ok(ScannedRead {
            content: raw,
            secret_matches: Vec::new(),
            was_redacted: false,
        });
    }

    let (content, matches) = secret_detection::scan_and_redact(&raw, sd);

    if !matches.is_empty() {
        let role_name = roles::active_role_name();
        let names: Vec<&str> = matches.iter().map(|m| m.pattern_name).collect();
        let mut unique: Vec<&str> = names;
        unique.sort_unstable();
        unique.dedup();
        let msg = format!(
            "[SECRET DETECTION] {} secret(s) found in {}: {}",
            matches.len(),
            path,
            unique.join(", ")
        );
        events::emit_policy_violation(&role_name, "read_file", &msg);
        tracing::warn!("{msg}");
    }

    let was_redacted = sd.redact && !matches.is_empty();
    Ok(ScannedRead {
        content,
        secret_matches: matches,
        was_redacted,
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BoundaryMode {
    Warn,
    Enforce,
}

impl BoundaryMode {
    fn parse(s: &str) -> Self {
        match s.trim().to_lowercase().as_str() {
            "enforce" | "strict" => Self::Enforce,
            _ => Self::Warn,
        }
    }
}

#[must_use]
pub fn boundary_mode_effective(role: &roles::Role) -> BoundaryMode {
    if let Ok(v) = std::env::var("LEAN_CTX_IO_BOUNDARY_MODE")
        && !v.trim().is_empty()
    {
        return BoundaryMode::parse(&v);
    }
    BoundaryMode::parse(&role.io.boundary_mode)
}

#[must_use]
pub fn is_secret_like(path: &Path) -> Option<&'static str> {
    let file = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
    let lower = file.to_lowercase();

    // Directory-level sensitive roots
    for comp in path.components() {
        if let std::path::Component::Normal(s) = comp {
            let c = s.to_string_lossy().to_lowercase();
            if c == ".ssh" {
                return Some(".ssh directory");
            }
            if c == ".aws" {
                return Some(".aws directory");
            }
            if c == ".gnupg" {
                return Some(".gnupg directory");
            }
        }
    }

    // Common secret-like files (deny-by-default unless explicitly allowed).
    if lower == ".env" {
        return Some(".env file");
    }
    if lower.starts_with(".env.") {
        let allow_suffixes = [".example", ".sample", ".template", ".dist", ".defaults"];
        if allow_suffixes.iter().any(|s| lower.ends_with(s)) {
            return None;
        }
        return Some(".env.* file");
    }

    if matches!(
        lower.as_str(),
        "id_rsa"
            | "id_ed25519"
            | "id_ecdsa"
            | "id_dsa"
            | "authorized_keys"
            | "known_hosts"
            | ".npmrc"
            | ".netrc"
            | ".pypirc"
            | ".dockerconfigjson"
            | "credentials.json"
            | "secrets.json"
            | "secrets.yaml"
            | "secrets.yml"
            | "keystore.jks"
            | "truststore.jks"
            | ".htpasswd"
            | "shadow"
            | "master.key"
    ) {
        return Some("credential file");
    }

    if lower.starts_with("service-account") {
        let p = std::path::Path::new(&lower);
        if p.extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("json") || ext.eq_ignore_ascii_case("key"))
        {
            return Some("service account key");
        }
    }

    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
    let secret_exts = ["pem", "key", "p12", "pfx", "kdbx"];
    if secret_exts.iter().any(|e| ext.eq_ignore_ascii_case(e)) {
        return Some("secret key material");
    }

    // AWS credentials file (often inside .aws/)
    if lower == "credentials" && path.to_string_lossy().to_lowercase().contains("/.aws/") {
        return Some("aws credentials");
    }

    None
}

pub fn check_secret_path_for_tool(tool: &str, path: &Path) -> Result<Option<String>, String> {
    let role_name = roles::active_role_name();
    let role = roles::active_role();
    let mode = boundary_mode_effective(&role);

    let Some(reason) = is_secret_like(path) else {
        return Ok(None);
    };

    if role.io.allow_secret_paths {
        return Ok(None);
    }

    let msg = format!(
        "[I/O BOUNDARY] Secret-like path detected ({reason}): {}.\n\
Role: {role_name}. To allow: switch role to 'admin' or set io.allow_secret_paths=true in the active role.",
        path.display()
    );
    events::emit_policy_violation(&role_name, tool, &msg);

    match mode {
        BoundaryMode::Enforce => Err(format!("ERROR: {msg}")),
        BoundaryMode::Warn => {
            if crate::core::protocol::meta_visible() {
                Ok(Some(format!("[BOUNDARY WARNING] {msg}")))
            } else {
                Ok(None)
            }
        }
    }
}

pub fn jail_and_check_path(
    tool: &str,
    candidate: &Path,
    jail_root: &Path,
) -> Result<(PathBuf, Option<String>), String> {
    let role_name = roles::active_role_name();
    let jailed = pathjail::jail_path(candidate, jail_root).map_err(|e| {
        // Only a real jail escape is a security event. A path that simply doesn't exist
        // (stale graph entry, removed file) is benign — emitting a policy violation for it
        // spams the event feed and mislabels missing files as denials.
        if !e.starts_with("path does not exist") {
            let msg = format!("pathjail denied: {} ({e})", candidate.display());
            events::emit_policy_violation(&role_name, tool, &msg);
        }
        e
    })?;
    let warning = check_secret_path_for_tool(tool, &jailed)?;
    Ok((jailed, warning))
}

pub fn ensure_ignore_gitignore_allowed(tool: &str) -> Result<(), String> {
    let role_name = roles::active_role_name();
    let role = roles::active_role();
    if role.io.allow_ignore_gitignore {
        return Ok(());
    }
    let msg = format!(
        "[I/O BOUNDARY] ignore_gitignore requires explicit policy.\n\
Role '{role_name}' does not allow scanning .gitignore'd paths. \
An agent cannot escalate to a privileged role at runtime, so configure this where lean-ctx starts:\n\
- set LEAN_CTX_ROLE=admin, or\n\
- add `io.allow_ignore_gitignore = true` to a role file (~/.lean-ctx/roles/<name>.toml), then select it via LEAN_CTX_ROLE.\n\
Docs: https://leanctx.com/docs/security/#ignore-gitignore"
    );
    events::emit_policy_violation(&role_name, tool, &msg);
    Err(format!("ERROR: {msg}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn nofollow_rejects_symlink() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real.txt");
        std::fs::write(&real, "secret").unwrap();
        let link = dir.path().join("link.txt");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let result = read_file_nofollow(&link.to_string_lossy());
        assert!(result.is_err());
    }

    #[test]
    fn nofollow_reads_regular_file() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("regular.txt");
        std::fs::write(&file, "hello").unwrap();
        let content = read_file_nofollow(&file.to_string_lossy()).unwrap();
        assert_eq!(content, "hello");
    }

    #[test]
    fn env_is_secret_like() {
        assert_eq!(is_secret_like(Path::new(".env")), Some(".env file"));
        assert_eq!(is_secret_like(Path::new(".env.local")), Some(".env.* file"));
        assert_eq!(is_secret_like(Path::new(".env.example")), None);
    }

    #[test]
    fn key_is_secret_like() {
        assert_eq!(
            is_secret_like(Path::new("key.pem")),
            Some("secret key material")
        );
        assert_eq!(
            is_secret_like(Path::new("cert.KEY")),
            Some("secret key material")
        );
    }

    #[test]
    fn credentials_json_is_secret_like() {
        assert_eq!(
            is_secret_like(Path::new("credentials.json")),
            Some("credential file")
        );
        assert_eq!(
            is_secret_like(Path::new("secrets.yaml")),
            Some("credential file")
        );
    }

    #[test]
    fn service_account_is_secret_like() {
        assert_eq!(
            is_secret_like(Path::new("service-account.json")),
            Some("service account key")
        );
        assert_eq!(
            is_secret_like(Path::new("service-account-prod.key")),
            Some("service account key")
        );
    }

    #[test]
    fn htpasswd_and_shadow_are_secret_like() {
        assert_eq!(
            is_secret_like(Path::new(".htpasswd")),
            Some("credential file")
        );
        assert_eq!(is_secret_like(Path::new("shadow")), Some("credential file"));
    }
}
