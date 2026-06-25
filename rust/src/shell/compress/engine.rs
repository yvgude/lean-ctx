use crate::core::patterns;
use crate::core::tokens::count_tokens;

use super::classification::{
    has_structural_output, is_search_output, is_verbatim_output, looks_like_toon,
};
use super::footer::shell_savings_footer;

pub(in crate::shell) fn compress_and_measure(
    command: &str,
    stdout: &str,
    stderr: &str,
    exit_code: i32,
) -> (String, usize) {
    let compressed_stdout = compress_for_outcome(command, stdout, exit_code);
    let compressed_stderr = compress_for_outcome(command, stderr, exit_code);

    let mut result = String::new();
    if !compressed_stdout.is_empty() {
        result.push_str(&compressed_stdout);
    }
    if !compressed_stderr.is_empty() {
        if !result.is_empty() {
            // On failure, label the stderr block so the agent can attribute the
            // error (mirrors `shell::combine_streams`); success keeps the plain
            // join for byte-stable output (#498).
            if exit_code != 0 {
                result.push('\n');
                result.push_str(crate::shell::STDERR_LABEL);
            }
            result.push('\n');
        }
        result.push_str(&compressed_stderr);
    }

    let content_for_counting = if let Some(pos) = result.rfind("\n[lean-ctx: ") {
        &result[..pos]
    } else {
        &result
    };
    let output_tokens = count_tokens(content_for_counting);
    (result, output_tokens)
}

/// Compress one stream, but never lossily for a command that actually FAILED.
///
/// A non-zero exit keeps the output verbatim (size-capped via [`truncate_verbatim`]
/// with safety-needle-preserving head/tail truncation) so the real error always
/// reaches the model and the agent never has to re-run the command without
/// lean-ctx (#809 / #810). This generalizes the build-tool-error guard inside
/// [`compress_if_beneficial`] to ANY non-zero exit. Empty output and explicit
/// `<lc_safe>` spans keep the normal pipeline (the latter so its markers are
/// stripped correctly); a succeeding command still compresses as before.
pub(crate) fn compress_for_outcome(command: &str, output: &str, exit_code: i32) -> String {
    if exit_code != 0 && !output.trim().is_empty() && !crate::core::protect::has_markers(output) {
        return truncate_verbatim(output, count_tokens(output));
    }
    compress_if_beneficial(command, output)
}

