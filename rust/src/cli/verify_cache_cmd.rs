//! `lean-ctx verify-cache` — a first-class, one-command proof that the session
//! cache is engaged: it reads a file twice through the real `SessionCache` and
//! asserts the second (unchanged) read collapses to a ~13-token `[unchanged …]`
//! stub instead of re-sending the whole file.
//!
//! Born out of the independent tokbench benchmark (GH #361), where the reviewer
//! had to verify the ~13-token re-read *by hand*. This makes that check
//! reproducible for any user or reviewer, with a machine-checkable exit code.

use crate::core::cache::SessionCache;
use crate::tools::ctx_read;
use crate::tools::CrpMode;

/// Tokens below which a re-read counts as a cache "stub". The marker itself is
/// ~13 tokens; the ceiling is generous to absorb long paths / file-ref labels
/// while staying far under any real file's full payload.
const STUB_MAX_TOKENS: usize = 64;

/// Exit codes: 0 = cache proven, 1 = expected stub but got full content,
/// 2 = stubbing disabled by configuration (valid setup, but no re-read savings).
pub(crate) fn cmd_verify_cache(args: &[String]) -> i32 {
    let json = args.iter().any(|a| a == "--json");

    let (path, cleanup) = match resolve_target(args) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("verify-cache: {e}");
            return 1;
        }
    };

    let outcome = run_probe(&path);

    if let Some(probe) = cleanup {
        let _ = std::fs::remove_file(&probe);
    }

    if json {
        print_json(&path, &outcome);
    } else {
        print_human(&path, &outcome);
    }
    outcome.exit_code()
}

/// What the two probe reads revealed about the cache.
struct ProbeOutcome {
    /// Tokens emitted by the first (full) read.
    first_tokens: usize,
    /// Tokens emitted by the second (re-)read — the stub on a working cache.
    second_tokens: usize,
    /// True when the second read returned an `[unchanged …]` / cached stub.
    is_stub: bool,
    /// False when config/policy forces full content on every read.
    stub_enabled: bool,
    /// Human-readable reason stubbing is off (only when `!stub_enabled`).
    disabled_reason: Option<String>,
    /// Cache policy in effect (e.g. `default` / `safe`).
    policy: String,
    /// Cache hits / total reads observed during this run.
    run_hits: u64,
    run_reads: u64,
    /// Persistent CEP session count + cross-call hit ratio (context, not asserted).
    cep_sessions: u64,
    cep_hit_ratio: f64,
}

impl ProbeOutcome {
    fn passed(&self) -> bool {
        self.stub_enabled
            && self.is_stub
            && self.second_tokens <= STUB_MAX_TOKENS
            && self.second_tokens < self.first_tokens
    }

    fn exit_code(&self) -> i32 {
        match (self.stub_enabled, self.passed()) {
            (false, _) => 2,
            (true, true) => 0,
            (true, false) => 1,
        }
    }

    fn saved_pct(&self) -> f64 {
        if self.first_tokens > 0 {
            (1.0 - (self.second_tokens as f64 / self.first_tokens as f64)) * 100.0
        } else {
            0.0
        }
    }
}

