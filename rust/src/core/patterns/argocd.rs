//! Argo CD (`argocd app get`/`sync`/`list`) output compression.
//!
//! `argocd app get`/`sync` print a key/value metadata header followed by a
//! resource table where most rows are `Synced`/`Healthy` (noise). We keep the
//! important status keys (sync/health/phase/message/url) and only the resource
//! rows that are *not* both Synced and Healthy, plus a kept/total tally.

use crate::core::compressor::strip_ansi;

const KEYS: &[&str] = &[
    "Name:",
    "URL:",
    "Project:",
    "Sync Status:",
    "Health Status:",
    "Operation:",
    "Phase:",
    "Message:",
];

pub fn compress(command: &str, output: &str) -> Option<String> {
    let sub = command
        .trim()
        .strip_prefix("argocd")
        .map_or("", str::trim_start);
    if sub.starts_with("app get") || sub.starts_with("app sync") || sub.starts_with("app wait") {
        return Some(compress_app(output));
    }
    if sub.starts_with("app list") {
        return Some(compress_table(output, "argocd app list"));
    }
    None
}

fn compress_app(output: &str) -> String {
    let mut header: Vec<String> = Vec::new();
    let mut table: Vec<String> = Vec::new();
    let mut in_table = false;

    for raw in output.lines() {
        let line = strip_ansi(raw);
        let t = line.trim();
        if t.is_empty() {
            continue;
        }
        if is_table_header(t) {
            in_table = true;
            table.push(t.to_string());
            continue;
        }
        if in_table {
            table.push(t.to_string());
            continue;
        }
        if KEYS.iter().any(|k| t.starts_with(k)) || t.to_ascii_lowercase().contains("error") {
            header.push(t.to_string());
        }
    }

    let mut parts = header;
    if table.len() > 1 {
        parts.push(filter_rows(&table));
    }
    if parts.is_empty() {
        return "argocd: ok".to_string();
    }
    parts.join("\n")
}

fn compress_table(output: &str, label: &str) -> String {
    let rows: Vec<&str> = output.lines().map(str::trim_end).collect();
    let table: Vec<String> = rows
        .iter()
        .map(|l| strip_ansi(l).trim().to_string())
        .filter(|l| !l.is_empty())
        .collect();
    if table.is_empty() {
        return format!("{label}: ok");
    }
    filter_rows(&table)
}

/// Keep the header row + rows that are not both Synced and Healthy.
fn filter_rows(table: &[String]) -> String {
    let header = &table[0];
    let mut kept: Vec<String> = vec![header.clone()];
    let mut healthy = 0usize;
    for row in &table[1..] {
        if row.contains("Synced") && row.contains("Healthy") {
            healthy += 1;
        } else {
            kept.push(row.clone());
        }
    }
    let total = table.len() - 1;
    let mut s = kept.join("\n");
    if healthy > 0 {
        s.push_str(&format!(
            "\n({healthy}/{total} resources Synced+Healthy, hidden)"
        ));
    }
    s
}

fn is_table_header(t: &str) -> bool {
    let u = t.to_ascii_uppercase();
    (u.starts_with("GROUP") || u.starts_with("NAME") || u.starts_with("TIMESTAMP"))
        && u.contains("STATUS")
        && u.contains("HEALTH")
}

#[cfg(test)]
mod tests {
    use super::*;

    const GET: &str = "Name:               argocd/myapp\nProject:            default\nURL:                https://argocd.example.com/applications/myapp\nSync Status:        Synced to HEAD (abc1234)\nHealth Status:      Healthy\n\nGROUP  KIND        NAMESPACE  NAME      STATUS     HEALTH       HOOK  MESSAGE\n       Service     myns       mysvc     Synced     Healthy            service/mysvc created\napps   Deployment  myns      mydeploy   OutOfSync  Progressing        waiting for rollout\n";

    #[test]
    fn keeps_status_and_unhealthy_rows() {
        let r = compress("argocd app get myapp", GET).unwrap();
        assert!(r.contains("Sync Status:"), "{r}");
        assert!(r.contains("Health Status:      Healthy"), "{r}");
        assert!(r.contains("mydeploy"), "keeps unhealthy row: {r}");
        assert!(r.contains("OutOfSync"), "{r}");
        assert!(!r.contains("mysvc"), "drops synced+healthy row: {r}");
        assert!(r.contains("1/2 resources"), "tally: {r}");
    }

    #[test]
    fn app_list_keeps_header_and_unhealthy() {
        let list = "NAME    CLUSTER  NAMESPACE  PROJECT  STATUS     HEALTH       SYNCPOLICY\napp-a   in-cl    ns-a       default  Synced     Healthy      Automated\napp-b   in-cl    ns-b       default  OutOfSync  Degraded     Automated";
        let r = compress("argocd app list", list).unwrap();
        assert!(r.contains("app-b"), "keeps unhealthy: {r}");
        assert!(!r.contains("app-a"), "drops healthy: {r}");
    }

    #[test]
    fn non_app_subcommand_none() {
        assert!(compress("argocd version", "v2.9.0").is_none());
    }
}
