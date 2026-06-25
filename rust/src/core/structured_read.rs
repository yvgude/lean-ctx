use std::collections::BTreeMap;

/// Markdown outline: heading tree + fenced code block boundaries.
#[must_use]
pub fn extract_markdown_outline(content: &str) -> String {
    let mut parts = Vec::new();
    let mut in_code_block = false;

    for line in content.lines() {
        let trimmed = line.trim();

        if trimmed.starts_with("```") {
            in_code_block = !in_code_block;
            continue;
        }
        if in_code_block {
            continue;
        }

        if let Some(heading) = parse_heading(trimmed) {
            parts.push(heading);
        }
    }

    if parts.is_empty() {
        return String::new();
    }

    parts.join("\n")
}

fn parse_heading(line: &str) -> Option<String> {
    let level = line.bytes().take_while(|&b| b == b'#').count();
    if !(1..=6).contains(&level) {
        return None;
    }
    let rest = line[level..].trim();
    if rest.is_empty() {
        return None;
    }
    let indent = "  ".repeat(level.saturating_sub(1));
    Some(format!("{indent}{rest}"))
}

/// JSON structure: key-tree with types, depth 3, max 20 keys per level.
/// Reuses logic from `patterns::json_schema` but produces a read-mode output.
#[must_use]
pub fn extract_json_structure(content: &str) -> String {
    let trimmed = content.trim();
    let val: serde_json::Value = match serde_json::from_str(trimmed) {
        Ok(v) => v,
        Err(_) => return String::new(),
    };
    format_json_value(&val, 0)
}

fn format_json_value(val: &serde_json::Value, depth: usize) -> String {
    let indent = "  ".repeat(depth);
    match val {
        serde_json::Value::Object(map) => {
            if map.is_empty() {
                return format!("{indent}{{}}");
            }
            if depth > 3 {
                return format!("{indent}{{...{} keys}}", map.len());
            }
            let mut entries = Vec::new();
            for (key, value) in map.iter().take(20) {
                match value {
                    serde_json::Value::Object(inner) if !inner.is_empty() && depth < 3 => {
                        let nested = format_json_value(value, depth + 1);
                        entries.push(format!("{indent}  {key}: {{\n{nested}\n{indent}  }}"));
                    }
                    serde_json::Value::Array(arr) if !arr.is_empty() => {
                        let item_type = arr.first().map_or("any", json_type_name);
                        entries.push(format!("{indent}  {key}: [{item_type}...{}]", arr.len()));
                    }
                    _ => {
                        entries.push(format!("{indent}  {key}: {}", json_type_name(value)));
                    }
                }
            }
            if map.len() > 20 {
                entries.push(format!("{indent}  ...+{} more keys", map.len() - 20));
            }
            entries.join("\n")
        }
        serde_json::Value::Array(arr) => {
            if arr.is_empty() {
                return format!("{indent}[]");
            }
            let first_schema = format_json_value(&arr[0], depth + 1);
            format!(
                "{indent}[{} items, each:\n{first_schema}\n{indent}]",
                arr.len()
            )
        }
        other => format!("{indent}{}", json_type_name(other)),
    }
}

fn json_type_name(val: &serde_json::Value) -> &'static str {
    match val {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "bool",
        serde_json::Value::Number(_) => "num",
        serde_json::Value::String(_) => "str",
        serde_json::Value::Array(_) => "array",
        serde_json::Value::Object(_) => "object",
    }
}

/// YAML structure: indent-based key extraction with nested structure.
#[must_use]
pub fn extract_yaml_structure(content: &str) -> String {
    let mut parts = Vec::new();
    let mut prev_indent = 0usize;

    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }

        let indent = line.len() - line.trim_start().len();
        if let Some(key) = extract_yaml_key(trimmed) {
            let level = indent / 2;
            let prefix = "  ".repeat(level);
            parts.push(format!("{prefix}{key}"));
            prev_indent = indent;
        } else if trimmed.starts_with("- ")
            && indent <= prev_indent + 2
            && let Some(key) = extract_yaml_key(trimmed.trim_start_matches("- "))
        {
            let level = indent / 2;
            let prefix = "  ".repeat(level);
            parts.push(format!("{prefix}- {key}"));
        }
    }

    deduplicate_consecutive(&parts)
}

fn extract_yaml_key(line: &str) -> Option<String> {
    let colon_pos = line.find(':')?;
    let key = line[..colon_pos].trim();
    if key.is_empty() || key.contains(' ') && !key.starts_with('"') {
        return None;
    }
    let value_part = line[colon_pos + 1..].trim();
    if value_part.is_empty() || value_part == "|" || value_part == ">" {
        Some(format!("{key}:"))
    } else if value_part.len() > 40 {
        Some(format!("{key}: ..."))
    } else {
        Some(format!("{key}: {value_part}"))
    }
}

fn deduplicate_consecutive(lines: &[String]) -> String {
    if lines.is_empty() {
        return String::new();
    }
    let mut result = Vec::with_capacity(lines.len());
    let mut prev = "";
    for line in lines {
        if line != prev {
            result.push(line.as_str());
            prev = line;
        }
    }
    result.join("\n")
}

