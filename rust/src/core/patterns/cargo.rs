macro_rules! static_regex {
    ($pattern:expr_2021) => {{
        static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
        RE.get_or_init(|| {
            regex::Regex::new($pattern).expect(concat!("BUG: invalid static regex: ", $pattern))
        })
    }};
}

fn compiling_re() -> &'static regex::Regex {
    static_regex!(r"Compiling (\S+) v(\S+)")
}
fn checking_re() -> &'static regex::Regex {
    static_regex!(r"Checking (\S+) v(\S+)")
}
fn error_re() -> &'static regex::Regex {
    static_regex!(r"error\[E(\d+)\]: (.+)")
}
fn warning_re() -> &'static regex::Regex {
    static_regex!(r"warning(?:\[clippy::([^\]]+)\])?: (.+)")
}
fn generated_warnings_re() -> &'static regex::Regex {
    static_regex!(r"generated (\d+) warnings?")
}
fn generic_error_re() -> &'static regex::Regex {
    static_regex!(r"error(?:\[E\d+\])?: (.+)")
}
fn clippy_rule_re() -> &'static regex::Regex {
    static_regex!(r"clippy::([A-Za-z0-9_-]+)")
}
fn failed_test_re() -> &'static regex::Regex {
    static_regex!(r"^test (.+) \.\.\. FAILED$")
}
fn failed_test_header_re() -> &'static regex::Regex {
    static_regex!(r"^---- (.+) stdout ----$")
}
fn test_result_re() -> &'static regex::Regex {
    static_regex!(r"test result: (\w+)\. (\d+) passed; (\d+) failed; (\d+) ignored")
}
fn finished_re() -> &'static regex::Regex {
    static_regex!(r"Finished .+ in (\d+\.?\d*s)")
}

/// Compress output from a recognized Cargo subcommand.
pub fn compress(command: &str, output: &str) -> Option<String> {
    let args = command.strip_prefix("cargo ").unwrap_or(command);
    let subcmd = args.split_whitespace().next().unwrap_or("");
    match subcmd {
        "build" | "b" | "check" | "c" => Some(compress_build(output)),
        "test" | "t" | "nextest" => Some(compress_test(output)),
        "clippy" => Some(compress_clippy(output)),
        "clean" => Some(compress_clean(output)),
        "install" => Some(compress_install(output)),
        "add" => Some(compress_add(output)),
        "remove" | "rm" => Some(compress_remove(output)),
        "doc" | "d" => Some(compress_doc(output)),
        "tree" => Some(compress_tree(output)),
        "fmt" => Some(compress_fmt(output)),
        "update" | "up" => Some(compress_update(output)),
        "metadata" => Some(compress_metadata(output)),
        "run" | "r" => Some(compress_run(output)),
        "bench" => Some(compress_bench(output)),
        _ => None,
    }
}

fn compress_build(output: &str) -> String {
    let mut compiled = 0u32;
    let mut checked = 0u32;
    let mut errors = Vec::new();
    let mut time = String::new();

    for line in output.lines() {
        if compiling_re().is_match(line) {
            compiled += 1;
        } else if checking_re().is_match(line) {
            checked += 1;
        }
        if let Some(caps) = error_re().captures(line) {
            errors.push(format!("E{}: {}", &caps[1], &caps[2]));
        }
        if let Some(caps) = finished_re().captures(line) {
            time = caps[1].to_string();
        }
    }

    let mut parts = Vec::new();
    if compiled > 0 {
        parts.push(counted_crates("compiled", compiled));
    }
    if checked > 0 {
        parts.push(counted_crates("checked", checked));
    }
    if !errors.is_empty() {
        parts.push(format!("{} errors:", errors.len()));
        for e in &errors {
            parts.push(format!("  {e}"));
        }
    }
    let warning_groups = group_warnings(output);
    let warning_total = warning_total(output, &warning_groups);
    if warning_total > 0 {
        parts.push(format_warning_groups(&warning_groups, warning_total));
    }
    if !time.is_empty() {
        parts.push(format!("({time})"));
    }

    if parts.is_empty() {
        return "ok".to_string();
    }
    parts.join("\n")
}

