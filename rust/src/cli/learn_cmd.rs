use crate::core::gotcha_tracker::{self, GotchaStore, learn};

use crate::core::causal_attribution::{self, Outcome, OutcomeSignal};
use serde_json::Value;
use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum FailurePatternType {
    Loop,
    StuckOnError,
    WrongTool,
    ExcessiveTokens,
    Timeout,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FailurePattern {
    pub pattern_type: FailurePatternType,
    pub frequency: u32,
    pub session_ids: Vec<String>,
    pub suggested_correction: String,
    pub confidence: f32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoopDetection {
    pub tool_name: String,
    pub arguments_hash: String,
    pub repetitions: usize,
    pub turn_range: (usize, usize),
    pub wasted_tokens: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Correction {
    pub trigger: String,
    pub action: String,
    pub priority: u8,
}

pub fn detect_loops(session_log: &[Value]) -> Vec<LoopDetection> {
    #[derive(Default)]
    struct Repetition {
        repetitions: usize,
        first_turn: usize,
        last_turn: usize,
        token_total: usize,
    }

    let mut repetitions: HashMap<(String, String), Repetition> = HashMap::new();
    for (turn, entry) in session_log.iter().enumerate() {
        let Some((tool_name, arguments)) = tool_call(entry) else {
            continue;
        };
        let arguments_hash = stable_hash(arguments);
        let repetition = repetitions
            .entry((tool_name.to_owned(), arguments_hash))
            .or_insert_with(|| Repetition {
                first_turn: turn,
                ..Default::default()
            });
        repetition.repetitions += 1;
        repetition.last_turn = turn;
        repetition.token_total += tokens(entry);
    }

    let mut loops: Vec<_> = repetitions
        .into_iter()
        .filter_map(|((tool_name, arguments_hash), repetition)| {
            (repetition.repetitions >= 3).then(|| LoopDetection {
                tool_name,
                arguments_hash,
                repetitions: repetition.repetitions,
                turn_range: (repetition.first_turn, repetition.last_turn),
                wasted_tokens: repetition.token_total,
            })
        })
        .collect();
    loops.sort_by(|left, right| {
        left.turn_range
            .cmp(&right.turn_range)
            .then_with(|| left.tool_name.cmp(&right.tool_name))
    });
    loops
}

pub fn mine_failures(outcomes: &[OutcomeSignal]) -> Vec<FailurePattern> {
    let mut sessions_by_kind: HashMap<FailurePatternType, Vec<String>> = HashMap::new();
    for outcome in outcomes {
        let evidence = outcome.evidence.to_lowercase();
        let kind = if evidence.contains("timeout") {
            Some(FailurePatternType::Timeout)
        } else if evidence.contains("wrong tool") || evidence.contains("tool mismatch") {
            Some(FailurePatternType::WrongTool)
        } else if evidence.contains("token") || evidence.contains("context limit") {
            Some(FailurePatternType::ExcessiveTokens)
        } else if evidence.contains("error") || matches!(outcome.outcome, Outcome::Failure) {
            Some(FailurePatternType::StuckOnError)
        } else {
            None
        };
        if let Some(kind) = kind {
            sessions_by_kind
                .entry(kind)
                .or_default()
                .push(outcome.session_id.clone());
        }
    }

    let causal_sources = causal_attribution::suggest_removals();
    let mut patterns: Vec<_> = sessions_by_kind
        .into_iter()
        .map(|(pattern_type, mut session_ids)| {
            session_ids.sort();
            session_ids.dedup();
            let frequency = session_ids.len() as u32;
            let suggested_correction = correction_for(pattern_type, &causal_sources);
            FailurePattern {
                pattern_type,
                frequency,
                session_ids,
                suggested_correction,
                confidence: (0.55 + frequency as f32 * 0.1).min(0.95),
            }
        })
        .collect();
    sort_failure_patterns(&mut patterns);
    patterns
}

fn sort_failure_patterns(patterns: &mut [FailurePattern]) {
    patterns.sort_by(|left, right| {
        right
            .frequency
            .cmp(&left.frequency)
            .then_with(|| left.pattern_type.cmp(&right.pattern_type))
    });
}

pub fn generate_corrections(patterns: &[FailurePattern]) -> Vec<Correction> {
    patterns
        .iter()
        .map(|pattern| Correction {
            trigger: format!(
                "{:?} observed in {} session(s)",
                pattern.pattern_type, pattern.frequency
            ),
            action: pattern.suggested_correction.clone(),
            priority: (pattern.confidence * 10.0).round() as u8,
        })
        .collect()
}

fn tool_call(value: &Value) -> Option<(&str, &Value)> {
    let object = value.as_object()?;
    let tool_name = object
        .get("tool_name")
        .or_else(|| object.get("tool"))
        .or_else(|| object.get("name"))?
        .as_str()?;
    let arguments = object
        .get("arguments")
        .or_else(|| object.get("args"))
        .or_else(|| object.get("input"))
        .unwrap_or(&Value::Null);
    Some((tool_name, arguments))
}

fn tokens(value: &Value) -> usize {
    value
        .get("tokens")
        .or_else(|| value.get("token_count"))
        .and_then(Value::as_u64)
        .unwrap_or_default() as usize
}

fn stable_hash(value: &Value) -> String {
    let bytes = serde_json::to_vec(value).unwrap_or_default();
    format!("{:016x}", fxhash(&bytes))
}

fn fxhash(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

fn correction_for(pattern_type: FailurePatternType, causal_sources: &[String]) -> String {
    let attribution = causal_sources
        .first()
        .map(|source| format!(" Remove low-value context from {source}."))
        .unwrap_or_default();
    match pattern_type {
        FailurePatternType::Loop => "After repeating a tool call twice, switch strategy; use ctx_compose before another ctx_read.".to_owned(),
        FailurePatternType::StuckOnError => format!("Read the complete error once, then use the targeted diagnostic tool.{attribution}"),
        FailurePatternType::WrongTool => "Choose the tool by intent: ctx_compose for orientation, ctx_search for symbols, ctx_read for exact files.".to_owned(),
        FailurePatternType::ExcessiveTokens => format!("Prefer focused search and compact reads over broad context collection.{attribution}"),
        FailurePatternType::Timeout => "Reduce scope and split the operation into bounded, independently verifiable steps.".to_owned(),
    }
}

pub fn load_outcome_signals() -> Result<Vec<OutcomeSignal>, String> {
    let path = crate::core::causal_attribution::CausalAttributor::default_path()?;
    if !path.exists() {
        return Ok(Vec::new());
    }
    let content = std::fs::read_to_string(&path)
        .map_err(|error| format!("read {}: {error}", path.display()))?;
    Ok(content
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter_map(|event| event.get("outcome").cloned())
        .filter_map(|outcome| serde_json::from_value(outcome).ok())
        .collect())
}

pub fn store_corrections(project_root: &str, corrections: &[Correction]) -> Result<(), String> {
    let path = std::path::Path::new(project_root)
        .join(".lean-ctx")
        .join("shared-context.jsonl");
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("create {}: {error}", parent.display()))?;
    }
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(|error| format!("open {}: {error}", path.display()))?;
    for correction in corrections {
        let fact = serde_json::json!({
            "category": "correction",
            "trigger": correction.trigger,
            "action": correction.action,
            "priority": correction.priority,
        });
        use std::io::Write;
        writeln!(file, "{fact}").map_err(|error| format!("write {}: {error}", path.display()))?;
    }
    Ok(())
}

pub(crate) fn cmd_learn(args: &[String]) {
    if args.is_empty() {
        match load_outcome_signals() {
            Ok(outcomes) => {
                let patterns = mine_failures(&outcomes);
                if patterns.is_empty() {
                    println!(
                        "No learnings yet. lean-ctx needs to detect and resolve errors across sessions first."
                    );
                    println!(
                        "Tip: Use lean-ctx normally — errors are automatically tracked and correlated."
                    );
                } else {
                    let corrections = generate_corrections(&patterns);
                    let project_root = std::env::current_dir()
                        .map_err(|error| error.to_string())
                        .and_then(|path| {
                            path.into_os_string()
                                .into_string()
                                .map_err(|_| "non-UTF-8 project path".to_owned())
                        });
                    match project_root.and_then(|root| store_corrections(&root, &corrections)) {
                        Ok(()) => println!(
                            "Stored {} correction(s) as SharedContext facts.",
                            corrections.len()
                        ),
                        Err(error) => eprintln!("Failed to store corrections: {error}"),
                    }
                    for correction in corrections {
                        println!("- {}", correction.action);
                    }
                }
            }
            Err(error) => eprintln!("Failed to load outcome data: {error}"),
        }
        return;
    }
    // Offline mining mode: `lean-ctx learn --mine <dir>` distills recurring
    // error signatures from a directory of .jsonl transcripts/logs.
    if let Some(pos) = args.iter().position(|a| a == "--mine") {
        let dir = args.get(pos + 1).map(String::as_str);
        cmd_learn_mine(dir);
        return;
    }

    let project_root = super::common::detect_project_root(args);
    let apply = args.iter().any(|a| a == "--apply");

    let mut store = GotchaStore::load(&project_root);
    let universal = gotcha_tracker::load_universal_gotchas();
    for ug in universal {
        store.add_universal(ug);
    }

    let learnings = learn::extract_learnings(&store);

    if learnings.is_empty() {
        println!(
            "No learnings yet. lean-ctx needs to detect and resolve errors across sessions first."
        );
        println!("Tip: Use lean-ctx normally — errors are automatically tracked and correlated.");
        return;
    }

    println!("=== Learned Gotchas ({} total) ===\n", learnings.len());
    for l in &learnings {
        println!("  {l}");
    }

    if apply {
        println!();
        match learn::apply_learnings(&project_root, &learnings) {
            Ok(files) if files.is_empty() => {
                println!("No learnings written (need >=2 occurrences with >=50% confidence).");
            }
            Ok(files) => println!(
                "Wrote {} learnings to {}",
                learnings.len(),
                files.join(" + ")
            ),
            Err(e) => eprintln!("Error: {e}"),
        }
    } else {
        println!(
            "\nUse `lean-ctx learn --apply` to write these to AGENTS.md (and CLAUDE.local.md if present)."
        );
    }
}

/// `lean-ctx learn --mine [dir]`: distill recurring error signatures from a
/// directory of `.jsonl` transcripts/logs. With no `dir`, it auto-discovers the
/// agent-transcripts directory (Claude Code / Cursor), so scanning real subagent
/// transcripts is zero-config. Read-only — it surfaces the project's recurring
/// pain points for review, it never mutates stored state.
fn cmd_learn_mine(dir: Option<&str>) {
    let path = if let Some(d) = dir {
        std::path::PathBuf::from(d)
    } else if let Some(p) = gotcha_tracker::mining::default_transcript_dir() {
        println!("Scanning auto-discovered transcripts: {}\n", p.display());
        p
    } else {
        eprintln!(
            "Usage: lean-ctx learn --mine [dir]  (no agent-transcripts dir found to auto-scan)"
        );
        return;
    };
    if !path.is_dir() {
        eprintln!("Error: '{}' is not a directory", path.display());
        return;
    }
    let mined = gotcha_tracker::mining::mine_jsonl_dir(&path);
    println!(
        "{}",
        gotcha_tracker::mining::format_mining_report(&mined, 2)
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn loop_detection_finds_repeated_tool_calls() {
        let log = vec![
            json!({"tool_name":"ctx_read","arguments":{"path":"src/lib.rs"},"tokens":10}),
            json!({"tool_name":"ctx_read","arguments":{"path":"src/lib.rs"},"tokens":20}),
            json!({"tool_name":"ctx_read","arguments":{"path":"src/lib.rs"},"tokens":30}),
        ];
        let loops = detect_loops(&log);
        assert_eq!(loops.len(), 1);
        assert_eq!(loops[0].repetitions, 3);
        assert_eq!(loops[0].wasted_tokens, 60);
    }

    #[test]
    fn failure_mining_identifies_patterns() {
        let outcomes = vec![
            OutcomeSignal {
                session_id: "one".into(),
                outcome: Outcome::Failure,
                evidence: "timeout while reading".into(),
            },
            OutcomeSignal {
                session_id: "two".into(),
                outcome: Outcome::Failure,
                evidence: "tool mismatch".into(),
            },
        ];
        let patterns = mine_failures(&outcomes);
        assert!(
            patterns
                .iter()
                .any(|pattern| pattern.pattern_type == FailurePatternType::Timeout)
        );
        assert!(
            patterns
                .iter()
                .any(|pattern| pattern.pattern_type == FailurePatternType::WrongTool)
        );
    }

    #[test]
    fn correction_generation_is_actionable() {
        let patterns = vec![FailurePattern {
            pattern_type: FailurePatternType::Loop,
            frequency: 3,
            session_ids: vec!["session".into()],
            suggested_correction: "Switch strategy after two repeats.".into(),
            confidence: 0.85,
        }];
        let corrections = generate_corrections(&patterns);
        assert_eq!(corrections[0].priority, 9);
        assert!(corrections[0].action.contains("Switch strategy"));
    }

    #[test]
    fn empty_session_produces_no_errors() {
        assert!(detect_loops(&[]).is_empty());
        assert!(mine_failures(&[]).is_empty());
        assert!(generate_corrections(&[]).is_empty());
    }
}
