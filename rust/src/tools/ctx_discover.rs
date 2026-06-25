use std::collections::HashMap;

use crate::core::stats::StatsStore;
use crate::core::tokens::count_tokens;
use crate::shell::output_policy::{OutputPolicy, classify};

/// Command families with a dedicated lean-ctx compressor, used to *recognise*
/// compressible commands in shell history (the savings numbers themselves come
/// from real measured `core::stats`, never from this table). `base` is the
/// `normalize_command` base name; keep roughly in sync with the dispatch in
/// `core::patterns::try_specific_pattern`.
const COMPRESSIBLE_FAMILIES: &[(&str, &str)] = &[
    ("git", "git status/diff/log/commit/push"),
    ("gh", "GitHub CLI"),
    ("glab", "GitLab CLI"),
    ("cargo", "cargo build/test/clippy"),
    ("npm", "npm install/run/test"),
    ("pnpm", "pnpm install/run/test"),
    ("yarn", "yarn install/run/test"),
    ("bun", "Bun runtime"),
    ("deno", "Deno runtime"),
    ("docker", "docker ps/images/logs/build"),
    ("kubectl", "kubectl get/describe/logs"),
    ("helm", "Kubernetes Helm"),
    ("pip", "pip install/list/freeze"),
    ("poetry", "Poetry"),
    ("uv", "uv add/lock/sync"),
    ("conda", "conda/mamba env"),
    ("pipx", "pipx install"),
    ("go", "go test/build/vet"),
    ("mypy", "mypy type check"),
    ("pyright", "pyright type check"),
    ("ruff", "ruff check/format"),
    ("eslint", "eslint/biome lint"),
    ("prettier", "prettier --check"),
    ("tsc", "TypeScript compiler"),
    ("pytest", "Python tests"),
    ("jest", "Jest tests"),
    ("vitest", "Vitest tests"),
    ("mocha", "Mocha tests"),
    ("playwright", "Playwright tests"),
    ("rspec", "Ruby tests"),
    ("rubocop", "RuboCop lint"),
    ("bundle", "Bundler"),
    ("rake", "Rake tasks"),
    ("curl", "HTTP requests"),
    ("wget", "HTTP downloads"),
    ("grep", "grep/rg search"),
    ("rg", "ripgrep search"),
    ("find", "find files"),
    ("fd", "fd file search"),
    ("ls", "directory listing"),
    ("jq", "JSON processing"),
    ("aws", "AWS CLI"),
    ("terraform", "Terraform"),
    ("tofu", "OpenTofu"),
    ("pulumi", "Pulumi IaC"),
    ("ansible", "Ansible"),
    ("prisma", "Prisma ORM"),
    ("psql", "PostgreSQL"),
    ("mysql", "MySQL/MariaDB"),
    ("cmake", "CMake build"),
    ("ninja", "Ninja build"),
    ("bazel", "Bazel build"),
    ("make", "Make targets"),
    ("just", "just recipes"),
    ("mvn", "Maven build"),
    ("gradle", "Gradle build"),
    ("dotnet", "dotnet build/test"),
    ("flutter", "Flutter build"),
    ("swift", "Swift build/test"),
    ("zig", "Zig build/test"),
    ("composer", "PHP Composer"),
    ("mix", "Elixir Mix"),
    ("next", "Next.js build"),
    ("vite", "Vite build"),
    ("turbo", "Turborepo"),
    ("nx", "Nx monorepo"),
    ("systemctl", "systemd units"),
    ("journalctl", "systemd logs"),
    ("dbt", "dbt models/tests"),
    ("alembic", "Alembic migrations"),
    ("flyway", "Flyway migrations"),
    ("ollama", "Ollama models"),
    ("mlflow", "MLflow runs"),
    ("semgrep", "Semgrep scan"),
    ("trivy", "Trivy scan"),
    ("grype", "Grype scan"),
    ("syft", "Syft SBOM"),
    ("cosign", "Cosign verify"),
    ("swiftlint", "SwiftLint"),
    ("jj", "Jujutsu VCS"),
    ("mise", "mise toolchain"),
    ("buf", "Protobuf buf"),
    ("gem", "RubyGems"),
    ("linkerd", "Linkerd check"),
    ("argocd", "Argo CD"),
    ("vercel", "Vercel deploy"),
    ("fly", "Fly.io deploy"),
    ("wrangler", "Cloudflare deploy"),
    ("skaffold", "Skaffold"),
    ("supabase", "Supabase"),
];

