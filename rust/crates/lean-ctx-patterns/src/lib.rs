// SPDX-License-Identifier: Apache-2.0

//! Deterministic shell-output compression patterns for the LeanCTX Engine.
//!
//! Every pattern is a pure text transform: command line plus output in,
//! compressed output (or `None`) out. Engine policy (protected commands,
//! user filters, configuration) stays in the engine, which reaches this crate
//! through its `core::patterns` facade. Engine-supplied facts enter through
//! [`PatternEnv`], so this crate never depends on the engine.

/// Pattern engine version. Bump when output format changes to maintain
/// shell-output determinism guarantees. Consumers (proofs, benchmarks)
/// can embed this version to detect pattern-breaking updates.
///
/// v2 (#936): `json_schema::compress` now prefers the lossless `json_crush`
/// form over the schema outline for redundant array-of-object payloads.
pub const PATTERN_ENGINE_VERSION: u32 = 2;

pub mod ansi;
pub mod json_crush;
pub mod safety_needles;
pub mod tokens;

pub mod alembic;
pub mod ansible;
pub mod argocd;
pub mod artisan;
pub mod aws;
pub mod bazel;
pub mod buf;
pub mod bun;
pub mod cargo;
pub mod clang;
pub mod cmake;
pub mod composer;
pub mod cosign;
pub mod curl;
pub mod dbt;
pub mod deno;
pub mod deploy;
pub mod deps_cmd;
pub mod docker;
pub mod dotnet;
pub mod emspace;
pub mod env_filter;
pub mod eslint;
pub mod fd;
pub mod find;
pub mod flutter;
pub mod flyway;
pub mod gem;
pub mod gh;
pub mod git;
pub mod glab;
pub mod golang;
pub mod grep;
pub mod grype;
pub mod helm;
pub mod jj;
pub mod json_schema;
pub mod just;
pub mod kubectl;
pub mod linkerd;
pub mod log_dedup;
pub mod lossless_collapse;
pub mod ls;
pub mod make;
pub mod maven;
pub mod mise;
pub mod mix;
pub mod mlflow;
pub mod mypy;
pub mod mysql;
pub mod next_build;
pub mod ninja;
pub mod npm;
pub mod ollama;
pub mod php;
pub mod pip;
pub mod playwright;
pub mod pnpm;
pub mod poetry;
pub mod prettier;
pub mod prisma;
pub mod psql;
pub mod pulumi;
pub mod pytest;
pub mod ruby;
pub mod ruff;
pub mod semgrep;
pub mod spark;
pub mod swift;
pub mod swiftlint;
pub mod syft;
pub mod sysinfo;
pub mod systemd;
pub mod template_collapse;
pub mod terraform;
pub mod test;
pub mod trivy;
pub mod typescript;
pub mod wget;
pub mod zig;

use crate::tokens::count_tokens;

/// Engine-supplied facts a pattern may need. The engine implements this from
/// its configuration. [`NoEnv`] is the neutral environment for callers without
/// one (tests, tools that run outside an engine).
pub trait PatternEnv {
    /// True when `command` (lower-cased) fetches a configured passthrough URL;
    /// the curl pattern then leaves the response to the generic fallbacks.
    fn is_curl_passthrough(&self, command: &str) -> bool;
}

/// The neutral [`PatternEnv`]: no passthrough URLs.
pub struct NoEnv;

impl PatternEnv for NoEnv {
    fn is_curl_passthrough(&self, _command: &str) -> bool {
        false
    }
}

