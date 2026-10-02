use crate::core::gain::GainEngine;

pub fn handle(
    action: &str,
    period: Option<&str>,
    model: Option<&str>,
    limit: Option<usize>,
) -> String {
    let engine = GainEngine::load();
    let lim = limit.unwrap_or(10).clamp(1, 50);
    let env_model = std::env::var("LEAN_CTX_MODEL")
        .or_else(|_| std::env::var("LCTX_MODEL"))
        .ok();
    let model = model.or(env_model.as_deref());

    match action {
        "status" | "report" | "" => format_summary(&engine, model),
        "score" => format_score(&engine, model),
        "tasks" => format_tasks(&engine),
        "heatmap" => format_heatmap(&engine, lim),
        "agents" => format_agents(&engine, lim),
        "cost" => crate::core::a2a::cost_attribution::format_cost_report(&engine.costs, lim),
        "wrapped" => render_wrapped(period.unwrap_or("all"), false),
        "json" => format_json(&engine, model, lim),
        _ => format!(
            "Unknown action '{action}'. Available: status, report, score, cost, tasks, heatmap, wrapped, agents, json"
        ),
    }
}

pub(crate) fn render_wrapped(period: &str, compact: bool) -> String {
    let report = crate::core::wrapped::WrappedReport::generate(period);
    if compact {
        report.format_compact()
    } else {
        report.format_ascii()
    }
}

/// The headline economics line. Without an observed provider turn the bill side
/// is invisible, so the line shows the gross figure as what it is — a local
/// estimate on observed tool output — and says the bill impact is unknown.
fn gain_line(s: &crate::core::gain::GainSummary, avoided: &str, spend: &str, roi: &str) -> String {
    if s.provider_path_observed {
        format!("Gain: {avoided} est. net avoided  | tool spend {spend}  | ROI {roi}")
    } else {
        format!(
            "Gain: {avoided} gross on observed tool output (local estimate)  | provider bill impact: unknown — proxy not in request path  | ROI n/a"
        )
    }
}

