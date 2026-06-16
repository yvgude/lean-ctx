//! `lean-ctx savings` — the verified savings ledger: verify/rechain/export,
//! Ed25519-signed batches, team push/roll-up, ROI report and the summary box.

use crate::core;

pub(in crate::cli::dispatch) fn cmd_savings(rest: &[String]) {
    let action = rest.first().map_or("summary", String::as_str);
    match action {
        "verify" => {
            let v = core::savings_ledger::verify();
            if v.valid {
                println!(
                    "Savings ledger: OK — {} event(s), SHA-256 chain intact.",
                    v.total
                );
            } else {
                println!(
                    "Savings ledger: TAMPERED — chain broke at entry {} (of {} verified).",
                    v.first_invalid_at.unwrap_or(0),
                    v.total
                );
                println!(
                    "  If you did not edit the ledger, repair the chain with: lean-ctx savings rechain"
                );
                std::process::exit(1);
            }
        }
        "rechain" => cmd_savings_rechain(),
        "export" => {
            let events = core::savings_ledger::all_events();
            match serde_json::to_string_pretty(&events) {
                Ok(json) => println!("{json}"),
                Err(e) => {
                    eprintln!("Export failed: {e}");
                    std::process::exit(1);
                }
            }
        }
        "sign" => cmd_savings_sign(&rest[1..]),
        "push" => cmd_savings_push(&rest[1..]),
        "team" => cmd_savings_team(&rest[1..]),
        "verify-batch" => cmd_savings_verify_batch(rest.get(1).map(String::as_str)),
        "roi" => cmd_savings_roi(&rest[1..]),
        "summary" | "" => print!("{}", format_savings_summary()),
        _ => {
            eprintln!(
                "Usage: lean-ctx savings [summary|team|verify|rechain|export|sign|push|verify-batch|roi]"
            );
            std::process::exit(1);
        }
    }
}

/// `lean-ctx savings rechain` — re-hashes the ledger under the v2 (float-free) scheme to
/// repair a chain broken by the legacy `{:.6}` round-trip bug. Event content is preserved;
/// only the chain links are recomputed. A break that survives re-chaining is real tampering.
fn cmd_savings_rechain() {
    match core::savings_ledger::rechain() {
        Ok(0) => println!("Savings ledger is empty — nothing to re-chain."),
        Ok(n) => {
            let v = core::savings_ledger::verify();
            if v.valid {
                println!(
                    "Savings ledger re-chained: {n} event(s) migrated to the v2 (float-free) hash. SHA-256 chain intact."
                );
            } else {
                println!(
                    "Re-chained {n} event(s), but the chain still breaks at entry {} — this indicates real tampering, not the float bug.",
                    v.first_invalid_at.unwrap_or(0)
                );
                std::process::exit(1);
            }
        }
        Err(e) => {
            eprintln!("Re-chain failed: {e}");
            std::process::exit(1);
        }
    }
}

/// `lean-ctx savings roi [--json]` — the privacy-preserving ROI/metering surface
/// derived from the signed savings batch (EPIC 12.20). Read-only: it never
/// mutates the ledger.
fn cmd_savings_roi(args: &[String]) {
    let agent_id = savings_agent_id();
    let report = core::savings_ledger::roi_report(&agent_id);

    if args.iter().any(|a| a == "--json") {
        match serde_json::to_string_pretty(&report) {
            Ok(json) => println!("{json}"),
            Err(e) => {
                eprintln!("ROI report serialization failed: {e}");
                std::process::exit(1);
            }
        }
        return;
    }

    println!("{}", report.headline());
    println!();
    println!("  Period:        {}", report.period);
    println!("  Events:        {}", report.total_events);
    println!(
        "  Saved tokens:  {} gross / {} net",
        report.saved_tokens, report.net_saved_tokens
    );
    println!("  Saved USD:     ${:.4}", report.saved_usd);
    println!(
        "  Avg / event:   {:.1} tok, ${:.6}",
        report.avg_saved_tokens_per_event, report.avg_saved_usd_per_event
    );
    println!(
        "  Chain:         {}",
        if report.chain_valid {
            "valid (SHA-256 intact)"
        } else {
            "BROKEN — run `lean-ctx savings rechain`"
        }
    );
    println!(
        "  Signature:     {}",
        if report.signed {
            "present (Ed25519)"
        } else {
            "unsigned (machine identity unavailable)"
        }
    );
    if !report.top_tools.is_empty() {
        println!("  Top tools:");
        for (tool, tokens) in report.top_tools.iter().take(5) {
            println!("    {tool}: {tokens} tok");
        }
    }
}

