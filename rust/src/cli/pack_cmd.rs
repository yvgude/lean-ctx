use std::io::Read as _;
use std::path::PathBuf;

/// Parse a package reference like `name@version` or `@scope/name@version`.
/// Scoped names start with `@`, so a bare `@scope/name` has no version,
/// while `@scope/name@1.0.0` splits at the *last* `@` that follows a `/`.
fn parse_pkg_ref(s: &str) -> (&str, Option<&str>) {
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

pub(crate) fn cmd_pack(args: &[String]) {
    let project_root = super::common::detect_project_root(args);

    let subcommand = args
        .iter()
        .find(|a| !a.starts_with("--"))
        .map_or("pr", String::as_str);

    match subcommand {
        "pr" => cmd_pack_pr(args, &project_root),
        "create" => cmd_pack_create(args, &project_root),
        "install" => cmd_pack_install(args, &project_root),
        "list" | "ls" => cmd_pack_list(),
        "info" => cmd_pack_info(args),
        "remove" | "rm" => cmd_pack_remove(args),
        "export" => cmd_pack_export(args),
        "import" => cmd_pack_import(args, &project_root),
        "verify" => cmd_pack_verify(args),
        "auto-load" => cmd_pack_auto_load(args),
        "publish" => cmd_pack_publish(args),
        "send" => cmd_pack_send(args, &project_root),
        "receive" => cmd_pack_receive(args, &project_root),
        "help" | "--help" | "-h" => print_usage(),
        other => {
            eprintln!("Unknown pack subcommand: {other}");
            print_usage();
        }
    }
}

fn cmd_pack_pr(args: &[String], project_root: &str) {
    let mut base: Option<String> = None;
    let mut format: Option<String> = None;
    let mut depth: Option<usize> = None;
    let mut diff_from_stdin = false;

    let mut it = args.iter().peekable();
    while let Some(a) = it.next() {
        if a == "pr" {
            continue;
        }
        if let Some(v) = a.strip_prefix("--base=") {
            base = Some(v.to_string());
            continue;
        }
        if a == "--base" {
            if let Some(v) = it.peek()
                && !v.starts_with("--")
            {
                base = Some((*v).clone());
                it.next();
            }
            continue;
        }
        if let Some(v) = a.strip_prefix("--format=") {
            format = Some(v.to_string());
            continue;
        }
        if a == "--format" {
            if let Some(v) = it.peek()
                && !v.starts_with("--")
            {
                format = Some((*v).clone());
                it.next();
            }
            continue;
        }
        if a == "--json" {
            format = Some("json".to_string());
            continue;
        }
        if let Some(v) = a.strip_prefix("--depth=") {
            depth = v.parse::<usize>().ok();
            continue;
        }
        if a == "--depth" {
            if let Some(v) = it.peek()
                && !v.starts_with("--")
            {
                depth = (*v).parse::<usize>().ok();
                it.next();
            }
            continue;
        }
        if a == "--diff-from-stdin" {
            diff_from_stdin = true;
        }
    }

    let diff = if diff_from_stdin {
        let mut buf = String::new();
        let _ = std::io::stdin().read_to_string(&mut buf);
        if buf.trim().is_empty() {
            None
        } else {
            Some(buf)
        }
    } else {
        None
    };

    let out = crate::tools::ctx_pack::handle(
        "pr",
        project_root,
        base.as_deref(),
        format.as_deref(),
        depth,
        diff.as_deref(),
    );
    println!("{out}");
}

fn cmd_pack_create(args: &[String], project_root: &str) {
    let mut name: Option<String> = None;
    let mut version = "1.0.0".to_string();
    let mut description = String::new();
    let mut author: Option<String> = None;
    let mut tags: Vec<String> = Vec::new();
    let mut layers_str: Option<String> = None;
    let mut level: u32 = 1;
    let mut scope: Option<String> = None;
    let mut private = false;

    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        if a == "create" {
            i += 1;
            continue;
        }
        if a == "--private" {
            private = true;
            i += 1;
            continue;
        }
        if let Some(v) = a.strip_prefix("--name=") {
            name = Some(v.to_string());
        } else if a == "--name" {
            i += 1;
            if let Some(v) = args.get(i).filter(|v| !v.starts_with("--")) {
                name = Some(v.clone());
            }
        } else if let Some(v) = a.strip_prefix("--version=") {
            version = v.to_string();
        } else if a == "--version" {
            i += 1;
            if let Some(v) = args.get(i).filter(|v| !v.starts_with("--")) {
                v.clone_into(&mut version);
            }
        } else if let Some(v) = a.strip_prefix("--description=") {
            description = v.to_string();
        } else if a == "--description" {
            i += 1;
            if let Some(v) = args.get(i).filter(|v| !v.starts_with("--")) {
                v.clone_into(&mut description);
            }
        } else if let Some(v) = a.strip_prefix("--author=") {
            author = Some(v.to_string());
        } else if a == "--author" {
            i += 1;
            if let Some(v) = args.get(i).filter(|v| !v.starts_with("--")) {
                author = Some(v.clone());
            }
        } else if let Some(v) = a.strip_prefix("--tags=") {
            tags = v.split(',').map(|s| s.trim().to_string()).collect();
        } else if let Some(v) = a.strip_prefix("--layers=") {
            layers_str = Some(v.to_string());
        } else if let Some(v) = a.strip_prefix("--level=") {
            level = v.parse::<u32>().unwrap_or(1).clamp(1, 3);
        } else if a == "--level" {
            i += 1;
            if let Some(v) = args.get(i).filter(|v| !v.starts_with("--")) {
                level = v.parse::<u32>().unwrap_or(1).clamp(1, 3);
            }
        } else if let Some(v) = a.strip_prefix("--scope=") {
            scope = Some(v.to_string());
        } else if a == "--scope" {
            i += 1;
            if let Some(v) = args.get(i).filter(|v| !v.starts_with("--")) {
                scope = Some(v.clone());
            }
        }
        i += 1;
    }

    let Some(pkg_name) = name else {
        eprintln!("ERROR: --name is required for pack create");
        return;
    };

    let requested_layers: Vec<&str> = layers_str.as_deref().map_or_else(
        || vec!["knowledge", "graph", "session", "gotchas"],
        |s| s.split(',').map(str::trim).collect(),
    );

    let mut builder = crate::core::context_package::PackageBuilder::new(&pkg_name, &version)
        .description(&description)
        .tags(tags)
        .level(level);

    if let Some(ref a) = author {
        builder = builder.author(a);
    }
    if let Some(ref s) = scope {
        builder = builder.scope(s);
    }
    if private {
        builder = builder.private();
    }

    let phash = crate::core::project_hash::hash_project_root(project_root);
    builder = builder.project_hash(&phash);

    if level >= 2 {
        builder.build_context_graph(project_root);
    }

    if requested_layers.contains(&"knowledge") || requested_layers.contains(&"patterns") {
        builder = builder.add_knowledge_from_project(project_root);
    }
    if requested_layers.contains(&"patterns") {
        builder = builder.add_patterns_from_project(project_root);
    }
    if requested_layers.contains(&"graph") {
        builder = builder.add_graph_from_project(project_root);
    }
    if requested_layers.contains(&"session")
        && let Some(session) = crate::core::session::SessionState::load_latest()
    {
        builder = builder.add_session(&session);
    }
    if requested_layers.contains(&"gotchas") {
        builder = builder.add_gotchas_from_project(project_root);
    }

    match builder.build() {
        Ok((manifest, content)) => {
            let registry = match crate::core::context_package::LocalRegistry::open() {
                Ok(r) => r,
                Err(e) => {
                    eprintln!("ERROR: cannot open registry: {e}");
                    return;
                }
            };

            match registry.install(&manifest, &content) {
                Ok(dir) => {
                    println!("Package created successfully:");
                    println!("  Name:    {}", manifest.name);
                    println!("  Version: {}", manifest.version);
                    println!("  Schema:  v{}", manifest.schema_version);
                    if let Some(lvl) = manifest.conformance_level {
                        println!("  Level:   {lvl}");
                    }
                    if let Some(ref s) = manifest.scope {
                        println!("  Scope:   {s}");
                    }
                    println!(
                        "  Layers:  {}",
                        manifest
                            .layers
                            .iter()
                            .map(crate::core::context_package::PackageLayer::as_str)
                            .collect::<Vec<_>>()
                            .join(", ")
                    );
                    println!("  Stats:");
                    println!("    Knowledge facts: {}", manifest.stats.knowledge_facts);
                    println!("    Graph nodes:     {}", manifest.stats.graph_nodes);
                    println!("    Graph edges:     {}", manifest.stats.graph_edges);
                    println!("    Patterns:        {}", manifest.stats.pattern_count);
                    println!("    Gotchas:         {}", manifest.stats.gotcha_count);
                    println!(
                        "    Compression:     {:.1}%",
                        manifest.stats.compression_ratio * 100.0
                    );
                    if let Some(ref gs) = manifest.graph_summary {
                        println!("  Graph v2:");
                        println!("    Nodes:      {}", gs.node_count);
                        println!("    Edges:      {}", gs.edge_count);
                        if let Some(mean) = gs.activation_mean {
                            println!("    Activation: {mean:.2}");
                        }
                        println!("    Types:      {}", gs.node_types.join(", "));
                    }
                    println!("  Size:    {} bytes", manifest.integrity.byte_size);
                    println!(
                        "  SHA256:  {}...{}",
                        &manifest.integrity.sha256[..8],
                        &manifest.integrity.sha256[56..]
                    );
                    println!("  Stored:  {}", dir.display());

                    // Early warning — export blocks these, the registry hard-rejects them.
                    if let Ok(reg) = crate::core::context_package::LocalRegistry::open() {
                        let findings =
                            scan_package_content(&reg, &manifest.name, &manifest.version);
                        if !findings.is_empty() {
                            eprintln!(
                                "\nWARNING: {} credential-shaped string(s) in the package content:",
                                findings.len()
                            );
                            print_secret_findings(&findings);
                            eprintln!(
                                "  Remove them and re-create — export and ctxpkg.com publishing will refuse this pack."
                            );
                        }
                    }
                }
                Err(e) => eprintln!("ERROR: install failed: {e}"),
            }
        }
        Err(e) => eprintln!("ERROR: build failed: {e}"),
    }
}

