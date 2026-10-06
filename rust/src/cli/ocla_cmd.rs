use anyhow::{Result, anyhow, bail};
use clap::{ArgMatches, Command};

use crate::core::savings_ledger::event::SavingsEvent;
use crate::core::{ocla::OclaService, savings_ledger};

// ── Status (Agent 07) ────────────────────────────────────────────────────────

type TraitEntry = (
    &'static str,
    for<'a> fn(&'a crate::core::ocla::OclaRegistry) -> &'a dyn OclaService,
);

const TRAITS: [TraitEntry; 13] = [
    ("observation_hook", |r| r.observation_hook.as_ref()),
    ("usage_sink", |r| r.usage_sink.as_ref()),
    ("metrics_exporter", |r| r.metrics_exporter.as_ref()),
    ("savings_ledger", |r| r.savings_ledger.as_ref()),
    ("intent_classifier", |r| r.intent_classifier.as_ref()),
    ("outcome_tracker", |r| r.outcome_tracker.as_ref()),
    ("compression_provider", |r| r.compression_provider.as_ref()),
    ("response_optimizer", |r| r.response_optimizer.as_ref()),
    ("efficiency_analyzer", |r| r.efficiency_analyzer.as_ref()),
    ("config_tuner", |r| r.config_tuner.as_ref()),
    ("experiment_runner", |r| r.experiment_runner.as_ref()),
    ("connector_scheduler", |r| r.connector_scheduler.as_ref()),
    ("agent_gateway", |r| r.agent_gateway.as_ref()),
];

pub fn register(app: Command) -> Command {
    app.subcommand(
        Command::new("ocla")
            .about("Inspect Open Context & Token Lifecycle Architecture state")
            .subcommand(Command::new("status").about("Show OCLA status and ledger coverage"))
            .subcommand(
                Command::new("reconcile")
                    .about("Reconcile legacy and unified savings ledgers")
                    .arg(
                        clap::Arg::new("json")
                            .long("json")
                            .action(clap::ArgAction::SetTrue),
                    ),
            )
            .subcommand(
                Command::new("ledger")
                    .about("Inspect the savings ledger")
                    .subcommand(Command::new("summary").about("Per-mechanism breakdown"))
                    .subcommand(Command::new("verify").about("Verify hash chain integrity"))
                    .subcommand(
                        Command::new("query")
                            .about("List events by mechanism")
                            .arg(clap::Arg::new("mechanism").long("mechanism").required(true))
                            .arg(
                                clap::Arg::new("limit")
                                    .long("limit")
                                    .default_value("10")
                                    .value_parser(clap::value_parser!(usize)),
                            ),
                    )
                    .subcommand(
                        Command::new("p5-coverage").about("Show P5 field population stats"),
                    ),
            ),
    )
}

pub fn handle(matches: &ArgMatches) -> Result<()> {
    match matches.subcommand() {
        Some(("ocla", nested)) => return handle(nested),
        Some(("status", _)) | None => print_status(),
        Some(("reconcile", nested)) => return handle_reconcile(nested),
        Some(("ledger", nested)) => return handle_ledger(nested),
        Some((name, _)) => bail!("unknown ocla subcommand: {name}"),
    }
    Ok(())
}

fn handle_reconcile(matches: &ArgMatches) -> Result<()> {
    let ledger = crate::core::ocla::unified_ledger::FileUnifiedLedger::from_data_dir()
        .map_err(|error| anyhow!(error.to_string()))?;
    let report = ledger
        .reconcile()
        .map_err(|error| anyhow!(error.to_string()))?;

    if matches.get_flag("json") {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        println!("Reconciliation report");
        println!("  Metric               Value");
        println!("  -------------------  -----");
        println!("  Matched              {}", report.matched);
        println!("  Unmatched legacy     {}", report.unmatched_legacy);
        println!("  Unmatched unified    {}", report.unmatched_unified);
        println!("  Token drift          {}", report.token_drift);
        println!("  Double-bookings      {}", report.double_bookings.len());
        for hash in &report.double_bookings {
            println!("    {hash}");
        }
    }

    Ok(())
}

