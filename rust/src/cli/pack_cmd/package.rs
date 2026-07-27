use std::path::PathBuf;

use super::{format_bytes, parse_flag, parse_pkg_ref};

#[derive(Default)]
struct PackCreateArgs {
    name: Option<String>,
    version: String,
    description: String,
    author: Option<String>,
    tags: Vec<String>,
    layers_str: Option<String>,
    level: u32,
    scope: Option<String>,
    private: bool,
    kind: Option<String>,
    from_dir: Option<String>,
}

fn parse_pack_create_args(args: &[String]) -> PackCreateArgs {
    let mut parsed = PackCreateArgs {
        version: "1.0.0".to_string(),
        level: 1,
        ..Default::default()
    };

    let mut i = 0;
    while i < args.len() {
        i += parse_pack_create_option(&mut parsed, args, i);
    }

    parsed
}

fn flag_value(args: &[String], index: usize) -> Option<String> {
    args.get(index).filter(|v| !v.starts_with("--")).cloned()
}

fn parse_pack_create_option(parsed: &mut PackCreateArgs, args: &[String], index: usize) -> usize {
    let arg = &args[index];
    match arg.as_str() {
        "create" => 1,
        "--private" => {
            parsed.private = true;
            1
        }
        "--kind" => {
            parsed.kind = flag_value(args, index + 1);
            2
        }
        "--from" => {
            parsed.from_dir = flag_value(args, index + 1);
            2
        }
        "--name" => {
            parsed.name = flag_value(args, index + 1);
            2
        }
        "--version" => {
            if let Some(value) = flag_value(args, index + 1) {
                parsed.version = value;
            }
            2
        }
        "--description" => {
            if let Some(value) = flag_value(args, index + 1) {
                parsed.description = value;
            }
            2
        }
        "--author" => {
            parsed.author = flag_value(args, index + 1);
            2
        }
        "--level" => {
            if let Some(value) = flag_value(args, index + 1) {
                parsed.level = parse_pack_level(&value);
            }
            2
        }
        "--scope" => {
            parsed.scope = flag_value(args, index + 1);
            2
        }
        _ => {
            parse_pack_create_inline_option(parsed, arg);
            1
        }
    }
}

fn parse_pack_create_inline_option(parsed: &mut PackCreateArgs, arg: &str) {
    if let Some(value) = arg.strip_prefix("--kind=") {
        parsed.kind = Some(value.to_string());
    } else if let Some(value) = arg.strip_prefix("--from=") {
        parsed.from_dir = Some(value.to_string());
    } else if let Some(value) = arg.strip_prefix("--name=") {
        parsed.name = Some(value.to_string());
    } else if let Some(value) = arg.strip_prefix("--version=") {
        parsed.version = value.to_string();
    } else if let Some(value) = arg.strip_prefix("--description=") {
        parsed.description = value.to_string();
    } else if let Some(value) = arg.strip_prefix("--author=") {
        parsed.author = Some(value.to_string());
    } else if let Some(value) = arg.strip_prefix("--tags=") {
        parsed.tags = value.split(',').map(|s| s.trim().to_string()).collect();
    } else if let Some(value) = arg.strip_prefix("--layers=") {
        parsed.layers_str = Some(value.to_string());
    } else if let Some(value) = arg.strip_prefix("--level=") {
        parsed.level = parse_pack_level(value);
    } else if let Some(value) = arg.strip_prefix("--scope=") {
        parsed.scope = Some(value.to_string());
    }
}

fn parse_pack_level(value: &str) -> u32 {
    value.parse::<u32>().unwrap_or(1).clamp(1, 3)
}

pub(super) fn cmd_pack_create(args: &[String], project_root: &str) {
    let parsed = parse_pack_create_args(args);
    let PackCreateArgs {
        name,
        version,
        description,
        author,
        tags,
        layers_str,
        level,
        scope,
        private,
        kind,
        from_dir,
    } = parsed;

    let Some(pkg_name) = name else {
        eprintln!("ERROR: --name is required for pack create");
        return;
    };

    // kind=skills (GH #727): a content pack built from a directory of files,
    // not from project stores — its own branch, everything else unchanged.
    match kind.as_deref() {
        None | Some("context") => {}
        Some("skills") => {
            let Some(dir) = from_dir else {
                eprintln!("ERROR: --from <dir> is required for --kind skills");
                eprintln!(
                    "Usage: lean-ctx pack create --kind skills --name @ns/name --from ./skills-dir"
                );
                return;
            };
            create_skills_pack(
                &pkg_name,
                &version,
                &description,
                author.as_deref(),
                tags,
                &dir,
            );
            return;
        }
        Some(other) => {
            eprintln!(
                "ERROR: unsupported --kind `{other}` for pack create (supported: context, skills)"
            );
            eprintln!("  kind=addon packs are built with `lean-ctx addon publish --check`.");
            return;
        }
    }

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

/// `pack create --kind skills` — build, sign and register a content pack
/// from a directory of skill files (GH #727).
fn create_skills_pack(
    name: &str,
    version: &str,
    description: &str,
    author: Option<&str>,
    tags: Vec<String>,
    dir: &str,
) {
    use crate::core::context_package::skills;

    let plan = match skills::build_skills_pack(
        std::path::Path::new(dir),
        name,
        version,
        description,
        author,
        tags,
    ) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("ERROR: {e}");
            return;
        }
    };

    let registry = match crate::core::context_package::LocalRegistry::open() {
        Ok(r) => r,
        Err(e) => {
            eprintln!("ERROR: cannot open registry: {e}");
            return;
        }
    };
    match registry.install(&plan.manifest, &plan.content) {
        Ok(pkg_dir) => {
            println!("Skills pack created successfully:");
            println!("  Name:     {}", plan.name);
            println!("  Version:  {}", plan.version);
            println!(
                "  Files:    {} ({} plaintext)",
                plan.file_count,
                format_bytes(plan.total_bytes as u64)
            );
            println!("  Signed:   ed25519 (verify with `lean-ctx pack verify`)");
            println!("  Location: {}", pkg_dir.display());
            let skills_root =
                skills::skills_dir(registry.root(), &plan.manifest.name, &plan.manifest.version);
            println!("  Materialized: {}", skills_root.display());
            println!(
                "\nPublish with: lean-ctx pack publish {}@{}",
                plan.name, plan.version
            );
        }
        Err(e) => eprintln!("ERROR: install failed: {e}"),
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

pub(super) fn cmd_pack_export(args: &[String]) {
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

/// `pack verify` — standalone conformance check (spec §8/§9), no install.
/// Exit code 0 = all files valid, 1 = any failure (CI-friendly).
pub(super) fn cmd_pack_verify(args: &[String]) {
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

pub(super) fn cmd_pack_publish(args: &[String]) {
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
