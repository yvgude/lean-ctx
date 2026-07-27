use super::{
    AddonManifest, ArtifactReceipt, InstalledStore, Path, addon_self_ref, artifact_install,
    bootstrap, cmd_audit, cmd_init, cmd_publish, cmd_registry, cmd_remove, cmd_revocations,
    cmd_revoke, cmd_unrevoke, cmd_update, cmd_verify, first_line, flag_value, install,
    install_declared_deps, looks_like_path, print_field, print_help, print_install_preview,
    registry, resolve_declared_deps,
};

pub fn cmd_addon(args: &[String]) -> i32 {
    let action = args.first().map_or("list", String::as_str);

    match action {
        "list" | "ls" => {
            cmd_list();
            0
        }
        "init" | "new" => cmd_init(args),
        "registry" => cmd_registry(args),
        "categories" | "cats" => {
            cmd_categories();
            0
        }
        "usage" | "stats" => {
            cmd_usage();
            0
        }
        "search" | "browse" => {
            cmd_search(args.get(1).map_or("", String::as_str));
            0
        }
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
        "revocations" => {
            cmd_revocations();
            0
        }
        "verify" => cmd_verify(),
        "audit" => match positional(args) {
            Some(target) => cmd_audit(&target),
            None => usage_exit("lean-ctx addon audit <name|path-to-lean-ctx-addon.toml>"),
        },
        "publish" => cmd_publish(args),
        "help" | "--help" | "-h" => {
            print_help();
            0
        }
        _ => {
            eprintln!("Unknown addon action: {action}");
            print_help();
            1
        }
    }
}

/// First non-flag argument after the action.
pub(super) fn positional(args: &[String]) -> Option<String> {
    args.get(1)
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty() && !s.starts_with('-'))
}

fn usage_exit(usage: &str) -> i32 {
    eprintln!("Usage: {usage}");
    1
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

fn cmd_info(name: &str) -> i32 {
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
            return 0;
        }
        eprintln!(
            "Addon `{name}` not found. Try `lean-ctx addon search`, or pass a path to a \
             lean-ctx-addon.toml."
        );
        return 1;
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
    0
}

fn cmd_add(target: &str, args: &[String]) -> i32 {
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
                return 1;
            }
        }
    } else if let Some(remote_ref) = crate::core::context_package::remote::parse_remote_ref(target)
    {
        match fetch_addon_pack(&remote_ref, flag_value(args, "--registry").as_deref()) {
            Ok((m, s)) => (m, s),
            Err(e) => {
                eprintln!("Error: {e}");
                return 1;
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
            return 1;
        };
        (m, "registry".to_string())
    };

    if let Err(e) = manifest.validate() {
        eprintln!("Error: {e}");
        return 1;
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
        return 1;
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
        return 1;
    }

    println!("About to install `{}`:\n", manifest.addon.name);
    print_install_preview(&manifest);

    // Depth-1 dependency resolution (GH #727): declared deps are part of the
    // consent surface — preview before asking, install before wiring. The
    // dependency list lives in the addon manifest itself, so a local
    // `lean-ctx-addon.toml` install resolves them the same as a hosted pack
    // (Finding A).
    // Self-dependency root: the addon's own scoped `@ns/slug` when the source
    // names a namespace (hosted pack), else `None` (a local manifest cannot
    // name itself) — never the bare `addon.name` slug (GH #727, Finding A).
    let root_ref = addon_self_ref(&source);
    let preview_deps = resolve_declared_deps(&manifest.dependencies, root_ref.as_deref(), args);
    if !preview_deps.is_empty() {
        println!("\nDeclared dependencies (installed alongside, depth-1):");
        for d in &preview_deps {
            println!("  + {}@{}", d.name, d.version);
        }
    }

    println!(
        "\nThis runs/connects to the above MCP server and exposes its tools through lean-ctx."
    );

    if !super::prompt::confirm(
        "Install this addon into the MCP gateway?",
        super::prompt::wants_yes(args),
    ) {
        println!("Aborted. Nothing was changed.");
        return 0;
    }

    // The slice wired into `[mcp.env]` must be the versions the install step
    // actually landed (lockfile honoured), never the preview's highest-match
    // resolution — otherwise `{pack_dir:}` could point at a directory that does
    // not exist (Finding B).
    let installed_deps = if preview_deps.is_empty() {
        Vec::new()
    } else {
        install_declared_deps(&manifest.dependencies, root_ref.as_deref(), args)
    };

    match provision_and_wire(manifest, &source, force, no_verify, &cfg, &installed_deps) {
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
            return 1;
        }
    }
    0
}

/// The impure provisioning pipeline `add` and `update` share, run after user
/// consent: pack-env expansion (#727) → managed artifact (GH #725) → bootstrap
/// (#1105) → health probe (#1076) → wire. On any error nothing is wired.
/// Returns the install outcome plus the probed tool count (`None` with
/// `--no-verify`).
pub(super) fn provision_and_wire(
    mut manifest: AddonManifest,
    source: &str,
    force: bool,
    no_verify: bool,
    cfg: &crate::core::config::Config,
    resolved_deps: &[crate::core::context_package::deps::ResolvedDep],
) -> Result<(install::InstallOutcome, Option<usize>), String> {
    // Pack-dir delivery (GH #727): expand `{pack_dir:@ns/name}` in [mcp.env]
    // against the resolved dependency versions. The caller installed those
    // dependencies already, so every path burned into the wiring exists. The
    // parameter *is* the ordering guarantee — this cannot be called before the
    // deps are resolved.
    if !manifest.mcp.env.is_empty() {
        let store_root = crate::core::context_package::LocalRegistry::open()?
            .root()
            .to_path_buf();
        manifest.mcp.env = crate::core::addons::pack_env::expand_pack_env(
            &manifest.mcp.env,
            resolved_deps,
            &store_root,
        )?;
    }

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
pub(super) fn fetch_addon_pack(
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
    let Bundle {
        manifest: pack_manifest,
        content,
    } = bundle;

    if pack_manifest.kind != crate::core::context_package::manifest::PackageKind::Addon {
        return Err(format!(
            "@{ns}/{name} is a kind={} package — install it with `lean-ctx pack install \
             {ns}/{name}` instead",
            pack_manifest.kind.as_str()
        ));
    }
    verify::validate_kind_coherence(&pack_manifest, &content).map_err(|errs| errs.join("; "))?;

    let payload = content
        .addon
        .expect("coherence guarantees content.addon for kind=addon");
    let manifest = AddonManifest::from_toml(&payload.manifest_toml)?;

    let source = format!("ctxpkg:@{ns}/{name}@{}", info.version);
    // Depth-1 dependencies (GH #727) travel inside the addon manifest itself
    // (`manifest.dependencies`, parsed from the pack's embedded TOML above), so
    // they resolve the same on every source path — the hosted `pack_manifest`
    // no longer needs to ride along.
    Ok((manifest, source))
}
