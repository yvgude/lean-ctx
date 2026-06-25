use chrono::Utc;

use crate::core::knowledge::ProjectKnowledge;
use crate::core::session::SessionState;

#[derive(Debug, Clone, Copy)]
pub struct ConsolidationBudgets {
    pub max_decisions: usize,
    pub max_findings: usize,
}

impl Default for ConsolidationBudgets {
    fn default() -> Self {
        Self {
            max_decisions: 5,
            max_findings: 8,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ConsolidationOutcome {
    pub promoted: u32,
    pub promoted_decisions: u32,
    pub promoted_findings: u32,
    pub lifecycle_archived: usize,
    pub lifecycle_remaining: usize,
}

pub fn consolidate_latest(
    project_root: &str,
    budgets: ConsolidationBudgets,
) -> Result<ConsolidationOutcome, String> {
    // Consolidate the session for the explicitly given project root rather than
    // whatever the process cwd resolves to. This is both correct (the caller
    // already knows the project) and required after session loads became
    // strictly project-scoped (#2362): load_latest() is cwd-bound and would miss
    // a session whose root differs from cwd.
    let session = SessionState::load_latest_for_project_root(project_root)
        .ok_or_else(|| "no active session".to_string())?;
    let policy = crate::core::config::Config::load()
        .memory_policy_effective()
        .map_err(|e| format!("invalid memory policy: {e}"))?;

    // Read-modify-write under the SAME in-process + cross-process lock that
    // foreground `remember`/`feedback` use. Loading *inside* the lock is what
    // keeps this background pass from clobbering facts a concurrent tool call
    // commits in between (issue #326): a bare `load_or_create` + `save` here
    // loses those updates and silently drops just-remembered facts (e.g. a
    // following `relate` then reports "no current fact exists").
    let (_knowledge, outcome) = ProjectKnowledge::mutate_locked(project_root, |knowledge| {
        let mut promoted_decisions = 0u32;
        let mut promoted_findings = 0u32;

        let mut decisions = session.decisions.clone();
        decisions.sort_by_key(|x| std::cmp::Reverse(x.timestamp));
        decisions.truncate(budgets.max_decisions);
        for d in &decisions {
            let key = slug_key(&d.summary, 50);
            knowledge.remember("decision", &key, &d.summary, &session.id, 0.9, &policy);
            promoted_decisions += 1;
        }

        let mut findings = session.findings.clone();
        findings.sort_by_key(|x| std::cmp::Reverse(x.timestamp));
        let mut kept = Vec::new();
        for f in &findings {
            if kept.len() >= budgets.max_findings {
                break;
            }
            if finding_salience(&f.summary) < 45 {
                continue;
            }
            kept.push(f.clone());
        }

        for f in &kept {
            let key = if let Some(ref file) = f.file {
                if let Some(line) = f.line {
                    format!("{file}:{line}")
                } else {
                    file.clone()
                }
            } else {
                format!("finding-{}", slug_key(&f.summary, 36))
            };
            knowledge.remember("finding", &key, &f.summary, &session.id, 0.75, &policy);
            promoted_findings += 1;
        }

        // One compact history entry (no prose output to user; stored for auditability).
        let task_desc = session
            .task
            .as_ref()
            .map_or_else(|| "(no task)".into(), |t| t.description.clone());
        let summary = format!(
            "consolidate@{} session={} task=\"{}\" decisions={} findings={}",
            Utc::now().format("%Y-%m-%d"),
            session.id,
            task_desc,
            promoted_decisions,
            promoted_findings
        );
        knowledge.consolidate(&summary, vec![session.id.clone()], &policy);

        let lifecycle = knowledge.run_memory_lifecycle(&policy);
        ConsolidationOutcome {
            promoted: promoted_decisions + promoted_findings,
            promoted_decisions,
            promoted_findings,
            lifecycle_archived: lifecycle.archived_count,
            lifecycle_remaining: lifecycle.remaining_facts,
        }
    })?;

    let _ = crate::core::events::emit(crate::core::events::EventKind::KnowledgeUpdate {
        category: "memory".to_string(),
        key: "consolidation".to_string(),
        action: "run".to_string(),
    });

    Ok(outcome)
}

fn slug_key(s: &str, max: usize) -> String {
    let mut out = String::new();
    for ch in s.chars() {
        if out.len() >= max {
            break;
        }
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
        } else if (ch.is_whitespace() || ch == '-' || ch == '_')
            && !out.ends_with('-')
            && !out.is_empty()
        {
            out.push('-');
        }
    }
    out.trim_matches('-').to_string()
}

fn finding_salience(summary: &str) -> u32 {
    let s = summary.to_lowercase();
    let mut score = 20u32;

    let boosts = [
        ("error", 25),
        ("failed", 25),
        ("panic", 30),
        ("assert", 20),
        ("forbidden", 25),
        ("timeout", 20),
        ("deadlock", 25),
        ("security", 25),
        ("vuln", 25),
        ("e0", 15), // rust error codes often start with E0xxx
    ];

    for (pat, b) in boosts {
        if s.contains(pat) {
            score = score.saturating_add(b);
        }
    }

    score
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn consolidate_promotes_decisions_and_salient_findings_only() {
        let _lock = crate::core::data_dir::test_env_lock();
        let tmp = tempfile::tempdir().expect("tempdir");
        crate::test_env::set_var(
            "LEAN_CTX_DATA_DIR",
            tmp.path().to_string_lossy().to_string(),
        );

        let project_root = tmp.path().join("proj");
        std::fs::create_dir_all(&project_root).expect("mkdir");
        let project_root_str = project_root.to_string_lossy().to_string();

        let mut session = SessionState::new();
        session.project_root = Some(project_root_str.clone());
        session.add_decision("Use archive-only memory lifecycle", None);
        session.add_finding(None, None, "panic: index out of bounds");
        session.add_finding(None, None, "just a note");
        session.save().expect("save session");

        let out = consolidate_latest(
            &project_root_str,
            ConsolidationBudgets {
                max_decisions: 5,
                max_findings: 5,
            },
        )
        .expect("consolidate");
        assert!(out.promoted_decisions >= 1);
        assert!(out.promoted_findings >= 1);

        let k = ProjectKnowledge::load(&project_root_str).expect("knowledge saved");
        let active = k.facts.iter().filter(|f| f.is_current()).count();
        assert!(active >= 2, "expected promoted facts");

        crate::test_env::remove_var("LEAN_CTX_DATA_DIR");
    }
}