fn compress_test(output: &str) -> String {
    let mut failed_tests = Vec::new();
    let mut passed = 0u32;
    let mut failed = 0u32;
    let mut skipped = 0u32;
    let mut time = String::new();
    let mut compile_lines = Vec::new();
    let mut passed_names: Vec<String> = Vec::new();
    let mut in_test_phase = false;

    for line in output.lines() {
        if let Some(caps) = test_result_re().captures(line) {
            passed += caps[2].parse::<u32>().unwrap_or_default();
            failed += caps[3].parse::<u32>().unwrap_or_default();
            skipped += caps[4].parse::<u32>().unwrap_or_default();
            in_test_phase = true;
        } else if line.trim_start().starts_with("running ") {
            in_test_phase = true;
        } else if !in_test_phase {
            compile_lines.push(line);
        }
        let trimmed = line.trim();
        if trimmed.starts_with("test ") && trimmed.ends_with("... ok") {
            if let Some(name) = trimmed
                .strip_prefix("test ")
                .and_then(|r| r.strip_suffix(" ... ok"))
            {
                passed_names.push(name.to_string());
            }
        }
        if let Some(caps) = failed_test_re().captures(trimmed) {
            failed_tests.push(caps[1].to_string());
        } else if let Some(caps) = failed_test_header_re().captures(trimmed) {
            failed_tests.push(caps[1].to_string());
        }
        if let Some(caps) = finished_re().captures(line) {
            time = caps[1].to_string();
        }
    }

    let mut parts = Vec::new();
    let compile_summary = compile_phase_summary(&compile_lines.join("\n"));
    if !compile_summary.is_empty() {
        parts.push(format!("[{compile_summary}]"));
    }
    if passed > 0 || failed > 0 || skipped > 0 {
        let mut result = format!("cargo test: {passed} passed, {failed} failed");
        if skipped > 0 {
            result.push_str(&format!(", {skipped} skipped"));
        }
        failed_tests.sort_unstable();
        failed_tests.dedup();
        if !failed_tests.is_empty() {
            let shown = failed_tests.iter().take(5).cloned().collect::<Vec<_>>();
            let suffix = if failed_tests.len() > shown.len() {
                format!(", ... +{} more", failed_tests.len() - shown.len())
            } else {
                String::new()
            };
            result.push_str(&format!(" ({}){suffix}", shown.join(", ")));
        }
        if failed_tests.is_empty() && !passed_names.is_empty() {
            let total = passed_names.len();
            let shown: Vec<_> = passed_names.iter().take(5).cloned().collect();
            let suffix = if total > 5 {
                format!(" ...+{} more", total - 5)
            } else {
                String::new()
            };
            result.push_str(&format!(
                "
  ran: {}{suffix}",
                shown.join(", ")
            ));
        }
        parts.push(result);
    }
    if !time.is_empty() {
        parts.push(format!("({time})"));
    }

    if parts.is_empty() {
        return "ok".to_string();
    }
    parts.join("\n")
}

fn compress_clippy(output: &str) -> String {
    let errors = group_clippy_errors(output);
    let warnings = group_warnings(output);
    let warning_total = warning_total(output, &warnings);

    let mut parts = Vec::new();
    if !errors.is_empty() {
        let error_total = errors.iter().map(|(_, count)| count).sum();
        parts.push(format_rule_groups(
            &errors,
            error_total,
            "error",
            "errors",
            "rules",
        ));
    }
    if warning_total > 0 {
        parts.push(format_rule_groups(
            &warnings,
            warning_total,
            "warning",
            "warnings",
            "rules",
        ));
    }

    if parts.is_empty() {
        return "clean".to_string();
    }
    parts.join("\n")
}

