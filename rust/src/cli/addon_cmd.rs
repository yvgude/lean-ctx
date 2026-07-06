//! `lean-ctx addon` — manage community addons (MCP extensions) (#858).
//!
//! Thin CLI over [`crate::core::addons`]: browse the registry, install an addon
//! (from the registry or a local `lean-ctx-addon.toml`), and remove it. `add`
//! and `remove` wire external code into the MCP gateway, so both pass through
//! the shared confirmation gate (`cli::prompt`).

use std::path::Path;

use crate::core::addons::manifest::AddonManifest;
use crate::core::addons::revocation::RevocationList;
use crate::core::addons::store::{ArtifactReceipt, InstalledStore};
use crate::core::addons::{artifact_install, bootstrap, install, registry};

pub fn cmd_addon(args: &[String]) {
    let action = args.first().map_or("list", String::as_str);

    match action {
        "list" | "ls" => cmd_list(),
        "init" | "new" => cmd_init(args),
        "registry" => cmd_registry(args),
        "categories" | "cats" => cmd_categories(),
        "usage" | "stats" => cmd_usage(),
        "search" | "browse" => cmd_search(args.get(1).map_or("", String::as_str)),
        "info" | "show" => match positional(args) {
            Some(name) => cmd_info(&name),
            None => usage_exit("lean-ctx addon info <name>"),
        },
        "add" | "install" => match positional(args) {
            Some(target) => cmd_add(&target, args),
            None => usage_exit("lean-ctx addon add <name|path-to-lean-ctx-addon.toml>"),
        },
        "remove" | "rm" | "uninstall" => match positional(args) {
            Some(name) => cmd_remove(&name, args),
            None => usage_exit("lean-ctx addon remove <name>"),
        },
        "update" | "upgrade" => match positional(args) {
            Some(name) => cmd_update(&name, args),
            None => usage_exit("lean-ctx addon update <name>"),
        },
        "revoke" => match positional(args) {
            Some(name) => cmd_revoke(&name, args),
            None => usage_exit("lean-ctx addon revoke <name> [--reason \"…\"] [--version X]"),
        },
        "unrevoke" => match positional(args) {
            Some(name) => cmd_unrevoke(&name, args),
            None => usage_exit("lean-ctx addon unrevoke <name>"),
        },
        "revocations" => cmd_revocations(),
        "verify" => cmd_verify(),
        "audit" => match positional(args) {
            Some(target) => cmd_audit(&target),
            None => usage_exit("lean-ctx addon audit <name|path-to-lean-ctx-addon.toml>"),
        },
        "publish" => cmd_publish(args),
        "help" | "--help" | "-h" => print_help(),
        _ => {
            eprintln!("Unknown addon action: {action}");
            print_help();
            std::process::exit(1);
        }
    }
}

/// First non-flag argument after the action.
fn positional(args: &[String]) -> Option<String> {
    args.get(1)
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty() && !s.starts_with('-'))
}

fn usage_exit(usage: &str) -> ! {
    eprintln!("Usage: {usage}");
    std::process::exit(1);
}

fn cmd_list() {
    let store = InstalledStore::load();
    let installed = store.list();

    if installed.is_empty() {
        println!("No addons installed.");
    } else {
        println!("Installed addons:\n");
        for a in &installed {
            let ver = if a.version.is_empty() {
                String::new()
            } else {
                format!(" v{}", a.version)
            };
            if let Some(reason) = crate::core::addons::revocation::blocked_reason(&a.name) {
                println!(
                    "  ⛔ {}{ver}  → REVOKED ({reason}) — will not run; remove with `addon remove {}`",
                    a.name, a.name
                );
            } else {
                println!(
                    "  ✓ {}{ver}  → gateway server `{}` ({})",
                    a.name, a.gateway_server, a.source
                );
            }
        }
    }

    let available = registry::all();
    if !available.is_empty() {
        println!("\nRegistry:\n");
        for m in &available {
            let installed_flag = if store.get(&m.addon.name).is_some() {
                " [installed]"
            } else {
                ""
            };
            let status = if m.is_installable() {
                ""
            } else {
                " · listed (no published endpoint yet)"
            };
            let badge = if m.addon.verified { " [verified]" } else { "" };
            println!(
                "  • {}{badge} — {}{status}{installed_flag}",
                m.addon.name,
                first_line(&m.addon.description)
            );
        }
    }

    println!(
        "\nAdd one with `lean-ctx addon add <name>` · build your own with `lean-ctx addon help`."
    );
}

fn cmd_search(query: &str) {
    let hits = registry::search(query);
    if hits.is_empty() {
        println!("No addons match `{query}`.");
        return;
    }
    if query.trim().is_empty() {
        println!("All registry addons:\n");
    } else {
        println!("Addons matching `{query}`:\n");
    }
    for m in &hits {
        let status = if m.is_installable() {
            "installable"
        } else {
            "listed"
        };
        let badge = if m.addon.verified { " [verified]" } else { "" };
        println!("  {}{badge} — {}", m.addon.name, m.display_name());
        println!("      {}", first_line(&m.addon.description));
        if m.addon.categories.is_empty() {
            println!("      {status}");
        } else {
            println!(
                "      categories: {} · {status}",
                m.addon.categories.join(", ")
            );
        }
    }
}

/// `addon categories` — browse the registry by category (discovery, P5). Counts
/// are computed from the live registry, so the list is always accurate.
fn cmd_categories() {
    use std::collections::BTreeMap;
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for m in registry::all() {
        for c in &m.addon.categories {
            *counts.entry(c.trim().to_string()).or_default() += 1;
        }
    }
    if counts.is_empty() {
        println!("No categories yet.");
        return;
    }
    println!("Addon categories:\n");
    for (cat, n) in &counts {
        println!("  {cat}  ({n})");
    }
    println!("\nFilter with `lean-ctx addon search <category>`.");
}

