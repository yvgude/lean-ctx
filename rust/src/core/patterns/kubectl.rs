macro_rules! static_regex {
    ($pattern:expr_2021) => {{
        static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
        RE.get_or_init(|| {
            regex::Regex::new($pattern).expect(concat!("BUG: invalid static regex: ", $pattern))
        })
    }};
}

fn log_ts_re() -> &'static regex::Regex {
    static_regex!(r"^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}\S*\s+")
}
fn resource_action_re() -> &'static regex::Regex {
    static_regex!(r"(\S+/\S+)\s+(configured|created|unchanged|deleted)")
}

#[must_use]
pub fn compress(command: &str, output: &str) -> Option<String> {
    if command.contains("logs") || command.contains("log ") {
        return Some(compress_logs(output));
    }
    if command.contains("describe") {
        return Some(compress_describe(output));
    }
    if command.contains("apply") {
        return Some(compress_apply(output));
    }
    if command.contains("delete") {
        return Some(compress_delete(output));
    }
    if command.contains("get") {
        return Some(compress_get(output));
    }
    if command.contains("exec") {
        return Some(compress_exec(output));
    }
    if command.contains("top") {
        return Some(compress_top(output));
    }
    if command.contains("rollout") {
        return Some(compress_rollout(output));
    }
    if command.contains("scale") {
        return Some(compress_simple(output));
    }
    Some(compact_table(output))
}

fn compress_get(output: &str) -> String {
    let lines: Vec<&str> = output.lines().collect();
    if lines.is_empty() {
        return "no resources".to_string();
    }
    if lines.len() == 1 && lines[0].starts_with("No resources") {
        return "no resources".to_string();
    }

    if lines.len() <= 1 {
        return output.trim().to_string();
    }

    let header = lines[0];
    let cols: Vec<&str> = header.split_whitespace().collect();

    // Find STATUS column index for aggregation
    let status_col = cols.iter().position(|c| c.eq_ignore_ascii_case("STATUS"));

    let data_lines: Vec<Vec<&str>> = lines[1..]
        .iter()
        .map(|l| l.split_whitespace().collect::<Vec<&str>>())
        .filter(|p| !p.is_empty())
        .collect();

    if data_lines.is_empty() {
        return "no resources".to_string();
    }

    let total = data_lines.len();

    // If STATUS column exists and we have many rows, produce aggregation summary
    if let Some(si) = status_col
        && total > 5
    {
        let mut status_counts: std::collections::HashMap<&str, usize> =
            std::collections::HashMap::new();
        for row in &data_lines {
            if let Some(status) = row.get(si) {
                *status_counts.entry(status).or_default() += 1;
            }
        }
        let mut summary_parts: Vec<String> = status_counts
            .iter()
            .map(|(k, v)| format!("{v} {k}"))
            .collect();
        summary_parts.sort_by(|a, b| b.cmp(a));
        return format!("{total} resources ({})", summary_parts.join(", "));
    }

    // For small result sets, show compact table
    let mut rows = Vec::new();
    for parts in &data_lines {
        let name = parts[0];
        let relevant: Vec<&str> = parts.iter().skip(1).take(4).copied().collect();
        rows.push(format!("{name} {}", relevant.join(" ")));
    }

    let col_hint = cols
        .iter()
        .skip(1)
        .take(4)
        .copied()
        .collect::<Vec<&str>>()
        .join(" ");
    format!("[{col_hint}]\n{}", rows.join("\n"))
}

fn compress_logs(output: &str) -> String {
    let lines: Vec<&str> = output.lines().collect();
    if lines.len() <= 10 {
        return output.to_string();
    }

    let mut deduped: Vec<(String, u32)> = Vec::new();
    for line in &lines {
        let stripped = log_ts_re().replace(line, "").trim().to_string();
        if stripped.is_empty() {
            continue;
        }

        if let Some(last) = deduped.last_mut()
            && last.0 == stripped
        {
            last.1 += 1;
            continue;
        }
        deduped.push((stripped, 1));
    }

    let result: Vec<String> = deduped
        .iter()
        .map(|(line, count)| {
            if *count > 1 {
                format!("{line} (x{count})")
            } else {
                line.clone()
            }
        })
        .collect();

    if result.len() > 30 {
        let tail = &result[result.len() - 20..];
        return format!("... ({} lines total)\n{}", lines.len(), tail.join("\n"));
    }
    result.join("\n")
}