fn group_warnings(output: &str) -> Vec<(String, u32)> {
    let mut counts = std::collections::BTreeMap::new();
    for line in output.lines() {
        let Some(caps) = warning_re().captures(line.trim()) else {
            continue;
        };
        let message = caps.get(2).map_or("", |capture| capture.as_str());
        if message.contains("generated ") {
            continue;
        }
        let rule = caps
            .get(1)
            .map(|capture| normalize_rule(capture.as_str()))
            .unwrap_or_else(|| message_prefix(message));
        *counts.entry(rule).or_insert(0) += 1;
    }
    let mut groups: Vec<_> = counts.into_iter().collect();
    groups.sort_unstable_by_key(|(name, count)| (std::cmp::Reverse(*count), name.clone()));
    groups
}

fn group_clippy_errors(output: &str) -> Vec<(String, u32)> {
    let mut rules = Vec::new();
    let mut last_error = None;
    for line in output.lines() {
        let trimmed = line.trim();
        if let Some(caps) = generic_error_re().captures(trimmed) {
            let message = caps.get(1).map_or("", |capture| capture.as_str());
            let rule = clippy_rule_re()
                .captures(trimmed)
                .and_then(|rule_caps| rule_caps.get(1))
                .map_or_else(
                    || message_prefix(message),
                    |capture| normalize_rule(capture.as_str()),
                );
            rules.push(rule);
            last_error = Some(rules.len() - 1);
        } else if let Some(index) = last_error
            && let Some(caps) = clippy_rule_re().captures(trimmed)
            && let Some(rule) = caps.get(1)
        {
            rules[index] = normalize_rule(rule.as_str());
        }
    }
    group_named_rules(rules)
}

fn group_named_rules(rules: Vec<String>) -> Vec<(String, u32)> {
    let mut counts = std::collections::BTreeMap::new();
    for rule in rules {
        *counts.entry(rule).or_insert(0) += 1;
    }
    let mut groups: Vec<_> = counts.into_iter().collect();
    groups.sort_unstable_by_key(|(name, count)| (std::cmp::Reverse(*count), name.clone()));
    groups
}

fn format_warning_groups(groups: &[(String, u32)], total: u32) -> String {
    format_rule_groups(groups, total, "warning", "warnings", "others")
}

fn format_rule_groups(
    groups: &[(String, u32)],
    total: u32,
    singular: &str,
    plural: &str,
    remainder_label: &str,
) -> String {
    let noun = if total == 1 { singular } else { plural };
    let shown = groups
        .iter()
        .take(5)
        .map(|(rule, count)| format!("{rule} ×{count}"))
        .collect::<Vec<_>>();
    let remainder = groups.len().saturating_sub(shown.len());
    let suffix = if remainder > 0 {
        format!(", +{remainder} {remainder_label}")
    } else {
        String::new()
    };
    format!("{total} {noun} ({}){suffix}", shown.join(", "))
}

fn warning_total(output: &str, groups: &[(String, u32)]) -> u32 {
    let generated = output
        .lines()
        .filter_map(|line| generated_warnings_re().captures(line))
        .filter_map(|caps| caps[1].parse::<u32>().ok())
        .sum();
    if generated == 0 {
        groups.iter().map(|(_, count)| count).sum()
    } else {
        generated
    }
}

fn compile_phase_summary(output: &str) -> String {
    let compiled = output
        .lines()
        .filter(|line| compiling_re().is_match(line))
        .count() as u32;
    let checked = output
        .lines()
        .filter(|line| checking_re().is_match(line))
        .count() as u32;
    let groups = group_warnings(output);
    let warnings = warning_total(output, &groups);
    let mut parts = Vec::new();
    if compiled > 0 {
        parts.push(counted_crates("compiled", compiled));
    }
    if checked > 0 {
        parts.push(counted_crates("checked", checked));
    }
    if warnings > 0 {
        let noun = if warnings == 1 { "warning" } else { "warnings" };
        parts.push(format!("{warnings} {noun}"));
    }
    parts.join(", ")
}

