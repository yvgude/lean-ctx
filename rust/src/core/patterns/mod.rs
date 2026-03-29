pub mod ansible;
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
pub mod systemd;
pub mod terraform;
pub mod test;
pub mod typescript;
pub mod wget;
pub mod zig;

pub fn compress_output(command: &str, output: &str) -> Option<String> {
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
    if c.starts_with("bun ") {
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
        return curl::compress(output);
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
    if c.starts_with("mix ") || c.starts_with("iex ") {
        return mix::compress(c, output);
    }
    if c.starts_with("bazel ") || c.starts_with("blaze ") {
        return bazel::compress(c, output);
    }
    if c.starts_with("systemctl ") || c.starts_with("journalctl") {
        return systemd::compress(c, output);
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
}
