use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde::Serialize;

use crate::core::artifacts::ResolvedArtifact;
use crate::core::tokens::count_tokens;

const DEFAULT_IMPACT_DEPTH: usize = 3;
const MAX_CHANGED_FILES_SHOWN: usize = 200;
const MAX_DIFF_BYTES: usize = 1_048_576; // 1 MiB

#[derive(Debug, Clone, Serialize)]
struct ChangedFile {
    path: String,
    status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    old_path: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct ImpactEntry {
    file: String,
    affected_files: Vec<String>,
}

#[derive(Debug, Serialize)]
struct PrPackJson {
    kind: &'static str,
    project_root: String,
    base: String,
    impact_depth: usize,
    changed_files: Vec<ChangedFile>,
    related_tests: Vec<String>,
    impacts: Vec<ImpactEntry>,
    context_artifacts: Vec<ResolvedArtifact>,
    warnings: Vec<String>,
    tokens: u64,
}

#[must_use]
pub fn handle(
    action: &str,
    project_root: &str,
    base: Option<&str>,
    format: Option<&str>,
    depth: Option<usize>,
    diff: Option<&str>,
) -> String {
    match action {
        "pr" => handle_pr(project_root, base, format, depth, diff),
        _ => "Unknown action. Use: pr, create, list, info, remove, install, export, import, auto_load, summary".to_string(),
    }
}

#[allow(clippy::too_many_arguments)]
pub fn handle_create(
    project_root: &str,
    name: &str,
    version: Option<&str>,
    description: Option<&str>,
    author: Option<&str>,
    tags: Option<&[String]>,
    layers: Option<&[String]>,
    level: Option<u32>,
    scope: Option<&str>,
) -> String {
    let version = version.unwrap_or("1.0.0");
    let description = description.unwrap_or("");
    let level = level.unwrap_or(1).clamp(1, 3);

    let requested_layers: Vec<&str> = layers.map_or_else(
        || vec!["knowledge", "graph", "session", "gotchas"],
        |l| l.iter().map(String::as_str).collect(),
    );

    let mut builder = crate::core::context_package::PackageBuilder::new(name, version)
        .description(description)
        .tags(tags.unwrap_or(&[]).to_vec())
        .level(level);

    if let Some(a) = author {
        builder = builder.author(a);
    }
    if let Some(s) = scope {
        builder = builder.scope(s);
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
                Err(e) => return format!("ERROR: cannot open registry: {e}"),
            };

            match registry.install(&manifest, &content) {
                Ok(dir) => {
                    let layers_str = manifest
                        .layers
                        .iter()
                        .map(crate::core::context_package::PackageLayer::as_str)
                        .collect::<Vec<_>>()
                        .join(", ");
                    format!(
                        "Package created:\n  Name: {}\n  Version: {}\n  Level: {}\n  Layers: {}\n  Knowledge facts: {}\n  Graph nodes: {}\n  Patterns: {}\n  Gotchas: {}\n  Size: {} bytes\n  Stored: {}",
                        manifest.name,
                        manifest.version,
                        manifest.conformance_level.unwrap_or(1),
                        layers_str,
                        manifest.stats.knowledge_facts,
                        manifest.stats.graph_nodes,
                        manifest.stats.pattern_count,
                        manifest.stats.gotcha_count,
                        manifest.integrity.byte_size,
                        dir.display()
                    )
                }
                Err(e) => format!("ERROR: install failed: {e}"),
            }
        }
        Err(e) => format!("ERROR: build failed: {e}"),
    }
}

#[must_use]
pub fn handle_list() -> String {
    let registry = match crate::core::context_package::LocalRegistry::open() {
        Ok(r) => r,
        Err(e) => return format!("ERROR: {e}"),
    };

    match registry.list() {
        Ok(entries) => {
            if entries.is_empty() {
                return "No packages installed.".to_string();
            }
            let mut out = String::new();
            out.push_str(&format!("{} package(s):\n", entries.len()));
            for e in &entries {
                out.push_str(&format!(
                    "- {} v{} [{}] ({} bytes){}\n",
                    e.name,
                    e.version,
                    e.layers.join(", "),
                    e.byte_size,
                    if e.auto_load { " [auto-load]" } else { "" }
                ));
            }
            out
        }
        Err(e) => format!("ERROR: {e}"),
    }
}