fn counted_crates(action: &str, count: u32) -> String {
    let noun = if count == 1 { "crate" } else { "crates" };
    format!("{action} {count} {noun}")
}

fn message_prefix(message: &str) -> String {
    message
        .split_whitespace()
        .next()
        .map(normalize_rule)
        .unwrap_or_else(|| "unknown".to_string())
}

fn normalize_rule(rule: &str) -> String {
    rule.trim_matches('`').replace('-', "_")
}

fn compress_clean(output: &str) -> String {
    output
        .lines()
        .find_map(|line| line.trim().strip_prefix("Removed "))
        .map_or_else(
            || "cleaned".to_string(),
            |removed| {
                format!(
                    "removed {}",
                    removed.split_once(',').map_or(removed, |(files, _)| files)
                )
            },
        )
}

fn compress_install(output: &str) -> String {
    summarize_dependency_action(output, "Installed ", "installed")
}

fn compress_add(output: &str) -> String {
    summarize_dependency_action(output, "Adding ", "added")
}

fn compress_remove(output: &str) -> String {
    summarize_dependency_action(output, "Removing ", "removed")
}

fn summarize_dependency_action(output: &str, prefix: &str, action: &str) -> String {
    output
        .lines()
        .find_map(|line| line.trim().strip_prefix(prefix))
        .and_then(|dependency| dependency.split_whitespace().next())
        .map_or_else(
            || action.to_string(),
            |dependency| format!("{action} {dependency}"),
        )
}

fn compress_doc(output: &str) -> String {
    let mut crate_count = 0u32;
    let mut warnings = 0u32;
    let mut time = String::new();

    for line in output.lines() {
        if line.contains("Documenting ") || compiling_re().is_match(line) {
            crate_count += 1;
        }
        if warning_re().is_match(line) && !line.contains("generated") {
            warnings += 1;
        }
        if let Some(caps) = finished_re().captures(line) {
            time = caps[1].to_string();
        }
    }

    let mut parts = Vec::new();
    if crate_count > 0 {
        parts.push(format!("documented {crate_count} crates"));
    }
    if warnings > 0 {
        parts.push(format!("{warnings} warnings"));
    }
    if !time.is_empty() {
        parts.push(format!("({time})"));
    }
    if parts.is_empty() {
        "ok".to_string()
    } else {
        parts.join("\n")
    }
}

fn compress_tree(output: &str) -> String {
    let lines: Vec<&str> = output.lines().collect();
    if lines.len() <= 20 {
        return output.to_string();
    }

    let direct: Vec<&str> = lines
        .iter()
        .filter(|l| !l.starts_with(' ') || l.starts_with("├── ") || l.starts_with("└── "))
        .copied()
        .collect();

    if direct.is_empty() {
        let shown = &lines[..20.min(lines.len())];
        return format!(
            "{}\n{}",
            shown.join("\n"),
            super::elision_marker(lines.len() - 20)
        );
    }

    format!(
        "{} direct deps ({} total lines):\n{}",
        direct.len(),
        lines.len(),
        direct.join("\n")
    )
}

fn compress_fmt(output: &str) -> String {
    let trimmed = output.trim();
    if trimmed.is_empty() {
        return "ok (formatted)".to_string();
    }

    let diffs: Vec<&str> = trimmed
        .lines()
        .filter(|l| l.starts_with("Diff in ") || l.starts_with("  --> "))
        .collect();

    if !diffs.is_empty() {
        return format!("{} formatting issues:\n{}", diffs.len(), diffs.join("\n"));
    }

    let lines: Vec<&str> = trimmed.lines().filter(|l| !l.trim().is_empty()).collect();
    if lines.len() <= 5 {
        lines.join("\n")
    } else {
        format!(
            "{}\n{}",
            lines[..5].join("\n"),
            super::elision_marker(lines.len() - 5)
        )
    }
}