/// `addon usage` — per-addon / per-tool call counters from the local meter
/// (P5). The honest basis for "most-used" discovery and usage-metered billing.
fn cmd_usage() {
    use crate::core::addons::meter::UsageLedger;
    let ledger = UsageLedger::load();
    let ranked = ledger.by_usage();
    if ranked.is_empty() {
        println!(
            "No addon usage recorded yet. (Metering is {}.)",
            if InstalledStore::load().list().is_empty() {
                "ready once you install + use an addon"
            } else {
                "on; call an addon tool via the gateway to populate it"
            }
        );
        return;
    }
    println!("Addon usage (most-used first):\n");
    for (name, usage) in ranked {
        let revoked = if crate::core::addons::revocation::blocked_reason(name).is_some() {
            " ⛔ revoked"
        } else {
            ""
        };
        println!(
            "  {name}{revoked} — {} call(s), {} error(s)",
            usage.calls, usage.errors
        );
        let mut tools: Vec<_> = usage.tools.iter().collect();
        tools.sort_by(|a, b| b.1.calls.cmp(&a.1.calls).then_with(|| a.0.cmp(b.0)));
        for (tool, ts) in tools.iter().take(5) {
            println!("      {tool}: {} call(s), {} error(s)", ts.calls, ts.errors);
        }
    }
}

fn cmd_info(name: &str) {
    let store = InstalledStore::load();
    let Some(manifest) = registry::get(name).or_else(|| {
        // Allow `info` on a local manifest path too.
        looks_like_path(name)
            .then(|| AddonManifest::from_path(Path::new(name)).ok())
            .flatten()
    }) else {
        // Not in the registry and not a manifest path — but it may be a
        // locally-installed addon recorded in the store.
        if let Some(installed) = store.get(name) {
            println!("{}", installed.name);
            print_field("Version", &installed.version);
            println!(
                "  Status:    installed (gateway server `{}`, {})",
                installed.gateway_server, installed.source
            );
            return;
        }
        eprintln!(
            "Addon `{name}` not found. Try `lean-ctx addon search`, or pass a path to a \
             lean-ctx-addon.toml."
        );
        std::process::exit(1);
    };

    println!("{} ({})", manifest.display_name(), manifest.addon.name);
    if !manifest.addon.description.is_empty() {
        println!("  {}", manifest.addon.description);
    }
    print_field("Author", &manifest.addon.author);
    print_field("Version", &manifest.addon.version);
    print_field("License", &manifest.addon.license);
    print_field("Homepage", &manifest.addon.homepage);
    if !manifest.addon.categories.is_empty() {
        println!("  Categories: {}", manifest.addon.categories.join(", "));
    }

    if let Some(installed) = store.get(name) {
        println!(
            "  Status:    installed (gateway server `{}`, {})",
            installed.gateway_server, installed.source
        );
    } else if manifest.is_installable() {
        println!(
            "  Status:    installable — `lean-ctx addon add {}`",
            manifest.addon.name
        );
    } else {
        println!("  Status:    listed (no published MCP endpoint yet)");
    }

    if manifest.is_installable() {
        println!();
        print_install_preview(&manifest);
    }
}

fn cmd_add(target: &str, args: &[String]) {
    // Resolution order: local manifest file → hosted ctxpkg pack (`ns/slug`,
    // GH #726) → bundled registry slug. A bare `ns/slug` that exists on disk
    // is treated as the local path it names.
    let is_local_path = Path::new(target)
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("toml"))
        || target.starts_with('.')
        || target.starts_with('/')
        || Path::new(target).exists();
    let (manifest, source) = if is_local_path {
        match AddonManifest::from_path(Path::new(target)) {
            Ok(m) => (m, "local".to_string()),
            Err(e) => {
                eprintln!("Error: {e}");
                std::process::exit(1);
            }
        }
    } else if let Some(remote_ref) = crate::core::context_package::remote::parse_remote_ref(target)
    {
        match fetch_addon_pack(&remote_ref, flag_value(args, "--registry").as_deref()) {
            Ok(pair) => pair,
            Err(e) => {
                eprintln!("Error: {e}");
                std::process::exit(1);
            }
        }
    } else {
        let Some(m) = registry::get(target) else {
            eprintln!(
                "Unknown addon `{target}`.\n\
                 Browse with `lean-ctx addon search`, install a hosted pack with \
                 `lean-ctx addon add <namespace>/<name>`, or pass a path to a \
                 lean-ctx-addon.toml."
            );
            std::process::exit(1);
        };
        (m, "registry".to_string())
    };

    if let Err(e) = manifest.validate() {
        eprintln!("Error: {e}");
        std::process::exit(1);
    }

    if !manifest.is_installable() {
        eprintln!(
            "`{name}` is listed but not yet one-click installable (no published MCP endpoint).\n\
             Follow {home} — once it ships an MCP server, `lean-ctx addon add {name}` will \
             wire it automatically.",
            name = manifest.addon.name,
            home = if manifest.addon.homepage.is_empty() {
                "its homepage"
            } else {
                &manifest.addon.homepage
            }
        );
        std::process::exit(1);
    }

    let force = args.iter().any(|a| a == "--force" || a == "-f");
    let no_verify = args.iter().any(|a| a == "--no-verify");
    let cfg = crate::core::config::Config::load();

    // Fail fast (#1080): run the full pre-persist gate — policy, kill-switch,
    // capability coherence — before rendering the preview or spawning a probe,
    // so a rejected addon surfaces a clear verdict and nothing is touched.
    // (The health probe later targets the post-artifact wiring instead of
    // this resolution, so only the verdict matters here.)
    if let Err(e) = install::preflight(&manifest, &cfg.addons, force) {
        eprintln!("Error: {e}");
        std::process::exit(1);
    }

    println!("About to install `{}`:\n", manifest.addon.name);
    print_install_preview(&manifest);
    println!(
        "\nThis runs/connects to the above MCP server and exposes its tools through lean-ctx."
    );

    if !super::prompt::confirm(
        "Install this addon into the MCP gateway?",
        super::prompt::wants_yes(args),
    ) {
        println!("Aborted. Nothing was changed.");
        return;
    }

    match provision_and_wire(manifest, &source, force, no_verify, &cfg) {
        Ok((outcome, verified)) => {
            println!(
                "\n✓ Installed `{}` → gateway server `{}`.",
                outcome.name, outcome.gateway_server
            );
            if outcome.enabled_gateway {
                println!("  Enabled the MCP gateway (gateway.enabled = true).");
            }
            if let Some(n) = verified {
                println!("  Verified: {n} tool(s) reachable.");
            }
            println!(
                "  Its tools are reachable via `ctx_tools` (find/call). \
                 Restart your MCP client to pick them up."
            );
        }
        Err(e) => {
            eprintln!("Error: {e}");
            std::process::exit(1);
        }
    }
}

