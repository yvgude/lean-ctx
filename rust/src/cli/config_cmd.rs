// SPDX-License-Identifier: Apache-2.0

use crate::core::config;
use crate::core::theme;

mod cache;
pub use cache::cmd_cache;

pub fn cmd_config(args: &[String]) {
    let cfg = config::Config::load();

    if args.is_empty() {
        println!("{}", cfg.show());
        println!(
            "\nTip: this is the full config. For the few knobs most people touch, run\n     `lean-ctx config show` (high-level summary), or change one with\n     `lean-ctx config set <key> <value>`."
        );
        return;
    }

    match args[0].as_str() {
        "init" | "create" => {
            let full = args.iter().any(|a| a == "--full");
            let created = if full {
                init_full_config()
            } else {
                write_simplified_config().map(|path| format!("Created simplified config at {path}"))
            };
            // F9c: a config that was not written is an error exit.
            match created {
                Ok(message) => println!("{message}"),
                Err(e) => {
                    eprintln!("Error: {e}");
                    std::process::exit(1);
                }
            }
        }
        "set" => {
            if args.len() < 3 {
                eprintln!("Usage: lean-ctx config set <key> <value>");
                std::process::exit(1);
            }
            let key = &args[1];
            let val = &args[2];

            // Special validation hooks for keys that need custom logic beyond
            // what the schema type system can express. These either hard-fail
            // early, or normalize the value/key that is actually persisted —
            // the resolved (write_key, write_val) then flows through the single
            // governed write path below so the #852 review covers every route.
            let (write_key, write_val): (String, String) = match key.as_str() {
                "theme" if theme::from_preset(val).is_none() && val != "custom" => {
                    eprintln!(
                        "Unknown theme '{val}'. Available: {}",
                        theme::PRESET_NAMES.join(", ")
                    );
                    std::process::exit(1);
                }
                "tee_on_error" | "tee_mode" => {
                    let normalized = match val.as_str() {
                        "true" => "failures",
                        "false" => "never",
                        other => other,
                    };
                    ("tee_mode".to_string(), normalized.to_string())
                }
                "project_root" => {
                    let path = std::path::Path::new(val.as_str());
                    if !path.exists() || !path.is_dir() {
                        eprintln!("Error: '{val}' is not an existing directory.");
                        std::process::exit(1);
                    }
                    (key.clone(), val.clone())
                }
                "embedding.model"
                    if crate::core::embeddings::model_registry::EmbeddingModel::from_str_name(
                        val,
                    )
                    .is_none() =>
                {
                    eprintln!(
                        "Unknown embedding model '{val}'. Available: minilm, potion-code \
                         (no ONNX Runtime needed), nomic — or hf:org/repo[@revision] for any \
                         HuggingFace repo with an \
                         ONNX export, e.g. hf:jinaai/jina-embeddings-v2-base-code for code \
                         (see docs/guides/custom-embeddings.md)."
                    );
                    std::process::exit(1);
                }
                "proxy.anthropic_upstream"
                | "proxy.openai_upstream"
                | "proxy.chatgpt_upstream"
                | "proxy.gemini_upstream" => {
                    let effective = normalize_optional_upstream(val).unwrap_or_default();
                    (key.clone(), effective)
                }
                _ => (key.clone(), val.clone()),
            };

            write_config_key(&write_key, &write_val, key, val, args);
        }
        "schema" => {
            let schema = config::schema::ConfigSchema::generate();
            println!(
                "{}",
                serde_json::to_string_pretty(&schema).unwrap_or_else(|_| "{}".to_string())
            );
        }
        "validate" => {
            cmd_validate();
        }
        "show" | "effective" => {
            cmd_show_effective();
        }
        "path" | "which" => {
            cmd_config_path();
        }
        "apply" | "reload" => {
            cmd_apply();
        }
        _ => {
            eprintln!("Usage: lean-ctx config [init|set|show|schema|validate|apply|path]");
            std::process::exit(1);
        }
    }
}

/// Single governed write path for `config set` (#852).
///
/// `key`/`value` are the resolved pair actually persisted; `display_key`/
/// `display_val` are what the user typed (kept for messaging, e.g. when a value
/// was normalized). Behavior:
/// - **no-op** (current == new): report "unchanged", write nothing.
/// - **consequential key** ([`config::risk`]): print a before→after review + a
///   risk note, then require confirmation or `--yes`; abort if declined.
/// - **routine key**: write directly, as before.
fn write_config_key(key: &str, value: &str, display_key: &str, display_val: &str, args: &[String]) {
    const BOLD: &str = "\x1b[1m";
    const DIM: &str = "\x1b[2m";
    const YELLOW: &str = "\x1b[33m";
    const RST: &str = "\x1b[0m";

    let current = config::setter::current_value(key);

    if current.as_deref() == Some(value) {
        println!("{display_key} is already set to {display_val} — unchanged.");
        return;
    }

    if let Some(risk) = config::risk::classify(key) {
        let before = current.as_deref().unwrap_or("(default)");
        let after = if value.is_empty() { "(default)" } else { value };
        println!("{BOLD}Review change to {display_key}{RST}");
        println!("  {before}  →  {after}");
        println!("  {YELLOW}{}{RST}", risk.note);
        if !super::prompt::confirm(
            &format!("Apply {display_key} = {display_val}?"),
            super::prompt::wants_yes(args),
        ) {
            println!("{DIM}Aborted — {display_key} left unchanged.{RST}");
            return;
        }
    }

    match config::setter::set_by_key(key, value) {
        Ok(_) => println!("Updated {display_key} = {display_val}"),
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    }
}