/// TOML structure: `[section]` headers + top-level key=value pairs.
#[must_use]
pub fn extract_toml_structure(content: &str) -> String {
    let mut sections: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut current_section = String::new();

    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }

        if trimmed.starts_with('[') {
            if let Some(end) = trimmed.find(']') {
                current_section = trimmed[1..end].to_string();
                sections.entry(current_section.clone()).or_default();
            }
            continue;
        }

        if let Some(eq_pos) = trimmed.find('=') {
            let key = trimmed[..eq_pos].trim();
            let value = trimmed[eq_pos + 1..].trim();
            let display_val = if value.len() > 40 { "..." } else { value };
            sections
                .entry(current_section.clone())
                .or_default()
                .push(format!("{key} = {display_val}"));
        }
    }

    let mut parts = Vec::new();
    for (section, keys) in &sections {
        if section.is_empty() {
            for k in keys {
                parts.push(k.clone());
            }
        } else {
            parts.push(format!("[{section}]"));
            for k in keys.iter().take(10) {
                parts.push(format!("  {k}"));
            }
            if keys.len() > 10 {
                parts.push(format!("  ...+{} more", keys.len() - 10));
            }
        }
    }

    parts.join("\n")
}

/// Lock file summary: package count + direct dependency names.
#[must_use]
pub fn extract_lock_summary(content: &str, path: &str) -> String {
    let lower = path.to_lowercase();
    if lower.ends_with("cargo.lock") {
        extract_cargo_lock_summary(content)
    } else if lower.ends_with("package-lock.json") {
        extract_npm_lock_summary(content)
    } else if lower.ends_with("yarn.lock") {
        extract_yarn_lock_summary(content)
    } else if lower.ends_with("poetry.lock") || lower.ends_with("pdm.lock") {
        extract_poetry_lock_summary(content)
    } else if lower.ends_with("go.sum") {
        extract_go_sum_summary(content)
    } else {
        extract_generic_lock_summary(content)
    }
}

fn extract_cargo_lock_summary(content: &str) -> String {
    let pkg_count = content
        .lines()
        .filter(|l| l.trim() == "[[package]]")
        .count();

    let mut local_crates: Vec<&str> = Vec::new();
    let mut local_deps: Vec<&str> = Vec::new();
    let mut current_name: Option<&str> = None;
    let mut has_source = false;
    let mut in_deps = false;

    for line in content.lines() {
        let t = line.trim();
        if t == "[[package]]" {
            if let Some(name) = current_name
                && !has_source
                && !local_crates.contains(&name)
            {
                local_crates.push(name);
            }
            current_name = None;
            has_source = false;
            in_deps = false;
            continue;
        }
        if t.starts_with("name = ") {
            current_name = Some(t.trim_start_matches("name = ").trim_matches('"'));
        } else if t.starts_with("source = ") {
            has_source = true;
        } else if t.starts_with("dependencies = [") {
            if !has_source {
                in_deps = true;
            }
        } else if in_deps {
            if t == "]" {
                in_deps = false;
            } else {
                let dep = t.trim_matches(|c: char| c == '"' || c == ',');
                let dep_name = dep.split_whitespace().next().unwrap_or(dep);
                if !dep_name.is_empty() && !local_deps.contains(&dep_name) && local_deps.len() < 30
                {
                    local_deps.push(dep_name);
                }
            }
        }
    }
    if let Some(name) = current_name
        && !has_source
        && !local_crates.contains(&name)
    {
        local_crates.push(name);
    }

    let mut out = format!("Cargo.lock: {pkg_count} packages");
    if !local_crates.is_empty() {
        out.push_str(&format!("\n  workspace: {}", local_crates.join(", ")));
    }
    if !local_deps.is_empty() {
        out.push_str(&format!("\n  direct deps: {}", local_deps.join(", ")));
    }
    out
}

fn extract_npm_lock_summary(content: &str) -> String {
    let val: serde_json::Value = match serde_json::from_str(content) {
        Ok(v) => v,
        Err(_) => return extract_generic_lock_summary(content),
    };
    let name = val.get("name").and_then(|v| v.as_str()).unwrap_or("?");
    let pkg_count = val
        .get("packages")
        .and_then(|v| v.as_object())
        .map(serde_json::Map::len)
        .or_else(|| {
            val.get("dependencies")
                .and_then(|v| v.as_object())
                .map(serde_json::Map::len)
        })
        .unwrap_or(0);
    format!("package-lock.json ({name}): {pkg_count} packages")
}

fn extract_yarn_lock_summary(content: &str) -> String {
    let pkg_count = content
        .lines()
        .filter(|l| !l.starts_with(' ') && !l.starts_with('#') && l.contains('@'))
        .count();
    format!("yarn.lock: ~{pkg_count} packages")
}

fn extract_poetry_lock_summary(content: &str) -> String {
    let pkg_count = content
        .lines()
        .filter(|l| l.trim() == "[[package]]")
        .count();
    format!("poetry.lock: {pkg_count} packages")
}