pub fn handle_info(name: &str, version: Option<&str>) -> String {
    let registry = match crate::core::context_package::LocalRegistry::open() {
        Ok(r) => r,
        Err(e) => return format!("ERROR: {e}"),
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
            let layers_str = manifest
                .layers
                .iter()
                .map(crate::core::context_package::PackageLayer::as_str)
                .collect::<Vec<_>>()
                .join(", ");
            let mut out = format!(
                "Package: {} v{}\nSchema: v{}\nLevel: {}\nLayers: {}\nDescription: {}\n",
                manifest.name,
                manifest.version,
                manifest.schema_version,
                manifest.conformance_level.unwrap_or(1),
                layers_str,
                manifest.description,
            );
            if let Some(ref a) = manifest.author {
                out.push_str(&format!("Author: {a}\n"));
            }
            if let Some(ref s) = manifest.scope {
                out.push_str(&format!("Scope: {s}\n"));
            }
            if !manifest.tags.is_empty() {
                out.push_str(&format!("Tags: {}\n", manifest.tags.join(", ")));
            }
            out.push_str(&format!(
                "Created: {}\nStats:\n  Knowledge facts: {}\n  Graph nodes: {}\n  Graph edges: {}\n  Patterns: {}\n  Gotchas: {}\n  Compression: {:.1}%\n  Est. tokens: ~{}\nIntegrity:\n  SHA256: {}\n  Size: {} bytes\n",
                manifest.created_at.format("%Y-%m-%d %H:%M UTC"),
                manifest.stats.knowledge_facts,
                manifest.stats.graph_nodes,
                manifest.stats.graph_edges,
                manifest.stats.pattern_count,
                manifest.stats.gotcha_count,
                manifest.stats.compression_ratio * 100.0,
                content.estimated_token_count(),
                manifest.integrity.sha256,
                manifest.integrity.byte_size,
            ));
            out
        }
        Err(e) => format!("ERROR: {e}"),
    }
}

#[must_use]
pub fn handle_remove(name: &str, version: Option<&str>) -> String {
    let registry = match crate::core::context_package::LocalRegistry::open() {
        Ok(r) => r,
        Err(e) => return format!("ERROR: {e}"),
    };

    match registry.remove(name, version) {
        Ok(0) => format!("No matching package found: {name}"),
        Ok(n) => format!("Removed {n} package(s)."),
        Err(e) => format!("ERROR: {e}"),
    }
}

#[must_use]
pub fn handle_install(name: &str, version: Option<&str>, project_root: &str) -> String {
    let registry = match crate::core::context_package::LocalRegistry::open() {
        Ok(r) => r,
        Err(e) => return format!("ERROR: {e}"),
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
            match crate::core::context_package::load_package(&manifest, &content, project_root) {
                Ok(report) => format!("{report}\nPackage applied successfully."),
                Err(e) => format!("ERROR: load failed: {e}"),
            }
        }
        Err(e) => format!("ERROR: {e}"),
    }
}

