use std::collections::{BTreeMap, BTreeSet};

use crate::core::task_relevance::RelevanceScore;

#[derive(Debug, Clone)]
pub struct PopDecision {
    pub included_modules: Vec<String>,
    pub excluded_modules: Vec<ExcludedModule>,
}

#[derive(Debug, Clone)]
pub struct ExcludedModule {
    pub module: String,
    pub candidate_files: usize,
    pub max_relevance: f64,
    pub reason: String,
}

pub fn decide_for_candidates(
    task: &str,
    project_root: &str,
    candidates: &[&RelevanceScore],
) -> PopDecision {
    let task_l = task.to_lowercase();

    let mut module_scores: BTreeMap<String, (usize, f64)> = BTreeMap::new(); // (count, max_score)
    for c in candidates {
        let m = module_for_path(&c.path, project_root);
        let e = module_scores.entry(m).or_insert((0, 0.0));
        e.0 += 1;
        e.1 = e.1.max(c.score);
    }

    let mut include: BTreeSet<String> = BTreeSet::new();
    for m in module_scores.keys() {
        if module_explicitly_mentioned(&task_l, m) {
            include.insert(m.clone());
        }
    }

    if include.is_empty() {
        for (m, (_n, max)) in &module_scores {
            if *max >= 0.7 {
                include.insert(m.clone());
            }
        }
    }

    let mut excluded = Vec::new();
    if !include.is_empty() {
        for (m, (count, max)) in &module_scores {
            if include.contains(m) {
                continue;
            }
            if *max >= 0.25 {
                continue;
            }
            if *count <= 1 {
                continue;
            }
            excluded.push(ExcludedModule {
                module: m.clone(),
                candidate_files: *count,
                max_relevance: *max,
                reason: format!("not mentioned by task, max_relevance={max:.2} (<0.25)"),
            });
        }
    }

    PopDecision {
        included_modules: include.into_iter().collect(),
        excluded_modules: excluded,
    }
}

pub fn filter_candidates_by_pop<'a>(
    project_root: &str,
    candidates: &'a [&RelevanceScore],
    pop: &PopDecision,
) -> Vec<&'a RelevanceScore> {
    if pop.excluded_modules.is_empty() {
        return candidates.to_vec();
    }
    let excluded: BTreeSet<&str> = pop
        .excluded_modules
        .iter()
        .map(|e| e.module.as_str())
        .collect();
    candidates
        .iter()
        .copied()
        .filter(|c| {
            let m = module_for_path(&c.path, project_root);
            !excluded.contains(m.as_str())
        })
        .collect()
}

pub fn module_for_path(path: &str, project_root: &str) -> String {
    let rel = path
        .strip_prefix(project_root)
        .unwrap_or(path)
        .trim_start_matches('/')
        .trim_start_matches('\\');
    rel.split('/')
        .next()
        .filter(|s| !s.is_empty())
        .unwrap_or(".")
        .to_string()
}

fn module_explicitly_mentioned(task_l: &str, module: &str) -> bool {
    if task_l.contains(module) {
        return true;
    }
    match module {
        "rust" => {
            task_l.contains("cargo")
                || task_l.contains("clippy")
                || task_l.contains("rust")
                || task_l.contains("ctx_")
                || task_l.contains("mcp")
        }
        "website" => {
            task_l.contains("website")
                || task_l.contains("docs")
                || task_l.contains("astro")
                || task_l.contains("tailwind")
                || task_l.contains("gitlab pages")
        }
        "packages" => {
            task_l.contains("vscode")
                || task_l.contains("chrome")
                || task_l.contains("extension")
                || task_l.contains("npm")
                || task_l.contains("node")
        }
        "cloud-infra" => task_l.contains("docker") || task_l.contains("infra"),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pop_excludes_unmentioned_module() {
        let root = "/repo";
        let task = "fix rust bug in ctx_read";
        let candidates = [
            RelevanceScore {
                path: "/repo/rust/src/tools/ctx_read.rs".to_string(),
                score: 0.9,
                recommended_mode: "full",
            },
            RelevanceScore {
                path: "/repo/website/src/index.astro".to_string(),
                score: 0.1,
                recommended_mode: "map",
            },
            RelevanceScore {
                path: "/repo/website/src/a.astro".to_string(),
                score: 0.05,
                recommended_mode: "map",
            },
        ];
        let refs: Vec<&RelevanceScore> = candidates.iter().collect();
        let pop = decide_for_candidates(task, root, &refs);
        assert!(pop.included_modules.contains(&"rust".to_string()));
        assert!(pop.excluded_modules.iter().any(|e| e.module == "website"));
        let kept = filter_candidates_by_pop(root, &refs, &pop);
        assert!(kept.iter().all(|c| !c.path.contains("/website/")));
    }

    #[test]
    fn pop_keeps_website_when_task_mentions_docs() {
        let root = "/repo";
        let task = "update website docs";
        let candidates = [
            RelevanceScore {
                path: "/repo/website/src/index.astro".to_string(),
                score: 0.2,
                recommended_mode: "map",
            },
            RelevanceScore {
                path: "/repo/rust/src/lib.rs".to_string(),
                score: 0.2,
                recommended_mode: "map",
            },
        ];
        let refs: Vec<&RelevanceScore> = candidates.iter().collect();
        let pop = decide_for_candidates(task, root, &refs);
        assert!(pop.included_modules.contains(&"website".to_string()));
        assert!(pop.excluded_modules.is_empty());
    }
}