fn print_status() {
    let registry = crate::core::ocla::OclaRegistry::global();
    println!("OCLA traits:");
    for (name, service) in TRAITS {
        let capability = service(registry).capability();
        println!("  {name}: builtin ({:?})", capability.status);
    }

    let path = savings_ledger::store::default_path();
    let summary = path
        .as_deref()
        .map(savings_ledger::store::summarize)
        .unwrap_or_default();
    println!(
        "Ledger: total events={}, saved tokens={}, saved USD={:.6}",
        summary.total_events, summary.saved_tokens, summary.saved_usd
    );

    let events = path
        .as_deref()
        .map(savings_ledger::store::load)
        .unwrap_or_default();
    println!("P5 field coverage (with / without):");
    println!(
        "  measurement_method: {} / {}",
        events
            .iter()
            .filter(|e| e.measurement_method.is_some())
            .count(),
        events
            .iter()
            .filter(|e| e.measurement_method.is_none())
            .count()
    );
    println!(
        "  evidence_class: {} / {}",
        events.iter().filter(|e| e.evidence_class.is_some()).count(),
        events.iter().filter(|e| e.evidence_class.is_none()).count()
    );
    println!(
        "  attribution_id: {} / {}",
        events.iter().filter(|e| e.attribution_id.is_some()).count(),
        events.iter().filter(|e| e.attribution_id.is_none()).count()
    );
}

/// Adapter for the existing argument-vector dispatcher.
pub fn cmd_ocla(args: &[String]) {
    let mut argv = vec!["ocla".to_string()];
    argv.extend(args.iter().cloned());
    let matches = register(Command::new("lean-ctx"))
        .try_get_matches_from(argv)
        .unwrap_or_else(|error| error.exit());
    handle(&matches).unwrap_or_else(|error| {
        eprintln!("ocla: {error}");
        std::process::exit(2);
    });
}

// ── Ledger (Agent 08) ────────────────────────────────────────────────────────

const MECHANISMS: [&str; 3] = ["compression", "routing", "caching"];
const P5_FIELDS: [&str; 19] = [
    "intent_tag",
    "outcome",
    "model_original",
    "model_routed",
    "routing_savings",
    "response_original_tokens",
    "response_delivered_tokens",
    "agent_chain_id",
    "chain_depth",
    "measurement_method",
    "evidence_class",
    "confidence",
    "quality_signal",
    "attribution_group",
    "attribution_id",
    "baseline_ref",
    "price_version",
    "customer_approval",
    "settlement_status",
];

fn handle_ledger(matches: &ArgMatches) -> Result<()> {
    let path =
        savings_ledger::store::default_path().ok_or_else(|| anyhow!("ledger path unavailable"))?;
    let action = matches.subcommand_name().unwrap_or("summary");

    match action {
        "summary" => {
            let summary = savings_ledger::store::summarize(&path);
            let events = savings_ledger::store::load(&path);
            println!("Ledger events: {}", summary.total_events);
            for (mechanism, count, tokens, usd) in mechanism_breakdown(&events, &summary) {
                println!("{mechanism}: {count} events, {tokens} tokens, ${usd:.6}");
            }
        }
        "verify" => {
            let result = savings_ledger::store::verify(&path);
            if result.valid {
                println!("Ledger valid: {} events", result.total);
            } else {
                println!(
                    "Ledger invalid at event {} ({} events read)",
                    result.first_invalid_at.unwrap_or(result.total),
                    result.total
                );
            }
        }
        "query" => {
            let mechanism = matches
                .get_one::<String>("mechanism")
                .ok_or_else(|| anyhow!("query requires --mechanism <M>"))?;
            let limit = matches.get_one::<usize>("limit").copied().unwrap_or(10);
            for event in events_for_mechanism(&savings_ledger::store::load(&path), mechanism, limit)
            {
                println!(
                    "{} {} {} tokens={} usd=${:.6} hash={}",
                    event.ts,
                    event.mechanism,
                    event.tool,
                    event.saved_tokens,
                    event.saved_usd,
                    event.entry_hash
                );
            }
        }
        "p5-coverage" => {
            let events = savings_ledger::store::load(&path);
            let events_with_p5 = events
                .iter()
                .filter(|event| p5_presence(event).iter().any(|populated| *populated))
                .count();
            println!("P5 coverage: {} events", events.len());
            println!(
                "Events with any P5 field: {events_with_p5}/{}",
                events.len()
            );
            for (field, populated) in P5_FIELDS.iter().zip(p5_counts(&events)) {
                println!("{field}: {populated}/{}", events.len());
            }
        }
        other => return Err(anyhow!("unknown ledger subcommand: {other}")),
    }
    Ok(())
}