fn format_summary(engine: &GainEngine, model: Option<&str>) -> String {
    let s = engine.summary(model);
    let bridge = crate::core::gain::bridge_status::BridgeStatus::detect();
    let saved = format_tokens(s.tokens_saved);
    let input = format_tokens(s.input_tokens);
    let out = format_tokens(s.output_tokens);
    let avoided = format_usd(s.avoided_usd);
    let spend = format_usd(s.tool_spend_usd);
    let energy = crate::core::energy::format_wh(s.energy_wh);
    let co2 = crate::core::energy::format_co2(s.co2_grams);
    let roi = s
        .roi
        .map_or_else(|| "n/a".to_string(), |r| format!("{r:.2}x"));
    let trend = match s.score.trend {
        crate::core::gain::gain_score::Trend::Rising => "rising",
        crate::core::gain::gain_score::Trend::Stable => "stable",
        crate::core::gain::gain_score::Trend::Declining => "declining",
    };

    let mut report = format!(
        "lean-ctx gain\n\
         ────────────\n\
         {bridge_line}\n\
         Score: {total}/100  (compression {comp}, cost {cost}, quality {qual}, consistency {cons}, navigability {nav})  trend={trend}\n\
         Tokens: {input} in → {out} out  | saved {saved}  ({rate:.1}%)\n\
         {gain_line}\n\
         Evidence: {evidence}\n\
         Impact: {energy} grid energy avoided  | {co2} CO₂e (est.)\n\
         Pricing: model={model_key} ({match_kind:?}) input=${in_m:.2}/M cache_write=${cw_m:.2}/M cache_read=${cr_m:.2}/M output=${out_m:.2}/M\n",
        bridge_line = bridge.summary_line(),
        gain_line = gain_line(&s, &avoided, &spend, &roi),
        evidence = s.economic_evidence.label(),
        total = s.score.total,
        comp = s.score.compression,
        cost = s.score.cost_efficiency,
        qual = s.score.quality,
        cons = s.score.consistency,
        nav = s.score.navigability,
        rate = s.gain_rate_pct,
        model_key = s.model.model_key,
        match_kind = s.model.match_kind,
        in_m = s.model.cost.input_per_m,
        cw_m = s.model.cost.cache_write_per_m,
        cr_m = s.model.cost.cache_read_per_m,
        out_m = s.model.cost.output_per_m,
    );

    if let Some(cache_performance) = format_cache_performance(engine) {
        report.push('\n');
        report.push_str(&cache_performance);
    }

    let streams = s.stream_savings;
    report.push_str(&format!(
        "\nStream                 | Tokens Saved | Rate       | USD Saved\n\
         -----------------------+--------------+------------+----------\n\
         First inject/cache_write | {:>12} | ${:>7.2}/M | {:>9}\n\
         Re-read/cache_read       | {:>12} | ${:>7.2}/M | {:>9}\n\
         New input                | {:>12} | ${:>7.2}/M | {:>9}\n\
         Output                   | {:>12} | ${:>7.2}/M | {:>9}\n\
         lean-ctx overhead+bounce | {:>12} | mixed      | {:>9}\n\
         Net bill impact          |              |            | {:>9}\n",
        format_tokens(streams.first_inject_tokens_saved),
        s.model.cost.cache_write_per_m,
        format_usd(streams.cache_write_usd_saved),
        format_tokens(streams.reread_tokens_saved),
        s.model.cost.cache_read_per_m,
        format_usd(streams.cache_read_usd_saved),
        format_tokens(streams.input_tokens_saved),
        s.model.cost.input_per_m,
        format_usd(streams.input_usd_saved),
        format_tokens(streams.output_tokens_saved),
        s.model.cost.output_per_m,
        format_usd(streams.output_usd_saved),
        format!(
            "-{}",
            format_tokens(
                streams
                    .first_inject_overhead_tokens
                    .saturating_add(streams.reread_overhead_tokens)
                    .saturating_add(streams.bounce_tokens)
            )
        ),
        format_usd(-streams.overhead_usd),
        s.net_bill_impact_usd
            .map_or_else(|| "unknown".to_string(), format_usd),
    ));

    // Net-of-injection honesty line (#361): reconcile the meter to the bill.
    // On a non-caching rail the fixed per-turn injection is re-billed every
    // turn, so the true bill impact is gross saved minus that tax.
    let overhead_pt = s.injected_overhead_tokens_per_turn;
    if s.turns > 0 {
        let sign = if s.net_tokens_saved < 0 { "-" } else { "" };
        report.push_str(&format!(
            "Injection: {op}/turn × {turns} turns = {tax} re-billed  | net saved {sign}{net}\n",
            op = format_tokens(overhead_pt),
            turns = s.turns,
            tax = format_tokens(s.injected_overhead_total_tokens),
            net = format_tokens(s.net_tokens_saved.unsigned_abs()),
        ));
    } else if overhead_pt > 0 {
        report.push_str(&format!(
            "Injection: {op}/turn fixed context tax (proxy not in request path — provider bill impact unknown)\n",
            op = format_tokens(overhead_pt),
        ));
    }
    // Budget breach (#964): the fixed per-turn prefix outgrew the configured
    // [context] budget_tokens. Machine-readable token so log/CI scrapers can key
    // on it; `doctor overhead --gate` is the hard CI gate.
    if s.over_budget {
        report.push_str(&format!(
            "OVER_BUDGET: {op}/turn > budget {b} — trim tools/rules or raise [context] budget_tokens.\n",
            op = format_tokens(overhead_pt),
            b = format_tokens(s.injected_overhead_budget_tokens),
        ));
    }

    if let Some(reason) = bridge.zero_savings_reason(s.tokens_saved) {
        report.push('\n');
        report.push_str(&reason);
        report.push('\n');
    }

    report
}

#[derive(Debug, Clone, Copy)]
struct CacheLayer<'a> {
    name: &'a str,
    description: &'a str,
    hits: u64,
    requests: u64,
}

fn format_cache_performance(engine: &GainEngine) -> Option<String> {
    let content = crate::core::content_cache::stats();
    let response = crate::core::ocla::response_cache::global_response_cache().stats();
    let layers = [
        CacheLayer {
            name: "Read Cache:",
            description: "SessionCache — file re-reads",
            hits: engine.stats.cep.total_cache_hits,
            requests: engine.stats.cep.total_cache_reads,
        },
        CacheLayer {
            name: "Search Cache:",
            description: "ContentCache — search index",
            hits: content.hits,
            requests: content.hits.saturating_add(content.misses),
        },
        CacheLayer {
            name: "Response Cache:",
            description: "OCLA — tool responses",
            hits: response.hits,
            requests: response.hits.saturating_add(response.misses),
        },
    ];
    let overall_hit_rate = crate::core::telemetry::global_metrics()
        .snapshot()
        .cache_hit_rate;
    format_cache_performance_layers(&layers, overall_hit_rate)
}