pub(crate) fn compress_if_beneficial(command: &str, output: &str) -> String {
    if output.trim().is_empty() {
        return String::new();
    }

    // #709: honour explicit <lc_safe>…</lc_safe> spans. Secret redaction has
    // already run upstream (ctx_shell::handle → redact_shell_output_secrets), so
    // the pipeline order is redact → protect → compress and a marker can never
    // smuggle a secret past redaction. Protected spans pass through verbatim;
    // each unprotected segment flows through the normal pipeline (footer stripped),
    // and a single savings footer is recomputed over the spliced result.
    if crate::core::protect::has_markers(output) {
        let original_tokens = count_tokens(output);
        let spliced = crate::core::protect::compress_preserving(output, |seg| {
            strip_shell_footer(&compress_if_beneficial(command, seg)).to_string()
        });
        let spliced_tokens = count_tokens(&spliced);
        return if spliced_tokens < original_tokens {
            shell_savings_footer(&spliced, original_tokens, spliced_tokens)
        } else {
            spliced
        };
    }

    // CRITICAL: Never compress error output from build/check/lint tools.
    // Compiler errors, type errors, lint findings etc. must be preserved verbatim
    // so the agent can see file paths, line numbers, and full diagnostics.
    if is_error_output_from_build_tool(command, output) {
        return truncate_verbatim(output, count_tokens(output));
    }

    // CRITICAL: Test-runner output is kept verbatim (only head/tail truncated
    // when huge, and even then middle test-result/failure lines are preserved).
    // This holds for fully-passing runs too, so pass/fail summaries can never be
    // semantically compressed or deduplicated away — on any OS or client.
    if is_test_runner_command(command) {
        return truncate_verbatim(output, count_tokens(output));
    }

    if !is_search_output(command) && crate::tools::ctx_shell::contains_auth_flow(output) {
        return output.to_string();
    }

    let original_tokens = count_tokens(output);

    if original_tokens < 30 {
        return output.to_string();
    }

    let min_output_tokens = 5;

    let cfg = crate::core::config::Config::load();
    let policy = crate::shell::output_policy::classify(command, &cfg.excluded_commands);
    if policy == crate::shell::output_policy::OutputPolicy::Verbatim
        || policy == crate::shell::output_policy::OutputPolicy::Passthrough
    {
        return truncate_verbatim(output, original_tokens);
    }

    // Format-aware passthrough (#342): output already in a compact, token-oriented
    // format the user opted to preserve (TOON by default) is kept verbatim.
    // Recompressing it saves little and rewrites the exact line/field shape an
    // agent relies on to validate a CLI output contract. This is output-shape
    // based, so any tool emitting the format is covered without listing commands.
    if cfg
        .preserve_compact_formats
        .iter()
        .any(|f| f.eq_ignore_ascii_case("toon"))
        && looks_like_toon(output)
    {
        return truncate_verbatim(output, original_tokens);
    }

    if is_verbatim_output(command) {
        return truncate_verbatim(output, original_tokens);
    }

    // Structural output AND version-control history are owned by their
    // dedicated compressor: apply it if it yields a gain, otherwise return the
    // output verbatim. Never let the generic terse/dedup/truncate fallbacks
    // below reshape it — they would corrupt commit subjects/hashes or drop
    // commits the caller explicitly requested (`git log --oneline -40`).
    if has_structural_output(command) || patterns::has_vcs_owner(command) {
        let cl = command.to_ascii_lowercase();
        if let Some(compressed) = patterns::try_specific_pattern(&cl, output)
            && !compressed.trim().is_empty()
        {
            let compressed_tokens = count_tokens(&compressed);
            if compressed_tokens >= min_output_tokens && compressed_tokens < original_tokens {
                return shell_savings_footer(&compressed, original_tokens, compressed_tokens);
            }
        }
        return output.to_string();
    }

    if let Some(mut compressed) = patterns::compress_output(command, output)
        && !compressed.trim().is_empty()
    {
        let level = crate::core::config::CompressionLevel::effective(&cfg);
        if level.is_active() {
            let terse_result =
                crate::core::terse::pipeline::compress(output, &level, Some(&compressed));
            if terse_result.quality_passed {
                compressed = terse_result.output;
            }
        }

        let compressed_tokens = count_tokens(&compressed);
        if compressed_tokens >= min_output_tokens && compressed_tokens < original_tokens {
            let ratio = compressed_tokens as f64 / original_tokens as f64;
            if ratio < 0.05 && original_tokens > 100 && original_tokens < 2000 {
                tracing::warn!("compression removed >95% of small output, returning original");
                return output.to_string();
            }
            return shell_savings_footer(&compressed, original_tokens, compressed_tokens);
        }
        if compressed_tokens < min_output_tokens {
            return output.to_string();
        }
    }

    {
        let level = crate::core::config::CompressionLevel::effective(&cfg);
        if level.is_active() {
            let terse_result = crate::core::terse::pipeline::compress(output, &level, None);
            if terse_result.quality_passed && terse_result.savings_pct >= 3.0 {
                return shell_savings_footer(
                    &terse_result.output,
                    terse_result.tokens_before as usize,
                    terse_result.tokens_after as usize,
                );
            }
        }
    }

    let cleaned = crate::core::compressor::lightweight_cleanup(output);
    let cleaned_tokens = count_tokens(&cleaned);
    if cleaned_tokens < original_tokens {
        let lines: Vec<&str> = cleaned.lines().collect();
        if lines.len() > 30 {
            let compressed = truncate_with_safety_scan(&lines, original_tokens);
            if let Some(c) = compressed {
                return c;
            }
        }
        if cleaned_tokens < original_tokens {
            return shell_savings_footer(&cleaned, original_tokens, cleaned_tokens);
        }
    }

    let lines: Vec<&str> = output.lines().collect();
    if lines.len() > 30
        && let Some(c) = truncate_with_safety_scan(&lines, original_tokens)
    {
        return c;
    }

    output.to_string()
}

/// Strip a trailing `\n[lean-ctx: …]` savings footer so per-segment results can
/// be spliced (protect spans, #709) before a single footer is recomputed.
fn strip_shell_footer(s: &str) -> &str {
    match s.rfind("\n[lean-ctx: ") {
        Some(pos) => &s[..pos],
        None => s,
    }
}

