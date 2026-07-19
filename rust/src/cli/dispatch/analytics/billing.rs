//! `lean-ctx billing` — read-only commercial-plane reporting (EPIC 13.6):
//! plans, entitlements, metered usage. Never gates the local plane.

use super::savings::savings_agent_id;
use crate::core;

/// `lean-ctx billing <plans|entitlements|usage|settlement>` — read-only
/// commercial-plane reporting and offline settlement-evidence verification.
/// substrate (EPIC 13.6). All subcommands are **informational and read-only**:
/// they describe plans/entitlements and meter local savings. The local plane is
/// never gated — there are no entitlement checks here, only reporting.
///
/// Pricing, invoicing and Stripe live in the private control-plane
/// (`lean-ctx-cloud`), never in the open engine (see
/// `docs/contracts/oss-plane-separation-v1.md`).
pub(in crate::cli::dispatch) fn cmd_billing(rest: &[String]) {
    let action = rest.first().map_or("usage", String::as_str);
    let json = rest.iter().any(|a| a == "--json");
    match action {
        "status" => cmd_billing_status(json),
        "plans" => cmd_billing_plans(json),
        "entitlements" => cmd_billing_entitlements(rest.get(1).map(String::as_str), json),
        "usage" => cmd_billing_usage(json),
        "settlement" => cmd_billing_settlement(rest, json),
        other => {
            eprintln!(
                "unknown billing action '{other}'. Use: status | plans | entitlements <plan> | usage | settlement <verify|export> [--json]"
            );
            std::process::exit(1);
        }
    }
}

/// `lean-ctx billing status [--json]` — the at-a-glance commercial state for this
/// machine: the effective plan (with offline-grace provenance), the hosted
/// entitlements it grants, and the local ROI headline. Read-only; it best-effort
/// refreshes the plan from the backend and falls back to the cached-with-grace
/// plan when offline. Never gates anything local.
fn cmd_billing_status(json: bool) {
    use crate::cloud_client::PlanSource;
    let eff = crate::cloud_client::refresh_effective_plan();
    let logged_in = crate::cloud_client::is_logged_in();
    let e = eff.plan.entitlements();
    let roi = core::savings_ledger::roi_report(&savings_agent_id());

    if json {
        let payload = serde_json::json!({
            "plan": eff.plan.as_str(),
            "source": plan_source_label(eff.source),
            "verified_at": eff.verified_at,
            "grace_days": eff.grace_days,
            "logged_in": logged_in,
            "entitlements": e,
            "roi": {
                "net_saved_tokens": roi.net_saved_tokens,
                "saved_usd": roi.saved_usd,
                "total_events": roi.total_events,
                "chain_valid": roi.chain_valid,
                "signed": roi.signed,
            }
        });
        print_json_or_die(&payload, "billing status");
        return;
    }

    println!("lean-ctx billing status\n");
    println!(
        "  Plan:         {}  ({})",
        eff.plan.as_str(),
        plan_source_detail(&eff)
    );
    println!(
        "  Account:      {}",
        if logged_in {
            "logged in"
        } else {
            "not logged in (Free)"
        }
    );
    println!("  cloud_sync:   {}", yesno(e.cloud_sync));
    println!("  seats:        {}", quota(e.seats));
    println!(
        "  private_registry: {}   sso_oidc: {}   sso_scim: {}",
        e.private_registry, e.sso_oidc, e.sso_scim
    );
    println!();
    println!(
        "  ROI:          {} net tokens · ${:.2}  ({}, {})",
        roi.net_saved_tokens,
        roi.saved_usd,
        if roi.chain_valid {
            "chain valid"
        } else {
            "chain BROKEN"
        },
        if roi.signed { "signed" } else { "unsigned" }
    );
    println!("  Full report:  lean-ctx roi");
    println!();
    match eff.source {
        PlanSource::Expired => {
            println!("  ! Cached plan expired — reconnect: lean-ctx login, then lean-ctx sync");
        }
        PlanSource::None if !logged_in => {
            println!(
                "  Upgrade:      lean-ctx cloud upgrade   (Pro: hosted sync · Team: shared ROI rollup)"
            );
        }
        _ => println!("  Manage:       lean-ctx cloud upgrade"),
    }
}

/// Stable wire label for a [`crate::cloud_client::PlanSource`].
fn plan_source_label(source: crate::cloud_client::PlanSource) -> &'static str {
    use crate::cloud_client::PlanSource;
    match source {
        PlanSource::Live => "live",
        PlanSource::Cached => "cached",
        PlanSource::Expired => "expired",
        PlanSource::None => "none",
    }
}

/// Human provenance line: how fresh the plan is and how long the offline grace
/// keeps it valid.
fn plan_source_detail(eff: &crate::cloud_client::EffectivePlan) -> String {
    use crate::cloud_client::PlanSource;
    match eff.source {
        PlanSource::Live => "live".to_string(),
        PlanSource::Cached => match eff.verified_at {
            Some(v) => {
                let age_days = (chrono::Utc::now().timestamp() - v).max(0) / 86_400;
                let remaining = (eff.grace_days - age_days).max(0);
                format!("cached — verified {age_days}d ago, valid {remaining}d more")
            }
            None => "cached".to_string(),
        },
        PlanSource::Expired => "cached plan expired".to_string(),
        PlanSource::None => "no account".to_string(),
    }
}

fn yesno(b: bool) -> &'static str {
    if b { "yes" } else { "no" }
}

