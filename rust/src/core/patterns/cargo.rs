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
fn error_re() -> &'static regex::Regex {
    static_regex!(r"error\[E(\d+)\]: (.+)")
}
fn warning_re() -> &'static regex::Regex {
    static_regex!(r"warning: (.+)")
}
fn test_result_re() -> &'static regex::Regex {
    static_regex!(r"test result: (\w+)\. (\d+) passed; (\d+) failed; (\d+) ignored")
}
fn finished_re() -> &'static regex::Regex {
    static_regex!(r"Finished .+ in (\d+\.?\d*s)")
}

#[must_use]
pub fn compress(command: &str, output: &str) -> Option<String> {
    if command.contains("build") || command.contains("check") {
        return Some(compress_build(output));
    }
    if command.contains("test") {
        return Some(compress_test(output));
    }
    if command.contains("clippy") {
        return Some(compress_clippy(output));
    }
    if command.contains("doc") {
        return Some(compress_doc(output));
    }
    if command.contains("tree") {
        return Some(compress_tree(output));
    }
    if command.contains("fmt") {
        return Some(compress_fmt(output));
    }
    if command.contains("update") {
        return Some(compress_update(output));
    }
    if command.contains("metadata") {
        return Some(compress_metadata(output));
    }
    if command.contains("run") {
        return Some(compress_run(output));
    }
    if command.contains("bench") {
        return Some(compress_bench(output));
    }
    None
}

fn compress_build(output: &str) -> String {
    let mut crate_count = 0u32;
    let mut errors = Vec::new();
    let mut warnings = 0u32;
    let mut time = String::new();

    for line in output.lines() {
        if compiling_re().is_match(line) {
            crate_count += 1;
        }
        if let Some(caps) = error_re().captures(line) {
            errors.push(format!("E{}: {}", &caps[1], &caps[2]));
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
        parts.push(format!("compiled {crate_count} crates"));
    }
    if !errors.is_empty() {
        parts.push(format!("{} errors:", errors.len()));
        for e in &errors {
            parts.push(format!("  {e}"));
        }
    }
    if warnings > 0 {
        parts.push(format!("{warnings} warnings"));
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
    let mut results = Vec::new();
    let mut failed_tests = Vec::new();
    let mut passed_tests = Vec::new();
    let mut time = String::new();

    for line in output.lines() {
        if let Some(caps) = test_result_re().captures(line) {
            results.push(format!(
                "{}: {} pass, {} fail, {} skip",
                &caps[1], &caps[2], &caps[3], &caps[4]
            ));
        }
        if line.contains("FAILED") && line.contains("---") {
            let name = line.split_whitespace().nth(1).unwrap_or("?");
            failed_tests.push(name.to_string());
        }
        if line.starts_with("test ")
            && line.ends_with(" ... ok")
            && let Some(name) = line
                .strip_prefix("test ")
                .and_then(|s| s.strip_suffix(" ... ok"))
        {
            let short_name = if name.len() > 50 {
                &name[..name.floor_char_boundary(50)]
            } else {
                name
            };
            passed_tests.push(short_name.to_string());
        }
        if let Some(caps) = finished_re().captures(line) {
            time = caps[1].to_string();
        }
    }

    let mut parts = Vec::new();
    if !results.is_empty() {
        parts.extend(results);
    }
    if !failed_tests.is_empty() {
        parts.push(format!("failed: {}", failed_tests.join(", ")));
    }
    if !passed_tests.is_empty() {
        let total = passed_tests.len();
        let shown: Vec<_> = passed_tests.into_iter().take(5).collect();
        let suffix = if total > 5 {
            format!(" ...+{} more", total - 5)
        } else {
            String::new()
        };
        parts.push(format!("ran: {}{suffix}", shown.join(", ")));
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
    let mut warnings = Vec::new();
    let mut errors = Vec::new();

    for line in output.lines() {
        if let Some(caps) = error_re().captures(line) {
            errors.push(caps[2].to_string());
        } else if let Some(caps) = warning_re().captures(line) {
            let msg = &caps[1];
            if !msg.contains("generated") && !msg.starts_with('`') {
                warnings.push(msg.to_string());
            }
        }
    }

    let mut parts = Vec::new();
    if !errors.is_empty() {
        parts.push(format!("{} errors: {}", errors.len(), errors.join("; ")));
    }
    if !warnings.is_empty() {
        parts.push(format!("{} warnings", warnings.len()));
    }

    if parts.is_empty() {
        return "clean".to_string();
    }
    parts.join("\n")
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
            "{}\n... ({} more lines)",
            shown.join("\n"),
            lines.len() - 20
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
            "{}\n... ({} more lines)",
            lines[..5].join("\n"),
            lines.len() - 5
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
            "{}\n... ({} more lines)",
            lines[..5].join("\n"),
            lines.len() - 5
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
    use super::*;

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
        assert!(result.contains("FAIL"), "should indicate failure");
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