pub struct DiscoverResult {
    pub total_commands: u32,
    pub already_optimized: u32,
    pub missed_commands: Vec<MissedCommand>,
    pub potential_tokens: usize,
    pub potential_usd: f64,
    /// True when at least one missed command had real measured savings backing
    /// its token estimate. When false, `potential_tokens` is 0 and callers
    /// should fall back to a frequency-based framing instead of a $ figure.
    pub has_measured_data: bool,
}

pub struct MissedCommand {
    pub prefix: String,
    pub description: String,
    /// Real measured savings for this family (e.g. "84%"), or "—" when the
    /// user has not yet run this family through lean-ctx.
    pub savings_range: String,
    pub count: u32,
    pub estimated_tokens: usize,
    pub measured: bool,
}

/// First whitespace token's file name — the `normalize_command` base.
fn base_of(command: &str) -> &str {
    let first = command.split_whitespace().next().unwrap_or(command);
    std::path::Path::new(first)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(first)
}

fn describe(base: &str) -> Option<&'static str> {
    COMPRESSIBLE_FAMILIES
        .iter()
        .find(|(b, _)| *b == base)
        .map(|(_, d)| *d)
}

/// Aggregated real measurements per command family from `core::stats`:
/// base → (`input_tokens`, `output_tokens`, count).
fn measured_by_family(store: &StatsStore) -> HashMap<String, (u64, u64, u64)> {
    let mut by_base: HashMap<String, (u64, u64, u64)> = HashMap::new();
    for (key, s) in &store.commands {
        let entry = by_base.entry(base_of(key).to_string()).or_default();
        entry.0 = entry.0.saturating_add(s.input_tokens);
        entry.1 = entry.1.saturating_add(s.output_tokens);
        entry.2 = entry.2.saturating_add(s.count);
    }
    by_base
}

#[must_use]
pub fn analyze_history(history: &[String], limit: usize) -> DiscoverResult {
    let store = crate::core::stats::load();
    analyze_history_with_stats(history, limit, &store)
}

/// Core analysis with the measured-stats source injected (testable, pure aside
/// from the policy classifier). `analyze_history` is the disk-backed wrapper.
fn analyze_history_with_stats(
    history: &[String],
    limit: usize,
    store: &StatsStore,
) -> DiscoverResult {
    let mut missed: HashMap<String, u32> = HashMap::new();
    let mut already_optimized = 0u32;
    let mut total_commands = 0u32;

    let measured = measured_by_family(store);

    for cmd in history {
        let trimmed = cmd.trim();
        if trimmed.is_empty() {
            continue;
        }
        total_commands += 1;

        if trimmed.starts_with("lean-ctx ") || trimmed.starts_with("lean-ctx\t") {
            already_optimized += 1;
            continue;
        }

        let base = base_of(trimmed);
        // A command is a missed save only if lean-ctx has a compressor for it
        // (known family or already measured) AND the policy engine would
        // actually compress it (not a protected verbatim/passthrough command).
        let known = describe(base).is_some() || measured.contains_key(base);
        if known && classify(trimmed, &[]) == OutputPolicy::Compressible {
            *missed.entry(base.to_string()).or_insert(0) += 1;
        }
    }

    let mut sorted: Vec<_> = missed.into_iter().collect();
    sorted.sort_by_key(|x| std::cmp::Reverse(x.1));

    let price_per_tok = crate::core::stats::DEFAULT_INPUT_PRICE_PER_M / 1_000_000.0;
    let mut potential_tokens = 0usize;
    let mut has_measured_data = false;

    let missed_commands: Vec<MissedCommand> = sorted
        .into_iter()
        .take(limit)
        .map(|(base, count)| {
            let description = describe(&base).unwrap_or("compressible output").to_string();
            // Project the family's *real* measured savings onto its plain-shell
            // frequency. Families without measurements contribute nothing — we
            // never invent a token figure.
            let (savings_range, estimated_tokens, is_measured) = match measured.get(&base) {
                Some(&(input, output, cnt)) if input > 0 && cnt > 0 => {
                    let rate = 1.0 - (output as f64 / input as f64);
                    let avg_input = input as f64 / cnt as f64;
                    let est = (f64::from(count) * avg_input * rate).max(0.0) as usize;
                    has_measured_data = true;
                    potential_tokens += est;
                    (format!("{:.0}%", (rate * 100.0).max(0.0)), est, true)
                }
                _ => ("—".to_string(), 0, false),
            };
            MissedCommand {
                prefix: base,
                description,
                savings_range,
                count,
                estimated_tokens,
                measured: is_measured,
            }
        })
        .collect();

    let potential_usd = potential_tokens as f64 * price_per_tok;

    DiscoverResult {
        total_commands,
        already_optimized,
        missed_commands,
        potential_tokens,
        potential_usd,
        has_measured_data,
    }
}

