use std::collections::HashMap;

pub fn compress(cmd: &str, output: &str) -> Option<String> {
    let trimmed = output.trim();
    if trimmed.is_empty() {
        return Some("ok".to_string());
    }

    if cmd.contains("s3 ls") || cmd.contains("s3 cp") || cmd.contains("s3 sync") {
        return Some(compress_s3(cmd, trimmed));
    }
    if cmd.contains("ec2 describe-instances") {
        return Some(compress_ec2_instances(trimmed));
    }
    if cmd.contains("lambda list-functions") {
        return Some(compress_lambda_list(trimmed));
    }
    if cmd.contains("cloudformation describe-stacks") || cmd.contains("cfn ") {
        return Some(compress_cfn(trimmed));
    }
    if cmd.contains("sts get-caller-identity") {
        return Some(trimmed.to_string());
    }
    if cmd.contains("logs") {
        return Some(compress_logs(trimmed));
    }
    if cmd.contains("ecs list") || cmd.contains("ecs describe") {
        return Some(compress_ecs(trimmed));
    }

    Some(compact_json_or_text(trimmed, 15))
}

fn compress_s3(cmd: &str, output: &str) -> String {
    if cmd.contains("s3 ls") {
        let entries: Vec<&str> = output.lines().filter(|l| !l.trim().is_empty()).collect();
        if entries.len() <= 20 {
            return entries.join("\n");
        }
        let dirs: Vec<&&str> = entries.iter().filter(|l| l.contains("PRE ")).collect();
        let files: Vec<&&str> = entries.iter().filter(|l| !l.contains("PRE ")).collect();
        return format!(
            "{} dirs, {} files\n{}",
            dirs.len(),
            files.len(),
            entries
                .iter()
                .take(15)
                .copied()
                .collect::<Vec<_>>()
                .join("\n")
        );
    }

    let mut uploaded = 0u32;
    let mut copied = 0u32;
    for line in output.lines() {
        if line.contains("upload:") {
            uploaded += 1;
        }
        if line.contains("copy:") {
            copied += 1;
        }
    }
    if uploaded + copied == 0 {
        return compact_lines(output, 10);
    }
    let mut result = String::new();
    if uploaded > 0 {
        result.push_str(&format!("{uploaded} uploaded"));
    }
    if copied > 0 {
        if !result.is_empty() {
            result.push_str(", ");
        }
        result.push_str(&format!("{copied} copied"));
    }
    result
}

fn compress_ec2_instances(output: &str) -> String {
    if let Ok(val) = serde_json::from_str::<serde_json::Value>(output) {
        let reservations = val.get("Reservations").and_then(|r| r.as_array());
        if let Some(res) = reservations {
            let mut instances = Vec::new();
            for r in res {
                if let Some(insts) = r.get("Instances").and_then(|i| i.as_array()) {
                    for inst in insts {
                        let id = inst
                            .get("InstanceId")
                            .and_then(|v| v.as_str())
                            .unwrap_or("?");
                        let state = inst
                            .get("State")
                            .and_then(|s| s.get("Name"))
                            .and_then(|n| n.as_str())
                            .unwrap_or("?");
                        let itype = inst
                            .get("InstanceType")
                            .and_then(|v| v.as_str())
                            .unwrap_or("?");
                        let name = inst
                            .get("Tags")
                            .and_then(|t| t.as_array())
                            .and_then(|tags| {
                                tags.iter()
                                    .find(|t| t.get("Key").and_then(|k| k.as_str()) == Some("Name"))
                            })
                            .and_then(|t| t.get("Value").and_then(|v| v.as_str()))
                            .unwrap_or("-");
                        instances.push(format!("  {id} {state} {itype} \"{name}\""));
                    }
                }
            }
            return format!("{} instances:\n{}", instances.len(), instances.join("\n"));
        }
    }
    compact_lines(output, 15)
}

fn compress_lambda_list(output: &str) -> String {
    if let Ok(val) = serde_json::from_str::<serde_json::Value>(output) {
        if let Some(fns) = val.get("Functions").and_then(|f| f.as_array()) {
            let items: Vec<String> = fns
                .iter()
                .map(|f| {
                    let name = f
                        .get("FunctionName")
                        .and_then(|v| v.as_str())
                        .unwrap_or("?");
                    let runtime = f.get("Runtime").and_then(|v| v.as_str()).unwrap_or("?");
                    let mem = f
                        .get("MemorySize")
                        .and_then(serde_json::Value::as_u64)
                        .unwrap_or(0);
                    format!("  {name} ({runtime}, {mem}MB)")
                })
                .collect();
            return format!("{} functions:\n{}", items.len(), items.join("\n"));
        }
    }
    compact_lines(output, 15)
}

fn compress_cfn(output: &str) -> String {
    if let Ok(val) = serde_json::from_str::<serde_json::Value>(output) {
        if let Some(stacks) = val.get("Stacks").and_then(|s| s.as_array()) {
            let items: Vec<String> = stacks
                .iter()
                .map(|s| {
                    let name = s.get("StackName").and_then(|v| v.as_str()).unwrap_or("?");
                    let status = s.get("StackStatus").and_then(|v| v.as_str()).unwrap_or("?");
                    format!("  {name}: {status}")
                })
                .collect();
            return format!("{} stacks:\n{}", items.len(), items.join("\n"));
        }
    }
    compact_lines(output, 10)
}

fn compress_logs(output: &str) -> String {
    let lines: Vec<&str> = output.lines().collect();
    if lines.len() <= 20 {
        return output.to_string();
    }
    let mut deduped: HashMap<String, u32> = HashMap::new();
    for line in &lines {
        let key = line
            .split_whitespace()
            .skip(2)
            .collect::<Vec<_>>()
            .join(" ");
        if !key.is_empty() {
            *deduped.entry(key).or_insert(0) += 1;
        }
    }
    let mut sorted: Vec<_> = deduped.into_iter().collect();
    sorted.sort_by_key(|x| std::cmp::Reverse(x.1));
    let top: Vec<String> = sorted
        .iter()
        .take(15)
        .map(|(msg, count)| {
            if *count > 1 {
                format!("  ({count}x) {msg}")
            } else {
                format!("  {msg}")
            }
        })
        .collect();
    format!(
        "{} log entries (deduped to {}):\n{}",
        lines.len(),
        top.len(),
        top.join("\n")
    )
}

fn compress_ecs(output: &str) -> String {
    compact_json_or_text(output, 15)
}

fn compact_json_or_text(text: &str, max: usize) -> String {
    if let Ok(val) = serde_json::from_str::<serde_json::Value>(text) {
        let keys = extract_top_keys(&val);
        if !keys.is_empty() {
            return format!("JSON: {{{}}}", keys.join(", "));
        }
    }
    compact_lines(text, max)
}

fn extract_top_keys(val: &serde_json::Value) -> Vec<String> {
    match val {
        serde_json::Value::Object(map) => map
            .keys()
            .take(20)
            .map(|k| {
                let v = &map[k];
                match v {
                    serde_json::Value::Array(a) => format!("{k}: [{} items]", a.len()),
                    serde_json::Value::Object(_) => format!("{k}: {{...}}"),
                    _ => format!("{k}: {v}"),
                }
            })
            .collect(),
        _ => vec![],
    }
}

fn compact_lines(text: &str, max: usize) -> String {
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
