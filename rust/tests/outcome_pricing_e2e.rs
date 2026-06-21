//! End-to-end "durchspielbar" proof for **Outcome-Based Pricing** (Epic #671).
//!
//! This is the open-engine half of the success-fee golden path, exercised with
//! REAL data — there are no fabricated token counts or savings anywhere:
//!
//! 1. real shell-output compression (`shell::compress`, the exact path
//!    `lean-ctx -c` runs), measured with the real tokenizer;
//! 2. recorded into the append-only, SHA-256 hash-chained **savings ledger**;
//! 3. exported as an Ed25519 **signed batch** and verified **offline** (plus a
//!    tamper check that must break the signature);
//! 4. metered into a **billable usage** record (`is_billable = signed &&
//!    chain_valid`);
//! 5. rendered as a **FOCUS** `FinOps` export carrying the savings as a Credit row.
//!
//! The private control-plane (`lean-ctx-cloud`) consumes the verified total from
//! steps 4/5 to raise the Stripe success-fee invoice. That commercial half is
//! proven in the cloud repo and is deliberately absent here
//! (`oss-plane-separation-v1`).

use lean_ctx::core::billing::metering::metered_usage;
use lean_ctx::core::finops_export::{self, DateRange};
use lean_ctx::core::savings_ledger::{self, SignedSavingsBatchV1, signed_batch};
use lean_ctx::core::tokens::count_tokens;
use lean_ctx::shell::compress::compress_if_beneficial_pub;

const AGENT_ID: &str = "pilot-e2e";

/// A realistic, reliably-compressible `git status` — the exact thing a coding
/// agent runs constantly. Built from real git status lines so the saving is a
/// genuine measurement of the engine compressor, never a hand-picked number.
fn realistic_git_status(modified: usize, untracked: usize) -> String {
    let mut out = String::from(
        "On branch main\nYour branch is up to date with 'origin/main'.\n\n\
         Changes not staged for commit:\n  \
         (use \"git add <file>...\" to update what will be committed)\n  \
         (use \"git restore <file>...\" to discard changes in working directory)\n",
    );
    for i in 0..modified {
        out.push_str(&format!("\tmodified:   rust/src/core/module_{i:02}.rs\n"));
    }
    out.push_str(
        "\nUntracked files:\n  (use \"git add <file>...\" to include in what will be committed)\n",
    );
    for i in 0..untracked {
        out.push_str(&format!("\trust/src/core/new_module_{i:02}.rs\n"));
    }
    out.push_str("\nno changes added to commit (use \"git add\" and/or \"git commit -a\")\n");
    out
}

/// Record one measured shell-compression event. Returns `(baseline, actual)`
/// token counts so the caller can assert a real saving occurred.
fn record_real_compression(command: &str, raw: &str) -> (usize, usize) {
    let compressed = compress_if_beneficial_pub(command, raw);
    let baseline = count_tokens(raw);
    let actual = count_tokens(&compressed);
    savings_ledger::record_tool_event("cli_shell", baseline, actual);
    (baseline, actual)
}

#[test]
fn outcome_pricing_golden_path_is_real_and_billable() {
    // Isolated data dir → the savings ledger AND the Ed25519 keystore both live
    // in a throwaway temp dir (`agent_identity::key_path` resolves via the data
    // dir), so signing is real but touches nothing on the developer machine.
    let tmp = std::env::temp_dir().join(format!("lctx-outcome-e2e-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).expect("mkdir temp data dir");

    // SAFETY: set before any cached resolver runs; this is a single-threaded
    // integration-test binary and the env is torn down at the end.
    unsafe {
        std::env::set_var("LEAN_CTX_DATA_DIR", &tmp);
        std::env::set_var("LEAN_CTX_AGENT_ID", AGENT_ID);
        std::env::set_var("LEAN_CTX_MODEL", "gpt-4o");
        std::env::set_var("LEAN_CTX_SAVINGS_LEDGER", "on");
    }

    // ── 1 + 2. Real compression → measured savings → signed hash chain ────────
    let (b1, a1) = record_real_compression("git status", &realistic_git_status(40, 25));
    let (b2, a2) = record_real_compression("git status", &realistic_git_status(18, 9));
    assert!(
        b1 > a1,
        "git status must compress (baseline {b1} > actual {a1})"
    );
    assert!(
        b2 > a2,
        "git status must compress (baseline {b2} > actual {a2})"
    );

    let summary = savings_ledger::summary();
    assert!(summary.total_events >= 2, "two measured events recorded");
    assert!(
        summary.net_saved_tokens() > 0,
        "real net token savings recorded"
    );
    assert!(
        summary.saved_usd > 0.0,
        "savings carry a USD value at the pinned model price"
    );

    // Chain integrity — the tamper-evidence the whole meter rests on.
    assert!(
        savings_ledger::verify().valid,
        "SHA-256 chain verifies intact"
    );

    // ── 3. Signed batch + OFFLINE verification + tamper check ─────────────────
    let mut batch = SignedSavingsBatchV1::build_all(AGENT_ID);
    batch.sign(AGENT_ID).expect("sign with machine identity");
    let artifact = tmp.join("signed-batch.json");
    signed_batch::write_artifact(&batch, &artifact).expect("write artifact");

    let loaded = signed_batch::load_artifact(&artifact).expect("load artifact");
    assert!(
        loaded.verify().signature_valid,
        "signed batch verifies offline, without the raw ledger"
    );
    assert_eq!(
        loaded.totals.net_saved_tokens,
        summary.net_saved_tokens(),
        "the signed totals commit the same savings the ledger reports"
    );

    // A single flipped byte in the totals must break the signature.
    let mut tampered = loaded.clone();
    tampered.totals.net_saved_tokens += 1;
    assert!(
        !tampered.verify().signature_valid,
        "tampering with the totals invalidates the signature"
    );

    // ── 4. Billable usage meter (is_billable = signed && chain_valid) ─────────
    let usage = metered_usage(AGENT_ID);
    assert!(
        usage.is_billable(),
        "usage is billable: {}",
        usage.headline()
    );
    assert!(usage.signed && usage.chain_valid);
    assert_eq!(
        usage.net_saved_tokens,
        summary.net_saved_tokens(),
        "the billed quantity is exactly the verified, signed savings"
    );

    // ── 5. FOCUS FinOps export — savings as a Credit row ──────────────────────
    let rows = finops_export::aggregate(&DateRange::default());
    assert!(!rows.is_empty(), "ledger aggregates into FOCUS rows");
    let csv = finops_export::focus::to_csv(&rows);
    assert!(
        csv.contains("Credit"),
        "FOCUS export carries a Credit (savings) row"
    );
    assert!(csv.contains("LeanCTX"), "provider is attributed");
    // Credit rows put the savings in BilledCost (column 0) as a negative value,
    // so exactly those rows — and not the header or Usage rows — begin with '-'.
    assert!(
        csv.lines().any(|l| l.starts_with('-')),
        "a credit row with negative BilledCost is present"
    );

    // ── teardown ──────────────────────────────────────────────────────────────
    unsafe {
        std::env::remove_var("LEAN_CTX_DATA_DIR");
        std::env::remove_var("LEAN_CTX_AGENT_ID");
        std::env::remove_var("LEAN_CTX_MODEL");
        std::env::remove_var("LEAN_CTX_SAVINGS_LEDGER");
    }
    let _ = std::fs::remove_dir_all(&tmp);
}