#[must_use]
pub fn discover_from_history(history: &[String], limit: usize) -> String {
    let result = analyze_history(history, limit);

    if result.missed_commands.is_empty() {
        return format!(
            "No missed savings found in last {} commands. \
            {} already optimized.",
            result.total_commands, result.already_optimized
        );
    }

    let mut lines = Vec::new();
    lines.push(format!(
        "Analyzed {} commands ({} already optimized):",
        result.total_commands, result.already_optimized
    ));
    lines.push(String::new());

    let total_missed: u32 = result.missed_commands.iter().map(|m| m.count).sum();
    lines.push(format!(
        "{total_missed} commands could benefit from lean-ctx:"
    ));
    lines.push(String::new());

    for m in &result.missed_commands {
        lines.push(format!(
            "  {:>4}x  {:<12} {} ({})",
            m.count, m.prefix, m.description, m.savings_range
        ));
    }

    lines.push(String::new());
    if result.has_measured_data {
        lines.push(format!(
            "Estimated potential: ~{} tokens saved (~${:.2}), projected from your measured savings",
            result.potential_tokens, result.potential_usd
        ));
    } else {
        lines.push(
            "No measured savings yet — run these via lean-ctx, then re-run discover for real numbers."
                .to_string(),
        );
    }
    lines.push(String::new());
    lines.push("Fix: run 'lean-ctx init --global' to auto-compress all commands.".to_string());
    lines.push("Or:  run 'lean-ctx init --agent <tool>' for AI tool hooks.".to_string());

    let output = lines.join("\n");
    let tokens = count_tokens(&output);
    format!("{output}\n\n[{tokens} tok]")
}

#[must_use]
pub fn format_cli_output(result: &DiscoverResult) -> String {
    if result.missed_commands.is_empty() {
        return format!(
            "All compressible commands are already using lean-ctx!\n\
             ({} commands analyzed, {} via lean-ctx)",
            result.total_commands, result.already_optimized
        );
    }

    let mut lines = Vec::new();
    let total_missed: u32 = result.missed_commands.iter().map(|m| m.count).sum();

    lines.push(format!(
        "Found {total_missed} compressible commands not using lean-ctx:\n"
    ));
    lines.push(format!(
        "  {:<14} {:>5}  {:>10}  {:<30} {}",
        "COMMAND", "COUNT", "SAVINGS", "DESCRIPTION", "EST. TOKENS"
    ));
    lines.push(format!("  {}", "-".repeat(80)));

    for m in &result.missed_commands {
        let est = if m.measured {
            format!("~{}", m.estimated_tokens)
        } else {
            "—".to_string()
        };
        lines.push(format!(
            "  {:<14} {:>5}x {:>10}  {:<30} {}",
            m.prefix, m.count, m.savings_range, m.description, est
        ));
    }

    lines.push(String::new());
    if result.has_measured_data {
        lines.push(format!(
            "Estimated missed savings: ~{} tokens (~${:.2}/month), projected from your measured rate",
            result.potential_tokens,
            result.potential_usd * 30.0
        ));
    } else {
        lines.push(
            "No measured savings yet — route these through lean-ctx, then re-run discover."
                .to_string(),
        );
    }
    lines.push(format!(
        "Already using lean-ctx: {} commands",
        result.already_optimized
    ));
    lines.push(String::new());
    lines.push("Run 'lean-ctx init --global' to enable compression for all commands.".to_string());

    lines.join("\n")
}

