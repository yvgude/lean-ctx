/// Parse a package reference like `name@version` or `@scope/name@version`.
/// Scoped names start with `@`, so a bare `@scope/name` has no version,
/// while `@scope/name@1.0.0` splits at the *last* `@` that follows a `/`.
pub(super) fn parse_pkg_ref(s: &str) -> (&str, Option<&str>) {
    if s.starts_with('@') {
        if let Some(slash_pos) = s.find('/') {
            let after_scope = &s[slash_pos..];
            if let Some(at_pos) = after_scope.rfind('@')
                && at_pos > 0
            {
                let split = slash_pos + at_pos;
                return (&s[..split], Some(&s[split + 1..]));
            }
        }
        (s, None)
    } else if let Some(at_pos) = s.rfind('@') {
        (&s[..at_pos], Some(&s[at_pos + 1..]))
    } else {
        (s, None)
    }
}

pub(super) fn cmd_pack_install(args: &[String], project_root: &str) {
    let mut pkg_name: Option<String> = None;
    let mut pkg_version: Option<String> = None;
    let mut from_file: Option<String> = None;

    for a in args {
        if a == "install" {
            continue;
        }
        if let Some(v) = a.strip_prefix("--file=") {
            from_file = Some(v.to_string());
        } else if let Some(v) = a.strip_prefix("--version=") {
            pkg_version = Some(v.to_string());
        } else if !a.starts_with("--") && pkg_name.is_none() {
            let (parsed_name, parsed_ver) = parse_pkg_ref(a);
            pkg_name = Some(parsed_name.to_string());
            if let Some(v) = parsed_ver {
                pkg_version = Some(v.to_string());
            }
        }
    }

    if let Some(file_path) = from_file {
        let registry = match crate::core::context_package::LocalRegistry::open() {
            Ok(r) => r,
            Err(e) => {
                eprintln!("ERROR: {e}");
                return;
            }
        };
        match registry.import_from_file(
            std::path::Path::new(&file_path),
            local_signature_policy(args),
        ) {
            Ok(manifest) => {
                println!("Imported: {} v{}", manifest.name, manifest.version);
                println!(
                    "  Signature: {}",
                    crate::core::context_package::registry::describe_package_signature(&manifest)
                );
                apply_or_report(&manifest.name, &manifest.version, project_root);
            }
            Err(e) => eprintln!("ERROR: import failed: {e}"),
        }
        return;
    }

    let Some(name) = pkg_name else {
        eprintln!("ERROR: package name is required");
        eprintln!("Usage: lean-ctx pack install <name>[@version] [--file=path [--allow-unsigned]]");
        eprintln!("       lean-ctx pack install <ns>/<name>[@version] [--registry <url>]");
        return;
    };

    // `ns/name` (or `@ns/name`) → hosted-registry install (GL #406).
    if crate::core::context_package::remote::parse_remote_ref(&name).is_some() {
        let raw_ref = match pkg_version {
            Some(v) => format!("{name}@{v}"),
            None => name,
        };
        super::super::pack_remote::cmd_pack_install_remote(
            &raw_ref,
            parse_flag(args, "--registry").as_deref(),
            project_root,
            false,
        );
        return;
    }

    let registry = match crate::core::context_package::LocalRegistry::open() {
        Ok(r) => r,
        Err(e) => {
            eprintln!("ERROR: {e}");
            return;
        }
    };

    let resolved_version;
    let version = if let Some(v) = pkg_version.as_deref() {
        v
    } else {
        resolved_version = registry
            .list()
            .ok()
            .and_then(|entries| {
                entries
                    .iter()
                    .filter(|e| e.name == name)
                    .max_by(|a, b| a.installed_at.cmp(&b.installed_at))
                    .map(|e| e.version.clone())
            })
            .unwrap_or_default();
        &resolved_version
    };

    apply_or_report(&name, version, project_root);
}

