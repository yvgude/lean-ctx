//! `lean-ctx addon` — manage community addons (MCP extensions) (#858).
//!
//! Thin CLI over [`crate::core::addons`]: browse the registry, install an addon
//! (from the registry or a local `lean-ctx-addon.toml`), and remove it. `add`
//! and `remove` wire external code into the MCP gateway, so both pass through
//! the shared confirmation gate (`cli::prompt`).

use std::path::Path;

use crate::core::addons::manifest::AddonManifest;
use crate::core::addons::revocation::RevocationList;
use crate::core::addons::store::InstalledStore;
use crate::core::addons::{install, registry};

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
    let (manifest, source) = if looks_like_path(target) {
        match AddonManifest::from_path(Path::new(target)) {
            Ok(m) => (m, "local".to_string()),
            Err(e) => {
                eprintln!("Error: {e}");
                std::process::exit(1);
            }
        }
    } else {
        let Some(m) = registry::get(target) else {
            eprintln!(
                "Unknown addon `{target}`.\n\
                 Browse with `lean-ctx addon search`, or pass a path to a \
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

    match install::install(&manifest, &source) {
        Ok(outcome) => {
            println!(
                "\n✓ Installed `{}` → gateway server `{}`.",
                outcome.name, outcome.gateway_server
            );
            if outcome.enabled_gateway {
                println!("  Enabled the MCP gateway (gateway.enabled = true).");
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

fn cmd_remove(name: &str, args: &[String]) {
    if InstalledStore::load().get(name).is_none() {
        eprintln!("Addon `{name}` is not installed.");
        std::process::exit(1);
    }

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

    let contents = scaffold::addon_manifest(&slug, transport);
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
        }
        crate::core::gateway::TransportKind::Http => {
            println!("  url:       {}", mcp.url);
            if !mcp.headers.is_empty() {
                let keys: Vec<&str> = mcp.headers.keys().map(String::as_str).collect();
                println!("  headers:   {}", keys.join(", "));
            }
        }
    }
    print_capabilities(manifest);
    print_security_review(manifest);
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
                                  [--http] [--force]\n    \
             search [query]       Search the registry (empty = list all)\n    \
             categories           Browse the registry by category\n    \
             usage                Per-addon / per-tool call counters\n    \
             info <name|path>     Show an addon's details + MCP wiring\n    \
             add <name|path>      Install from the registry or a local\n                         \
                                  lean-ctx-addon.toml (asks for confirmation)\n    \
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
             -y, --yes            Skip the confirmation prompt (scripts/CI)\n\
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
             4. Get listed:    open a merge request adding your entry to\n                      \
                               rust/data/addon_registry.json (see docs/guides/addons.md).\n\
         \n    \
             Full guide: docs/guides/addons.md"
    );
}
