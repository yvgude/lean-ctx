pub mod ansible;
pub mod artisan;
pub mod aws;
pub mod bazel;
pub mod bun;
pub mod cargo;
pub mod cmake;
pub mod composer;
pub mod curl;
pub mod deno;
pub mod deps_cmd;
pub mod docker;
pub mod dotnet;
pub mod env_filter;
pub mod eslint;
pub mod find;
pub mod flutter;
pub mod gh;
pub mod git;
pub mod golang;
pub mod grep;
pub mod helm;
pub mod json_schema;
pub mod kubectl;
pub mod log_dedup;
pub mod ls;
pub mod make;
pub mod maven;
pub mod mix;
pub mod mypy;
pub mod mysql;
pub mod next_build;
pub mod npm;
pub mod php;
pub mod pip;
pub mod playwright;
pub mod pnpm;
pub mod poetry;
pub mod prettier;
pub mod prisma;
pub mod psql;
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

pub fn compress_output(command: &str, output: &str) -> Option<String> {
    let cleaned = crate::core::compressor::strip_ansi(output);
    let output = if cleaned.len() < output.len() {
        &cleaned
    } else {
        output
    };

    if let Some(engine) = crate::core::filters::FilterEngine::load() {
        if let Some(filtered) = engine.apply(command, output) {
            return Some(filtered);
        }
    }

    let specific = try_specific_pattern(command, output);
    if specific.is_some() {
        return specific;
    }

    if let Some(r) = json_schema::compress(output) {
        return Some(r);
    }

    if let Some(r) = log_dedup::compress(output) {
        return Some(r);
    }

    if let Some(r) = test::compress(output) {
        return Some(r);
    }

    None
}

fn try_specific_pattern(cmd: &str, output: &str) -> Option<String> {
    let cl = cmd.to_ascii_lowercase();
    let c = cl.as_str();

    if c.starts_with("git ") {
        return git::compress(c, output);
    }
    if c.starts_with("gh ") {
        return gh::compress(c, output);
    }
    if c == "terraform" || c.starts_with("terraform ") {
        return terraform::compress(c, output);
    }
    if c == "make" || c.starts_with("make ") {
        return make::compress(c, output);
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
        return test::compress(output);
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
        assert!(compress_output("docker ps", output).is_some());
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
        let output = "1 pass tests\nDone 12ms";
        let compressed = compress_output("bunx test", output).unwrap();
        assert!(compressed.contains("bun test: 1 passed"));
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
}