fn compress_describe(output: &str) -> String {
    let lines: Vec<&str> = output.lines().collect();
    if lines.len() <= 20 {
        return output.to_string();
    }

    let mut sections = Vec::new();
    let mut current_section = String::new();
    let mut current_lines: Vec<&str> = Vec::new();
    for line in &lines {
        if !line.starts_with(' ')
            && !line.starts_with('\t')
            && line.ends_with(':')
            && !line.contains("  ")
        {
            if !current_section.is_empty() {
                let count = current_lines.len();
                if count <= 3 {
                    sections.push(format!("{current_section}\n{}", current_lines.join("\n")));
                } else {
                    sections.push(format!("{current_section} ({count} lines)"));
                }
            }
            current_section = line.trim_end_matches(':').to_string();
            current_lines.clear();
            // Events section detected
        } else {
            current_lines.push(line);
        }
    }

    if !current_section.is_empty() {
        let count = current_lines.len();
        if current_section == "Events" && count > 5 {
            let last_events = &current_lines[count.saturating_sub(5)..];
            sections.push(format!(
                "Events (last 5 of {count}):\n{}",
                last_events.join("\n")
            ));
        } else if count <= 5 {
            sections.push(format!("{current_section}\n{}", current_lines.join("\n")));
        } else {
            sections.push(format!("{current_section} ({count} lines)"));
        }
    }

    sections.join("\n")
}

fn compress_apply(output: &str) -> String {
    let trimmed = output.trim();
    if trimmed.is_empty() {
        return "ok".to_string();
    }

    let mut configured = 0u32;
    let mut created = 0u32;
    let mut unchanged = 0u32;
    let mut deleted = 0u32;
    let mut resources = Vec::new();

    for line in trimmed.lines() {
        if let Some(caps) = resource_action_re().captures(line) {
            let resource = &caps[1];
            let action = &caps[2];
            match action {
                "configured" => configured += 1,
                "created" => created += 1,
                "unchanged" => unchanged += 1,
                "deleted" => deleted += 1,
                _ => {}
            }
            resources.push(format!("{resource} {action}"));
        }
    }

    let total = configured + created + unchanged + deleted;
    if total == 0 {
        return compact_output(trimmed, 5);
    }

    let mut summary = Vec::new();
    if created > 0 {
        summary.push(format!("{created} created"));
    }
    if configured > 0 {
        summary.push(format!("{configured} configured"));
    }
    if unchanged > 0 {
        summary.push(format!("{unchanged} unchanged"));
    }
    if deleted > 0 {
        summary.push(format!("{deleted} deleted"));
    }

    format!("ok ({total} resources: {})", summary.join(", "))
}

fn compress_delete(output: &str) -> String {
    let trimmed = output.trim();
    if trimmed.is_empty() {
        return "ok".to_string();
    }

    let deleted: Vec<&str> = trimmed.lines().filter(|l| l.contains("deleted")).collect();

    if deleted.is_empty() {
        return compact_output(trimmed, 3);
    }
    format!("deleted {} resources", deleted.len())
}

fn compress_exec(output: &str) -> String {
    let trimmed = output.trim();
    if trimmed.is_empty() {
        return "ok".to_string();
    }
    let lines: Vec<&str> = trimmed.lines().collect();
    if lines.len() > 20 {
        let tail = &lines[lines.len() - 10..];
        return format!("... ({} lines)\n{}", lines.len(), tail.join("\n"));
    }
    trimmed.to_string()
}

fn compress_top(output: &str) -> String {
    compact_table(output)
}

fn compress_rollout(output: &str) -> String {
    compact_output(output, 5)
}

fn compress_simple(output: &str) -> String {
    let trimmed = output.trim();
    if trimmed.is_empty() {
        return "ok".to_string();
    }
    compact_output(trimmed, 3)
}