/// Resolves the [`config::Config`] that `config init --full` should persist.
///
/// `existing_raw` is the verbatim content of the current GLOBAL `config.toml`
/// (never the project-local `.lean-ctx.toml` — those overrides must not leak
/// into the global file). When it holds a non-empty, parseable document we
/// return its deserialized form so every user value is retained; an empty/absent
/// file yields defaults, and an unparseable one is surfaced as an error so the
/// caller can refuse to clobber it.
///
/// This is the regression guard for #443: `config init --full` previously wrote
/// `Config::default()`, and `Config::save()` overwrites any key present in both
/// the incoming document and the file (see `config_io::merge_table`), which
/// silently reset customized values like `max_ram_percent` or `compression_level`.
fn config_for_full_init(existing_raw: Option<&str>) -> Result<config::Config, String> {
    match existing_raw.map(str::trim).filter(|raw| !raw.is_empty()) {
        Some(raw) => toml::from_str::<config::Config>(raw).map_err(|e| e.to_string()),
        None => Ok(config::Config::default()),
    }
}

/// Implements `config init --full`: (re)writes the global config as a fully
/// annotated reference document, seeded with the user's existing values (#443).
///
/// Unlike `save()` (which keeps the file minimal), this emits every key with its
/// documentation. The body is a verbatim serialization of the resolved config,
/// so no customized value is ever lost; an unparseable existing file is refused
/// upstream by [`config_for_full_init`] rather than clobbered.
fn init_full_config() -> Result<String, String> {
    let path = config::Config::path().ok_or("cannot determine the config path")?;

    let existing_raw = std::fs::read_to_string(&path).ok();

    let cfg = config_for_full_init(existing_raw.as_deref()).map_err(|e| {
        format!(
            "refusing to overwrite an unparseable config.toml ({e}).\n  \
             Fix it manually or run `lean-ctx doctor --fix`, then retry."
        )
    })?;

    let schema = config::schema::ConfigSchema::generate();
    let rendered = config::render_annotated_config(&cfg, &schema);

    crate::config_io::write_atomic_with_backup(&path, &rendered)?;
    Ok(format!(
        "Created full annotated config at {}",
        path.display()
    ))
}