/// The impure provisioning pipeline `add` and `update` share, run after user
/// consent: managed artifact (GH #725) → bootstrap (#1105) → health probe
/// (#1076) → wire. On any error nothing is wired. Returns the install outcome
/// plus the probed tool count (`None` with `--no-verify`).
fn provision_and_wire(
    mut manifest: AddonManifest,
    source: &str,
    force: bool,
    no_verify: bool,
    cfg: &crate::core::config::Config,
) -> Result<(install::InstallOutcome, Option<usize>), String> {
    // Managed artifact (GH #725, Phase 1): a prebuilt binary for this platform
    // takes precedence over [install]/PATH. It lands in the managed bin dir
    // (never PATH), hash-verified; the gateway command is rewritten to the
    // absolute path and the SHA-256 auto-pinned as the spawn-time binhash.
    let mut artifact_receipt: Option<ArtifactReceipt> = None;
    if let Some(asset) = manifest.artifact_for_current_platform().cloned() {
        let triple = artifact_install::current_target_triple();
        println!("\nInstalling prebuilt binary for {triple} (sha256-pinned)…");
        let path = artifact_install::ensure_addon_binary(
            &manifest.addon.name,
            &manifest.addon.version,
            &asset,
        )
        .map_err(|e| format!("artifact install failed: {e}\n  Nothing was wired."))?;
        println!("  ✓ {}", path.display());
        artifact_receipt = Some(ArtifactReceipt {
            platform: triple.to_string(),
            url: asset.url.clone(),
            sha256: asset.sha256.clone(),
            path: path.display().to_string(),
        });
        manifest.mcp.command = path.display().to_string();
        manifest.mcp.sha256 = asset.sha256;
    } else if manifest.install.is_declared() {
        // Bootstrap (#1105): provision the upstream package via its pinned
        // manager *before* probing — the [mcp] command depends on it. The
        // policy floor (addons.allow_bootstrap) was already enforced in
        // preflight. Skipped when a managed artifact resolved above (the
        // artifact IS the binary the bootstrap would have provisioned).
        println!(
            "\nInstalling `{}` via {} (pinned {})…",
            manifest.install.package.trim(),
            manifest.install.manager.trim(),
            manifest.install.version.trim()
        );
        let outcome = bootstrap::ensure_installed(&manifest.install)
            .map_err(|e| format!("bootstrap install failed: {e}\n  Nothing was wired."))?;
        match outcome.status {
            bootstrap::BootstrapStatus::AlreadyPresent => {
                println!("  Already installed — skipped.");
            }
            bootstrap::BootstrapStatus::Installed => println!("  ✓ Installed."),
        }
        if let Some(warning) = outcome.warning {
            eprintln!("  ⚠ {warning}");
        }
    }

    // Health probe (#1076): confirm the server actually speaks MCP *before* we
    // wire it, so a broken command/args fails now with a clear message instead
    // of opaquely at first `ctx_tools` use. Skip with `--no-verify`. Probes the
    // post-artifact wiring, i.e. exactly what the gateway will spawn.
    let server = manifest.to_gateway_server();
    let mut verified: Option<usize> = None;
    if !no_verify {
        // First spawn may download a package (npx/uvx), so allow extra headroom
        // over the per-call timeout.
        let timeout = std::time::Duration::from_secs(cfg.gateway.call_timeout_secs.max(60));
        print!("Verifying the MCP server responds… ");
        let _ = std::io::Write::flush(&mut std::io::stdout());
        match crate::core::addons::health::probe(&server, timeout) {
            Ok(report) => {
                println!("ok ({} tool(s)).", report.tool_count);
                verified = Some(report.tool_count);
            }
            Err(e) => {
                println!("failed.");
                return Err(format!(
                    "`{}` did not pass its health check: {e}\n  \
                     Nothing was installed. Check the command/args (and capabilities), then retry \
                     — or skip the check with `--no-verify`.",
                    manifest.addon.name
                ));
            }
        }
    }

    let outcome = install::install(&manifest, source, force, artifact_receipt)?;
    Ok((outcome, verified))
}

/// Resolve `ns/slug[@version]` against the hosted ctxpkg registry and unwrap
/// the `kind=addon` pack into the addon manifest it embeds (GH #726).
///
/// Trust chain before anything is returned: artifact SHA-256 against the
/// registry index (in `download_verified`), then full pack verification —
/// integrity hashes, **mandatory** ed25519 signature (packs carrying
/// executable references get no unsigned path), kind=addon and
/// kind↔payload coherence. The embedded TOML then walks the exact same
/// consent/preflight/probe pipeline as every other source.
fn fetch_addon_pack(
    remote_ref: &crate::core::context_package::remote::RemoteRef,
    registry_flag: Option<&str>,
) -> Result<(AddonManifest, String), String> {
    use crate::core::context_package::{remote, verify};

    let base = remote::registry_base(registry_flag);
    let ns = &remote_ref.namespace;
    let name = &remote_ref.name;
    let token = remote::publish_token(None);

    println!("Resolving @{ns}/{name} via {base} …");
    let versions = remote::fetch_versions(&base, ns, name, token.as_deref())?;
    let info = remote::select_version(&versions, remote_ref.version.as_deref())?;
    if info.yanked {
        eprintln!(
            "WARNING: @{ns}/{name}@{} is YANKED — installing only because the version \
             was pinned explicitly",
            info.version
        );
    }
    let bytes = remote::download_verified(&base, ns, name, info, token.as_deref())?;
    let text = String::from_utf8(bytes).map_err(|_| "package is not valid UTF-8".to_string())?;

    let report = verify::verify_package_text(&text);
    if !report.valid() {
        return Err(format!(
            "pack verification failed — refusing to install:\n  {}",
            report.errors.join("\n  ")
        ));
    }
    if report.signature != verify::CheckOutcome::Pass {
        return Err(
            "pack is unsigned — addon packs reference executables, so a verifying \
             ed25519 signature is mandatory"
                .into(),
        );
    }

    #[derive(serde::Deserialize)]
    struct Bundle {
        manifest: crate::core::context_package::PackageManifest,
        content: crate::core::context_package::PackageContent,
    }
    let bundle: Bundle = serde_json::from_str(&text).map_err(|e| format!("parse package: {e}"))?;

    if bundle.manifest.kind != crate::core::context_package::manifest::PackageKind::Addon {
        return Err(format!(
            "@{ns}/{name} is a kind={} package — install it with `lean-ctx pack install \
             {ns}/{name}` instead",
            bundle.manifest.kind.as_str()
        ));
    }
    verify::validate_kind_coherence(&bundle.manifest, &bundle.content)
        .map_err(|errs| errs.join("; "))?;

    let payload = bundle
        .content
        .addon
        .expect("coherence guarantees content.addon for kind=addon");
    let manifest = AddonManifest::from_toml(&payload.manifest_toml)?;

    let source = format!("ctxpkg:@{ns}/{name}@{}", info.version);
    Ok((manifest, source))
}

