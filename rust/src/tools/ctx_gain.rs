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

    for r in rows.iter().take(13) {
        let ratio = r.tokens_saved as f64 / max_saved as f64;
        let bar = pad_right(&t.gradient_bar(ratio, 12), 12);
        let cat = pad_right(&format!("{a}{}{rst}", r.category.label()), 14);
        let saved = pad_right(
            &format!("{s}{bold}{}{rst}", format_tokens(r.tokens_saved)),
            9,
        );
        let cmds = format!("{dim}{:>5} cmds{rst}", r.commands);
        let tools = format!("{dim}{:>3} tools{rst}", r.tool_calls);
        let spend = format!("{m}{}{rst}", format_usd(r.tool_spend_usd));
        out.push(sec_line(&format!(
            " {cat} {bar} {saved} {cmds}  {tools}  {spend}"
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
            let cost_s = format!("{s}${:.4}{rst}", agent.cost_usd);
            let model_tag = agent
                .model_key
                .as_deref()
                .map(|mk| format!(" {dim}[{mk}]{rst}"))
                .unwrap_or_default();
            out.push(sec_line(&format!(
                " {dim}{}. {rst}{name} {bar} {cost_s} {dim}{}c{rst}{model_tag}",
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
            let cost_s = format!("{s}${:.4}{rst}", tool.cost_usd);
            out.push(sec_line(&format!(
                " {dim}{}. {rst}{name} {bar} {cost_s} {dim}{}c avg {:.0}in+{:.0}out{rst}",
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
        let calls = format!("{s}{bold}{:>3}{rst}c", agent.total_calls);
        let toks = format!(
            "{dim}{}in {}out{rst}",
            format_tokens(agent.total_input_tokens),
            format_tokens(agent.total_output_tokens)
        );
        out.push(sec_line(&format!(
            " {dim}{}.{rst} {name} {bar} {calls} {toks}",
            i + 1
        )));
    }

    out.push(sec_line(""));
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
        let saved = pad_right(
            &format!("{s}{bold}{}{rst}", format_tokens(r.tokens_saved)),
            8,
        );
        let pct = t.pct_color(f64::from(r.compression_pct));
        out.push(sec_line(&format!(
            " {dim}{:>2}.{rst} {path_col} {bar} {saved} {pct}{:>2.0}%{rst} {dim}{}x{rst}",
            i + 1,
            r.compression_pct,
            r.access_count
        )));
    }

    out.push(sec_line(""));
    out.push(format!("  {}", t.box_bottom_square(w)));
}

fn truncate_str(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}…", &s[..max - 1])
    }
}

fn shorten_path(path: &str, max: usize) -> String {
    if path.len() <= max {
        return path.to_string();
    }
    if let Some(pos) = path.rfind('/') {
        let file = &path[pos + 1..];
        if file.len() >= max - 3 {
            return format!("…{}", &file[file.len() - (max - 1)..]);
        }
        let remaining = max - file.len() - 4;
        let start = &path[..remaining.min(path.len())];
        return format!("{start}…/{file}");
    }
    format!("{}…", &path[..max - 1])
}
