use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::Path;

use crate::core::cache::SessionCache;
use crate::core::graph_provider::{self, GraphProvider};
use crate::core::protocol;
use crate::core::task_relevance::{compute_relevance, parse_task_hints};
use crate::tools::CrpMode;

const DEFAULT_MAX_FILES: usize = 10;

pub fn handle(
    cache: &mut SessionCache,
    root: &str,
    task: Option<&str>,
    changed_files: Option<&[String]>,
    budget_tokens: usize,
    max_files: Option<usize>,
    crp_mode: CrpMode,
) -> String {
    let project_root = if root.trim().is_empty() { "." } else { root };
    let open = graph_provider::open_or_build(project_root);
    let gp = open.as_ref().map(|o| &o.provider);

    let mut candidates: BTreeMap<String, f64> = BTreeMap::new();

    if let Some(t) = task
        && let Some(gp) = gp
    {
        let (task_files, task_keywords) = parse_task_hints(t);
        let mut relevance = compute_relevance(gp, &task_files, &task_keywords);
        crate::core::git_signals::apply_boost(&mut relevance, project_root);
        crate::core::diagnostics_store::apply_boost(&mut relevance);
        crate::core::editor_signal::apply_boost(&mut relevance);
        for r in relevance.iter().take(50) {
            if r.score < 0.1 {
                break;
            }
            candidates.insert(r.path.clone(), r.score);
        }
    }

    if let Some(changed) = changed_files {
        for p in changed {
            let rel = normalize_rel_path(p, project_root);
            if let Some(gp) = gp {
                for (path, dist) in blast_radius(gp, &rel, 2) {
                    let boost = 1.0 / (dist.max(1) as f64);
                    candidates
                        .entry(path)
                        .and_modify(|s| *s = (*s + boost).min(1.0))
                        .or_insert(boost.min(1.0));
                }
            } else {
                candidates.entry(rel).or_insert(1.0);
            }
        }
    }

    if candidates.is_empty() {
        return "ctx_prefetch: no candidates (provide task or changed_files)".to_string();
    }

    let mut scored: Vec<(String, f64)> = candidates.into_iter().collect();
    scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

    let max_files = max_files.unwrap_or(DEFAULT_MAX_FILES).max(1);
    let mut picked: Vec<String> = Vec::new();
    for (p, _s) in scored {
        picked.push(p);
        if picked.len() >= max_files {
            break;
        }
    }

    let mut total = 0usize;
    let mut prefetched: Vec<(String, String)> = Vec::new(); // (path, mode)
    let mut withheld: Vec<String> = Vec::new();
    let mut remaining = crate::core::limits::max_read_bytes().saturating_mul(max_files);
    let jail_root = Path::new(project_root);
    for p in &picked {
        let full = to_fs_path(project_root, p);
        let Ok((jailed, warning)) = crate::core::io_boundary::jail_and_check_path(
            "ctx_prefetch",
            Path::new(&full),
            jail_root,
        ) else {
            continue;
        };
        if warning.is_some() {
            continue;
        }
        let jailed_s = jailed.to_string_lossy().to_string();

        if crate::core::binary_detect::is_binary_file(&jailed_s) {
            continue;
        }
        let cap = crate::core::limits::max_read_bytes() as u64;
        if let Ok(meta) = std::fs::metadata(&jailed)
            && meta.len() > cap
        {
            continue;
        }

        // A prefetch exists to fill the session cache, a derived store: the
        // candidate is acquired through the common rooted admitted reader
        // (G01) and only a source whose ORIGINAL may be kept is prefetched.
        // A withheld source is named; a restricted one is left to an explicit
        // read, which delivers it without retaining it.
        let acquired = crate::tools::admitted_sources::read(
            &jailed_s,
            project_root,
            "ctx_prefetch",
            &mut remaining,
        );
        if let crate::tools::admitted_sources::SourceRead::Withheld(reason) = &acquired {
            withheld.push(crate::tools::admitted_sources::withheld_line(
                &protocol::shorten_path(&jailed_s),
                reason,
            ));
            let _ = cache.invalidate(&jailed_s);
            continue;
        }
        let Some(content) = acquired.storable() else {
            let _ = cache.invalidate(&jailed_s);
            continue;
        };
        let tokens = crate::core::tokens::count_tokens(content);
        total = total.saturating_add(tokens);

        let mode = if crate::tools::ctx_read::is_instruction_file(&jailed_s) {
            "full"
        } else if budget_tokens > 0 {
            let ratio = budget_tokens as f64 / total.max(1) as f64;
            if ratio >= 0.8 {
                "full"
            } else if ratio >= 0.4 {
                "map"
            } else {
                "signatures"
            }
        } else {
            "signatures"
        };

        // The read path admits only inside a request scope; bind it so the
        // cached copy is the admitted one for every caller.
        let _ = crate::tools::admitted_sources::as_agent_acquisition(project_root, || {
            crate::tools::ctx_read::handle_with_task_resolved(
                cache, &jailed_s, mode, crp_mode, task,
            )
        });
        prefetched.push((jailed_s, mode.to_string()));
    }

    let mut lines = vec![
        format!(
            "ctx_prefetch: prefetched {} file(s) (max_files={})",
            prefetched.len(),
            max_files
        ),
        format!("  root: {}", project_root),
    ];
    for (p, mode) in prefetched.iter().take(20) {
        let r = cache.get_file_ref(p);
        let short = protocol::shorten_path(p);
        lines.push(format!("  - [{r}] {short} mode={mode}"));
    }
    lines.extend(withheld.into_iter().map(|line| format!("  - {line}")));
    lines.join("\n")
}

