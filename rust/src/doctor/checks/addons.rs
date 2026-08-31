//! Installed addons: what actually loaded, and what did not.
//!
//! `addon list` reads the store — the files on disk. This check reads the
//! **extension registry**, which is what the running engine uses. Between the
//! two sits the thing worth verifying: `addon add` checks a module's four magic
//! bytes, which is a prefix and not a parse, so a truncated or corrupt module
//! installs cleanly, sits in the right directory with the right digest, and is
//! then refused by the loader. Only a check on this side can tell a healthy
//! install from that one, which is why `addon list` points here.

use crate::doctor::{BOLD, DIM, GREEN, Outcome, RED, RST, YELLOW};

/// Compare the modules present in the addon store against the compressors the
/// registry actually holds.
///
/// Returns `None` when nothing is installed: a user with no addons should not
/// get a line about addons.
pub(crate) fn addons_loaded_outcome() -> Option<Outcome> {
    let store = crate::core::context_package::addons::store_root().ok()?;

    // Names the store expects to provide, derived exactly the way the loader
    // derives them — one version per addon, registered by file stem.
    let expected: Vec<String> = {
        #[cfg(feature = "wasm")]
        {
            crate::core::context_package::addons::installed_modules(&store)
                .iter()
                .filter_map(|p| p.file_stem().map(|s| s.to_string_lossy().into_owned()))
                .collect()
        }
        #[cfg(not(feature = "wasm"))]
        {
            let _ = &store;
            Vec::new()
        }
    };

    // A declared MCP server is not a compressor and never appears in the
    // registry, so it must not be counted as a module that failed to load.
    let wired = wired_addon_count();

    if expected.is_empty() && wired == 0 {
        return None;
    }

    let registered: Vec<String> = crate::core::extension_registry::global()
        .read()
        .map(|r| r.compressor_names())
        .unwrap_or_default();

    let missing: Vec<&String> = expected
        .iter()
        .filter(|name| !registered.contains(name))
        .collect();

    let gateway_note = if wired > 0 && !crate::core::config::Config::load().gateway.enabled {
        format!(", {wired} MCP server(s) wired but {YELLOW}gateway off{RST}")
    } else if wired > 0 {
        format!(", {wired} MCP server(s) wired")
    } else {
        String::new()
    };

    if missing.is_empty() {
        let modules = if expected.is_empty() {
            format!("{DIM}no WASM modules{RST}")
        } else {
            format!("{GREEN}{} module(s) loaded{RST}", expected.len())
        };
        return Some(Outcome {
            ok: true,
            line: format!("{BOLD}Addons{RST}  {modules}{gateway_note}"),
        });
    }

    // Installed but not registered: the module is on disk and the engine
    // declined it. Say which, because the name is what the author debugs with.
    //
    // The hint names the parse, not the ABI: missing entrypoints do *not*
    // prevent registration — they surface when the compressor is called — so a
    // module that fails here failed to be read as WebAssembly at all.
    Some(Outcome {
        ok: false,
        line: format!(
            "{BOLD}Addons{RST}  {RED}{} of {} module(s) failed to load{RST}: {}  \
             {DIM}(present on disk but not readable as WebAssembly — likely truncated, \
             corrupt, or not built for wasm32-unknown-unknown){RST}{gateway_note}",
            missing.len(),
            expected.len(),
            missing
                .iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join(", "),
        ),
    })
}

/// How many installed addons declare an `[mcp]` server.
fn wired_addon_count() -> usize {
    let Ok(store) = crate::core::context_package::addons::store_root() else {
        return 0;
    };
    let root = crate::core::context_package::addons::addons_root(&store);
    let Ok(names) = std::fs::read_dir(root) else {
        return 0;
    };
    let mut count = 0;
    for name_entry in names.flatten().filter(|e| e.path().is_dir()) {
        let Ok(versions) = std::fs::read_dir(name_entry.path()) else {
            continue;
        };
        let declares = versions
            .flatten()
            .filter(|e| e.path().is_dir())
            .any(|version| {
                std::fs::read_to_string(version.path().join("lean-ctx-addon.toml"))
                    .ok()
                    .and_then(|t| crate::core::context_package::addon_manifest::parse(&t).ok())
                    .is_some_and(|m| m.mcp.is_some())
            });
        if declares {
            count += 1;
        }
    }
    count
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A user with no addons should not be told about addons.
    #[test]
    fn no_addons_means_no_line() {
        let _iso = crate::core::data_dir::isolated_data_dir();
        assert!(addons_loaded_outcome().is_none());
    }

    /// The point of the check: a module present on disk but absent from the
    /// registry is a failure the user can act on, and `addon list` alone could
    /// never report it, because the store looks perfectly healthy.
    ///
    /// The payload here has valid WebAssembly magic and a corrupt body. That is
    /// precisely the gap install cannot close: `addon add` checks the magic
    /// bytes, which is a four-byte prefix, not a parse — so a truncated or
    /// corrupted module installs cleanly and only fails when the engine tries
    /// to load it. (An *empty but valid* module, by contrast, loads fine; the
    /// ABI entrypoints are resolved when a compressor is called, not when the
    /// module is read.)
    #[cfg(feature = "wasm")]
    #[test]
    fn a_module_that_does_not_load_is_reported_as_a_failure() {
        let _iso = crate::core::data_dir::isolated_data_dir();
        let store = crate::core::context_package::addons::store_root().unwrap();
        let dir = crate::core::context_package::addons::addons_root(&store)
            .join("@ns__broken")
            .join("1.0.0");
        std::fs::create_dir_all(&dir).unwrap();
        let mut corrupt = vec![0x00, 0x61, 0x73, 0x6d, 1, 0, 0, 0];
        corrupt.extend_from_slice(b"not a section");
        std::fs::write(dir.join("brk.wasm"), &corrupt).unwrap();

        let outcome = addons_loaded_outcome().expect("a line, since a module is installed");
        assert!(!outcome.ok, "{}", outcome.line);
        assert!(
            outcome.line.contains("brk"),
            "names the module: {}",
            outcome.line
        );
    }
}