/// The policy-free part of the engine's `compress_output`: everything after the
/// protected-command gate, the leading-`cd` unwrap, ANSI stripping and the
/// user filter rules, which the engine applies first. `clean_output` is the
/// ANSI-stripped output, `output` the raw one every result is measured against.
pub fn compress_clean(
    env: &dyn PatternEnv,
    command: &str,
    clean_output: &str,
    output: &str,
) -> Option<String> {
    // VCS history (git/jj/gh/glab/hg) is owned by its dedicated compressor. Its
    // lines look log-ish (one per commit) but are NOT application logs, so the
    // generic json/log/test fallbacks would mis-summarize them — e.g. truncating
    // an explicit `git log --oneline -40` to "last 15" or reading a commit
    // subject like "fix: pending_errors" as an error line. Return the dedicated
    // compressor's result directly — even when it is not shorter, so an already
    // compact oneline log is preserved verbatim (full history intact) instead of
    // being reshaped by a generic heuristic.
    if has_vcs_owner(command) {
        // The VCS compressor is authoritative and is kept even when it is not
        // strictly *shorter*, so a compact `git log --oneline` is preserved
        // verbatim (full history intact). It must still never return *more*
        // tokens than its input, though — a tiny adversarial `git status` body
        // could otherwise reshape into a one-token-larger summary and break the
        // never-inflate invariant. Allow equal (verbatim), reject only growth.
        return try_specific_pattern_with(env, command, clean_output)
            .filter(|c| !c.trim().is_empty())
            .filter(|c| !inflates_tokens(c, output));
    }

    if let Some(compressed) = try_specific_pattern_with(env, command, clean_output)
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

/// True for version-control commands whose output is authoritative under their
/// own compressor and must not be reinterpreted by the generic log/test
/// fallbacks (commit history is not application log output). emspace is owned
/// the same way: its page bodies are `page edit` anchors and must stay exact.
pub fn has_vcs_owner(command: &str) -> bool {
    let c = command.trim_start().to_ascii_lowercase();
    c.starts_with("git ")
        || c.starts_with("jj ")
        || c.starts_with("gh ")
        || c.starts_with("glab ")
        || c.starts_with("hg ")
        || c.starts_with("emspace ")
}

/// Collapse whitespace into single spaces so comparisons align with logical word tokens.
fn normalize_shell_tokens(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// True when `compressed` carries *more* tokens than the raw `original` — i.e.
/// the transform inflated rather than compressed. Compared against the raw
/// (pre-ANSI-strip) output so the guard matches the public `compress_output`
/// invariant exactly: the returned text never tokenizes larger than its input.
pub fn inflates_tokens(compressed: &str, original: &str) -> bool {
    count_tokens(compressed) > count_tokens(original)
}

/// `Some(compressed)` only when it carries fewer tokens than `original`
/// (ties broken by length), compared on whitespace-normalized text.
pub fn shorter_only(compressed: String, original: &str) -> Option<String> {
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

/// How an elided block announces itself (GH #1679).
///
/// The old marker was `... (N more lines)`, which reads as content: a caller
/// could not tell it from a line the command actually printed. Worse, it was the
/// *only* trace of the cut — the savings footer that would have announced it is
/// hidden on the MCP path by default (`SavingsFooter::Auto` → invisible in an
/// MCP context), which is the primary path. So a lossy result was
/// indistinguishable from a complete one, with nothing to recover from.
///
/// This marker is unmistakably ours, and names the way back. `raw=true` is what
/// the server instructions and every other compression annotation already point
/// at, and unlike an archive handle it is always available — it does not depend
/// on the response having been archived or on a hint tier being enabled.
pub fn elision_marker(omitted: usize) -> String {
    let plural = if omitted == 1 { "line" } else { "lines" };
    format!("[lean-ctx: {omitted} {plural} elided — re-run with raw=true for the full output]")
}

type PatternMatcher = fn(&str) -> bool;
type PatternHandler = fn(&str, &str) -> Option<String>;

/// Ordered prefix → compressor dispatch table (#660 CC reduction): first
/// matching entry wins, mirroring the previous `if c.starts_with(..) { return
/// ...; }` cascade exactly — including that a matched entry's `None` is
/// final, never falls through to a later entry. Registering a new CLI tool
/// is now a one-line addition here instead of a new branch in
/// `try_specific_pattern`.
/// Number of registered shell-output patterns, for the docs drift check (#1896).
pub fn pattern_count() -> usize {
    PATTERNS.len()
}

const PATTERNS: &[(PatternMatcher, PatternHandler)] = &[
    (
        |c| c.starts_with("git "),
        |c, output| git::compress(c, output),
    ),
    (
        |c| c.starts_with("gh "),
        |c, output| gh::compress(c, output),
    ),
    (
        |c| c.starts_with("glab "),
        |c, output| glab::try_glab_pattern(c, output),
    ),
    (
        |c| c.starts_with("emspace "),
        |c, output| emspace::compress(c, output),
    ),
    (
        |c| c == "terraform" || c.starts_with("terraform "),
        |c, output| terraform::compress(c, output),
    ),
    (
        |c| c == "make" || c.starts_with("make "),
        |c, output| make::compress(c, output),
    ),
    (
        |c| c == "just" || c.starts_with("just "),
        |c, output| just::compress(c, output),
    ),
    (
        |c| {
            c.starts_with("mvn ")
                || c.starts_with("./mvnw ")
                || c.starts_with("mvnw ")
                || c.starts_with("gradle ")
                || c.starts_with("./gradlew ")
                || c.starts_with("gradlew ")
        },
        |c, output| maven::compress(c, output),
    ),
    (
        |c| c.starts_with("kubectl ") || c.starts_with("k "),
        |c, output| kubectl::compress(c, output),
    ),
    (
        |c| c.starts_with("helm "),
        |c, output| helm::compress(c, output),
    ),
    (
        |c| c.starts_with("pnpm "),
        |c, output| pnpm::compress(c, output),
    ),
    (
        |c| c.starts_with("bun ") || c.starts_with("bunx "),
        |c, output| bun::compress(c, output),
    ),
    (
        |c| c.starts_with("deno "),
        |c, output| deno::compress(c, output),
    ),
    (
        |c| c.starts_with("npm ") || c.starts_with("yarn "),
        |c, output| npm::compress(c, output),
    ),
    (
        |c| c.starts_with("cargo "),
        |c, output| cargo::compress(c, output),
    ),
    (
        |c| c.starts_with("docker ") || c.starts_with("docker-compose "),
        |c, output| docker::compress(c, output),
    ),
    (
        |c| c.starts_with("pip ") || c.starts_with("pip3 ") || c.starts_with("python -m pip"),
        |c, output| pip::compress(c, output),
    ),
    (
        |c| c.starts_with("mypy") || c.starts_with("python -m mypy") || c.starts_with("dmypy "),
        |c, output| mypy::compress(c, output),
    ),
    (
        |c| c.starts_with("pytest") || c.starts_with("python -m pytest"),
        |c, output| pytest::compress(c, output).or_else(|| test::compress(output)),
    ),
    (
        |c| c.starts_with("ruff "),
        |c, output| ruff::compress(c, output),
    ),
    (
        |c| {
            c.starts_with("eslint")
                || c.starts_with("npx eslint")
                || c.starts_with("biome ")
                || c.starts_with("stylelint")
        },
        |c, output| eslint::compress(c, output),
    ),
    (
        |c| c.starts_with("prettier") || c.starts_with("npx prettier"),
        |_c, output| prettier::compress(output),
    ),
    (
        |c| c.starts_with("go ") || c.starts_with("golangci-lint") || c.starts_with("golint"),
        |c, output| golang::compress(c, output),
    ),
    (
        |c| {
            c.starts_with("playwright")
                || c.starts_with("npx playwright")
                || c.starts_with("cypress")
                || c.starts_with("npx cypress")
        },
        |c, output| playwright::compress(c, output),
    ),
    (
        |c| c.starts_with("vitest") || c.starts_with("npx vitest") || c.starts_with("pnpm vitest"),
        |_c, output| test::compress(output),
    ),
    (
        |c| {
            c.starts_with("next ")
                || c.starts_with("npx next")
                || c.starts_with("vite ")
                || c.starts_with("npx vite")
                || c.starts_with("vp ")
                || c.starts_with("vite-plus ")
        },
        |c, output| next_build::compress(c, output),
    ),
    (
        |c| c.starts_with("tsc") || c.contains("typescript"),
        |_c, output| typescript::compress(output),
    ),
    (
        |c| {
            c.starts_with("rubocop")
                || c.starts_with("bundle ")
                || c.starts_with("rake ")
                || c.starts_with("rails test")
                || c.starts_with("rspec")
        },
        |c, output| ruby::compress(c, output),
    ),
    (|c| is_grep_family(c), |_c, output| grep::compress(output)),
    (
        |c| c.starts_with("find "),
        |_c, output| find::compress(output),
    ),
    (
        |c| c.starts_with("fd ") || c.starts_with("fdfind "),
        |_c, output| fd::compress(output),
    ),
    (
        |c| c.starts_with("ls ") || c == "ls",
        |_c, output| ls::compress(output),
    ),
    (
        |c| c.starts_with("curl "),
        |_c, output| curl::compress(output),
    ),
    (
        |c| c.starts_with("wget "),
        |_c, output| wget::compress(output),
    ),
    (
        |c| c == "env" || c.starts_with("env ") || c.starts_with("printenv"),
        |_c, output| env_filter::compress(output),
    ),
    (
        |c| c.starts_with("dotnet "),
        |c, output| dotnet::compress(c, output),
    ),
    (
        |c| {
            c.starts_with("flutter ")
                || (c.starts_with("dart ") && (c.contains(" analyze") || c.ends_with(" analyze")))
        },
        |c, output| flutter::compress(c, output),
    ),
    (
        |c| {
            c.starts_with("poetry ")
                || c.starts_with("uv ")
                || c.starts_with("conda ")
                || c.starts_with("mamba ")
                || c.starts_with("pipx ")
        },
        |c, output| poetry::compress(c, output),
    ),
    (
        |c| c.starts_with("aws "),
        |c, output| aws::compress(c, output),
    ),
    (
        |c| c.starts_with("psql ") || c.starts_with("pg_"),
        |c, output| psql::compress(c, output),
    ),
    (
        |c| c.starts_with("mysql ") || c.starts_with("mariadb "),
        |c, output| mysql::compress(c, output),
    ),
    (
        |c| c.starts_with("prisma ") || c.starts_with("npx prisma"),
        |c, output| prisma::compress(c, output),
    ),
    (
        |c| c.starts_with("swift "),
        |c, output| swift::compress(c, output),
    ),
    (
        |c| c.starts_with("zig "),
        |c, output| zig::compress(c, output),
    ),
    (
        |c| c.starts_with("cmake ") || c.starts_with("ctest"),
        |c, output| cmake::compress(c, output),
    ),
    (
        |c| c.starts_with("ninja"),
        |c, output| ninja::compress(c, output),
    ),
    (
        |c| c.starts_with("ansible") || c.starts_with("ansible-playbook"),
        |c, output| ansible::compress(c, output),
    ),
    (
        |c| c.starts_with("composer "),
        |c, output| composer::compress(c, output),
    ),
    (
        |c| c.starts_with("php artisan") || c.starts_with("artisan "),
        |c, output| artisan::compress(c, output),
    ),
    (
        |c| c.starts_with("./vendor/bin/pest") || c.starts_with("pest "),
        |_c, output| artisan::compress("php artisan test", output),
    ),
    (
        |c| c.starts_with("mix ") || c.starts_with("iex "),
        |c, output| mix::compress(c, output),
    ),
    (
        |c| c.starts_with("bazel ") || c.starts_with("blaze "),
        |c, output| bazel::compress(c, output),
    ),
    (
        |c| c.starts_with("systemctl ") || c.starts_with("journalctl"),
        |c, output| systemd::compress(c, output),
    ),
    (
        |c| c.starts_with("jest") || c.starts_with("npx jest") || c.starts_with("pnpm jest"),
        |_c, output| test::compress(output),
    ),
    (
        |c| c.starts_with("mocha") || c.starts_with("npx mocha"),
        |_c, output| test::compress(output),
    ),
    (
        |c| c.starts_with("tofu "),
        |c, output| terraform::compress(c, output),
    ),
    (
        |c| c.starts_with("ps ") || c == "ps",
        |_c, output| sysinfo::compress_ps(output),
    ),
    (
        |c| c.starts_with("df ") || c == "df",
        |_c, output| sysinfo::compress_df(output),
    ),
    (
        |c| c.starts_with("du ") || c == "du",
        |_c, output| sysinfo::compress_du(output),
    ),
    (
        |c| c.starts_with("ping "),
        |_c, output| sysinfo::compress_ping(output),
    ),
    (
        |c| c.starts_with("jq ") || c == "jq",
        |_c, output| json_schema::compress(output),
    ),
    (
        |c| c.starts_with("hadolint"),
        |c, output| eslint::compress(c, output),
    ),
    (
        |c| c.starts_with("yamllint") || c.starts_with("npx yamllint"),
        |c, output| eslint::compress(c, output),
    ),
    (
        |c| c.starts_with("markdownlint") || c.starts_with("npx markdownlint"),
        |c, output| eslint::compress(c, output),
    ),
    (
        |c| c.starts_with("oxlint") || c.starts_with("npx oxlint"),
        |c, output| eslint::compress(c, output),
    ),
    (
        |c| c.starts_with("pyright") || c.starts_with("basedpyright"),
        |c, output| mypy::compress(c, output),
    ),
    (
        |c| c.starts_with("turbo ") || c.starts_with("npx turbo"),
        |c, output| npm::compress(c, output),
    ),
    (
        |c| c.starts_with("nx ") || c.starts_with("npx nx"),
        |c, output| npm::compress(c, output),
    ),
    (
        |c| c.starts_with("clang++ ") || c.starts_with("clang "),
        |c, output| clang::compress(c, output),
    ),
    (
        |c| {
            c.starts_with("gcc ")
                || c.starts_with("g++ ")
                || c.starts_with("cc ")
                || c.starts_with("c++ ")
        },
        |c, output| cmake::compress(c, output),
    ),
    // --- data domain (#657) ---
    (
        |c| c == "dbt" || c.starts_with("dbt "),
        |c, output| dbt::compress(c, output),
    ),
    (
        |c| c == "alembic" || c.starts_with("alembic "),
        |c, output| alembic::compress(c, output),
    ),
    (
        |c| c == "flyway" || c.starts_with("flyway "),
        |c, output| flyway::compress(c, output),
    ),
    (
        |c| c.starts_with("spark-submit") || c.starts_with("spark-sql") || c.starts_with("pyspark"),
        |c, output| spark::compress(c, output),
    ),
    // --- ai domain (#658) ---
    (
        |c| c == "ollama" || c.starts_with("ollama "),
        |c, output| ollama::compress(c, output),
    ),
    (
        |c| c.starts_with("mlflow "),
        |c, output| mlflow::compress(c, output),
    ),
    // --- security / supply-chain (#659) ---
    (
        |c| c.starts_with("semgrep "),
        |c, output| semgrep::compress(c, output),
    ),
    (
        |c| c.starts_with("trivy "),
        |c, output| trivy::compress(c, output),
    ),
    (
        |c| c.starts_with("grype "),
        |c, output| grype::compress(c, output),
    ),
    (
        |c| c.starts_with("syft "),
        |c, output| syft::compress(c, output),
    ),
    (
        |c| c.starts_with("cosign "),
        |c, output| cosign::compress(c, output),
    ),
    (
        |c| c.starts_with("swiftlint"),
        |c, output| swiftlint::compress(c, output),
    ),
    // --- vcs / toolchain (#660) ---
    (
        |c| c == "jj" || c.starts_with("jj "),
        |c, output| jj::compress(c, output),
    ),
    (
        |c| c == "mise" || c.starts_with("mise "),
        |c, output| mise::compress(c, output),
    ),
    (
        |c| c == "buf" || c.starts_with("buf "),
        |c, output| buf::compress(c, output),
    ),
    (
        |c| c.starts_with("gem "),
        |c, output| gem::compress(c, output),
    ),
    // --- edge / infra (#661) ---
    (
        |c| c == "pulumi" || c.starts_with("pulumi "),
        |c, output| pulumi::compress(c, output),
    ),
    (
        |c| c.starts_with("linkerd "),
        |c, output| linkerd::compress(c, output),
    ),
    (
        |c| c.starts_with("argocd "),
        |c, output| argocd::compress(c, output),
    ),
    (
        |c| {
            c == "vercel"
                || c.starts_with("vercel ")
                || c == "fly"
                || c.starts_with("fly ")
                || c.starts_with("flyctl ")
                || c.starts_with("wrangler ")
                || c.starts_with("skaffold ")
                || c.starts_with("supabase ")
        },
        |c, output| deploy::compress(c, output),
    ),
];

/// A grep-family search command, bare or with arguments (#980, #985).
///
/// The bare form matters: `proxy::compress::infer_command` synthesises `grep`
/// (no arguments) from tool names like `search_files`. The old `starts_with("grep ")`
/// required a trailing space and never matched, so those results fell through to the
/// terse pipeline which corrupted source identifiers.
fn is_grep_family(cmd: &str) -> bool {
    let first = cmd.split_whitespace().next().unwrap_or("");
    let base = first.rsplit('/').next().unwrap_or(first);
    matches!(
        base,
        "grep" | "egrep" | "fgrep" | "rg" | "ag" | "ack" | "ugrep" | "sift"
    )
}

/// Route `cmd` to its dedicated compressor: the first matching entry of the
/// dispatch table wins, and a matched entry's `None` is final.
pub fn try_specific_pattern_with(env: &dyn PatternEnv, cmd: &str, output: &str) -> Option<String> {
    let cl = cmd.to_ascii_lowercase();
    let c = cl.as_str();

    // No earlier table entry matches `curl `, so answering here is the same as
    // the curl entry declining.
    if c.starts_with("curl ") && env.is_curl_passthrough(c) {
        return None;
    }

    PATTERNS
        .iter()
        .find(|(matches, _)| matches(c))
        .and_then(|(_, handle)| handle(c, output))
}

#[cfg(test)]
mod gh1679 {
    use super::*;

    /// The reported failure: a `gh issue view --comments` result had 34 lines
    /// removed from the middle, marked only by `... (34 more lines)`. That
    /// string is shaped like content — the caller could not tell it from a line
    /// the command printed — and it was the only trace, because the savings
    /// footer that would have announced the cut is hidden on the MCP path by
    /// default. A lossy result was indistinguishable from a complete one.
    #[test]
    fn an_elision_announces_itself_as_ours() {
        let marker = elision_marker(34);

        assert!(
            marker.starts_with("[lean-ctx:"),
            "must be unmistakably ours, not content-shaped: {marker}"
        );
        assert!(
            !marker.contains("more lines)"),
            "the old content-shaped wording must be gone: {marker}"
        );
        assert!(marker.contains("34"), "{marker}");
    }

    /// The marker is the only thing guaranteed to survive, so the way back has
    /// to be in it. `raw=true` always works — unlike an archive handle, it does
    /// not depend on the response having been archived or on a hint tier.
    #[test]
    fn the_marker_names_a_recovery_that_always_exists() {
        assert!(
            elision_marker(1).contains("raw=true"),
            "{}",
            elision_marker(1)
        );
    }

    #[test]
    fn one_line_is_not_called_lines() {
        assert!(
            elision_marker(1).contains("1 line elided"),
            "{}",
            elision_marker(1)
        );
        assert!(
            elision_marker(2).contains("2 lines elided"),
            "{}",
            elision_marker(2)
        );
    }

    /// Every compressor that elides must use the shared marker. A new one that
    /// hand-rolls `... (N more lines)` would reintroduce exactly this bug, so
    /// the source tree is the assertion.
    #[test]
    fn no_compressor_hand_rolls_the_old_marker() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut offenders = Vec::new();

        for entry in std::fs::read_dir(&dir).expect("patterns dir") {
            let path = entry.expect("dir entry").path();
            if path.extension().and_then(|e| e.to_str()) != Some("rs") {
                continue;
            }
            let source = std::fs::read_to_string(&path).expect("read pattern source");
            // Only the production half: tests legitimately name the old string
            // to assert it is gone, and this module documents it.
            let production = source
                .split_once("#[cfg(test)]")
                .map_or(source.as_str(), |(head, _)| head);
            if production.contains("more lines)")
                && path.file_name().and_then(|n| n.to_str()) != Some("lib.rs")
            {
                offenders.push(path.display().to_string());
            }
        }

        assert!(
            offenders.is_empty(),
            "these compressors elide with the old content-shaped marker; \
             use super::elision_marker() instead: {offenders:?}"
        );
    }
}