/// `addon publish [manifest] --namespace <ns>` — build the signed
/// `kind=addon` pack from a `lean-ctx-addon.toml` and upload it to the
/// hosted ctxpkg registry (GH #726). `--check` runs every local gate
/// (schema, audit, signing, self-verification) and stops before the network.
fn cmd_publish(args: &[String]) {
    let manifest_path = args
        .iter()
        .skip(1)
        .find(|a| {
            !a.starts_with('-')
                && Path::new(a.as_str())
                    .extension()
                    .is_some_and(|ext| ext.eq_ignore_ascii_case("toml"))
        })
        .map_or_else(|| "lean-ctx-addon.toml".to_string(), String::clone);

    let Some(namespace) = flag_value(args, "--namespace") else {
        eprintln!(
            "Usage: lean-ctx addon publish [lean-ctx-addon.toml] --namespace <ns> \
             [--check] [--registry <url>] [--token <ctxp_…>]"
        );
        eprintln!();
        eprintln!("The namespace is your ctxpkg.com account handle — the pack publishes");
        eprintln!("as @<ns>/<addon-name>. `--check` validates and signs locally without");
        eprintln!("uploading anything.");
        std::process::exit(1);
    };

    let plan =
        match crate::core::addons::publish::build_addon_pack(Path::new(&manifest_path), &namespace)
        {
            Ok(p) => p,
            Err(e) => {
                eprintln!("Error: {e}");
                std::process::exit(1);
            }
        };

    println!(
        "Built @{}/{}@{} (kind=addon, {} bytes)",
        plan.namespace,
        plan.slug,
        plan.version,
        plan.bundle_json.len()
    );
    println!("  Audit verdict: {}", plan.audit.verdict.as_str());
    for f in &plan.audit.findings {
        println!("    {} {} — {}", f.level.as_str(), f.code, f.message);
    }
    if plan.artifact_platforms.is_empty() {
        println!("  Artifacts: none (installs use the runner/[install] path)");
    } else {
        println!("  Artifacts: {}", plan.artifact_platforms.join(", "));
    }
    if plan.has_bootstrap {
        println!("  Bootstrap: [install] fallback for platforms without an artifact");
    }

    if args.iter().any(|a| a == "--check") {
        println!("\n--check: all local gates passed — nothing was uploaded.");
        return;
    }

    use crate::core::context_package::remote;
    let base = remote::registry_base(flag_value(args, "--registry").as_deref());
    let Some(token) = remote::publish_token(flag_value(args, "--token").as_deref()) else {
        eprintln!("ERROR: no publish token — pass --token or set CTXPKG_TOKEN");
        eprintln!("Mint one at ctxpkg.com/account (sign in, then Tokens → Mint).");
        std::process::exit(1);
    };
    if token.starts_with("ctxr_") {
        eprintln!(
            "ERROR: this is a read-only install token (ctxr_) — publishing needs a ctxp_ token"
        );
        std::process::exit(1);
    }

    println!(
        "\nPublishing @{}/{}@{} to {base} …",
        plan.namespace, plan.slug, plan.version
    );
    match remote::publish(
        &base,
        &token,
        &plan.namespace,
        &plan.slug,
        &plan.version,
        plan.bundle_json.as_bytes(),
    ) {
        Ok(receipt) => {
            println!("Published: {}", receipt.published);
            println!("Artifact SHA-256: {}", receipt.artifact_sha256);
            println!(
                "Install with: lean-ctx addon add {}/{}",
                plan.namespace, plan.slug
            );
        }
        Err(e) => {
            eprintln!("ERROR: {e}");
            std::process::exit(1);
        }
    }
}

