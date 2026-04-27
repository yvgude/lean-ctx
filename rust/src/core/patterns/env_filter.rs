use std::collections::BTreeMap;

pub fn compress(output: &str) -> Option<String> {
    let trimmed = output.trim();
    if trimmed.is_empty() {
        return Some("(empty)".to_string());
    }

    let mut groups: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut ungrouped = Vec::new();

    let sensitive_patterns = [
        "KEY",
        "SECRET",
        "TOKEN",
        "PASSWORD",
        "PASSWD",
        "CREDENTIALS",
        "AUTH",
        "API_KEY",
        "PRIVATE",
        "CERT",
    ];

    for line in trimmed.lines() {
        let l = line.trim();
        if l.is_empty() {
            continue;
        }

        if let Some((key, value)) = l.split_once('=') {
            let is_sensitive = sensitive_patterns
                .iter()
                .any(|p| key.to_uppercase().contains(p));
            let display_value = if is_sensitive {
                "***".to_string()
            } else if value.len() > 80 {
                let truncated: String = value.chars().take(40).collect();
                format!("{truncated}...")
            } else {
                value.to_string()
            };

            let prefix = key.split('_').next().unwrap_or("OTHER").to_string();

            groups
                .entry(prefix)
                .or_default()
                .push(format!("{key}={display_value}"));
        } else {
            ungrouped.push(l.to_string());
        }
    }

    let total: usize = groups.values().map(std::vec::Vec::len).sum();

    let mut parts = Vec::new();
    parts.push(format!("{total} variables:"));

    for (prefix, vars) in &groups {
        if vars.len() >= 3 {
            parts.push(format!("[{prefix}_*] ({} vars)", vars.len()));
            for v in vars.iter().take(3) {
                parts.push(format!("  {v}"));
            }
            if vars.len() > 3 {
                parts.push(format!("  ... +{} more", vars.len() - 3));
            }
        }
    }

    let small_groups: Vec<String> = groups
        .iter()
        .filter(|(_, v)| v.len() < 3)
        .flat_map(|(_, v)| v.iter().cloned())
        .collect();

    if !small_groups.is_empty() {
        parts.push(format!("[other] ({} vars)", small_groups.len()));
        for v in small_groups.iter().take(5) {
            parts.push(format!("  {v}"));
        }
        if small_groups.len() > 5 {
            parts.push(format!("  ... +{} more", small_groups.len() - 5));
        }
    }

    Some(parts.join("\n"))
}