fn cmd_apply() {
    use crate::daemon;
    use crate::ipc;

    println!("Applying config changes…");

    // 1. Validate config first
    println!("\n[1/4] Validating config…");
    let schema = config::schema::ConfigSchema::generate();
    let known = schema.known_keys();
    let cfg = config::Config::load();

    if let Some(path) = config::Config::path()
        && path.exists()
        && let Ok(raw) = std::fs::read_to_string(&path)
        && let Ok(table) = raw.parse::<toml::Table>()
    {
        let mut user_keys = Vec::new();
        fn collect_flat(table: &toml::Table, prefix: &str, out: &mut Vec<String>) {
            for (k, v) in table {
                let full = if prefix.is_empty() {
                    k.clone()
                } else {
                    format!("{prefix}.{k}")
                };
                if let toml::Value::Table(sub) = v {
                    collect_flat(sub, &full, out);
                } else {
                    out.push(full);
                }
            }
        }
        collect_flat(&table, "", &mut user_keys);
        for key in &user_keys {
            if let Some(entry) = config::retired::retired_key(key) {
                eprintln!("  [WARN] Retired key (no effect, {}): {key}", entry.note);
            }
        }
        for hint in config::renamed::hints(&user_keys) {
            eprintln!("  [INFO] Renamed key: {hint}");
        }
        let warnings: Vec<_> = user_keys
            .iter()
            .filter(|uk| config::retired::retired_key(uk).is_none())
            .filter(|uk| config::renamed::renamed_key(uk).is_none())
            .filter(|uk| {
                !known.contains(uk) && !known.iter().any(|k| uk.starts_with(&format!("{k}.")))
            })
            .collect();
        if warnings.is_empty() {
            println!("  ✓ All config keys valid.");
        } else {
            for w in &warnings {
                eprintln!("  [WARN] Unknown key: {w}");
            }
            eprintln!(
                "  {} unknown key(s) found. Continuing anyway…",
                warnings.len()
            );
        }
    }

    // 2. Restart processes
    println!("\n[2/4] Restarting processes…");
    crate::proxy_autostart::stop();

    if let Err(e) = daemon::stop_daemon() {
        eprintln!("  Warning: daemon stop: {e}");
    }

    let orphans = ipc::process::kill_all_by_name("lean-ctx");
    if orphans > 0 {
        println!("  Terminated {orphans} orphan process(es).");
    }

    std::thread::sleep(std::time::Duration::from_millis(500));

    // Preserve IDE-owned MCP stdio servers. Killing one here severs the active
    // Codex connection and can leave the host stuck on "connecting" until it
    // is restarted. `find_killable_pids` excludes those processes while still
    // cleaning up stale daemon/proxy workers.
    let remaining = ipc::process::find_killable_pids("lean-ctx");
    if !remaining.is_empty() {
        for &pid in &remaining {
            let _ = ipc::process::force_kill(pid);
        }
        std::thread::sleep(std::time::Duration::from_millis(300));
    }

    daemon::cleanup_daemon_files();
    crate::proxy_autostart::start();

    match daemon::start_daemon(&[]) {
        Ok(()) => println!("  ✓ Daemon restarted."),
        Err(e) => {
            eprintln!("  ✗ Daemon start failed: {e}");
            std::process::exit(1);
        }
    }

    // 3. Safety checks
    println!("\n[3/4] Running safety checks…");
    println!("  RAM guard: max {}% system", cfg.max_ram_percent);

    if let Ok(data_dir) = crate::core::data_dir::lean_ctx_data_dir() {
        let sessions_dir = data_dir.join("sessions");
        let session_count = std::fs::read_dir(&sessions_dir)
            .map_or(0, |rd| rd.filter_map(std::result::Result::ok).count());
        println!("  Sessions dir: {session_count} files");
    }

    // 4. Summary
    println!("\n[4/4] Config applied successfully.");
    println!("  Theme:       {}", cfg.theme);
    println!("  Ultra compact: {}", cfg.ultra_compact);
    println!("  Checkpoint:  every {} calls", cfg.checkpoint_interval);
    if let Some(ref root) = cfg.project_root {
        println!("  Project root: {root}");
    }
}

/// Print the absolute path of the resolved global `config.toml` (one line,
/// scriptable).
///
/// This is the canonical way to verify that the CLI and the MCP server resolve
/// the *same* config file (GH #594): run `lean-ctx config path` in a terminal
/// and from the editor's MCP context, then compare. Output is the bare path so
/// it can be diffed/captured in scripts and tests; it exits non-zero only when
/// no config directory can be resolved at all.
fn cmd_config_path() {
    let Some(p) = config::Config::path() else {
        eprintln!("Error: cannot resolve a config directory");
        std::process::exit(1);
    };
    println!("{}", p.display());
}

fn cmd_validate() {
    // GH #450: always surface *where* the effective settings come from first, so
    // a "no config" result is never a dead end and a silently shadowed value
    // (env / project-local / parse error) is immediately visible.
    print_config_provenance();

    let schema = config::schema::ConfigSchema::generate();
    let known = schema.known_keys();

    let path = match config::Config::path() {
        Some(p) if p.exists() => p,
        Some(p) => {
            println!("[OK] No config.toml at {} — using defaults.", p.display());
            return;
        }
        None => {
            println!("[OK] No config dir resolved — using defaults.");
            return;
        }
    };

    let raw = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("[ERROR] Cannot read {}: {e}", path.display());
            std::process::exit(1);
        }
    };

    let table: toml::Table = match raw.parse() {
        Ok(t) => t,
        Err(e) => {
            eprintln!("[ERROR] Invalid TOML: {e}");
            std::process::exit(1);
        }
    };

    let mut warnings = 0u32;
    let mut validated = 0u32;

    fn collect_keys(table: &toml::Table, prefix: &str, out: &mut Vec<String>) {
        for (k, v) in table {
            let full = if prefix.is_empty() {
                k.clone()
            } else {
                format!("{prefix}.{k}")
            };
            match v {
                toml::Value::Table(sub) => collect_keys(sub, &full, out),
                toml::Value::Array(arr) => {
                    out.push(full.clone());
                    for item in arr {
                        if let toml::Value::Table(sub) = item {
                            for sk in sub.keys() {
                                out.push(format!("{full}[].{sk}"));
                            }
                        }
                    }
                }
                _ => out.push(full),
            }
        }
    }

    let mut user_keys = Vec::new();
    collect_keys(&table, "", &mut user_keys);

    for uk in &user_keys {
        let base = uk.split("[].").next().unwrap_or(uk);
        let field = uk.rsplit("[].").next().unwrap_or("");
        let check_key = if uk.contains("[].") {
            format!("{base}.{field}")
        } else {
            uk.clone()
        };

        if known.contains(&check_key)
            || known
                .iter()
                .any(|k| check_key.starts_with(&format!("{k}.")))
        {
            validated += 1;
        } else {
            warnings += 1;
            let suggestion = find_closest(&check_key, &known);
            if let Some(sug) = suggestion {
                eprintln!("[WARN] Unknown key '{uk}' -- did you mean '{sug}'?");
            } else {
                eprintln!("[WARN] Unknown key '{uk}' -- this field does not exist");
            }
        }
    }

    let cfg = config::Config::load();
    let budget = cfg.max_disk_mb_effective();
    if budget > 0 {
        let explicit_archive = cfg.archive.max_disk_mb;
        let explicit_bm25 = cfg.bm25_max_cache_mb;
        let sum = explicit_archive + explicit_bm25;
        if sum > budget {
            warnings += 1;
            println!(
                "  ⚠ max_disk_mb={budget} but archive.max_disk_mb({explicit_archive}) + bm25_max_cache_mb({explicit_bm25}) = {sum} exceeds budget"
            );
        }
    }

    let total = validated + warnings;
    if warnings == 0 {
        println!(
            "[OK] All {total} keys validated successfully ({}).",
            path.display()
        );
    } else {
        println!(
            "[RESULT] {validated} of {total} keys validated, {warnings} unknown ({}).",
            path.display()
        );
        std::process::exit(1);
    }
}

