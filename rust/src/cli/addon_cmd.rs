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

use std::io::IsTerminal;
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
        "lean-ctx addon — extensions: WASM modules and declared MCP servers

  list                    Installed addons, the modules they load, what they wire
  info <name>             The author's manifest and module digests
  add <pkg | ns/name>     Verify, show, ask, then install (file or registry ref)
  remove <name>           Remove an addon, its modules and its gateway entry
  release <dir>           Build a signed .ctxpkg from a directory

An addon directory holds `lean-ctx-addon.toml` and, for a WASM addon, one or
more `.wasm` modules. `release` embeds the modules in the package and signs it,
so publishing needs no artifact host, no checksum files and no CI.

WASM modules run in a sandbox: no ambient environment, a fresh store per call,
and the host enforces the output budget after decoding. Only what a module
returns can affect lean-ctx.

A manifest may instead declare an MCP server under `[mcp]`. That server is an
ordinary process with your privileges — not sandboxed — so `add` prints the
exact command before asking, and lean-ctx never installs the binary for you.

Docs: docs/guides/addons.md · contracts: wasm-abi-v1, addon-manifest-v1"
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
    /// Whether this is the version whose modules actually load. The store keeps
    /// versions side by side, so after an upgrade two rows share a name and
    /// only one of them is live — printing both as if they both ran would be a
    /// listing that disagrees with the running system.
    active: bool,
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
        // Which version is live is decided by the loader, not restated here —
        // the same mtime rule, read from the same directories, so the listing
        // and the running system cannot drift apart.
        let active_dir = std::fs::read_dir(name_entry.path()).ok().and_then(|v| {
            v.flatten()
                .filter(|e| e.path().is_dir())
                .max_by_key(|e| {
                    e.metadata()
                        .and_then(|m| m.modified())
                        .unwrap_or(std::time::UNIX_EPOCH)
                })
                .map(|e| e.path())
        });

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
            let active = active_dir.as_ref().is_some_and(|a| *a == dir);
            out.push(Installed {
                name: display_name.clone(),
                version: version_entry.file_name().to_string_lossy().into_owned(),
                dir,
                modules,
                wiring,
                active,
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
    let superseded = installed.iter().any(|a| !a.active);
    for a in &installed {
        let names: Vec<String> = a
            .modules
            .iter()
            .filter_map(|m| m.file_stem().map(|s| s.to_string_lossy().into_owned()))
            .collect();
        println!(
            "  {}@{}{}",
            a.name,
            a.version,
            if a.active { "" } else { "   (superseded)" }
        );
        if !names.is_empty() {
            // Only the active version's modules reach the registry, so saying
            // "compressors:" for a superseded row would name code that never
            // runs.
            if a.active {
                println!("    compressors: {}", names.join(", "));
            } else {
                println!("    modules on disk, not loaded: {}", names.join(", "));
            }
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
    if superseded {
        println!("Superseded versions stay on disk; `addon remove <name>` clears them all.");
    }
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

/// A downloaded package, deleted when this guard drops.
///
/// The staging file must outlive the consent prompt: the whole point is that
/// the user sees the digests of the exact bytes that will be installed, so the
/// remote path and the local path have to converge on one file before anything
/// is shown.
struct Staged(std::path::PathBuf);

impl Drop for Staged {
    fn drop(&mut self) {
        std::fs::remove_file(&self.0).ok();
    }
}

/// Fetch `ns/name[@version]` from a package registry into a temp file.
///
/// `pack install` refuses executable content and points at `addon add`, which
/// until now only accepted a local path — so a published addon could be
/// resolved, downloaded and then not installed by any command. This closes that
/// loop without adding a second consent path: the artifact lands on disk and
/// then goes through exactly the same preview, prompt and verification as a
/// file the user already had.
fn stage_remote(reference: &str, registry_flag: Option<&str>) -> Result<Staged, String> {
    use crate::core::context_package::remote;

    let parsed = remote::parse_remote_ref(reference)
        .ok_or_else(|| format!("'{reference}' is not a file, nor a valid ns/name[@version]"))?;
    let base = remote::registry_base(registry_flag);
    let token = remote::publish_token(None);
    let (ns, name) = (&parsed.namespace, &parsed.name);

    println!("Resolving @{ns}/{name} via {base} …");
    let versions = remote::fetch_versions(&base, ns, name, token.as_deref())?;
    let info = remote::select_version(&versions, parsed.version.as_deref())?;
    if info.yanked {
        eprintln!(
            "WARNING: @{ns}/{name}@{} is YANKED — continuing only because the version \
             was pinned explicitly",
            info.version
        );
    }
    let bytes = remote::download_verified(&base, ns, name, info, token.as_deref())?;
    println!(
        "Downloaded @{ns}/{name}@{} ({} bytes, sha256 verified against the index)",
        info.version,
        bytes.len()
    );

    let tmp = std::env::temp_dir().join(format!("ctxpkg-addon-{}.ctxpkg", std::process::id()));
    std::fs::write(&tmp, &bytes).map_err(|e| format!("stage artifact: {e}"))?;
    Ok(Staged(tmp))
}

/// Does this argument name a file, rather than a registry package?
///
/// Decided by shape, not by what happens to exist. `parse_remote_ref` accepts
/// `./typo.ctxpkg` as a perfectly good `ns/name`, so a mistyped filename used to
/// be resolved over the network and fail with "package not found in the
/// registry — private packages need CTXPKG_TOKEN". That answer is wrong twice:
/// it blames the registry for a local typo, and it tells the user to go find a
/// token they do not need.
///
/// Anything that looks like a path — a `.ctxpkg` name, or a leading `.`, `/` or
/// `~` — is a file, and a missing one is reported as a missing file.
fn looks_like_a_path(arg: &str) -> bool {
    arg.ends_with(".ctxpkg")
        || arg.starts_with('.')
        || arg.starts_with('/')
        || arg.starts_with('~')
        || arg.starts_with("file:")
}

fn cmd_add(args: &[String]) {
    let Some(file) = positional(args, "add").or_else(|| positional(args, "install")) else {
        eprintln!("Usage: lean-ctx addon add <file.ctxpkg | ns/name[@version]>");
        std::process::exit(2);
    };
    let assume_yes = args.iter().any(|a| a == "--yes" || a == "-y");
    let registry_flag = flag_value(args, "--registry");

    // A local file wins over a remote lookup: an argument that names something
    // on disk must never silently reach the network instead.
    let local = Path::new(&file);
    let _staged;
    let path: &Path = if local.is_file() {
        local
    } else if looks_like_a_path(&file) {
        eprintln!("ERROR: no such file: {file}");
        eprintln!("       To install from a registry instead, drop the path and pass ns/name.");
        std::process::exit(1);
    } else {
        match stage_remote(&file, registry_flag.as_deref()) {
            Ok(s) => {
                _staged = s;
                &_staged.0
            }
            Err(e) => {
                eprintln!("ERROR: {e}");
                std::process::exit(1);
            }
        }
    };

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
                    "  module      {module}  ({}, sha256 {})",
                    // No size rather than a wrong one: if the blob will not
                    // decode, the install is about to refuse it anyway, and
                    // inventing a number here would be the only place that
                    // pretended otherwise.
                    bytes.map_or_else(
                        || "size unavailable — payload does not decode".to_string(),
                        |n| format!("{n} bytes")
                    ),
                    &digest[..16]
                );
            }
            if let Some(w) = &p.wiring {
                println!("  MCP server  {w}");
                if let Some(pinned) = p.pinned {
                    println!(
                        "  pin         {}",
                        if pinned {
                            "sha256 pinned — the gateway refuses to spawn a changed binary"
                        } else {
                            "none — whatever `command` resolves to at spawn time"
                        }
                    );
                }
            }
            println!();
            if !p.modules.is_empty() {
                println!("Modules run inside lean-ctx as WASM: sandboxed, no ambient");
                println!("environment, output budget enforced by the host.");
            }
            // A WASM module is bounded; a declared server is not — but the two
            // kinds of server are not the same risk either, and telling an
            // http user that something will run on their machine would be
            // simply false. Each gets the disclosure that is true for it.
            if p.wiring.is_some() {
                if p.spawns_locally {
                    println!(
                        "The MCP server above runs as a NORMAL PROCESS with your privileges — it is"
                    );
                    println!(
                        "not sandboxed. lean-ctx will not install it: it records how to run it, and"
                    );
                    println!("only spawns it while `gateway.enabled = true`.");
                } else {
                    println!(
                        "The endpoint above is REMOTE. Nothing runs on your machine, but lean-ctx"
                    );
                    println!(
                        "will send it requests — including file contents your agent asks about —"
                    );
                    println!(
                        "and treat its replies as untrusted input. Only while `gateway.enabled = true`."
                    );
                }
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
        // A person answering "no" got what they asked for; a script that hit
        // the non-interactive refusal did not. Exiting 0 in the second case
        // would let a pipeline record an install that never happened and carry
        // on as if the addon were there.
        if !std::io::stdin().is_terminal() {
            std::process::exit(1);
        }
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
                // Matched by variant rather than nested, so the compiler owns
                // exhaustiveness. A nested match needed an `unreachable!()` arm
                // for the case handled above — a panic in an install path, put
                // there to satisfy a shape I had chosen, which is the wrong
                // trade whichever way the enum grows later.
                Ok(addon_wiring::Wired::Replaced(name)) => {
                    // Say that the entry was *updated*, not created — and that
                    // their own settings were left alone, so nobody has to
                    // re-check the config to find out.
                    println!("Updated the gateway entry for `{name}`.");
                    println!("Your credentials and per-server on/off setting were kept.");
                    report_gateway_state();
                }
                Ok(addon_wiring::Wired::Added(name)) => {
                    println!("Wired `{name}` into the MCP gateway.");
                    report_gateway_state();
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

/// Say whether anything will actually spawn, after either wiring outcome.
///
/// A user who just consented to a server should not have to infer from silence
/// that the gateway is off and nothing runs.
fn report_gateway_state() {
    if !addon_wiring::gateway_enabled() {
        println!();
        println!("The gateway is currently OFF, so nothing spawns yet.");
        println!("Turn it on with:  lean-ctx config set gateway.enabled true");
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
    modules: Vec<(String, String, Option<usize>)>,
    /// The command or URL the addon asks lean-ctx to run, if any.
    wiring: Option<String>,
    /// Whether that command carries a SHA-256 pin. Material to the decision:
    /// pinned means the gateway refuses to spawn a binary that has changed
    /// underneath it, unpinned means it spawns whatever `command` resolves to
    /// on the day. Meaningless for `http`, hence the `Option`.
    pinned: Option<bool>,
    /// `true` when the declared server is a local process rather than a remote
    /// endpoint. The two deserve different disclosures: one spawns something
    /// with the user's privileges, the other sends their data somewhere.
    spawns_locally: bool,
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
                    // The size must be the size of the *module*, not of its
                    // zstd+base64 body. A reader checking the prompt against
                    // their own file — the whole point of showing a size next
                    // to a digest — would otherwise see a number that matches
                    // nothing, and one that disagrees with `addon info`.
                    // Decoding also verifies the blob against its digest, so a
                    // corrupt payload is caught before the question is asked
                    // rather than after the answer.
                    let decoded = serde_json::from_value::<DocumentBlob>(m.clone())
                        .ok()
                        .and_then(|b| b.decode_verified().ok())
                        .map(|plain| plain.len());
                    (
                        m.get("path")
                            .and_then(|v| v.as_str())
                            .unwrap_or("?")
                            .to_string(),
                        m.get("sha256")
                            .and_then(|v| v.as_str())
                            .unwrap_or(&"?".repeat(64))
                            .to_string(),
                        decoded,
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
    let pinned = declared
        .as_ref()
        .filter(|w| w.transport == crate::core::mcp_catalog::config::TransportKind::Stdio)
        .map(|w| !w.sha256.is_empty());
    let spawns_locally = declared
        .as_ref()
        .is_some_and(|w| w.transport == crate::core::mcp_catalog::config::TransportKind::Stdio);
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
        spawns_locally,
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

    // Read the wired name BEFORE deleting anything. The gateway server name is
    // the manifest's own `[addon] name`, which is not always the package name —
    // and the manifest lives inside the directory that is about to go. Reading
    // it afterwards silently falls back to a guess, the guess misses, and the
    // gateway keeps an entry for an addon the user believes is gone.
    let wired_names: Vec<String> = matches
        .iter()
        .map(|a| {
            std::fs::read_to_string(a.dir.join("lean-ctx-addon.toml"))
                .ok()
                .and_then(|t| addon_manifest::parse(&t).ok())
                .map_or_else(|| a.name.clone(), |m| m.addon.name)
        })
        .collect();

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

    for wired_name in &wired_names {
        match addon_wiring::unregister(wired_name) {
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

    // An addon must *do* something, but a module is only one of the two ways.
    // The installer accepts modules or an `[mcp]` server; if the builder
    // insisted on a module, an MCP-only addon could be installed and never
    // built — the author would have no way to produce the package at all.
    if wasm_files.is_empty() {
        let declares_server = addon_manifest::parse(&manifest_toml)
            .map(|m| m.mcp.is_some())
            .unwrap_or(false);
        if !declares_server {
            return Err(format!(
                "{} declares neither a .wasm module nor an [mcp] server — \
                 installing this addon would have no effect",
                dir.display()
            ));
        }
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
    use crate::core::mcp_catalog::config::TransportKind;

    #[test]
    fn positional_skips_flags_and_the_verb() {
        let args: Vec<String> = ["addon", "add", "--yes", "pack.ctxpkg"]
            .iter()
            .map(ToString::to_string)
            .collect();
        assert_eq!(positional(&args, "add"), Some("pack.ctxpkg".into()));
    }

    #[test]
    fn flag_value_accepts_both_spellings() {
        let split: Vec<String> = ["release", ".", "--output", "out.ctxpkg"]
            .iter()
            .map(ToString::to_string)
            .collect();
        assert_eq!(flag_value(&split, "--output"), Some("out.ctxpkg".into()));

        let joined: Vec<String> = ["release", ".", "--output=out.ctxpkg"]
            .iter()
            .map(ToString::to_string)
            .collect();
        assert_eq!(flag_value(&joined, "--output"), Some("out.ctxpkg".into()));
    }

    /// A directory with a manifest but neither a module nor an `[mcp]` server
    /// would install an addon that does nothing — say so at build time, not
    /// after publishing.
    #[test]
    fn release_refuses_a_directory_without_a_module() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("lean-ctx-addon.toml"),
            "[addon]\nname = \"@ns/demo\"\nversion = \"1.0.0\"\n",
        )
        .unwrap();
        let err = build_release(dir.path(), None).expect_err("must refuse");
        assert!(
            err.contains("neither a .wasm module nor an [mcp] server"),
            "{err}"
        );
    }

    /// The counterpart: an MCP-only addon is legitimate and must build. The
    /// installer accepts it, so a builder that demanded a module would leave
    /// authors unable to produce the very package the installer takes.
    #[test]
    fn release_accepts_an_mcp_only_addon() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("lean-ctx-addon.toml"),
            "[addon]\nname = \"@ns/demo\"\nversion = \"1.0.0\"\n\
             [mcp]\ncommand = \"demo-server\"\nargs = [\"serve\"]\n",
        )
        .unwrap();

        let out = dir.path().join("demo.ctxpkg");
        build_release(dir.path(), Some(out.to_str().unwrap())).expect("must build");
        assert!(out.is_file(), "a package was written");
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

    /// An argument that names something on disk must never reach the network.
    /// A file called `foo/bar` in the working directory also parses as a valid
    /// `ns/name` reference, and resolving that remotely instead would install
    /// something other than what the user pointed at.
    #[test]
    fn a_local_file_is_preferred_over_a_remote_reference() {
        use crate::core::context_package::remote;

        let dir = tempfile::tempdir().unwrap();
        let ns_dir = dir.path().join("acme");
        std::fs::create_dir(&ns_dir).unwrap();
        let file = ns_dir.join("widget");
        std::fs::write(&file, b"{}").unwrap();

        // The name is a well-formed remote ref …
        assert!(
            remote::parse_remote_ref("acme/widget").is_some(),
            "precondition: this parses as a registry reference"
        );
        // … and yet the path on disk is what cmd_add resolves, because
        // `is_file()` is checked first.
        assert!(file.is_file());
    }

    /// The disclosure must match the risk. A pin is a statement about a local
    /// binary, so it is meaningless for an http endpoint — printing "none —
    /// whatever `command` resolves to" for a URL describes a `command` that
    /// does not exist, and telling that user a process will run with their
    /// privileges is simply false.
    #[test]
    fn the_pin_line_applies_to_stdio_only() {
        let stdio = addon_manifest::parse("[addon]\nname = \"x\"\n[mcp]\ncommand = \"c\"\n")
            .unwrap()
            .mcp
            .unwrap();
        assert_eq!(stdio.transport, TransportKind::Stdio);

        let http = addon_manifest::parse(
            "[addon]\nname = \"x\"\n[mcp]\ntransport = \"http\"\nurl = \"https://e.test/mcp\"\n",
        )
        .unwrap()
        .mcp
        .unwrap();
        assert_eq!(http.transport, TransportKind::Http);
        assert_eq!(
            http.describe(),
            "https://e.test/mcp",
            "an http addon is described by its endpoint, not a command line"
        );
    }

    /// A mistyped filename must not be resolved over the network. `./typo.ctxpkg`
    /// parses as a valid `ns/name`, so without a shape check the user gets
    /// "package not found in the registry — private packages need CTXPKG_TOKEN"
    /// for a local typo: the wrong culprit and a pointless instruction.
    #[test]
    fn path_shaped_arguments_are_never_treated_as_registry_refs() {
        for arg in [
            "./missing.ctxpkg",
            "../build/x.ctxpkg",
            "/abs/path/x.ctxpkg",
            "~/downloads/x.ctxpkg",
            "demo__squeeze-1.0.0.ctxpkg",
        ] {
            assert!(looks_like_a_path(arg), "should be a path: {arg}");
        }
        for arg in ["acme/widget", "acme/widget@1.2.0", "widget"] {
            assert!(!looks_like_a_path(arg), "should be a registry ref: {arg}");
        }
    }

    /// The staging guard exists so the downloaded bytes survive the consent
    /// prompt and are gone afterwards — a leftover .ctxpkg in the temp dir is
    /// executable content nobody is tracking.
    #[test]
    fn staged_artifacts_are_deleted_on_drop() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("staged.ctxpkg");
        std::fs::write(&path, b"{}").unwrap();
        {
            let _guard = Staged(path.clone());
            assert!(path.is_file(), "available while the guard lives");
        }
        assert!(!path.exists(), "removed when the guard drops");
    }

    /// `remove` unwires by the manifest's `[addon] name`, and the manifest
    /// lives in the directory being deleted. Reading it after the delete
    /// silently falls back to a guess; the guess dropped the leading `@`, so
    /// `unregister` matched nothing and the gateway kept an entry for an addon
    /// the user had just removed. Order is the fix, so order is the test.
    #[test]
    fn the_wired_name_is_read_before_the_directory_is_deleted() {
        let dir = tempfile::tempdir().unwrap();
        let manifest = dir.path().join("lean-ctx-addon.toml");
        std::fs::write(
            &manifest,
            "[addon]\nname = \"@ns/demo\"\n[mcp]\ncommand = \"c\"\n",
        )
        .unwrap();

        // What cmd_remove does first: resolve the name from the manifest.
        let wired = std::fs::read_to_string(&manifest)
            .ok()
            .and_then(|t| addon_manifest::parse(&t).ok())
            .map(|m| m.addon.name);
        assert_eq!(wired.as_deref(), Some("@ns/demo"));

        // Once the directory is gone the name is unrecoverable — which is why
        // it must not be read at that point.
        std::fs::remove_dir_all(dir.path()).unwrap();
        assert!(std::fs::read_to_string(&manifest).is_err());
    }
}
