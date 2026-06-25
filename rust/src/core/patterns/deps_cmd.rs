use std::path::Path;

#[must_use]
pub fn compress(path: &str) -> Option<String> {
    let content = std::fs::read_to_string(path).ok()?;
    let filename = Path::new(path)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(path);

    match filename {
        "package.json" => compress_package_json(&content),
        "Cargo.toml" => compress_cargo_toml(&content),
        "requirements.txt" => compress_requirements(&content),
        "go.mod" => compress_go_mod(&content),
        "Gemfile" => compress_gemfile(&content),
        "pyproject.toml" => compress_pyproject(&content),
        _ => None,
    }
}

#[must_use]
pub fn detect_and_compress(dir: &str) -> Option<String> {
    let candidates = [
        "package.json",
        "Cargo.toml",
        "requirements.txt",
        "go.mod",
        "Gemfile",
        "pyproject.toml",
    ];

    for name in &candidates {
        let path = format!("{}/{}", dir.trim_end_matches('/'), name);
        if Path::new(&path).exists()
            && let Some(result) = compress(&path)
        {
            return Some(result);
        }
    }

    None
}

fn compress_package_json(content: &str) -> Option<String> {
    let val: serde_json::Value = serde_json::from_str(content).ok()?;
    let obj = val.as_object()?;

    let name = obj.get("name").and_then(|v| v.as_str()).unwrap_or("?");
    let version = obj.get("version").and_then(|v| v.as_str()).unwrap_or("?");

    let mut result = format!("Node ({name}@{version}):");

    if let Some(deps) = obj.get("dependencies").and_then(|v| v.as_object()) {
        let dep_list: Vec<String> = deps
            .iter()
            .map(|(k, v)| format!("{k} ({})", v.as_str().unwrap_or("?")))
            .collect();
        result.push_str(&format!(
            "\n  deps ({}): {}",
            dep_list.len(),
            dep_list.join(", ")
        ));
    }

    if let Some(deps) = obj.get("devDependencies").and_then(|v| v.as_object()) {
        let dep_list: Vec<String> = deps
            .iter()
            .take(10)
            .map(|(k, v)| format!("{k} ({})", v.as_str().unwrap_or("?")))
            .collect();
        let suffix = if deps.len() > 10 {
            format!(", ... +{} more", deps.len() - 10)
        } else {
            String::new()
        };
        result.push_str(&format!(
            "\n  devDeps ({}): {}{}",
            deps.len(),
            dep_list.join(", "),
            suffix
        ));
    }

    Some(result)
}

fn compress_cargo_toml(content: &str) -> Option<String> {
    let mut name = String::new();
    let mut version = String::new();
    let mut deps = Vec::new();
    let mut in_deps = false;

    for line in content.lines() {
        let trimmed = line.trim();

        if trimmed.starts_with("[dependencies]") {
            in_deps = true;
            continue;
        }
        if trimmed.starts_with('[') && in_deps {
            in_deps = false;
        }

        if trimmed.starts_with("name")
            && let Some(v) = extract_toml_string(trimmed)
        {
            name = v;
        }
        if trimmed.starts_with("version")
            && !in_deps
            && let Some(v) = extract_toml_string(trimmed)
        {
            version = v;
        }

        if in_deps
            && !trimmed.is_empty()
            && !trimmed.starts_with('#')
            && let Some(dep_name) = trimmed.split('=').next()
        {
            deps.push(dep_name.trim().to_string());
        }
    }

    if deps.is_empty() {
        return None;
    }

    let mut result = format!("Rust ({name}@{version}):");
    result.push_str(&format!("\n  deps ({}): {}", deps.len(), deps.join(", ")));

    Some(result)
}

fn compress_requirements(content: &str) -> Option<String> {
    let deps: Vec<&str> = content
        .lines()
        .filter(|l| !l.trim().is_empty() && !l.trim().starts_with('#'))
        .collect();

    if deps.is_empty() {
        return None;
    }

    let short: Vec<String> = deps
        .iter()
        .map(|d| {
            d.split("==")
                .next()
                .unwrap_or(d)
                .split(">=")
                .next()
                .unwrap_or(d)
                .trim()
                .to_string()
        })
        .collect();

    Some(format!(
        "Python:\n  deps ({}): {}",
        short.len(),
        short.join(", ")
    ))
}

fn compress_go_mod(content: &str) -> Option<String> {
    let mut module = String::new();
    let mut deps = Vec::new();
    let mut in_require = false;

    for line in content.lines() {
        let trimmed = line.trim();

        if trimmed.starts_with("module ") {
            module = trimmed.strip_prefix("module ").unwrap_or("").to_string();
        }
        if trimmed == "require (" {
            in_require = true;
            continue;
        }
        if trimmed == ")" && in_require {
            in_require = false;
        }
        if in_require
            && !trimmed.is_empty()
            && let Some(name) = trimmed.split_whitespace().next()
            && !name.contains("// indirect")
        {
            let short = name.rsplit('/').next().unwrap_or(name);
            deps.push(short.to_string());
        }
    }

    if deps.is_empty() {
        return None;
    }

    Some(format!(
        "Go ({module}):\n  deps ({}): {}",
        deps.len(),
        deps.join(", ")
    ))
}

fn compress_gemfile(content: &str) -> Option<String> {
    let gems: Vec<&str> = content
        .lines()
        .filter(|l| l.trim().starts_with("gem "))
        .map(|l| {
            l.trim()
                .strip_prefix("gem ")
                .unwrap_or(l)
                .split(',')
                .next()
                .unwrap_or("")
                .trim()
                .trim_matches('\'')
                .trim_matches('"')
        })
        .collect();

    if gems.is_empty() {
        return None;
    }

    Some(format!(
        "Ruby:\n  gems ({}): {}",
        gems.len(),
        gems.join(", ")
    ))
}

fn compress_pyproject(content: &str) -> Option<String> {
    let mut deps = Vec::new();
    let mut in_deps = false;

    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed == "dependencies = [" || trimmed.starts_with("dependencies") {
            in_deps = true;
            continue;
        }
        if trimmed == "]" && in_deps {
            in_deps = false;
        }
        if in_deps {
            let clean = trimmed.trim_matches(|c: char| c == '"' || c == '\'' || c == ',');
            let name = clean
                .split(">=")
                .next()
                .unwrap_or(clean)
                .split("==")
                .next()
                .unwrap_or(clean)
                .trim();
            if !name.is_empty() {
                deps.push(name.to_string());
            }
        }
    }

    if deps.is_empty() {
        return None;
    }

    Some(format!(
        "Python:\n  deps ({}): {}",
        deps.len(),
        deps.join(", ")
    ))
}

fn extract_toml_string(line: &str) -> Option<String> {
    let after_eq = line.split('=').nth(1)?.trim();
    Some(after_eq.trim_matches('"').to_string())
}