/// Renders a shareable "before lean-ctx" SVG card from a discover analysis — the
/// "ghost tokens you're leaving on the table" framing that drives the first-run share
/// loop. Same 1200x630 social-card dimensions and visual language as the Wrapped card,
/// but in an amber/red "leak" palette. Pure string building; all data-derived text is
/// XML-escaped. Aggregate estimates only — never command contents or arguments.
#[must_use]
pub fn render_before_card(result: &DiscoverResult) -> String {
    let saved = crate::core::wrapped::format_tokens(result.potential_tokens as u64);
    let monthly_usd = result.potential_usd * 30.0;
    let total_missed: u32 = result.missed_commands.iter().map(|m| m.count).sum();
    let top = before_card_top_commands(result);
    format!(
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="1200" height="630" viewBox="0 0 1200 630" font-family="Inter, system-ui, -apple-system, Segoe UI, Roboto, sans-serif">
  <defs>
    <linearGradient id="bg" x1="0" y1="0" x2="1" y2="1">
      <stop offset="0" stop-color="#0b1020"/>
      <stop offset="1" stop-color="#131a2e"/>
    </linearGradient>
    <linearGradient id="accent" x1="0" y1="0" x2="1" y2="0">
      <stop offset="0" stop-color="#f59e0b"/>
      <stop offset="1" stop-color="#ef4444"/>
    </linearGradient>
  </defs>
  <rect width="1200" height="630" fill="url(#bg)"/>
  <rect x="0" y="0" width="1200" height="8" fill="url(#accent)"/>
  <text x="70" y="92" fill="#e5e7eb" font-size="34" font-weight="700">lean-ctx <tspan fill="#f59e0b">Ghost Tokens</tspan></text>
  <text x="70" y="130" fill="#94a3b8" font-size="24">before lean-ctx — estimated from my shell history</text>
  <text x="70" y="300" fill="#f59e0b" font-size="120" font-weight="800" font-family="ui-monospace, SFMono-Regular, Menlo, monospace">{saved}</text>
  <text x="76" y="346" fill="#94a3b8" font-size="26">tokens/month left on the table</text>
  <text x="70" y="430" fill="#e5e7eb" font-size="60" font-weight="800" font-family="ui-monospace, SFMono-Regular, Menlo, monospace">${monthly_usd:.0}</text>
  <text x="74" y="462" fill="#94a3b8" font-size="22">potential monthly savings</text>
  <text x="70" y="512" fill="#cbd5e1" font-size="22">{total_missed} uncompressed commands · {already} already via lean-ctx</text>
{top}
  <text x="70" y="600" fill="#475569" font-size="17">Estimate from local shell history · run `lean-ctx onboard` to stop the leak</text>
  <text x="1130" y="600" text-anchor="end" fill="#f59e0b" font-size="26" font-weight="700">leanctx.com</text>
</svg>"##,
        already = result.already_optimized,
    )
}

/// The top three missed commands as a single muted line. Empty when none.
fn before_card_top_commands(result: &DiscoverResult) -> String {
    if result.missed_commands.is_empty() {
        return String::new();
    }
    let joined = result
        .missed_commands
        .iter()
        .take(3)
        .map(|m| format!("{} {}x", m.prefix, m.count))
        .collect::<Vec<_>>()
        .join("    ·    ");
    format!(
        "  <text x=\"70\" y=\"556\" fill=\"#cbd5e1\" font-size=\"22\">top missed  {}</text>",
        xml_escape(&joined)
    )
}

/// Minimal XML text escaping for data-derived strings in the SVG card.
fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