fn blast_radius(gp: &GraphProvider, start_rel: &str, max_depth: usize) -> Vec<(String, usize)> {
    let all_edges = gp.edges();
    let mut adj: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for e in &all_edges {
        adj.entry(e.from.as_str()).or_default().push(e.to.as_str());
        adj.entry(e.to.as_str()).or_default().push(e.from.as_str());
    }

    let mut out = Vec::new();
    let mut q: VecDeque<(String, usize)> = VecDeque::new();
    let mut seen: BTreeSet<String> = BTreeSet::new();

    q.push_back((start_rel.to_string(), 0));
    seen.insert(start_rel.to_string());

    while let Some((node, depth)) = q.pop_front() {
        out.push((node.clone(), depth));
        if depth >= max_depth {
            continue;
        }
        if let Some(nbrs) = adj.get(node.as_str()) {
            for &n in nbrs {
                let ns = n.to_string();
                if seen.insert(ns.clone()) {
                    q.push_back((ns, depth + 1));
                }
            }
        }
    }
    out
}

fn normalize_rel_path(path: &str, project_root: &str) -> String {
    let p = Path::new(path);
    if p.is_absolute()
        && let Ok(stripped) = p.strip_prefix(project_root)
    {
        return stripped
            .to_string_lossy()
            .trim_start_matches('/')
            .to_string();
    }
    path.trim_start_matches('/').to_string()
}

fn to_fs_path(project_root: &str, rel_or_abs: &str) -> String {
    let p = Path::new(rel_or_abs);
    if p.is_absolute() {
        return rel_or_abs.to_string();
    }
    Path::new(project_root)
        .join(rel_or_abs)
        .to_string_lossy()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    const AWS_KEY: &str = concat!("AK", "IAIOSFODNN7EXAMPLE");

    fn fixture() -> (tempfile::TempDir, String) {
        let project = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(project.path())
            .unwrap()
            .to_string_lossy()
            .into_owned();
        (project, root)
    }

    fn cached(cache: &SessionCache, root: &str, name: &str) -> Option<String> {
        cache.get_full_content(&Path::new(root).join(name).to_string_lossy())
    }

    /// G01: the old prefetch read each candidate raw and handed it to the read
    /// path outside any request scope — the session cache then held a key
    /// that `ctx_read` masks, and a restricted source was retained.
    #[test]
    fn prefetch_caches_admitted_storable_sources_only() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
        let (_project, root) = fixture();
        std::fs::write(Path::new(&root).join("clean.rs"), "pub fn clean() {}\n").unwrap();
        std::fs::write(
            Path::new(&root).join("keyed.rs"),
            format!("pub fn connect() {{\n    let aws_key = \"{AWS_KEY}\";\n}}\n"),
        )
        .unwrap();
        std::fs::write(
            Path::new(&root).join("marked.rs"),
            "// TOP SECRET\npub fn plan() {}\n",
        )
        .unwrap();
        let changed = vec![
            "clean.rs".to_string(),
            "keyed.rs".to_string(),
            "marked.rs".to_string(),
        ];
        let mut cache = SessionCache::new();
        let out = handle(
            &mut cache,
            &root,
            None,
            Some(&changed),
            0,
            None,
            CrpMode::Off,
        );
        assert!(
            out.contains("clean.rs") && out.contains("keyed.rs"),
            "{out}"
        );
        assert!(!out.contains("marked.rs"), "{out}");
        assert!(
            cached(&cache, &root, "keyed.rs").is_none_or(|text| !text.contains(AWS_KEY)),
            "the session cache holds a raw key"
        );
        assert!(cached(&cache, &root, "marked.rs").is_none());
    }

    #[test]
    fn a_withheld_candidate_is_named_and_not_cached() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let policy = crate::core::policy::load(
            "name = \"ctx-prefetch-test\"\nversion = \"1.0.0\"\ndescription = \"test\"\n\
             [filters]\nclassification = \"block\"\nblocked_labels = [\"CONFIDENTIAL\"]\n",
        )
        .unwrap();
        let _policy = crate::core::policy::runtime::TestPolicyOverride::set(Some(policy));
        let (_project, root) = fixture();
        std::fs::write(
            Path::new(&root).join("plan.rs"),
            "CONFIDENTIAL\npub fn hidden_marker() {}\n",
        )
        .unwrap();
        let changed = vec!["plan.rs".to_string()];
        let mut cache = SessionCache::new();
        let out = handle(
            &mut cache,
            &root,
            None,
            Some(&changed),
            0,
            None,
            CrpMode::Off,
        );
        assert!(out.contains("prefetched 0 file(s)"), "{out}");
        assert!(out.contains("[withheld] "), "{out}");
        assert!(!out.contains("hidden_marker"), "{out}");
        assert!(cached(&cache, &root, "plan.rs").is_none());
    }
}