fn apply_package(name: &str, version: &str, project_root: &str) {
    let registry = match crate::core::context_package::LocalRegistry::open() {
        Ok(r) => r,
        Err(e) => {
            eprintln!("ERROR: {e}");
            return;
        }
    };

    match registry.load_package(name, version) {
        Ok((manifest, content)) => {
            match crate::core::context_package::load_package(&manifest, &content, project_root) {
                Ok(report) => {
                    println!("{report}");
                    println!("Package applied successfully.");
                }
                Err(e) => eprintln!("ERROR: load failed: {e}"),
            }
        }
        Err(e) => eprintln!("ERROR: {e}"),
    }
}

pub(super) fn cmd_pack_list() {
    let registry = match crate::core::context_package::LocalRegistry::open() {
        Ok(r) => r,
        Err(e) => {
            eprintln!("ERROR: {e}");
            return;
        }
    };

    match registry.list() {
        Ok(entries) => {
            if entries.is_empty() {
                println!("No packages installed.");
                println!("Create one with: lean-ctx pack create --name <name>");
                return;
            }

            let header = format!(
                "{:<24} {:<10} {:<30} {:<10} AUTO-LOAD",
                "NAME", "VERSION", "LAYERS", "SIZE"
            );
            println!("{header}");
            println!("{}", "-".repeat(84));

            for e in &entries {
                println!(
                    "{:<24} {:<10} {:<30} {:<10} {}",
                    e.name,
                    e.version,
                    e.layers.join(", "),
                    format_bytes(e.byte_size),
                    if e.auto_load { "yes" } else { "no" }
                );
            }
            println!("\n{} package(s) installed.", entries.len());
        }
        Err(e) => eprintln!("ERROR: {e}"),
    }
}

pub(super) fn cmd_pack_info(args: &[String]) {
    let pkg_ref = args.iter().find(|a| !a.starts_with("--") && *a != "info");
    let Some(pkg_ref) = pkg_ref else {
        eprintln!("Usage: lean-ctx pack info <name>[@version]");
        return;
    };

    let (name, version) = parse_pkg_ref(pkg_ref);

    let registry = match crate::core::context_package::LocalRegistry::open() {
        Ok(r) => r,
        Err(e) => {
            eprintln!("ERROR: {e}");
            return;
        }
    };

    let resolved_ver;
    let ver = if let Some(v) = version {
        v
    } else {
        resolved_ver = registry
            .list()
            .ok()
            .and_then(|entries| {
                entries
                    .iter()
                    .filter(|e| e.name == name)
                    .max_by(|a, b| a.installed_at.cmp(&b.installed_at))
                    .map(|e| e.version.clone())
            })
            .unwrap_or_default();
        &resolved_ver
    };

    match registry.load_package(name, ver) {
        Ok((manifest, content)) => {
            println!("Package: {} v{}", manifest.name, manifest.version);
            println!("Schema:  v{}", manifest.schema_version);
            if let Some(lvl) = manifest.conformance_level {
                let label = match lvl {
                    1 => "Basic",
                    2 => "Graph",
                    3 => "Cognitive",
                    _ => "Unknown",
                };
                println!("Level:   {lvl} ({label})");
            }
            if let Some(ref s) = manifest.scope {
                println!("Scope:   {s}");
            }
            if !manifest.description.is_empty() {
                println!("Description: {}", manifest.description);
            }
            if let Some(ref a) = manifest.author {
                println!("Author: {a}");
            }
            println!(
                "Created: {}",
                manifest.created_at.format("%Y-%m-%d %H:%M UTC")
            );
            println!(
                "Layers: {}",
                manifest
                    .layers
                    .iter()
                    .map(crate::core::context_package::PackageLayer::as_str)
                    .collect::<Vec<_>>()
                    .join(", ")
            );
            if !manifest.tags.is_empty() {
                println!("Tags: {}", manifest.tags.join(", "));
            }
            println!("\nStats:");
            println!("  Knowledge facts:  {}", manifest.stats.knowledge_facts);
            println!("  Graph nodes:      {}", manifest.stats.graph_nodes);
            println!("  Graph edges:      {}", manifest.stats.graph_edges);
            println!("  Patterns:         {}", manifest.stats.pattern_count);
            println!("  Gotchas:          {}", manifest.stats.gotcha_count);
            println!(
                "  Compression:      {:.1}%",
                manifest.stats.compression_ratio * 100.0
            );
            if let Some(ref gs) = manifest.graph_summary {
                println!("\nGraph v2:");
                println!("  Nodes:       {}", gs.node_count);
                println!("  Edges:       {}", gs.edge_count);
                if let Some(mean) = gs.activation_mean {
                    println!("  Activation:  {mean:.2}");
                }
                if !gs.node_types.is_empty() {
                    println!("  Types:       {}", gs.node_types.join(", "));
                }
            }
            println!("  Est. tokens:      ~{}", content.estimated_token_count());
            println!("\nIntegrity:");
            println!("  SHA256:       {}", manifest.integrity.sha256);
            println!("  Content hash: {}", manifest.integrity.content_hash);
            println!(
                "  Size:         {}",
                format_bytes(manifest.integrity.byte_size)
            );
            println!("\nProvenance:");
            println!(
                "  Tool:    {} v{}",
                manifest.provenance.tool, manifest.provenance.tool_version
            );
            if let Some(ref h) = manifest.provenance.project_hash {
                println!("  Project: {h}");
            }
        }
        Err(e) => eprintln!("ERROR: {e}"),
    }
}