#[cfg(test)]
mod tests {
    use super::{analyze_history, analyze_history_with_stats, render_before_card};
    use crate::core::stats::{CommandStats, StatsStore};

    fn history() -> Vec<String> {
        vec![
            "git status".into(),
            "git diff".into(),
            "cargo build".into(),
            "cargo test".into(),
            "lean-ctx gain".into(),
            "vim notes.txt".into(),
        ]
    }

    fn stats_with(entries: &[(&str, u64, u64, u64)]) -> StatsStore {
        let mut s = StatsStore::default();
        for (key, count, input, output) in entries {
            s.commands.insert(
                (*key).to_string(),
                CommandStats {
                    count: *count,
                    input_tokens: *input,
                    output_tokens: *output,
                },
            );
            s.total_commands += count;
            s.total_input_tokens += input;
            s.total_output_tokens += output;
        }
        s
    }

    #[test]
    fn no_measured_data_yields_zero_potential_and_no_fabrication() {
        // Empty stats: detection still works, but NO token figure is invented.
        let r = analyze_history_with_stats(&history(), 20, &StatsStore::default());
        assert!(!r.has_measured_data, "no stats => no measured data");
        assert_eq!(r.potential_tokens, 0, "must not fabricate tokens");
        assert_eq!(r.potential_usd, 0.0, "must not fabricate dollars");
        assert!(
            r.missed_commands
                .iter()
                .all(|m| !m.measured && m.savings_range == "—"),
            "unmeasured families show em dash, not a fake range"
        );
        // git (2) + cargo (2) recognised; vim ignored; lean-ctx already-optimized.
        assert_eq!(r.already_optimized, 1);
        assert!(
            r.missed_commands
                .iter()
                .any(|m| m.prefix == "git" && m.count == 2)
        );
    }

    #[test]
    fn measured_family_projects_real_savings() {
        // git measured at 90% savings (1000 in -> 100 out over 2 runs => 500 avg in).
        let store = stats_with(&[("git status", 2, 1000, 100)]);
        let r = analyze_history_with_stats(&history(), 20, &store);
        assert!(r.has_measured_data);
        let git = r
            .missed_commands
            .iter()
            .find(|m| m.prefix == "git")
            .expect("git present");
        assert!(git.measured);
        assert_eq!(git.savings_range, "90%", "real measured rate, not a guess");
        // 2 plain-shell git cmds * 500 avg_input * 0.9 = 900 projected.
        assert_eq!(git.estimated_tokens, 900);
        assert!(git.savings_range.ends_with('%'));
        // cargo has no measurement => contributes nothing.
        let cargo = r
            .missed_commands
            .iter()
            .find(|m| m.prefix == "cargo")
            .unwrap();
        assert_eq!(cargo.estimated_tokens, 0);
        assert_eq!(r.potential_tokens, 900);
    }

    #[test]
    fn newly_shipped_families_are_recognised() {
        // Regression guard: families added in #657-#661 must be discoverable.
        let hist: Vec<String> = ["dbt run", "trivy image nginx", "pulumi up", "jj log"]
            .into_iter()
            .map(String::from)
            .collect();
        let r = analyze_history_with_stats(&hist, 20, &StatsStore::default());
        for fam in ["dbt", "trivy", "pulumi", "jj"] {
            assert!(
                r.missed_commands.iter().any(|m| m.prefix == fam),
                "{fam} should be recognised as compressible"
            );
        }
    }

    #[test]
    fn before_card_is_well_formed_and_branded() {
        let result = analyze_history(&history(), 20);
        let svg = render_before_card(&result);
        assert!(svg.starts_with("<svg"), "must be an SVG document");
        assert!(svg.trim_end().ends_with("</svg>"), "must close the svg tag");
        assert!(svg.contains("leanctx.com"), "must carry the brand footer");
        assert!(svg.contains("Ghost Tokens"), "must frame the leak");
        assert!(
            svg.contains("tokens/month left on the table"),
            "headline label present"
        );
    }

    #[test]
    fn xml_escape_neutralizes_markup() {
        assert_eq!(super::xml_escape("a<b>&\"'"), "a&lt;b&gt;&amp;&quot;&apos;");
    }
}
