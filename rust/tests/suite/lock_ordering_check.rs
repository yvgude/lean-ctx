//! CI check: every `static ... Mutex` / `static ... RwLock` in `src/` must be
//! referenced in `LOCK_ORDERING.md`. Prevents undocumented locks from accumulating.

use std::collections::HashSet;
use std::path::Path;

fn collect_lock_files_from_source(src_dir: &Path) -> Vec<(String, String)> {
    let mut results = Vec::new();
    visit_dir(src_dir, &mut results);
    results
}

fn visit_dir(dir: &Path, results: &mut Vec<(String, String)>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            visit_dir(&path, results);
        } else if path.extension().is_some_and(|e| e == "rs") {
            check_file(&path, results);
        }
    }
}

fn check_file(path: &Path, results: &mut Vec<(String, String)>) {
    let Ok(content) = std::fs::read_to_string(path) else {
        return;
    };

    let rel = path
        .strip_prefix(env!("CARGO_MANIFEST_DIR"))
        .unwrap_or(path);

    // Test-only serialization locks (e.g. `static TEST_LOCK: Mutex<()>`) live inside
    // inline `#[cfg(test)] mod tests { ... }` blocks and are not part of the
    // production lock-ordering graph, so they must not require documentation. We
    // track brace depth to skip such blocks. An external `#[cfg(test)] mod tests;`
    // (semicolon, no body) does NOT open a skip scope, so production locks declared
    // after it (common in large modules) are still checked.
    let mut depth: i32 = 0;
    let mut in_test = false;
    let mut test_depth: i32 = 0;
    let mut armed = false; // saw `#[cfg(test)]`, awaiting the gated item's `{`

    for (i, line) in content.lines().enumerate() {
        let trimmed = line.trim();

        if !in_test && trimmed.contains("#[cfg(test)]") {
            armed = true;
        }

        let opens = line.matches('{').count() as i32;
        let closes = line.matches('}').count() as i32;

        // Whether THIS line is the item a preceding `#[cfg(test)]` gates: a
        // `#[cfg(test)] static LOCK: Mutex<…>` is test-only by definition and
        // must not require documentation, exactly like locks inside
        // `#[cfg(test)] mod tests { … }` blocks.
        let gated_by_cfg_test = armed;

        if armed {
            if opens > 0 {
                // The gated item has a body — enter the test scope.
                in_test = true;
                test_depth = depth;
                armed = false;
            } else if trimmed.contains(';') {
                // Gated item without a body (external `mod x;`, `use ...;`,
                // a gated `static …;`) — no scope.
                armed = false;
            }
        }

        let is_static_decl =
            !(trimmed.starts_with("//") || trimmed.starts_with("///") || trimmed.starts_with("#["))
                && (trimmed.starts_with("static ") || trimmed.starts_with("pub static "))
                && (trimmed.contains("Mutex<") || trimmed.contains("RwLock<"));

        if is_static_decl && !in_test && !gated_by_cfg_test {
            let lock_name = extract_lock_name(trimmed);
            let location = format!("{}:{}", rel.display(), i + 1);
            results.push((lock_name, location));
        }

        depth += opens - closes;
        if in_test && depth <= test_depth {
            in_test = false;
        }
    }
}

fn extract_lock_name(line: &str) -> String {
    let line = line.trim_start_matches("pub ");
    if let Some(rest) = line.strip_prefix("static ")
        && let Some(colon) = rest.find(':')
    {
        return rest[..colon].trim().to_string();
    }
    for part in line.split_whitespace() {
        if part.chars().all(|c| c.is_uppercase() || c == '_') && part.len() > 1 {
            return part.trim_end_matches(':').to_string();
        }
    }
    line.chars().take(60).collect()
}

fn load_lock_ordering_doc() -> String {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let doc_path = manifest.join("LOCK_ORDERING.md");
    std::fs::read_to_string(&doc_path)
        .unwrap_or_else(|e| panic!("Cannot read {}: {e}", doc_path.display()))
}

#[test]
fn all_static_locks_documented() {
    let src_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let locks = collect_lock_files_from_source(&src_dir);
    let doc = load_lock_ordering_doc();

    let doc_referenced: HashSet<&str> = doc
        .lines()
        .filter(|l| l.contains('|') && (l.contains("Mutex") || l.contains("RwLock")))
        .flat_map(|l| l.split('|'))
        .map(|s| s.trim().trim_start_matches('`').trim_end_matches('`'))
        .filter(|s| !s.is_empty())
        .collect();

    let mut undocumented = Vec::new();
    for (name, location) in &locks {
        let name_clean = name.trim();
        let is_documented = doc_referenced
            .iter()
            .any(|entry| entry.contains(name_clean) || doc.contains(&location.replace("src/", "")));
        if !is_documented {
            undocumented.push(format!("  {name_clean} at {location}"));
        }
    }

    assert!(
        undocumented.is_empty(),
        "Undocumented static Mutex/RwLock found! Add them to LOCK_ORDERING.md:\n{}",
        undocumented.join("\n")
    );
}
