//! Verified Savings Ledger (G1) — the per-event, auditable counterfactual store.
//!
//! Local-only and on by default (set `LEAN_CTX_SAVINGS_LEDGER=off` to disable). It never
//! leaves the machine; opt-in org roll-up + cryptographic signing are later phases. See
//! `docs/business/03-verified-savings-ledger.md`.

pub mod event;
pub mod push;
pub mod roi;
pub mod signed_batch;
pub mod store;

pub use event::SavingsEvent;
pub use roi::{RoiReport, roi_report};
pub use signed_batch::{BatchVerifyResult, SignedSavingsBatchV1};
pub use store::{LedgerSummary, VerifyResult};

use std::sync::OnceLock;

fn enabled() -> bool {
    enabled_from(std::env::var("LEAN_CTX_SAVINGS_LEDGER").ok().as_deref())
}

/// Pure opt-out logic (testable without mutating process env). Enabled unless explicitly
/// set to a falsy value.
fn enabled_from(value: Option<&str>) -> bool {
    match value {
        Some(v) => !matches!(
            v.trim().to_lowercase().as_str(),
            "off" | "0" | "false" | "no"
        ),
        None => true,
    }
}

/// Resolved (`model_key`, `input_price_per_m`) for this process. The active model is stable
/// within a process, so we resolve the pricing table once.
fn model_and_price() -> &'static (String, f64) {
    static CACHE: OnceLock<(String, f64)> = OnceLock::new();
    CACHE.get_or_init(|| {
        let resolved = std::env::var("LEAN_CTX_MODEL")
            .or_else(|_| std::env::var("LCTX_MODEL"))
            .ok()
            .filter(|s| !s.trim().is_empty())
            // No explicit model → value savings against the real model the proxy
            // measured most, instead of the blended fallback (cross-process hint
            // from `proxy_usage.json`). Falls back to blended when absent.
            .or_else(crate::proxy::usage_meter::persisted_dominant_model);
        let quote =
            crate::core::gain::model_pricing::ModelPricing::load().quote(resolved.as_deref());
        (quote.model_key, quote.cost.input_per_m)
    })
}

/// Privacy-preserving repo attribution: truncated SHA-256 of the process working
/// directory. Never the file path or contents. Process-scoped (cached once).
fn repo_hash() -> &'static str {
    static CACHE: OnceLock<String> = OnceLock::new();
    CACHE.get_or_init(|| {
        use sha2::{Digest, Sha256};
        let cwd = std::env::current_dir()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default();
        let mut hasher = Sha256::new();
        hasher.update(cwd.as_bytes());
        let hex = crate::core::agent_identity::hex_encode(&hasher.finalize());
        hex.get(..16).unwrap_or(&hex).to_string()
    })
}

fn agent_id() -> &'static str {
    crate::core::agent_identity::current_agent_id()
}

/// The tokenizer family the **ledger** denominates savings in: the active
/// model's own family, so recorded tokens (and the USD derived from them) match
/// the units the provider actually bills (#685). Resolved once per process from
/// the same model `model_and_price` resolves — for the default `O200kBase` model
/// (OpenAI/Cursor/unknown) this is `o200k_base`, so the common path is unchanged.
pub(crate) fn ledger_family() -> crate::core::tokens::TokenizerFamily {
    static CACHE: OnceLock<crate::core::tokens::TokenizerFamily> = OnceLock::new();
    *CACHE.get_or_init(|| crate::core::tokens::detect_tokenizer(&model_and_price().0))
}

/// Count `text` in the ledger's tokenizer family (the active model's own) so
/// recorded savings are model-correct (#685). For the default `O200kBase` model
/// this is byte-identical to [`crate::core::tokens::count_tokens`] — same BPE,
/// same cache key — so the common path keeps its exact o200k numbers at zero
/// extra cost; only a resolved Claude/Gemini/Llama model triggers re-tokenizing.
///
/// NOTE: this is for the **internal ledger only**. Tool-output framing/footers
/// must stay on `count_tokens` (o200k) to keep outputs byte-stable for provider
/// prompt caching (#498) — do not route those through here.
#[must_use]
pub fn count_for_ledger(text: &str) -> usize {
    crate::core::tokens::count_tokens_for(text, ledger_family())
}

