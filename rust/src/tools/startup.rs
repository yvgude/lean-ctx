use super::server::LeanCtxServer;

#[derive(Clone, Debug, Default)]
pub(super) struct StartupContext {
    pub(super) project_root: Option<String>,
    pub(super) shell_cwd: Option<String>,
}

/// Creates a new `LeanCtxServer` with default configuration.
pub fn create_server() -> LeanCtxServer {
    LeanCtxServer::new()
}

pub(super) fn has_project_marker(dir: &std::path::Path) -> bool {
    crate::core::pathutil::has_project_marker(dir)
}

pub(super) fn is_suspicious_root(dir: &std::path::Path) -> bool {
    let s = dir.to_string_lossy();
    s.contains("/.claude")
        || s.contains("/.codebuddy")
        || s.contains("/.codex")
        || s.contains("/.lmstudio")
        || s.contains("\\.claude")
        || s.contains("\\.codebuddy")
        || s.contains("\\.codex")
        || s.contains("\\.lmstudio")
}

pub(super) fn canonicalize_path(path: &std::path::Path) -> String {
    crate::core::pathutil::safe_canonicalize_or_self(path)
        .to_string_lossy()
        .to_string()
}

pub(super) fn detect_startup_context(
    explicit_project_root: Option<&str>,
    startup_cwd: Option<&std::path::Path>,
) -> StartupContext {
    let shell_cwd = startup_cwd.map(canonicalize_path);
    let project_root = explicit_project_root
        .map(|root| canonicalize_path(std::path::Path::new(root)))
        .or_else(|| {
            startup_cwd
                .and_then(maybe_derive_project_root_from_absolute)
                .map(|p| canonicalize_path(&p))
        });

    let shell_cwd = match (shell_cwd, project_root.as_ref()) {
        (Some(cwd), Some(root))
            if std::path::Path::new(&cwd).starts_with(std::path::Path::new(root)) =>
        {
            Some(cwd)
        }
        (_, Some(root)) => Some(root.clone()),
        (cwd, None) => cwd,
    };

    StartupContext {
        project_root,
        shell_cwd,
    }
}

pub(super) fn maybe_derive_project_root_from_absolute(
    abs: &std::path::Path,
) -> Option<std::path::PathBuf> {
    let mut cur = if abs.is_dir() {
        abs.to_path_buf()
    } else {
        abs.parent()?.to_path_buf()
    };
    loop {
        if has_project_marker(&cur) {
            return Some(crate::core::pathutil::safe_canonicalize_or_self(&cur));
        }
        if !cur.pop() {
            break;
        }
    }
    None
}

pub(crate) fn auto_consolidate_knowledge(project_root: &str) {
    use crate::core::knowledge::ProjectKnowledge;
    use crate::core::session::SessionState;
    use chrono::Utc;

    let Some(mut session) = SessionState::load_latest() else {
        return;
    };

    let watermark = session.last_consolidate_ts;

    let new_findings: Vec<_> = session
        .findings
        .iter()
        .filter(|f| match watermark {
            Some(ts) => f.timestamp > ts,
            None => true,
        })
        .collect();

    let new_decisions: Vec<_> = session
        .decisions
        .iter()
        .filter(|d| match watermark {
            Some(ts) => d.timestamp > ts,
            None => true,
        })
        .collect();

    if new_findings.is_empty() && new_decisions.is_empty() {
        return;
    }

    let Ok(policy) = crate::core::config::Config::load().memory_policy_effective() else {
        return;
    };
    // Load-modify-save under the shared in-process + cross-process lock so this
    // background consolidation merges onto the latest committed facts instead of
    // clobbering a concurrent foreground `remember`/`relate` write (issue #326).
    let _ = ProjectKnowledge::mutate_locked(project_root, |knowledge| {
        for finding in &new_findings {
            let key = if let Some(ref file) = finding.file {
                if let Some(line) = finding.line {
                    format!("{file}:{line}")
                } else {
                    file.clone()
                }
            } else {
                let slug: String = finding
                    .summary
                    .chars()
                    .take(60)
                    .collect::<String>()
                    .replace(' ', "-")
                    .to_lowercase();
                format!("finding-{slug}")
            };
            knowledge.remember("finding", &key, &finding.summary, &session.id, 0.7, &policy);
        }

        for decision in &new_decisions {
            let key = decision
                .summary
                .chars()
                .take(50)
                .collect::<String>()
                .replace(' ', "-")
                .to_lowercase();
            knowledge.remember(
                "decision",
                &key,
                &decision.summary,
                &session.id,
                0.85,
                &policy,
            );
        }

        let task_desc = session
            .task
            .as_ref()
            .map(|t| t.description.clone())
            .unwrap_or_default();

        let summary = format!(
            "Auto-consolidate session {}: {} — {} findings, {} decisions",
            session.id,
            task_desc,
            new_findings.len(),
            new_decisions.len()
        );
        knowledge.consolidate(&summary, vec![session.id.clone()], &policy);
    });

    session.last_consolidate_ts = Some(Utc::now());
    let _ = session.save();
}
