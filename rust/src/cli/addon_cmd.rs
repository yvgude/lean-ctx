//! `lean-ctx addon` — the extension surface.
//!
//! Five verbs, deliberately: `list`, `info`, `add`, `remove`, `release`.
//!
//! There is no `search`. Search implies an index someone curates, and that is
//! a marketplace — explicitly out of scope. An addon arrives as a file or from
//! a registry the user names; lean-ctx does not rank or recommend.
//!
//! `add` is the one verb that stores executable code, so it is the only one
//! that asks. Everything the prompt shows — publisher key, module digests — is
//! read from the pack the user is about to install, after its signature and
//! content digest have already been verified.

use std::path::{Path, PathBuf};

use crate::core::context_package::{
    LocalRegistry, addon_manifest, addon_wiring, addons, content::DocumentBlob,
};

pub(crate) fn cmd_addon(args: &[String]) {
    let sub = args
        .iter()
        .find(|a| !a.starts_with('-') && a.as_str() != "addon" && a.as_str() != "addons")
        .map(String::as_str);

    match sub {
        Some("list" | "ls") | None => cmd_list(),
        Some("info") => cmd_info(args),
        Some("add" | "install") => cmd_add(args),
        Some("remove" | "rm" | "uninstall") => cmd_remove(args),
        Some("release") => cmd_release(args),
        Some("help" | "--help" | "-h") => print_usage(),
        Some(other) => {
            eprintln!("lean-ctx addon: unknown subcommand '{other}'");
            print_usage();
            std::process::exit(1);
        }
    }
}

fn print_usage() {
    println!(
        "lean-ctx addon — WASM extensions loaded into the context pipeline

  list                    Installed addons and the modules they load
  info <name>             The author's manifest and module digests
  add <file.ctxpkg>       Verify, ask, then install
  remove <name>           Remove an addon and its modules
  release <dir>           Build a signed .ctxpkg from a directory

An addon directory holds `lean-ctx-addon.toml` and one or more `.wasm`
modules. `release` embeds the modules in the package and signs it, so
publishing needs no artifact host, no checksum files and no CI.

Extensions run in a WASM sandbox: no ambient environment, a fresh store per
call, and the host enforces the output budget after decoding. Only what a
module returns can affect lean-ctx.

Docs: docs/contracts/wasm-abi-v1.md"
    );
}

fn store_root() -> Option<PathBuf> {
    addons::store_root().ok()
}

/// One installed addon as it exists on disk.
struct Installed {
    name: String,
    version: String,
    dir: PathBuf,
    modules: Vec<PathBuf>,
    /// The `[mcp]` command line, when the addon declares one.
    wiring: Option<String>,
}

/// Read the store rather than the package index.
///
/// The store is what the extension registry actually loads, so listing it
/// cannot disagree with reality — an index row whose files are missing would
/// otherwise be reported as an installed addon that does nothing.
fn installed_addons() -> Vec<Installed> {
    let Some(root) = store_root() else {
        return Vec::new();
    };
    let addons_root = addons::addons_root(&root);
    let Ok(names) = std::fs::read_dir(&addons_root) else {
        return Vec::new();
    };

    let mut out = Vec::new();
    for name_entry in names.flatten().filter(|e| e.path().is_dir()) {
        let display_name = name_entry.file_name().to_string_lossy().replace("__", "/");
        let Ok(versions) = std::fs::read_dir(name_entry.path()) else {
            continue;
        };
        for version_entry in versions.flatten().filter(|e| e.path().is_dir()) {
            let dir = version_entry.path();
            let mut modules: Vec<PathBuf> = std::fs::read_dir(&dir)
                .into_iter()
                .flatten()
                .flatten()
                .map(|e| e.path())
                .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("wasm"))
                .collect();
            modules.sort();
            let wiring = std::fs::read_to_string(dir.join("lean-ctx-addon.toml"))
                .ok()
                .and_then(|t| addon_manifest::parse(&t).ok())
                .and_then(|m| m.mcp.map(|w| w.describe()));
            out.push(Installed {
                name: display_name.clone(),
                version: version_entry.file_name().to_string_lossy().into_owned(),
                dir,
                modules,
                wiring,
            });
        }
    }
    out.sort_by(|a, b| (&a.name, &a.version).cmp(&(&b.name, &b.version)));
    out
}