/// The tokenizer family that produced the token counts we record (G2). Resolved
/// once — now the active model's family (see [`ledger_family`]), so the ledger no
/// longer claims `o200k_base` for a Claude/Gemini run it measured differently.
fn tokenizer() -> &'static str {
    static CACHE: OnceLock<String> = OnceLock::new();
    CACHE.get_or_init(|| ledger_family().to_string())
}

/// Shared event skeleton with the per-process attribution + pricing context filled in.
/// Chain hashes are computed by `store::append`.
fn new_event(tool: &str) -> SavingsEvent {
    let (model_id, price_per_m) = model_and_price();
    SavingsEvent {
        ts: chrono::Utc::now().to_rfc3339(),
        tool: tool.to_string(),
        model_id: model_id.clone(),
        tokenizer: tokenizer().to_string(),
        baseline_tokens: 0,
        actual_tokens: 0,
        saved_tokens: 0,
        bounce_adjustment: 0,
        unit_price_per_m_usd: *price_per_m,
        saved_usd: 0.0,
        repo_hash: repo_hash().to_string(),
        agent_id: agent_id().to_string(),
        prev_hash: String::new(),
        entry_hash: String::new(),
    }
}

/// Best-effort append of one auditable savings event for a value-producing read.
/// Skips zero-saving events (keeps the ledger meaningful and cheap) and never panics.
pub fn record_read_event(original_tokens: usize, saved_tokens: usize) {
    record_tool_event(
        "ctx_read",
        original_tokens,
        original_tokens.saturating_sub(saved_tokens),
    );
}

/// Best-effort append of one auditable savings event for any non-read tool
/// (GL #479 D2: shell, grep/search, …). Callers MUST pass the **measured**
/// baseline — the raw tokens the uncompressed output would have cost — never a
/// counterfactual estimate (e.g. the search 2.5x factor stays out of here, so
/// `lean-ctx ledger verify` only ever attests measured numbers). Skips events
/// where compression saved nothing and never panics.
pub fn record_tool_event(tool: &str, baseline_tokens: usize, actual_tokens: usize) {
    let saved = baseline_tokens.saturating_sub(actual_tokens);
    if saved == 0 || !enabled() {
        return;
    }
    let Some(path) = store::default_path() else {
        return;
    };

    let mut event = new_event(tool);
    event.baseline_tokens = baseline_tokens as u64;
    event.actual_tokens = actual_tokens as u64;
    event.saved_tokens = saved as u64;
    event.saved_usd = saved as f64 / 1_000_000.0 * event.unit_price_per_m_usd;
    let _ = store::append(&path, event);
}

/// Best-effort append of a *bounce* event (G7): a compressed read later invalidated by a
/// full re-read, so the earlier saving was (partly) illusory. Recorded as a negative
/// adjustment with `tool = "bounce"` so totals net out without editing the original entry.
pub fn record_bounce_event(wasted_tokens: usize) {
    if wasted_tokens == 0 || !enabled() {
        return;
    }
    let Some(path) = store::default_path() else {
        return;
    };
    let wasted = wasted_tokens as u64;

    let mut event = new_event("bounce");
    event.baseline_tokens = wasted;
    event.actual_tokens = wasted;
    event.bounce_adjustment = wasted;
    event.saved_usd = -(wasted as f64 / 1_000_000.0 * event.unit_price_per_m_usd);
    let _ = store::append(&path, event);
}

/// Total bounce-adjusted tokens recorded, optionally limited to the last `days` (by event
/// timestamp). `None` = all time. Used to net the Wrapped headline per period.
#[must_use]
pub fn bounce_tokens(days: Option<u32>) -> u64 {
    let Some(path) = store::default_path() else {
        return 0;
    };
    store::bounce_tokens_since(&path, days)
}

/// Aggregated totals + model/day/tool slices over the whole ledger.
#[must_use]
pub fn summary() -> LedgerSummary {
    store::default_path()
        .map(|p| store::summarize(&p))
        .unwrap_or_default()
}

