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
pub(crate) fn destination(tool: &str, root: &Path, candidate: &Path) -> Result<PathBuf, String> {
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