/// Run the real two-read probe against `path` using an in-process cache — the
/// same `SessionCache` + `ctx_read` path the Pi bridge / daemon serve.
fn run_probe(path: &str) -> ProbeOutcome {
    let crp = CrpMode::effective();
    let mut cache = SessionCache::new();

    // Mirror the stub gates the *registered* ctx_read handler honours, so a
    // config that suppresses stubbing is reported as such instead of as a
    // confusing failure. Two independent gates disable re-read stubs:
    //   * cache_policy `safe` (delivers a map) or `off` (the registered handler
    //     forces `fresh=true`, always re-reading from disk), and
    //   * forced full reads (no_degrade, or read.default_mode=full + crp=off).
    let no_degrade = crate::core::config::Config::load().no_degrade_effective();
    let prof = crate::core::profiles::active_profile();
    let force_full = no_degrade
        || (prof.read.default_mode_effective() == "full"
            && prof.compression.crp_mode_effective() == "off");
    let policy = crate::server::compaction_sync::effective_cache_policy();
    let stub_enabled = policy != "safe" && policy != "off" && !force_full;
    let disabled_reason = if stub_enabled {
        None
    } else if policy == "safe" {
        Some("cache policy is 'safe' (stubbing disabled)".to_string())
    } else if policy == "off" {
        Some("cache policy is 'off' (every read goes to disk)".to_string())
    } else if no_degrade {
        Some("no_degrade is enabled (full content forced)".to_string())
    } else {
        Some("active profile forces full reads (read.default_mode=full, crp=off)".to_string())
    };

    let first = ctx_read::handle_with_task_resolved(&mut cache, path, "full", crp, None);
    let second = ctx_read::handle_with_task_resolved(&mut cache, path, "full", crp, None);

    let is_stub = second.content.contains("[unchanged") || second.content.contains(" cached ");

    let stats = cache.get_stats();
    let run_reads = stats.total_reads();
    let run_hits = stats.cache_hits();

    let cep = crate::core::stats::load().cep;
    let cep_hit_ratio = if cep.total_cache_reads > 0 {
        (cep.total_cache_hits as f64 / cep.total_cache_reads as f64) * 100.0
    } else {
        0.0
    };

    ProbeOutcome {
        first_tokens: first.output_tokens,
        second_tokens: second.output_tokens,
        is_stub,
        stub_enabled,
        disabled_reason,
        policy: policy.to_string(),
        run_hits,
        run_reads,
        cep_sessions: cep.sessions,
        cep_hit_ratio,
    }
}

/// Resolve the file to probe: an explicit path argument if given and readable,
/// otherwise a synthetic probe file written to the temp dir (cleaned up after).
/// Returns `(path, Some(probe_to_delete))` when a synthetic file was created.
fn resolve_target(args: &[String]) -> Result<(String, Option<String>), String> {
    if let Some(explicit) = args.iter().find(|a| !a.starts_with('-')) {
        let p = std::path::Path::new(explicit);
        if p.is_file() {
            return Ok((explicit.clone(), None));
        }
        return Err(format!("'{explicit}' is not a readable file"));
    }

    let file_name = format!(".lean-ctx-verify-cache-{}.txt", std::process::id());
    let content = synthetic_probe_source();

    // Prefer the current directory (inside the project root) so the read does not
    // trip the defense-in-depth path-escape guard; fall back to the temp dir.
    let candidates = [
        std::env::current_dir().ok().map(|d| d.join(&file_name)),
        Some(std::env::temp_dir().join(&file_name)),
    ];
    for cand in candidates.into_iter().flatten() {
        if std::fs::write(&cand, &content).is_ok() {
            let path = cand.to_string_lossy().into_owned();
            return Ok((path.clone(), Some(path)));
        }
    }
    Err("could not create a probe file in the current or temp directory".to_string())
}

/// Deterministic, clearly-larger-than-a-stub source used when no path is given.
fn synthetic_probe_source() -> String {
    let mut s = String::from(
        "// lean-ctx verify-cache synthetic probe — safe to delete.\n\
         use std::collections::HashMap;\n\n",
    );
    for i in 0..24 {
        s.push_str(&format!(
            "/// Probe function number {i}, present so the full read is unambiguous.\n\
             pub fn probe_{i}(input: &str, factor: usize) -> HashMap<String, usize> {{\n    \
             let mut counts: HashMap<String, usize> = HashMap::new();\n    \
             for token in input.split_whitespace() {{\n        \
             *counts.entry(token.to_string()).or_insert(0) += factor + {i};\n    }}\n    \
             counts\n}}\n\n"
        ));
    }
    s
}