fn compress_update(output: &str) -> String {
    let mut updated = Vec::new();
    let mut unchanged = 0u32;

    for line in output.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("Updating ") || trimmed.starts_with("    Updating ") {
            updated.push(trimmed.trim_start_matches("    ").to_string());
        } else if trimmed.starts_with("Unchanged ") || trimmed.contains("Unchanged") {
            unchanged += 1;
        }
    }

    if updated.is_empty() && unchanged == 0 {
        let lines: Vec<&str> = output.lines().filter(|l| !l.trim().is_empty()).collect();
        if lines.is_empty() {
            return "ok (up-to-date)".to_string();
        }
        if lines.len() <= 5 {
            return lines.join("\n");
        }
        return format!(
            "{}\n{}",
            lines[..5].join("\n"),
            super::elision_marker(lines.len() - 5)
        );
    }

    let mut parts = Vec::new();
    if !updated.is_empty() {
        parts.push(format!("{} updated:", updated.len()));
        for u in updated.iter().take(15) {
            parts.push(format!("  {u}"));
        }
        if updated.len() > 15 {
            parts.push(format!("  ... +{} more", updated.len() - 15));
        }
    }
    if unchanged > 0 {
        parts.push(format!("{unchanged} unchanged"));
    }
    parts.join("\n")
}

fn compress_run(output: &str) -> String {
    let mut program_lines = Vec::new();
    let mut compiling = 0u32;
    let mut time = String::new();

    for line in output.lines() {
        let trimmed = line.trim();
        if compiling_re().is_match(trimmed) || trimmed.starts_with("Compiling ") {
            compiling += 1;
            continue;
        }
        if trimmed.starts_with("Downloading ")
            || trimmed.starts_with("Downloaded ")
            || trimmed.starts_with("Blocking waiting")
            || trimmed.starts_with("Locking ")
        {
            continue;
        }
        if trimmed.starts_with("Running `") || trimmed.starts_with("Running ") {
            continue;
        }
        if let Some(caps) = finished_re().captures(trimmed) {
            time = caps[1].to_string();
            continue;
        }
        program_lines.push(line);
    }

    let mut result = String::new();
    if compiling > 0 {
        result.push_str(&format!("(compiled {compiling} crates"));
        if !time.is_empty() {
            result.push_str(&format!(", {time}"));
        }
        result.push_str(")\n");
    }

    if program_lines.len() <= 50 {
        result.push_str(&program_lines.join("\n"));
    } else {
        result.push_str(&program_lines[..25].join("\n"));
        result.push_str(&format!(
            "\n... ({} lines omitted)\n",
            program_lines.len() - 50
        ));
        result.push_str(&program_lines[program_lines.len() - 25..].join("\n"));
    }

    if result.trim().is_empty() {
        return "ok".to_string();
    }
    result
}

fn compress_bench(output: &str) -> String {
    let mut compiling = 0u32;
    let mut bench_results = Vec::new();
    let mut time = String::new();
    let mut errors = Vec::new();

    for line in output.lines() {
        let trimmed = line.trim();
        if compiling_re().is_match(trimmed) || trimmed.starts_with("Compiling ") {
            compiling += 1;
            continue;
        }
        if trimmed.starts_with("Downloading ")
            || trimmed.starts_with("Downloaded ")
            || trimmed.starts_with("Blocking waiting")
            || trimmed.starts_with("Locking ")
        {
            continue;
        }
        if trimmed.starts_with("Benchmarking ")
            || trimmed.starts_with("Gnuplot ")
            || trimmed.starts_with("Collecting ")
            || trimmed.starts_with("Warming up")
            || trimmed.starts_with("Analyzing ")
        {
            continue;
        }
        if trimmed.starts_with("Running ") && trimmed.contains("target") {
            continue;
        }
        if let Some(caps) = finished_re().captures(trimmed) {
            time = caps[1].to_string();
            continue;
        }
        if let Some(caps) = error_re().captures(trimmed) {
            errors.push(format!("E{}: {}", &caps[1], &caps[2]));
            continue;
        }
        if trimmed.starts_with("test ") && trimmed.contains("bench:") {
            bench_results.push(trimmed.to_string());
            continue;
        }
        if trimmed.contains("time:") || trimmed.contains("thrpt:") {
            bench_results.push(trimmed.to_string());
            continue;
        }
        if let Some(caps) = test_result_re().captures(trimmed) {
            bench_results.push(format!(
                "{}: {} pass, {} fail, {} skip",
                &caps[1], &caps[2], &caps[3], &caps[4]
            ));
        }
    }

    let mut parts = Vec::new();

    if !errors.is_empty() {
        parts.push(format!("{} errors:", errors.len()));
        for e in &errors {
            parts.push(format!("  {e}"));
        }
        return parts.join("\n");
    }

    if compiling > 0 {
        let mut header = format!("compiled {compiling} crates");
        if !time.is_empty() {
            header.push_str(&format!(" ({time})"));
        }
        parts.push(header);
    }

    if bench_results.is_empty() {
        parts.push("no benchmark results captured".to_string());
    } else {
        parts.push(format!("{} benchmarks:", bench_results.len()));
        for b in &bench_results {
            parts.push(format!("  {b}"));
        }
    }

    if parts.is_empty() {
        return "ok".to_string();
    }
    parts.join("\n")
}