/// Per-day `(day, bounce_events, read_events)` for the last `days` days —
/// the dashboard's "is the system learning?" trend (#507).
#[must_use]
pub fn daily_bounce_trend(days: u32) -> Vec<(String, u64, u64)> {
    store::default_path()
        .map(|p| store::daily_bounce_trend(&p, days))
        .unwrap_or_default()
}

/// Re-walks the hash chain and reports whether it is intact.
pub fn verify() -> VerifyResult {
    store::default_path().map_or_else(VerifyResult::empty, |p| store::verify(&p))
}

/// Re-hashes the ledger under the current (v2) canonical scheme, repairing a chain broken by
/// the legacy float round-trip bug. Returns the number of re-chained events (0 if no ledger).
pub fn rechain() -> std::io::Result<usize> {
    match store::default_path() {
        Some(p) if p.exists() => store::rechain(&p),
        _ => Ok(0),
    }
}

/// Every recorded event (for `ledger export`).
#[must_use]
pub fn all_events() -> Vec<SavingsEvent> {
    store::default_path()
        .map(|p| store::load(&p))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opt_out_logic_is_correct() {
        assert!(enabled_from(None), "enabled by default when unset");
        assert!(enabled_from(Some("on")));
        assert!(enabled_from(Some("1")));
        assert!(!enabled_from(Some("off")));
        assert!(!enabled_from(Some("0")));
        assert!(!enabled_from(Some("false")));
        assert!(!enabled_from(Some(" No ")), "trim + case-insensitive");
    }

    #[test]
    fn repo_hash_is_truncated_hex() {
        let h = repo_hash();
        assert_eq!(h.len(), 16);
        assert!(h.chars().all(|c| c.is_ascii_hexdigit()));
    }

    /// #685: ledger counts are model-correct via a *real* BPE for the resolved
    /// family — never a fabricated scalar. Robust regardless of which model this
    /// process resolved to (the count always matches `count_tokens_for`).
    #[test]
    fn count_for_ledger_is_a_real_bpe_count_for_resolved_family() {
        let text = "fn honest_accounting(n: u64) -> u64 { n }";
        assert_eq!(
            count_for_ledger(text),
            crate::core::tokens::count_tokens_for(text, ledger_family())
        );
        assert!(count_for_ledger(text) > 0);
        assert_eq!(count_for_ledger(""), 0);
    }

    /// The honest tokenizer label the ledger stamps on every event matches the
    /// family its counts are denominated in — no more hardcoded `o200k_base` for
    /// a Claude/Gemini run (#685).
    #[test]
    fn tokenizer_label_matches_ledger_family() {
        assert_eq!(tokenizer(), ledger_family().to_string());
    }

    /// GL #479 D2: tool events must never panic and must skip the degenerate
    /// cases (no saving / inverted inputs) so the ledger only carries value.
    #[test]
    fn record_tool_event_skips_zero_and_inverted_savings() {
        // actual >= baseline → saved == 0 → no-op (must not panic or write).
        record_tool_event("cli_shell", 100, 100);
        record_tool_event("ctx_search", 50, 80);
        record_tool_event("cli_shell", 0, 0);
    }

    /// GL #479 D2 wiring proof: a measured shell/search saving lands in the
    /// ledger with the *raw* baseline and the right tool tag.
    #[test]
    fn record_tool_event_appends_measured_event() {
        let dir = std::env::temp_dir().join(format!("lctx-ledger-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("mkdir");
        crate::test_env::set_var("LEAN_CTX_DATA_DIR", dir.to_str().unwrap());

        record_tool_event("cli_shell", 5000, 800);

        let ledger = dir.join("savings").join("ledger.jsonl");
        let content = std::fs::read_to_string(&ledger).expect("ledger written");
        crate::test_env::remove_var("LEAN_CTX_DATA_DIR");
        let _ = std::fs::remove_dir_all(&dir);

        let last = content.lines().last().expect("one event");
        let ev: SavingsEvent = serde_json::from_str(last).expect("valid event JSON");
        assert_eq!(ev.tool, "cli_shell");
        assert_eq!(ev.baseline_tokens, 5000, "raw baseline, no estimate factor");
        assert_eq!(ev.actual_tokens, 800);
        assert_eq!(ev.saved_tokens, 4200);
    }
}
