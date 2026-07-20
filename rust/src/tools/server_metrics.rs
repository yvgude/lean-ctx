use std::sync::atomic::Ordering;

use super::server::{CepComputedStats, CrpMode, LeanCtxServer, ToolCallRecord};
use super::startup::auto_consolidate_knowledge;
use super::{ctx_compress, ctx_share};

impl LeanCtxServer {
    /// Records a tool call's token savings without timing information.
    pub async fn record_call(
        &self,
        tool: &str,
        original: usize,
        saved: usize,
        mode: Option<String>,
    ) {
        self.record_call_with_timing(tool, original, saved, mode, 0)
            .await;
    }

    /// Records a tool call like `record_call`, but includes an optional file
    /// path for observability and the measured handler duration (#1020 — the
    /// duration is what makes the row land in `tool-calls.log`).
    pub async fn record_call_with_path(
        &self,
        tool: &str,
        original: usize,
        saved: usize,
        mode: Option<String>,
        path: Option<&str>,
        duration_ms: u64,
    ) {
        self.record_call_with_timing_inner(tool, original, saved, mode, duration_ms, path)
            .await;
    }

    /// Records a tool call's token savings, duration, and emits events and stats.
    pub async fn record_call_with_timing(
        &self,
        tool: &str,
        original: usize,
        saved: usize,
        mode: Option<String>,
        duration_ms: u64,
    ) {
        self.record_call_with_timing_inner(tool, original, saved, mode, duration_ms, None)
            .await;
    }

    async fn record_call_with_timing_inner(
        &self,
        tool: &str,
        original: usize,
        saved: usize,
        mode: Option<String>,
        duration_ms: u64,
        path: Option<&str>,
    ) {
        if let Some(agent_id) = self.presence_agent_id.read().await.clone()
            && let Err(error) = crate::core::agents::AgentRegistry::heartbeat_persistent(&agent_id)
        {
            tracing::warn!("lean-ctx: failed to update MCP agent heartbeat: {error}");
        }
        let ts = chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
        let mut calls = self.tool_calls.write().await;
        calls.push(ToolCallRecord {
            tool: tool.to_string(),
            original_tokens: original,
            saved_tokens: saved,
            mode: mode.clone(),
            duration_ms,
            timestamp: ts.clone(),
        });

        const MAX_TOOL_CALL_RECORDS: usize = 500;
        if calls.len() > MAX_TOOL_CALL_RECORDS {
            let excess = calls.len() - MAX_TOOL_CALL_RECORDS;
            calls.drain(..excess);
        }

        // #1020: persist whenever we have a duration OR real metrics. The old
        // `duration_ms > 0` gate dropped every read/search row (they record with
        // measured metrics but were previously logged via a separate zero-filled
        // path), so `tool-calls.log` only ever showed `orig=0 saved=0 mode=-`.
        if duration_ms > 0 || original > 0 {
            Self::append_tool_call_log(tool, duration_ms, original, saved, mode.as_deref(), &ts);
        }

        crate::core::events::emit_tool_call(
            tool,
            original as u64,
            saved as u64,
            mode.clone(),
            duration_ms,
            path.map(ToString::to_string),
        );

        let output_tokens = original.saturating_sub(saved);
        crate::core::stats::record(tool, original, output_tokens);
        // MCP shell savings are measured (raw vs compressed output), so they are
        // ledger-grade (GL #479 D2). Reads are ledgered by the ctx_read /
        // ctx_multi_read callers (#685, decoupled from the heatmap) and ctx_search
        // records itself — only shell is recorded here, exactly once. `actual_tokens`
        // is the *sent* output; a prior duplicate block passed `saved` and so both
        // double-counted shell events and stored the wrong saving (#685).
        if tool == "ctx_shell" {
            crate::core::savings_ledger::record_tool_event(tool, original, output_tokens);
        }


        // OCLA ObservationHook: project every MCP tool call as a structured
        // observation for the canonical observation capability.
        {
            let session_r = self.session.read().await;
            let agent_id = self
                .presence_agent_id
                .read()
                .await
                .clone()
                .unwrap_or_default();
            let ctx = crate::core::ocla::OclaRequestContext {
                request_id: format!("mcp-{}", self.call_count.load(Ordering::Relaxed)),
                session_id: session_r.id.clone(),
                agent_id,
                content_ref: path.map_or_else(|| format!("tool:{tool}"), |p| format!("file:{p}")),
                tenant_id: None,
            };
            drop(session_r);
            let observation = crate::core::ocla::Observation {
                context: ctx,
                name: format!("tool_call:{tool}"),
                attributes: std::collections::BTreeMap::from([
                    ("original_tokens".into(), original.to_string()),
                    ("saved_tokens".into(), saved.to_string()),
                    ("duration_ms".into(), duration_ms.to_string()),
                ]),
            };
            let hook = crate::core::ocla::OclaRegistry::global()
                .observation_hook
                .as_ref();
            let _ = hook.observe(observation);
        }
        let mut session = self.session.write().await;
        session.record_tool_call(saved as u64, original as u64);
        if tool == "ctx_shell" {
            session.record_command();
        }
        let pending_save = if session.should_save() {
            session.prepare_save().ok()
        } else {
            None
        };
        drop(calls);
        drop(session);

        if let Some(prepared) = pending_save {
            tokio::task::spawn_blocking(move || {
                let _ = prepared.write_to_disk();
            });
        }

        self.write_mcp_live_stats().await;
    }