/// Resolves the signing identity (same precedence as the ledger's own attribution).
/// `pub(super)`: billing meters usage under the same identity.
pub(super) fn savings_agent_id() -> String {
    core::savings_ledger::push::agent_id()
}

/// `lean-ctx savings sign [--out FILE]` — builds + Ed25519-signs a portable savings batch and
/// writes the JSON artifact (offline-verifiable with `savings verify-batch`).
fn cmd_savings_sign(args: &[String]) {
    use core::savings_ledger::{SignedSavingsBatchV1, signed_batch};

    let out_override = args.iter().find_map(|a| {
        a.strip_prefix("--out=")
            .map(std::path::PathBuf::from)
            .or_else(|| (a == "--out").then_some(std::path::PathBuf::new()))
    });
    // Support `--out FILE` (space-separated) too.
    let out_override = match out_override {
        Some(p) if p.as_os_str().is_empty() => args
            .iter()
            .skip_while(|a| a.as_str() != "--out")
            .nth(1)
            .map(std::path::PathBuf::from),
        other => other,
    };

    let agent_id = savings_agent_id();
    let mut batch = SignedSavingsBatchV1::build_all(&agent_id);
    if batch.totals.total_events == 0 {
        eprintln!(
            "Savings ledger is empty — nothing to sign yet. It fills as lean-ctx compresses your reads."
        );
        std::process::exit(1);
    }
    if let Err(e) = batch.sign(&agent_id) {
        eprintln!("Signing failed: {e}");
        std::process::exit(1);
    }

    let out = match out_override {
        Some(p) => Ok(p),
        None => signed_batch::default_artifact_path(),
    };
    let path = out.and_then(|p| signed_batch::write_artifact(&batch, &p));
    match path {
        Ok(p) => {
            use core::wrapped::format_tokens;
            let pk = batch.signer_public_key.as_deref().unwrap_or("");
            println!("Signed savings batch written to {}", p.display());
            println!(
                "  Net saved:  {} tokens (~${:.2}) over {} event(s)",
                format_tokens(batch.totals.net_saved_tokens),
                batch.totals.saved_usd,
                batch.totals.total_events
            );
            println!("  Chain head: {}", batch.last_entry_hash);
            println!(
                "  Chain:      {}",
                if batch.chain_valid {
                    "intact (SHA-256)"
                } else {
                    "BROKEN — run `lean-ctx savings verify`"
                }
            );
            println!("  Signer key: {pk}");
            println!(
                "\nVerify anywhere (no ledger needed):  lean-ctx savings verify-batch {}",
                p.display()
            );
        }
        Err(e) => {
            eprintln!("Could not write artifact: {e}");
            std::process::exit(1);
        }
    }
}

/// `lean-ctx savings push [--team-url URL]` — signs the local savings ledger and pushes
/// the batch to the team server for opt-in org roll-up.
/// Resolve the team server URL: `--team-url[=]` arg → `team_url` in config.toml.
fn resolve_team_url(args: &[String]) -> Option<String> {
    args.iter()
        .find_map(|a| a.strip_prefix("--team-url=").map(String::from))
        .or_else(|| {
            args.iter()
                .position(|a| a == "--team-url")
                .and_then(|i| args.get(i + 1).cloned())
        })
        .or_else(|| crate::core::config::Config::load().team_url.clone())
        .filter(|s| !s.trim().is_empty())
}

