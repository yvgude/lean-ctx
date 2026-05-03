use crate::core::gotcha_tracker::GotchaStore;

pub fn reminders_for_task(project_root: &str, task: &str) -> Vec<String> {
    let task_terms = tokenize(task);
    if task_terms.is_empty() {
        return Vec::new();
    }

    let store = GotchaStore::load(project_root);
    if store.gotchas.is_empty() {
        return Vec::new();
    }

    #[derive(Clone)]
    struct Scored {
        line: String,
        score: f32,
    }

    let mut scored: Vec<Scored> = Vec::new();

    for g in &store.gotchas {
        let searchable = format!(
            "{} {} {} {}",
            g.trigger.to_lowercase(),
            g.resolution.to_lowercase(),
            g.tags.join(" ").to_lowercase(),
            g.category.short_label().to_lowercase()
        );
        let matches = task_terms
            .iter()
            .filter(|t| searchable.contains(*t))
            .count();
        if matches == 0 {
            continue;
        }
        let rel = matches as f32 / task_terms.len() as f32;
        let sev = g.severity.multiplier();
        let rec = (g.prevented_count as f32).ln_1p().min(3.0) / 3.0; // 0..1
        let score = rel * g.confidence * sev * (1.0 + rec * 0.2);

        let mut line = format!(
            "gotcha: {} → {}",
            sanitize_one_line(&g.trigger),
            sanitize_one_line(&g.resolution)
        );
        line = truncate_chars(&line, crate::core::budgets::PROSPECTIVE_REMINDER_MAX_CHARS);
        scored.push(Scored { line, score });
    }

    scored.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.line.cmp(&b.line))
    });

    scored
        .into_iter()
        .take(crate::core::budgets::PROSPECTIVE_REMINDERS_LIMIT)
        .map(|s| format!("[remember] {}", s.line))
        .collect()
}

fn tokenize(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    for ch in s.chars() {
        if ch.is_ascii_alphanumeric() {
            cur.push(ch.to_ascii_lowercase());
        } else if !cur.is_empty() {
            if cur.len() >= 3 {
                out.push(cur.clone());
            }
            cur.clear();
        }
    }
    if !cur.is_empty() && cur.len() >= 3 {
        out.push(cur);
    }
    out.sort();
    out.dedup();
    out
}

fn sanitize_one_line(s: &str) -> String {
    let mut t = s.replace(['\n', '\r'], " ");
    t = t.replace('`', "");
    while t.contains("  ") {
        t = t.replace("  ", " ");
    }
    t.trim().to_string()
}

fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out = String::new();
    for (i, ch) in s.chars().enumerate() {
        if i + 1 >= max {
            break;
        }
        out.push(ch);
    }
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::gotcha_tracker::{Gotcha, GotchaCategory, GotchaSeverity, GotchaSource};
    use chrono::Utc;

    #[test]
    fn reminders_budgeted() {
        let _lock = crate::core::data_dir::test_env_lock();
        let tmp = tempfile::tempdir().expect("tempdir");
        std::env::set_var(
            "LEAN_CTX_DATA_DIR",
            tmp.path().to_string_lossy().to_string(),
        );

        let project_root = tmp.path().join("proj");
        std::fs::create_dir_all(&project_root).expect("mkdir");
        let project_root_str = project_root.to_string_lossy().to_string();

        let mut store = GotchaStore::load(&project_root_str);
        for i in 0..10 {
            store.gotchas.push(Gotcha {
                id: format!("g{i}"),
                category: GotchaCategory::Build,
                severity: GotchaSeverity::Warning,
                trigger: format!("cargo build error E050{i}"),
                resolution: "split borrows".to_string(),
                file_patterns: vec![],
                occurrences: 2,
                session_ids: vec!["s1".to_string()],
                first_seen: Utc::now(),
                last_seen: Utc::now(),
                confidence: 0.8,
                source: GotchaSource::AutoDetected {
                    command: "cargo build".to_string(),
                    exit_code: 1,
                },
                prevented_count: 0,
                tags: vec!["rust".to_string()],
            });
        }

        // Persist gotchas where GotchaStore::load expects them.
        store.save(&project_root_str).expect("save");

        let reminders = reminders_for_task(&project_root_str, "fix cargo build error E0502 borrow");
        assert!(reminders.len() <= crate::core::budgets::PROSPECTIVE_REMINDERS_LIMIT);

        std::env::remove_var("LEAN_CTX_DATA_DIR");
    }
}
