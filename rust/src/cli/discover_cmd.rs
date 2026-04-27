use super::common::load_shell_history;

pub fn cmd_discover(_args: &[String]) {
    let history = load_shell_history();
    if history.is_empty() {
        println!("No shell history found.");
        return;
    }

    let result = crate::tools::ctx_discover::analyze_history(&history, 20);
    println!("{}", crate::tools::ctx_discover::format_cli_output(&result));
}

pub fn cmd_ghost(args: &[String]) {
    let json = args.iter().any(|a| a == "--json");

    let history = load_shell_history();
    let discover = crate::tools::ctx_discover::analyze_history(&history, 20);

    let session = crate::core::session::SessionState::load_latest();
    let store = crate::core::stats::load();

    let unoptimized_tokens = discover.potential_tokens;
    let _unoptimized_usd = discover.potential_usd;

    let redundant_reads = store.cep.total_cache_hits as usize;
    let redundant_tokens = redundant_reads * 200;

    let wasted_original = store
        .cep
        .total_tokens_original
        .saturating_sub(store.cep.total_tokens_compressed) as usize;
    let truncated_tokens = wasted_original / 3;

    let total_ghost = unoptimized_tokens + redundant_tokens + truncated_tokens;
    let total_usd =
        total_ghost as f64 * crate::core::stats::DEFAULT_INPUT_PRICE_PER_M / 1_000_000.0;
    let monthly_usd = total_usd * 30.0;

    if json {
        let obj = serde_json::json!({
            "ghost_tokens": total_ghost,
            "breakdown": {
                "unoptimized_shells": unoptimized_tokens,
                "redundant_reads": redundant_tokens,
                "truncated_contexts": truncated_tokens,
            },
            "estimated_usd": total_usd,
            "monthly_usd": monthly_usd,
            "session_active": session.is_some(),
            "history_commands": discover.total_commands,
            "already_optimized": discover.already_optimized,
        });
        println!("{}", serde_json::to_string_pretty(&obj).unwrap_or_default());
        return;
    }

    let bold = "\x1b[1m";
    let green = "\x1b[32m";
    let yellow = "\x1b[33m";
    let red = "\x1b[31m";
    let dim = "\x1b[2m";
    let rst = "\x1b[0m";
    let white = "\x1b[97m";

    println!();
    println!("  {bold}{white}lean-ctx ghost report{rst}");
    println!("  {dim}{}{rst}", "=".repeat(40));
    println!();

    if total_ghost == 0 {
        println!("  {green}No ghost tokens detected!{rst}");
        println!(
            "  {dim}All {} commands optimized.{rst}",
            discover.total_commands
        );
        println!();
        return;
    }

    let severity = if total_ghost > 10000 {
        red
    } else if total_ghost > 3000 {
        yellow
    } else {
        green
    };

    println!(
        "  {bold}Ghost Tokens found:{rst}     {severity}{total_ghost:>8}{rst} tokens {dim}(~${total_usd:.2}){rst}"
    );
    println!();

    if unoptimized_tokens > 0 {
        let missed_count: u32 = discover.missed_commands.iter().map(|m| m.count).sum();
        println!(
            "  {dim}  Unoptimized shells:{rst}  {white}{unoptimized_tokens:>8}{rst} {dim}({missed_count} cmds without lean-ctx){rst}"
        );
    }
    if redundant_tokens > 0 {
        println!(
            "  {dim}  Redundant reads:{rst}     {white}{redundant_tokens:>8}{rst} {dim}({redundant_reads} cache hits = wasted re-reads){rst}"
        );
    }
    if truncated_tokens > 0 {
        println!(
            "  {dim}  Oversized contexts:{rst}  {white}{truncated_tokens:>8}{rst} {dim}(uncompressed portion of tool results){rst}"
        );
    }

    println!();
    println!("  {bold}Monthly savings potential:{rst} {green}${monthly_usd:.2}{rst}");

    if !discover.missed_commands.is_empty() {
        println!();
        println!("  {bold}Top unoptimized commands:{rst}");
        for m in discover.missed_commands.iter().take(5) {
            println!(
                "    {dim}{:>4}x{rst}  {white}{:<12}{rst} {dim}{}{rst}",
                m.count, m.prefix, m.description
            );
        }
    }

    println!();
    if discover.already_optimized == 0 {
        println!(
            "  {yellow}Run '{bold}lean-ctx setup{rst}{yellow}' to eliminate ghost tokens.{rst}"
        );
    } else {
        println!(
            "  {dim}Already optimized: {}/{} commands{rst}",
            discover.already_optimized, discover.total_commands
        );
    }
    println!();
}
