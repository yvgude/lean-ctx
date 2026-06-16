/// Pattern engine version. Bump when output format changes to maintain
/// shell-output determinism guarantees. Consumers (proofs, benchmarks)
/// can embed this version to detect pattern-breaking updates.
pub const PATTERN_ENGINE_VERSION: u32 = 1;

pub mod pattern_trait;
pub use pattern_trait::{CompressionPattern, CompressionResult};

pub mod ansible;
pub mod artisan;
pub mod aws;
pub mod bazel;
pub mod bun;
pub mod cargo;
pub mod clang;
pub mod cmake;
pub mod composer;
pub mod curl;
pub mod deno;
pub mod deps_cmd;
pub mod docker;
pub mod dotnet;
pub mod env_filter;
pub mod eslint;
pub mod fd;
pub mod find;
pub mod flutter;
pub mod gh;
pub mod git;
pub mod glab;
pub mod golang;
pub mod grep;
pub mod helm;
pub mod json_schema;
pub mod just;
pub mod kubectl;
pub mod log_dedup;
pub mod ls;
pub mod make;
pub mod maven;
pub mod mix;
pub mod mypy;
pub mod mysql;
pub mod next_build;
pub mod ninja;
pub mod npm;
pub mod php;
pub mod pip;
pub mod playwright;
pub mod pnpm;
pub mod poetry;
pub mod prettier;
pub mod prisma;
pub mod psql;
pub mod pytest;
pub mod ruby;
pub mod ruff;
pub mod swift;
pub mod sysinfo;
pub mod systemd;
pub mod terraform;
pub mod test;
pub mod typescript;
pub mod wget;
pub mod zig;

use crate::core::tokens::count_tokens;

pub fn compress_output(command: &str, output: &str) -> Option<String> {
    // Policy gate: protected commands (Passthrough + Verbatim) must
    // never be pattern-compressed. The caller (compress_if_beneficial
    // or ctx_shell::handle) should have already checked, but we
    // defend in depth here.
    let policy = crate::shell::output_policy::classify(command, &[]);
    if policy.is_protected() {
        return None;
    }

    let stripped;
    let clean_output = {
        let s = crate::core::compressor::strip_ansi(output);
        if s.len() < output.len() {
            stripped = s;
            &stripped
        } else {
            output
        }
    };

    if let Some(engine) = crate::core::filters::FilterEngine::load()
        && let Some(filtered) = engine.apply(command, clean_output)
    {
        return shorter_only(filtered, output);
    }

    if let Some(compressed) = try_specific_pattern(command, clean_output)
        && let Some(r) = shorter_only(compressed, output)
    {
        return Some(r);
    }

    if let Some(r) = json_schema::compress(clean_output)
        && let Some(r) = shorter_only(r, output)
    {
        return Some(r);
    }

    if let Some(r) = log_dedup::compress(clean_output)
        && let Some(r) = shorter_only(r, output)
    {
        return Some(r);
    }

    if let Some(r) = test::compress(clean_output)
        && let Some(r) = shorter_only(r, output)
    {
        return Some(r);
    }

    None
}

