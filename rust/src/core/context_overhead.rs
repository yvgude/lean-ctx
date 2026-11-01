//! Honest accounting of the fixed per-turn context lean-ctx injects (GitHub #361).
//!
//! Three components ride every request and — on a provider WITHOUT prompt caching
//! — are re-billed on every turn:
//!  - the exposed MCP **tool schemas** (description + input schema of each tool),
//!  - the MCP **server instructions** block, and
//!  - the **rules block** lean-ctx writes into the host's instruction file
//!    (`CLAUDE.md` / `AGENTS.md`).
//!
//! `lean-ctx gain` measures *compression on lean-ctx-touched reads* — its
//! denominator is lean-ctx traffic, not the provider bill. On a phase-isolated /
//! non-caching workload (separate process per phase, no provider cache) the
//! cached-re-read lever has no surface, so the headline can read net-positive
//! while the bill moved net-negative. Surfacing this overhead — and stating the
//! denominator — keeps the meter honest.
//!
//! Net bill impact ≈ `gross_saved_tokens − total_tokens() × turns`.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::core::tokens::count_tokens;

static PROACTIVE_INJECTED_TOKENS: AtomicUsize = AtomicUsize::new(0);

/// Record tokens appended as proactive context so savings reports can account
/// for the dynamic response-side injection separately from fixed overhead.
pub fn record_proactive_injection(tokens: usize) {
    PROACTIVE_INJECTED_TOKENS.fetch_add(tokens, Ordering::Relaxed);
}

/// Total proactive context tokens appended by this process.
#[must_use]
pub fn proactive_injected_tokens() -> usize {
    PROACTIVE_INJECTED_TOKENS.load(Ordering::Relaxed)
}

#[cfg(test)]
fn reset_proactive_injection() {
    PROACTIVE_INJECTED_TOKENS.store(0, Ordering::Relaxed);
}

/// A measured breakdown, in tokens, of the per-turn context lean-ctx adds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ContextOverhead {
    /// Number of MCP tools exposed (the schema-bearing surface).
    pub tool_count: usize,
    /// Tokens for all exposed tool descriptions + input schemas.
    pub tool_schema_tokens: usize,
    /// Tokens for the MCP server instructions block (capped at the instruction budget).
    pub instruction_tokens: usize,
    /// Tokens for the rules block injected into the host instruction file.
    pub rules_block_tokens: usize,
}

impl ContextOverhead {
    /// Total per-turn overhead in tokens.
    #[must_use]
    pub fn total_tokens(&self) -> usize {
        self.tool_schema_tokens + self.instruction_tokens + self.rules_block_tokens
    }

    /// Process-cached overhead. The tool surface and rules block are static and
    /// the instruction block varies only with slow-moving session state, so a
    /// once-per-process measurement is the right tradeoff for callers that render
    /// repeatedly (the `gain` dashboard re-renders every second in `--live`) —
    /// it avoids per-tick disk I/O and re-tokenization.
    #[must_use]
    pub fn cached() -> Self {
        static CACHE: OnceLock<ContextOverhead> = OnceLock::new();
        *CACHE.get_or_init(Self::measure)
    }

    /// Measure the overhead for the currently-configured MCP surface. Uses the
    /// same advertisement policy as the live `tools/list` handler (candidate
    /// set, profile gates, invoker, description compression), so the number
    /// reflects what this install actually advertises (#572).
    #[must_use]
    pub fn measure() -> Self {
        Self::measure_for_client("")
    }

    /// [`Self::measure`] with the instructions a named MCP client receives
    /// (`clientInfo.name`, e.g. `claude-code`): hook-covered clients and
    /// clients with an instruction cap get a different block.
    #[must_use]
    pub fn measure_for_client(client_name: &str) -> Self {
        let tools = crate::server::tool_visibility::advertised_tool_defs_default();
        let tool_count = tools.len();
        let tool_schema_tokens = tools.iter().map(tool_tokens).sum();

        let instructions = crate::instructions::build_instructions_with_client(
            crate::tools::CrpMode::effective(),
            client_name,
        );
        let instruction_tokens = count_tokens(&instructions);

        // The rules block only rides every turn when lean-ctx actually injects it
        // into the host instruction file. With `rules_injection = off` no file is
        // written (the `rules_inject` injectors early-return), so it adds zero
        // per-turn overhead — counting it would overstate the faithful-arm tax and
        // make the net-of-injection figure pessimistic (#361).
        let rules_block_tokens = if crate::core::config::Config::load().rules_injection_effective()
            == crate::core::config::RulesInjection::Off
        {
            0
        } else {
            count_tokens(&crate::rules_inject::canonical_rules_block())
        };

        Self {
            tool_count,
            tool_schema_tokens,
            instruction_tokens,
            rules_block_tokens,
        }
    }
}