fn mechanism_breakdown(
    events: &[SavingsEvent],
    summary: &savings_ledger::store::LedgerSummary,
) -> Vec<(&'static str, usize, u64, f64)> {
    MECHANISMS
        .iter()
        .map(|mechanism| {
            let count = events
                .iter()
                .filter(|event| event.mechanism == *mechanism)
                .count();
            let (tokens, usd) = summary
                .by_mechanism
                .iter()
                .find(|row| row.0 == *mechanism)
                .map_or((0, 0.0), |row| (row.1, row.2));
            (*mechanism, count, tokens, usd)
        })
        .collect()
}

fn events_for_mechanism<'a>(
    events: &'a [SavingsEvent],
    mechanism: &str,
    limit: usize,
) -> impl Iterator<Item = &'a SavingsEvent> {
    events
        .iter()
        .filter(move |event| event.mechanism == mechanism)
        .rev()
        .take(limit)
}

fn p5_counts(events: &[SavingsEvent]) -> [usize; 19] {
    let mut counts = [0; 19];
    for event in events {
        let populated = p5_presence(event);
        for (count, is_populated) in counts.iter_mut().zip(populated) {
            *count += usize::from(is_populated);
        }
    }
    counts
}

fn p5_presence(event: &SavingsEvent) -> [bool; 19] {
    [
        event.intent_tag.is_some(),
        event.outcome.is_some(),
        event.model_original.is_some(),
        event.model_routed.is_some(),
        event.routing_savings.is_some(),
        event.response_original_tokens.is_some(),
        event.response_delivered_tokens.is_some(),
        event.agent_chain_id.is_some(),
        event.chain_depth.is_some(),
        event.measurement_method.is_some(),
        event.evidence_class.is_some(),
        event.confidence.is_some(),
        event.quality_signal.is_some(),
        event.attribution_group.is_some(),
        event.attribution_id.is_some(),
        event.baseline_ref.is_some(),
        event.price_version.is_some(),
        event.customer_approval.is_some(),
        event.settlement_status.is_some(),
    ]
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::savings_ledger::event::MECHANISM_COMPRESSION;

    #[test]
    fn register_accepts_status() {
        let matches = register(Command::new("lean-ctx"))
            .try_get_matches_from(["lean-ctx", "ocla", "status"])
            .expect("status should parse");
        let (_, ocla) = matches.subcommand().expect("ocla subcommand");
        assert!(matches!(ocla.subcommand_name(), Some("status")));
    }

    #[test]
    fn register_accepts_reconcile_json() {
        let matches = register(Command::new("lean-ctx"))
            .try_get_matches_from(["lean-ctx", "ocla", "reconcile", "--json"])
            .expect("reconcile should parse");
        let (_, ocla) = matches.subcommand().expect("ocla subcommand");
        let (_, reconcile) = ocla.subcommand().expect("reconcile subcommand");
        assert!(reconcile.get_flag("json"));
    }

    fn event(mechanism: &str, saved_tokens: u64) -> SavingsEvent {
        serde_json::from_value(serde_json::json!({
            "ts": "2026-07-20T00:00:00Z",
            "tool": "ctx_read",
            "mechanism": mechanism,
            "model_id": "test",
            "tokenizer": "o200k_base",
            "baseline_tokens": saved_tokens + 10,
            "actual_tokens": 10,
            "saved_tokens": saved_tokens,
            "bounce_adjustment": 0,
            "unit_price_per_m_usd": 1.0,
            "saved_usd": 0.001,
            "repo_hash": "repo",
            "agent_id": "agent",
            "prev_hash": "genesis",
            "entry_hash": "hash",
            "version": "5"
        }))
        .expect("valid test event")
    }

    #[test]
    fn p5_counts_only_populated_fields() {
        let mut populated = event(MECHANISM_COMPRESSION, 10);
        populated.intent_tag = Some("coding".into());
        populated.confidence = Some(0.9);
        let counts = p5_counts(&[populated, event("routing", 0)]);
        assert_eq!(counts[0], 1);
        assert_eq!(counts[11], 1);
        assert!(counts.iter().skip(1).take(10).all(|count| *count == 0));
    }

    #[test]
    fn query_returns_newest_matching_events_and_honors_limit() {
        let events = vec![
            event(MECHANISM_COMPRESSION, 1),
            event("routing", 2),
            event(MECHANISM_COMPRESSION, 3),
        ];
        let result: Vec<u64> = events_for_mechanism(&events, MECHANISM_COMPRESSION, 1)
            .map(|event| event.saved_tokens)
            .collect();
        assert_eq!(result, vec![3]);
    }
}
