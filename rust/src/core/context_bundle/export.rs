// SPDX-License-Identifier: Apache-2.0
//! Where a bundle may be written as a file.
//!
//! A bundle file leaves the delivery path of a tool result, so its
//! destination follows the rule the package export already applies
//! (`cli::pack_cmd::checkpoint`): bound to the project the bundle was built
//! for, also when the general tool jail is compiled out or switched off.
//! The caller writes the RELEASED text there, never the text as built.

use std::path::{Path, PathBuf};

/// The file a bundle of the project `root` may be written to, or the reason
/// it may not. `candidate` is the absolute destination the caller named.
pub fn destination(tool: &str, root: &Path, candidate: &Path) -> Result<PathBuf, String> {
    let (output, _) = crate::core::io_boundary::jail_and_check_export_path(tool, candidate, root)?;
    // The jail also admits lean-ctx's own data and state directories and
    // configured allow paths; a bundle of project sources belongs inside the
    // project only. Both sides go through the same canonicalizer.
    let canonical_root = crate::core::pathutil::canonicalize_secure(root)
        .map_err(|_| "project root is unavailable".to_string())?;
    let inside = output
        .parent()
        .and_then(|parent| crate::core::pathutil::canonicalize_secure(parent).ok())
        .is_some_and(|parent| parent.starts_with(&canonical_root));
    if !inside {
        return Err("a bundle file must stay inside the project root".to_string());
    }
    crate::core::pathjail::enforce_writable(&output)?;
    Ok(output)
}

/// Write `bytes` to a bundle destination below `root`, refusing a symlinked
/// final component. `destination` leaves a dangling symlink unresolved (the
/// jail resolves only the existing ancestor), so a cloned repository can ship
/// `bundle.xml -> ../../outside`; the confined write rebuilds the path below
/// the project and fails closed on any symlinked component.
pub fn write(tool: &str, root: &Path, candidate: &Path, bytes: &[u8]) -> Result<PathBuf, String> {
    let output = destination(tool, root, candidate)?;
    crate::core::pathjail_write::write_requested_below(root, candidate, &output, bytes)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    /// A cloned repository can ship the bundle file as a dangling symlink; the
    /// write must be refused, not carried out of the project.
    #[test]
    fn write_refuses_a_symlinked_bundle_file() {
        let _iso = crate::core::data_dir::isolated_data_dir();
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("proj");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("Cargo.toml"), "[package]\nname=\"p\"\n").unwrap();
        let outside = tmp.path().join("outside.xml");

        // dangling link (target absent): destination's parent check passes.
        symlink(&outside, root.join("bundle.xml")).unwrap();
        let err = write("ctx_pack", &root, &root.join("bundle.xml"), b"<x/>").unwrap_err();
        assert!(err.contains("symlink"), "{err}");
        assert!(!outside.exists());

        // existing-file link is refused too.
        let victim = tmp.path().join("victim.xml");
        std::fs::write(&victim, "keep").unwrap();
        symlink(&victim, root.join("bundle2.xml")).unwrap();
        assert!(write("ctx_pack", &root, &root.join("bundle2.xml"), b"<x/>").is_err());
        assert_eq!(std::fs::read_to_string(&victim).unwrap(), "keep");

        // a plain destination inside the project still writes.
        let path = write("ctx_pack", &root, &root.join("ok.xml"), b"<x/>").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"<x/>");
    }
}
