macro_rules! static_regex {
    ($pattern:expr) => {{
        static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
        RE.get_or_init(|| {
            regex::Regex::new($pattern).expect(concat!("BUG: invalid static regex: ", $pattern))
        })
    }};
}

fn log_timestamp_re() -> &'static regex::Regex {
    static_regex!(r"^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}")
}

pub fn compress(command: &str, output: &str) -> Option<String> {
    if command.contains("build") {
        return Some(compress_build(output));
    }
    if command.contains("compose") && command.contains("ps") {
        return Some(compress_compose_ps(output));
    }
    if command.contains("compose")
        && (command.contains("up")
            || command.contains("down")
            || command.contains("start")
            || command.contains("stop"))
    {
        return Some(compress_compose_action(output));
    }
    if command.contains("ps") {
        return Some(compress_ps(output));
    }
    if command.contains("images") {
        return Some(compress_images(output));
    }
    if command.contains("logs") {
        return Some(compress_logs(output));
    }
    if command.contains("network") {
        return Some(compress_network(output));
    }
    if command.contains("volume") {
        return Some(compress_volume(output));
    }
    if command.contains("inspect") {
        return Some(compress_inspect(output));
    }
    if command.contains("exec") || command.contains("run") {
        return Some(compress_exec(output));
    }
    if command.contains("system") && command.contains("df") {
        return Some(compress_system_df(output));
    }
    if command.contains("info") {
        return Some(compress_info(output));
    }
    if command.contains("version") {
        return Some(compress_version(output));
    }
    None
}

fn compress_build(output: &str) -> String {
    let mut steps = 0u32;
    let mut last_step = String::new();
    let mut errors = Vec::new();

    for line in output.lines() {
        if line.starts_with("Step ") || (line.starts_with('#') && line.contains('[')) {
            steps += 1;
            last_step = line.trim().to_string();
        }
        if line.contains("ERROR") || line.contains("error:") {
            errors.push(line.trim().to_string());
        }
    }

    if !errors.is_empty() {
        return format!(
            "{steps} steps, {} errors:\n{}",
            errors.len(),
            errors.join("\n")
        );
    }

    if steps > 0 {
        format!("{steps} steps, last: {last_step}")
    } else {
        "built".to_string()
    }
}

fn compress_ps(output: &str) -> String {
    let lines: Vec<&str> = output.lines().collect();
    if lines.len() <= 1 {
        return "no containers".to_string();
    }

    let header = lines[0];
    let col_positions = parse_docker_columns(header);

    let mut containers = Vec::new();
    for line in &lines[1..] {
        if line.trim().is_empty() {
            continue;
        }

        let name = extract_column(line, &col_positions, "NAMES")
            .unwrap_or_else(|| extract_last_word(line));
        let mut status =
            extract_column(line, &col_positions, "STATUS").unwrap_or_else(|| "?".to_string());
        let image = extract_column(line, &col_positions, "IMAGE");

        // Fallback: if health/exit annotations are in the raw line but missing
        // from the column-extracted status (column slicing can truncate them),
        // recover them from the raw line.
        for annotation in &["(unhealthy)", "(healthy)", "(health: starting)"] {
            if line.contains(annotation) && !status.contains(annotation) {
                status = format!("{status} {annotation}");
            }
        }
        if line.contains("Exited") && !status.contains("Exited") {
            if let Some(pos) = line.find("Exited") {
                let end = line[pos..].find(')').map_or(pos + 6, |p| pos + p + 1);
                let exited_str = &line[pos..end.min(line.len())];
                status = exited_str.to_string();
            }
        }

        let mut entry = name.clone();
        if let Some(img) = image {
            entry = format!("{name} ({img})");
        }
        entry = format!("{entry}: {status}");
        containers.push(entry);
    }

    if containers.is_empty() {
        return "no containers".to_string();
    }
    containers.join("\n")
}

fn parse_docker_columns(header: &str) -> Vec<(String, usize)> {
    let cols = [
        "CONTAINER ID",
        "IMAGE",
        "COMMAND",
        "CREATED",
        "STATUS",
        "PORTS",
        "NAMES",
    ];
    let mut positions: Vec<(String, usize)> = Vec::new();
    for col in &cols {
        if let Some(pos) = header.find(col) {
            positions.push((col.to_string(), pos));
        }
    }
    positions.sort_by_key(|(_, pos)| *pos);
    positions
}