fn print_human(path: &str, o: &ProbeOutcome) {
    println!("lean-ctx verify-cache\n");
    println!("  Target:        {path}");
    println!("  Cache policy:  {}", o.policy);

    if !o.stub_enabled {
        let reason = o.disabled_reason.as_deref().unwrap_or("stubbing disabled");
        println!("\n  WARN — re-read stubbing is OFF: {reason}.");
        println!(
            "  The cache works, but unchanged re-reads will re-send full content.\n  \
             Re-enable savings, then re-run: lean-ctx config set cache_policy default"
        );
        return;
    }

    println!("  Read #1 (full):     {} tokens", o.first_tokens);
    println!(
        "  Read #2 (re-read):  {} tokens  {}",
        o.second_tokens,
        if o.is_stub {
            "[unchanged stub]"
        } else {
            "[full — NOT cached]"
        }
    );
    println!("  Re-read savings:    {:.0}%", o.saved_pct());
    println!("  Cache hits (run):   {}/{}", o.run_hits, o.run_reads);
    println!(
        "  CEP sessions:       {} ({:.0}% cross-call hit ratio)",
        o.cep_sessions, o.cep_hit_ratio
    );

    if o.passed() {
        println!(
            "\n  PASS — session cache engaged: the unchanged re-read cost {} tokens \
             (≈13-token stub).",
            o.second_tokens
        );
    } else {
        println!(
            "\n  FAIL — expected a ~13-token stub on re-read, got {} tokens. The session \
             cache is not collapsing unchanged re-reads.",
            o.second_tokens
        );
        println!(
            "  On Pi, ensure the embedded MCP bridge is on (LEAN_CTX_PI_ENABLE_MCP=1) so reads \
             share one cache."
        );
    }
}

fn print_json(path: &str, o: &ProbeOutcome) {
    let status = if !o.stub_enabled {
        "stubbing_disabled"
    } else if o.passed() {
        "pass"
    } else {
        "fail"
    };
    let json = serde_json::json!({
        "status": status,
        "target": path,
        "cache_policy": o.policy,
        "stub_enabled": o.stub_enabled,
        "disabled_reason": o.disabled_reason,
        "first_read_tokens": o.first_tokens,
        "second_read_tokens": o.second_tokens,
        "second_read_is_stub": o.is_stub,
        "stub_threshold_tokens": STUB_MAX_TOKENS,
        "reread_saved_pct": (o.saved_pct() * 10.0).round() / 10.0,
        "run_cache_hits": o.run_hits,
        "run_cache_reads": o.run_reads,
        "cep_sessions": o.cep_sessions,
        "cep_hit_ratio_pct": (o.cep_hit_ratio * 10.0).round() / 10.0,
    });
    println!(
        "{}",
        serde_json::to_string_pretty(&json).unwrap_or_default()
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn outcome(first: usize, second: usize, is_stub: bool, stub_enabled: bool) -> ProbeOutcome {
        ProbeOutcome {
            first_tokens: first,
            second_tokens: second,
            is_stub,
            stub_enabled,
            disabled_reason: if stub_enabled { None } else { Some("x".into()) },
            policy: "default".into(),
            run_hits: 1,
            run_reads: 2,
            cep_sessions: 0,
            cep_hit_ratio: 0.0,
        }
    }

    #[test]
    fn pass_when_second_read_is_small_stub() {
        let o = outcome(2000, 13, true, true);
        assert!(o.passed());
        assert_eq!(o.exit_code(), 0);
    }

    #[test]
    fn fail_when_second_read_not_stubbed() {
        let o = outcome(2000, 1900, false, true);
        assert!(!o.passed());
        assert_eq!(o.exit_code(), 1);
    }

    #[test]
    fn fail_when_marker_present_but_above_stub_ceiling() {
        // Stub marker present, but payload above the ceiling ⇒ not a real stub.
        let o = outcome(2000, 200, true, true);
        assert!(!o.passed());
        assert_eq!(o.exit_code(), 1);
    }

    #[test]
    fn warn_exit_when_stubbing_disabled_by_config() {
        let o = outcome(2000, 2000, false, false);
        assert!(!o.passed());
        assert_eq!(o.exit_code(), 2);
    }

    #[test]
    fn saved_pct_reflects_collapse() {
        assert!(outcome(1000, 10, true, true).saved_pct() > 98.0);
        assert_eq!(outcome(0, 0, true, true).saved_pct(), 0.0);
    }
}
