//! On-disk stores & memory: BM25/semantic indexes, archive FTS, RAM
//! guardian, knowledge stores, capacity accounting.

#[allow(clippy::wildcard_imports)]
use crate::doctor::common::*;
use crate::doctor::{BOLD, DIM, GREEN, Outcome, RED, RST, YELLOW};

pub(crate) fn cache_safety_outcome() -> Outcome {
    use crate::core::neural::cache_alignment::CacheAlignedOutput;
    use crate::core::provider_cache::ProviderCacheState;

    let mut issues = Vec::new();

    let mut aligned = CacheAlignedOutput::new();
    aligned.add_stable_block("test", "stable content".into(), 1);
    aligned.add_variable_block("test_var", "variable content".into(), 1);
    let rendered = aligned.render();
    if rendered.find("stable content").unwrap_or(usize::MAX)
        > rendered.find("variable content").unwrap_or(0)
    {
        issues.push("cache_alignment: stable blocks not ordered first");
    }

    let mut state = ProviderCacheState::new();
    let section = crate::core::provider_cache::CacheableSection::new(
        "doctor_test",
        "test content".into(),
        crate::core::provider_cache::SectionPriority::System,
        true,
    );
    state.mark_sent(&section);
    if state.needs_update(&section) {
        issues.push("provider_cache: hash tracking broken");
    }

    if issues.is_empty() {
        Outcome {
            ok: true,
            line: format!(
                "{BOLD}Cache safety{RST}  {GREEN}cache_alignment + provider_cache operational{RST}"
            ),
        }
    } else {
        Outcome {
            ok: false,
            line: format!("{BOLD}Cache safety{RST}  {RED}{}{RST}", issues.join("; ")),
        }
    }
}
/// Flags a quarantined `stats.json.corrupt` (#706): the stats loader moves an
/// unparseable display cache aside instead of silently overwriting history.
/// Doctor surfaces the quarantine so the loss is visible and recoverable
/// (the savings ledger remains the source of truth for a rebuild).
pub(crate) fn stats_quarantine_outcome() -> Outcome {
    let Ok(data_dir) = crate::core::data_dir::lean_ctx_data_dir() else {
        return Outcome {
            ok: true,
            line: format!("{BOLD}Stats store{RST}  {DIM}skipped (no data dir){RST}"),
        };
    };
    let quarantine = data_dir.join("stats.json.corrupt");
    if quarantine.exists() {
        Outcome {
            ok: false,
            line: format!(
                "{BOLD}Stats store{RST}  {YELLOW}corrupt stats.json was quarantined at {} — \
                 history before the corruption is inside; inspect/merge it, then delete the \
                 file to clear this warning{RST}",
                quarantine.display()
            ),
        }
    } else {
        Outcome {
            ok: true,
            line: format!("{BOLD}Stats store{RST}  {GREEN}no quarantined corruption{RST}"),
        }
    }
}