fn format_cache_performance_layers(
    layers: &[CacheLayer<'_>],
    overall_hit_rate: f64,
) -> Option<String> {
    let visible: Vec<_> = layers.iter().filter(|layer| layer.requests > 0).collect();
    if visible.is_empty() {
        return None;
    }

    let mut output = String::from("Cache Performance\n");
    for layer in visible {
        let hit_rate = layer.hits as f64 / layer.requests as f64 * 100.0;
        output.push_str(&format!(
            "  ├─ {:<15} {:>3.0}% ({})\n",
            layer.name, hit_rate, layer.description
        ));
    }
    output.push_str(&format!(
        "  └─ {:<15} {:>3.0}% (weighted by request volume)\n",
        "Overall:",
        overall_hit_rate * 100.0
    ));
    Some(output)
}

fn format_score(engine: &GainEngine, model: Option<&str>) -> String {
    let s = engine.summary(model);
    format!(
        "Gain Score: {}/100\n\
         ──────────────────\n\
         Compression:     {}/100\n\
         Cost efficiency: {}/100\n\
         Quality:         {}/100\n\
         Consistency:     {}/100\n\
         Navigability:    {}/100\n\
         Trend:           {:?}\n",
        s.score.total,
        s.score.compression,
        s.score.cost_efficiency,
        s.score.quality,
        s.score.consistency,
        s.score.navigability,
        s.score.trend
    )
}

fn format_tasks(engine: &GainEngine) -> String {
    let rows = engine.task_breakdown();
    if rows.is_empty() {
        return "No task data yet.".to_string();
    }
    let mut lines = Vec::new();
    lines.push("Task Breakdown (gain-first):".to_string());
    lines.push(String::new());
    for r in rows.iter().take(13) {
        lines.push(format!(
            "  {:<14}  saved {:>8} tok  cmds {:>5}  tools {:>5}  tool spend {}",
            r.category.label(),
            format_tokens(r.tokens_saved),
            r.commands,
            r.tool_calls,
            format_usd(r.tool_spend_usd)
        ));
    }
    lines.join("\n")
}

fn format_heatmap(engine: &GainEngine, limit: usize) -> String {
    let rows = engine.heatmap_gains(limit);
    if rows.is_empty() {
        return "No heatmap data recorded yet.".to_string();
    }
    let mut lines = Vec::new();
    lines.push(format!("Heatmap (top {limit} files by tokens saved):"));
    for (i, r) in rows.iter().enumerate() {
        lines.push(format!(
            "  {}. {} — {} tok saved, {} accesses, {:.0}% compression",
            i + 1,
            r.path,
            format_tokens(r.tokens_saved),
            r.access_count,
            r.compression_pct
        ));
    }
    lines.join("\n")
}

fn format_agents(engine: &GainEngine, limit: usize) -> String {
    let top = engine.costs.top_agents(limit);
    if top.is_empty() {
        return "No agent cost data recorded yet.".to_string();
    }
    let mut lines = Vec::new();
    lines.push(format!("Top Agents by tool spend (top {limit}):"));
    for (i, a) in top.iter().enumerate() {
        lines.push(format!(
            "  {}. {} ({}) — {} calls, {} in + {} out tok, {}{}",
            i + 1,
            a.agent_id,
            a.agent_type,
            a.total_calls,
            format_tokens(a.total_input_tokens),
            format_tokens(a.total_output_tokens),
            format_usd(a.cost_usd),
            a.model_key
                .as_deref()
                .map(|m| format!(" [{m}]"))
                .unwrap_or_default()
        ));
    }
    lines.join("\n")
}

fn format_json(engine: &GainEngine, model: Option<&str>, limit: usize) -> String {
    #[derive(serde::Serialize)]
    struct Payload {
        bridge: crate::core::gain::bridge_status::BridgeStatus,
        summary: crate::core::gain::GainSummary,
        tasks: Vec<crate::core::gain::TaskGainRow>,
        heatmap: Vec<crate::core::gain::FileGainRow>,
    }
    let payload = Payload {
        bridge: crate::core::gain::bridge_status::BridgeStatus::detect(),
        summary: engine.summary(model),
        tasks: engine.task_breakdown(),
        heatmap: engine.heatmap_gains(limit),
    };
    serde_json::to_string_pretty(&payload).unwrap_or_else(|_| "{}".to_string())
}

fn format_tokens(tokens: u64) -> String {
    if tokens >= 1_000_000_000_000 {
        format!("{:.2}T", tokens as f64 / 1_000_000_000_000.0)
    } else if tokens >= 1_000_000_000 {
        format!("{:.2}B", tokens as f64 / 1_000_000_000.0)
    } else if tokens >= 1_000_000 {
        format!("{:.1}M", tokens as f64 / 1_000_000.0)
    } else if tokens >= 1_000 {
        format!("{:.1}K", tokens as f64 / 1_000.0)
    } else {
        format!("{tokens}")
    }
}

fn format_usd(amount: f64) -> String {
    if amount >= 0.01 {
        format!("${amount:.2}")
    } else {
        format!("${amount:.3}")
    }
}

/// Premium themed deep sections for the CLI `gain --deep` dashboard.
pub fn format_deep_themed(model: Option<&str>, limit: usize) -> String {
    use crate::core::theme;
    let engine = GainEngine::load();
    let cfg = crate::core::config::Config::load();
    let t = theme::load_theme(&cfg.theme);
    let lim = limit.clamp(1, 50);

    let mut out = Vec::new();
    format_tasks_themed(&engine, &t, &mut out);
    format_cost_themed(&engine, &t, lim, model, &mut out);
    format_agents_themed(&engine, &t, lim, &mut out);
    format_heatmap_themed(&engine, &t, lim, &mut out);
    format_injection_methodology(&t, &mut out);
    out.join("\n")
}

#[allow(clippy::many_single_char_names)] // ANSI formatting locals: t,a,s,m,w
fn format_tasks_themed(engine: &GainEngine, t: &crate::core::theme::Theme, out: &mut Vec<String>) {
    use crate::core::theme::{self, pad_right};
    let rows = engine.task_breakdown();
    if rows.is_empty() {
        return;
    }
    let rst = theme::rst();
    let bold = theme::bold();
    let dim = theme::dim();
    let a = t.accent.fg();
    let s = t.success.fg();
    let m = t.muted.fg();

    let w = 70;
    let ss = t.box_side_square();
    let sec_line = |content: &str| -> String {
        let padded = pad_right(content, w);
        format!("  {ss}{padded}{ss}")
    };

    out.push(String::new());
    out.push(format!("  {}", t.box_top_labeled(w, "TASK BREAKDOWN")));
    out.push(sec_line(""));

    let max_saved = rows
        .iter()
        .map(|r| r.tokens_saved)
        .max()
        .unwrap_or(1)
        .max(1);

    // Column widths sum to 67 of the 70-wide box: 1 + 12 + 1 + 10 + 1 + 8 + 1
    // + 11 + 1 + 12 + 1 + 8. Counts are right-aligned inside the ANSI wrapper
    // (the `{:>N}` sees plain text) so five-digit tool counts no longer push
    // the spend column past the border.
    for r in rows.iter().take(13) {
        let ratio = r.tokens_saved as f64 / max_saved as f64;
        let bar = pad_right(&t.gradient_bar(ratio, 10), 10);
        let cat = pad_right(r.category.label(), 12);
        let saved = format!("{s}{bold}{:>8}{rst}", format_tokens(r.tokens_saved));
        let cmds = format!("{dim}{:>6} cmds{rst}", r.commands);
        let tools = format!("{dim}{:>6} tools{rst}", r.tool_calls);
        let spend = format!("{m}{:>8}{rst}", format_usd(r.tool_spend_usd));
        out.push(sec_line(&format!(
            " {a}{cat}{rst} {bar} {saved} {cmds} {tools} {spend}"
        )));
    }

    out.push(sec_line(""));
    out.push(format!("  {}", t.box_bottom_square(w)));
}

#[allow(clippy::many_single_char_names)] // ANSI formatting locals
fn format_cost_themed(
    engine: &GainEngine,
    t: &crate::core::theme::Theme,
    limit: usize,
    model: Option<&str>,
    out: &mut Vec<String>,
) {
    use crate::core::theme::{self, pad_right};
    let rst = theme::rst();
    let bold = theme::bold();
    let dim = theme::dim();
    let a = t.accent.fg();
    let s = t.success.fg();
    let w_col = t.warning.fg();

    let w = 70;
    let ss = t.box_side_square();
    let sec_line = |content: &str| -> String {
        let padded = pad_right(content, w);
        format!("  {ss}{padded}{ss}")
    };

    let store = &engine.costs;
    let (total_in, total_out, total_cached) = store.total_tokens();
    let total_cost = store.total_cost();

    let env_model = std::env::var("LEAN_CTX_MODEL")
        .or_else(|_| std::env::var("LCTX_MODEL"))
        .ok();
    let resolved_model = model.or(env_model.as_deref());

    out.push(String::new());
    let header = format!(
        "COST ATTRIBUTION ── {} agents, {} tools",
        store.agents.len(),
        store.tools.len()
    );
    out.push(format!("  {}", t.box_top_labeled(w, &header)));
    out.push(sec_line(""));

    out.push(sec_line(&format!(
        " {bold}Total:{rst} {s}{}{rst} in + {s}{}{rst} out + {dim}{}{rst} cached = {a}{bold}${total_cost:.4}{rst}",
        format_tokens(total_in),
        format_tokens(total_out),
        format_tokens(total_cached),
    )));

    if let Some(mk) = resolved_model {
        let pricing = crate::core::gain::model_pricing::ModelPricing::load();
        let q = pricing.quote(Some(mk));
        out.push(sec_line(&format!(
            " {dim}model={} in=${:.2}/M out=${:.2}/M{rst}",
            q.model_key, q.cost.input_per_m, q.cost.output_per_m
        )));
    }

    let top_agents = store.top_agents(limit);
    if !top_agents.is_empty() {
        out.push(sec_line(""));
        out.push(sec_line(&format!(" {bold}Top Agents{rst}")));
        let max_cost = top_agents
            .first()
            .map_or(1.0_f64, |a2| a2.cost_usd.max(0.001));
        for (i, agent) in top_agents.iter().enumerate() {
            let ratio = agent.cost_usd / max_cost;
            let bar = pad_right(&t.gradient_bar(ratio, 8), 8);
            let name = pad_right(
                &format!("{a}{}{rst}", truncate_str(&agent.agent_id, 18)),
                20,
            );
            let cost_s = format!("{s}{:>8}{rst}", format!("${:.4}", agent.cost_usd));
            let model_tag = agent
                .model_key
                .as_deref()
                .map(|mk| format!(" {dim}[{mk}]{rst}"))
                .unwrap_or_default();
            out.push(sec_line(&format!(
                " {dim}{:>2}. {rst}{name} {bar} {cost_s} {dim}{:>4}c{rst}{model_tag}",
                i + 1,
                agent.total_calls
            )));
        }
    }

    let top_tools = store.top_tools(limit);
    if !top_tools.is_empty() {
        out.push(sec_line(""));
        out.push(sec_line(&format!(" {bold}Top Tools{rst}")));
        let max_cost = top_tools
            .first()
            .map_or(1.0_f64, |t2| t2.cost_usd.max(0.001));
        for (i, tool) in top_tools.iter().enumerate() {
            let ratio = tool.cost_usd / max_cost;
            let bar = pad_right(&t.gradient_bar(ratio, 8), 8);
            let name = pad_right(
                &format!("{w_col}{}{rst}", pad_right(&tool.tool_name, 12)),
                14,
            );
            let cost_s = format!("{s}{:>9}{rst}", format!("${:.4}", tool.cost_usd));
            out.push(sec_line(&format!(
                " {dim}{:>2}. {rst}{name} {bar} {cost_s} {dim}{:>6}c avg {:>4.0}in+{:>5.0}out{rst}",
                i + 1,
                tool.total_calls,
                tool.avg_input_tokens,
                tool.avg_output_tokens
            )));
        }
    }

    out.push(sec_line(""));
    out.push(format!("  {}", t.box_bottom_square(w)));
}

fn format_agents_themed(
    engine: &GainEngine,
    t: &crate::core::theme::Theme,
    limit: usize,
    out: &mut Vec<String>,
) {
    use crate::core::theme::{self, pad_right};
    let top = engine.costs.top_agents(limit);
    if top.is_empty() {
        return;
    }
    let rst = theme::rst();
    let bold = theme::bold();
    let dim = theme::dim();
    let a = t.accent.fg();
    let s = t.success.fg();

    let w = 70;
    let ss = t.box_side_square();
    let sec_line = |content: &str| -> String {
        let padded = pad_right(content, w);
        format!("  {ss}{padded}{ss}")
    };

    out.push(String::new());
    out.push(format!("  {}", t.box_top_labeled(w, "AGENTS")));
    out.push(sec_line(""));

    let max_calls = top
        .iter()
        .map(|a2| a2.total_calls)
        .max()
        .unwrap_or(1)
        .max(1);

    for (i, agent) in top.iter().enumerate() {
        let ratio = agent.total_calls as f64 / max_calls as f64;
        let bar = pad_right(&t.gradient_bar(ratio, 10), 10);
        let name = pad_right(
            &format!("{a}{}{rst}", truncate_str(&agent.agent_id, 22)),
            24,
        );
        let calls = format!("{s}{bold}{:>4}{rst}c", agent.total_calls);
        let toks = format!(
            "{dim}{:>7}in {:>8}out{rst}",
            format_tokens(agent.total_input_tokens),
            format_tokens(agent.total_output_tokens)
        );
        out.push(sec_line(&format!(
            " {dim}{:>2}.{rst} {name} {bar} {calls} {toks}",
            i + 1
        )));
    }

    out.push(sec_line(""));
    out.push(format!("  {}", t.box_bottom_square(w)));
}

/// #1104: Injection methodology note for `gain --deep`. Explains the baseline
/// subtraction and cache-rate correction so the net-of-injection figure is
/// transparent.
fn format_injection_methodology(t: &crate::core::theme::Theme, out: &mut Vec<String>) {
    use crate::core::theme::{self, pad_right};
    let rst = theme::rst();
    let dim = theme::dim();
    let w = 70;
    let ss = t.box_side_square();
    let row = |s: &str| -> String { format!("  {ss}{}{ss}", pad_right(s, w)) };

    let overhead = crate::core::context_overhead::ContextOverhead::cached();
    let cache_rate = crate::core::config::Config::load()
        .dashboard_cache_hit_rate()
        .unwrap_or(0.75);

    out.push(String::new());
    out.push(format!(
        "  {}",
        t.box_top_labeled(w, "INJECTION METHODOLOGY")
    ));
    out.push(row(""));
    out.push(row(&format!(
        " {dim}lean-ctx injects a fixed per-turn prefix (tool schemas +{rst}"
    )));
    out.push(row(&format!(
        " {dim}instructions + rules). The net-of-injection figure corrects{rst}"
    )));
    out.push(row(&format!(
        " {dim}for two factors that the gross overhead overstates:{rst}"
    )));
    out.push(row(""));
    out.push(row(&format!(
        " {dim}1. Baseline: native IDE tools (Read/Grep/Shell/Glob/Write){rst}"
    )));
    out.push(row(&format!(
        " {dim}   also inject ~2,400 tok/turn. Only the delta above that{rst}"
    )));
    out.push(row(&format!(
        " {dim}   baseline is lean-ctx overhead.{rst}"
    )));
    out.push(row(&format!(
        " {dim}2. Cache: providers cache stable prefixes (Anthropic ~90%,{rst}"
    )));
    out.push(row(&format!(
        " {dim}   OpenAI ~50%). Effective cost = delta × (1 − cache_rate).{rst}"
    )));
    out.push(row(""));
    out.push(row(&format!(" {dim}Current config:{rst}")));
    out.push(row(&format!(
        " {dim}  Total overhead:  {} tok/turn ({} tools){rst}",
        overhead.total_tokens(),
        overhead.tool_count
    )));
    out.push(row(&format!(
        " {dim}  Native baseline: 2,400 tok/turn{rst}"
    )));
    out.push(row(&format!(
        " {dim}  Delta:           {} tok/turn{rst}",
        (overhead.total_tokens() as u64).saturating_sub(2400)
    )));
    out.push(row(&format!(
        " {dim}  Cache hit rate:  {:.0}% (config: dashboard_cache_hit_rate){rst}",
        cache_rate * 100.0
    )));
    out.push(row(&format!(
        " {dim}  Effective cost:  {} tok/turn{rst}",
        ((overhead.total_tokens() as u64).saturating_sub(2400) as f64 * (1.0 - cache_rate)) as u64
    )));
    out.push(row(""));
    out.push(row(&format!(
        " {dim}Worst-case (no cache): lean-ctx gain --no-cache-adjust{rst}"
    )));
    out.push(row(""));
    out.push(format!("  {}", t.box_bottom_square(w)));
}

fn format_heatmap_themed(
    engine: &GainEngine,
    t: &crate::core::theme::Theme,
    limit: usize,
    out: &mut Vec<String>,
) {
    use crate::core::theme::{self, pad_right};
    let rows = engine.heatmap_gains(limit);
    if rows.is_empty() {
        return;
    }
    let rst = theme::rst();
    let bold = theme::bold();
    let dim = theme::dim();
    let s = t.success.fg();

    let w = 70;
    let ss = t.box_side_square();
    let sec_line = |content: &str| -> String {
        let padded = pad_right(content, w);
        format!("  {ss}{padded}{ss}")
    };

    out.push(String::new());
    out.push(format!(
        "  {}",
        t.box_top_labeled(w, &format!("HEATMAP ── top {limit}"))
    ));
    out.push(sec_line(""));

    let max_saved = rows
        .iter()
        .map(|r| r.tokens_saved)
        .max()
        .unwrap_or(1)
        .max(1);

    for (i, r) in rows.iter().enumerate() {
        let ratio = r.tokens_saved as f64 / max_saved as f64;
        let bar = pad_right(&t.gradient_bar(ratio, 10), 10);
        let short_path = shorten_path(&r.path, 28);
        let path_col = pad_right(&format!("{dim}{short_path}{rst}"), 30);
        let saved = format!("{s}{bold}{:>8}{rst}", format_tokens(r.tokens_saved));
        let pct = t.pct_color(f64::from(r.compression_pct));
        // `{:>3.0}%` — a 100% row is three digits and used to shove the access
        // column one place right.
        out.push(sec_line(&format!(
            " {dim}{:>2}.{rst} {path_col} {bar} {saved} {pct}{:>3.0}%{rst} {dim}{:>4}x{rst}",
            i + 1,
            r.compression_pct,
            r.access_count
        )));
    }

    out.push(sec_line(""));
    out.push(format!("  {}", t.box_bottom_square(w)));
}

/// Longest prefix of `s` that is at most `max_bytes` bytes and ends on a char
/// boundary. Byte-indexed slicing (`&s[..n]`) panics mid-codepoint — paths and
/// agent ids regularly carry multibyte characters (umlauts, CJK, emoji), which
/// crashed `gain --deep` (GitHub #386).
fn safe_prefix(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes {
        return s;
    }
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// Longest suffix of `s` that is at most `max_bytes` bytes and starts on a
/// char boundary.
fn safe_suffix(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes {
        return s;
    }
    let mut start = s.len() - max_bytes;
    while start < s.len() && !s.is_char_boundary(start) {
        start += 1;
    }
    &s[start..]
}

fn truncate_str(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}…", safe_prefix(s, max.saturating_sub(1)))
    }
}