/// Print where the editable settings actually come from (GH #450): the resolved
/// `config.toml` path, the layout pin, any parse error, and the env /
/// project-local overrides that can silently shadow a saved value. This makes the
/// "my quick settings keep resetting" reports self-diagnosing — the reporter sees
/// the exact mechanism instead of an opaque "no config" message.
fn print_config_provenance() {
    let prov = config::Config::provenance();

    println!("Config source:");
    match &prov.config_path {
        Some(p) if prov.config_exists => println!("  config.toml:    {} (exists)", p.display()),
        Some(p) => println!(
            "  config.toml:    {} (missing — using defaults)",
            p.display()
        ),
        None => println!("  config.toml:    <no config dir resolved — using defaults>"),
    }
    println!(
        "  layout pin:     {}",
        if prov.xdg_pinned { "xdg" } else { "unpinned" }
    );

    if let Some(err) = &prov.parse_error {
        println!("  [!] parse error: config.toml is unparseable — running on DEFAULTS:");
        println!("                  {err}");
        println!("                  Run `lean-ctx doctor --fix` to repair.");
    }

    if prov.local_exists && !prov.local_keys.is_empty() {
        let path = prov
            .local_path
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_default();
        println!(
            "  [!] project-local: {path} overrides {}",
            prov.local_keys.join(", ")
        );
        println!("                  (these win over the global config for this project)");
    }

    if !prov.env_overrides.is_empty() {
        let list = prov
            .env_overrides
            .iter()
            .map(|e| format!("{} ({})", e.var, e.setting))
            .collect::<Vec<_>>()
            .join(", ");
        println!("  [!] env override: {list}");
        println!(
            "                  (these win over config.toml; unset them for saved values to apply)"
        );
    }

    if prov.has_shadow() {
        println!(
            "  -> A saved setting can appear to \"reset\" because a source above shadows it (GH #450)."
        );
    }
    println!();
}

/// Closest config key: dotted-path match within distance 3, then a
/// leaf-segment match within distance 2 (`proxy.prt` → `proxy.port`).
/// Distances come from the shared [`crate::core::levenshtein`] helper (#712);
/// the fixed budgets here predate the shared length-scaled one and are kept —
/// dotted keys are long, so a length-scaled budget would over-suggest.
fn find_closest(needle: &str, haystack: &[String]) -> Option<String> {
    use crate::core::levenshtein::levenshtein;
    let mut best: Option<(usize, &str)> = None;
    for candidate in haystack {
        let d = levenshtein(needle, candidate);
        if d <= 3 && best.as_ref().is_none_or(|(best_d, _)| d < *best_d) {
            best = Some((d, candidate));
        }
    }
    if best.is_some() {
        return best.map(|(_, s)| s.to_string());
    }
    let leaf = needle.rsplit('.').next().unwrap_or(needle);
    let mut leaf_best: Option<(usize, &str)> = None;
    for candidate in haystack {
        let cand_leaf = candidate.rsplit('.').next().unwrap_or(candidate);
        let d = levenshtein(leaf, cand_leaf);
        if d <= 2 && leaf_best.as_ref().is_none_or(|(best_d, _)| d < *best_d) {
            leaf_best = Some((d, candidate));
        }
    }
    leaf_best.map(|(_, s)| s.to_string())
}