/// Resolve the team bearer token: `--team-token[=]` arg → `LEAN_CTX_TEAM_TOKEN`
/// env → `team_token` in config.toml. `None` means push/pull is unauthenticated.
fn resolve_team_token(args: &[String]) -> Option<String> {
    args.iter()
        .find_map(|a| a.strip_prefix("--team-token=").map(String::from))
        .or_else(|| {
            args.iter()
                .position(|a| a == "--team-token")
                .and_then(|i| args.get(i + 1).cloned())
        })
        .or_else(|| std::env::var("LEAN_CTX_TEAM_TOKEN").ok())
        .or_else(|| crate::core::config::Config::load().team_token.clone())
        .filter(|s| !s.trim().is_empty())
}

fn cmd_savings_push(args: &[String]) {
    use core::savings_ledger::push::{PushError, ingest_endpoint, push_batch};

    let Some(url) = resolve_team_url(args) else {
        eprintln!("No team URL configured. Use --team-url or set team_url in config.toml");
        std::process::exit(1);
    };
    let team_token = resolve_team_token(args);

    match push_batch(&url, team_token.as_deref()) {
        Ok(outcome) => {
            use core::wrapped::format_tokens;
            println!("\x1b[32m✓\x1b[0m Savings batch pushed to team server.");
            println!(
                "  Net saved:  {} tokens (~${:.2})",
                format_tokens(outcome.net_saved_tokens),
                outcome.saved_usd
            );
            println!("  Endpoint:   {}", ingest_endpoint(&url));
        }
        Err(PushError::Unauthorized) => {
            eprintln!(
                "Team server denied the push (HTTP 401/403). Set a member token via \
                 --team-token, LEAN_CTX_TEAM_TOKEN, or team_token in config.toml."
            );
            std::process::exit(1);
        }
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    }
}

/// `lean-ctx savings team` — pull the team server's aggregated savings roll-up
/// (the opt-in org view) and print it. Requires a token with the `audit` scope.
fn cmd_savings_team(args: &[String]) {
    let Some(url) = resolve_team_url(args) else {
        eprintln!("No team URL configured. Use --team-url or set team_url in config.toml");
        std::process::exit(1);
    };
    let endpoint = format!("{}/v1/savings/summary", url.trim_end_matches('/'));
    let mut request = ureq::get(&endpoint);
    if let Some(tok) = resolve_team_token(args) {
        request = request.header("Authorization", &format!("Bearer {tok}"));
    }
    match request.call() {
        Ok(resp) => {
            let status = resp.status();
            let text = resp.into_body().read_to_string().unwrap_or_default();
            match status.as_u16() {
                200 => print!("{}", format_team_savings(&text)),
                401 | 403 => {
                    eprintln!(
                        "Team server denied access (HTTP {status}). The token needs the \
                         'audit' scope (owner/admin). Set it via --team-token, \
                         LEAN_CTX_TEAM_TOKEN, or team_token in config.toml."
                    );
                    std::process::exit(1);
                }
                code => {
                    eprintln!("Team server error (HTTP {code}): {text}");
                    std::process::exit(1);
                }
            }
        }
        Err(e) => {
            eprintln!("Failed to reach team server at {endpoint}: {e}");
            std::process::exit(1);
        }
    }
}