/// `addon update <name>` — re-resolve the registry entry and reinstall when it
/// changed (GH #725). Managed binaries install side-by-side into a new version
/// dir; only after the health probe passes is the gateway pointer flipped and
/// the old version pruned — a failed update leaves the working install intact.
fn cmd_update(name: &str, args: &[String]) {
    let Some(entry) = InstalledStore::load().get(name).cloned() else {
        eprintln!("Addon `{name}` is not installed.");
        std::process::exit(1);
    };
    if entry.source == "local" {
        eprintln!(
            "`{name}` was installed from a local manifest — update it by re-running \
             `lean-ctx addon add <path-to-lean-ctx-addon.toml>`."
        );
        std::process::exit(1);
    }
    // Re-resolve from where it came: a hosted ctxpkg pack updates against the
    // registry it was installed from (latest non-yanked version), everything
    // else against the bundled registry snapshot.
    let (manifest, update_source) = if let Some(spec) = entry.source.strip_prefix("ctxpkg:") {
        let unpinned = spec.split('@').take(2).collect::<Vec<_>>().join("@");
        let Some(remote_ref) = crate::core::context_package::remote::parse_remote_ref(&unpinned)
        else {
            eprintln!(
                "`{name}` has a malformed install source `{}`.",
                entry.source
            );
            std::process::exit(1);
        };
        match fetch_addon_pack(&remote_ref, flag_value(args, "--registry").as_deref()) {
            Ok(pair) => pair,
            Err(e) => {
                eprintln!("Error: {e}");
                std::process::exit(1);
            }
        }
    } else {
        let Some(m) = registry::get(name) else {
            eprintln!(
                "`{name}` is no longer in the registry — remove it or reinstall from a path."
            );
            std::process::exit(1);
        };
        (m, entry.source.clone())
    };

    let force = args.iter().any(|a| a == "--force" || a == "-f");
    let no_verify = args.iter().any(|a| a == "--no-verify");

    // Up-to-date check: same version and (for managed binaries) same artifact
    // pin ⇒ nothing to do. `--force` reinstalls anyway.
    let same_version = manifest.addon.version == entry.version;
    let same_artifact = match (
        manifest.artifact_for_current_platform(),
        entry.artifact.as_ref(),
    ) {
        (Some(asset), Some(receipt)) => asset.sha256.eq_ignore_ascii_case(&receipt.sha256),
        (None, None) => true,
        _ => false,
    };
    if same_version && same_artifact && !force {
        println!(
            "`{name}` is up to date (v{}).",
            if entry.version.is_empty() {
                "unversioned".to_string()
            } else {
                entry.version.clone()
            }
        );
        return;
    }

    let cfg = crate::core::config::Config::load();
    if let Err(e) = install::preflight(&manifest, &cfg.addons, force) {
        eprintln!("Error: {e}");
        std::process::exit(1);
    }

    println!(
        "Updating `{name}`: v{} → v{}",
        entry.version, manifest.addon.version
    );
    if !super::prompt::confirm("Proceed with the update?", super::prompt::wants_yes(args)) {
        println!("Aborted. Nothing was changed.");
        return;
    }

    let new_version = manifest.addon.version.clone();
    match provision_and_wire(manifest, &update_source, force, no_verify, &cfg) {
        Ok((outcome, verified)) => {
            // The new version is wired and healthy — now prune superseded
            // managed binaries (side-by-side rollback safety until here).
            artifact_install::prune_other_versions(name, &new_version);
            println!(
                "\n✓ Updated `{}` to v{new_version} (gateway server `{}`).",
                outcome.name, outcome.gateway_server
            );
            if let Some(n) = verified {
                println!("  Verified: {n} tool(s) reachable.");
            }
            println!("  Restart your MCP client to pick up the new version.");
        }
        Err(e) => {
            eprintln!("Error: {e}\n  The previous install remains wired.");
            std::process::exit(1);
        }
    }
}

fn cmd_remove(name: &str, args: &[String]) {
    let Some(entry) = InstalledStore::load().get(name).cloned() else {
        eprintln!("Addon `{name}` is not installed.");
        std::process::exit(1);
    };

    if !super::prompt::confirm(
        &format!("Remove addon `{name}` (unwire its MCP server)?"),
        super::prompt::wants_yes(args),
    ) {
        println!("Aborted.");
        return;
    }

    match install::remove(name) {
        Ok(outcome) => {
            println!(
                "✓ Removed `{}` (gateway server `{}`).",
                outcome.name, outcome.gateway_server
            );
            // Uninstall the bootstrapped package (#1105), best-effort — a failed
            // uninstall must never block the unwire that already succeeded.
            if let Some(receipt) = entry.install {
                println!(
                    "Uninstalling `{}` via {}…",
                    receipt.package, receipt.manager
                );
                match bootstrap::uninstall(&receipt) {
                    Ok(()) => println!("  ✓ Uninstalled."),
                    Err(e) => eprintln!(
                        "  Note: could not uninstall `{}` automatically: {e}\n  \
                         Remove it manually if you no longer need it.",
                        receipt.package
                    ),
                }
            }
            // Delete managed binaries (GH #725), best-effort for the same reason.
            if entry.artifact.is_some() && artifact_install::remove_managed_binaries(name) {
                println!("  ✓ Deleted managed binaries.");
            }
            if outcome.last_removed {
                println!(
                    "  No addons remain. The gateway stays enabled — disable it with \
                     `lean-ctx config set gateway.enabled false` if you no longer need it."
                );
            }
        }
        Err(e) => {
            eprintln!("Error: {e}");
            std::process::exit(1);
        }
    }
}

/// `addon revoke <name>` — block an addon from running everywhere (install,
/// catalog, every proxy call). Protective, so it does not prompt.
fn cmd_revoke(name: &str, args: &[String]) {
    let reason = flag_value(args, "--reason").unwrap_or_else(|| "manually revoked".to_string());
    let version = flag_value(args, "--version");

    let mut list = RevocationList::load();
    list.revoke(name, &reason, version.clone());
    match list.save() {
        Ok(()) => {
            let scope =
                version.map_or_else(|| "all versions".to_string(), |v| format!("version {v}"));
            println!("✓ Revoked `{name}` ({scope}): {reason}");
            println!(
                "  It will no longer run via the gateway (its tools disappear from `ctx_tools`)."
            );
            if InstalledStore::load().get(name).is_some() {
                println!("  It is still installed — `lean-ctx addon remove {name}` to unwire it.");
            }
            crate::core::gateway::catalog::invalidate();
        }
        Err(e) => {
            eprintln!("Error: {e}");
            std::process::exit(1);
        }
    }
}

/// `addon unrevoke <name>` — lift a revocation (removes protection), so confirm.
fn cmd_unrevoke(name: &str, args: &[String]) {
    let mut list = RevocationList::load();
    if !list.revocations.contains_key(name) {
        eprintln!("Addon `{name}` is not revoked.");
        std::process::exit(1);
    }
    if !super::prompt::confirm(
        &format!("Lift the revocation on `{name}` (allow it to run again)?"),
        super::prompt::wants_yes(args),
    ) {
        println!("Aborted.");
        return;
    }
    list.unrevoke(name);
    match list.save() {
        Ok(()) => {
            println!("✓ Lifted revocation on `{name}`.");
            crate::core::gateway::catalog::invalidate();
        }
        Err(e) => {
            eprintln!("Error: {e}");
            std::process::exit(1);
        }
    }
}