/// Detects whether the output contains error diagnostics from a build/check/lint tool.
/// When true, compression is bypassed to preserve file paths, line numbers, and messages.
fn is_error_output_from_build_tool(command: &str, output: &str) -> bool {
    let cmd = command.trim().to_ascii_lowercase();

    let is_build_tool = cmd.starts_with("cargo check")
        || cmd.starts_with("cargo build")
        || cmd.starts_with("cargo clippy")
        || cmd.starts_with("cargo test")
        || cmd.starts_with("cargo fmt")
        || cmd.starts_with("cargo run")
        || cmd.starts_with("rustc ")
        || cmd.starts_with("gcc ")
        || cmd.starts_with("g++ ")
        || cmd.starts_with("clang ")
        || cmd.starts_with("clang++ ")
        || cmd.starts_with("make ")
        || cmd.starts_with("cmake ")
        || cmd.starts_with("go build")
        || cmd.starts_with("go vet")
        || cmd.starts_with("go test")
        || cmd.starts_with("golangci-lint")
        || cmd.starts_with("tsc ")
        || cmd.starts_with("tsc\t")
        || cmd == "tsc"
        || cmd.starts_with("npx tsc")
        || cmd.starts_with("eslint")
        || cmd.starts_with("npx eslint")
        || cmd.starts_with("biome ")
        || cmd.starts_with("prettier ")
        || cmd.starts_with("mypy ")
        || cmd.starts_with("pyright ")
        || cmd.starts_with("pylint ")
        || cmd.starts_with("ruff check")
        || cmd.starts_with("flake8")
        || cmd.starts_with("black --check")
        || cmd.starts_with("swift build")
        || cmd.starts_with("swiftc ")
        || cmd.starts_with("xcodebuild ")
        || cmd.starts_with("javac ")
        || cmd.starts_with("gradle ")
        || cmd.starts_with("./gradlew ")
        || cmd.starts_with("mvn ")
        || cmd.starts_with("./mvnw ")
        || cmd.starts_with("dotnet build")
        || cmd.starts_with("dotnet test")
        || cmd.starts_with("msbuild")
        || cmd.starts_with("zig build")
        || cmd.starts_with("nim c ")
        || cmd.starts_with("ghc ")
        || cmd.starts_with("stack build")
        || cmd.starts_with("cabal build")
        || cmd.starts_with("mix compile")
        || cmd.starts_with("mix test")
        || cmd.starts_with("mix credo")
        || cmd.starts_with("shellcheck ")
        || cmd.starts_with("hadolint ")
        || cmd.starts_with("terraform validate")
        || cmd.starts_with("terraform plan")
        || cmd.starts_with("ansible-lint")
        || cmd.starts_with("rubocop ")
        || cmd.starts_with("solhint ")
        || cmd.starts_with("slither ");

    if !is_build_tool {
        return false;
    }

    // Check if the output actually contains error indicators
    output.contains("error[")
        || output.contains("error:")
        || output.contains("Error:")
        || output.contains("ERROR:")
        || output.contains(" error ")
        || output.contains("warning[")
        || output.contains("warning:")
        || output.contains("failed")
        || output.contains("FAILED")
        || output.contains("panicked at")
        || output.contains("cannot find")
        || output.contains("not found")
        || output.contains("undefined")
        || output.contains("unresolved")
        || output.contains("expected ")
        || output.contains("mismatched types")
        || output.contains("aborting due to")
        || output.contains("could not compile")
}

/// Strips leading `VAR=value` environment assignments from a command segment so
/// `RUST_BACKTRACE=1 cargo test` / `CI=true pytest` are still recognized as the
/// underlying test runner.
fn strip_env_prefix(segment: &str) -> &str {
    let mut rest = segment.trim_start();
    loop {
        let Some(first) = rest.split_whitespace().next() else {
            return rest;
        };
        // An env assignment is a single token containing '=' before any '/' so it
        // isn't confused with a path or a flag like `--threads=4`.
        let is_env_assignment = first.contains('=')
            && !first.starts_with('-')
            && first.split('=').next().is_some_and(|name| {
                !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
            });
        if !is_env_assignment {
            return rest;
        }
        rest = rest[first.len()..].trim_start();
    }
}

/// Detects test-runner commands across ecosystems. Their output must never be
/// semantically compressed/deduplicated — only verbatim head/tail truncation
/// (with middle test/error lines preserved). Matched even for fully-passing
/// runs so per-suite summaries always survive. Checks each pipeline segment so
/// `cargo test … | grep …` / `pytest … | tail` are caught too.
fn is_test_runner_command(command: &str) -> bool {
    command
        .split('|')
        .map(|seg| strip_env_prefix(seg.trim()).to_ascii_lowercase())
        .any(|seg| {
            seg.starts_with("cargo test")
                || seg.starts_with("cargo nextest")
                || seg.starts_with("nextest")
                || seg.starts_with("pytest")
                || seg.starts_with("python -m pytest")
                || seg.starts_with("python3 -m pytest")
                || seg.starts_with("py.test")
                || seg.starts_with("go test")
                || seg.starts_with("gotestsum")
                || seg.starts_with("npm test")
                || seg.starts_with("npm run test")
                || seg.starts_with("pnpm test")
                || seg.starts_with("pnpm run test")
                || seg.starts_with("yarn test")
                || seg.starts_with("bun test")
                || seg.starts_with("deno test")
                || seg.starts_with("jest")
                || seg.starts_with("npx jest")
                || seg.starts_with("vitest")
                || seg.starts_with("npx vitest")
                || seg.starts_with("mocha")
                || seg.starts_with("npx mocha")
                || seg.starts_with("dotnet test")
                || seg.starts_with("mix test")
                || seg.starts_with("rspec")
                || seg.starts_with("bundle exec rspec")
                || seg.starts_with("phpunit")
                || seg.starts_with("./vendor/bin/phpunit")
                || seg.starts_with("./gradlew test")
                || seg.starts_with("gradle test")
                || seg.starts_with("mvn test")
                || seg.starts_with("ctest")
        })
}