    /// Increments the call counter and returns true if a checkpoint is due.
    pub fn increment_and_check(&self) -> bool {
        let count = self.call_count.fetch_add(1, Ordering::Relaxed) + 1;
        let interval = Self::checkpoint_interval_effective();
        interval > 0 && count.is_multiple_of(interval)
    }

    /// Generates a compressed context checkpoint with session state and multi-agent sync.
    pub async fn auto_checkpoint(&self) -> Option<String> {
        let cache = self.cache.read().await;
        if cache.get_all_entries().is_empty() {
            return None;
        }
        let complexity = crate::core::adaptive::classify_from_context(&cache);
        let checkpoint = ctx_compress::handle(&cache, false, CrpMode::effective());
        drop(cache);

        let mut session = self.session.write().await;
        let _ = session.save();
        let session_summary = session.format_compact();
        let has_insights = !session.findings.is_empty() || !session.decisions.is_empty();
        let project_root = session.project_root.clone();
        // Snapshot the session under the lock; persist the summary off the hot path.
        let summary_candidate = crate::core::session_summary::build_candidate(&session);
        drop(session);

        if has_insights && let Some(ref root) = project_root {
            let root = root.clone();
            std::thread::spawn(move || {
                auto_consolidate_knowledge(&root);
            });
        }

        // Periodically record a recallable AI session summary (#292), off-thread.
        if let Some(ref root) = project_root {
            let root = root.clone();
            std::thread::spawn(move || {
                let _ =
                    crate::core::session_summary::maybe_record_periodic(&root, summary_candidate);
            });
        }

        let multi_agent_block = self
            .auto_multi_agent_checkpoint(project_root.as_ref())
            .await;

        self.record_call("ctx_compress", 0, 0, Some("auto".to_string()))
            .await;

        self.record_cep_snapshot().await;

        if !crate::core::protocol::meta_visible() {
            return None;
        }

        let doc_reminder = {
            let session = self.session.read().await;
            let calls = self.tool_calls.read().await;
            Self::activity_nudge(&session, &calls)
        };

        Some(format!(
            "{checkpoint}\n\n--- SESSION STATE ---\n{session_summary}\n\n{}{multi_agent_block}{doc_reminder}",
            complexity.instruction_suffix()
        ))
    }

    async fn auto_multi_agent_checkpoint(&self, project_root: Option<&String>) -> String {
        let Some(root) = project_root else {
            return String::new();
        };

        let registry = crate::core::agents::AgentRegistry::load_or_create();
        let active = registry.list_active(Some(root));
        if active.len() <= 1 {
            return String::new();
        }

        let agent_id = self.agent_id.read().await;
        let my_id = match agent_id.as_deref() {
            Some(id) => id.to_string(),
            None => return String::new(),
        };
        drop(agent_id);

        let cache = self.cache.read().await;
        let entries = cache.get_all_entries();
        if !entries.is_empty() {
            let mut by_access: Vec<_> = entries.iter().collect();
            by_access.sort_by_key(|x| std::cmp::Reverse(x.1.read_count()));
            let top_paths: Vec<&str> = by_access
                .iter()
                .take(5)
                .map(|(key, _)| key.as_str())
                .collect();
            let paths_csv = top_paths.join(",");

            let _ = ctx_share::handle(
                "push",
                Some(&my_id),
                None,
                Some(&paths_csv),
                None,
                &cache,
                root,
            );
        }
        drop(cache);

        let pending_count = registry
            .scratchpad
            .iter()
            .filter(|e| !e.read_by.contains(&my_id) && e.from_agent != my_id)
            .count();

        let shared_dir = crate::core::data_dir::lean_ctx_data_dir()
            .unwrap_or_default()
            .join("agents")
            .join("shared");
        let shared_count = if shared_dir.exists() {
            std::fs::read_dir(&shared_dir).map_or(0, std::iter::Iterator::count)
        } else {
            0
        };

        let agent_names: Vec<String> = active
            .iter()
            .map(|a| {
                let role = a.role.as_deref().unwrap_or(&a.agent_type);
                format!("{role}({})", &a.agent_id[..8.min(a.agent_id.len())])
            })
            .collect();

        format!(
            "\n\n--- MULTI-AGENT SYNC ---\nAgents: {} | Pending msgs: {} | Shared contexts: {}\nAuto-shared top-5 cached files.\n--- END SYNC ---",
            agent_names.join(", "),
            pending_count,
            shared_count,
        )
    }