fn normalize_optional_upstream(value: &str) -> Option<String> {
    use crate::core::config::normalize_url_opt;
    let trimmed = value.trim();
    if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("default") {
        None
    } else {
        normalize_url_opt(trimmed)
    }
}

pub fn cmd_benchmark(args: &[String]) {
    use crate::core::benchmark;

    let action = args.first().map_or("run", std::string::String::as_str);

    // `benchmark --help` is answered by `benchmark_cmd`, whose help lists
    // these subcommands as well.
    match action {
        "dual-arm" => {
            let is_json = args.iter().any(|a| a == "--json");
            let output = parse_flag_value(args, "--output");
            match crate::core::scorecard::dual_arm::run_dual_arm() {
                Ok(sc) => {
                    let rendered = if is_json { sc.to_json() } else { sc.to_human() };
                    if let Some(path) = output {
                        if let Err(e) = std::fs::write(&path, &rendered) {
                            eprintln!("Failed to write dual-arm scorecard to {path}: {e}");
                            std::process::exit(1);
                        }
                        eprintln!("Wrote dual-arm scorecard to {path}");
                    } else {
                        print!("{rendered}");
                    }
                }
                Err(e) => {
                    eprintln!("Dual-arm bench failed: {e}");
                    std::process::exit(1);
                }
            }
        }
        "scorecard" => {
            let is_json = args.iter().any(|a| a == "--json");
            let output = parse_flag_value(args, "--output");
            match crate::core::scorecard::run_scorecard() {
                Ok(sc) => {
                    let rendered = if is_json { sc.to_json() } else { sc.to_human() };
                    if let Some(path) = output {
                        if let Err(e) = std::fs::write(&path, &rendered) {
                            eprintln!("Failed to write scorecard to {path}: {e}");
                            std::process::exit(1);
                        }
                        eprintln!("Wrote scorecard to {path}");
                    } else {
                        print!("{rendered}");
                    }
                }
                Err(e) => {
                    eprintln!("Scorecard failed: {e}");
                    std::process::exit(1);
                }
            }
        }
        "run" => {
            let path = args.get(1).map_or(".", std::string::String::as_str);
            let is_json = args.iter().any(|a| a == "--json");

            let result = benchmark::run_project_benchmark(path);
            if is_json {
                println!("{}", benchmark::format_json(&result));
            } else {
                println!("{}", benchmark::format_terminal(&result));
            }
        }
        "report" => {
            let path = args.get(1).map_or(".", std::string::String::as_str);
            let result = benchmark::run_project_benchmark(path);
            println!("{}", benchmark::format_markdown(&result));
        }
        _ => {
            if std::path::Path::new(action).exists() {
                let result = benchmark::run_project_benchmark(action);
                println!("{}", benchmark::format_terminal(&result));
            } else {
                eprintln!("lean-ctx benchmark: unknown subcommand or path '{action}'");
                eprintln!(
                    "Usage: lean-ctx benchmark [--format markdown|json|text] [--output <path>] [--share]\n{}",
                    super::benchmark_cmd::SUBCOMMAND_USAGE
                );
                std::process::exit(2);
            }
        }
    }
}

fn parse_flag_value(args: &[String], flag: &str) -> Option<String> {
    args.iter()
        .position(|a| a == flag)
        .and_then(|i| args.get(i + 1))
        .cloned()
}

fn load_mcp_live_stats() -> Option<serde_json::Value> {
    let path = crate::core::paths::state_dir().ok()?.join("mcp-live.json");
    let content = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&content).ok()
}

fn stats_json_value(
    store: &crate::core::stats::StatsStore,
    mcp_cache: Option<serde_json::Value>,
) -> serde_json::Value {
    let mut value = serde_json::to_value(store).unwrap_or_else(|_| serde_json::json!({}));
    if let (Some(object), Some(mcp_cache)) = (value.as_object_mut(), mcp_cache) {
        object.insert("mcp_cache".to_string(), mcp_cache);
    }
    value
}

fn mcp_cache_stats_lines(value: &serde_json::Value) -> Vec<String> {
    let metric = |key| {
        value
            .get(key)
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0)
    };
    let mcp_reads = metric("total_reads");
    let mcp_hits = metric("cache_hits");
    let mcp_rate = if mcp_reads > 0 {
        (mcp_hits as f64 / mcp_reads as f64 * 100.0).round() as u32
    } else {
        0
    };
    let updated = value
        .get("updated_at")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("unknown");
    let mut lines = vec![
        "MCP Session Cache (ctx_read via AI editor):".to_string(),
        format!("  Reads:         {mcp_reads}"),
        format!("  Hits:          {mcp_hits}"),
        format!("  Hit Rate:      {mcp_rate}%"),
        format!("  Tokens Saved:  {}", metric("tokens_saved")),
        format!("  Last Updated:  {updated}"),
    ];

    let dedup_reads = metric("dedup_reads");
    if dedup_reads > 0 {
        let dedup_hits = metric("dedup_hits");
        let dedup_rate = dedup_hits as f64 / dedup_reads as f64 * 100.0;
        lines.extend([
            String::new(),
            "Content Dedup (repeated ctx_read output):".to_string(),
            format!("  Reads Checked: {dedup_reads}"),
            format!("  Hits:          {dedup_hits}"),
            format!("  Hit Rate:      {dedup_rate:.1}%"),
            format!("  Tokens Saved:  {}", metric("dedup_tokens_saved")),
        ]);
    }

    lines
}