pub(crate) fn bm25_cache_health_outcome() -> Outcome {
    let Ok(data_dir) = crate::core::data_dir::lean_ctx_data_dir() else {
        return Outcome {
            ok: true,
            line: format!("{BOLD}BM25 cache{RST}  {DIM}skipped (no data dir){RST}"),
        };
    };

    let vectors_dir = data_dir.join("vectors");
    let Ok(entries) = std::fs::read_dir(&vectors_dir) else {
        return Outcome {
            ok: true,
            line: format!("{BOLD}BM25 cache{RST}  {GREEN}no vector dirs{RST}"),
        };
    };

    // Single source of truth with `save`/`load` (decoupled from the RAM profile;
    // see bm25_index::persist_ceiling_bytes) so the warning threshold here always
    // matches what is actually enforced on disk.
    let max_bytes = crate::core::bm25_index::persist_ceiling_bytes();
    let effective_mb = max_bytes / (1024 * 1024);
    let warn_bytes = max_bytes * 80 / 100; // 80% of effective limit
    let mut total_dirs = 0u32;
    let mut total_bytes = 0u64;
    let mut oversized: Vec<(String, u64)> = Vec::new();
    let mut warnings: Vec<(String, u64)> = Vec::new();
    let mut quarantined_count = 0u32;

    for entry in entries.flatten() {
        let dir = entry.path();
        if !dir.is_dir() {
            continue;
        }
        total_dirs += 1;

        if dir.join("bm25_index.json.quarantined").exists()
            || dir.join("bm25_index.bin.quarantined").exists()
            || dir.join("bm25_index.bin.zst.quarantined").exists()
        {
            quarantined_count += 1;
        }

        let index_path = if dir.join("bm25_index.bin.zst").exists() {
            dir.join("bm25_index.bin.zst")
        } else if dir.join("bm25_index.bin").exists() {
            dir.join("bm25_index.bin")
        } else {
            dir.join("bm25_index.json")
        };
        if let Ok(meta) = std::fs::metadata(&index_path) {
            let size = meta.len();
            total_bytes += size;
            let display = index_path.display().to_string();
            if size > max_bytes {
                oversized.push((display, size));
            } else if size > warn_bytes {
                warnings.push((display, size));
            }
        }
    }

    if !oversized.is_empty() {
        let details: Vec<String> = oversized
            .iter()
            .map(|(p, s)| format!("{p} ({:.1} GB)", *s as f64 / 1_073_741_824.0))
            .collect();
        return Outcome {
            ok: false,
            line: format!(
                "{BOLD}BM25 cache{RST}  {RED}{} index(es) exceed limit ({:.0} MB){RST}: {}  {DIM}(run: lean-ctx cache prune){RST}",
                oversized.len(),
                max_bytes / (1024 * 1024),
                details.join(", ")
            ),
        };
    }

    if !warnings.is_empty() {
        let details: Vec<String> = warnings
            .iter()
            .map(|(p, s)| format!("{p} ({:.0} MB)", *s as f64 / 1_048_576.0))
            .collect();
        return Outcome {
            ok: true,
            line: format!(
                "{BOLD}BM25 cache{RST}  {YELLOW}{} index(es) >80% of {effective_mb} MB limit{RST}: {}  {DIM}(consider extra_ignore_patterns){RST}",
                warnings.len(),
                details.join(", ")
            ),
        };
    }

    let quarantine_note = if quarantined_count > 0 {
        format!("  {YELLOW}{quarantined_count} quarantined (run: lean-ctx cache prune){RST}")
    } else {
        String::new()
    };

    Outcome {
        ok: true,
        line: format!(
            "{BOLD}BM25 cache{RST}  {GREEN}{total_dirs} index(es), {:.1} MB total{RST}{quarantine_note}",
            total_bytes as f64 / 1_048_576.0
        ),
    }
}
/// Runtime status of the semantic (BM25) index for the active project: whether
/// it is idle/building/ready/failed, how long the last build took, and — crucially
/// — *why* it might be stuck (e.g. "indexed but NOT persisted: too large").
///
/// This answers issue #249: users had no way to tell whether the semantic index
/// was working, how fast it was, or why it kept "warming up" forever.
pub(crate) fn semantic_index_outcome() -> Option<Outcome> {
    let session = crate::core::session::SessionState::load_latest()?;
    let project_root = session.project_root?;

    let summary = crate::core::index_orchestrator::bm25_summary(&project_root);
    let disk = crate::core::index_orchestrator::disk_status(&project_root);
    let persisted = if disk.bm25_index.exists {
        match disk.bm25_index.size_bytes {
            Some(b) => format!("persisted {:.1} MB", b as f64 / 1_048_576.0),
            None => "persisted".to_string(),
        }
    } else {
        "not persisted".to_string()
    };

    let timing = match summary.elapsed_ms {
        Some(ms) if summary.state == "building" => format!(", {:.1}s elapsed", ms as f64 / 1000.0),
        Some(ms) => format!(", built in {:.1}s", ms as f64 / 1000.0),
        None => String::new(),
    };

    let outcome = match summary.state {
        "failed" => Outcome {
            ok: false,
            line: format!(
                "{BOLD}Semantic index{RST}  {RED}FAILED{RST}: {}  {DIM}(run: lean-ctx index build-semantic){RST}",
                summary
                    .last_error
                    .or(summary.note)
                    .unwrap_or_else(|| "unknown error".to_string())
            ),
        },
        "building" => Outcome {
            ok: true,
            line: format!("{BOLD}Semantic index{RST}  {YELLOW}building{timing}{RST}"),
        },
        _ if summary
            .note
            .as_deref()
            .is_some_and(|n| n.contains("NOT persisted")) =>
        {
            Outcome {
                ok: false,
                line: format!(
                    "{BOLD}Semantic index{RST}  {YELLOW}rebuilds every cold start{RST}: {}",
                    summary.note.unwrap_or_default()
                ),
            }
        }
        "ready" => Outcome {
            ok: true,
            line: format!(
                "{BOLD}Semantic index{RST}  {GREEN}ready{RST} {DIM}({persisted}{timing}){RST}"
            ),
        },
        // idle: never asked to build this session — report disk state only.
        _ if disk.bm25_index.exists => Outcome {
            ok: true,
            line: format!(
                "{BOLD}Semantic index{RST}  {GREEN}ready{RST} {DIM}({persisted}, on disk){RST}"
            ),
        },
        _ => Outcome {
            ok: true,
            line: format!(
                "{BOLD}Semantic index{RST}  {DIM}not built yet (builds on first semantic search/compose){RST}"
            ),
        },
    };
    Some(outcome)
}
pub(crate) fn archive_footprint_outcome() -> Outcome {
    let bytes = crate::core::archive_fts::db_size_bytes();
    let cap_mb = std::env::var("LEAN_CTX_ARCHIVE_DB_MAX_MB")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .filter(|m| *m > 0)
        .unwrap_or(500);
    let cap_bytes = cap_mb * 1024 * 1024;
    let mb = bytes as f64 / 1_048_576.0;
    if bytes > cap_bytes {
        Outcome {
            ok: false,
            line: format!(
                "{BOLD}Archive FTS{RST}  {RED}{mb:.0} MB exceeds {cap_mb} MB cap{RST}  {DIM}(run: lean-ctx cache prune; auto-enforced on next session){RST}"
            ),
        }
    } else if bytes > cap_bytes * 80 / 100 {
        Outcome {
            ok: true,
            line: format!(
                "{BOLD}Archive FTS{RST}  {YELLOW}{mb:.0} MB (>80% of {cap_mb} MB cap){RST}"
            ),
        }
    } else {
        Outcome {
            ok: true,
            line: format!("{BOLD}Archive FTS{RST}  {GREEN}{mb:.1} MB / {cap_mb} MB cap{RST}"),
        }
    }
}
pub(crate) fn memory_profile_outcome() -> Outcome {
    let cfg = crate::core::config::Config::load();
    let profile = crate::core::config::MemoryProfile::effective(&cfg);
    // The BM25 *disk* ceiling is decoupled from the RAM profile (#249); show the
    // real effective ceiling rather than a hardcoded per-profile figure.
    let bm25_mb = cfg.bm25_max_cache_mb_effective();
    let (label, detail) = match profile {
        crate::core::config::MemoryProfile::Low => (
            "low",
            format!("embeddings+semantic cache disabled, BM25 disk {bm25_mb} MB"),
        ),
        crate::core::config::MemoryProfile::Balanced => (
            "balanced",
            format!("default — single embedding engine, BM25 disk {bm25_mb} MB"),
        ),
        crate::core::config::MemoryProfile::Performance => (
            "performance",
            format!("full caches, BM25 disk {bm25_mb} MB"),
        ),
    };
    let source = if crate::core::config::MemoryProfile::from_env().is_some() {
        "env"
    } else if cfg.memory_profile != crate::core::config::MemoryProfile::default() {
        "config"
    } else {
        "default"
    };
    Outcome {
        ok: true,
        line: format!(
            "{BOLD}Memory profile{RST}  {GREEN}{label}{RST}  {DIM}({source}: {detail}){RST}"
        ),
    }
}
pub(crate) fn memory_cleanup_outcome() -> Outcome {
    let cfg = crate::core::config::Config::load();
    let cleanup = crate::core::config::MemoryCleanup::effective(&cfg);
    let (label, detail) = match cleanup {
        crate::core::config::MemoryCleanup::Aggressive => (
            "aggressive",
            "cache cleared after 5 min idle, single-IDE optimized",
        ),
        crate::core::config::MemoryCleanup::Shared => (
            "shared",
            "cache retained 1 hour, multi-IDE/multi-model optimized",
        ),
    };
    let source = if crate::core::config::MemoryCleanup::from_env().is_some() {
        "env"
    } else if cfg.memory_cleanup != crate::core::config::MemoryCleanup::default() {
        "config"
    } else {
        "default"
    };
    Outcome {
        ok: true,
        line: format!(
            "{BOLD}Memory cleanup{RST}  {GREEN}{label}{RST}  {DIM}({source}: {detail}){RST}"
        ),
    }
}
pub(crate) fn ram_guardian_outcome() -> Outcome {
    // Measure the daemon's RSS (not the CLI process) when the daemon is running.
    let daemon_pid = crate::daemon::read_daemon_pid();
    let snap = match daemon_pid {
        Some(pid) if crate::ipc::process::is_alive(pid) => {
            crate::core::memory_guard::MemorySnapshot::capture_for_pid(pid)
        }
        _ => crate::core::memory_guard::MemorySnapshot::capture(),
    };
    let Some(snap) = snap else {
        return Outcome {
            ok: true,
            line: format!(
                "{BOLD}RAM Guardian{RST}  {YELLOW}not available{RST}  {DIM}(platform unsupported){RST}"
            ),
        };
    };
    let allocator = if cfg!(all(feature = "jemalloc", not(windows))) {
        "jemalloc"
    } else {
        "system"
    };
    let source = if daemon_pid.is_some() {
        "daemon"
    } else {
        "self"
    };
    let ok = snap.pressure_level == crate::core::memory_guard::PressureLevel::Normal;
    let color = if ok { GREEN } else { RED };
    let pressure_hint = match snap.pressure_level {
        crate::core::memory_guard::PressureLevel::Normal => String::new(),
        level => {
            format!(
                "  {YELLOW}pressure={level:?} — consider: memory_profile=\"low\" or increase max_ram_percent{RST}"
            )
        }
    };
    Outcome {
        ok,
        line: format!(
            "{BOLD}RAM Guardian{RST}  {color}{:.0} MB{RST} / {:.1} GB system ({:.1}%)  {DIM}limit: {:.0} MB ({allocator}, {source}){RST}{pressure_hint}",
            snap.rss_bytes as f64 / 1_048_576.0,
            snap.system_ram_bytes as f64 / 1_073_741_824.0,
            snap.rss_percent,
            snap.rss_limit_bytes as f64 / 1_048_576.0,
        ),
    }
}
/// Reports knowledge stores whose `project_root` was deleted (removed git
/// worktrees, thrown-away projects). Such a store can never be written again, so
/// its eviction cap can never self-heal — it is pure accumulated bloat. This is
/// informational (never a hard failure); `lean-ctx doctor --fix` reclaims it (#615).
pub(crate) fn orphaned_knowledge_outcome() -> Outcome {
    let orphans = crate::core::knowledge::maintenance::find_orphaned_stores();
    if orphans.is_empty() {
        return Outcome {
            ok: true,
            line: format!("{BOLD}Knowledge stores{RST}  {GREEN}no orphaned stores{RST}"),
        };
    }
    let bytes: u64 = orphans.iter().map(|o| o.size_bytes).sum();
    Outcome {
        ok: true,
        line: format!(
            "{BOLD}Knowledge stores{RST}  {YELLOW}{} orphaned ({} reclaimable){RST}  {DIM}(deleted projects — reclaim: lean-ctx cache prune){RST}",
            orphans.len(),
            human_bytes(bytes)
        ),
    }
}
pub(crate) fn capacity_warnings() -> Vec<Outcome> {
    let Ok(data_dir) = crate::core::data_dir::lean_ctx_data_dir() else {
        return vec![];
    };

    let cfg = crate::core::config::Config::load();
    let policy = cfg.memory_policy_effective().unwrap_or_default();

    let knowledge_dir = data_dir.join("knowledge");
    let Ok(entries) = std::fs::read_dir(&knowledge_dir) else {
        return vec![Outcome {
            ok: true,
            line: format!("{BOLD}Capacity{RST} {GREEN}no memory stores{RST}"),
        }];
    };

    let mut results = Vec::new();

    for entry in entries.flatten() {
        let hash_dir = entry.path();
        if !hash_dir.is_dir() {
            continue;
        }
        let hash = hash_dir
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();
        let short_hash = &hash[..hash.len().min(8)];

        let mut checks: Vec<(String, usize, usize)> = Vec::new();

        if let Ok(content) = std::fs::read_to_string(hash_dir.join("knowledge.json"))
            && let Ok(k) =
                serde_json::from_str::<crate::core::knowledge::ProjectKnowledge>(&content)
        {
            checks.push((
                "facts".to_string(),
                k.facts.len(),
                policy.knowledge.max_facts,
            ));
            checks.push((
                "patterns".to_string(),
                k.patterns.len(),
                policy.knowledge.max_patterns,
            ));
            checks.push((
                "history".to_string(),
                k.history.len(),
                policy.knowledge.max_history,
            ));
        }

        if let Ok(content) = std::fs::read_to_string(hash_dir.join("embeddings.json"))
            && let Ok(idx) = serde_json::from_str::<
                crate::core::knowledge_embedding::KnowledgeEmbeddingIndex,
            >(&content)
        {
            checks.push((
                "embeddings".to_string(),
                idx.entries.len(),
                policy.embeddings.max_facts,
            ));
        }

        if let Ok(content) = std::fs::read_to_string(hash_dir.join("gotchas.json"))
            && let Ok(g) =
                serde_json::from_str::<crate::core::gotcha_tracker::GotchaStore>(&content)
        {
            checks.push((
                "gotchas".to_string(),
                g.gotchas.len(),
                policy.gotcha.max_gotchas_per_project,
            ));
        }

        let episodes_path = data_dir
            .join("memory")
            .join("episodes")
            .join(format!("{hash}.json"));
        if let Ok(content) = std::fs::read_to_string(&episodes_path)
            && let Ok(e) =
                serde_json::from_str::<crate::core::episodic_memory::EpisodicStore>(&content)
        {
            checks.push((
                "episodes".to_string(),
                e.episodes.len(),
                policy.episodic.max_episodes,
            ));
        }

        let procedures_path = data_dir
            .join("memory")
            .join("procedures")
            .join(format!("{hash}.json"));
        if let Ok(content) = std::fs::read_to_string(&procedures_path)
            && let Ok(p) =
                serde_json::from_str::<crate::core::procedural_memory::ProceduralStore>(&content)
        {
            checks.push((
                "procedures".to_string(),
                p.procedures.len(),
                policy.procedural.max_procedures,
            ));
        }

        let mut warnings: Vec<String> = Vec::new();
        let mut critical = false;

        for (name, current, limit) in &checks {
            if *limit == 0 {
                continue;
            }
            let pct = (*current as f64 / *limit as f64 * 100.0) as u32;
            // A store sitting *at* its cap is healthy: eviction (run_lifecycle)
            // keeps it there by design. Only flag CRIT when it is genuinely
            // *over* cap, which means eviction is not keeping up.
            if pct > 100 {
                critical = true;
                warnings.push(format!("{name}: {current}/{limit} ({pct}%)"));
            } else if pct >= 80 {
                warnings.push(format!("{name}: {current}/{limit} ({pct}%)"));
            }
        }

        if !warnings.is_empty() {
            let color = if critical { RED } else { YELLOW };
            let label = if critical { "CRIT" } else { "WARN" };
            results.push(Outcome {
                ok: !critical,
                line: format!(
                    "{BOLD}Capacity [{short_hash}]{RST} {color}{label}: {}{RST}",
                    warnings.join(", ")
                ),
            });
            // Actionable guidance (#972). A store *at* its cap is healthy by
            // design — eviction keeps it there; only *over* cap means curation is
            // falling behind. Point the operator at the right lever either way.
            results.push(Outcome {
                ok: true,
                line: format!("  {DIM}→ {}{RST}", capacity_hint(critical)),
            });
        }
    }

    // Global checks (not per project hash)

    // Archive disk usage vs limit
    let archive_limit_bytes = cfg.archive_max_disk_mb_effective() * 1_048_576;
    if archive_limit_bytes > 0 {
        let archive_used = crate::core::archive::disk_usage_bytes();
        let pct = (archive_used as f64 / archive_limit_bytes as f64 * 100.0) as u32;
        if pct >= 95 {
            results.push(Outcome {
                ok: false,
                line: format!(
                    "{BOLD}Capacity [archive]{RST} {RED}CRIT: disk {}/{}MB ({pct}%){RST}",
                    archive_used / 1_048_576,
                    archive_limit_bytes / 1_048_576
                ),
            });
        } else if pct >= 80 {
            results.push(Outcome {
                ok: true,
                line: format!(
                    "{BOLD}Capacity [archive]{RST} {YELLOW}WARN: disk {}/{}MB ({pct}%){RST}",
                    archive_used / 1_048_576,
                    archive_limit_bytes / 1_048_576
                ),
            });
        }
    }

    // Graph index file count vs limit
    let graph_max_files = cfg.graph_index_max_files;
    if graph_max_files > 0
        && let Some(session) = crate::core::session::SessionState::load_latest()
        && let Some(ref project_root) = session.project_root
    {
        let disk_status = crate::core::index_orchestrator::disk_status(project_root);
        if let Some(graph_files) = disk_status.graph_index.file_count {
            let pct = (graph_files as f64 / graph_max_files as f64 * 100.0) as u32;
            if pct >= 95 {
                results.push(Outcome {
                            ok: false,
                            line: format!(
                                "{BOLD}Capacity [graph]{RST} {RED}CRIT: files {graph_files}/{graph_max_files} ({pct}%){RST}"
                            ),
                        });
            } else if pct >= 80 {
                results.push(Outcome {
                            ok: true,
                            line: format!(
                                "{BOLD}Capacity [graph]{RST} {YELLOW}WARN: files {graph_files}/{graph_max_files} ({pct}%){RST}"
                            ),
                        });
            }
        }
    }

    if results.is_empty() {
        results.push(Outcome {
            ok: true,
            line: format!("{BOLD}Capacity{RST} {GREEN}all stores within limits{RST}"),
        });
    }

    results
}
/// Actionable next-step for a memory capacity warning (#972). Separated from the
/// on-disk capacity scan so the messaging is unit-tested directly: a store *at*
/// its cap is healthy (eviction holds it there), while *over* cap means curation
/// is behind and the operator should compact or raise the limit.
pub(crate) fn capacity_hint(critical: bool) -> &'static str {
    if critical {
        "over cap — eviction is behind. Run `lean-ctx knowledge consolidate --all` to compact project memory now, or raise the relevant memory.* cap"
    } else {
        "at/near cap is healthy by design — lean-ctx self-curates (write-time dedup #970, hourly cluster-compaction #971, 90-day prune #972). Raise a cap only if recall quality drops (memory.*)"
    }
}