    /// Appends a tool call entry to the rotating `tool-calls.log` file.
    pub fn append_tool_call_log(
        tool: &str,
        duration_ms: u64,
        original: usize,
        saved: usize,
        mode: Option<&str>,
        timestamp: &str,
    ) {
        const MAX_LOG_LINES: usize = 50;
        if let Ok(dir) = crate::core::paths::state_dir() {
            let log_path = dir.join("tool-calls.log");
            let mode_str = mode.unwrap_or("-");
            let slow = if duration_ms > 5000 { " **SLOW**" } else { "" };
            let line = format!(
                "{timestamp}\t{tool}\t{duration_ms}ms\torig={original}\tsaved={saved}\tmode={mode_str}{slow}\n"
            );

            let mut lines: Vec<String> = std::fs::read_to_string(&log_path)
                .unwrap_or_default()
                .lines()
                .map(std::string::ToString::to_string)
                .collect();

            lines.push(line.trim_end().to_string());
            if lines.len() > MAX_LOG_LINES {
                lines.drain(0..lines.len() - MAX_LOG_LINES);
            }

            let _ = std::fs::write(&log_path, lines.join("\n") + "\n");
        }
    }

    fn compute_cep_stats(
        calls: &[ToolCallRecord],
        stats: &crate::core::cache::CacheStats,
        complexity: &crate::core::adaptive::TaskComplexity,
    ) -> CepComputedStats {
        let total_original: u64 = calls.iter().map(|c| c.original_tokens as u64).sum();
        let total_saved: u64 = calls.iter().map(|c| c.saved_tokens as u64).sum();
        let total_compressed = total_original.saturating_sub(total_saved);
        let compression_rate = if total_original > 0 {
            total_saved as f64 / total_original as f64
        } else {
            0.0
        };

        let modes_used: std::collections::HashSet<&str> =
            calls.iter().filter_map(|c| c.mode.as_deref()).collect();
        let mode_diversity = (modes_used.len() as f64 / 10.0).min(1.0);
        let cache_util = stats.hit_rate() / 100.0;
        // Output efficiency (#501): 1 - avg echo ratio. An agent that keeps
        // re-quoting delivered content burns the input savings on output.
        let output_efficiency = 1.0 - crate::core::output_echo::current_avg_ratio();
        let cep_score = cache_util * 0.25
            + mode_diversity * 0.15
            + compression_rate * 0.45
            + output_efficiency * 0.15;

        let mut mode_counts: std::collections::HashMap<String, u64> =
            std::collections::HashMap::new();
        for call in calls {
            if let Some(ref mode) = call.mode {
                *mode_counts.entry(mode.clone()).or_insert(0) += 1;
            }
        }

        CepComputedStats {
            cep_score: (cep_score * 100.0).round() as u32,
            cache_util: (cache_util * 100.0).round() as u32,
            mode_diversity: (mode_diversity * 100.0).round() as u32,
            compression_rate: (compression_rate * 100.0).round() as u32,
            total_original,
            total_compressed,
            total_saved,
            mode_counts,
            complexity: format!("{complexity:?}"),
            cache_hits: stats.cache_hits(),
            total_reads: stats.total_reads(),
            tool_call_count: calls.len() as u64,
        }
    }

