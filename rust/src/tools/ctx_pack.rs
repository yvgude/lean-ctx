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
        _ => "Unknown action. Use: pr".to_string(),
    }
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
                    serde_json::to_string_pretty(&json)
                        .unwrap_or_else(|e| format!("{{\"error\": \"serialization failed: {e}\"}}"))
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

    let tokens = count_tokens(&out);
    out.push_str(&format!("[ctx_pack pr: {tokens} tok]\n"));
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
        let raw =
            crate::tools::ctx_impact::handle("analyze", Some(&c.path), project_root, Some(depth));
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
    if input.contains("diff --git") || input.contains("\n+++ ") || input.starts_with("diff --git") {
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
            .ok()
            .is_some_and(|s| s.success());
        if ok {
            return Some(cand.to_string());
        }
    }
    None
}

fn dedup_changes(mut changes: Vec<ChangedFile>) -> Vec<ChangedFile> {
    let mut seen: BTreeMap<String, usize> = BTreeMap::new();
    let mut out: Vec<ChangedFile> = Vec::new();
    for c in changes.drain(..) {
        let key = c.path.clone();
        if let Some(i) = seen.get(&key) {
            out[*i] = c;
            continue;
        }
        seen.insert(key, out.len());
        out.push(c);
    }
    out
}