pub fn cmd_stats(args: &[String]) {
    match args.first().map(std::string::String::as_str) {
        Some("reset-cep") => {
            crate::core::stats::reset_cep();
            println!("CEP stats reset. Shell hook data preserved.");
        }
        Some("json") => {
            let store = crate::core::stats::load();
            let value = stats_json_value(&store, load_mcp_live_stats());
            println!(
                "{}",
                serde_json::to_string_pretty(&value).unwrap_or_else(|_| "{}".to_string())
            );
        }
        _ => {
            let store = crate::core::stats::load();
            let input_saved = store
                .total_input_tokens
                .saturating_sub(store.total_output_tokens);
            let pct = if store.total_input_tokens > 0 {
                input_saved as f64 / store.total_input_tokens as f64 * 100.0
            } else {
                0.0
            };
            println!("Commands:    {}", store.total_commands);
            println!("Input:       {} tokens", store.total_input_tokens);
            println!("Output:      {} tokens", store.total_output_tokens);
            // #1284: scope the headline honestly — this ratio covers volume
            // that flowed THROUGH lean-ctx. Native tool calls that bypassed
            // lean-ctx are not in the denominator (they are counted separately
            // as native_shell_passthrough in metering).
            println!("Saved:       {input_saved} tokens ({pct:.1}% of metered lean-ctx volume)");
            println!();
            println!("CEP sessions:  {}", store.cep.sessions);
            println!(
                "CEP tokens:    {} → {}",
                store.cep.total_tokens_original, store.cep.total_tokens_compressed
            );
            println!();
            println!("Subcommands: stats reset-cep | stats json");
        }
    }
}

pub struct PruneResult {
    pub scanned: u32,
    pub removed: u32,
    pub failed: u32,
    pub bytes_freed: u64,
}

fn record_removed_file(result: &mut PruneResult, path: &std::path::Path, label: &str) {
    let bytes = std::fs::metadata(path).map_or(0, |meta| meta.len());

    match std::fs::remove_file(path) {
        Ok(()) => {
            result.bytes_freed += bytes;
            result.removed += 1;
            println!("  {label}: {}", path.display());
        }
        Err(error) => {
            result.failed += 1;
            eprintln!("  Failed to remove {}: {error}", path.display());
        }
    }
}

fn record_removed_dir(
    result: &mut PruneResult,
    path: &std::path::Path,
    bytes: u64,
    message: impl FnOnce() -> String,
) {
    match std::fs::remove_dir_all(path) {
        Ok(()) => {
            result.bytes_freed += bytes;
            result.removed += 1;
            println!("  {}", message());
        }
        Err(error) => {
            result.failed += 1;
            eprintln!("  Failed to remove {}: {error}", path.display());
        }
    }
}

/// Files a local (per-checkout) index writes into its directory. Anything
/// else there, such as `hosted-v2/`, is not the local index's to delete.
const LOCAL_INDEX_FILES: &[&str] = &[
    "bm25_index.bin.zst",
    "bm25_index.bin",
    "bm25_index.json",
    "embeddings.json",
    "embeddings.bin",
    "bm25_provider_origins.json",
    "project_root.txt",
];