/// Collapse whitespace into single spaces so comparisons align with logical word tokens.
fn normalize_shell_tokens(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn shorter_only(compressed: String, original: &str) -> Option<String> {
    let orig_n = normalize_shell_tokens(original);
    let comp_n = normalize_shell_tokens(&compressed);
    let ct_c = count_tokens(&comp_n);
    let ct_o = count_tokens(&orig_n);
    if ct_c < ct_o || (ct_c == ct_o && comp_n.len() < orig_n.len()) {
        Some(compressed)
    } else {
        None
    }
}

pub fn try_specific_pattern(cmd: &str, output: &str) -> Option<String> {
    let cl = cmd.to_ascii_lowercase();
    let c = cl.as_str();

    if c.starts_with("git ") {
        return git::compress(c, output);
    }
    if c.starts_with("gh ") {
        return gh::compress(c, output);
    }
    if c.starts_with("glab ") {
        return glab::try_glab_pattern(c, output);
    }
    if c == "terraform" || c.starts_with("terraform ") {
        return terraform::compress(c, output);
    }
    if c == "make" || c.starts_with("make ") {
        return make::compress(c, output);
    }
    if c == "just" || c.starts_with("just ") {
        return just::compress(c, output);
    }
    if c.starts_with("mvn ")
        || c.starts_with("./mvnw ")
        || c.starts_with("mvnw ")
        || c.starts_with("gradle ")
        || c.starts_with("./gradlew ")
        || c.starts_with("gradlew ")
    {
        return maven::compress(c, output);
    }
    if c.starts_with("kubectl ") || c.starts_with("k ") {
        return kubectl::compress(c, output);
    }
    if c.starts_with("helm ") {
        return helm::compress(c, output);
    }
    if c.starts_with("pnpm ") {
        return pnpm::compress(c, output);
    }
    if c.starts_with("bun ") || c.starts_with("bunx ") {
        return bun::compress(c, output);
    }
    if c.starts_with("deno ") {
        return deno::compress(c, output);
    }
    if c.starts_with("npm ") || c.starts_with("yarn ") {
        return npm::compress(c, output);
    }
    if c.starts_with("cargo ") {
        return cargo::compress(c, output);
    }
    if c.starts_with("docker ") || c.starts_with("docker-compose ") {
        return docker::compress(c, output);
    }
    if c.starts_with("pip ") || c.starts_with("pip3 ") || c.starts_with("python -m pip") {
        return pip::compress(c, output);
    }
    if c.starts_with("mypy") || c.starts_with("python -m mypy") || c.starts_with("dmypy ") {
        return mypy::compress(c, output);
    }
    if c.starts_with("pytest") || c.starts_with("python -m pytest") {
        return pytest::compress(c, output).or_else(|| test::compress(output));
    }
    if c.starts_with("ruff ") {
        return ruff::compress(c, output);
    }
    if c.starts_with("eslint")
        || c.starts_with("npx eslint")
        || c.starts_with("biome ")
        || c.starts_with("stylelint")
    {
        return eslint::compress(c, output);
    }
    if c.starts_with("prettier") || c.starts_with("npx prettier") {
        return prettier::compress(output);
    }
    if c.starts_with("go ") || c.starts_with("golangci-lint") || c.starts_with("golint") {
        return golang::compress(c, output);
    }
    if c.starts_with("playwright")
        || c.starts_with("npx playwright")
        || c.starts_with("cypress")
        || c.starts_with("npx cypress")
    {
        return playwright::compress(c, output);
    }
    if c.starts_with("vitest") || c.starts_with("npx vitest") || c.starts_with("pnpm vitest") {
        return test::compress(output);
    }
    if c.starts_with("next ")
        || c.starts_with("npx next")
        || c.starts_with("vite ")
        || c.starts_with("npx vite")
        || c.starts_with("vp ")
        || c.starts_with("vite-plus ")
    {
        return next_build::compress(c, output);
    }
    if c.starts_with("tsc") || c.contains("typescript") {
        return typescript::compress(output);
    }
    if c.starts_with("rubocop")
        || c.starts_with("bundle ")
        || c.starts_with("rake ")
        || c.starts_with("rails test")
        || c.starts_with("rspec")
    {
        return ruby::compress(c, output);
    }
    if c.starts_with("grep ") || c.starts_with("rg ") {
        return grep::compress(output);
    }
    if c.starts_with("find ") {
        return find::compress(output);
    }
    if c.starts_with("fd ") || c.starts_with("fdfind ") {
        return fd::compress(output);
    }
    if c.starts_with("ls ") || c == "ls" {
        return ls::compress(output);
    }
    if c.starts_with("curl ") {
        return curl::compress_with_cmd(c, output);
    }
    if c.starts_with("wget ") {
        return wget::compress(output);
    }
    if c == "env" || c.starts_with("env ") || c.starts_with("printenv") {
        return env_filter::compress(output);
    }
    if c.starts_with("dotnet ") {
        return dotnet::compress(c, output);
    }
    if c.starts_with("flutter ")
        || (c.starts_with("dart ") && (c.contains(" analyze") || c.ends_with(" analyze")))
    {
        return flutter::compress(c, output);
    }
    if c.starts_with("poetry ")
        || c.starts_with("uv sync")
        || (c.starts_with("uv ") && c.contains("pip install"))
        || c.starts_with("conda ")
        || c.starts_with("mamba ")
        || c.starts_with("pipx ")
    {
        return poetry::compress(c, output);
    }
    if c.starts_with("aws ") {
        return aws::compress(c, output);
    }
    if c.starts_with("psql ") || c.starts_with("pg_") {
        return psql::compress(c, output);
    }
    if c.starts_with("mysql ") || c.starts_with("mariadb ") {
        return mysql::compress(c, output);
    }
    if c.starts_with("prisma ") || c.starts_with("npx prisma") {
        return prisma::compress(c, output);
    }
    if c.starts_with("swift ") {
        return swift::compress(c, output);
    }
    if c.starts_with("zig ") {
        return zig::compress(c, output);
    }
    if c.starts_with("cmake ") || c.starts_with("ctest") {
        return cmake::compress(c, output);
    }
    if c.starts_with("ninja") {
        return ninja::compress(c, output);
    }
    if c.starts_with("ansible") || c.starts_with("ansible-playbook") {
        return ansible::compress(c, output);
    }
    if c.starts_with("composer ") {
        return composer::compress(c, output);
    }
    if c.starts_with("php artisan") || c.starts_with("artisan ") {
        return artisan::compress(c, output);
    }
    if c.starts_with("./vendor/bin/pest") || c.starts_with("pest ") {
        return artisan::compress("php artisan test", output);
    }
    if c.starts_with("mix ") || c.starts_with("iex ") {
        return mix::compress(c, output);
    }
    if c.starts_with("bazel ") || c.starts_with("blaze ") {
        return bazel::compress(c, output);
    }
    if c.starts_with("systemctl ") || c.starts_with("journalctl") {
        return systemd::compress(c, output);
    }
    if c.starts_with("jest") || c.starts_with("npx jest") || c.starts_with("pnpm jest") {
        return test::compress(output);
    }
    if c.starts_with("mocha") || c.starts_with("npx mocha") {
        return test::compress(output);
    }
    if c.starts_with("tofu ") {
        return terraform::compress(c, output);
    }
    if c.starts_with("ps ") || c == "ps" {
        return sysinfo::compress_ps(output);
    }
    if c.starts_with("df ") || c == "df" {
        return sysinfo::compress_df(output);
    }
    if c.starts_with("du ") || c == "du" {
        return sysinfo::compress_du(output);
    }
    if c.starts_with("ping ") {
        return sysinfo::compress_ping(output);
    }
    if c.starts_with("jq ") || c == "jq" {
        return json_schema::compress(output);
    }
    if c.starts_with("hadolint") {
        return eslint::compress(c, output);
    }
    if c.starts_with("yamllint") || c.starts_with("npx yamllint") {
        return eslint::compress(c, output);
    }
    if c.starts_with("markdownlint") || c.starts_with("npx markdownlint") {
        return eslint::compress(c, output);
    }
    if c.starts_with("oxlint") || c.starts_with("npx oxlint") {
        return eslint::compress(c, output);
    }
    if c.starts_with("pyright") || c.starts_with("basedpyright") {
        return mypy::compress(c, output);
    }
    if c.starts_with("turbo ") || c.starts_with("npx turbo") {
        return npm::compress(c, output);
    }
    if c.starts_with("nx ") || c.starts_with("npx nx") {
        return npm::compress(c, output);
    }
    if c.starts_with("clang++ ") || c.starts_with("clang ") {
        return clang::compress(c, output);
    }
    if c.starts_with("gcc ")
        || c.starts_with("g++ ")
        || c.starts_with("cc ")
        || c.starts_with("c++ ")
    {
        return cmake::compress(c, output);
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn routes_git_commands() {
        let output = "On branch main\nnothing to commit";
        assert!(compress_output("git status", output).is_some());
    }

    #[test]
    fn routes_cargo_commands() {
        let output = "   Compiling lean-ctx v2.1.1\n    Finished `release` profile [optimized] target(s) in 30.5s";
        assert!(compress_output("cargo build --release", output).is_some());
    }

    #[test]
    fn routes_npm_commands() {
        let output = "added 150 packages, and audited 151 packages in 5s\n\n25 packages are looking for funding\n  run `npm fund` for details\n\nfound 0 vulnerabilities";
        assert!(compress_output("npm install", output).is_some());
    }

    #[test]
    fn routes_docker_commands() {
        let output = "CONTAINER ID   IMAGE     COMMAND   CREATED   STATUS    PORTS     NAMES";
        // docker ps is Verbatim (via is_container_listing), so compress_output
        // correctly returns None (policy gate). docker build should still compress.
        assert!(compress_output("docker ps", output).is_none());
        let build_output =
            "Step 1/5 : FROM node:18\n ---> abc123\nStep 2/5 : COPY . .\nSuccessfully built def456";
        assert!(compress_output("docker build .", build_output).is_some());
    }

    #[test]
    fn routes_mypy_commands() {
        let output = "src/main.py:10: error: Missing return  [return]\nFound 1 error in 1 file (checked 3 source files)";
        assert!(compress_output("mypy .", output).is_some());
        assert!(compress_output("python -m mypy src/", output).is_some());
    }

    #[test]
    fn routes_pytest_commands() {
        let output = "===== test session starts =====\ncollected 5 items\ntest_main.py ..... [100%]\n===== 5 passed in 0.5s =====";
        assert!(compress_output("pytest", output).is_some());
        assert!(compress_output("python -m pytest tests/", output).is_some());
    }

    #[test]
    fn unknown_command_returns_none() {
        assert!(compress_output("some-unknown-tool --version", "v1.0").is_none());
    }

    #[test]
    fn case_insensitive_routing() {
        let output = "On branch main\nnothing to commit";
        assert!(compress_output("Git Status", output).is_some());
        assert!(compress_output("GIT STATUS", output).is_some());
    }

    #[test]
    fn routes_vp_and_vite_plus() {
        let output = "  VITE v5.0.0  ready in 200 ms\n\n  -> Local:   http://localhost:5173/\n  -> Network: http://192.168.1.2:5173/";
        assert!(compress_output("vp build", output).is_some());
        assert!(compress_output("vite-plus build", output).is_some());
    }

    #[test]
    fn routes_bunx_commands() {
        let output = "1 pass tests\n0 fail tests\n3 skip tests\nDone 12ms\nsome extra line\nmore output here";
        let result = compress_output("bunx test", output);
        assert!(
            result.is_some(),
            "bunx should compress when output is large enough"
        );
        assert!(result.unwrap().contains("bun test: 1 passed"));
    }

    #[test]
    fn routes_deno_task() {
        let output = "Task dev deno run --allow-net server.ts\nListening on http://localhost:8000";
        assert!(try_specific_pattern("deno task dev", output).is_some());
    }

    #[test]
    fn routes_jest_commands() {
        let output = "PASS  tests/main.test.js\nTest Suites: 1 passed, 1 total\nTests:       5 passed, 5 total\nTime:        2.5 s";
        assert!(try_specific_pattern("jest", output).is_some());
        assert!(try_specific_pattern("npx jest --coverage", output).is_some());
    }

    #[test]
    fn routes_mocha_commands() {
        let output = "  3 passing (50ms)\n  1 failing\n\n  1) Array #indexOf():\n     Error: expected -1 to equal 0";
        assert!(try_specific_pattern("mocha", output).is_some());
        assert!(try_specific_pattern("npx mocha tests/", output).is_some());
    }

    #[test]
    fn routes_tofu_commands() {
        let output = "Initializing the backend...\nInitializing provider plugins...\nTerraform has been successfully initialized!";
        assert!(try_specific_pattern("tofu init", output).is_some());
    }

    #[test]
    fn routes_ps_commands() {
        let mut lines = vec!["USER PID %CPU %MEM VSZ RSS TTY STAT START TIME COMMAND".to_string()];
        for i in 0..20 {
            lines.push(format!("user {i} 0.0 0.1 1234 123 ? S 10:00 0:00 proc_{i}"));
        }
        let output = lines.join("\n");
        assert!(try_specific_pattern("ps aux", &output).is_some());
    }

    #[test]
    fn routes_ping_commands() {
        let output = "PING google.com (1.2.3.4): 56 data bytes\n64 bytes from 1.2.3.4: icmp_seq=0 ttl=116 time=12ms\n3 packets transmitted, 3 packets received, 0.0% packet loss\nrtt min/avg/max/stddev = 11/12/13/1 ms";
        assert!(try_specific_pattern("ping -c 3 google.com", output).is_some());
    }

    #[test]
    fn routes_jq_to_json_schema() {
        let output = "{\"name\": \"test\", \"version\": \"1.0\", \"items\": [{\"id\": 1}, {\"id\": 2}, {\"id\": 3}, {\"id\": 4}, {\"id\": 5}, {\"id\": 6}, {\"id\": 7}, {\"id\": 8}, {\"id\": 9}, {\"id\": 10}]}";
        assert!(try_specific_pattern("jq '.items' data.json", output).is_some());
    }

    #[test]
    fn routes_linting_tools() {
        let lint_output = "src/main.py:10: error: Missing return\nsrc/main.py:20: error: Unused var\nFound 2 errors";
        assert!(try_specific_pattern("hadolint Dockerfile", lint_output).is_some());
        assert!(try_specific_pattern("oxlint src/", lint_output).is_some());
        assert!(try_specific_pattern("pyright src/", lint_output).is_some());
        assert!(try_specific_pattern("basedpyright src/", lint_output).is_some());
    }

    #[test]
    fn routes_fd_commands() {
        let output = "src/main.rs\nsrc/lib.rs\nsrc/util/helpers.rs\nsrc/util/math.rs\ntests/integration.rs\n";
        assert!(try_specific_pattern("fd --extension rs", output).is_some());
        assert!(try_specific_pattern("fdfind .rs", output).is_some());
    }

    #[test]
    fn routes_just_commands() {
        let output = "Available recipes:\n    build\n    test\n    lint\n";
        assert!(try_specific_pattern("just --list", output).is_some());
        assert!(try_specific_pattern("just build", output).is_some());
    }

    #[test]
    fn routes_ninja_commands() {
        let output = "[1/10] Compiling foo.c\n[10/10] Linking app\n";
        assert!(try_specific_pattern("ninja", output).is_some());
        assert!(try_specific_pattern("ninja -j4", output).is_some());
    }

    #[test]
    fn routes_clang_commands() {
        let output =
            "src/main.c:10:5: error: use of undeclared identifier 'foo'\n1 error generated.\n";
        assert!(try_specific_pattern("clang src/main.c", output).is_some());
        assert!(try_specific_pattern("clang++ -std=c++17 main.cpp", output).is_some());
    }

    #[test]
    fn routes_cargo_run() {
        let output = "   Compiling foo v0.1.0\n    Finished `dev` profile\nHello, world!";
        assert!(try_specific_pattern("cargo run", output).is_some());
    }

    #[test]
    fn routes_cargo_bench() {
        let output = "   Compiling foo v0.1.0\ntest bench_parse ... bench: 1234 ns/iter";
        assert!(try_specific_pattern("cargo bench", output).is_some());
    }

    #[test]
    fn routes_build_tools() {
        let build_output = "   Compiling foo v0.1.0\n    Finished release [optimized]";
        assert!(try_specific_pattern("gcc -o main main.c", build_output).is_some());
        assert!(try_specific_pattern("g++ -o main main.cpp", build_output).is_some());
    }

    #[test]
    fn routes_monorepo_tools() {
        let output = "npm warn deprecated inflight@1.0.6\nnpm warn deprecated rimraf@3.0.2\nadded 150 packages, and audited 151 packages in 5s\n\n25 packages are looking for funding\n  run `npm fund` for details\n\nfound 0 vulnerabilities";
        assert!(try_specific_pattern("turbo install", output).is_some());
        assert!(try_specific_pattern("nx install", output).is_some());
    }

    #[test]
    fn gh_api_passthrough_never_compresses() {
        let huge = "line\n".repeat(5000);
        assert!(
            compress_output("gh api repos/owner/repo/actions/jobs/123/logs", &huge).is_none(),
            "gh api must never be compressed, even for large output"
        );
        assert!(compress_output("gh api repos/owner/repo/actions/runs/123/logs", &huge).is_none());
    }

    #[test]
    fn gh_log_flags_passthrough() {
        let huge = "line\n".repeat(5000);
        assert!(compress_output("gh run view 123 --log-failed", &huge).is_none());
        assert!(compress_output("gh run view 123 --log", &huge).is_none());
    }

    #[test]
    fn gh_structured_commands_still_compress() {
        let output = "On branch main\nnothing to commit";
        assert!(try_specific_pattern("gh pr list", output).is_some());
        assert!(try_specific_pattern("gh run list", output).is_some());
    }
}