pub(super) fn cmd_pack_remove(args: &[String]) {
    let pkg_ref = args
        .iter()
        .find(|a| !a.starts_with("--") && *a != "remove" && *a != "rm");

    let Some(pkg_ref) = pkg_ref else {
        eprintln!("Usage: lean-ctx pack remove <name>[@version]");
        return;
    };

    let (name, version) = parse_pkg_ref(pkg_ref);

    let registry = match crate::core::context_package::LocalRegistry::open() {
        Ok(r) => r,
        Err(e) => {
            eprintln!("ERROR: {e}");
            return;
        }
    };

    match registry.remove(name, version) {
        Ok(0) => eprintln!("No matching package found: {name}"),
        Ok(n) => println!("Removed {n} package(s)."),
        Err(e) => eprintln!("ERROR: {e}"),
    }
}

/// Local file imports require a signature unless the caller passes
/// `--allow-unsigned` (GHSA-gvg8-285q-m62g).
pub(super) fn local_signature_policy(
    args: &[String],
) -> crate::core::context_package::registry::SignaturePolicy<'static> {
    if args.iter().any(|a| a == "--allow-unsigned") {
        eprintln!("WARNING: --allow-unsigned: an unsigned package's origin cannot be verified.");
        crate::core::context_package::registry::SignaturePolicy::AllowUnsigned
    } else {
        crate::core::context_package::registry::SignaturePolicy::RequireSigned
    }
}

pub(super) fn cmd_pack_import(args: &[String], project_root: &str) {
    let file_path = args.iter().find(|a| !a.starts_with("--") && *a != "import");
    let apply = args.iter().any(|a| a == "--apply");

    let Some(file_path) = file_path else {
        eprintln!("Usage: lean-ctx pack import <file.ctxpkg> [--apply] [--allow-unsigned]");
        return;
    };

    let registry = match crate::core::context_package::LocalRegistry::open() {
        Ok(r) => r,
        Err(e) => {
            eprintln!("ERROR: {e}");
            return;
        }
    };

    match registry.import_from_file(
        std::path::Path::new(file_path),
        local_signature_policy(args),
    ) {
        Ok(manifest) => {
            println!("Imported: {} v{}", manifest.name, manifest.version);
            println!(
                "  Signature: {}",
                crate::core::context_package::registry::describe_package_signature(&manifest)
            );
            println!(
                "  Layers: {}",
                manifest
                    .layers
                    .iter()
                    .map(crate::core::context_package::PackageLayer::as_str)
                    .collect::<Vec<_>>()
                    .join(", ")
            );
            println!("  Size:   {}", format_bytes(manifest.integrity.byte_size));

            if apply {
                apply_or_report(&manifest.name, &manifest.version, project_root);
            } else {
                println!("\nTo apply this package to the current project:");
                println!("  lean-ctx pack install {}", manifest.name);
            }
        }
        Err(e) => eprintln!("ERROR: import failed: {e}"),
    }
}