fn cmd_pack_install(args: &[String], project_root: &str) {
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
        match registry.import_from_file(std::path::Path::new(&file_path)) {
            Ok(manifest) => {
                println!("Imported: {} v{}", manifest.name, manifest.version);
                apply_package(&manifest.name, &manifest.version, project_root);
            }
            Err(e) => eprintln!("ERROR: import failed: {e}"),
        }
        return;
    }

    let Some(name) = pkg_name else {
        eprintln!("ERROR: package name is required");
        eprintln!("Usage: lean-ctx pack install <name>[@version] [--file=path]");
        eprintln!("       lean-ctx pack install <ns>/<name>[@version] [--registry <url>]");
        return;
    };

    // `ns/name` (or `@ns/name`) → hosted-registry install (GL #406).
    if crate::core::context_package::remote::parse_remote_ref(&name).is_some() {
        let raw_ref = match pkg_version {
            Some(v) => format!("{name}@{v}"),
            None => name,
        };
        cmd_pack_install_remote(
            &raw_ref,
            parse_flag(args, "--registry").as_deref(),
            project_root,
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

    apply_package(&name, version, project_root);
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

fn cmd_pack_list() {
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

fn cmd_pack_info(args: &[String]) {
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

fn cmd_pack_remove(args: &[String]) {
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

/// Serialize a stored package's content and run the built-in secret scanner
/// over it — the same patterns the hosted registry hard-blocks at publish.
fn scan_package_content(
    registry: &crate::core::context_package::LocalRegistry,
    name: &str,
    version: &str,
) -> Vec<crate::core::secret_detection::SecretMatch> {
    let Ok((_, content)) = registry.load_package(name, version) else {
        return Vec::new();
    };
    let Ok(json) = serde_json::to_string_pretty(&content) else {
        return Vec::new();
    };
    crate::core::secret_detection::detect_secrets(&json)
}

fn print_secret_findings(findings: &[crate::core::secret_detection::SecretMatch]) {
    for f in findings.iter().take(10) {
        eprintln!("    {:<22} {}", f.pattern_name, f.redacted_preview);
    }
    if findings.len() > 10 {
        eprintln!("    … and {} more", findings.len() - 10);
    }
}

fn cmd_pack_export(args: &[String]) {
    let mut pkg_ref: Option<&str> = None;
    let mut output: Option<String> = None;
    let mut sign = false;
    let mut private = false;
    let mut allow_secrets = false;

    for a in args {
        if a == "export" {
            continue;
        }
        if let Some(v) = a.strip_prefix("--output=") {
            output = Some(v.to_string());
        } else if let Some(v) = a.strip_prefix("-o=") {
            output = Some(v.to_string());
        } else if a == "--sign" {
            sign = true;
        } else if a == "--private" {
            private = true;
        } else if a == "--allow-secrets" {
            allow_secrets = true;
        } else if !a.starts_with("--") && pkg_ref.is_none() {
            pkg_ref = Some(a.as_str());
        }
    }

    let Some(pkg_ref) = pkg_ref else {
        eprintln!(
            "Usage: lean-ctx pack export <name>[@version] [--output=path] [--sign] [--private] [--allow-secrets]"
        );
        return;
    };
    if private && !sign {
        eprintln!("ERROR: --private only applies to signed exports — add --sign");
        return;
    }

    let (parsed_name, parsed_ver) = parse_pkg_ref(pkg_ref);
    let (name, version) = if let Some(v) = parsed_ver {
        (parsed_name.to_string(), v.to_string())
    } else {
        let registry = match crate::core::context_package::LocalRegistry::open() {
            Ok(r) => r,
            Err(e) => {
                eprintln!("ERROR opening registry: {e}");
                return;
            }
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
        (parsed_name.to_string(), ver)
    };

    let out_path =
        output.unwrap_or_else(|| crate::core::contracts::default_package_filename(&name, &version));

    let registry = match crate::core::context_package::LocalRegistry::open() {
        Ok(r) => r,
        Err(e) => {
            eprintln!("ERROR: {e}");
            return;
        }
    };

    // Pre-flight secret scan — same patterns the hosted registry enforces.
    let findings = scan_package_content(&registry, &name, &version);
    if !findings.is_empty() {
        eprintln!(
            "Secret scan: {} credential-shaped string(s) in {name}@{version}:",
            findings.len()
        );
        print_secret_findings(&findings);
        if !allow_secrets {
            eprintln!("ERROR: export blocked.");
            eprintln!(
                "  Remove the secrets (e.g. `lean-ctx knowledge remove --category <cat> --key <key>`),"
            );
            eprintln!("  rotate them if they were live, then re-create and export.");
            eprintln!(
                "  `--allow-secrets` forces a local-only export — ctxpkg.com rejects it at publish anyway."
            );
            return;
        }
        eprintln!("WARNING: continuing because of --allow-secrets — do NOT publish this artifact.");
    }

    if sign {
        let (key, created) = match crate::core::context_package::keys::load_or_create() {
            Ok(k) => k,
            Err(e) => {
                eprintln!("ERROR: signing key: {e}");
                return;
            }
        };
        if created {
            println!(
                "Generated a new ed25519 signing key at ~/.lean-ctx/{}",
                crate::core::context_package::keys::KEY_REL_PATH
            );
            println!("This key IS your publisher identity — back it up.");
        }
        match registry.export_to_file_signed(
            &name,
            &version,
            &PathBuf::from(&out_path),
            &key,
            private,
        ) {
            Ok(bytes) => {
                let vis = if private { ", private" } else { "" };
                println!(
                    "Exported (signed{vis}): {out_path} ({})",
                    format_bytes(bytes)
                );
                println!(
                    "Signer public key: {}",
                    crate::core::context_package::keys::public_key_hex(&key)
                );
            }
            Err(e) => eprintln!("ERROR: {e}"),
        }
        return;
    }

    match registry.export_to_file(&name, &version, &PathBuf::from(&out_path)) {
        Ok(bytes) => {
            println!("Exported: {out_path} ({})", format_bytes(bytes));
        }
        Err(e) => eprintln!("ERROR: {e}"),
    }
}

fn cmd_pack_import(args: &[String], project_root: &str) {
    let file_path = args.iter().find(|a| !a.starts_with("--") && *a != "import");
    let apply = args.iter().any(|a| a == "--apply");

    let Some(file_path) = file_path else {
        eprintln!("Usage: lean-ctx pack import <file.ctxpkg> [--apply]");
        return;
    };

    let registry = match crate::core::context_package::LocalRegistry::open() {
        Ok(r) => r,
        Err(e) => {
            eprintln!("ERROR: {e}");
            return;
        }
    };

    match registry.import_from_file(std::path::Path::new(file_path)) {
        Ok(manifest) => {
            println!("Imported: {} v{}", manifest.name, manifest.version);
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
                apply_package(&manifest.name, &manifest.version, project_root);
            } else {
                println!("\nTo apply this package to the current project:");
                println!("  lean-ctx pack install {}", manifest.name);
            }
        }
        Err(e) => eprintln!("ERROR: import failed: {e}"),
    }
}

/// `pack verify` — standalone conformance check (spec §8/§9), no install.
/// Exit code 0 = all files valid, 1 = any failure (CI-friendly).
fn cmd_pack_verify(args: &[String]) {
    use crate::core::context_package::verify::{CheckOutcome, verify_package_file};

    let files: Vec<&String> = args
        .iter()
        .filter(|a| !a.starts_with("--") && *a != "verify")
        .collect();
    if files.is_empty() {
        eprintln!("Usage: lean-ctx pack verify <file.ctxpkg> [more files...]");
        std::process::exit(2);
    }

    let label = |o: CheckOutcome| match o {
        CheckOutcome::Pass => "pass",
        CheckOutcome::Fail => "FAIL",
        CheckOutcome::Skipped => "skipped",
    };

    let mut all_valid = true;
    for file in files {
        match verify_package_file(std::path::Path::new(file)) {
            Ok(report) => {
                let verdict = if report.valid() { "VALID" } else { "INVALID" };
                let subject = match (&report.name, &report.version) {
                    (Some(n), Some(v)) => format!("{n}@{v}"),
                    _ => "(unparseable manifest)".into(),
                };
                println!("{verdict}  {file}  {subject}");
                println!("  structure      {}", label(report.structure));
                println!("  content hash   {}", label(report.content_hash));
                println!("  package hash   {}", label(report.package_hash));
                let sig = if report.signature == CheckOutcome::Skipped {
                    "skipped (unsigned)"
                } else {
                    label(report.signature)
                };
                println!("  signature      {sig}");
                for err in &report.errors {
                    println!("    - {err}");
                }
                if !report.valid() {
                    all_valid = false;
                }
            }
            Err(e) => {
                println!("ERROR    {file}");
                println!("    - {e}");
                all_valid = false;
            }
        }
    }
    if !all_valid {
        std::process::exit(1);
    }
}

fn cmd_pack_auto_load(args: &[String]) {
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

fn format_bytes(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{bytes} B")
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    }
}

fn cmd_pack_publish(args: &[String]) {
    use crate::core::context_package::remote;

    let file = args.iter().find(|a| a.ends_with(".ctxpkg"));
    let Some(file) = file else {
        eprintln!(
            "Usage: lean-ctx pack publish <file.ctxpkg> [--registry <url>] [--token <ctxp_…>]"
        );
        eprintln!();
        eprintln!("The token comes from your ctxpkg.com account (ctxpkg.com/account) or");
        eprintln!("the CTXPKG_TOKEN environment variable. Packages must be signed and");
        eprintln!("scoped (@namespace/name) — see `lean-ctx pack export --sign`.");
        return;
    };

    let path = std::path::Path::new(file);
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("ERROR: read {file}: {e}");
            return;
        }
    };

    // Fail locally before any network call: parse, verify signature, check scope.
    let (ns, name, version) = match remote::preflight_bundle(&bytes) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("ERROR: {e}");
            return;
        }
    };

    let base = remote::registry_base(parse_flag(args, "--registry").as_deref());
    let Some(token) = remote::publish_token(parse_flag(args, "--token").as_deref()) else {
        eprintln!("ERROR: no publish token — pass --token or set CTXPKG_TOKEN");
        eprintln!("Mint one at ctxpkg.com/account (sign in, then Tokens → Mint).");
        return;
    };
    if token.starts_with("ctxr_") {
        eprintln!(
            "ERROR: this is a read-only install token (ctxr_) — publishing needs a ctxp_ token"
        );
        return;
    }

    println!("Publishing @{ns}/{name}@{version} to {base} …");
    match remote::publish(&base, &token, &ns, &name, &version, &bytes) {
        Ok(receipt) => {
            println!("Published: {}", receipt.published);
            println!("Artifact SHA-256: {}", receipt.artifact_sha256);
            println!("Install with: lean-ctx pack install {ns}/{name}");
        }
        Err(e) => eprintln!("ERROR: {e}"),
    }
}

/// Install `ns/name[@version]` from the hosted registry: resolve the version,
/// download, verify the artifact hash against the index, then run the normal
/// import path (manifest validation + content integrity + local signature
/// re-verification) and pin the result in `.lean-ctx/ctxpkg.lock`.
fn cmd_pack_install_remote(raw_ref: &str, registry_flag: Option<&str>, project_root: &str) {
    use crate::core::context_package::{LocalRegistry, lockfile, remote};

    let Some(remote_ref) = remote::parse_remote_ref(raw_ref) else {
        eprintln!("ERROR: '{raw_ref}' is not a valid ns/name[@version] reference");
        return;
    };
    let base = remote::registry_base(registry_flag);
    let ns = &remote_ref.namespace;
    let name = &remote_ref.name;
    // CTXPKG_TOKEN (ctxp_ or read-only ctxr_) unlocks private packages (#524).
    let token = remote::publish_token(None);

    println!("Resolving @{ns}/{name} via {base} …");
    let versions = match remote::fetch_versions(&base, ns, name, token.as_deref()) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("ERROR: {e}");
            return;
        }
    };
    let info = match remote::select_version(&versions, remote_ref.version.as_deref()) {
        Ok(i) => i,
        Err(e) => {
            eprintln!("ERROR: {e}");
            return;
        }
    };
    if info.yanked {
        eprintln!(
            "WARNING: @{ns}/{name}@{} is YANKED — installing only because the version \
             was pinned explicitly",
            info.version
        );
    }

    let bytes = match remote::download_verified(&base, ns, name, info, token.as_deref()) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("ERROR: {e}");
            return;
        }
    };
    println!(
        "Downloaded @{ns}/{name}@{} ({}, sha256 verified)",
        info.version,
        format_bytes(bytes.len() as u64)
    );

    // Hand the artifact to the standard import path via a temp file so every
    // local gate (extension, size cap, manifest validation, content integrity)
    // applies identically to remote and local installs.
    let tmp = std::env::temp_dir().join(format!("ctxpkg-install-{}.ctxpkg", std::process::id()));
    if let Err(e) = std::fs::write(&tmp, &bytes) {
        eprintln!("ERROR: stage artifact: {e}");
        return;
    }
    let imported = (|| {
        let registry = LocalRegistry::open()?;
        registry.import_from_file(&tmp)
    })();
    std::fs::remove_file(&tmp).ok();

    let manifest = match imported {
        Ok(m) => m,
        Err(e) => {
            eprintln!("ERROR: import failed: {e}");
            return;
        }
    };

    // Registry compromise ≠ client compromise: re-verify the signature locally.
    match crate::core::context_package::verify_signature(&manifest) {
        Ok(true) => println!("Signature: ed25519 verified locally"),
        Ok(false) => {
            eprintln!(
                "WARNING: package is unsigned — the hosted registry should not have accepted it"
            );
        }
        Err(e) => {
            eprintln!("ERROR: signature verification failed: {e}");
            return;
        }
    }

    if let Err(e) = lockfile::upsert(
        std::path::Path::new(project_root),
        lockfile::LockedPackage {
            name: manifest.name.clone(),
            version: manifest.version.clone(),
            artifact_sha256: info.artifact_sha256.clone(),
            registry: base,
        },
    ) {
        eprintln!("WARNING: could not update ctxpkg.lock: {e}");
    } else {
        println!("Pinned in {}", lockfile::LOCKFILE_REL_PATH);
    }

    apply_package(&manifest.name, &manifest.version, project_root);
}

