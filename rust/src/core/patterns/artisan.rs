macro_rules! static_regex {
    ($pattern:expr) => {{
        static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
        RE.get_or_init(|| {
            regex::Regex::new($pattern).expect(concat!("BUG: invalid static regex: ", $pattern))
        })
    }};
}

fn migration_status_re() -> &'static regex::Regex {
    static_regex!(r"\|\s*(Ran|Pending)\s*\|\s*(.+?)\s*\|")
}
fn route_re() -> &'static regex::Regex {
    static_regex!(r"(GET|POST|PUT|PATCH|DELETE|ANY)\s*\|\s*(\S+)\s*\|\s*(\S+)")
}
fn test_result_re() -> &'static regex::Regex {
    static_regex!(r"Tests:\s*(\d+)\s*passed(?:,\s*(\d+)\s*failed)?")
}
fn pest_result_re() -> &'static regex::Regex {
    static_regex!(r"(\d+)\s*passed.*?(\d+)\s*failed|(\d+)\s*passed")
}

pub fn compress(command: &str, output: &str) -> Option<String> {
    let trimmed = output.trim();
    if trimmed.is_empty() {
        return Some("ok".to_string());
    }

    if command.contains("migrate") && command.contains("--status") {
        return Some(compress_migrate_status(trimmed));
    }
    if command.contains("migrate") {
        return Some(compress_migrate(trimmed));
    }
    if command.contains("test") {
        return Some(compress_test(trimmed));
    }
    if command.contains("route:list") {
        return Some(compress_routes(trimmed));
    }
    if command.contains("make:") {
        return Some(compress_make(trimmed));
    }
    if command.contains("queue:work") || command.contains("queue:listen") {
        return Some(compress_queue(trimmed));
    }
    if command.contains("tinker") {
        return Some(compress_tinker(trimmed));
    }

    Some(compact_lines(trimmed, 10))
}

fn compress_migrate(output: &str) -> String {
    let mut ran = 0u32;
    let mut errors = Vec::new();

    for line in output.lines() {
        let t = line.trim();
        if t.contains("Migrating:") || t.contains("DONE") {
            ran += 1;
        }
        if t.starts_with("SQLSTATE") || t.contains("ERROR") || t.contains("Exception") {
            errors.push(t.to_string());
        }
    }

    if !errors.is_empty() {
        return format!("migrate FAILED:\n{}", errors.join("\n"));
    }
    if ran > 0 {
        format!("migrated {ran} tables")
    } else if output.contains("Nothing to migrate") {
        "nothing to migrate".to_string()
    } else {
        compact_lines(output, 5)
    }
}

fn compress_migrate_status(output: &str) -> String {
    let statuses: Vec<String> = migration_status_re()
        .captures_iter(output)
        .map(|c| {
            let status = if &c[1] == "Ran" { "+" } else { "-" };
            format!("{} {}", status, c[2].trim())
        })
        .collect();

    if statuses.is_empty() {
        return compact_lines(output, 10);
    }

    let ran = statuses.iter().filter(|s| s.starts_with('+')).count();
    let pending = statuses.iter().filter(|s| s.starts_with('-')).count();
    let mut result = format!("{ran} ran, {pending} pending:");

    for s in statuses.iter().rev().take(10) {
        result.push_str(&format!("\n  {s}"));
    }
    if statuses.len() > 10 {
        result.push_str(&format!("\n  ... +{} more", statuses.len() - 10));
    }
    result
}

fn compress_test(output: &str) -> String {
    let mut passed = 0u32;
    let mut failed = 0u32;
    let mut failures = Vec::new();
    let mut time = String::new();

    for line in output.lines() {
        let t = line.trim();
        if let Some(caps) = test_result_re().captures(t) {
            passed = caps[1].parse().unwrap_or(0);
            failed = caps
                .get(2)
                .and_then(|m| m.as_str().parse().ok())
                .unwrap_or(0);
        }
        if let Some(caps) = pest_result_re().captures(t) {
            if let Some(p) = caps.get(3) {
                passed = p.as_str().parse().unwrap_or(0);
            } else {
                passed = caps[1].parse().unwrap_or(0);
                failed = caps[2].parse().unwrap_or(0);
            }
        }
        if t.starts_with("FAIL") || t.starts_with("✕") || t.starts_with("×") {
            failures.push(t.to_string());
        }
        if t.contains("Time:") || t.contains("Duration:") {
            time = t.to_string();
        }
    }

    let status = if failed > 0 { "FAIL" } else { "ok" };
    let mut result = format!("{status}: {passed} passed, {failed} failed");
    if !time.is_empty() {
        result.push_str(&format!(" ({})", time.trim()));
    }
    if !failures.is_empty() {
        result.push_str("\nfailed:");
        for f in failures.iter().take(10) {
            result.push_str(&format!("\n  {f}"));
        }
    }
    result
}