fn cmd_billing_plans(json: bool) {
    let plans: Vec<core::billing::Entitlements> = core::billing::Plan::all()
        .iter()
        .map(|p| p.entitlements())
        .collect();
    if json {
        print_json_or_die(&plans, "plans");
        return;
    }
    println!("lean-ctx plans (commercial plane — additive, never gates local):\n");
    for e in &plans {
        println!("  {} — seats: {}", e.plan.as_str(), quota(e.seats));
        println!(
            "    hosted_index_mb: {}  connectors: {}  private_registry: {}",
            quota(e.hosted_index_mb),
            quota(e.managed_connectors),
            e.private_registry
        );
        println!(
            "    sso_oidc: {}  sso_scim: {}  audit_retention_days: {}  revenue_share: {}  supporter: {}",
            e.sso_oidc, e.sso_scim, e.audit_retention_days, e.revenue_share, e.supporter
        );
    }
    println!("\nThe Personal plane (local engine) is free + ungated regardless of plan.");
}

fn cmd_billing_entitlements(plan_arg: Option<&str>, json: bool) {
    let plan = core::billing::Plan::parse(plan_arg.unwrap_or("free"));
    let e = plan.entitlements();
    if json {
        print_json_or_die(&e, "entitlements");
        return;
    }
    println!("Entitlements for plan '{}':", plan.as_str());
    println!("  seats:                {}", quota(e.seats));
    println!("  hosted_index_mb:      {}", quota(e.hosted_index_mb));
    println!("  managed_connectors:   {}", quota(e.managed_connectors));
    println!("  private_registry:     {}", e.private_registry);
    println!("  sso_oidc:             {}", e.sso_oidc);
    println!("  sso_scim:             {}", e.sso_scim);
    println!("  audit_retention_days: {}", e.audit_retention_days);
    println!("  revenue_share:        {}", e.revenue_share);
    println!("  supporter:            {}", e.supporter);
}

fn cmd_billing_usage(json: bool) {
    let agent_id = savings_agent_id();
    let usage = core::billing::metered_usage(&agent_id);
    if json {
        print_json_or_die(&usage, "usage");
        return;
    }
    println!("{}", usage.headline());
    println!();
    println!("  Period:        {}", usage.period);
    println!("  Metered events: {}", usage.metered_events);
    println!("  Net tokens:    {}", usage.net_saved_tokens);
    println!("  Saved USD:     ${:.4}", usage.saved_usd);
    println!(
        "  Source integrity: {}",
        if usage.source_integrity_verified() {
            "verified (signed + chain intact)"
        } else {
            "unverified (requires a signed, intact ledger)"
        }
    );
    println!("  Settlement:    not evaluated by billing-plane-v1 usage");
    println!("  Provenance:    {}", usage.last_entry_hash);
}

fn cmd_billing_settlement(rest: &[String], json: bool) {
    let action = rest.get(1).map_or("verify", String::as_str);
    let positional: Vec<&str> = rest
        .iter()
        .skip(2)
        .filter(|arg| !arg.starts_with("--"))
        .map(String::as_str)
        .collect();
    let Some(input) = positional.first() else {
        eprintln!(
            "Usage: lean-ctx billing settlement verify <manifest.json> <trust-store.json> [--json]\n       lean-ctx billing settlement export <manifest.json> <trust-store.json> <canonical.json> [--json]"
        );
        std::process::exit(2);
    };
    let manifest =
        match core::billing::SettlementEvidenceManifestV2::load(std::path::Path::new(input)) {
            Ok(manifest) => manifest,
            Err(error) => {
                eprintln!("settlement evidence load failed: {error}");
                std::process::exit(2);
            }
        };
    let Some(trust_path) = positional.get(1) else {
        eprintln!("settlement verification requires an out-of-band trust-store path");
        std::process::exit(2);
    };
    let trust_store = match core::billing::settlement_evidence::SettlementEvidenceTrustStoreV2::load(
        std::path::Path::new(trust_path),
    ) {
        Ok(store) => store,
        Err(error) => {
            eprintln!("settlement trust-store load failed: {error}");
            std::process::exit(2);
        }
    };
    let result = core::billing::reconcile_settlement_evidence_v2(&manifest, &trust_store);
    if action == "export" {
        let Some(output) = positional.get(2) else {
            eprintln!("settlement export requires an output path");
            std::process::exit(2);
        };
        if let Err(error) = manifest.export(std::path::Path::new(output)) {
            eprintln!("settlement evidence export failed: {error}");
            std::process::exit(2);
        }
    } else if action != "verify" {
        eprintln!("unknown settlement action '{action}'. Use: verify | export");
        std::process::exit(2);
    }
    if json {
        print_json_or_die(&result, "settlement evidence");
    } else {
        println!(
            "Settlement evidence v2: {}",
            if result.eligible {
                "structurally eligible"
            } else {
                "ineligible"
            }
        );
        println!("  Manifest: {}", result.manifest_id);
        println!("  Trust store: {}", result.trust_store_id);
        println!(
            "  Evidence: {}/{} active",
            result.active_evidence_count, result.evidence_count
        );
        for reason in &result.reasons {
            println!("  - {reason:?}");
        }
        println!("  Invoice/contract/approval authority: not asserted by OSS verifier");
    }
    if !result.eligible {
        std::process::exit(2);
    }
}

/// Render a quota: [`core::billing::plans::UNBOUNDED`] → "unlimited", else the
/// number (a plain `0` means *none*).
fn quota(n: u32) -> String {
    if n == core::billing::plans::UNBOUNDED {
        "unlimited".to_string()
    } else {
        n.to_string()
    }
}

fn print_json_or_die<T: serde::Serialize>(value: &T, what: &str) {
    match serde_json::to_string_pretty(value) {
        Ok(json) => println!("{json}"),
        Err(e) => {
            eprintln!("{what} serialization failed: {e}");
            std::process::exit(1);
        }
    }
}
