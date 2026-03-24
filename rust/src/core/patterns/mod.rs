pub mod cargo;
pub mod curl;
pub mod deps_cmd;
pub mod docker;
pub mod env_filter;
pub mod eslint;
pub mod find;
pub mod gh;
pub mod git;
pub mod golang;
pub mod grep;
pub mod json_schema;
pub mod kubectl;
pub mod log_dedup;
pub mod ls;
pub mod next_build;
pub mod npm;
pub mod pip;
pub mod playwright;
pub mod pnpm;
pub mod prettier;
pub mod ruby;
pub mod ruff;
pub mod test;
pub mod typescript;
pub mod wget;

pub fn compress_output(command: &str, output: &str) -> Option<String> {
    let cmd_lower = command.to_lowercase();

    let specific = try_specific_pattern(&cmd_lower, output);
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

fn try_specific_pattern(cmd_lower: &str, output: &str) -> Option<String> {
    if cmd_lower.starts_with("git ") {
        return git::compress(cmd_lower, output);
    }

    if cmd_lower.starts_with("gh ") {
        return gh::compress(cmd_lower, output);
    }

    if cmd_lower.starts_with("kubectl ") || cmd_lower.starts_with("k ") {
        return kubectl::compress(cmd_lower, output);
    }

    if cmd_lower.starts_with("pnpm ") {
        return pnpm::compress(cmd_lower, output);
    }
    if cmd_lower.starts_with("npm ") || cmd_lower.starts_with("yarn ") {
        return npm::compress(cmd_lower, output);
    }

    if cmd_lower.starts_with("cargo ") {
        return cargo::compress(cmd_lower, output);
    }

    if cmd_lower.starts_with("docker ") || cmd_lower.starts_with("docker-compose ") {
        return docker::compress(cmd_lower, output);
    }

    if cmd_lower.starts_with("pip ") || cmd_lower.starts_with("pip3 ") || cmd_lower.starts_with("python -m pip") {
        return pip::compress(cmd_lower, output);
    }

    if cmd_lower.starts_with("ruff ") {
        return ruff::compress(cmd_lower, output);
    }

    if cmd_lower.starts_with("eslint") || cmd_lower.starts_with("npx eslint") || cmd_lower.starts_with("biome ") || cmd_lower.starts_with("stylelint") {
        return eslint::compress(cmd_lower, output);
    }

    if cmd_lower.starts_with("prettier") || cmd_lower.starts_with("npx prettier") {
        return prettier::compress(output);
    }

    if cmd_lower.starts_with("go ") || cmd_lower.starts_with("golangci-lint") || cmd_lower.starts_with("golint") {
        return golang::compress(cmd_lower, output);
    }

    if cmd_lower.starts_with("playwright") || cmd_lower.starts_with("npx playwright") || cmd_lower.starts_with("cypress") || cmd_lower.starts_with("npx cypress") {
        return playwright::compress(cmd_lower, output);
    }

    if cmd_lower.starts_with("vitest") || cmd_lower.starts_with("npx vitest") || cmd_lower.starts_with("pnpm vitest") {
        return test::compress(output);
    }

    if cmd_lower.starts_with("next ") || cmd_lower.starts_with("npx next") || cmd_lower.starts_with("vite ") || cmd_lower.starts_with("npx vite") {
        return next_build::compress(cmd_lower, output);
    }

    if cmd_lower.starts_with("tsc") || cmd_lower.contains("typescript") {
        return typescript::compress(output);
    }
    if cmd_lower.starts_with("rubocop") || cmd_lower.starts_with("bundle ") || cmd_lower.starts_with("rake ") || cmd_lower.starts_with("rails test") {
        return ruby::compress(cmd_lower, output);
    }
    if cmd_lower.starts_with("grep ") || cmd_lower.starts_with("rg ") {
        return grep::compress(output);
    }
    if cmd_lower.starts_with("find ") {
        return find::compress(output);
    }
    if cmd_lower.starts_with("ls ") || cmd_lower == "ls" {
        return ls::compress(output);
    }
    if cmd_lower.starts_with("curl ") {
        return curl::compress(output);
    }
    if cmd_lower.starts_with("wget ") {
        return wget::compress(output);
    }
    if cmd_lower == "env" || cmd_lower.starts_with("env ") || cmd_lower.starts_with("printenv") {
        return env_filter::compress(output);
    }

    None
}