    async fn write_mcp_live_stats(&self) {
        let count = self.call_count.load(Ordering::Relaxed);
        if count > 1 && !count.is_multiple_of(5) {
            return;
        }

        let cache = self.cache.read().await;
        let calls = self.tool_calls.read().await;
        let stats = cache.get_stats();
        let complexity = crate::core::adaptive::classify_from_context(&cache);

        let cs = Self::compute_cep_stats(&calls, stats, &complexity);
        let started_at = calls
            .first()
            .map(|c| c.timestamp.clone())
            .unwrap_or_default();

        drop(cache);
        drop(calls);

        // Persist CEP on the live-stats cadence (first call + every 5th) so even
        // short sessions register `sessions`/`total_cache_hits` instead of only
        // recording on an `auto_checkpoint` that a brief workload may never reach.
        // `record_cep_session` is delta-based and PID-guarded, so the extra call
        // that coincides with a checkpoint is a no-op for the totals (#361).
        crate::core::stats::record_cep_session(
            cs.cep_score,
            cs.cache_hits,
            cs.total_reads,
            cs.total_original,
            cs.total_compressed,
            &cs.mode_counts,
            cs.tool_call_count,
            &cs.complexity,
        );

        let live = serde_json::json!({
            "cep_score": cs.cep_score,
            "cache_utilization": cs.cache_util,
            "mode_diversity": cs.mode_diversity,
            "compression_rate": cs.compression_rate,
            "task_complexity": cs.complexity,
            "files_cached": cs.total_reads,
            "total_reads": cs.total_reads,
            "cache_hits": cs.cache_hits,
            "tokens_saved": cs.total_saved,
            "tokens_original": cs.total_original,
            "tool_calls": cs.tool_call_count,
            "started_at": started_at,
            "updated_at": chrono::Local::now().to_rfc3339(),
        });

        if let Ok(dir) = crate::core::paths::state_dir() {
            let _ = std::fs::write(dir.join("mcp-live.json"), live.to_string());
        }
    }

    /// Persists a CEP (Cognitive Efficiency Protocol) score snapshot for analytics.
    pub async fn record_cep_snapshot(&self) {
        let cache = self.cache.read().await;
        let calls = self.tool_calls.read().await;
        let stats = cache.get_stats();
        let complexity = crate::core::adaptive::classify_from_context(&cache);

        let cs = Self::compute_cep_stats(&calls, stats, &complexity);

        drop(cache);
        drop(calls);

        crate::core::stats::record_cep_session(
            cs.cep_score,
            cs.cache_hits,
            cs.total_reads,
            cs.total_original,
            cs.total_compressed,
            &cs.mode_counts,
            cs.tool_call_count,
            &cs.complexity,
        );
    }

    fn activity_nudge(
        session: &crate::core::session::SessionState,
        calls: &[ToolCallRecord],
    ) -> &'static str {
        let last_doc_ts = session
            .progress
            .last()
            .map(|p| p.timestamp)
            .or_else(|| session.decisions.last().map(|d| d.timestamp))
            .or_else(|| session.findings.last().map(|f| f.timestamp));

        if let Some(ts) = last_doc_ts {
            let age = chrono::Utc::now() - ts;
            if age.num_minutes() < 8 {
                return "";
            }
        }

        let (weighted_score, significant_tools, shell_heavy, edit_heavy) =
            Self::compute_activity_score(calls, last_doc_ts);

        if weighted_score < 20 || significant_tools < 5 {
            if session.stats.total_tool_calls >= 30
                && session.decisions.is_empty()
                && session.progress.is_empty()
            {
                return "\n[CHECKPOINT: please document current progress via ctx_session(action=\"task\") or ctx_knowledge(action=\"remember\")]";
            }
            return "";
        }

