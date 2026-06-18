use std::collections::HashMap;
use std::sync::Mutex;

use crate::core::cache::SessionCache;
use crate::core::context_ledger::PressureAction;
use crate::core::mode_predictor::{FileSignature, ModePredictor};

/// Per-process counters of which signal decided each auto-mode resolution.
/// Surfaced by `ctx_metrics` so the learning loops are observable (#496).
static SOURCE_COUNTS: Mutex<Option<HashMap<&'static str, u64>>> = Mutex::new(None);

fn count_source(source: &'static str) {
    if let Ok(mut guard) = SOURCE_COUNTS.lock() {
        *guard
            .get_or_insert_with(HashMap::new)
            .entry(source)
            .or_insert(0) += 1;
    }
}

/// Snapshot of auto-mode decision sources, sorted by count descending.
pub fn source_counts() -> Vec<(&'static str, u64)> {
    let Ok(guard) = SOURCE_COUNTS.lock() else {
        return Vec::new();
    };
    let mut items: Vec<(&'static str, u64)> = guard
        .as_ref()
        .map(|m| m.iter().map(|(k, v)| (*k, *v)).collect())
        .unwrap_or_default();
    items.sort_by_key(|(_, n)| std::cmp::Reverse(*n));
    items
}

fn sources_path() -> Option<std::path::PathBuf> {
    crate::core::data_dir::lean_ctx_data_dir()
        .ok()
        .map(|d| d.join("auto_mode_sources.json"))
}

/// Persist the in-process counters by *adding* them into the cumulative
/// on-disk file, then reset the process counters. The counters live in the
/// MCP/CLI process — the dashboard is a separate process and can only see
/// them through this file (#505).
pub fn flush_sources() {
    let drained: Vec<(String, u64)> = {
        let Ok(mut guard) = SOURCE_COUNTS.lock() else {
            return;
        };
        match guard.take() {
            Some(m) if !m.is_empty() => m.into_iter().map(|(k, v)| (k.to_string(), v)).collect(),
            _ => return,
        }
    };
    let Some(path) = sources_path() else {
        return;
    };
    let mut on_disk: HashMap<String, u64> = std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    for (k, v) in drained {
        *on_disk.entry(k).or_insert(0) += v;
    }
    let Ok(json) = serde_json::to_string_pretty(&on_disk) else {
        return;
    };
    let tmp = path.with_extension("json.tmp");
    if std::fs::write(&tmp, json).is_ok() {
        let _ = std::fs::rename(&tmp, &path);
    }
}

/// Cumulative auto-mode decision sources from disk (all processes, all time),
/// sorted by count descending. Used by the dashboard's Live Signals panel.
pub fn persisted_source_counts() -> Vec<(String, u64)> {
    let Some(path) = sources_path() else {
        return Vec::new();
    };
    let map: HashMap<String, u64> = std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    let mut items: Vec<(String, u64)> = map.into_iter().collect();
    items.sort_by_key(|(_, n)| std::cmp::Reverse(*n));
    items
}

pub struct AutoModeContext<'a> {
    pub path: &'a str,
    pub token_count: usize,
    pub task: Option<&'a str>,
    pub cache: Option<&'a SessionCache>,
}

pub struct ResolvedMode {
    pub mode: String,
    pub source: &'static str,
}

/// Single entry point for auto-mode resolution.
/// Merges Pipeline A (select_mode_with_task) and Pipeline B (resolve_auto_mode).
pub fn resolve(ctx: &AutoModeContext) -> ResolvedMode {
    // Quality loop (#494), signal 1: an edit on this file just failed after a
    // compressed read — the agent needs the real body now, one-shot.
    if crate::core::edit_quality::take_pending_escalation(ctx.path) {
        return resolved("full", "edit_fail_escalation");
    }

    let r = resolve_inner(ctx);

    // Quality loop (#494), signal 2: this mode keeps producing edit failures
    // for this file type — compression here is a proven net loss, use full.
    if r.mode != "full" && crate::core::edit_quality::is_risky_mode(ctx.path, &r.mode) {
        return resolved("full", "edit_quality_penalty");
    }
    r
}