fn cmd_list() {
    let installed = installed_addons();
    if installed.is_empty() {
        println!("No addons installed.");
        println!();
        println!("Install one:  lean-ctx addon add <file.ctxpkg>");
        println!("Build one:    lean-ctx addon release <dir>");
        return;
    }

    println!("Installed addons ({}):", installed.len());
    println!();
    for a in &installed {
        let names: Vec<String> = a
            .modules
            .iter()
            .filter_map(|m| m.file_stem().map(|s| s.to_string_lossy().into_owned()))
            .collect();
        println!("  {}@{}", a.name, a.version);
        if !names.is_empty() {
            println!("    compressors: {}", names.join(", "));
        }
        if let Some(w) = a.wiring.as_deref() {
            println!("    MCP server:  {w}");
        }
        if names.is_empty() && a.wiring.is_none() {
            println!("    (nothing is loaded from this addon)");
        }
    }
    println!();
    println!("Modules register as compressors and are visible in `lean-ctx doctor`.");
    if !addon_wiring::gateway_enabled() {
        println!("The MCP gateway is OFF — declared servers are recorded but not spawned.");
    }
}

fn cmd_info(args: &[String]) {
    let Some(name) = positional(args, "info") else {
        eprintln!("Usage: lean-ctx addon info <name>");
        std::process::exit(2);
    };

    let matches: Vec<Installed> = installed_addons()
        .into_iter()
        .filter(|a| a.name == name || a.name.trim_start_matches('@') == name)
        .collect();

    if matches.is_empty() {
        eprintln!("No installed addon named `{name}`.");
        eprintln!("See what is installed with: lean-ctx addon list");
        std::process::exit(1);
    }

    for a in matches {
        println!("{}@{}", a.name, a.version);
        println!("  stored at   {}", a.dir.display());
        for module in &a.modules {
            let bytes = std::fs::read(module).unwrap_or_default();
            println!(
                "  module      {}  ({} bytes, sha256 {})",
                module.file_name().unwrap_or_default().to_string_lossy(),
                bytes.len(),
                &sha256_hex(&bytes)[..16],
            );
        }
        let manifest_path = a.dir.join("lean-ctx-addon.toml");
        match std::fs::read_to_string(&manifest_path) {
            Ok(text) => {
                println!();
                println!("  --- lean-ctx-addon.toml (as published) ---");
                for line in text.lines() {
                    println!("  {line}");
                }
            }
            Err(_) => println!("  (no authoring manifest stored)"),
        }
    }
}

fn cmd_add(args: &[String]) {
    let Some(file) = positional(args, "add").or_else(|| positional(args, "install")) else {
        eprintln!("Usage: lean-ctx addon add <file.ctxpkg>");
        std::process::exit(2);
    };
    let path = Path::new(&file);
    let assume_yes = args.iter().any(|a| a == "--yes" || a == "-y");

    let registry = match LocalRegistry::open() {
        Ok(r) => r,
        Err(e) => {
            eprintln!("ERROR: {e}");
            std::process::exit(1);
        }
    };

    // Show what is being installed BEFORE asking. The preview is derived from
    // the file on disk; the install call re-reads and re-verifies it, so the
    // prompt can never describe something other than what gets stored.
    match preview(path) {
        Ok(p) => {
            println!("{}@{}", p.name, p.version);
            println!("  {}", p.description);
            println!(
                "  signature   {}",
                p.signed
                    .as_deref()
                    .map_or("UNSIGNED — anyone could have produced this file", |k| k)
            );
            for (module, digest, bytes) in &p.modules {
                println!(
                    "  module      {module}  ({bytes} bytes, sha256 {})",
                    &digest[..16]
                );
            }
            if let Some(w) = &p.wiring {
                println!("  MCP server  {w}");
                println!(
                    "  pin         {}",
                    if p.pinned {
                        "sha256 pinned — the gateway refuses to spawn a changed binary"
                    } else {
                        "none — whatever `command` resolves to at spawn time"
                    }
                );
            }
            println!();
            if !p.modules.is_empty() {
                println!("Modules run inside lean-ctx as WASM: sandboxed, no ambient");
                println!("environment, output budget enforced by the host.");
            }
            if p.wiring.is_some() {
                // The honest part. A WASM module is bounded; an MCP server is
                // an ordinary process with the user's own privileges. Saying so
                // plainly is the difference between consent and a click-through.
                println!(
                    "The MCP server above runs as a NORMAL PROCESS with your privileges — it is"
                );
                println!(
                    "not sandboxed. lean-ctx will not install it: it records how to run it, and"
                );
                println!("only spawns it while `gateway.enabled = true`.");
            }
            println!();
        }
        Err(e) => {
            eprintln!("ERROR: {e}");
            std::process::exit(1);
        }
    }

    if !crate::cli::prompt::confirm("Install this addon?", assume_yes) {
        println!("Aborted. Nothing was installed.");
        return;
    }

    match registry.install_addon_from_file(path) {
        Ok(manifest) => {
            println!("Installed {}@{}", manifest.name, manifest.version);

            // Wiring happens after the package is stored: if the store write
            // fails there must be no gateway entry pointing at an addon that
            // was never installed.
            match wire_after_install(path) {
                Ok(addon_wiring::Wired::NothingToWire) => {}
                Ok(addon_wiring::Wired::Added(name) | addon_wiring::Wired::Replaced(name)) => {
                    println!("Wired `{name}` into the MCP gateway.");
                    if !addon_wiring::gateway_enabled() {
                        println!();
                        println!("The gateway is currently OFF, so nothing spawns yet.");
                        println!("Turn it on with:  lean-ctx config set gateway.enabled true");
                    }
                }
                Err(e) => {
                    // The package is installed; only the wiring failed. Say
                    // exactly that rather than implying the whole install broke.
                    eprintln!("WARNING: installed, but could not wire the MCP server: {e}");
                    eprintln!("         Re-run `lean-ctx addon add` after fixing it.");
                }
            }
            println!("Modules load on the next lean-ctx start.");
        }
        Err(e) => {
            eprintln!("ERROR: install failed: {e}");
            std::process::exit(1);
        }
    }
}