fn cmd_pack_send(args: &[String], project_root: &str) {
    use crate::core::a2a_transport::{
        AgentIdentityV1, TransportContentType, TransportEnvelopeV1, serialize_envelope,
    };

    let file: Option<String> = args
        .iter()
        .find(|a| crate::core::contracts::is_package_file(std::path::Path::new(a.as_str())))
        .cloned();
    let target_url = parse_flag(args, "--target");
    let recipient = parse_flag(args, "--to");
    let secret = parse_flag(args, "--secret");

    let Some(f) = file else {
        eprintln!(
            "Usage: lean-ctx pack send <file.{ext}> [--target <url>] [--to <agent>] [--secret <key>]",
            ext = crate::core::contracts::PACKAGE_EXTENSION
        );
        return;
    };
    let pkg_file = PathBuf::from(f);

    let content = match std::fs::read_to_string(&pkg_file) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Error reading {}: {e}", pkg_file.display());
            return;
        }
    };

    let sender = AgentIdentityV1::from_current("cli", "lean-ctx-cli");
    let mut envelope = TransportEnvelopeV1::new(
        sender,
        recipient.as_deref(),
        TransportContentType::ContextPackage,
        content,
    );
    envelope
        .metadata
        .insert("source_file".to_string(), pkg_file.display().to_string());

    {
        use sha2::{Digest, Sha256};
        let hash =
            crate::core::agent_identity::hex_encode(&Sha256::digest(project_root.as_bytes()));
        envelope
            .metadata
            .insert("project_root_hash".to_string(), hash[..16].to_string());
    }

    if let Some(ref s) = secret {
        envelope.sign(s.as_bytes());
    }

    let json = match serialize_envelope(&envelope) {
        Ok(j) => j,
        Err(e) => {
            eprintln!("Error serializing envelope: {e}");
            return;
        }
    };

    if let Some(ref url) = target_url {
        let endpoint = format!("{}/v1/a2a/handoff", url.trim_end_matches('/'));
        let body = json.as_bytes().to_vec();
        match ureq::post(&endpoint)
            .header("Content-Type", "application/json")
            .send(&body)
        {
            Ok(resp) => {
                let status = resp.status();
                if (200..300).contains(&status.as_u16()) {
                    eprintln!("Sent to {endpoint} — HTTP {status}");
                } else {
                    eprintln!("ERROR: server returned HTTP {status} for {endpoint}");
                }
            }
            Err(e) => eprintln!("Send failed: {e}"),
        }
    } else {
        let out_path = pkg_file.with_extension(format!(
            "{}.envelope.json",
            crate::core::contracts::PACKAGE_EXTENSION
        ));
        match std::fs::write(&out_path, &json) {
            Ok(()) => eprintln!("Envelope written: {}", out_path.display()),
            Err(e) => eprintln!("Write failed: {e}"),
        }
    }
}