const MAX_VERBATIM_TOKENS: usize = 8000;

/// For verbatim commands: never transform content, only head/tail truncate if huge.
///
/// Even when truncating, every safety- and test-relevant line from the omitted
/// middle is preserved (test-result summaries, panics, failures, errors). This
/// guarantees a large test run — even a fully passing one with dozens of
/// per-suite `test result:` lines — never silently loses its outcome lines,
/// regardless of OS or client (issue: compression must never swallow signal).
fn truncate_verbatim(output: &str, original_tokens: usize) -> String {
    if original_tokens <= MAX_VERBATIM_TOKENS {
        return output.to_string();
    }
    let lines: Vec<&str> = output.lines().collect();
    let total = lines.len();
    if total <= 60 {
        return output.to_string();
    }
    let head = 30.min(total);
    let tail = 20.min(total.saturating_sub(head));
    let middle = &lines[head..total - tail];

    // Preserve up to 200 safety/test/diagnostic lines from the omitted middle so
    // buried failures and per-suite summaries survive head/tail truncation.
    let preserved = crate::core::safety_needles::extract_safety_lines(middle, 200);
    let omitted = middle.len() - preserved.len();

    let mut result = String::with_capacity(output.len() / 2);
    for line in &lines[..head] {
        result.push_str(line);
        result.push('\n');
    }
    if preserved.is_empty() {
        result.push_str(&format!(
            "\n[{omitted} lines omitted — output too large for context window]\n\n"
        ));
    } else {
        result.push_str(&format!(
            "\n[{omitted} lines omitted, {} test/diagnostic lines preserved]\n",
            preserved.len()
        ));
        for line in &preserved {
            result.push_str(line);
            result.push('\n');
        }
        result.push('\n');
    }
    for line in lines.iter().skip(total - tail) {
        result.push_str(line);
        result.push('\n');
    }
    let truncated_tokens = count_tokens(&result);
    if crate::core::protocol::savings_footer_visible() {
        result.push_str(&format!(
            "[lean-ctx: {original_tokens}→{truncated_tokens} tok, verbatim truncated]"
        ));
    }
    result
}

fn truncate_with_safety_scan(lines: &[&str], original_tokens: usize) -> Option<String> {
    use crate::core::safety_needles;

    let first = &lines[..5];
    let last = &lines[lines.len() - 5..];
    let middle = &lines[5..lines.len() - 5];

    let safety_lines = safety_needles::extract_safety_lines(middle, 80);
    let safety_count = safety_lines.len();
    let omitted = middle.len() - safety_count;

    let mut parts = Vec::new();
    parts.push(first.join("\n"));
    if safety_count > 0 {
        parts.push(format!(
            "[{omitted} lines omitted, {safety_count} safety-relevant lines preserved]"
        ));
        parts.push(safety_lines.join("\n"));
    } else {
        parts.push(format!("[{omitted} lines omitted]"));
    }
    parts.push(last.join("\n"));

    let compressed = parts.join("\n");
    let ct = count_tokens(&compressed);
    if ct >= original_tokens {
        return None;
    }
    Some(shell_savings_footer(&compressed, original_tokens, ct))
}

/// Public wrapper for integration tests to exercise the compression pipeline.
#[must_use]
pub fn compress_if_beneficial_pub(command: &str, output: &str) -> String {
    compress_if_beneficial(command, output)
}

/// Preserve build/test output verbatim, applying only the safety-line-preserving
/// head/tail truncation when it is oversized.
///
/// The proxy funnel uses this when a foreign shell tool produced unmistakable
/// build/test output but supplied no recognizable command — the engine's
/// command-gated verbatim guards cannot fire, yet compiler errors, panics and
/// test summaries must still reach the model intact for a bug-fix task.
pub(crate) fn preserve_verbatim_pub(output: &str) -> String {
    truncate_verbatim(output, count_tokens(output))
}