pub fn prune_bm25_caches() -> PruneResult {
    let mut result = PruneResult {
        scanned: 0,
        removed: 0,
        failed: 0,
        bytes_freed: 0,
    };

    let Ok(data_dir) = crate::core::data_dir::lean_ctx_data_dir() else {
        return result;
    };
    let vectors_dir = data_dir.join("vectors");
    let Ok(entries) = std::fs::read_dir(&vectors_dir) else {
        return result;
    };

    let max_bytes = crate::core::config::Config::load().bm25_max_cache_mb_effective() * 1024 * 1024;

    for entry in entries.flatten() {
        let dir = entry.path();
        if !dir.is_dir() {
            continue;
        }
        result.scanned += 1;

        for q_name in &[
            "bm25_index.json.quarantined",
            "bm25_index.bin.quarantined",
            "bm25_index.bin.zst.quarantined",
        ] {
            let quarantined = dir.join(q_name);
            if quarantined.exists() {
                record_removed_file(&mut result, &quarantined, "Removed quarantined");
            }
        }

        let index_path = if dir.join("bm25_index.bin.zst").exists() {
            dir.join("bm25_index.bin.zst")
        } else if dir.join("bm25_index.bin").exists() {
            dir.join("bm25_index.bin")
        } else {
            dir.join("bm25_index.json")
        };
        if let Ok(meta) = std::fs::metadata(&index_path)
            && meta.len() > max_bytes
        {
            record_removed_file(&mut result, &index_path, "Removed oversized");
        }

        let marker = dir.join("project_root.txt");
        if let Ok(root_str) = std::fs::read_to_string(&marker) {
            let root_path = std::path::Path::new(root_str.trim());
            if !root_path.exists() && dir.join("hosted-v2").exists() {
                // The namespace directory also holds the hosted index of
                // every checkout of this repository: only the local index
                // files of the gone checkout go.
                for name in LOCAL_INDEX_FILES {
                    let file = dir.join(name);
                    if file.exists() {
                        record_removed_file(&mut result, &file, "Removed orphaned");
                    }
                }
            } else if !root_path.exists() {
                let freed = dir_size(&dir);
                record_removed_dir(&mut result, &dir, freed, || {
                    format!(
                        "Removed orphaned ({:.1} MB, project gone: {}): {}",
                        freed as f64 / 1_048_576.0,
                        root_str.trim(),
                        dir.display()
                    )
                });
            }
        }
    }

    result
}

pub fn prune_graph_caches() -> PruneResult {
    let mut result = PruneResult {
        scanned: 0,
        removed: 0,
        failed: 0,
        bytes_freed: 0,
    };

    let Ok(data_dir) = crate::core::data_dir::lean_ctx_data_dir() else {
        return result;
    };
    let graphs_dir = data_dir.join("graphs");
    let Ok(entries) = std::fs::read_dir(&graphs_dir) else {
        return result;
    };

    for entry in entries.flatten() {
        let dir = entry.path();
        if !dir.is_dir() {
            continue;
        }
        result.scanned += 1;

        // #696 C4: the property graph (graph.db + graph.meta.json) is the sole
        // store. The meta carries the absolute project root, so an orphaned
        // `graphs/<hash>/` dir (project deleted) can still be pruned.
        let meta_file = dir.join("graph.meta.json");
        let db_file = dir.join("graph.db");
        if !meta_file.exists() && !db_file.exists() {
            continue;
        }

        let root_from_meta = try_read_project_root_from_graph(&meta_file);
        if let Some(root) = root_from_meta
            && !root.is_empty()
            && !std::path::Path::new(&root).exists()
        {
            let freed = dir_size(&dir);
            record_removed_dir(&mut result, &dir, freed, || {
                format!(
                    "Removed orphaned graph ({:.1} MB, project gone: {}): {}",
                    freed as f64 / 1_048_576.0,
                    root,
                    dir.display()
                )
            });
            continue;
        }

        // Oversized guard: a pathologically large (e.g. corrupt) graph store is
        // dropped so the next query rebuilds it cleanly — a rebuild cost, not
        // data loss.
        if let Ok(meta) = std::fs::metadata(&db_file)
            && meta.len() > 100 * 1024 * 1024
        {
            let freed = dir_size(&dir);
            record_removed_dir(&mut result, &dir, freed, || {
                format!(
                    "Removed oversized graph ({:.1} MB): {}",
                    freed as f64 / 1_048_576.0,
                    dir.display()
                )
            });
        }
    }

    result
}

/// Read the absolute project root recorded in a `graph.meta.json` file, if
/// present (#696 C4 — replaces reading it from the retired JSON index).
fn try_read_project_root_from_graph(path: &std::path::Path) -> Option<String> {
    let content = std::fs::read_to_string(path).ok()?;
    let val: serde_json::Value = serde_json::from_str(&content).ok()?;
    val.get("project_root")?.as_str().map(String::from)
}

/// Create the simplified config. An existing one is never replaced: the
/// template would reset every setting the person tightened.
fn write_simplified_config() -> Result<String, String> {
    use std::io::Write as _;
    let path = config::Config::path().ok_or_else(|| "Cannot determine config path".to_string())?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{e}"))?;
    }
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::AlreadyExists {
                format!(
                    "{} exists and stays as it is; `lean-ctx config init --full` merges into it, `lean-ctx config set` changes one key",
                    path.display()
                )
            } else {
                format!("{e}")
            }
        })?;
    file.write_all(config::SIMPLIFIED_TEMPLATE.as_bytes())
        .map_err(|e| format!("{e}"))?;
    Ok(path.to_string_lossy().to_string())
}

