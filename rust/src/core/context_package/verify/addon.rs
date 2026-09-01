//! `kind=addon` payload validation.
//!
//! Split out of `verify.rs` because that file is at the 1500-line gate, and a
//! payload whose bytes get executed deserves to be read on its own anyway.

/// Validate a `kind=addon` payload: a verbatim authoring manifest plus at
/// least one embedded WASM module.
///
/// The modules are hash-pinned `DocumentBlob`s, so the same tamper detection
/// that protects skills content protects executable bytes here — decode is
/// verified against the pinned digest before anything reaches disk. Paths must
/// end in `.wasm`: the store is scanned for modules to load, and a payload that
/// can drop arbitrary filenames there would widen that scan into "execute
/// whatever the pack shipped".
pub(super) fn validate_addon(
    addon: &crate::core::context_package::content::AddonContent,
    errors: &mut Vec<String>,
) {
    use crate::core::context_package::content::{
        MAX_ADDON_MODULE_BYTES, MAX_ADDON_MODULES, MAX_ADDON_TOTAL_BYTES,
    };

    if addon.manifest_toml.trim().is_empty() {
        errors.push("kind=addon payload has an empty manifest_toml".into());
        return;
    }

    // The manifest is the contract between author and host, so it is parsed
    // here rather than at install: a package that cannot be understood must not
    // be storable, let alone installable.
    let manifest = match crate::core::context_package::addon_manifest::parse(&addon.manifest_toml) {
        Ok(m) => m,
        Err(e) => {
            errors.push(e);
            return;
        }
    };

    // An addon declares WASM modules, an MCP server, or both. Neither means
    // installing it would have no effect — a defect in the package, not a state
    // worth storing.
    if addon.modules.is_empty() && manifest.mcp.is_none() {
        errors.push(
            "kind=addon payload declares neither a WASM module nor an [mcp] server — \
             installing it would have no effect"
                .into(),
        );
        return;
    }
    if addon.modules.len() > MAX_ADDON_MODULES {
        errors.push(format!(
            "addon payload has {} modules (cap: {MAX_ADDON_MODULES})",
            addon.modules.len()
        ));
        return;
    }

    let mut seen = std::collections::HashSet::new();
    let mut total: usize = 0;
    for blob in &addon.modules {
        if let Err(e) = super::validate_document_path(&blob.path) {
            errors.push(e);
            continue;
        }
        if !std::path::Path::new(&blob.path)
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("wasm"))
        {
            errors.push(format!(
                "addon module `{}` must be a `.wasm` file",
                blob.path
            ));
            continue;
        }
        if !seen.insert(blob.path.as_str()) {
            errors.push(format!("duplicate addon module `{}`", blob.path));
            continue;
        }
        if blob.sha256.len() != 64 || !blob.sha256.chars().all(|c| c.is_ascii_hexdigit()) {
            errors.push(format!(
                "`{}`: sha256 must be a 64-char hex string",
                blob.path
            ));
            continue;
        }
        match blob.decode_verified() {
            Ok(plain) => {
                if plain.len() > MAX_ADDON_MODULE_BYTES {
                    errors.push(format!(
                        "addon module `{}` is {} bytes (cap: {MAX_ADDON_MODULE_BYTES})",
                        blob.path,
                        plain.len()
                    ));
                }
                // Reject anything that is not a WebAssembly module before it is
                // ever stored: `\0asm` + version 1. Cheap, and it keeps a
                // mislabelled or corrupt payload from reaching the loader.
                if !plain.starts_with(b"\0asm") {
                    errors.push(format!(
                        "addon module `{}` is not a WebAssembly module (bad magic)",
                        blob.path
                    ));
                }
                total += plain.len();
            }
            Err(e) => errors.push(e),
        }
    }
    if total > MAX_ADDON_TOTAL_BYTES {
        errors.push(format!(
            "addon payload decodes to {total} bytes (cap: {MAX_ADDON_TOTAL_BYTES})"
        ));
    }
}
