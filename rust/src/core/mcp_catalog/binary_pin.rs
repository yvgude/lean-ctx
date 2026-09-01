//! Fail-closed SHA-256 pinning for gateway `stdio` servers.
//!
//! `[[gateway.servers]] binary_sha256` — and the `sha256` field of an addon's
//! `[mcp]` table, which translates into it — is a promise that the executable
//! about to be spawned is the one the user vouched for. Until 3.10.1 the value
//! was parsed, stored, shown, and then dropped at the spawn point, so a pin
//! bought nothing but the appearance of one. `docs/contracts/addon-manifest-v1.md`
//! described the enforcement that this module now actually performs.
//!
//! Two details that matter more than the hashing itself:
//!
//! - **Resolve against the PATH the child will see.** The server's own `env`
//!   may override `PATH`; hashing whatever our process would find while the
//!   child runs something else would verify the wrong file.
//! - **Spawn the resolved path, not the bare name.** Once a pin is set the
//!   caller launches the exact file that was hashed, so name resolution cannot
//!   land on a different binary between the check and the spawn. This does not
//!   make the pair atomic — nothing short of holding the file open would — but
//!   it removes the ambiguity a pin exists to remove.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::core::pathjail;

/// Resolve `command` the way the spawned child would, honouring a `PATH`
/// override in the server's own environment.
fn resolve(command: &str, env: &BTreeMap<String, String>) -> Result<PathBuf, String> {
    let as_path = Path::new(command);
    if as_path.is_absolute() || as_path.components().count() > 1 {
        return as_path
            .is_file()
            .then(|| pathjail::canonicalize_or_self(as_path))
            .ok_or_else(|| format!("pinned binary does not exist: {command}"));
    }

    let path_var = match env.get("PATH") {
        Some(overridden) => std::ffi::OsString::from(overridden),
        None => std::env::var_os("PATH").unwrap_or_default(),
    };
    for directory in std::env::split_paths(&path_var) {
        let candidate = directory.join(command);
        if candidate.is_file() {
            return Ok(pathjail::canonicalize_or_self(&candidate));
        }
    }
    Err(format!("pinned binary `{command}` was not found on PATH"))
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    bytes.iter().fold(String::with_capacity(64), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

/// Verify the pin, returning the path to spawn.
///
/// `Ok(None)` means "unpinned, spawn the command as written" — an empty pin is
/// the documented default and must stay a no-op, or every existing gateway
/// entry would break. Any other outcome, including a binary that cannot be
/// found or read, is an error: a pin that cannot be checked has failed.
pub(crate) async fn verify(
    command: &str,
    env: &BTreeMap<String, String>,
    pin: &str,
) -> Result<Option<PathBuf>, String> {
    let expected = pin.trim().trim_start_matches("0x");
    if expected.is_empty() {
        return Ok(None);
    }

    let path = resolve(command, env)?;
    let bytes = tokio::fs::read(&path)
        .await
        .map_err(|e| format!("cannot read pinned binary {}: {e}", path.display()))?;
    let actual = hex(&Sha256::digest(&bytes));

    if actual.eq_ignore_ascii_case(expected) {
        Ok(Some(path))
    } else {
        Err(format!(
            "binary_sha256 mismatch for `{command}` — refusing to spawn.\n  \
             expected {expected}\n  actual   {actual}\n  \
             path     {}\n\
             The pinned executable is not the one on disk. Re-pin deliberately \
             (shasum -a 256) only if you know why it changed.",
            path.display()
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_exe(dir: &Path, name: &str, body: &[u8]) -> PathBuf {
        let p = dir.join(name);
        std::fs::write(&p, body).unwrap();
        p
    }

    #[tokio::test]
    async fn an_empty_pin_is_a_no_op() {
        let env = BTreeMap::new();
        assert_eq!(verify("anything", &env, "").await.unwrap(), None);
        assert_eq!(verify("anything", &env, "   ").await.unwrap(), None);
    }

    #[tokio::test]
    async fn a_matching_pin_returns_the_resolved_path() {
        let tmp = tempfile::tempdir().unwrap();
        let exe = write_exe(tmp.path(), "srv", b"payload");
        let digest = hex(&Sha256::digest(b"payload"));

        let got = verify(exe.to_str().unwrap(), &BTreeMap::new(), &digest)
            .await
            .expect("pin matches")
            .expect("pinned");
        assert_eq!(got, pathjail::canonicalize_or_self(&exe));

        // The value `shasum -a 256` prints, and an 0x-prefixed upper-case
        // variant, are the same pin.
        let loud = format!("0x{}", digest.to_uppercase());
        assert!(
            verify(exe.to_str().unwrap(), &BTreeMap::new(), &loud)
                .await
                .is_ok()
        );
    }

    #[tokio::test]
    async fn a_mismatch_refuses_and_names_both_digests() {
        let tmp = tempfile::tempdir().unwrap();
        let exe = write_exe(tmp.path(), "srv", b"actual bytes");
        let wrong = "0".repeat(64);

        let err = verify(exe.to_str().unwrap(), &BTreeMap::new(), &wrong)
            .await
            .expect_err("must refuse");
        assert!(err.contains("mismatch"), "{err}");
        assert!(err.contains(&wrong), "the expected digest: {err}");
        assert!(
            err.contains(&hex(&Sha256::digest(b"actual bytes"))),
            "the actual digest: {err}"
        );
    }

    /// A pin that cannot be checked has failed — silently spawning would be the
    /// one outcome a pin exists to prevent.
    #[tokio::test]
    async fn a_missing_binary_is_an_error_not_a_skip() {
        let err = verify(
            "/nonexistent/lean-ctx-test-binary",
            &BTreeMap::new(),
            &"a".repeat(64),
        )
        .await
        .expect_err("must refuse");
        assert!(err.contains("does not exist"), "{err}");
    }

    /// The child resolves `PATH` from its own environment, so the check must
    /// too — otherwise we hash one file and spawn another.
    #[tokio::test]
    async fn resolution_honours_a_path_override_in_the_server_env() {
        let tmp = tempfile::tempdir().unwrap();
        let exe = write_exe(tmp.path(), "pinned-srv", b"in the override dir");
        let digest = hex(&Sha256::digest(b"in the override dir"));

        let mut env = BTreeMap::new();
        env.insert("PATH".to_string(), tmp.path().display().to_string());

        let got = verify("pinned-srv", &env, &digest)
            .await
            .expect("found via the overridden PATH")
            .expect("pinned");
        assert_eq!(got, pathjail::canonicalize_or_self(&exe));

        // Without the override the bare name is not on the real PATH.
        assert!(
            verify("pinned-srv", &BTreeMap::new(), &digest)
                .await
                .is_err()
        );
    }
}