fn compress_routes(output: &str) -> String {
    let routes: Vec<String> = route_re()
        .captures_iter(output)
        .map(|c| format!("{} {} → {}", &c[1], &c[2], &c[3]))
        .collect();

    if routes.is_empty() {
        return compact_lines(output, 15);
    }

    let mut result = format!("{} routes:", routes.len());
    for r in routes.iter().take(20) {
        result.push_str(&format!("\n  {r}"));
    }
    if routes.len() > 20 {
        result.push_str(&format!("\n  ... +{} more", routes.len() - 20));
    }
    result
}

fn compress_make(output: &str) -> String {
    let lines: Vec<&str> = output
        .lines()
        .filter(|l| {
            let t = l.trim();
            !t.is_empty() && !t.starts_with("INFO")
        })
        .collect();

    if lines.is_empty() {
        return "created".to_string();
    }

    let created = output
        .lines()
        .find(|l| l.contains("created successfully") || l.contains(".php"));

    if let Some(c) = created {
        c.trim().to_string()
    } else {
        compact_lines(output, 3)
    }
}

fn compress_queue(output: &str) -> String {
    let mut processed = 0u32;
    let mut failed = 0u32;
    let mut last_job = String::new();

    for line in output.lines() {
        let t = line.trim();
        if t.contains("Processed") || t.contains("[DONE]") {
            processed += 1;
            if let Some(job) = t.split_whitespace().last() {
                last_job = job.to_string();
            }
        }
        if t.contains("FAILED") || t.contains("[ERROR]") {
            failed += 1;
        }
    }

    if processed == 0 && failed == 0 {
        return compact_lines(output, 5);
    }

    let mut result = format!("queue: {processed} processed");
    if failed > 0 {
        result.push_str(&format!(", {failed} failed"));
    }
    if !last_job.is_empty() {
        result.push_str(&format!(" (last: {last_job})"));
    }
    result
}

fn compress_tinker(output: &str) -> String {
    let lines: Vec<&str> = output
        .lines()
        .filter(|l| {
            let t = l.trim();
            !t.is_empty()
                && !t.starts_with("Psy Shell")
                && !t.starts_with(">>>")
                && !t.starts_with("...")
        })
        .collect();

    if lines.is_empty() {
        return "tinker (no output)".to_string();
    }
    if lines.len() <= 10 {
        return lines.join("\n");
    }
    format!(
        "{}\n... ({} more lines)",
        lines[..8].join("\n"),
        lines.len() - 8
    )
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn artisan_migrate_success() {
        let output =
            "Migrating: 2026_01_01_create_users_table\nMigrating: 2026_01_02_create_posts_table";
        let result = compress("php artisan migrate", output).unwrap();
        assert!(result.contains("migrated 2"), "shows count: {result}");
    }

    #[test]
    fn artisan_migrate_nothing() {
        let output = "Nothing to migrate.";
        let result = compress("php artisan migrate", output).unwrap();
        assert!(result.contains("nothing to migrate"), "{result}");
    }

    #[test]
    fn artisan_test_success() {
        let output = "  PASS  Tests\\Unit\\UserTest\n  ✓ it can create user\n  ✓ it validates email\n\n  Tests:  2 passed\n  Time:   0.45s";
        let result = compress("php artisan test", output).unwrap();
        assert!(result.contains("ok: 2 passed"), "{result}");
    }

    #[test]
    fn artisan_test_failure() {
        let output = "  FAIL  Tests\\Unit\\UserTest\n  ✕ it validates email\n\n  Tests:  1 passed, 1 failed\n  Time:   0.52s";
        let result = compress("php artisan test", output).unwrap();
        assert!(result.contains("FAIL: 1 passed, 1 failed"), "{result}");
    }

    #[test]
    fn artisan_make_model() {
        let output = "\n   INFO  Model [app/Models/Invoice.php] created successfully.\n";
        let result = compress("php artisan make:model Invoice", output).unwrap();
        assert!(
            result.contains("Invoice") || result.contains("created"),
            "{result}"
        );
    }

    #[test]
    fn pest_test_output() {
        let output = "  PASS  Tests\\Feature\\AuthTest\n  ✓ login works\n  ✓ register works\n\n  3 passed (0.8s)";
        let result = compress("./vendor/bin/pest", output).unwrap();
        assert!(result.contains("3 passed"), "{result}");
    }

    #[test]
    fn route_list_compression() {
        let output = "  GET|HEAD  /api/users ................. UserController@index\n  POST      /api/users ................. UserController@store\n  GET|HEAD  /api/users/{user} .......... UserController@show\n  PUT|PATCH /api/users/{user} .......... UserController@update\n  DELETE    /api/users/{user} .......... UserController@destroy";
        let result = compress("php artisan route:list", output).unwrap();
        assert!(result.len() < output.len(), "should compress");
    }
}
