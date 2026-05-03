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

fn format_summary(engine: &GainEngine, model: Option<&str>) -> String {
    let s = engine.summary(model);
    let saved = format_tokens(s.tokens_saved);
    let input = format_tokens(s.input_tokens);
    let out = format_tokens(s.output_tokens);
    let avoided = format_usd(s.avoided_usd);
    let spend = format_usd(s.tool_spend_usd);
    let roi = s
        .roi
        .map_or_else(|| "n/a".to_string(), |r| format!("{r:.2}x"));
    let trend = match s.score.trend {
        crate::core::gain::gain_score::Trend::Rising => "rising",
        crate::core::gain::gain_score::Trend::Stable => "stable",
        crate::core::gain::gain_score::Trend::Declining => "declining",
    };

    format!(
        "lean-ctx gain\n\
         ────────────\n\
         Score: {total}/100  (compression {comp}, cost {cost}, quality {qual}, consistency {cons})  trend={trend}\n\
         Tokens: {input} in → {out} out  | saved {saved}  ({rate:.1}%)\n\
         Gain: {avoided} avoided  | tool spend {spend}  | ROI {roi}\n\
         Pricing: model={model_key} ({match_kind:?}) in=${in_m:.2}/M out=${out_m:.2}/M\n",
        total = s.score.total,
        comp = s.score.compression,
        cost = s.score.cost_efficiency,
        qual = s.score.quality,
        cons = s.score.consistency,
        rate = s.gain_rate_pct,
        model_key = s.model.model_key,
        match_kind = s.model.match_kind,
        in_m = s.model.cost.input_per_m,
        out_m = s.model.cost.output_per_m,
    )
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
         Trend:           {:?}\n",
        s.score.total,
        s.score.compression,
        s.score.cost_efficiency,
        s.score.quality,
        s.score.consistency,
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
        summary: crate::core::gain::GainSummary,
        tasks: Vec<crate::core::gain::TaskGainRow>,
        heatmap: Vec<crate::core::gain::FileGainRow>,
    }
    let payload = Payload {
        summary: engine.summary(model),
        tasks: engine.task_breakdown(),
        heatmap: engine.heatmap_gains(limit),
    };
    serde_json::to_string_pretty(&payload).unwrap_or_else(|_| "{}".to_string())
}

fn format_tokens(tokens: u64) -> String {
    if tokens >= 1_000_000 {
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
