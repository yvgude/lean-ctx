use std::collections::HashMap;

#[must_use]
pub fn compress(output: &str) -> Option<String> {
    let lines: Vec<&str> = output.lines().filter(|l| !l.trim().is_empty()).collect();
    if lines.len() < 5 {
        return None;
    }

    let mut by_dir: HashMap<String, Vec<String>> = HashMap::new();
    let mut total_files = 0usize;

    for line in &lines {
        let path = line.trim().strip_prefix("./").unwrap_or(line.trim());

        if should_skip(path) {
            continue;
        }

        total_files += 1;

        if let Some(slash_pos) = path.rfind('/') {
            let dir = &path[..slash_pos];
            let file = &path[slash_pos + 1..];
            by_dir
                .entry(dir.to_string())
                .or_default()
                .push(file.to_string());
        } else {
            by_dir
                .entry(".".to_string())
                .or_default()
                .push(path.to_string());
        }
    }

    if total_files == 0 {
        return None;
    }

    let mut result = format!("{total_files}F {}D:\n", by_dir.len());
    let mut sorted_dirs: Vec<_> = by_dir.iter().collect();
    sorted_dirs.sort_by_key(|(dir, _)| (*dir).clone());

    for (dir, files) in &sorted_dirs {
        result.push_str(&format!("\n{dir}/"));
        let show: Vec<_> = files.iter().take(10).collect();
        let mut line_buf = String::new();
        for f in &show {
            if line_buf.len() + f.len() + 1 > 60 {
                result.push_str(&format!("\n  {line_buf}"));
                line_buf.clear();
            }
            if !line_buf.is_empty() {
                line_buf.push(' ');
            }
            line_buf.push_str(f);
        }
        if !line_buf.is_empty() {
            result.push_str(&format!("\n  {line_buf}"));
        }
        if files.len() > 10 {
            result.push_str(&format!("\n  ... +{} more", files.len() - 10));
        }
    }

    Some(result)
}

fn should_skip(path: &str) -> bool {
    let skip_dirs = [
        "node_modules/",
        ".git/",
        "target/debug/",
        "target/release/",
        "__pycache__/",
        ".svelte-kit/",
        ".next/",
        "dist/",
        ".DS_Store",
    ];
    skip_dirs.iter().any(|d| path.contains(d))
}