fn cmd_pack_receive(args: &[String], project_root: &str) {
    use crate::core::a2a_transport::{TransportContentType, parse_envelope};

    let file: Option<String> = args
        .iter()
        .find(|a| {
            let p = std::path::Path::new(a.as_str());
            p.extension()
                .and_then(|ext| ext.to_str())
                .is_some_and(|ext| ext == "json" || crate::core::contracts::is_package_file(p))
        })
        .cloned();
    let secret = parse_flag(args, "--secret");
    let apply = args.iter().any(|a| a == "--apply");

    let Some(f) = file else {
        eprintln!("Usage: lean-ctx pack receive <envelope.json> [--secret <key>] [--apply]");
        return;
    };
    let envelope_file = PathBuf::from(f);

    let json = match std::fs::read_to_string(&envelope_file) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Error reading {}: {e}", envelope_file.display());
            return;
        }
    };

    let envelope = match parse_envelope(&json) {
        Ok(env) => env,
        Err(e) => {
            eprintln!("Error parsing envelope: {e}");
            return;
        }
    };

    if let Some(ref s) = secret {
        if !envelope.verify_signature(s.as_bytes()) {
            eprintln!("ERROR: Signature verification failed. Envelope may be tampered.");
            return;
        }
        eprintln!("Signature verified.");
    } else if envelope.signature.is_some() {
        eprintln!("WARNING: Envelope is signed but no --secret provided. Skipping verification.");
    }

    eprintln!(
        "Received from: {} ({})",
        envelope.sender.agent_id, envelope.sender.agent_type
    );
    eprintln!("Content type: {:?}", envelope.content_type);
    eprintln!("Payload size: {} bytes", envelope.payload_json.len());

    match envelope.content_type {
        TransportContentType::ContextPackage => {
            let tmp = std::env::temp_dir().join(format!(
                "lean-ctx-received-{}.{}",
                std::process::id(),
                crate::core::contracts::PACKAGE_EXTENSION
            ));
            if let Err(e) = std::fs::write(&tmp, &envelope.payload_json) {
                eprintln!("Error writing temp file: {e}");
                return;
            }
            if apply {
                let registry = match crate::core::context_package::LocalRegistry::open() {
                    Ok(r) => r,
                    Err(e) => {
                        eprintln!("ERROR: {e}");
                        return;
                    }
                };
                match registry.import_from_file(&tmp) {
                    Ok(manifest) => {
                        eprintln!("Imported: {} v{}", manifest.name, manifest.version);
                        apply_package(&manifest.name, &manifest.version, project_root);
                    }
                    Err(e) => eprintln!("ERROR: import failed: {e}"),
                }
            } else {
                eprintln!("Package saved to {}. Use --apply to import.", tmp.display());
            }
        }
        TransportContentType::HandoffBundle => {
            let out_path = std::path::Path::new(project_root)
                .join(".lean-ctx")
                .join("handoffs")
                .join("received-bundle.json");
            if let Some(parent) = out_path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            match std::fs::write(&out_path, &envelope.payload_json) {
                Ok(()) => eprintln!("Handoff bundle saved: {}", out_path.display()),
                Err(e) => eprintln!("Write failed: {e}"),
            }
        }
        _ => {
            eprintln!(
                "Content type {:?} — payload printed to stdout.",
                envelope.content_type
            );
            println!("{}", envelope.payload_json);
        }
    }
}