/// Render the team `/v1/savings/summary` JSON into a compact, human-readable view.
fn format_team_savings(body: &str) -> String {
    use core::wrapped::format_tokens;
    let Ok(v) = serde_json::from_str::<serde_json::Value>(body) else {
        return "Team savings: could not parse the server response.\n".to_string();
    };

    let net = v["totals"]["net_saved_tokens"].as_u64().unwrap_or(0);
    let usd = v["totals"]["saved_usd"].as_f64().unwrap_or(0.0);
    let members = v["member_count"].as_u64().unwrap_or(0);

    let mut out = String::new();
    out.push_str("\n  \x1b[1mTeam savings\x1b[0m (opt-in roll-up)\n");
    out.push_str(&format!("  Members reporting: {members}\n"));
    out.push_str(&format!(
        "  Net saved:         {} tokens (~${usd:.2})\n",
        format_tokens(net)
    ));

    if members == 0 {
        out.push_str(
            "\n  No member has pushed a signed savings batch yet.\n  \
             One-off:    lean-ctx savings push\n  \
             Automatic:  lean-ctx config set team_url <url> \
             && lean-ctx config set team_token <member-token> \
             && lean-ctx config set team_auto_push true\n",
        );
        return out;
    }

    if let Some(rows) = v["by_member"].as_array().filter(|r| !r.is_empty()) {
        out.push_str("\n  Per member:\n");
        for m in rows {
            let id = m["agent_id"].as_str().unwrap_or("?");
            let mnet = m["net_saved_tokens"].as_u64().unwrap_or(0);
            let musd = m["saved_usd"].as_f64().unwrap_or(0.0);
            out.push_str(&format!(
                "    {id:<30} {} tokens (~${musd:.2})\n",
                format_tokens(mnet)
            ));
        }
    }

    if let Some(rows) = v["by_model"].as_array().filter(|r| !r.is_empty()) {
        out.push_str("\n  Per model:\n");
        for m in rows {
            let model = m["model"].as_str().unwrap_or("?");
            let t = m["saved_tokens"].as_u64().unwrap_or(0);
            out.push_str(&format!("    {model:<30} {} tokens\n", format_tokens(t)));
        }
    }
    out.push('\n');
    out
}

/// `lean-ctx savings verify-batch FILE` — verifies an exported batch's Ed25519 signature
/// offline (integrity + origin), without needing the original ledger.
fn cmd_savings_verify_batch(file: Option<&str>) {
    use core::savings_ledger::signed_batch;

    let Some(file) = file else {
        eprintln!("Usage: lean-ctx savings verify-batch <file.json>");
        std::process::exit(1);
    };
    let batch = match signed_batch::load_artifact(std::path::Path::new(file)) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("Cannot read artifact: {e}");
            std::process::exit(1);
        }
    };
    let res = batch.verify();
    if res.signature_valid {
        use core::wrapped::format_tokens;
        println!("Signed savings batch: VALID");
        println!(
            "  Signed by:  {}",
            res.signer_public_key.as_deref().unwrap_or("?")
        );
        println!("  Agent:      {}", batch.agent_id);
        println!("  Created:    {}", batch.created_at);
        println!("  lean-ctx:   {}", batch.lean_ctx_version);
        println!(
            "  Net saved:  {} tokens (~${:.2}) over {} event(s)",
            format_tokens(batch.totals.net_saved_tokens),
            batch.totals.saved_usd,
            batch.totals.total_events
        );
        println!("  Chain head: {}", batch.last_entry_hash);
        if !batch.chain_valid {
            println!("  NOTE: the ledger chain was already broken when this batch was signed.");
        }
    } else {
        println!(
            "Signed savings batch: INVALID — {}",
            res.error.as_deref().unwrap_or("signature check failed")
        );
        std::process::exit(1);
    }
}