/// Parse the just-installed package's manifest and apply its `[mcp]` wiring.
///
/// Reads the package file again rather than threading the manifest out of the
/// installer: the file is the same one that was verified moments ago, and this
/// keeps the wiring step independent of the storage path's signature.
fn wire_after_install(path: &Path) -> Result<addon_wiring::Wired, String> {
    let json = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    let value: serde_json::Value = serde_json::from_str(&json).map_err(|e| e.to_string())?;
    let toml_text = value
        .get("content")
        .and_then(|c| c.get("addon"))
        .and_then(|a| a.get("manifest_toml"))
        .and_then(|v| v.as_str())
        .ok_or("package has no addon manifest")?;
    let manifest = addon_manifest::parse(toml_text)?;
    addon_wiring::register(&manifest)
}

struct Preview {
    name: String,
    version: String,
    description: String,
    signed: Option<String>,
    modules: Vec<(String, String, usize)>,
    /// The command or URL the addon asks lean-ctx to run, if any.
    wiring: Option<String>,
    /// Whether that command carries a SHA-256 pin. Material to the decision:
    /// pinned means the gateway refuses to spawn a binary that has changed
    /// underneath it, unpinned means it spawns whatever `command` resolves to
    /// on the day.
    pinned: bool,
}

/// Read a pack for display only. Verification happens in the install path;
/// this exists so the consent prompt can name what it is asking about.
fn preview(path: &Path) -> Result<Preview, String> {
    let json =
        std::fs::read_to_string(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    let value: serde_json::Value =
        serde_json::from_str(&json).map_err(|e| format!("parse package: {e}"))?;
    let manifest = value.get("manifest").ok_or("package has no manifest")?;

    let str_at = |k: &str| -> String {
        manifest
            .get(k)
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string()
    };

    let modules = value
        .get("content")
        .and_then(|c| c.get("addon"))
        .and_then(|a| a.get("modules"))
        .and_then(|m| m.as_array())
        .map(|arr| {
            arr.iter()
                .map(|m| {
                    (
                        m.get("path")
                            .and_then(|v| v.as_str())
                            .unwrap_or("?")
                            .to_string(),
                        m.get("sha256")
                            .and_then(|v| v.as_str())
                            .unwrap_or(&"?".repeat(64))
                            .to_string(),
                        m.get("body").and_then(|v| v.as_str()).map_or(0, str::len),
                    )
                })
                .collect()
        })
        .unwrap_or_default();

    let declared = value
        .get("content")
        .and_then(|c| c.get("addon"))
        .and_then(|a| a.get("manifest_toml"))
        .and_then(|v| v.as_str())
        .and_then(|toml| addon_manifest::parse(toml).ok())
        .and_then(|m| m.mcp);
    let pinned = declared.as_ref().is_some_and(|w| !w.sha256.is_empty());
    let wiring = declared.map(|w| w.describe());

    Ok(Preview {
        name: str_at("name"),
        version: str_at("version"),
        description: str_at("description"),
        signed: manifest
            .get("signature")
            .and_then(|s| s.get("public_key"))
            .and_then(|v| v.as_str())
            .map(|k| format!("signed by {}…", &k[..k.len().min(16)])),
        modules,
        wiring,
        pinned,
    })
}

fn cmd_remove(args: &[String]) {
    let Some(name) = positional(args, "remove")
        .or_else(|| positional(args, "rm"))
        .or_else(|| positional(args, "uninstall"))
    else {
        eprintln!("Usage: lean-ctx addon remove <name>");
        std::process::exit(2);
    };

    let matches: Vec<Installed> = installed_addons()
        .into_iter()
        .filter(|a| a.name == name || a.name.trim_start_matches('@') == name)
        .collect();

    if matches.is_empty() {
        eprintln!("No installed addon named `{name}`.");
        std::process::exit(1);
    }

    let mut removed = 0;
    for a in &matches {
        match std::fs::remove_dir_all(&a.dir) {
            Ok(()) => {
                println!("Removed {}@{}", a.name, a.version);
                removed += 1;
            }
            Err(e) => eprintln!("ERROR: remove {}: {e}", a.dir.display()),
        }
    }

    // Also drop the index row so `pack list` does not keep advertising it.
    if let Ok(registry) = LocalRegistry::open() {
        let _ = registry.remove(&matches[0].name, None);
    }

    // And the gateway entry, by the manifest's own addon name — which is the
    // gateway server name, and is not always the package name.
    for a in &matches {
        let manifest_path = a.dir.join("lean-ctx-addon.toml");
        let wired_name = std::fs::read_to_string(&manifest_path)
            .ok()
            .and_then(|t| addon_manifest::parse(&t).ok())
            .map(|m| m.addon.name)
            .unwrap_or_else(|| a.name.trim_start_matches('@').to_string());
        match addon_wiring::unregister(&wired_name) {
            Ok(true) => println!("Unwired `{wired_name}` from the MCP gateway."),
            Ok(false) => {}
            Err(e) => eprintln!("WARNING: could not update the gateway config: {e}"),
        }
    }

    if removed > 0 {
        println!("Its modules stop loading on the next lean-ctx start.");
    }
}

fn cmd_release(args: &[String]) {
    let dir = positional(args, "release").unwrap_or_else(|| ".".to_string());
    let dir = Path::new(&dir);
    let output = flag_value(args, "--output").or_else(|| flag_value(args, "-o"));

    match build_release(dir, output.as_deref()) {
        Ok(out) => {
            println!("Wrote {}", out.display());
            println!();
            println!("Install locally:  lean-ctx addon add {}", out.display());
            println!("Publish:          lean-ctx pack publish {}", out.display());
        }
        Err(e) => {
            eprintln!("ERROR: {e}");
            std::process::exit(1);
        }
    }
}

/// Build a signed `.ctxpkg` from an addon directory.
///
/// This is the answer to the one complaint the previous addon channel drew:
/// authors had to run their own CI to produce checksum files for externally
/// hosted artifacts. Here the modules are read from disk, hashed, compressed
/// and embedded, and the pack signature covers all of it — one local command,
/// no artifact host, no CI required.
fn build_release(dir: &Path, output: Option<&str>) -> Result<PathBuf, String> {
    let manifest_path = dir.join("lean-ctx-addon.toml");
    let manifest_toml = std::fs::read_to_string(&manifest_path)
        .map_err(|e| format!("read {}: {e}", manifest_path.display()))?;

    let parsed: toml::Value =
        toml::from_str(&manifest_toml).map_err(|e| format!("{}: {e}", manifest_path.display()))?;
    let table = parsed
        .get("addon")
        .ok_or_else(|| format!("{}: missing [addon] table", manifest_path.display()))?;
    let name = table
        .get("name")
        .and_then(toml::Value::as_str)
        .ok_or_else(|| format!("{}: [addon] name is required", manifest_path.display()))?;
    let version = table
        .get("version")
        .and_then(toml::Value::as_str)
        .ok_or_else(|| format!("{}: [addon] version is required", manifest_path.display()))?;
    let description = table
        .get("description")
        .and_then(toml::Value::as_str)
        .unwrap_or("");

    let mut wasm_files: Vec<PathBuf> = std::fs::read_dir(dir)
        .map_err(|e| format!("read {}: {e}", dir.display()))?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("wasm"))
        .collect();
    wasm_files.sort();

    if wasm_files.is_empty() {
        return Err(format!(
            "no .wasm module in {} — an addon without a module would install nothing",
            dir.display()
        ));
    }

    let mut modules = Vec::new();
    for path in &wasm_files {
        let bytes = std::fs::read(path).map_err(|e| format!("read {}: {e}", path.display()))?;
        let file_name = path
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| format!("{}: unusable file name", path.display()))?;
        if !bytes.starts_with(b"\0asm") {
            return Err(format!(
                "{file_name} is not a WebAssembly module (bad magic) — was it built with \
                 `--target wasm32-unknown-unknown`?"
            ));
        }
        modules.push(DocumentBlob::from_plaintext(file_name, &bytes)?);
    }

    let out_path = output.map_or_else(
        || {
            let safe = name.replace('/', "__").replace('@', "");
            dir.join(format!(
                "{safe}-{version}.{}",
                crate::core::contracts::PACKAGE_EXTENSION
            ))
        },
        PathBuf::from,
    );

    crate::core::context_package::addons_build::write_addon_package(
        &out_path,
        name,
        version,
        description,
        &manifest_toml,
        modules,
    )?;

    println!(
        "Packed {} module(s) from {}",
        wasm_files.len(),
        dir.display()
    );
    Ok(out_path)
}