fn extract_column(line: &str, cols: &[(String, usize)], name: &str) -> Option<String> {
    let idx = cols.iter().position(|(n, _)| n == name)?;
    let start = cols[idx].1;
    let end = cols.get(idx + 1).map_or(line.len(), |(_, p)| *p);
    if start >= line.len() {
        return None;
    }
    let end = end.min(line.len());
    let val = line[start..end].trim().to_string();
    if val.is_empty() {
        None
    } else {
        Some(val)
    }
}

fn extract_last_word(line: &str) -> String {
    line.split_whitespace().last().unwrap_or("?").to_string()
}

fn compress_images(output: &str) -> String {
    let lines: Vec<&str> = output.lines().collect();
    if lines.len() <= 1 {
        return "no images".to_string();
    }

    let mut images = Vec::new();
    for line in &lines[1..] {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() >= 5 {
            let repo = parts[0];
            let tag = parts[1];
            let size = parts.last().unwrap_or(&"?");
            if repo == "<none>" {
                continue;
            }
            images.push(format!("{repo}:{tag} ({size})"));
        }
    }

    if images.is_empty() {
        return "no images".to_string();
    }
    format!("{} images:\n{}", images.len(), images.join("\n"))
}

fn compress_logs(output: &str) -> String {
    let lines: Vec<&str> = output.lines().collect();
    if lines.len() <= 10 {
        return output.to_string();
    }

    let mut deduped: Vec<(String, u32)> = Vec::new();
    for line in &lines {
        let normalized = log_timestamp_re().replace(line, "[T]").to_string();
        let stripped = normalized.trim().to_string();
        if stripped.is_empty() {
            continue;
        }

        if let Some(last) = deduped.last_mut() {
            if last.0 == stripped {
                last.1 += 1;
                continue;
            }
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
        let result_strs: Vec<&str> = result.iter().map(std::string::String::as_str).collect();
        let middle = &result_strs[..result_strs.len() - 15];
        let safety = crate::core::safety_needles::extract_safety_lines(middle, 20);
        let last_lines = &result[result.len() - 15..];

        let mut out = format!("... ({} lines total", lines.len());
        if !safety.is_empty() {
            out.push_str(&format!(", {} safety-relevant preserved", safety.len()));
        }
        out.push_str(")\n");
        for s in &safety {
            out.push_str(s);
            out.push('\n');
        }
        out.push_str(&last_lines.join("\n"));
        out
    } else {
        result.join("\n")
    }
}

fn compress_compose_ps(output: &str) -> String {
    let lines: Vec<&str> = output.lines().collect();
    if lines.len() <= 1 {
        return "no services".to_string();
    }

    let mut services = Vec::new();
    for line in &lines[1..] {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() >= 3 {
            let name = parts[0];
            let status_parts: Vec<&str> = parts[1..].to_vec();
            let status = status_parts.join(" ");
            services.push(format!("{name}: {status}"));
        }
    }

    if services.is_empty() {
        return "no services".to_string();
    }
    format!("{} services:\n{}", services.len(), services.join("\n"))
}

fn compress_compose_action(output: &str) -> String {
    let trimmed = output.trim();
    if trimmed.is_empty() {
        return "ok".to_string();
    }

    let mut created = 0u32;
    let mut started = 0u32;
    let mut stopped = 0u32;
    let mut removed = 0u32;

    for line in trimmed.lines() {
        let l = line.to_lowercase();
        if l.contains("created") || l.contains("creating") {
            created += 1;
        }
        if l.contains("started") || l.contains("starting") {
            started += 1;
        }
        if l.contains("stopped") || l.contains("stopping") {
            stopped += 1;
        }
        if l.contains("removed") || l.contains("removing") {
            removed += 1;
        }
    }

    let mut parts = Vec::new();
    if created > 0 {
        parts.push(format!("{created} created"));
    }
    if started > 0 {
        parts.push(format!("{started} started"));
    }
    if stopped > 0 {
        parts.push(format!("{stopped} stopped"));
    }
    if removed > 0 {
        parts.push(format!("{removed} removed"));
    }

    if parts.is_empty() {
        return "ok".to_string();
    }
    format!("ok ({})", parts.join(", "))
}

fn compress_network(output: &str) -> String {
    let lines: Vec<&str> = output.lines().collect();
    if lines.len() <= 1 {
        return output.trim().to_string();
    }

    let mut networks = Vec::new();
    for line in &lines[1..] {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() >= 3 {
            let name = parts[1];
            let driver = parts[2];
            networks.push(format!("{name} ({driver})"));
        }
    }

    if networks.is_empty() {
        return "no networks".to_string();
    }
    networks.join(", ")
}

fn compress_volume(output: &str) -> String {
    let lines: Vec<&str> = output.lines().collect();
    if lines.len() <= 1 {
        return output.trim().to_string();
    }

    let volumes: Vec<&str> = lines[1..]
        .iter()
        .filter_map(|l| l.split_whitespace().nth(1))
        .collect();

    if volumes.is_empty() {
        return "no volumes".to_string();
    }
    format!("{} volumes: {}", volumes.len(), volumes.join(", "))
}

fn compress_inspect(output: &str) -> String {
    let trimmed = output.trim();
    if trimmed.starts_with('[') || trimmed.starts_with('{') {
        if let Ok(val) = serde_json::from_str::<serde_json::Value>(trimmed) {
            return compress_json_value(&val, 0);
        }
    }
    if trimmed.lines().count() > 20 {
        let lines: Vec<&str> = trimmed.lines().collect();
        return format!(
            "{}\n... ({} more lines)",
            lines[..10].join("\n"),
            lines.len() - 10
        );
    }
    trimmed.to_string()
}

fn compress_exec(output: &str) -> String {
    let trimmed = output.trim();
    if trimmed.is_empty() {
        return "ok".to_string();
    }
    let lines: Vec<&str> = trimmed.lines().collect();
    if lines.len() > 30 {
        let last = &lines[lines.len() - 10..];
        return format!("... ({} lines)\n{}", lines.len(), last.join("\n"));
    }
    trimmed.to_string()
}

fn compress_system_df(output: &str) -> String {
    let mut parts = Vec::new();
    let mut current_type = String::new();

    for line in output.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("TYPE") {
            continue;
        }
        if trimmed.starts_with("Images")
            || trimmed.starts_with("Containers")
            || trimmed.starts_with("Local Volumes")
            || trimmed.starts_with("Build Cache")
        {
            current_type = trimmed.to_string();
            continue;
        }
        if !current_type.is_empty() && trimmed.contains("RECLAIMABLE") {
            current_type.clear();
        }
    }

    let lines: Vec<&str> = output
        .lines()
        .filter(|l| {
            let t = l.trim();
            !t.is_empty()
                && (t.contains("RECLAIMABLE")
                    || t.contains("SIZE")
                    || t.starts_with("Images")
                    || t.starts_with("Containers")
                    || t.starts_with("Local Volumes")
                    || t.starts_with("Build Cache")
                    || t.chars().next().is_some_and(|c| c.is_ascii_digit()))
        })
        .collect();

    if lines.is_empty() {
        return compact_output(output, 10);
    }

    for line in &lines {
        let trimmed = line.trim();
        if !trimmed.starts_with("TYPE") && !trimmed.is_empty() {
            parts.push(trimmed.to_string());
        }
    }

    if parts.is_empty() {
        compact_output(output, 10)
    } else {
        parts.join("\n")
    }
}