fn compress_metadata(output: &str) -> String {
    let parsed: Result<serde_json::Value, _> = serde_json::from_str(output);
    let Ok(json) = parsed else {
        let lines: Vec<&str> = output.lines().collect();
        if lines.len() <= 20 {
            return output.to_string();
        }
        return format!(
            "{}\n... ({} more lines, non-JSON metadata)",
            lines[..10].join("\n"),
            lines.len() - 10
        );
    };

    let mut parts = Vec::new();

    if let Some(workspace_members) = json.get("workspace_members").and_then(|v| v.as_array()) {
        parts.push(format!("workspace_members: {}", workspace_members.len()));
        for m in workspace_members.iter().take(20) {
            if let Some(s) = m.as_str() {
                let short = s.split(' ').take(2).collect::<Vec<_>>().join(" ");
                parts.push(format!("  {short}"));
            }
        }
        if workspace_members.len() > 20 {
            parts.push(format!("  ... +{} more", workspace_members.len() - 20));
        }
    }

    if let Some(target_dir) = json.get("target_directory").and_then(|v| v.as_str()) {
        parts.push(format!("target_directory: {target_dir}"));
    }

    if let Some(workspace_root) = json.get("workspace_root").and_then(|v| v.as_str()) {
        parts.push(format!("workspace_root: {workspace_root}"));
    }

    if let Some(packages) = json.get("packages").and_then(|v| v.as_array()) {
        parts.push(format!("packages: {}", packages.len()));
        for pkg in packages.iter().take(30) {
            let name = pkg.get("name").and_then(|v| v.as_str()).unwrap_or("?");
            let version = pkg.get("version").and_then(|v| v.as_str()).unwrap_or("?");
            let features: Vec<&str> = pkg
                .get("features")
                .and_then(|v| v.as_object())
                .map(|f| f.keys().map(std::string::String::as_str).collect())
                .unwrap_or_default();
            if features.is_empty() {
                parts.push(format!("  {name} v{version}"));
            } else {
                parts.push(format!(
                    "  {name} v{version} [features: {}]",
                    features.join(", ")
                ));
            }
        }
        if packages.len() > 30 {
            parts.push(format!("  ... +{} more", packages.len() - 30));
        }
    }

    if let Some(resolve) = json.get("resolve")
        && let Some(nodes) = resolve.get("nodes").and_then(|v| v.as_array())
    {
        let total_deps: usize = nodes
            .iter()
            .map(|n| {
                n.get("deps")
                    .and_then(|v| v.as_array())
                    .map_or(0, std::vec::Vec::len)
            })
            .sum();
        parts.push(format!(
            "resolve: {} nodes, {} dep edges",
            nodes.len(),
            total_deps
        ));
    }

    if parts.is_empty() {
        "cargo metadata: ok (empty)".to_string()
    } else {
        parts.join("\n")
    }
}