fn positional(args: &[String], after: &str) -> Option<String> {
    args.iter()
        .skip_while(|a| a.as_str() != after)
        .skip(1)
        .find(|a| !a.starts_with('-'))
        .cloned()
}

fn flag_value(args: &[String], flag: &str) -> Option<String> {
    if let Some(v) = args
        .iter()
        .find_map(|a| a.strip_prefix(&format!("{flag}=")))
    {
        return Some(v.to_string());
    }
    args.iter()
        .position(|a| a == flag)
        .and_then(|i| args.get(i + 1))
        .cloned()
}

fn sha256_hex(data: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(data);
    crate::core::agent_identity::hex_encode(&h.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn positional_skips_flags_and_the_verb() {
        let args: Vec<String> = ["addon", "add", "--yes", "pack.ctxpkg"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(positional(&args, "add"), Some("pack.ctxpkg".into()));
    }

    #[test]
    fn flag_value_accepts_both_spellings() {
        let split: Vec<String> = ["release", ".", "--output", "out.ctxpkg"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(flag_value(&split, "--output"), Some("out.ctxpkg".into()));

        let joined: Vec<String> = ["release", ".", "--output=out.ctxpkg"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(flag_value(&joined, "--output"), Some("out.ctxpkg".into()));
    }

    /// A directory with a manifest but no module would install an addon that
    /// does nothing — say so at build time, not after publishing.
    #[test]
    fn release_refuses_a_directory_without_a_module() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("lean-ctx-addon.toml"),
            "[addon]\nname = \"@ns/demo\"\nversion = \"1.0.0\"\n",
        )
        .unwrap();
        let err = build_release(dir.path(), None).expect_err("must refuse");
        assert!(err.contains("no .wasm module"), "{err}");
    }

    /// A `.wasm` that is not WebAssembly is caught before it is packed, with a
    /// message that names the likely cause.
    #[test]
    fn release_refuses_a_file_that_is_not_webassembly() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("lean-ctx-addon.toml"),
            "[addon]\nname = \"@ns/demo\"\nversion = \"1.0.0\"\n",
        )
        .unwrap();
        std::fs::write(dir.path().join("bogus.wasm"), b"not wasm at all").unwrap();
        let err = build_release(dir.path(), None).expect_err("must refuse");
        assert!(err.contains("not a WebAssembly module"), "{err}");
        assert!(
            err.contains("wasm32-unknown-unknown"),
            "must hint the cause: {err}"
        );
    }

    #[test]
    fn release_requires_name_and_version() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("lean-ctx-addon.toml"), "[addon]\n").unwrap();
        let err = build_release(dir.path(), None).expect_err("must refuse");
        assert!(err.contains("name is required"), "{err}");
    }
}