fn compress_info(output: &str) -> String {
    let mut key_info = Vec::new();
    let important_keys = [
        "Server Version",
        "Operating System",
        "Architecture",
        "CPUs",
        "Total Memory",
        "Docker Root Dir",
        "Storage Driver",
        "Containers:",
        "Images:",
    ];

    for line in output.lines() {
        let trimmed = line.trim();
        for key in &important_keys {
            if trimmed.starts_with(key) {
                key_info.push(trimmed.to_string());
                break;
            }
        }
    }

    if key_info.is_empty() {
        return compact_output(output, 10);
    }
    key_info.join("\n")
}

fn compress_version(output: &str) -> String {
    let mut parts = Vec::new();
    let important = ["Version:", "API version:", "Go version:", "OS/Arch:"];

    for line in output.lines() {
        let trimmed = line.trim();
        for key in &important {
            if trimmed.starts_with(key) {
                parts.push(trimmed.to_string());
                break;
            }
        }
    }

    if parts.is_empty() {
        return compact_output(output, 5);
    }
    parts.join("\n")
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

fn compress_json_value(val: &serde_json::Value, depth: usize) -> String {
    if depth > 2 {
        return "...".to_string();
    }
    match val {
        serde_json::Value::Object(map) => {
            let keys: Vec<String> = map.keys().take(15).cloned().collect();
            let total = map.len();
            if total > 15 {
                format!("{{{} ... +{} keys}}", keys.join(", "), total - 15)
            } else {
                format!("{{{}}}", keys.join(", "))
            }
        }
        serde_json::Value::Array(arr) => format!("[...{}]", arr.len()),
        other => format!("{other}"),
    }
}
