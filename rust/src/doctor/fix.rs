use chrono::Utc;

use super::{
    DIM, RST, compact_score, display_user_path, print_compact_status, shell_aliases_outcome,
};

pub(super) struct DoctorFixOptions {
    pub json: bool,
}

pub(super) fn run_fix(opts: &DoctorFixOptions) -> Result<i32, String> {
    use crate::core::setup_report::{
        PlatformInfo, SetupItem, SetupReport, SetupStepReport, doctor_report_path,
    };

    let _quiet_guard = opts
        .json
        .then(|| crate::setup::EnvVarGuard::set("LEAN_CTX_QUIET", "1"));
    let started_at = Utc::now();
    let home = dirs::home_dir().ok_or_else(|| "Cannot determine home directory".to_string())?;

    let mut steps: Vec<SetupStepReport> = Vec::new();

    let mut shell_step = SetupStepReport {
        name: "shell_hook".to_string(),
        ok: true,
        items: Vec::new(),
        warnings: Vec::new(),
        errors: Vec::new(),
    };
    let before = shell_aliases_outcome();
    if before.ok {
        shell_step.items.push(SetupItem {
            name: "init --global".to_string(),
            status: "already".to_string(),
            path: None,
            note: None,
        });
    } else {
        if opts.json {
            crate::cli::cmd_init_quiet(&["--global".to_string()]);
        } else {
            crate::cli::cmd_init(&["--global".to_string()]);
        }
        let after = shell_aliases_outcome();
        shell_step.ok = after.ok;
        shell_step.items.push(SetupItem {
            name: "init --global".to_string(),
            status: if after.ok {
                "fixed".to_string()
            } else {
                "failed".to_string()
            },
            path: None,
            note: if after.ok {
                None
            } else {
                Some("shell hook still not detected by doctor checks".to_string())
            },
        });
        if !after.ok {
            shell_step
                .warnings
                .push("shell hook not detected after init --global".to_string());
        }
    }
    steps.push(shell_step);

    let mut mcp_step = SetupStepReport {
        name: "mcp_config".to_string(),
        ok: true,
        items: Vec::new(),
        warnings: Vec::new(),
        errors: Vec::new(),
    };
    let binary = crate::core::portable_binary::resolve_portable_binary();
    // #281: doctor --fix must not (re)register the MCP server when the user opted
    // out via `auto_update_mcp = false`. Hooks/rules/scope repair still runs.
    let update_mcp = crate::core::config::Config::load()
        .setup
        .should_update_mcp();
    let targets = if update_mcp {
        crate::core::editor_registry::build_targets(&home)
    } else {
        Vec::new()
    };
    for t in &targets {
        if !t.detect_path.exists() {
            continue;
        }
        let short = t.config_path.to_string_lossy().to_string();

        let mode = if t.agent_key.is_empty() {
            crate::hooks::HookMode::Mcp
        } else {
            crate::hooks::recommend_hook_mode(&t.agent_key)
        };

        let res = crate::core::editor_registry::write_config_with_options(
            t,
            &binary,
            crate::core::editor_registry::WriteOptions {
                overwrite_invalid: true,
            },
        );

        match res {
            Ok(r) => {
                let status = match r.action {
                    crate::core::editor_registry::WriteAction::Created => "created",
                    crate::core::editor_registry::WriteAction::Updated => "updated",
                    crate::core::editor_registry::WriteAction::Already => "already",
                };
                let note_parts: Vec<String> = [Some(format!("mode={mode}")), r.note]
                    .into_iter()
                    .flatten()
                    .collect();
                mcp_step.items.push(SetupItem {
                    name: t.name.to_string(),
                    status: status.to_string(),
                    path: Some(short),
                    note: Some(note_parts.join("; ")),
                });
            }
            Err(e) => {
                mcp_step.ok = false;
                mcp_step.items.push(SetupItem {
                    name: t.name.to_string(),
                    status: "error".to_string(),
                    path: Some(short),
                    note: Some(e.clone()),
                });
                mcp_step.errors.push(format!("{}: {e}", t.name));
            }
        }
    }
    if !update_mcp {
        mcp_step
            .warnings
            .push("MCP registration skipped (auto_update_mcp=false)".to_string());
    } else if mcp_step.items.is_empty() {
        mcp_step
            .warnings
            .push("no supported AI tools detected; skipped MCP config repair".to_string());
    }
    steps.push(mcp_step);

    // Resolve workspace/user dual-scope conflicts (issue #338)
    let mut ws_scope_step = SetupStepReport {
        name: "workspace_scope".to_string(),
        ok: true,
        items: Vec::new(),
        warnings: Vec::new(),
        errors: Vec::new(),
    };
    let user_has_lean_ctx = !targets.is_empty();
    let ws_fixed = super::workspace_scope::fix_workspace_dual_scope(user_has_lean_ctx);
    ws_scope_step.items.push(SetupItem {
        name: "dual_scope_dedup".to_string(),
        status: if ws_fixed > 0 {
            format!("fixed {ws_fixed}")
        } else {
            "clean".to_string()
        },
        path: None,
        note: if ws_fixed > 0 {
            Some("removed lean-ctx from workspace-scope (user-scope preferred)".to_string())
        } else {
            None
        },
    });
    steps.push(ws_scope_step);

    let mut rules_step = SetupStepReport {
        name: "agent_rules".to_string(),
        ok: true,
        items: Vec::new(),
        warnings: Vec::new(),
        errors: Vec::new(),
    };
    let inj = crate::rules_inject::inject_all_rules(&home);
    if !inj.injected.is_empty() {
        rules_step.items.push(SetupItem {
            name: "injected".to_string(),
            status: inj.injected.len().to_string(),
            path: None,
            note: Some(inj.injected.join(", ")),
        });
    }
    if !inj.updated.is_empty() {
        rules_step.items.push(SetupItem {
            name: "updated".to_string(),
            status: inj.updated.len().to_string(),
            path: None,
            note: Some(inj.updated.join(", ")),
        });
    }
    if !inj.already.is_empty() {
        rules_step.items.push(SetupItem {
            name: "already".to_string(),
            status: inj.already.len().to_string(),
            path: None,
            note: Some(inj.already.join(", ")),
        });
    }
    if !inj.errors.is_empty() {
        rules_step.ok = false;
        rules_step.errors.extend(inj.errors.clone());
    }
    steps.push(rules_step);

    let mut hooks_step = SetupStepReport {
        name: "agent_hooks".to_string(),
        ok: true,
        items: Vec::new(),
        warnings: Vec::new(),
        errors: Vec::new(),
    };
    let targets = crate::core::editor_registry::build_targets(&home);
    for t in &targets {
        if !t.detect_path.exists() || t.agent_key.trim().is_empty() {
            continue;
        }
        let mode = crate::hooks::recommend_hook_mode(&t.agent_key);
        crate::hooks::install_agent_hook_with_mode(&t.agent_key, true, mode);
        hooks_step.items.push(SetupItem {
            name: format!("{} hooks", t.name),
            status: "installed".to_string(),
            path: Some(t.detect_path.to_string_lossy().to_string()),
            note: Some(format!("mode={mode}; merge-based install/repair")),
        });
    }
    if !hooks_step.items.is_empty() {
        steps.push(hooks_step);
    }

    let mut skill_step = SetupStepReport {
        name: "skill_files".to_string(),
        ok: true,
        items: Vec::new(),
        warnings: Vec::new(),
        errors: Vec::new(),
    };
    let skill_result = crate::setup::install_skill_files(&home);
    for (name, installed) in &skill_result {
        skill_step.items.push(SetupItem {
            name: name.clone(),
            status: if *installed {
                "installed".to_string()
            } else {
                "already".to_string()
            },
            path: None,
            note: Some("SKILL.md".to_string()),
        });
    }
    if !skill_result.is_empty() {
        steps.push(skill_step);
    }

    let mut bm25_step = SetupStepReport {
        name: "bm25_cache_prune".to_string(),
        ok: true,
        items: Vec::new(),
        warnings: Vec::new(),
        errors: Vec::new(),
    };
    let prune_result = crate::cli::prune_bm25_caches();
    bm25_step.items.push(SetupItem {
        name: "prune".to_string(),
        status: if prune_result.removed > 0 {
            "pruned".to_string()
        } else {
            "clean".to_string()
        },
        path: None,
        note: Some(format!(
            "scanned {}, removed {}, freed {:.1} MB",
            prune_result.scanned,
            prune_result.removed,
            prune_result.bytes_freed as f64 / 1_048_576.0
        )),
    });
    steps.push(bm25_step);

    let mut proxy_env_step = SetupStepReport {
        name: "proxy_env".to_string(),
        ok: true,
        items: Vec::new(),
        warnings: Vec::new(),
        errors: Vec::new(),
    };
    let cleaned = crate::proxy_setup::cleanup_stale_proxy_env(&home);
    proxy_env_step.items.push(SetupItem {
        name: "stale_proxy_urls".to_string(),
        status: if cleaned > 0 {
            format!("cleaned {cleaned} stale URL(s)")
        } else {
            "no stale URLs".to_string()
        },
        path: None,
        note: None,
    });
    steps.push(proxy_env_step);

    let mut startup_step = SetupStepReport {
        name: "crash_loop_reset".to_string(),
        ok: true,
        items: Vec::new(),
        warnings: Vec::new(),
        errors: Vec::new(),
    };
    crate::core::startup_guard::reset_crash_loop(crate::core::startup_guard::MCP_PROCESS_NAME);
    startup_step.items.push(SetupItem {
        name: "crash_loop_backoff".to_string(),
        status: "reset".to_string(),
        path: None,
        note: Some(
            "cleared MCP startup history (fixes backoff after IDE restart loops)".to_string(),
        ),
    });
    steps.push(startup_step);

    // Merge a split data layout (stats.json in two trees) into the canonical
    // dir *before* the XDG split, so `doctor --fix` actually resolves the "data
    // dir split" check instead of only ever splitting a single dir (GH #414).
    let mut consolidate_step = SetupStepReport {
        name: "data_dir_consolidate".to_string(),
        ok: true,
        items: Vec::new(),
        warnings: Vec::new(),
        errors: Vec::new(),
    };
    match crate::core::data_consolidate::consolidate() {
        Some(report) => {
            consolidate_step.items.push(SetupItem {
                name: "merge".to_string(),
                status: format!("merged {}", report.files_moved),
                path: Some(report.canonical.to_string_lossy().to_string()),
                note: Some(format!(
                    "consolidated {} split dir(s) into the canonical data dir ({} moved, {} superseded)",
                    report.merged_from.len(),
                    report.files_moved,
                    report.files_superseded
                )),
            });
            if !report.errors.is_empty() {
                consolidate_step.ok = false;
                consolidate_step.errors.extend(report.errors.clone());
            }
        }
        None => {
            consolidate_step.items.push(SetupItem {
                name: "merge".to_string(),
                status: "clean".to_string(),
                path: None,
                note: Some("no split data dirs to consolidate".to_string()),
            });
        }
    }
    steps.push(consolidate_step);

    // Split a legacy/mixed single-dir install into the typed XDG dirs (GH #408).
    let mut xdg_step = SetupStepReport {
        name: "xdg_layout".to_string(),
        ok: true,
        items: Vec::new(),
        warnings: Vec::new(),
        errors: Vec::new(),
    };
    match crate::core::xdg_migrate::migrate() {
        Some(report) => {
            let moved = report.moved.len();
            let skipped = report.skipped.len();
            let conflicts = report.conflicts.len();
            xdg_step.items.push(SetupItem {
                name: "split".to_string(),
                status: if moved > 0 {
                    format!("moved {moved}")
                } else {
                    "clean".to_string()
                },
                path: Some(report.source.to_string_lossy().to_string()),
                note: Some(format!(
                    "split single-dir install into XDG config/data/state/cache \
                     ({moved} moved/merged, {skipped} duplicate(s) dropped, \
                     {conflicts} kept as *.legacy)"
                )),
            });
            if !report.errors.is_empty() {
                xdg_step.ok = false;
                xdg_step.errors.extend(report.errors.clone());
            }
        }
        None => {
            xdg_step.items.push(SetupItem {
                name: "split".to_string(),
                status: "clean".to_string(),
                path: None,
                note: Some("already XDG-split or fresh install — nothing to migrate".to_string()),
            });
        }
    }
    steps.push(xdg_step);

    // Reclaim a residual legacy `~/.lean-ctx` that lingered after the data moved
    // to XDG (older --fix runs, the GH #408 default flip): drain leftover reports
    // and remove the empty dir so it stops being silently re-adopted as the data
    // dir, and so the doctor report below lands in XDG, not legacy (#434, #436).
    let mut reclaim_step = SetupStepReport {
        name: "legacy_reclaim".to_string(),
        ok: true,
        items: Vec::new(),
        warnings: Vec::new(),
        errors: Vec::new(),
    };
    match crate::core::xdg_migrate::reclaim_legacy() {
        Some(report) => {
            let moved = report.moved.len();
            reclaim_step.items.push(SetupItem {
                name: "reclaim".to_string(),
                status: if moved > 0 {
                    format!("reclaimed {moved}")
                } else {
                    "removed".to_string()
                },
                path: Some(report.source.to_string_lossy().to_string()),
                note: Some(format!(
                    "drained {moved} leftover entr{} from ~/.lean-ctx into XDG and removed the empty dir",
                    if moved == 1 { "y" } else { "ies" }
                )),
            });
            if !report.errors.is_empty() {
                reclaim_step.ok = false;
                reclaim_step.errors.extend(report.errors.clone());
            }
        }
        None => {
            reclaim_step.items.push(SetupItem {
                name: "reclaim".to_string(),
                status: "clean".to_string(),
                path: None,
                note: Some("no residual ~/.lean-ctx to reclaim".to_string()),
            });
        }
    }
    steps.push(reclaim_step);

    // Record the XDG commitment now that the install is split + the residual
    // legacy dir is gone, so a stray `~/.lean-ctx` can never re-collapse this
    // install again (GL #623). `ensure_pinned` is a no-op for a deliberate
    // single-dir (`LEAN_CTX_DATA_DIR`) or an unmigrated legacy/mixed install.
    let mut pin_step = SetupStepReport {
        name: "layout_pin".to_string(),
        ok: true,
        items: Vec::new(),
        warnings: Vec::new(),
        errors: Vec::new(),
    };
    crate::core::layout_pin::ensure_pinned();
    let pinned = crate::core::layout_pin::is_xdg_pinned();
    pin_step.items.push(SetupItem {
        name: "pin".to_string(),
        status: if pinned { "xdg" } else { "single-dir" }.to_string(),
        path: None,
        note: Some(
            if pinned {
                "install committed to the XDG layout; ~/.lean-ctx can no longer hijack it"
            } else {
                "single-dir/legacy install — layout left in place"
            }
            .to_string(),
        ),
    });
    steps.push(pin_step);

    // Prune knowledge stores whose project_root was deleted (removed git
    // worktrees, thrown-away projects). They can never be written again, so
    // their per-store eviction cap can never self-heal — pure accumulated bloat
    // (GH #615). Only the explicit --fix path deletes; the background lifecycle
    // never does, since a missing root can also be a temporarily-unmounted drive.
    let mut orphan_step = SetupStepReport {
        name: "orphaned_knowledge".to_string(),
        ok: true,
        items: Vec::new(),
        warnings: Vec::new(),
        errors: Vec::new(),
    };
    let prune = crate::core::knowledge::maintenance::prune_orphaned_stores();
    orphan_step.items.push(SetupItem {
        name: "prune".to_string(),
        status: if prune.removed > 0 {
            format!("removed {}", prune.removed)
        } else {
            "clean".to_string()
        },
        path: None,
        note: Some(if prune.removed > 0 {
            format!(
                "pruned {} knowledge store(s) for deleted projects ({} reclaimed)",
                prune.removed,
                super::common::human_bytes(prune.reclaimed_bytes)
            )
        } else {
            "no knowledge stores for deleted projects".to_string()
        }),
    });
    steps.push(orphan_step);

    let mut verify_step = SetupStepReport {
        name: "verify".to_string(),
        ok: true,
        items: Vec::new(),
        warnings: Vec::new(),
        errors: Vec::new(),
    };
    let (passed, total) = compact_score();
    verify_step.items.push(SetupItem {
        name: "doctor_compact".to_string(),
        status: format!("{passed}/{total}"),
        path: None,
        note: None,
    });
    if passed != total {
        verify_step.warnings.push(format!(
            "doctor compact not fully passing: {passed}/{total}"
        ));
    }
    steps.push(verify_step);

    let finished_at = Utc::now();
    let success = steps.iter().all(|s| s.ok);

    let report = SetupReport {
        schema_version: 1,
        started_at,
        finished_at,
        success,
        platform: PlatformInfo {
            os: std::env::consts::OS.to_string(),
            arch: std::env::consts::ARCH.to_string(),
        },
        steps,
        warnings: Vec::new(),
        errors: Vec::new(),
    };

    let path = doctor_report_path()?;
    let json_text = serde_json::to_string_pretty(&report).map_err(|e| e.to_string())?;
    crate::config_io::write_atomic_with_backup(&path, &json_text)?;

    if opts.json {
        println!("{json_text}");
    } else {
        let (passed, total) = compact_score();
        print_compact_status(passed, total);
        println!("  {DIM}report saved:{RST} {}", display_user_path(&path));
    }

    Ok(i32::from(!report.success))
}