fn extract_go_sum_summary(content: &str) -> String {
    let mut modules = std::collections::HashSet::new();
    for line in content.lines() {
        if let Some(space) = line.find(' ') {
            modules.insert(&line[..space]);
        }
    }
    format!("go.sum: {} modules", modules.len())
}

fn extract_generic_lock_summary(content: &str) -> String {
    let line_count = content.lines().count();
    format!("lock file: {line_count} lines")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markdown_outline_extracts_headings() {
        let md =
            "# Title\n\nSome text.\n\n## Section A\n\n### Sub A1\n\n## Section B\n\nMore text.";
        let outline = extract_markdown_outline(md);
        assert!(outline.contains("Title"));
        assert!(outline.contains("  Section A"));
        assert!(outline.contains("    Sub A1"));
        assert!(outline.contains("  Section B"));
    }

    #[test]
    fn markdown_outline_skips_code_blocks() {
        let md = "# Real\n\n```\n# Not a heading\n```\n\n## Also Real";
        let outline = extract_markdown_outline(md);
        assert!(outline.contains("Real"));
        assert!(outline.contains("Also Real"));
        assert!(!outline.contains("Not a heading"));
    }

    #[test]
    fn markdown_outline_empty_for_no_headings() {
        let md = "Just plain text\nwithout any headings.";
        assert!(extract_markdown_outline(md).is_empty());
    }

    #[test]
    fn json_structure_extracts_keys() {
        let json = r#"{"name": "test", "version": "1.0", "deps": {"a": 1, "b": 2}}"#;
        let structure = extract_json_structure(json);
        assert!(structure.contains("name: str"));
        assert!(structure.contains("version: str"));
        assert!(structure.contains("deps: {"));
        assert!(structure.contains("a: num"));
    }

    #[test]
    fn json_structure_handles_arrays() {
        let json = r#"[{"id": 1}, {"id": 2}]"#;
        let structure = extract_json_structure(json);
        assert!(structure.contains("2 items"));
        assert!(structure.contains("id: num"));
    }

    #[test]
    fn json_structure_empty_for_invalid() {
        assert!(extract_json_structure("not json").is_empty());
    }

    #[test]
    fn yaml_structure_extracts_keys() {
        let yaml =
            "name: my-app\nversion: 1.0\nservices:\n  web:\n    port: 8080\n  db:\n    port: 5432";
        let structure = extract_yaml_structure(yaml);
        assert!(structure.contains("name: my-app"));
        assert!(structure.contains("version: 1.0"));
        assert!(structure.contains("services:"));
        assert!(structure.contains("web:"));
    }

    #[test]
    fn yaml_structure_skips_comments() {
        let yaml = "# Comment\nkey: value\n# Another comment\nkey2: value2";
        let structure = extract_yaml_structure(yaml);
        assert!(!structure.contains("Comment"));
        assert!(structure.contains("key: value"));
        assert!(structure.contains("key2: value2"));
    }

    #[test]
    fn toml_structure_extracts_sections() {
        let toml =
            "[package]\nname = \"test\"\nversion = \"0.1.0\"\n\n[dependencies]\nserde = \"1.0\"";
        let structure = extract_toml_structure(toml);
        assert!(structure.contains("[package]"));
        assert!(structure.contains("name = \"test\""));
        assert!(structure.contains("[dependencies]"));
        assert!(structure.contains("serde = \"1.0\""));
    }

    #[test]
    fn toml_structure_handles_top_level_keys() {
        let toml = "key = \"value\"\n\n[section]\na = 1";
        let structure = extract_toml_structure(toml);
        assert!(structure.contains("key = \"value\""));
        assert!(structure.contains("[section]"));
    }

    #[test]
    fn cargo_lock_summary() {
        let lock = "[[package]]\nname = \"serde\"\nversion = \"1.0\"\n\n[[package]]\nname = \"tokio\"\nversion = \"1.0\"";
        let summary = extract_lock_summary(lock, "Cargo.lock");
        assert!(summary.contains("2 packages"));
    }

    #[test]
    fn npm_lock_summary() {
        let lock = r#"{"name":"app","lockfileVersion":3,"packages":{"":{},"node_modules/a":{},"node_modules/b":{}}}"#;
        let summary = extract_lock_summary(lock, "package-lock.json");
        assert!(summary.contains("app"));
        assert!(summary.contains("3 packages"));
    }

    #[test]
    fn yarn_lock_summary_counts() {
        let lock = "# yarn lockfile v1\n\na@^1.0:\n  version \"1.0\"\n\nb@^2.0:\n  version \"2.0\"";
        let summary = extract_lock_summary(lock, "yarn.lock");
        assert!(summary.contains("2 packages"));
    }

    #[test]
    fn go_sum_summary_counts_modules() {
        let sum = "github.com/a/b v1.0.0 h1:abc=\ngithub.com/a/b v1.0.0/go.mod h1:def=\ngithub.com/c/d v2.0.0 h1:ghi=";
        let summary = extract_lock_summary(sum, "go.sum");
        assert!(summary.contains("2 modules"));
    }
}