/// `addon revocations` — list the active local revocations.
fn cmd_revocations() {
    let list = RevocationList::load();
    if list.revocations.is_empty() {
        println!("No revocations.");
        return;
    }
    println!("Revoked addons:\n");
    for (name, rev) in &list.revocations {
        let scope = rev
            .version
            .as_deref()
            .map(|v| format!(" (version {v})"))
            .unwrap_or_default();
        println!("  ⛔ {name}{scope} — {}", rev.reason);
    }
}

/// `addon verify` — re-check each installed addon's live wiring against the
/// integrity hash pinned at install (P2). Exits non-zero if any addon drifted.
fn cmd_verify() {
    use crate::core::addons::integrity::{self, IntegrityStatus};
    let findings = integrity::verify_all();
    if findings.is_empty() {
        println!("No addons installed.");
        return;
    }
    let mut drift = false;
    println!("Addon integrity:\n");
    for f in &findings {
        let glyph = match f.status {
            IntegrityStatus::Ok => "✓",
            IntegrityStatus::Drift => {
                drift = true;
                "⛔"
            }
            IntegrityStatus::Missing | IntegrityStatus::Unpinned => "•",
        };
        println!("  {glyph} {} — {}", f.name, f.status.label());
    }
    if drift {
        eprintln!(
            "\nOne or more addons no longer match their pinned wiring. Review the \
             `[[gateway.servers]]` entries, then re-install (`addon add`) or remove them."
        );
        std::process::exit(1);
    }
}

/// `addon init [name]` — scaffold a ready-to-edit `lean-ctx-addon.toml` in the
/// current directory. `--http` for an HTTP addon, `--force` to overwrite.
fn cmd_init(args: &[String]) {
    use crate::core::addons::scaffold;
    use crate::core::gateway::TransportKind;

    let transport = if args.iter().any(|a| a == "--http") {
        TransportKind::Http
    } else {
        TransportKind::Stdio
    };
    let force = args.iter().any(|a| a == "--force" || a == "-f");

    // `--command "npx -y pkg@1.2.3"` (stdio only): wire a real command and let
    // the scaffold pick capabilities that actually let it run (GH #1079).
    let command: Option<Vec<String>> = (transport == TransportKind::Stdio)
        .then(|| flag_value(args, "--command"))
        .flatten()
        .map(|spec| spec.split_whitespace().map(str::to_string).collect());

    // Slug: explicit positional, else the current directory name.
    let slug = positional(args).or_else(|| {
        std::env::current_dir()
            .ok()
            .and_then(|d| d.file_name().map(|n| n.to_string_lossy().into_owned()))
            .and_then(|n| scaffold::slugify(&n))
    });
    let Some(raw) = slug else {
        eprintln!("Could not derive an addon name. Pass one: `lean-ctx addon init my-addon`.");
        std::process::exit(1);
    };
    let Some(slug) = scaffold::slugify(&raw) else {
        eprintln!("`{raw}` has no usable slug characters ([a-z0-9-]).");
        std::process::exit(1);
    };

    let path = Path::new(scaffold::MANIFEST_FILENAME);
    if path.exists() && !force {
        eprintln!(
            "{} already exists. Re-run with --force to overwrite.",
            scaffold::MANIFEST_FILENAME
        );
        std::process::exit(1);
    }

    let contents = scaffold::addon_manifest(&slug, transport, command.as_deref());
    if let Err(e) = std::fs::write(path, contents) {
        eprintln!("Error writing {}: {e}", scaffold::MANIFEST_FILENAME);
        std::process::exit(1);
    }

    println!("✓ Wrote {} (addon `{slug}`).", scaffold::MANIFEST_FILENAME);
    println!("\nNext:");
    println!("  1. Edit the manifest — fill in description/author/homepage.");
    println!(
        "  2. Audit it:    lean-ctx addon audit ./{}",
        scaffold::MANIFEST_FILENAME
    );
    println!(
        "  3. Test live:   lean-ctx addon add ./{}",
        scaffold::MANIFEST_FILENAME
    );
    println!("  4. Get listed:  see docs/guides/addons.md");
}

/// `addon registry validate [path]` — run the registry security/quality bar
/// (#864 + #403) against a registry JSON file, or the bundled + local registry
/// if no path is given. The dry-run harness an author / CI uses before opening a
/// merge request. Non-zero exit when problems are found.
fn cmd_registry(args: &[String]) {
    let sub = args.get(1).map_or("", String::as_str);
    if sub != "validate" {
        eprintln!("Usage: lean-ctx addon registry validate [path-to-registry.json]");
        std::process::exit(1);
    }

    let (entries, label) = match args.get(2).map(String::as_str) {
        Some(path) if !path.starts_with('-') => match load_registry_file(path) {
            Ok(e) => (e, path.to_string()),
            Err(e) => {
                eprintln!("Error: {e}");
                std::process::exit(1);
            }
        },
        _ => (
            registry::all(),
            "installed registry (bundled + local)".to_string(),
        ),
    };

    let problems = registry::validate_entries(&entries);
    if problems.is_empty() {
        println!(
            "✓ {label}: {} entr{} pass the security + quality bar.",
            entries.len(),
            if entries.len() == 1 { "y" } else { "ies" }
        );
        return;
    }
    eprintln!("✗ {label}: {} problem(s):\n", problems.len());
    for p in &problems {
        eprintln!("  • {p}");
    }
    std::process::exit(1);
}

/// Parse a registry JSON file (`{ "addons": [ … ] }`) into manifests.
fn load_registry_file(path: &str) -> Result<Vec<AddonManifest>, String> {
    let raw = std::fs::read_to_string(path).map_err(|e| format!("cannot read {path}: {e}"))?;
    #[derive(serde::Deserialize)]
    struct RegistryFile {
        #[serde(default)]
        addons: Vec<AddonManifest>,
    }
    serde_json::from_str::<RegistryFile>(&raw)
        .map(|f| f.addons)
        .map_err(|e| format!("{path} is not a valid registry file: {e}"))
}

