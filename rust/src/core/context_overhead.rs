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

use crate::core::tokens::count_tokens;

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
        let tools = crate::server::tool_visibility::advertised_tool_defs_default();
        let tool_count = tools.len();
        let tool_schema_tokens = tools.iter().map(tool_tokens).sum();

        let instructions =
            crate::instructions::build_instructions(crate::tools::CrpMode::effective());
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

/// Pure net-of-injection reconciliation: the total injection tax
/// (`overhead_per_turn × turns`) and the signed net savings after subtracting
/// it. Lives here — the home of injection accounting — so both `lean-ctx gain`
/// and the verified savings ledger/ROI reconcile against the same math. The net
/// is signed because on a non-caching rail a short run can legitimately go
/// net-negative until savings outgrow the per-turn injection (#361, #685).
#[must_use]
pub fn net_of_injection(tokens_saved: u64, overhead_per_turn: u64, turns: u64) -> (u64, i64) {
    let total = overhead_per_turn.saturating_mul(turns);
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
    crate::proxy::metrics::load_persisted().map_or(0, |m| m.requests_total)
}

#[cfg(test)]
mod tests {
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
        // The "faithful arm" (#361): tool_profile=minimal (6 tools) +
        // LEAN_CTX_MINIMAL (no session/knowledge prefix) + rules_injection=off
        // (no rules block) must keep the fixed per-turn prefix tiny. This is the
        // regression guard for the "~3K tokens/turn injected" critique — if any
        // knob silently stops applying, the total balloons and this fails.
        const MINIMAL_ARM_PREFIX_BUDGET_TOKENS: usize = 1500;

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
            o.tool_count <= 8,
            "minimal profile must keep the surface lean, got {} tools",
            o.tool_count
        );
        assert!(
            o.total_tokens() <= MINIMAL_ARM_PREFIX_BUDGET_TOKENS,
            "minimal-arm per-turn prefix = {} tok (schemas {} + instr {} + rules {}), budget {}",
            o.total_tokens(),
            o.tool_schema_tokens,
            o.instruction_tokens,
            o.rules_block_tokens,
            MINIMAL_ARM_PREFIX_BUDGET_TOKENS,
        );
    }

    #[test]
    fn net_of_injection_subtracts_per_turn_tax() {
        // 1000 saved, 50/turn over 8 turns = 400 tax → net 600.
        assert_eq!(net_of_injection(1000, 50, 8), (400, 600));
    }

    #[test]
    fn net_of_injection_can_go_negative_on_short_runs() {
        // The honest case the report must not hide: gross < injection tax.
        assert_eq!(net_of_injection(100, 50, 8), (400, -300));
    }

    #[test]
    fn net_of_injection_collapses_to_gross_without_proxy_turns() {
        // No proxy in the path → no counted turns → net == gross.
        assert_eq!(net_of_injection(1234, 3000, 0), (0, 1234));
    }
}
