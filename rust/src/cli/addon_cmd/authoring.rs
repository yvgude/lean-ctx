use super::{AddonManifest, Path, flag_value, looks_like_path, positional, registry};

/// `addon init [name]` — scaffold a ready-to-edit `lean-ctx-addon.toml` in the
/// current directory. `--http` for an HTTP addon, `--force` to overwrite.
pub(super) fn cmd_init(args: &[String]) -> i32 {
    use crate::core::addons::scaffold;
    use crate::core::mcp_catalog::TransportKind;

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
        return 1;
    };
    let Some(slug) = scaffold::slugify(&raw) else {
        eprintln!("`{raw}` has no usable slug characters ([a-z0-9-]).");
        return 1;
    };

    let path = Path::new(scaffold::MANIFEST_FILENAME);
    if path.exists() && !force {
        eprintln!(
            "{} already exists. Re-run with --force to overwrite.",
            scaffold::MANIFEST_FILENAME
        );
        return 1;
    }

    let contents = scaffold::addon_manifest(&slug, transport, command.as_deref());
    if let Err(e) = std::fs::write(path, contents) {
        eprintln!("Error writing {}: {e}", scaffold::MANIFEST_FILENAME);
        return 1;
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
    0
}

/// `addon registry validate [path]` — run the registry security/quality bar
/// (#864 + #403) against a registry JSON file, or the bundled + local registry
/// if no path is given. The dry-run harness an author / CI uses before opening a
/// merge request. Non-zero exit when problems are found.
pub(super) fn cmd_registry(args: &[String]) -> i32 {
    let sub = args.get(1).map_or("", String::as_str);
    if sub != "validate" {
        eprintln!("Usage: lean-ctx addon registry validate [path-to-registry.json]");
        return 1;
    }

    let (entries, label) = match args.get(2).map(String::as_str) {
        Some(path) if !path.starts_with('-') => match load_registry_file(path) {
            Ok(e) => (e, path.to_string()),
            Err(e) => {
                eprintln!("Error: {e}");
                return 1;
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
        return 0;
    }
    eprintln!("✗ {label}: {} problem(s):\n", problems.len());
    for p in &problems {
        eprintln!("  • {p}");
    }
    1
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
pub(super) fn cmd_audit(target: &str) -> i32 {
    let manifest = if looks_like_path(target) {
        match AddonManifest::from_path(Path::new(target)) {
            Ok(m) => m,
            Err(e) => {
                eprintln!("Error: {e}");
                return 1;
            }
        }
    } else {
        let Some(m) = registry::get(target) else {
            eprintln!("Unknown addon `{target}`. Pass a name from the registry or a path.");
            return 1;
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
        if manifest.mcp.transport == crate::core::mcp_catalog::TransportKind::Http {
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
        return 1;
    }
    0
}