impl ContextOverhead {
    /// Per-turn prefix of the opt-in Claude Code governor setup (governor
    /// design v1, slice 1). Its hooks emit no session or per-prompt text, so
    /// the prefix is the rule block plus, in the MCP arm, one deferred-tool
    /// name line per tool: initialize instructions are suppressed
    /// (`LEAN_CTX_RULES_INJECTION=off` on the server) and schemas stay behind
    /// Tool Search until fetched (`alwaysLoad: false`). The name-line cost
    /// models Claude Code's deferred-tool listing [inference, bench-verified].
    #[must_use]
    pub(crate) fn measure_governor(
        arm: crate::hooks::agents::claude_governor::GovernorArm,
    ) -> Self {
        use crate::hooks::agents::claude_governor::{GovernorArm, rules_block};
        let rules_block_tokens = count_tokens(rules_block(arm));
        let (tool_count, tool_schema_tokens) = match arm {
            GovernorArm::NoMcp => (0, 0),
            GovernorArm::McpDeferred => {
                let tools = crate::server::tool_visibility::advertised_tool_defs_default();
                let names: Vec<String> = tools
                    .iter()
                    .map(|t| format!("mcp__lean-ctx__{}", t.name))
                    .collect();
                (tools.len(), count_tokens(&names.join("\n")))
            }
        };
        Self {
            tool_count,
            tool_schema_tokens,
            instruction_tokens: 0,
            rules_block_tokens,
        }
    }
}

/// Description + input-schema tokens for one tool definition — exactly the two
/// fields a client re-sends in every request's tool list.
pub fn tool_tokens(t: &rmcp::model::Tool) -> usize {
    let desc = t
        .description
        .as_ref()
        .map_or(0, |d| count_tokens(d.as_ref()));
    let schema = count_tokens(&serde_json::to_string(&t.input_schema).unwrap_or_default());
    desc + schema
}

/// Estimated per-turn overhead of native IDE tools (Read, Grep, Shell, Glob,
/// Write, StrReplace). lean-ctx stands in for them only where they are denied
/// (Replace mode); see [`native_baseline_tokens_per_turn`].
pub const NATIVE_BASELINE_TOKENS_PER_TURN: u64 = 2400;

/// The native prefix lean-ctx's own prefix replaces in the active hook mode.
/// Only Replace denies the native tools; in Hybrid and MCP mode they stay in
/// the host's tool list and lean-ctx's prefix adds to theirs, so nothing is
/// subtracted.
#[must_use]
pub fn native_baseline_tokens_per_turn() -> u64 {
    if crate::hooks::replace_mode_chosen() {
        NATIVE_BASELINE_TOKENS_PER_TURN
    } else {
        0
    }
}

/// Price of a cached prefix token relative to base input: a cache hit reads at
/// 0.1× and a miss writes at 1.25× (5-minute TTL) on Anthropic and on OpenAI
/// GPT-5.6+. Not modelled: cheaper reads on some models (0.05× on Claude Opus
/// and Sonnet 5.5) and longer TTLs (Anthropic 1 h at 2×, OpenAI 12 h at 2.5×).
const CACHE_READ_MULTIPLIER: f64 = 0.1;
const CACHE_WRITE_MULTIPLIER: f64 = 1.25;

/// Share of turns whose prefix the provider serves from its cache. An estimate
/// (default 75%), set with `dashboard_cache_hit_rate`.
#[must_use]
pub fn provider_cache_hit_rate() -> f64 {
    crate::core::config::Config::load()
        .dashboard_cache_hit_rate()
        .unwrap_or(0.75)
}