#[allow(clippy::many_single_char_names)] // ANSI formatting locals: s,v,t,m,w
fn format_savings_summary() -> String {
    use core::wrapped::format_tokens;
    let s = core::savings_ledger::summary();
    if s.total_events == 0 {
        return "Savings ledger is empty — it fills as lean-ctx compresses your reads.\n"
            .to_string();
    }
    let v = core::savings_ledger::verify();
    let energy_tokens = if s.bounce_events > 0 {
        s.net_saved_tokens()
    } else {
        s.saved_tokens
    };

    let t = core::theme::load_theme(&core::config::Config::load().theme);
    let rst = core::theme::rst();
    let bold = core::theme::bold();
    let dim = core::theme::dim();
    let sc = t.success.fg();
    let m = t.muted.fg();
    let w = 56;
    let ss = t.box_side_square();
    let sl = |content: &str| -> String {
        let padded = core::theme::pad_right(content, w);
        format!("  {ss}{padded}{ss}")
    };

    let integrity_badge = if v.valid {
        format!("{sc}✓ SHA-256 chain intact{rst}")
    } else {
        format!(
            "{}✗ BROKEN — run `lean-ctx savings verify`{rst}",
            t.danger.fg()
        )
    };

    let mut out = Vec::new();
    out.push(String::new());
    out.push(format!(
        "  {}",
        t.box_top_labeled(w, "VERIFIED SAVINGS LEDGER")
    ));
    out.push(sl(&format!(
        "  {bold}Events{rst}      {m}{}{rst}",
        s.total_events
    )));
    // Integrity status on its own line — the "BROKEN" badge is too long to share
    // the Events row without overflowing the box border.
    out.push(sl(&format!("  {integrity_badge}")));
    if s.bounce_events > 0 {
        out.push(sl(&format!(
            "  {bold}Saved{rst}       {sc}{}{rst}  {dim}(gross){rst}",
            format_tokens(s.saved_tokens)
        )));
        out.push(sl(&format!(
            "  {bold}Bounce{rst}      {m}{}{rst}  {dim}({} re-reads){rst}",
            format_tokens(s.bounce_tokens),
            s.bounce_events
        )));
        out.push(sl(&format!(
            "  {bold}Net saved{rst}   {sc}{bold}{}{rst}",
            format_tokens(s.net_saved_tokens())
        )));
        out.push(sl(&format!(
            "  {bold}USD{rst}         {sc}{bold}${:.2}{rst}  {dim}(net of bounce){rst}",
            s.saved_usd
        )));
    } else {
        out.push(sl(&format!(
            "  {bold}Saved{rst}       {sc}{bold}{}{rst}",
            format_tokens(s.saved_tokens)
        )));
        out.push(sl(&format!(
            "  {bold}USD{rst}         {sc}{bold}${:.2}{rst}",
            s.saved_usd
        )));
    }
    {
        let energy = core::energy::format_for_tokens(energy_tokens);
        let charges = core::energy::phone_charges_hint(energy_tokens)
            .map(|h| format!("  ({h})"))
            .unwrap_or_default();
        out.push(sl(&format!(
            "  {bold}Energy{rst}      {sc}{energy}{rst}{dim}{charges}{rst}"
        )));
    }
    out.push(format!("  {}", t.box_bottom_square(w)));

    if !s.by_model.is_empty() {
        out.push(String::new());
        out.push(format!("  {}", t.box_top_labeled(w, "BY MODEL")));
        for (model, tok, usd) in s.by_model.iter().take(5) {
            out.push(sl(&format!(
                "  {m}{model:<22}{rst} {:>10} tok  {sc}${usd:.2}{rst}",
                format_tokens(*tok)
            )));
        }
        out.push(format!("  {}", t.box_bottom_square(w)));
    }

    if s.by_day.len() >= 2 {
        out.push(String::new());
        out.push(format!("  {}", t.box_top_labeled(w, "RECENT DAYS")));
        let recent: Vec<_> = s.by_day.iter().rev().take(7).collect();
        for (day, tok, usd) in recent.into_iter().rev() {
            out.push(sl(&format!(
                "  {m}{day}{rst}  {:>10} tok  {sc}${usd:.2}{rst}",
                format_tokens(*tok)
            )));
        }
        out.push(format!("  {}", t.box_bottom_square(w)));
    }

    out.push(String::new());
    out.join("\n")
}