#[cfg(test)]
mod tests {
    use super::compress;

    #[test]
    fn cargo_build_success() {
        let output = "   Compiling lean-ctx v2.1.1\n    Finished release profile [optimized] target(s) in 30.5s";
        let result = compress("cargo build", output).unwrap();
        assert!(result.contains("compiled"), "should mention compilation");
        assert!(result.contains("30.5s"), "should include build time");
    }

    #[test]
    fn cargo_build_with_errors() {
        let output = "   Compiling lean-ctx v2.1.1\nerror[E0308]: mismatched types\n --> src/main.rs:10:5\n  |\n10|     1 + \"hello\"\n  |         ^^^^^^^ expected integer, found &str";
        let result = compress("cargo build", output).unwrap();
        assert!(result.contains("E0308"), "should contain error code");
    }

    #[test]
    fn cargo_test_success() {
        let output = "running 5 tests\ntest test_one ... ok\ntest test_two ... ok\ntest test_three ... ok\ntest test_four ... ok\ntest test_five ... ok\n\ntest result: ok. 5 passed; 0 failed; 0 ignored";
        let result = compress("cargo test", output).unwrap();
        assert!(result.contains("5 pass"), "should show passed count");
    }

    #[test]
    fn cargo_test_failure() {
        let output = "running 3 tests\ntest test_ok ... ok\ntest test_fail ... FAILED\ntest test_ok2 ... ok\n\ntest result: FAILED. 2 passed; 1 failed; 0 ignored";
        let result = compress("cargo test", output).unwrap();
        assert!(result.contains("1 failed"), "should indicate failure");
        assert!(result.contains("test_fail"), "should name failed test");
    }

    #[test]
    fn test_build_with_checking() {
        let output = "Compiling app v0.1.0\nChecking dep v1.0.0\nChecking dep-two v2.0.0\nFinished dev profile in 1.2s";
        let result = compress("cargo check", output).unwrap();
        assert!(result.contains("compiled 1 crate"));
        assert!(result.contains("checked 2 crates"));
    }

    #[test]
    fn test_build_with_warnings() {
        let warnings = ["warning: unused value"; 10].join("\n");
        let output =
            format!("Compiling app v0.1.0\n{warnings}\nwarning: app generated 10 warnings");
        let result = compress("cargo build", &output).unwrap();
        assert!(result.contains("10 warnings (unused ×10)"));
    }

    #[test]
    fn test_clippy_grouped() {
        let output = "warning[clippy::needless-borrow]: needless borrow\nwarning[clippy::unused-imports]: unused import\nwarning[clippy::dead-code]: dead code\nwarning[clippy::manual-map]: manual map\nwarning[clippy::map-clone]: map clone\nwarning[clippy::redundant-closure]: redundant closure";
        let result = compress("cargo clippy", output).unwrap();
        assert!(result.contains("6 warnings"));
        assert!(result.contains("needless_borrow ×1"));
        assert!(result.contains("+1 rules"));
    }

    #[test]
    fn test_test_with_compile_warnings() {
        let output = "Compiling app v0.1.0\nwarning: unused value\nrunning 2 tests\ntest one ... ok\ntest two ... ok\ntest result: ok. 2 passed; 0 failed; 0 ignored\nFinished test profile in 1.4s";
        let result = compress("cargo test", output).unwrap();
        assert!(result.contains("[compiled 1 crate, 1 warning]"));
        assert!(result.contains("cargo test: 2 passed, 0 failed"));
    }

    #[test]
    fn test_dispatch_precision() {
        assert!(
            compress(
                "cargo test",
                "test result: ok. 0 passed; 0 failed; 0 ignored"
            )
            .is_some()
        );
        assert!(compress("cargo latest", "latest version").is_none());
    }