pub fn handle_export(name: &str, version: Option<&str>, output: Option<&str>) -> String {
    let registry = match crate::core::context_package::LocalRegistry::open() {
        Ok(r) => r,
        Err(e) => return format!("ERROR: {e}"),
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

    let out_path = output.map_or_else(
        || crate::core::contracts::default_package_filename(name, ver),
        ToString::to_string,
    );

    match registry.export_to_file(name, ver, &std::path::PathBuf::from(&out_path)) {
        Ok(bytes) => format!("Exported: {out_path} ({bytes} bytes)"),
        Err(e) => format!("ERROR: {e}"),
    }
}

pub fn handle_import(file_path: &str, apply: bool, project_root: &str) -> String {
    let registry = match crate::core::context_package::LocalRegistry::open() {
        Ok(r) => r,
        Err(e) => return format!("ERROR: {e}"),
    };

    match registry.import_from_file(std::path::Path::new(file_path)) {
        Ok(manifest) => {
            let layers_str = manifest
                .layers
                .iter()
                .map(crate::core::context_package::PackageLayer::as_str)
                .collect::<Vec<_>>()
                .join(", ");
            let mut out = format!(
                "Imported: {} v{}\n  Layers: {}\n  Size: {} bytes\n",
                manifest.name, manifest.version, layers_str, manifest.integrity.byte_size,
            );
            if apply {
                match crate::core::context_package::LocalRegistry::open() {
                    Ok(reg) => match reg.load_package(&manifest.name, &manifest.version) {
                        Ok((m, c)) => {
                            match crate::core::context_package::load_package(&m, &c, project_root) {
                                Ok(report) => {
                                    out.push_str(&format!("{report}\nPackage applied."));
                                }
                                Err(e) => out.push_str(&format!("ERROR applying: {e}")),
                            }
                        }
                        Err(e) => out.push_str(&format!("ERROR loading: {e}")),
                    },
                    Err(e) => out.push_str(&format!("ERROR: {e}")),
                }
            }
            out
        }
        Err(e) => format!("ERROR: import failed: {e}"),
    }
}

#[must_use]
pub fn handle_auto_load(name: Option<&str>, version: Option<&str>, enable: bool) -> String {
    let registry = match crate::core::context_package::LocalRegistry::open() {
        Ok(r) => r,
        Err(e) => return format!("ERROR: {e}"),
    };

    let Some(name) = name else {
        return match registry.auto_load_packages() {
            Ok(entries) => {
                if entries.is_empty() {
                    "No packages set for auto-load.".to_string()
                } else {
                    let mut out = "Auto-load packages:\n".to_string();
                    for e in &entries {
                        out.push_str(&format!("- {} v{}\n", e.name, e.version));
                    }
                    out
                }
            }
            Err(e) => format!("ERROR: {e}"),
        };
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

    match registry.set_auto_load(name, ver, enable) {
        Ok(()) => {
            if enable {
                format!("Auto-load enabled for {name}@{ver}")
            } else {
                format!("Auto-load disabled for {name}@{ver}")
            }
        }
        Err(e) => format!("ERROR: {e}"),
    }
}

#[must_use]
pub fn handle_summary(project_root: &str) -> String {
    let phash = crate::core::project_hash::hash_project_root(project_root);

    let registry = match crate::core::context_package::LocalRegistry::open() {
        Ok(r) => r,
        Err(e) => return format!("ERROR: {e}"),
    };

    let entries = registry.list().unwrap_or_default();
    let matching: Vec<_> = entries.iter().collect();

    let mut out = format!("Project: {project_root}\nProject hash: {phash}\n");
    out.push_str(&format!("Installed packages: {}\n", matching.len()));

    if !matching.is_empty() {
        out.push_str("\nPackages:\n");
        for e in &matching {
            out.push_str(&format!(
                "- {} v{} [{}]{}\n",
                e.name,
                e.version,
                e.layers.join(", "),
                if e.auto_load { " [auto-load]" } else { "" }
            ));
        }
    }

    let auto_count = matching.iter().filter(|e| e.auto_load).count();
    out.push_str(&format!("Auto-load: {auto_count} package(s)\n"));
    out
}

fn handle_pr(
    project_root: &str,
    base: Option<&str>,
    format: Option<&str>,
    depth: Option<usize>,
    diff: Option<&str>,
) -> String {
    let root = project_root.to_string();
    let base = base.map_or_else(
        || detect_default_base(&root).unwrap_or_else(|| "HEAD~1".to_string()),
        ToString::to_string,
    );
    let impact_depth = depth.unwrap_or(DEFAULT_IMPACT_DEPTH).max(1);

    let mut warnings: Vec<String> = Vec::new();
    let mut changed = if let Some(d) = diff {
        if d.len() > MAX_DIFF_BYTES {
            warnings.push(format!(
                "Diff input too large ({} bytes, limit {MAX_DIFF_BYTES}). Truncating at char boundary.",
                d.len()
            ));
            let mut boundary = MAX_DIFF_BYTES;
            while boundary > 0 && !d.is_char_boundary(boundary) {
                boundary -= 1;
            }
            let truncated = &d[..boundary];
            parse_changes_from_input(truncated)
        } else {
            parse_changes_from_input(d)
        }
    } else {
        git_diff_name_status(&root, &base, &mut warnings)
    };

    if changed.len() > MAX_CHANGED_FILES_SHOWN {
        warnings.push(format!(
            "Too many changed files ({}). Truncating to {MAX_CHANGED_FILES_SHOWN}.",
            changed.len()
        ));
        changed.truncate(MAX_CHANGED_FILES_SHOWN);
    }

    let related_tests = collect_related_tests(&changed, &root);
    let impacts = collect_impacts(&changed, &root, impact_depth);
    let context_artifacts = collect_relevant_artifacts(&changed, &root, &mut warnings);

    let format = format.unwrap_or("markdown");
    match format {
        "json" => {
            let mut json = PrPackJson {
                kind: "leanctx.pr_pack",
                project_root: root,
                base,
                impact_depth,
                changed_files: changed,
                related_tests,
                impacts,
                context_artifacts,
                warnings,
                tokens: 0,
            };
            match serde_json::to_string_pretty(&json) {
                Ok(s) => {
                    json.tokens = count_tokens(&s) as u64;
                    serde_json::to_string_pretty(&json).unwrap()
                }
                Err(e) => format!("{{\"error\": \"serialization failed: {e}\"}}"),
            }
        }
        _ => format_markdown(
            project_root,
            &base,
            impact_depth,
            &changed,
            &related_tests,
            &impacts,
            &context_artifacts,
            &warnings,
        ),
    }
}

fn format_markdown(
    project_root: &str,
    base: &str,
    impact_depth: usize,
    changed: &[ChangedFile],
    related_tests: &[String],
    impacts: &[ImpactEntry],
    artifacts: &[ResolvedArtifact],
    warnings: &[String],
) -> String {
    let mut out = String::new();
    out.push_str("# PR Context Pack\n\n");
    out.push_str(&format!("- Project root: `{project_root}`\n"));
    out.push_str(&format!("- Base: `{base}`\n"));
    out.push_str(&format!("- Impact depth: `{impact_depth}`\n\n"));

    if !warnings.is_empty() {
        out.push_str("## Warnings\n");
        for w in warnings {
            out.push_str(&format!("- {w}\n"));
        }
        out.push('\n');
    }

    out.push_str("## Changed files\n");
    for c in changed {
        match &c.old_path {
            Some(old) => out.push_str(&format!("- `{}` ({}) ← `{old}`\n", c.path, c.status)),
            None => out.push_str(&format!("- `{}` ({})\n", c.path, c.status)),
        }
    }
    out.push('\n');

    if !artifacts.is_empty() {
        out.push_str("## Context artifacts\n");
        for a in artifacts {
            let kind = if a.is_dir { "dir" } else { "file" };
            let exists = if a.exists { "exists" } else { "missing" };
            out.push_str(&format!(
                "- `{}` ({kind}, {exists}) — {}\n",
                a.path, a.description
            ));
        }
        out.push('\n');
    }

    if !related_tests.is_empty() {
        out.push_str("## Related tests\n");
        for t in related_tests {
            out.push_str(&format!("- `{t}`\n"));
        }
        out.push('\n');
    }

    if !impacts.is_empty() {
        out.push_str("## Impact (property graph)\n");
        for imp in impacts {
            out.push_str(&format!(
                "- `{}`: {} affected files\n",
                imp.file,
                imp.affected_files.len()
            ));
            for f in imp.affected_files.iter().take(30) {
                out.push_str(&format!("  - `{f}`\n"));
            }
            if imp.affected_files.len() > 30 {
                out.push_str("  - ...\n");
            }
        }
        out.push('\n');
    }

    out
}

fn collect_related_tests(changed: &[ChangedFile], project_root: &str) -> Vec<String> {
    let mut all: BTreeSet<String> = BTreeSet::new();
    for c in changed {
        for t in crate::tools::ctx_review::find_related_tests(&c.path, project_root) {
            all.insert(t);
        }
    }
    all.into_iter().collect()
}

fn collect_impacts(changed: &[ChangedFile], project_root: &str, depth: usize) -> Vec<ImpactEntry> {
    let mut out = Vec::new();
    for c in changed {
        if c.status == "D" {
            continue;
        }
        let raw = crate::tools::ctx_impact::handle(
            "analyze",
            Some(&c.path),
            project_root,
            Some(depth),
            None,
        );
        let affected = parse_ctx_impact_output(&raw);
        out.push(ImpactEntry {
            file: c.path.clone(),
            affected_files: affected,
        });
    }
    out
}

fn parse_ctx_impact_output(raw: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for line in raw.lines() {
        let l = line.trim_end();
        if let Some(rest) = l.strip_prefix("  ") {
            let item = rest.trim().to_string();
            if item.starts_with("...") {
                continue;
            }
            if !item.is_empty() {
                out.push(item);
            }
        }
    }
    out.sort();
    out.dedup();
    out
}

fn collect_relevant_artifacts(
    changed: &[ChangedFile],
    project_root: &str,
    warnings: &mut Vec<String>,
) -> Vec<ResolvedArtifact> {
    let root = Path::new(project_root);
    let resolved = crate::core::artifacts::load_resolved(root);
    warnings.extend(resolved.warnings);

    let mut out: Vec<ResolvedArtifact> = Vec::new();
    for a in resolved.artifacts {
        if !a.exists {
            continue;
        }
        if is_artifact_relevant(&a, changed) {
            out.push(a);
        }
    }
    out.sort_by(|a, b| a.path.cmp(&b.path).then_with(|| a.name.cmp(&b.name)));
    out
}

fn is_artifact_relevant(a: &ResolvedArtifact, changed: &[ChangedFile]) -> bool {
    if a.path.is_empty() {
        return false;
    }
    if a.is_dir {
        let prefix = if a.path.ends_with('/') {
            a.path.clone()
        } else {
            format!("{}/", a.path)
        };
        return changed.iter().any(|c| c.path.starts_with(&prefix));
    }
    changed.iter().any(|c| c.path == a.path)
}

fn parse_changes_from_input(input: &str) -> Vec<ChangedFile> {
    if input.contains("diff --git") || input.contains("\n+++ ") {
        let paths = parse_unified_diff_paths(input);
        let mut out = Vec::new();
        for p in paths {
            out.push(ChangedFile {
                path: p,
                status: "M".to_string(),
                old_path: None,
            });
        }
        return dedup_changes(out);
    }

    let mut out = Vec::new();
    for line in input.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let parts: Vec<&str> = trimmed.split_whitespace().collect();
        if parts.len() >= 2 {
            let status = parts[0].to_string();
            if status.starts_with('R') && parts.len() >= 3 {
                out.push(ChangedFile {
                    path: parts[2].to_string(),
                    status: "R".to_string(),
                    old_path: Some(parts[1].to_string()),
                });
            } else {
                out.push(ChangedFile {
                    path: parts[1].to_string(),
                    status: status.chars().next().unwrap_or('M').to_string(),
                    old_path: None,
                });
            }
        } else {
            out.push(ChangedFile {
                path: trimmed.to_string(),
                status: "M".to_string(),
                old_path: None,
            });
        }
    }
    dedup_changes(out)
}

fn parse_unified_diff_paths(diff: &str) -> Vec<String> {
    let mut out: BTreeSet<String> = BTreeSet::new();
    for line in diff.lines() {
        if let Some(rest) = line.strip_prefix("+++ b/") {
            let p = rest.trim();
            if !p.is_empty() && p != "/dev/null" {
                out.insert(p.to_string());
            }
        }
        if let Some(rest) = line.strip_prefix("--- a/") {
            let p = rest.trim();
            if !p.is_empty() && p != "/dev/null" {
                out.insert(p.to_string());
            }
        }
    }
    out.into_iter().collect()
}

fn git_diff_name_status(
    project_root: &str,
    base: &str,
    warnings: &mut Vec<String>,
) -> Vec<ChangedFile> {
    let out = std::process::Command::new("git")
        .args(["diff", "--name-status", &format!("{base}...HEAD")])
        .current_dir(project_root)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output();
    let Ok(o) = out else {
        warnings.push("Failed to execute git diff".to_string());
        return Vec::new();
    };
    if !o.status.success() {
        let stderr = String::from_utf8_lossy(&o.stderr);
        warnings.push(format!("git diff failed: {}", stderr.trim()));
        return Vec::new();
    }
    let s = String::from_utf8_lossy(&o.stdout);
    parse_changes_from_input(&s)
}

fn detect_default_base(project_root: &str) -> Option<String> {
    for cand in ["origin/main", "origin/master", "main", "master"] {
        let ok = std::process::Command::new("git")
            .args(["rev-parse", "--verify", cand])
            .current_dir(project_root)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|s| s.success());
        if ok {
            return Some(cand.to_string());
        }
    }
    None
}

fn dedup_changes(changes: Vec<ChangedFile>) -> Vec<ChangedFile> {
    let mut seen: BTreeMap<String, usize> = BTreeMap::new();
    let mut out: Vec<ChangedFile> = Vec::new();
    for c in changes {
        let key = c.path.clone();
        if let Some(&i) = seen.get(&key) {
            out[i] = c;
        } else {
            seen.insert(key, out.len());
            out.push(c);
        }
    }
    out
}