pub(super) fn cmd_pack_auto_load(args: &[String]) {
    let mut pkg_ref: Option<&str> = None;
    let mut enable = true;

    for a in args {
        if a == "auto-load" {
            continue;
        }
        if a == "--off" || a == "--disable" {
            enable = false;
        } else if !a.starts_with("--") && pkg_ref.is_none() {
            pkg_ref = Some(a.as_str());
        }
    }

    let Some(pkg_ref) = pkg_ref else {
        let registry = match crate::core::context_package::LocalRegistry::open() {
            Ok(r) => r,
            Err(e) => {
                eprintln!("ERROR: {e}");
                return;
            }
        };
        match registry.auto_load_packages() {
            Ok(entries) => {
                if entries.is_empty() {
                    println!("No packages set for auto-load.");
                } else {
                    println!("Auto-load packages:");
                    for e in &entries {
                        println!("  {} v{}", e.name, e.version);
                    }
                }
            }
            Err(e) => eprintln!("ERROR: {e}"),
        }
        return;
    };

    let (parsed_name, parsed_ver) = parse_pkg_ref(pkg_ref);
    let (name, version) = if let Some(v) = parsed_ver {
        (parsed_name, v.to_string())
    } else {
        let Ok(registry) = crate::core::context_package::LocalRegistry::open() else {
            eprintln!("Failed to open package registry");
            return;
        };
        let ver = registry
            .list()
            .ok()
            .and_then(|entries| {
                entries
                    .iter()
                    .filter(|e| e.name == parsed_name)
                    .max_by(|a, b| a.installed_at.cmp(&b.installed_at))
                    .map(|e| e.version.clone())
            })
            .unwrap_or_default();
        (parsed_name, ver)
    };

    let registry = match crate::core::context_package::LocalRegistry::open() {
        Ok(r) => r,
        Err(e) => {
            eprintln!("ERROR: {e}");
            return;
        }
    };

    match registry.set_auto_load(name, &version, enable) {
        Ok(()) => {
            if enable {
                println!("Auto-load enabled for {name}@{version}");
            } else {
                println!("Auto-load disabled for {name}@{version}");
            }
        }
        Err(e) => eprintln!("ERROR: {e}"),
    }
}

pub(crate) fn format_bytes(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{bytes} B")
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    }
}

/// Apply a context pack to the project — or, for kind=skills, report where
/// the verified files were materialized (they load from disk, not sessions).
pub(crate) fn apply_or_report(name: &str, version: &str, project_root: &str) {
    use crate::core::context_package::manifest::PackageKind;

    let kind = crate::core::context_package::LocalRegistry::open()
        .ok()
        .and_then(|r| r.load_package(name, version).ok())
        .map(|(m, _)| m.kind);

    if kind == Some(PackageKind::Skills) {
        let root = crate::core::context_package::LocalRegistry::open()
            .map(|r| crate::core::context_package::skills::skills_dir(r.root(), name, version));
        match root {
            Ok(dir) => {
                println!(
                    "Skills pack {name}@{version} materialized at {}",
                    dir.display()
                );
                println!("  Files are read-only and SHA-256 verified against the manifest.");
            }
            Err(e) => eprintln!("ERROR: {e}"),
        }
        return;
    }

    apply_package(name, version, project_root);
}

/// Parse `--flag=value` or `--flag value` from args.
pub(crate) fn parse_flag(args: &[String], flag: &str) -> Option<String> {
    let prefix = format!("{flag}=");
    let mut iter = args.iter();
    while let Some(a) = iter.next() {
        if let Some(v) = a.strip_prefix(&prefix) {
            return Some(v.to_string());
        }
        if a == flag
            && let Some(next) = iter.next()
            && !next.starts_with("--")
        {
            return Some(next.clone());
        }
    }
    None
}
