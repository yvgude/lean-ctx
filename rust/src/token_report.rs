use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::core::knowledge::ProjectKnowledge;
use crate::core::session::SessionState;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenReport {
    pub schema_version: u32,
    pub generated_at: DateTime<Utc>,
    pub version: String,
    pub project_root: String,
    pub data_dir: String,
    pub knowledge: Option<ProjectKnowledgeSummary>,
    pub session: Option<SessionSummary>,
    pub cep: Option<CepSummary>,
    pub warnings: Vec<String>,
    pub errors: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectKnowledgeSummary {
    pub project_hash: String,
    pub active_facts: usize,
    pub archived_facts: usize,
    pub patterns: usize,
    pub history: usize,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionSummary {
    pub id: String,
    pub started_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub tool_calls: u32,
    pub tokens_saved: u64,
    pub tokens_input: u64,
    pub cache_hits: u32,
    pub files_read: u32,
    pub commands_run: u32,
    pub repeated_files: u32,
    pub intents_total: u32,
    pub intents_inferred: u32,
    pub intents_explicit: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CepSummary {
    pub sessions: u64,
    pub total_cache_hits: u64,
    pub total_cache_reads: u64,
    pub total_tokens_original: u64,
    pub total_tokens_compressed: u64,
    pub last_snapshot: Option<CepSnapshot>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CepSnapshot {
    pub timestamp: String,
    pub score: u32,
    pub cache_hit_rate: u32,
    pub mode_diversity: u32,
    pub compression_rate: u32,
    pub tool_calls: u64,
    pub tokens_saved: u64,
    pub complexity: String,
}

pub fn run_cli(args: &[String]) -> i32 {
    let json = args.iter().any(|a| a == "--json");
    let help = args.iter().any(|a| a == "--help" || a == "-h");
    if help {
        println!("Usage:");
        println!("  lean-ctx token-report [--json] [--project-root <path>]");
        return 0;
    }

    let project_root = extract_flag(args, "--project-root");

    match build_report(project_root.as_deref()) {
        Ok((report, path)) => {
            let text = serde_json::to_string_pretty(&report).unwrap_or_else(|_| "{}".to_string());
            let _ = crate::config_io::write_atomic_with_backup(&path, &text);

            if json {
                println!("{text}");
            } else {
                print_human(&report, &path);
            }

            i32::from(!report.errors.is_empty())
        }
        Err(e) => {
            eprintln!("{e}");
            2
        }
    }
}

fn build_report(project_root_override: Option<&str>) -> Result<(TokenReport, PathBuf), String> {
    let generated_at = Utc::now();
    let version = env!("CARGO_PKG_VERSION").to_string();

    let data_dir = crate::core::data_dir::lean_ctx_data_dir()?;

    let cwd = std::env::current_dir()
        .map_or_else(|_| ".".to_string(), |p| p.to_string_lossy().to_string());
    let project_root = project_root_override.map_or_else(
        || crate::core::protocol::detect_project_root_or_cwd(&cwd),
        std::string::ToString::to_string,
    );

    let mut warnings: Vec<String> = Vec::new();
    let errors: Vec<String> = Vec::new();

    let knowledge = ProjectKnowledge::load(&project_root).map(|k| ProjectKnowledgeSummary {
        project_hash: k.project_hash.clone(),
        active_facts: k.facts.iter().filter(|f| f.is_current()).count(),
        archived_facts: k.facts.iter().filter(|f| !f.is_current()).count(),
        patterns: k.patterns.len(),
        history: k.history.len(),
        updated_at: k.updated_at,
    });

    if knowledge.is_none() {
        warnings.push("no project knowledge found".to_string());
    }

    let session = SessionState::load_latest().map(|s| {
        let repeated_files = s.files_touched.iter().filter(|f| f.read_count > 1).count() as u32;
        SessionSummary {
            id: s.id,
            started_at: s.started_at,
            updated_at: s.updated_at,
            tool_calls: s.stats.total_tool_calls,
            tokens_saved: s.stats.total_tokens_saved,
            tokens_input: s.stats.total_tokens_input,
            cache_hits: s.stats.cache_hits,
            files_read: s.stats.files_read,
            commands_run: s.stats.commands_run,
            repeated_files,
            intents_total: s.intents.len() as u32,
            intents_inferred: s.stats.intents_inferred,
            intents_explicit: s.stats.intents_explicit,
        }
    });

    if session.is_none() {
        warnings.push("no active session found".to_string());
    }

    let store = crate::core::stats::load();
    let last_snapshot = store.cep.scores.last().map(|s| CepSnapshot {
        timestamp: s.timestamp.clone(),
        score: s.score,
        cache_hit_rate: s.cache_hit_rate,
        mode_diversity: s.mode_diversity,
        compression_rate: s.compression_rate,
        tool_calls: s.tool_calls,
        tokens_saved: s.tokens_saved,
        complexity: s.complexity.clone(),
    });

    let cep = CepSummary {
        sessions: store.cep.sessions,
        total_cache_hits: store.cep.total_cache_hits,
        total_cache_reads: store.cep.total_cache_reads,
        total_tokens_original: store.cep.total_tokens_original,
        total_tokens_compressed: store.cep.total_tokens_compressed,
        last_snapshot,
    };

    let report_path = data_dir.join("report").join("latest.json");

    let report = TokenReport {
        schema_version: 1,
        generated_at,
        version,
        project_root,
        data_dir: data_dir.to_string_lossy().to_string(),
        knowledge,
        session,
        cep: Some(cep),
        warnings,
        errors,
    };

    Ok((report, report_path))
}

fn print_human(report: &TokenReport, path: &Path) {
    println!("lean-ctx token-report  v{}", report.version);
    println!("  project: {}", report.project_root);
    println!("  data:    {}", report.data_dir);

    if let Some(k) = &report.knowledge {
        println!(
            "  knowledge: {} active, {} archived, {} patterns, {} history",
            k.active_facts, k.archived_facts, k.patterns, k.history
        );
    } else {
        println!("  knowledge: (none)");
    }

    if let Some(s) = &report.session {
        println!(
            "  session: {} calls, {} tok saved, {} files read ({} repeated), {} intents ({} inferred, {} explicit)",
            s.tool_calls,
            s.tokens_saved,
            s.files_read,
            s.repeated_files,
            s.intents_total,
            s.intents_inferred,
            s.intents_explicit
        );
    } else {
        println!("  session: (none)");
    }

    if let Some(cep) = &report.cep {
        if let Some(last) = &cep.last_snapshot {
            println!(
                "  cep(last): score={} cache_hit_rate={} mode_diversity={} compression_rate={} tool_calls={} tok_saved={}",
                last.score,
                last.cache_hit_rate,
                last.mode_diversity,
                last.compression_rate,
                last.tool_calls,
                last.tokens_saved
            );
        } else {
            println!("  cep(last): (none)");
        }
    }

    if !report.warnings.is_empty() {
        println!("  warnings: {}", report.warnings.len());
    }
    if !report.errors.is_empty() {
        println!("  errors: {}", report.errors.len());
    }
    println!("  report saved: {}", path.display());
}

fn extract_flag(args: &[String], flag: &str) -> Option<String> {
    args.windows(2).find(|w| w[0] == flag).map(|w| w[1].clone())
}