fn shorten_path(path: &str, max: usize) -> String {
    if path.len() <= max {
        return path.to_string();
    }
    if let Some(pos) = path.rfind('/') {
        let file = &path[pos + 1..];
        // `file.len() + 3 >= max` (not `file.len() >= max - 3`): the
        // subtraction underflows for max < 3.
        if file.len() + 3 >= max {
            return format!("…{}", safe_suffix(file, max.saturating_sub(1)));
        }
        let remaining = max.saturating_sub(file.len() + 4);
        let start = safe_prefix(path, remaining);
        return format!("{start}…/{file}");
    }
    format!("{}…", safe_prefix(path, max.saturating_sub(1)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_report_surfaces_bridge_precondition() {
        let _lock = crate::core::data_dir::test_env_lock();
        // The agent/benchmark-facing status + report actions must always state
        // the bridge precondition (connected + tool-count) so a `0` saved is
        // never silent (#307 / GitHub #361).
        for action in ["status", "report", ""] {
            let out = handle(action, None, None, None);
            assert!(
                out.contains("Bridge:"),
                "action '{action}' must surface the bridge status line, got:\n{out}"
            );
        }
    }

    #[test]
    fn json_action_embeds_bridge_engagement() {
        let _lock = crate::core::data_dir::test_env_lock();
        let out = handle("json", None, None, Some(5));
        let val: serde_json::Value =
            serde_json::from_str(&out).expect("json action must emit valid JSON");
        let engagement = val
            .get("bridge")
            .and_then(|b| b.get("engagement"))
            .and_then(serde_json::Value::as_str)
            .expect("bridge.engagement must be present");
        assert!(
            matches!(engagement, "proxy_down" | "no_requests" | "engaged"),
            "unexpected engagement tag: {engagement}"
        );
    }

    #[test]
    fn summary_reports_bill_weighted_token_stream_table() {
        let _iso = crate::core::data_dir::isolated_data_dir();
        let out = handle("status", None, Some("gpt-5"), Some(5));
        for label in [
            "Stream                 | Tokens Saved | Rate       | USD Saved",
            "First inject/cache_write",
            "Re-read/cache_read",
            "New input",
            "Output",
            "lean-ctx overhead+bounce",
            "Net bill impact",
        ] {
            assert!(out.contains(label), "missing stream row {label:?}:\n{out}");
        }
    }

    #[test]
    fn truncation_is_char_boundary_safe() {
        // GitHub #386: byte-indexed truncation panicked mid-codepoint when
        // paths/agent ids contained multibyte characters. Sweep every cut
        // position across multibyte inputs — must never panic.
        let samples = [
            "/Users/müller/Projekte/größe/mod.rs",
            "/home/用户/プロジェクト/файл.rs",
            "agent-🚀🔥-ünïcödé-identifier",
            "ä",
            "",
            "no-multibyte-at-all/plain.rs",
        ];
        for s in samples {
            for max in 0..=s.len() + 2 {
                let _ = truncate_str(s, max);
                let _ = shorten_path(s, max);
            }
        }
    }

    #[test]
    fn truncation_keeps_ascii_behaviour() {
        assert_eq!(truncate_str("short", 10), "short");
        assert_eq!(truncate_str("exactly-ten", 11), "exactly-ten");
        assert_eq!(truncate_str("longer-than-max", 8), "longer-…");
        assert_eq!(shorten_path("/a/b/file.rs", 50), "/a/b/file.rs");
        let p = shorten_path("/very/long/path/to/some/file.rs", 20);
        assert!(p.contains('…') && p.ends_with("file.rs"), "got: {p}");
    }

    #[test]
    fn gain_json_schema_keys_are_stable_for_jetbrains_dtos() {
        let _lock = crate::core::data_dir::test_env_lock();
        // Contract with packages/jetbrains-lean-ctx dto/GainData.kt (@SerializedName).
        // A Rust rename that drops any of these keys must fail HERE, not silently
        // in the plugin. Keep in sync with GainDataTest.kt.
        let out = handle("json", None, None, Some(5));
        let v: serde_json::Value = serde_json::from_str(&out).expect("valid json");

        let summary = v.get("summary").expect("summary");
        for k in [
            "tokens_saved",
            "gain_rate_pct",
            "avoided_usd",
            "model",
            "score",
            // Net-of-injection reconciliation (#361) — part of the stable DTO.
            "injected_overhead_tokens_per_turn",
            "turns",
            "injected_overhead_total_tokens",
            "net_tokens_saved",
            // Economic evidence: an unobservable bill impact is null, never gross.
            "economic_evidence",
            "provider_path_observed",
            "net_bill_impact_tokens",
            "net_bill_impact_usd",
        ] {
            assert!(summary.get(k).is_some(), "summary.{k} missing");
        }
        assert!(
            summary["model"].get("model_key").is_some(),
            "summary.model.model_key missing"
        );
        let streams = summary
            .get("stream_savings")
            .expect("summary.stream_savings");
        for k in [
            "first_inject_tokens_saved",
            "reread_tokens_saved",
            "cache_write_usd_saved",
            "cache_read_usd_saved",
            "gross_usd_saved",
            "overhead_usd",
            "net_usd_saved",
        ] {
            assert!(
                streams.get(k).is_some(),
                "summary.stream_savings.{k} missing"
            );
        }

        let score = summary.get("score").expect("score");
        for k in [
            "total",
            "compression",
            "cost_efficiency",
            "quality",
            "consistency",
            // Code Health Engine component (#1086) — part of the stable DTO.
            "navigability",
            "trend",
        ] {
            assert!(score.get(k).is_some(), "score.{k} missing");
        }

        let tasks = v
            .get("tasks")
            .expect("tasks")
            .as_array()
            .expect("tasks array");
        if let Some(t) = tasks.first() {
            for k in [
                "category",
                "commands",
                "tokens_saved",
                "tool_calls",
                "tool_spend_usd",
            ] {
                assert!(t.get(k).is_some(), "task.{k} missing");
            }
        }
        let heatmap = v
            .get("heatmap")
            .expect("heatmap")
            .as_array()
            .expect("heatmap array");
        if let Some(h) = heatmap.first() {
            for k in ["path", "access_count", "tokens_saved", "compression_pct"] {
                assert!(h.get(k).is_some(), "heatmap.{k} missing");
            }
        }
    }

    #[test]
    fn cache_performance_is_hidden_without_requests() {
        let layers = [
            CacheLayer {
                name: "Read Cache:",
                description: "SessionCache — file re-reads",
                hits: 0,
                requests: 0,
            },
            CacheLayer {
                name: "Search Cache:",
                description: "ContentCache — search index",
                hits: 0,
                requests: 0,
            },
        ];

        assert!(format_cache_performance_layers(&layers, 0.0).is_none());
    }

    #[test]
    fn cache_performance_aligns_visible_layers_and_omits_idle_layers() {
        let layers = [
            CacheLayer {
                name: "Read Cache:",
                description: "SessionCache — file re-reads",
                hits: 72,
                requests: 100,
            },
            CacheLayer {
                name: "Search Cache:",
                description: "ContentCache — search index",
                hits: 17,
                requests: 20,
            },
            CacheLayer {
                name: "Response Cache:",
                description: "OCLA — tool responses",
                hits: 0,
                requests: 0,
            },
        ];

        let output =
            format_cache_performance_layers(&layers, 0.45).expect("active layers are shown");
        let rows: Vec<_> = output.lines().skip(1).collect();
        assert_eq!(rows.len(), 3);
        assert!(output.contains("Read Cache:      72% (SessionCache — file re-reads)"));
        assert!(output.contains("Search Cache:    85% (ContentCache — search index)"));
        assert!(!output.contains("Response Cache:"));
        assert!(output.contains("Overall:         45% (weighted by request volume)"));
        assert!(
            rows.windows(2).all(|rows| {
                rows[0].find('%').expect("rate") == rows[1].find('%').expect("rate")
            })
        );
    }
}