        if shell_heavy {
            "\n[CHECKPOINT: multiple shell commands executed — any test results or findings worth persisting via ctx_knowledge(action=\"remember\")?]"
        } else if edit_heavy {
            "\n[CHECKPOINT: several files modified — document the architecture decision or pattern via ctx_knowledge(action=\"remember\")?]"
        } else {
            "\n[CHECKPOINT: significant work detected — consider persisting decisions via ctx_knowledge(action=\"remember\")]"
        }
    }

    fn compute_activity_score(
        calls: &[ToolCallRecord],
        last_doc_ts: Option<chrono::DateTime<chrono::Utc>>,
    ) -> (u32, u32, bool, bool) {
        let mut weighted_score: u32 = 0;
        let mut significant_tools: u32 = 0;
        let mut shell_count: u32 = 0;
        let mut edit_count: u32 = 0;

        let since_doc: Vec<&ToolCallRecord> = if let Some(ts) = last_doc_ts {
            let ts_str = ts.format("%Y-%m-%d %H:%M:%S").to_string();
            calls.iter().filter(|c| c.timestamp > ts_str).collect()
        } else {
            calls.iter().collect()
        };

        for call in &since_doc {
            let tool = call.tool.as_str();
            let is_knowledge = tool == "ctx_knowledge" || tool == "ctx_session";
            if is_knowledge {
                weighted_score = 0;
                significant_tools = 0;
                shell_count = 0;
                edit_count = 0;
                continue;
            }

            let (weight, significant) = match tool {
                "edit" | "write" | "str_replace" => {
                    edit_count += 1;
                    (4u32, true)
                }
                "ctx_shell" => {
                    shell_count += 1;
                    let is_test_or_build = call
                        .mode
                        .as_deref()
                        .is_some_and(|m| m.contains("test") || m.contains("build"));
                    if is_test_or_build {
                        (3, true)
                    } else {
                        (2, true)
                    }
                }
                "ctx_read" => {
                    let is_cache_hit = call.saved_tokens > 0
                        && call.original_tokens > 0
                        && call.saved_tokens == call.original_tokens;
                    if is_cache_hit { (0, false) } else { (1, false) }
                }
                _ => (1, false),
            };

            weighted_score = weighted_score.saturating_add(weight);
            if significant {
                significant_tools += 1;
            }
        }

        let shell_heavy = shell_count >= 3 && shell_count > edit_count;
        let edit_heavy = edit_count >= 3 && edit_count >= shell_count;

        (weighted_score, significant_tools, shell_heavy, edit_heavy)
    }
}

#[cfg(test)]
mod activity_score_tests {
    use super::*;

    fn make_call(tool: &str, mode: Option<&str>) -> ToolCallRecord {
        ToolCallRecord {
            tool: tool.to_string(),
            original_tokens: 100,
            saved_tokens: 50,
            mode: mode.map(String::from),
            duration_ms: 10,
            timestamp: "2026-01-01 12:00:00".to_string(),
        }
    }

    fn make_cache_hit() -> ToolCallRecord {
        ToolCallRecord {
            tool: "ctx_read".to_string(),
            original_tokens: 100,
            saved_tokens: 100,
            mode: Some("full".to_string()),
            duration_ms: 1,
            timestamp: "2026-01-01 12:00:00".to_string(),
        }
    }

    #[test]
    fn empty_calls_zero_score() {
        let (score, sig, _, _) = LeanCtxServer::compute_activity_score(&[], None);
        assert_eq!(score, 0);
        assert_eq!(sig, 0);
    }

    #[test]
    fn edits_have_highest_weight() {
        let calls = vec![
            make_call("edit", None),
            make_call("edit", None),
            make_call("edit", None),
        ];
        let (score, sig, _, edit_heavy) = LeanCtxServer::compute_activity_score(&calls, None);
        assert_eq!(score, 12);
        assert_eq!(sig, 3);
        assert!(edit_heavy);
    }

    #[test]
    fn shell_test_build_weight_three() {
        let calls = vec![
            make_call("ctx_shell", Some("test")),
            make_call("ctx_shell", Some("build")),
            make_call("ctx_shell", Some("test")),
        ];
        let (score, sig, shell_heavy, _) = LeanCtxServer::compute_activity_score(&calls, None);
        assert_eq!(score, 9);
        assert_eq!(sig, 3);
        assert!(shell_heavy);
    }

    #[test]
    fn cache_hits_zero_weight() {
        let calls = vec![make_cache_hit(), make_cache_hit(), make_cache_hit()];
        let (score, sig, _, _) = LeanCtxServer::compute_activity_score(&calls, None);
        assert_eq!(score, 0);
        assert_eq!(sig, 0);
    }

    #[test]
    fn knowledge_call_resets_score() {
        let calls = vec![
            make_call("edit", None),
            make_call("edit", None),
            make_call("ctx_knowledge", None),
            make_call("ctx_read", None),
        ];
        let (score, sig, _, _) = LeanCtxServer::compute_activity_score(&calls, None);
        assert_eq!(score, 1);
        assert_eq!(sig, 0);
    }

    #[test]
    fn mixed_workflow_scoring() {
        let calls = vec![
            make_call("ctx_read", None),
            make_call("ctx_read", None),
            make_call("edit", None),
            make_call("edit", None),
            make_call("ctx_shell", Some("test output")),
            make_call("ctx_shell", None),
        ];
        let (score, sig, _, _) = LeanCtxServer::compute_activity_score(&calls, None);
        assert_eq!(score, 2 + 4 + 4 + 3 + 2);
        assert_eq!(sig, 4);
    }
}