fn resolve_inner(ctx: &AutoModeContext) -> ResolvedMode {
    if crate::tools::ctx_read::is_instruction_file(ctx.path) {
        return resolved("full", "instruction_file");
    }

    if crate::core::binary_detect::is_binary_file(ctx.path) {
        return resolved("full", "binary");
    }

    if let Some(cache) = ctx.cache
        && let Some(cached) = cache.get(ctx.path)
    {
        if !file_unchanged(ctx.path, cached) {
            return resolved("diff", "cache_changed");
        }
        // Unchanged. Resolving to "full" is only a *cheap* stub hit when full
        // content was actually delivered before. If the first read was a
        // compressed mode (map/signatures), `full_content_delivered` is false,
        // so forcing "full" here re-delivers the entire file on the very next
        // read — a compression bounce that costs *more* tokens than the first
        // read and collapses the cache hit rate: the 2nd read of every file
        // blows up to full and stub hits only begin at the 3rd read (which
        // agents rarely reach). Only short-circuit once full was delivered;
        // otherwise fall through to the predictor, which deterministically
        // reproduces the cached compressed mode and serves it from the
        // compressed-output cache as a cheap, consistent hit.
        if cache.is_full_delivered(ctx.path) {
            return resolved("full", "cache_hit");
        }
    }

    if ctx.token_count <= 200 {
        return resolved("full", "small_file");
    }

    let ext = std::path::Path::new(ctx.path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("");

    if is_config_or_data(ext, ctx.path) {
        return resolved("full", "config_data");
    }

    if let Ok(bt) = crate::core::bounce_tracker::global().lock()
        && bt.should_force_full(ctx.path)
    {
        return resolved("full", "bounce_tracker");
    }

    // Per-path long-term memory (#496): a file that historically bounced in
    // the majority of its reads will bounce again — compression is a proven
    // net loss for it, across process restarts.
    if crate::core::path_mode_memory::should_force_full(ctx.path) {
        return resolved("full", "path_bounce_memory");
    }

    // Active compiler error (#499): the agent reads this file to fix the
    // build — compressed modes would hide the error region.
    if crate::core::diagnostics_store::has_error(ctx.path) {
        return resolved("full", "active_diagnostic");
    }

    // Suspect file (#361 capability): the task explicitly names this file
    // (e.g. "fix the version sort in versioncmp.c"), so the agent is about to
    // inspect it for the defect. Keep the full body it needs to localize and
    // edit, ahead of any task-type intent default that might compress it.
    if task_names_file(ctx.task, ctx.path) {
        return resolved("full", "task_suspect_file");
    }

    if let Some(mode) = intent_recommended_mode(ctx.task) {
        return resolved(&mode, "intent");
    }

    let sig = FileSignature::from_path(ctx.path, ctx.token_count);
    let predictor = ModePredictor::new();
    let mut predicted = predictor
        .predict_best_mode(&sig)
        .unwrap_or_else(|| "full".to_string());
    if predicted == "auto" {
        predicted = "full".to_string();
    }

    if predicted != "full"
        && let Some(bandit_override) = bandit_explore(ctx.path, ctx.token_count)
    {
        predicted = bandit_override;
    }

    // Heatmap signal (#496): a frequently-read file where compression barely
    // saves anything will likely trigger a follow-up read — step one mode more
    // conservative. avg_compression_ratio is the historical fraction saved.
    if predicted != "full"
        && let Some((access_count, avg_ratio)) = crate::core::heatmap::entry_stats(ctx.path)
        && access_count >= 5
        && avg_ratio < 0.30
    {
        let conservative = match predicted.as_str() {
            "signatures" | "aggressive" | "entropy" => "map".to_string(),
            "map" if ctx.token_count <= 6000 => "full".to_string(),
            other => other.to_string(),
        };
        if conservative != predicted {
            return resolved(&conservative, "heatmap_conservative");
        }
    }

    let policy = crate::core::adaptive_mode_policy::AdaptiveModePolicyStore::load();
    let chosen = policy.choose_auto_mode(ctx.task, &predicted);

    if ctx.token_count > 2000 {
        if (predicted == "map" || predicted == "signatures")
            && chosen != "map"
            && chosen != "signatures"
        {
            return resolved(&predicted, "predictor_guard");
        }
        if chosen == "full" && predicted != "full" {
            return resolved(&predicted, "predictor_override");
        }
    }

    if chosen != predicted {
        return resolved(&chosen, "adaptive_policy");
    }

    if predicted != "full" {
        return resolved(&predicted, "predictor");
    }

    // Cold-read fallback. Every read that reaches here missed the session cache
    // (a warm hit returns `full`/`diff` above), so on a phase-isolated harness
    // there is no warm re-read to amortize a `full` cold read. `structure_first`
    // lets such a host opt into a lower `map` floor for medium code files; all
    // capability guards (diagnostic / edit-fail / bounce / intent) already ran
    // above, and the anti-inflation guarantee keeps `map` break-even at worst.
    let structure_first = crate::core::config::Config::load().structure_first_effective();
    let heuristic = heuristic_mode(ext, ctx.token_count, structure_first);
    let source = if structure_first && heuristic == "map" && ctx.token_count <= 6000 {
        "structure_first"
    } else {
        "heuristic"
    };
    resolved(&heuristic, source)
}

/// Unified pressure downgrade table.
/// Used by both context_gate and intent_router pressure paths.
pub fn pressure_downgrade(requested_mode: &str, action: &PressureAction) -> Option<String> {
    match action {
        PressureAction::SuggestCompression => match requested_mode {
            "auto" | "full" => Some("map".to_string()),
            _ => None,
        },
        PressureAction::ForceCompression => match requested_mode {
            "full" => Some("map".to_string()),
            "auto" | "map" => Some("signatures".to_string()),
            _ => None,
        },
        PressureAction::EvictLeastRelevant => match requested_mode {
            "full" => Some("map".to_string()),
            "auto" | "map" => Some("signatures".to_string()),
            "signatures" => Some("reference".to_string()),
            _ => None,
        },
        PressureAction::NoAction => None,
    }
}

/// True when the task text explicitly names this file (basename match). A real
/// filename mention ("versioncmp.c") is a strong suspect signal for a bug-fix;
/// requiring an extension-bearing, non-trivial basename keeps it precise — the
/// bare stem in "improve the parser" must not match `parser.rs`. A rare false
/// positive only costs a little compression on a file the user literally named,
/// so the failure mode is capability-safe.
fn task_names_file(task: Option<&str>, path: &str) -> bool {
    let Some(task) = task else {
        return false;
    };
    let basename = std::path::Path::new(path)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("");
    if basename.len() < 4 || !basename.contains('.') {
        return false;
    }
    task.to_ascii_lowercase()
        .contains(&basename.to_ascii_lowercase())
}

fn intent_recommended_mode(task: Option<&str>) -> Option<String> {
    let task_desc = task?;
    let classification = crate::core::intent_engine::classify(task_desc);
    if classification.confidence < 0.4 {
        return None;
    }
    let route = crate::core::intent_engine::route_intent(task_desc, &classification);
    let mode =
        crate::core::intent_router::read_mode_for_tier(route.model_tier, classification.task_type);
    if mode == "auto" {
        return None;
    }
    Some(mode)
}

fn bandit_explore(file_path: &str, token_count: usize) -> Option<String> {
    let project_root =
        crate::core::session::SessionState::load_latest().and_then(|s| s.project_root)?;
    let ext = std::path::Path::new(file_path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("");
    let bucket = match token_count {
        0..=2000 => "sm",
        2001..=10000 => "md",
        10001..=50000 => "lg",
        _ => "xl",
    };
    let bandit_key = format!("{ext}_{bucket}");
    let mut store = crate::core::bandit::BanditStore::load(&project_root);
    let bandit = store.get_or_create(&bandit_key);
    let arm = bandit.select_arm();
    if arm.budget_ratio < 0.25 && token_count > 2000 {
        Some("aggressive".to_string())
    } else {
        None
    }
}

fn heuristic_mode(ext: &str, token_count: usize, structure_first: bool) -> String {
    if token_count > 8000 {
        if is_code(ext) {
            return "map".to_string();
        }
        return "aggressive".to_string();
    }
    // Raised from 3000 → 6000: at 3-6k tokens, returning only signatures forces
    // the agent into a follow-up full/lines read for the body it actually
    // needs. Keeping `full` here trades a few hundred tokens per call for
    // fewer round-trips — the right call per the total-task-token principle.
    if token_count > 6000 && is_code(ext) {
        return "map".to_string();
    }
    // Structure-first cold-read floor (#361): on a phase-isolated harness a cold
    // `full` read never amortizes, so medium code files default to `map`
    // (deps + exports + key signatures) — cheaper and a better localization
    // surface. `map` keeps far more than `signatures` (no empty bodies), so the
    // follow-up-read risk that justifies the 6000 floor above is much lower; the
    // 500-token floor stays above the trivial files where `full` is already best.
    if structure_first && token_count > 500 && is_code(ext) {
        return "map".to_string();
    }
    "full".to_string()
}

/// Fast O(1) staleness check: if the file's mtime still matches what was
/// stored when the cache entry was created, the content is unchanged — no need
/// to read the file or compute any hash. Falls back to "changed" when metadata
/// is unavailable (e.g. file deleted) or when the cache entry predates mtime
/// tracking (legacy entries with `stored_mtime = None`).
///
/// mtime comparison is sufficient for correctness on all major filesystems:
/// every `write(2)` / `truncate(2)` updates mtime (POSIX guarantee).
fn file_unchanged(path: &str, cached: &crate::core::cache::CacheEntry) -> bool {
    let Some(stored_mtime) = cached.stored_mtime else {
        return false;
    };
    let Ok(meta) = std::fs::metadata(path) else {
        return false;
    };
    let Ok(current_mtime) = meta.modified() else {
        return false;
    };
    current_mtime == stored_mtime
}

fn is_code(ext: &str) -> bool {
    matches!(
        ext,
        "rs" | "ts"
            | "tsx"
            | "js"
            | "jsx"
            | "py"
            | "go"
            | "java"
            | "c"
            | "cpp"
            | "cc"
            | "h"
            | "hpp"
            | "rb"
            | "cs"
            | "kt"
            | "swift"
            | "php"
            | "zig"
            | "ex"
            | "exs"
            | "scala"
            | "sc"
            | "dart"
            | "sh"
            | "bash"
            | "svelte"
            | "vue"
    )
}

fn is_config_or_data(ext: &str, path: &str) -> bool {
    if matches!(ext, "xml" | "ini" | "cfg" | "env") {
        return true;
    }
    let name = std::path::Path::new(path)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("");
    matches!(
        name,
        "Cargo.toml"
            | "package.json"
            | "tsconfig.json"
            | "Makefile"
            | "Dockerfile"
            | "docker-compose.yml"
            | ".gitignore"
            | ".env"
            | "pyproject.toml"
            | "go.mod"
            | "build.gradle"
            | "pom.xml"
    )
}

fn resolved(mode: &str, source: &'static str) -> ResolvedMode {
    count_source(source);
    ResolvedMode {
        mode: mode.to_string(),
        source,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pressure_suggest_full_to_map() {
        assert_eq!(
            pressure_downgrade("full", &PressureAction::SuggestCompression),
            Some("map".to_string())
        );
    }

    #[test]
    fn pressure_suggest_auto_to_map() {
        assert_eq!(
            pressure_downgrade("auto", &PressureAction::SuggestCompression),
            Some("map".to_string())
        );
    }

    #[test]
    fn pressure_suggest_does_not_touch_signatures() {
        assert!(pressure_downgrade("signatures", &PressureAction::SuggestCompression).is_none());
    }

    #[test]
    fn pressure_force_full_to_map() {
        assert_eq!(
            pressure_downgrade("full", &PressureAction::ForceCompression),
            Some("map".to_string())
        );
    }

    #[test]
    fn pressure_force_map_to_signatures() {
        assert_eq!(
            pressure_downgrade("map", &PressureAction::ForceCompression),
            Some("signatures".to_string())
        );
    }

    #[test]
    fn pressure_evict_signatures_to_reference() {
        assert_eq!(
            pressure_downgrade("signatures", &PressureAction::EvictLeastRelevant),
            Some("reference".to_string())
        );
    }

    #[test]
    fn pressure_noaction_returns_none() {
        assert!(pressure_downgrade("full", &PressureAction::NoAction).is_none());
    }

    #[test]
    fn flush_sources_merges_additively_into_disk_file() {
        let _lock = crate::core::data_dir::test_env_lock();
        let dir = std::env::temp_dir().join(format!("lctx-amr-flush-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        crate::test_env::set_var("LEAN_CTX_DATA_DIR", dir.to_str().unwrap());
        let _ = std::fs::remove_file(dir.join("auto_mode_sources.json"));

        // Unique test-only keys: parallel resolve() tests count real sources
        // into the same process-global map, so shared keys would be flaky.
        count_source("test_flush_alpha");
        count_source("test_flush_alpha");
        count_source("test_flush_beta");
        flush_sources();

        count_source("test_flush_alpha");
        flush_sources();

        let persisted = persisted_source_counts();
        let get = |k: &str| {
            persisted
                .iter()
                .find(|(s, _)| s == k)
                .map_or(0, |(_, n)| *n)
        };
        assert_eq!(
            get("test_flush_alpha"),
            3,
            "two flushes must merge additively"
        );
        assert_eq!(get("test_flush_beta"), 1);

        crate::test_env::remove_var("LEAN_CTX_DATA_DIR");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn small_file_always_full() {
        let ctx = AutoModeContext {
            path: "test.rs",
            token_count: 100,
            task: None,
            cache: None,
        };
        let result = resolve(&ctx);
        assert_eq!(result.mode, "full");
        assert_eq!(result.source, "small_file");
    }

    #[test]
    fn config_file_returns_full() {
        let ctx = AutoModeContext {
            path: "config.ini",
            token_count: 500,
            task: None,
            cache: None,
        };
        let result = resolve(&ctx);
        assert_eq!(result.mode, "full");
        assert_eq!(result.source, "config_data");
    }

    #[test]
    fn cached_compressed_only_file_does_not_escalate_to_full() {
        // Cache regression: a file first read in a compressed mode has
        // `full_content_delivered=false`. Resolving its re-read to "full" would
        // re-deliver the entire file on the 2nd read — a compression bounce that
        // costs more tokens than the first read and defeats the cache. The
        // resolver must fall through so the cached compressed mode is reused.
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("medium.rs");
        let body = "fn placeholder() { let _ = 1; }\n".repeat(400);
        std::fs::write(&file, &body).unwrap();
        let path = file.to_str().unwrap();

        let mut cache = SessionCache::new();
        cache.store(path, &body);
        // A compressed first read does NOT mark full content as delivered.

        let ctx = AutoModeContext {
            path,
            token_count: 3000,
            task: None,
            cache: Some(&cache),
        };
        let result = resolve(&ctx);
        assert_ne!(
            result.mode, "full",
            "compressed-only cached file must not escalate to full on re-read"
        );
        assert_ne!(result.source, "cache_hit");
    }

    #[test]
    fn cached_full_delivered_file_short_circuits_to_stub() {
        // Once full content was actually delivered, the cache_hit shortcut still
        // applies: a re-read resolves to "full" (a cheap `[unchanged]` stub).
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("medium.rs");
        let body = "fn placeholder() { let _ = 1; }\n".repeat(400);
        std::fs::write(&file, &body).unwrap();
        let path = file.to_str().unwrap();

        let mut cache = SessionCache::new();
        cache.store(path, &body);
        cache.mark_full_delivered(path);

        let ctx = AutoModeContext {
            path,
            token_count: 3000,
            task: None,
            cache: Some(&cache),
        };
        let result = resolve(&ctx);
        assert_eq!(result.mode, "full");
        assert_eq!(result.source, "cache_hit");
    }

    #[test]
    fn intent_explore_returns_map() {
        let ctx = AutoModeContext {
            path: "large.rs",
            token_count: 5000,
            task: Some("how does the cache work?"),
            cache: None,
        };
        let result = resolve(&ctx);
        assert_eq!(result.mode, "map");
        assert_eq!(result.source, "intent");
    }

    #[test]
    fn task_names_file_matches_explicit_filename() {
        assert!(task_names_file(
            Some("fix the version sort in versioncmp.c"),
            "src/versioncmp.c"
        ));
        assert!(task_names_file(
            Some("why does graph.ts loop?"),
            "web/src/graph.ts"
        ));
    }

    #[test]
    fn task_names_file_ignores_bare_stems_and_trivia() {
        // A bare stem mention must not match the file.
        assert!(!task_names_file(
            Some("improve the parser"),
            "src/parser.rs"
        ));
        assert!(!task_names_file(None, "src/parser.rs"));
        // Trivial / extension-less basenames are excluded.
        assert!(!task_names_file(Some("touch a.c"), "a.c"));
        assert!(!task_names_file(Some("look at Makefile"), "Makefile"));
    }

    #[test]
    fn task_suspect_file_overrides_intent() {
        let _lock = crate::core::data_dir::test_env_lock();
        let dir = std::env::temp_dir().join(format!("lctx-amr-suspect-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        crate::test_env::set_var("LEAN_CTX_DATA_DIR", dir.to_str().unwrap());

        // An explore-style task that would otherwise map (cf.
        // intent_explore_returns_map) — but it names the file, so the suspect
        // guard keeps the full body for localization.
        let ctx = AutoModeContext {
            path: "large.rs",
            token_count: 5000,
            task: Some("how does large.rs build the cache?"),
            cache: None,
        };
        let result = resolve(&ctx);
        assert_eq!(result.mode, "full");
        assert_eq!(result.source, "task_suspect_file");

        crate::test_env::remove_var("LEAN_CTX_DATA_DIR");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn heuristic_full_for_medium_code_by_default() {
        // Default (structure_first off): medium code stays full so the agent
        // gets the body in one round-trip on a warm, re-readable session.
        assert_eq!(heuristic_mode("rs", 1500, false), "full");
        assert_eq!(heuristic_mode("ts", 1000, false), "full");
    }

    #[test]
    fn heuristic_structure_first_maps_medium_code() {
        // Structure-first: medium code becomes `map` on a cold read.
        assert_eq!(heuristic_mode("rs", 1500, true), "map");
        assert_eq!(heuristic_mode("c", 800, true), "map");
    }

    #[test]
    fn heuristic_structure_first_keeps_tiny_and_prose_full() {
        // Below the 500-token floor `full` is already best.
        assert_eq!(heuristic_mode("rs", 400, true), "full");
        // Non-code (prose / data) is never structure-first mapped.
        assert_eq!(heuristic_mode("md", 4000, true), "full");
        assert_eq!(heuristic_mode("txt", 1000, true), "full");
    }

    #[test]
    fn heuristic_large_code_maps_regardless() {
        assert_eq!(heuristic_mode("rs", 9000, false), "map");
        assert_eq!(heuristic_mode("rs", 9000, true), "map");
    }

    /// Bug-fix read pattern: while localizing a planted defect the agent reads
    /// many medium source files cold. With structure_first the resolver returns
    /// `map` (cheap, localization-friendly) instead of an un-amortized `full`,
    /// while every capability guard still takes precedence because it runs
    /// before this fallback (here: the small-file guard keeps a tiny file full).
    #[test]
    fn structure_first_resolve_bugfix_cold_read() {
        let _lock = crate::core::data_dir::test_env_lock();
        let dir = std::env::temp_dir().join(format!("lctx-amr-sf-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        crate::test_env::set_var("LEAN_CTX_DATA_DIR", dir.to_str().unwrap());
        crate::test_env::set_var("LEAN_CTX_STRUCTURE_FIRST", "1");

        let suspect = AutoModeContext {
            path: "src/versioncmp.c",
            token_count: 1500,
            task: None,
            cache: None,
        };
        let result = resolve(&suspect);
        assert_eq!(result.mode, "map");
        assert_eq!(result.source, "structure_first");

        let tiny = AutoModeContext {
            path: "src/util.c",
            token_count: 120,
            task: None,
            cache: None,
        };
        assert_eq!(resolve(&tiny).mode, "full");

        crate::test_env::remove_var("LEAN_CTX_STRUCTURE_FIRST");
        crate::test_env::remove_var("LEAN_CTX_DATA_DIR");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn structure_first_off_keeps_medium_code_full() {
        let _lock = crate::core::data_dir::test_env_lock();
        let dir = std::env::temp_dir().join(format!("lctx-amr-sfoff-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        crate::test_env::set_var("LEAN_CTX_DATA_DIR", dir.to_str().unwrap());
        crate::test_env::set_var("LEAN_CTX_STRUCTURE_FIRST", "0");

        let ctx = AutoModeContext {
            path: "src/versioncmp.c",
            token_count: 1500,
            task: None,
            cache: None,
        };
        let result = resolve(&ctx);
        assert_eq!(result.mode, "full");
        assert_eq!(result.source, "heuristic");

        crate::test_env::remove_var("LEAN_CTX_STRUCTURE_FIRST");
        crate::test_env::remove_var("LEAN_CTX_DATA_DIR");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