/// `addon audit <name|path>` — run the publish/list gate (#403): wiring risk +
/// capability coherence + malware heuristics, then the verified/paid verdict.
/// Exits non-zero on a `fail` verdict so it is usable in CI / a publish hook.
fn cmd_audit(target: &str) {
    let manifest = if looks_like_path(target) {
        match AddonManifest::from_path(Path::new(target)) {
            Ok(m) => m,
            Err(e) => {
                eprintln!("Error: {e}");
                std::process::exit(1);
            }
        }
    } else {
        let Some(m) = registry::get(target) else {
            eprintln!("Unknown addon `{target}`. Pass a name from the registry or a path.");
            std::process::exit(1);
        };
        m
    };

    let report = crate::core::addons::audit::audit(&manifest);
    println!("Audit of `{}`:\n", manifest.addon.name);
    println!("  verdict:        {}", report.verdict.as_str());
    println!(
        "  capabilities:   {}",
        if manifest.capabilities.is_some() {
            if report.capability_coherent {
                "declared + coherent with wiring"
            } else {
                "declared but INCOHERENT with wiring"
            }
        } else {
            "not declared"
        }
    );
    println!(
        "  binary pin:     {}",
        if manifest.mcp.transport == crate::core::gateway::TransportKind::Http {
            "n/a (http transport)"
        } else if report.binary_pinned {
            "pinned (sha256)"
        } else {
            "unpinned"
        }
    );
    println!(
        "  paid-eligible:  {} (verified/paid tier requires a clean audit, declared + coherent \
         capabilities, and a pinned binary)",
        if report.paid_eligible { "yes" } else { "no" }
    );

    // Track B: when the manifest carries `[pricing]`, show whether it clears the
    // mandatory paid-listing gate and, if not, exactly what blocks the sale.
    if let Some(pricing) = &manifest.pricing
        && pricing.is_paid()
    {
        let price = match pricing.model {
            crate::core::addons::PricingModel::OneTime => {
                format!(
                    "{} {} one-time",
                    pricing.price_cents,
                    pricing.currency_or_default()
                )
            }
            crate::core::addons::PricingModel::Usage => format!(
                "{} {}/1k tool calls (usage)",
                pricing.usage_price_per_1k_cents,
                pricing.currency_or_default()
            ),
        };
        println!("  pricing:        {price}");
        let gate = crate::core::addons::paid_listing_gate(&manifest, &report);
        if gate.eligible {
            println!("  paid listing:   ELIGIBLE — clears the security gate");
        } else {
            println!("  paid listing:   BLOCKED");
            for blocker in &gate.blockers {
                println!("                    - {blocker}");
            }
        }
    }

    if report.findings.is_empty() {
        println!("\n  No findings.");
    } else {
        println!("\n  Findings:");
        for f in &report.findings {
            println!(
                "    {} [{}] {} ({})",
                f.level.glyph(),
                f.level.as_str(),
                f.message,
                f.code
            );
        }
    }

    if report.verdict == crate::core::addons::AuditVerdict::Fail {
        eprintln!(
            "\nAudit failed — this addon must not be listed until the blocking findings are resolved."
        );
        std::process::exit(1);
    }
}

/// Read the value following `flag` in `args` (e.g. `--reason "text"`).
fn flag_value(args: &[String], flag: &str) -> Option<String> {
    args.iter()
        .position(|a| a == flag)
        .and_then(|i| args.get(i + 1))
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn print_install_preview(manifest: &AddonManifest) {
    let mcp = &manifest.mcp;
    println!(
        "  trust:     {}",
        crate::core::addons::TrustTier::of(manifest).label()
    );
    println!("  transport: {}", mcp.transport.as_str());
    match mcp.transport {
        crate::core::gateway::TransportKind::Stdio => {
            println!("  command:   {}", mcp.command);
            if !mcp.args.is_empty() {
                println!("  args:      {}", mcp.args.join(" "));
            }
            if !mcp.env.is_empty() {
                let keys: Vec<&str> = mcp.env.keys().map(String::as_str).collect();
                println!("  env:       {}", keys.join(", "));
            }
            if !mcp.sha256.trim().is_empty() {
                println!("  binary:    sha256-pinned");
            }
            if let Some(asset) = manifest.artifact_for_current_platform() {
                println!(
                    "  artifact:  {} → managed bin dir (sha256-pinned, never PATH)",
                    asset.filename
                );
            }
        }
        crate::core::gateway::TransportKind::Http => {
            println!("  url:       {}", mcp.url);
            if !mcp.headers.is_empty() {
                let keys: Vec<&str> = mcp.headers.keys().map(String::as_str).collect();
                println!("  headers:   {}", keys.join(", "));
            }
        }
    }
    print_bootstrap(manifest);
    print_capabilities(manifest);
    print_security_review(manifest);
}

/// Disclose the bootstrap install a `[install]` block performs on `add` (#1105):
/// the exact, shell-free package-manager commands the user is consenting to.
fn print_bootstrap(manifest: &AddonManifest) {
    let install = &manifest.install;
    if !install.is_declared() {
        return;
    }
    // A managed artifact for this platform supersedes the bootstrap (GH #725) —
    // say so instead of describing an install that will not run.
    if manifest.artifact_for_current_platform().is_some() {
        println!(
            "\n  Install on add: skipped — the prebuilt artifact above is used \
             instead of `{}`.",
            install.manager.trim()
        );
        return;
    }
    let prog = install
        .manager()
        .map_or_else(|| install.manager.trim().to_string(), |m| m.as_str().into());
    println!("\n  Install on add — runs a pinned package manager before first use:");
    println!("    manager:   {}", install.manager.trim());
    println!(
        "    package:   {} (pinned {})",
        install.package.trim(),
        install.version.trim()
    );
    println!("    install:   {prog} {}", install.install_argv().join(" "));
    println!(
        "    uninstall: {prog} {}   (run on `addon remove`)",
        install.uninstall_argv().join(" ")
    );
    // Pre-flight: tell the user up front whether the manager is even present, so
    // a missing toolchain is visible before they consent rather than mid-install.
    if let Some(m) = install.manager() {
        if m.is_available() {
            println!("    requires:  `{prog}` on PATH — ✓ found");
        } else {
            println!(
                "    requires:  `{prog}` on PATH — ✗ NOT found ({})",
                m.install_hint()
            );
        }
    }
}

/// Show the declared capabilities the user is about to grant (P1). A declared
/// `[capabilities]` block means the addon runs under a per-addon OS sandbox +
/// scrubbed environment derived from exactly these permissions; an addon with
/// no block runs under the legacy `addons.sandbox` mode.
fn print_capabilities(manifest: &AddonManifest) {
    match &manifest.capabilities {
        Some(caps) => {
            println!(
                "\n  Capabilities — network/filesystem/env enforced (sandbox + scrub, \
                 inherited by children); exec declared + audited:"
            );
            for line in caps.summary() {
                println!("    • {line}");
            }
        }
        None => {
            if manifest.mcp.transport == crate::core::gateway::TransportKind::Stdio {
                println!(
                    "\n  Capabilities: none declared — governed by `addons.sandbox` \
                     (set a [capabilities] block for a per-addon sandbox)."
                );
            }
        }
    }
}

/// Static risk review shown before install — disclosure, not a verdict (the
/// install policy gate enforces; see [`crate::core::addons::policy`]). Sourced
/// from the full audit (#403) so wiring risk, capability-coherence and malware
/// heuristics all surface before the user consents.
fn print_security_review(manifest: &AddonManifest) {
    let findings = crate::core::addons::audit::audit(manifest).findings;
    if findings.is_empty() {
        return;
    }
    println!("\n  Security review:");
    for f in &findings {
        println!(
            "    {} [{}] {}",
            f.level.glyph(),
            f.level.as_str(),
            f.message
        );
    }
}

fn print_field(label: &str, value: &str) {
    if !value.trim().is_empty() {
        println!(
            "  {label}:{}{value}",
            " ".repeat(11usize.saturating_sub(label.len() + 1))
        );
    }
}

fn looks_like_path(target: &str) -> bool {
    Path::new(target)
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("toml"))
        || target.contains('/')
        || target.starts_with('.')
        || Path::new(target).is_file()
}