    #[test]
    fn test_clean_handler() {
        let result = compress("cargo clean", "Removed 42 files, 12.3MiB total").unwrap();
        assert_eq!(result, "removed 42 files");
    }

    #[test]
    fn cargo_clippy_clean() {
        let output = "    Checking lean-ctx v2.1.1\n    Finished `dev` profile [unoptimized + debuginfo] target(s) in 5.2s";
        let result = compress("cargo clippy", output).unwrap();
        assert!(result.contains("clean"), "clean clippy should say clean");
    }

    #[test]
    fn cargo_check_routes_to_build() {
        let output = "    Checking lean-ctx v2.1.1\n    Finished `dev` profile [unoptimized + debuginfo] target(s) in 2.1s";
        let result = compress("cargo check", output);
        assert!(
            result.is_some(),
            "cargo check should route to build compressor"
        );
    }

    #[test]
    fn cargo_metadata_json() {
        let json = r#"{
            "packages": [
                {"name": "lean-ctx", "version": "3.2.9", "features": {"tree-sitter": ["dep:tree-sitter"]}},
                {"name": "serde", "version": "1.0.200", "features": {"derive": ["serde_derive"]}}
            ],
            "workspace_members": ["lean-ctx 3.2.9 (path+file:///foo)"],
            "workspace_root": "/foo",
            "target_directory": "/foo/target",
            "resolve": {
                "nodes": [
                    {"id": "lean-ctx", "deps": [{"name": "serde"}]},
                    {"id": "serde", "deps": []}
                ]
            }
        }"#;
        let result = compress("cargo metadata", json).unwrap();
        assert!(
            result.contains("workspace_members: 1"),
            "should list workspace members"
        );
        assert!(result.contains("packages: 2"), "should list packages");
        assert!(
            result.contains("resolve: 2 nodes"),
            "should summarize resolve graph"
        );
        assert!(
            result.len() < json.len(),
            "compressed output should be shorter"
        );
    }

    #[test]
    fn cargo_run_strips_compilation() {
        let output = "   Compiling lean-ctx v2.1.1\n    Finished `dev` profile [unoptimized] target(s) in 5.2s\n     Running `target/debug/lean-ctx`\nHello, world!\nResult: 42";
        let result = compress("cargo run", output).unwrap();
        assert!(
            !result.contains("Running `target"),
            "should strip Running line"
        );
        assert!(
            result.contains("Hello, world!"),
            "should keep program output"
        );
        assert!(result.contains("compiled"), "should summarize compilation");
    }

    #[test]
    fn cargo_bench_keeps_results() {
        let output = "   Compiling lean-ctx v2.1.1\n    Finished `bench` profile [optimized] target(s) in 12.0s\n     Running benches/main.rs\ntest bench_parse  ... bench:     1,234 ns/iter (+/- 56)\ntest bench_render ... bench:     5,678 ns/iter (+/- 123)\n\ntest result: ok. 0 passed; 0 failed; 2 ignored";
        let result = compress("cargo bench", output).unwrap();
        assert!(result.contains("bench_parse"), "should keep bench results");
        assert!(result.contains("bench_render"), "should keep bench results");
        assert!(result.contains("compiled"), "should summarize compilation");
    }

    #[test]
    fn cargo_bench_with_criterion() {
        let output = "   Compiling bench-suite v0.1.0\nBenchmarking parser/parse_large\nCollecting 100 samples\nWarming up for 3.0000 s\nAnalyzing results...\nparser/parse_large      time:   [1.2345 ms 1.3000 ms 1.3500 ms]";
        let result = compress("cargo bench", output).unwrap();
        assert!(
            result.contains("time:"),
            "should keep criterion timing lines"
        );
        assert!(!result.contains("Collecting"), "should strip progress");
    }

    #[test]
    fn cargo_metadata_non_json() {
        let output = "error: `cargo metadata` exited with an error\nsome detailed error";
        let result = compress("cargo metadata", output).unwrap();
        assert!(
            result.contains("error"),
            "should pass through non-JSON output"
        );
    }
}