fn cmd_show_effective() {
    let cfg = config::Config::load();
    let compression = config::CompressionLevel::effective(&cfg);
    let policy = cfg.memory_policy_effective().unwrap_or_default();

    println!("{}", box_top("Simplified (high-level)"));
    box_row(&format!(
        " compression_level   = {:10}  {}",
        format!("{compression:?}"),
        source_hint(
            "LEAN_CTX_COMPRESSION",
            cfg.compression_level != config::CompressionLevel::Off
        )
    ));
    box_row(&format!(
        " max_disk_mb         = {:10}  {}",
        cfg.max_disk_mb_effective(),
        source_hint("LEAN_CTX_MAX_DISK_MB", cfg.max_disk_mb > 0)
    ));
    box_row(&format!(
        " max_ram_percent     = {:10}  {}",
        cfg.max_ram_percent,
        source_hint("LEAN_CTX_MAX_RAM_PERCENT", cfg.max_ram_percent != 5)
    ));
    box_row(&format!(
        " mcp_max_rss_mb      = {:10}  {}",
        cfg.mcp_max_rss_mb_effective(),
        source_hint("LEAN_CTX_MCP_MAX_RSS_MB", cfg.mcp_max_rss_mb != 512)
    ));
    box_row(&format!(
        " daemon_max_rss_mb   = {:10}  {}",
        cfg.daemon_max_rss_mb_effective(),
        source_hint("LEAN_CTX_DAEMON_MAX_RSS_MB", cfg.daemon_max_rss_mb != 2048)
    ));
    box_row(&format!(
        " mcp_idle_exit_minutes = {:8}  {}",
        cfg.mcp_idle_exit_minutes_effective(),
        source_hint(
            "LEAN_CTX_MCP_IDLE_EXIT_MINUTES",
            cfg.mcp_idle_exit_minutes > 0
        )
    ));
    box_row(&format!(
        " session_retention_days = {:8}  {}",
        cfg.session_retention_days_effective(),
        source_hint(
            "LEAN_CTX_SESSION_RETENTION_DAYS",
            cfg.session_retention_days != 7
        )
    ));
    box_row(&format!(
        " max_staleness_days  = {:10}  {}",
        cfg.max_staleness_days_effective(),
        source_hint("LEAN_CTX_MAX_STALENESS_DAYS", cfg.max_staleness_days > 0)
    ));
    box_row(&format!(
        " memory_profile      = {:10}  {}",
        format!("{:?}", cfg.memory_profile),
        source_hint("LEAN_CTX_MEMORY_PROFILE", false)
    ));
    println!("{}", box_bottom());

    println!();
    println!("{}", box_top("Derived effective limits"));
    box_row(&format!(
        " archive_max_disk_mb    = {:>6} MB",
        cfg.archive_max_disk_mb_effective()
    ));
    box_row(&format!(
        " bm25_max_cache_mb      = {:>6} MB",
        cfg.bm25_max_cache_mb_effective()
    ));
    box_row(&format!(
        " archive_max_age_hours  = {:>6} h",
        cfg.archive_max_age_hours_effective()
    ));
    box_row(&format!(
        " graph_index_max_files  = {:>6}",
        cfg.graph_index_max_files
    ));
    box_row("");
    box_row(" memory.knowledge facts         = no limit");
    box_row(&format!(
        " memory.knowledge.max_patterns  = {:>6}",
        policy.knowledge.max_patterns
    ));
    println!("{}", box_bottom());

    if cfg.max_disk_mb_effective() > 0 {
        println!();
        println!(
            "  ℹ  max_disk_mb={} → limits scaled proportionally (factor: {:.1}x)",
            cfg.max_disk_mb_effective(),
            (cfg.max_disk_mb_effective() as f64 / 500.0).clamp(0.5, 10.0)
        );
    }
}

/// Interior width of the `config show` frames, in terminal columns.
const SHOW_BOX_W: usize = 60;

/// `╭─── Label ───…───╮`, always `SHOW_BOX_W` columns wide inside the corners.
fn box_top(label: &str) -> String {
    let head = format!("─── {label} ");
    let fill = SHOW_BOX_W.saturating_sub(crate::core::theme::visual_len(&head));
    format!("╭{head}{}╮", "─".repeat(fill))
}

fn box_bottom() -> String {
    format!("╰{}╯", "─".repeat(SHOW_BOX_W))
}

/// Print one framed row, padded (or truncated) to the frame's interior width so
/// the right border lines up regardless of the value's length.
fn box_row(content: &str) {
    println!("│{}│", crate::core::theme::pad_right(content, SHOW_BOX_W));
}

fn source_hint(env_var: &str, config_set: bool) -> &'static str {
    if std::env::var(env_var).is_ok() {
        "← env"
    } else if config_set {
        "← config"
    } else {
        "← default"
    }
}

fn dir_size(path: &std::path::Path) -> u64 {
    let mut total = 0u64;
    if let Ok(entries) = std::fs::read_dir(path) {
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_file() {
                total += std::fs::metadata(&p).map_or(0, |m| m.len());
            } else if p.is_dir() {
                total += dir_size(&p);
            }
        }
    }
    total
}

#[cfg(test)]
mod tests;