fn first_line(s: &str) -> String {
    let line = s.lines().next().unwrap_or("").trim();
    if line.chars().count() > 88 {
        let cut: String = line.chars().take(87).collect();
        format!("{cut}…")
    } else {
        line.to_string()
    }
}

fn print_help() {
    eprintln!(
        "lean-ctx addon — community extensions (MCP servers) for lean-ctx\n\
         \n\
         USAGE:\n    \
             lean-ctx addon <action> [args]\n\
         \n\
         ACTIONS:\n    \
             list                 List installed addons + the registry\n    \
             init [name]          Scaffold a lean-ctx-addon.toml here\n                         \
                                  [--http] [--force]\n                         \
                                  [--command \"npx -y pkg@1.2.3\"]\n    \
             search [query]       Search the registry (empty = list all)\n    \
             categories           Browse the registry by category\n    \
             usage                Per-addon / per-tool call counters\n    \
             info <name|path>     Show an addon's details + MCP wiring\n    \
             add <name|path>      Install from the registry, a hosted pack\n                         \
                                  (<namespace>/<name>, ctxpkg.com) or a local\n                         \
                                  lean-ctx-addon.toml (asks for confirmation)\n    \
             update <name>        Update an addon from where it came (side-by-\n                         \
                                  side managed binary, health-gated, auto-prune)\n    \
             publish [manifest]   Build + sign the kind=addon pack and upload\n                         \
                                  it to ctxpkg.com --namespace <ns> [--check]\n    \
             remove <name>        Uninstall an addon\n    \
             revoke <name>        Block an addon from running (kill-switch)\n                         \
                                  [--reason \"…\"] [--version X]\n    \
             unrevoke <name>      Lift a revocation\n    \
             revocations          List active revocations\n    \
             verify               Re-check installed addons against their\n                         \
                                  pinned wiring (integrity lock)\n    \
             audit <name|path>    Run the publish/list gate: wiring risk +\n                         \
                                  capability coherence + malware heuristics\n    \
             registry validate [path]\n                         \
                                  Validate a registry file (or the installed\n                         \
                                  registry) against the security + quality bar\n    \
             help                 Show this help\n\
         \n\
         FLAGS:\n    \
             -y, --yes            Skip the confirmation prompt (scripts/CI)\n    \
             --no-verify          add: skip the post-install MCP health probe\n    \
             --force, -f          add: install despite an under-declared\n                         \
                                  capability warning (init: overwrite)\n\
         \n\
         BUILD YOUR OWN ADDON:\n    \
             1. Expose your tool as an MCP server (stdio binary or HTTP endpoint).\n    \
             2. Add a lean-ctx-addon.toml to your repo:\n\
         \n        \
                 [addon]\n        \
                 name = \"my-addon\"            # slug: [a-z0-9-]\n        \
                 display_name = \"My Addon\"\n        \
                 description = \"What it does, in one line.\"\n        \
                 author = \"you\"\n        \
                 homepage = \"https://github.com/you/my-addon\"\n        \
                 license = \"Apache-2.0\"\n        \
                 categories = [\"workflow\"]\n        \
                 keywords = [\"...\"]\n\
         \n        \
                 [mcp]\n        \
                 transport = \"stdio\"          # or \"http\"\n        \
                 command = \"my-addon-mcp\"     # stdio: executable to spawn\n        \
                 args = [\"serve\"]\n        \
                 # sha256 = \"<shasum -a 256>\"  # stdio: pin the binary (P3)\n        \
                 # url = \"https://...\"         # http: streamable endpoint\n\
         \n        \
                 [capabilities]               # secure-by-default; widen only what you need\n        \
                 network = \"none\"             # \"full\" to reach the internet\n        \
                 filesystem = \"read_only\"     # \"read_write\" to write outside tmp\n        \
                 exec = \"none\"                # or [\"lean-ctx\"] if you spawn subprocesses\n\
         \n    \
             3. Test it live:  lean-ctx addon add ./lean-ctx-addon.toml\n    \
             4. Publish:       lean-ctx addon publish --namespace <your-handle>\n                      \
                               — self-service via ctxpkg.com; users install with\n                      \
                               `lean-ctx addon add <your-handle>/my-addon`.\n                      \
                               (Curated default catalog: MR against\n                      \
                               rust/data/addon_registry.json, docs/guides/addons.md.)\n\
         \n    \
             Full guide: docs/guides/addons.md"
    );
}