/// What one prefix token costs per turn, in base-input tokens: hits read at
/// `CACHE_READ_MULTIPLIER`, misses write at `CACHE_WRITE_MULTIPLIER`.
/// `--no-cache-adjust` prices a provider without caching (1.0, #1104).
#[must_use]
pub fn prefix_cost_factor() -> f64 {
    if no_cache_adjust_active() {
        return 1.0;
    }
    cost_factor_at(provider_cache_hit_rate())
}

fn cost_factor_at(hit_rate: f64) -> f64 {
    let hit = hit_rate.clamp(0.0, 1.0);
    hit * CACHE_READ_MULTIPLIER + (1.0 - hit) * CACHE_WRITE_MULTIPLIER
}

// #1104: `--no-cache-adjust` prices the prefix without caching (worst-case view).
std::thread_local! {
    static NO_CACHE_ADJUST: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

pub fn set_no_cache_adjust(v: bool) {
    NO_CACHE_ADJUST.with(|c| c.set(v));
}

fn no_cache_adjust_active() -> bool {
    NO_CACHE_ADJUST.with(std::cell::Cell::get)
}

/// Net-of-injection reconciliation with baseline + cache corrections (#1104).
///
/// Two corrections over the original `saved - overhead × turns`:
/// 1. **Baseline**: in Replace mode the denied native tools' ~2,400 tok/turn of
///    schemas leave the prefix, so only the delta above them is lean-ctx's.
/// 2. **Cache**: the prefix bills at [`prefix_cost_factor`] per turn — cache
///    reads are cheap, not free, and misses carry the write premium.
#[must_use]
pub fn net_of_injection(tokens_saved: u64, overhead_per_turn: u64, turns: u64) -> (u64, i64) {
    net_of_injection_with(
        tokens_saved,
        overhead_per_turn,
        turns,
        native_baseline_tokens_per_turn(),
        prefix_cost_factor(),
    )
}

fn net_of_injection_with(
    tokens_saved: u64,
    overhead_per_turn: u64,
    turns: u64,
    native_baseline: u64,
    cost_factor: f64,
) -> (u64, i64) {
    let delta = overhead_per_turn.saturating_sub(native_baseline);
    let effective_per_turn = (delta as f64 * cost_factor) as u64;
    let total = effective_per_turn.saturating_mul(turns);
    let net = tokens_saved as i64 - total as i64;
    (total, net)
}

/// Provider turns (requests) the proxy actually observed carrying the injected
/// prefix. The proxy is the only component that sees every provider turn, so its
/// persisted request count is the honest multiplier for the per-turn injection
/// tax. `0` when the proxy is not in the request path — we never guess turns we
/// did not see, so [`net_of_injection`] then collapses to the gross savings.
#[must_use]
pub fn observed_turns() -> u64 {
    crate::core::proxy_metrics::load_persisted().map_or(0, |m| m.requests_total)
}

#[cfg(test)]
pub mod tests {
    use super::*;

    #[test]
    fn measure_reports_nonzero_components() {
        // Isolated (default) config: shared rules injection, no pinned profile —
        // every component carries tokens. `isolated_data_dir` also holds the env
        // lock, so a concurrent test toggling the knobs can't perturb this.
        let _iso = crate::core::data_dir::isolated_data_dir();
        let o = ContextOverhead::measure();
        assert!(o.tool_count > 0, "must expose at least one tool");
        assert!(o.tool_schema_tokens > 0, "tool schemas carry tokens");
        assert!(o.instruction_tokens > 0, "instructions carry tokens");
        assert!(o.rules_block_tokens > 0, "rules block carries tokens");
        assert_eq!(
            o.total_tokens(),
            o.tool_schema_tokens + o.instruction_tokens + o.rules_block_tokens
        );
    }

    #[test]
    fn total_is_sum_of_parts() {
        let o = ContextOverhead {
            tool_count: 10,
            tool_schema_tokens: 100,
            instruction_tokens: 200,
            rules_block_tokens: 50,
        };
        assert_eq!(o.total_tokens(), 350);
    }

    #[test]
    fn rules_injection_off_zeroes_the_rules_block() {
        // With rules injection off, no rules file is written, so the per-turn
        // overhead must not count the rules block (#361). The tool/instruction
        // surface is unaffected. `isolated_data_dir` holds the env lock.
        let _iso = crate::core::data_dir::isolated_data_dir();
        let on = ContextOverhead::measure();
        crate::test_env::set_var("LEAN_CTX_RULES_INJECTION", "off");
        let off = ContextOverhead::measure();
        crate::test_env::remove_var("LEAN_CTX_RULES_INJECTION");

        assert!(on.rules_block_tokens > 0, "default still injects rules");
        assert_eq!(off.rules_block_tokens, 0, "off must drop the rules block");
        assert_eq!(
            off.total_tokens(),
            off.tool_schema_tokens + off.instruction_tokens,
            "off total excludes the rules block"
        );
    }

    #[test]
    fn minimal_arm_per_turn_prefix_stays_within_budget() {
        // The "faithful arm" (#361): tool_profile=minimal (5 tools) +
        // LEAN_CTX_MINIMAL (no session/knowledge prefix) + rules_injection=off
        // (no rules block) must keep the fixed per-turn prefix tiny. This is the
        // regression guard for the "~3K tokens/turn injected" critique — if any
        // knob silently stops applying, the total balloons and this fails.
        // macOS/Linux baseline is ~1829 after three reviewed additions: the v3
        // agent-loop + navigation-paradox one-liner (#609, always-on in the COMPACT
        // skeleton), the ctx_search `handle` param (#608), and the proactive
        // `RECOVER` recovery one-liner + the `ctx_read` `raw` schema param
        // (premium-recovery-layer) — together ~+39 tok over the prior ~1790. Windows
        // additionally carries `build_shell_hint()` — a ~5-tok PowerShell-cmdlet
        // warning that is empty on POSIX. The budget covers that surface plus a small
        // margin for `shell_name()` variance (#1051), and still sits ~1.1K under the
        // ~3K balloon this guard exists to catch — it is not a license for silent creep.
        // 3.12.0: +5 for the `ctx_read` `raw` note "redaction still applies" (#2139), a
        // security statement agents must see; the prefix measured 1966 on the largest platform.
        const MINIMAL_ARM_PREFIX_BUDGET_TOKENS: usize = 1970;

        let _iso = crate::core::data_dir::isolated_data_dir();
        crate::test_env::set_var("LEAN_CTX_TOOL_PROFILE", "minimal");
        crate::test_env::set_var("LEAN_CTX_MINIMAL", "1");
        crate::test_env::set_var("LEAN_CTX_RULES_INJECTION", "off");
        let o = ContextOverhead::measure();
        crate::test_env::remove_var("LEAN_CTX_TOOL_PROFILE");
        crate::test_env::remove_var("LEAN_CTX_MINIMAL");
        crate::test_env::remove_var("LEAN_CTX_RULES_INJECTION");

        assert_eq!(
            o.rules_block_tokens, 0,
            "rules_injection=off must zero the rules block"
        );
        assert!(
            o.tool_count <= crate::core::tool_profiles::ToolProfile::Minimal.tool_count() + 1,
            "minimal profile must keep the surface lean, got {} tools (expected ≤ {})",
            o.tool_count,
            crate::core::tool_profiles::ToolProfile::Minimal.tool_count() + 1,
        );
        // #1651: enforce the largest platform, not the running one. The shell
        // hint is empty on POSIX and not on Windows, so measuring only where
        // you happen to sit answers a different question than CI asks. #1646
        // spent three CI round-trips on an edit that measured 1953 on macOS and
        // failed at 1968 on Windows; the CHANGELOG records the same trap in
        // #1625. A local run now reports the number CI will enforce.
        let windows_only = if cfg!(windows) {
            0
        } else {
            crate::instructions::max_shell_hint_tokens()
        };
        let worst_case = o.total_tokens() + windows_only;

        assert!(
            worst_case <= MINIMAL_ARM_PREFIX_BUDGET_TOKENS,
            "minimal-arm per-turn prefix = {worst_case} tok on the largest platform \
             (here {} = schemas {} + instr {} + rules {}, plus {windows_only} for the \
             Windows-only shell hint), budget {}.\n\
             Editing a `tool_def` description also changes \
             docs/reference/generated/mcp-tools.md — re-run \
             `cargo run --example gen_docs --features dev-tools` or the Documentation \
             job fails too. Neither consequence is visible from the diff.",
            o.total_tokens(),
            o.tool_schema_tokens,
            o.instruction_tokens,
            o.rules_block_tokens,
            MINIMAL_ARM_PREFIX_BUDGET_TOKENS,
        );
    }

    #[test]
    fn default_arm_per_turn_prefix_stays_within_budget_per_client() {
        // The arm users actually get: no pinned profile (lazy core), default
        // instructions, rules injection on. The minimal-arm guard above never
        // saw this surface, so the default could grow unnoticed (Optimierungsplan
        // P1). Budgets are the measured 3.12.0 baseline plus a small margin, on
        // the largest platform; lowering them is the goal, raising them needs a
        // reason in the commit.
        // 3.12.0 baseline: claude-code 2969, codex 2770, cursor 3248 (12 tools,
        // schemas 2236, rules 248; instructions 468 / 269 / 747), measured
        // against the developer's real HOME. A3 (one rules channel, each client
        // with its `setup` carrier in a temp HOME, the anchor path, no per-turn
        // solution ladder): claude-code 2548, codex 2548, cursor 2499
        // (instructions 112 / 112 / 63, rules 183).
        const BUDGETS: &[(&str, usize)] = &[
            ("claude-code", 2560),
            ("codex-mcp-client", 2560),
            ("cursor-vscode", 2510),
        ];

        let iso = crate::core::data_dir::isolated_data_dir();
        // A3: what a host is billed depends on its installed rules carrier —
        // with the carrier holding the block the instructions collapse to an
        // anchor (one rules channel). Measuring against the developer's real
        // HOME made the numbers depend on that machine; each client now gets a
        // temp HOME with exactly the carrier `setup` writes for it.
        let saved: Vec<_> = ["HOME", "CLAUDE_CONFIG_DIR", "CODEX_HOME"]
            .iter()
            .map(|k| (*k, std::env::var_os(k)))
            .collect();
        crate::test_env::remove_var("CLAUDE_CONFIG_DIR");
        crate::test_env::remove_var("CODEX_HOME");
        let install_carrier = |client: &str, home: &std::path::Path| {
            let target = match client {
                "claude-code" => {
                    crate::hooks::agents::sync_claude_global_rules_block(
                        home,
                        crate::hooks::recommend_hook_mode("claude"),
                    )
                    .expect("claude block");
                    return;
                }
                "codex-mcp-client" => ("Codex CLI", ".codex/instructions.md"),
                _ => ("Cursor", ".cursor/rules/lean-ctx.mdc"),
            };
            let path = home.join(target.1);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            let block = crate::rules_inject::expected_blocks_by_target(home)
                .remove(target.0)
                .unwrap();
            std::fs::write(path, block).unwrap();
        };
        let windows_only = if cfg!(windows) {
            0
        } else {
            crate::instructions::max_shell_hint_tokens()
        };
        let over: Vec<String> = BUDGETS
            .iter()
            .filter_map(|&(client, budget)| {
                let home = iso.path().join(format!("home-{client}"));
                std::fs::create_dir_all(&home).unwrap();
                crate::test_env::set_var("HOME", &home);
                install_carrier(client, &home);
                let o = ContextOverhead::measure_for_client(client);
                let worst_case = o.total_tokens() + windows_only;
                eprintln!(
                    "{client}: {worst_case} tok (schemas {} + instr {} + rules {} + {windows_only})",
                    o.tool_schema_tokens, o.instruction_tokens, o.rules_block_tokens
                );
                (worst_case > budget).then(|| {
                    format!(
                        "{client}: {worst_case} tok on the largest platform ({} tools: schemas {} \
                         + instr {} + rules {} + {windows_only} Windows-only), budget {budget}",
                        o.tool_count,
                        o.tool_schema_tokens,
                        o.instruction_tokens,
                        o.rules_block_tokens,
                    )
                })
            })
            .collect();
        for (k, v) in saved {
            match v {
                Some(v) => crate::test_env::set_var(k, v),
                None => crate::test_env::remove_var(k),
            }
        }
        assert!(
            over.is_empty(),
            "default-arm per-turn prefix over budget:\n{}",
            over.join("\n")
        );
    }

    #[test]
    fn governor_arm_per_turn_prefix_stays_within_budget() {
        // Governor design v1 §3: the opt-in Claude Code governor wins on short
        // tasks only by costing ~0, so its prefix is guarded like the default
        // arm above. Measured at introduction (macOS): governor (no MCP) 86 tok
        // (rules only), governor-mcp 201 tok (12 deferred names 121 + rules 80),
        // against 2952 for the default claude-code arm. The budgets add a small
        // margin; raising them needs a reason in the commit.
        use crate::hooks::agents::claude_governor::GovernorArm;
        const BUDGETS: &[(GovernorArm, usize)] =
            &[(GovernorArm::NoMcp, 100), (GovernorArm::McpDeferred, 220)];
        let _iso = crate::core::data_dir::isolated_data_dir();
        let default_claude = ContextOverhead::measure_for_client("claude-code").total_tokens();
        for &(arm, budget) in BUDGETS {
            let o = ContextOverhead::measure_governor(arm);
            assert_eq!(o.instruction_tokens, 0, "{arm:?}: no instructions");
            assert!(
                o.total_tokens() <= budget,
                "{arm:?}: governor prefix {} tok (schemas/names {} + rules {}), budget {budget}",
                o.total_tokens(),
                o.tool_schema_tokens,
                o.rules_block_tokens,
            );
            assert!(
                o.total_tokens() * 10 < default_claude,
                "{arm:?} ≪ default arm"
            );
        }
        assert_eq!(
            ContextOverhead::measure_governor(GovernorArm::NoMcp).tool_count,
            0
        );
    }

    #[test]
    fn cached_prefix_is_cheap_not_free_and_misses_pay_the_write_premium() {
        // 75% hits: 0.75 × 0.1 + 0.25 × 1.25 = 0.3875 per prefix token.
        assert!((cost_factor_at(0.75) - 0.3875).abs() < 1e-12);
        assert!(cost_factor_at(1.0) > 0.0, "a warm cache still bills reads");
        assert!(cost_factor_at(0.0) > 1.0, "a cold cache bills the write");
        // Replace: 3400 − 2400 = 1000 delta → 387/turn → 8 turns = 3096.
        assert_eq!(
            net_of_injection_with(10_000, 3400, 8, NATIVE_BASELINE_TOKENS_PER_TURN, 0.3875),
            (3096, 6904)
        );
        // Hybrid/MCP keep the native tools: the whole prefix is lean-ctx's.
        assert_eq!(
            net_of_injection_with(10_000, 3400, 8, 0, 0.3875),
            (10_536, -536)
        );
    }

    #[test]
    fn only_replace_mode_subtracts_the_native_baseline() {
        let _iso = crate::core::data_dir::isolated_data_dir();
        let mut seen = Vec::new();
        for mode in ["", "hybrid", "mcp", "replace"] {
            if mode.is_empty() {
                crate::test_env::remove_var("LEAN_CTX_HOOK_MODE");
            } else {
                crate::test_env::set_var("LEAN_CTX_HOOK_MODE", mode);
            }
            seen.push((mode, native_baseline_tokens_per_turn()));
        }
        crate::test_env::remove_var("LEAN_CTX_HOOK_MODE");
        assert_eq!(
            seen,
            [
                ("", 0),
                ("hybrid", 0),
                ("mcp", 0),
                ("replace", NATIVE_BASELINE_TOKENS_PER_TURN)
            ]
        );
    }

    #[test]
    fn net_of_injection_collapses_to_gross_without_proxy_turns() {
        assert_eq!(net_of_injection(1234, 3000, 0), (0, 1234));
    }

    /// `PROACTIVE_INJECTED_TOKENS` is a single process-global counter, so the
    /// two tests below raced under `cargo test`'s parallelism: one reset and
    /// added while the other reset and read, and the reader observed this
    /// module's own `17 + 5`. The failure value was literally `22`, which is
    /// what made the source unambiguous.
    ///
    /// A lock scoped to this pair is the right size for the problem —
    /// `test_env_lock` would serialize them against every env-mutating test in
    /// the suite to protect one `AtomicUsize`.
    static COUNTER_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn proactive_injection_counter_accumulates() {
        let _guard = COUNTER_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        reset_proactive_injection();
        record_proactive_injection(17);
        record_proactive_injection(5);
        assert_eq!(proactive_injected_tokens(), 22);
    }

    #[test]
    fn proactive_injection_counter_starts_at_zero_after_reset() {
        let _guard = COUNTER_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        reset_proactive_injection();
        assert_eq!(proactive_injected_tokens(), 0);
    }
}