/// Parse `--flag=value` or `--flag value` from args.
fn parse_flag(args: &[String], flag: &str) -> Option<String> {
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

fn print_usage() {
    let ext = crate::core::contracts::PACKAGE_EXTENSION;
    eprintln!(
        "lean-ctx pack — Context Package Manager\n\n\
         SUBCOMMANDS:\n\
         \n\
         Create & Manage:\n\
         \x20 create   --name <name> [--version <v>] [--level 1|2|3] [--scope @ns] [--description <d>] [--author <a>] [--tags <t>] [--layers <l>]\n\
         \x20 list     List all installed packages\n\
         \x20 info     <name>[@version]  Show package details\n\
         \x20 remove   <name>[@version]  Remove a package\n\
         \n\
         Share & Distribute:\n\
         \x20 export   <name>[@version] [--output=<path>] [--sign] [--private] [--allow-secrets]  Export to .{ext} file (--sign: ed25519, required for publish; --private: hidden on the hosted registry; secret scan blocks credential-shaped content unless --allow-secrets)\n\
         \x20 import   <file.{ext}> [--apply]            Import from file\n\
         \x20 verify   <file.{ext}> [...]                Verify integrity + signature, no install (spec \u{a7}8/\u{a7}9; exit 1 on failure)\n\
         \x20 install  <name>[@version] [--file=<path>]    Apply package to current project\n\
         \x20 install  <ns>/<name>[@version]              Install from the hosted registry\n\
         \x20                                             (ctxpkg.com; verifies sha256 + signature, pins in ctxpkg.lock)\n\
         \x20 publish  <file.{ext}> [--registry <url>] [--token <ctxp_…>]  Publish (signed, scoped @ns/name)\n\
         \n\
         A2A Transport:\n\
         \x20 send     <file.{ext}> [--target <url>] [--to <agent>] [--secret <key>]\n\
         \x20 receive  <envelope.json> [--secret <key>] [--apply]\n\
         \n\
         Automation:\n\
         \x20 auto-load [<name>[@version]] [--off]          Manage auto-load packages\n\
         \n\
         PR Pack:\n\
         \x20 pr       [--base <ref>] [--format json|markdown] [--depth <n>]  PR context pack\n\
         \n\
         CONFORMANCE LEVELS:\n\
         \x20 1 (Basic)     Flat nodes, no edges (any tool can implement)\n\
         \x20 2 (Graph)     Typed nodes + edges, dependency resolution, graph-merge\n\
         \x20 3 (Cognitive)  Activation energy, Hebbian weights, temporal decay\n\
         \n\
         EXAMPLES:\n\
         \x20 lean-ctx pack create --name rust-patterns --description \"Rust best practices\"\n\
         \x20 lean-ctx pack create --name auth-service --level 2 --scope @company\n\
         \x20 lean-ctx pack export rust-patterns --output=rust-patterns.{ext}\n\
         \x20 lean-ctx pack send rust-patterns.{ext} --target http://remote:3344\n\
         \x20 lean-ctx pack receive envelope.json --secret mykey --apply\n\
         \x20 lean-ctx pack list\n"
    );
}