fn compact_table(text: &str) -> String {
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    if lines.len() <= 15 {
        return lines.join("\n");
    }
    format!(
        "{}\n... ({} more rows)",
        lines[..15].join("\n"),
        lines.len() - 15
    )
}

fn compact_output(text: &str, max: usize) -> String {
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    if lines.len() <= max {
        return lines.join("\n");
    }
    format!(
        "{}\n... ({} more lines)",
        lines[..max].join("\n"),
        lines.len() - max
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn get_pods_many_aggregates_by_status() {
        let output = "\
NAME                    READY   STATUS    RESTARTS   AGE
api-server-abc123       1/1     Running   0          5d
api-server-def456       1/1     Running   0          5d
worker-1-ghi789        1/1     Running   2          3d
worker-2-jkl012        1/1     Running   0          3d
scheduler-mno345       0/1     Pending   0          1h
cache-pqr678           0/1     CrashLoopBackOff   5   2d
db-stu901              1/1     Running   0          10d
";
        let result = compress("kubectl get pods -A", output);
        let r = result.unwrap();
        assert!(r.contains("7 resources"));
        assert!(r.contains("Running"));
        assert!(r.contains("Pending"));
        assert!(r.contains("CrashLoopBackOff"));
    }

    #[test]
    fn get_pods_few_shows_compact_table() {
        let output = "\
NAME          READY   STATUS    RESTARTS   AGE
my-pod-123    1/1     Running   0          5d
my-pod-456    1/1     Running   0          3d
";
        let result = compress("kubectl get pods", output);
        let r = result.unwrap();
        assert!(r.contains("my-pod-123"));
        assert!(r.contains("[READY STATUS RESTARTS AGE]"));
    }

    #[test]
    fn get_no_resources() {
        let result = compress(
            "kubectl get pods",
            "No resources found in default namespace.\n",
        );
        assert_eq!(result.unwrap(), "no resources");
    }

    #[test]
    fn apply_aggregates_actions() {
        let output = "\
service/api-gateway configured
deployment.apps/api-server configured
configmap/settings created
secret/db-credentials unchanged
";
        let result = compress("kubectl apply -f deploy/", output);
        let r = result.unwrap();
        assert!(r.contains("4 resources"));
        assert!(r.contains("2 configured"));
        assert!(r.contains("1 created"));
        assert!(r.contains("1 unchanged"));
    }

    #[test]
    fn logs_deduplicates_repeated_lines() {
        let mut lines = Vec::new();
        for i in 0..20 {
            lines.push(format!("2026-05-25T10:{i:02}:00Z INFO: Processing request"));
        }
        lines.push("2026-05-25T10:20:00Z ERROR: Connection refused".to_string());
        let output = lines.join("\n");
        let result = compress("kubectl logs my-pod", &output);
        let r = result.unwrap();
        assert!(r.contains("(x"));
        assert!(r.contains("Connection refused"));
    }

    #[test]
    fn describe_extracts_events() {
        let mut output = String::new();
        output.push_str("Name: my-pod\n");
        output.push_str("Namespace: default\n");
        output.push_str("Labels:\n");
        for i in 0..10 {
            output.push_str(&format!("  app=myapp-{i}\n"));
        }
        output.push_str("Conditions:\n");
        output.push_str("  Type    Status\n");
        output.push_str("  Ready   True\n");
        output.push_str("Events:\n");
        for i in 0..8 {
            output.push_str(&format!("  Normal  Pulled  {i}m  kubelet  pulled image\n"));
        }
        let result = compress("kubectl describe pod my-pod", &output);
        let r = result.unwrap();
        assert!(r.contains("Events (last 5 of 8)"));
    }

    #[test]
    fn delete_counts_resources() {
        let output = "\
pod \"worker-1\" deleted
pod \"worker-2\" deleted
pod \"worker-3\" deleted
";
        let result = compress("kubectl delete pods -l app=worker", output);
        assert_eq!(result.unwrap(), "deleted 3 resources");
    }
}
