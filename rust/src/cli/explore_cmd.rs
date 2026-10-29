//! `lean-ctx explore` — FastContext-style iterative code exploration (CLI).
//!
//! Exposes the same deterministic loop as the `ctx_explore` MCP tool: a bounded
//! multi-turn search (BM25 + static call/import graph + AST symbols) that returns
//! compact `path:start-end` citations rather than file bodies. `--json` emits the
//! citation list for editor/script consumption; `--citation` prints only the
//! `<final_answer>` block.

use crate::tools::CrpMode;
use crate::tools::ctx_explore::{self, Citation, ExploreOptions};

/// Parsed `explore` invocation. Separated from execution so flag handling is
/// unit-testable without running a real search.
#[derive(Debug, PartialEq)]
struct Args {
    query: Option<String>,
    path: String,
    max_turns: Option<usize>,
    citation: bool,
    json: bool,
    help: bool,
}

impl Default for Args {
    fn default() -> Self {
        Self {
            query: None,
            path: ".".to_string(),
            max_turns: None,
            citation: false,
            json: false,
            help: false,
        }
    }
}

fn parse_args(args: &[String]) -> Args {
    let mut parsed = Args::default();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--json" => parsed.json = true,
            "--citation" | "--citations" => parsed.citation = true,
            "--help" | "-h" => parsed.help = true,
            "--query" | "-q" => {
                i += 1;
                parsed.query = args.get(i).cloned();
            }
            "--path" | "-p" => {
                i += 1;
                if let Some(v) = args.get(i) {
                    parsed.path.clone_from(v);
                }
            }
            "--max-turns" | "-t" => {
                i += 1;
                parsed.max_turns = args.get(i).and_then(|s| s.parse::<usize>().ok());
            }
            // First bare token is the query; later bare tokens are ignored.
            other if !other.starts_with('-') && parsed.query.is_none() => {
                parsed.query = Some(other.to_string());
            }
            _ => {}
        }
        i += 1;
    }
    parsed
}

pub(crate) fn cmd_explore(args: &[String]) {
    let parsed = parse_args(args);

    if parsed.help {
        print_help();
        return;
    }

    let Some(query) = parsed.query.filter(|q| !q.trim().is_empty()) else {
        eprintln!(
            "usage: lean-ctx explore <query> [--citation] [--json] [--max-turns N] [--path DIR]"
        );
        std::process::exit(2);
    };

    let opts = ExploreOptions::new(parsed.max_turns, parsed.citation);
    // One tool run for the CLI (request scope, capture, release, redaction,
    // lineage — see `CliToolRun`). The printed text is the released one; a
    // tool error is reported on stderr and is never a carrier.
    let json = parsed.json;
    let run = super::common::CliToolRun::new("ctx_explore", &parsed.path, None);
    let run = if json { run.human_facing() } else { run };
    let printed = run.deliver(|| {
        let outcome = ctx_explore::handle(&query, &parsed.path, CrpMode::Off, &opts);
        if outcome.text.starts_with("ERROR") {
            return Err(outcome.text.trim_start_matches("ERROR: ").to_string());
        }
        Ok(if json {
            to_json(&outcome.citations)
        } else {
            outcome.text
        })
    });
    match printed {
        Ok(text) => super::common::print_released(&text),
        Err(error) => {
            eprintln!("explore: {error}");
            std::process::exit(1);
        }
    }
}

/// Serialize citations with stable field names for editors/scripts.
fn to_json(citations: &[Citation]) -> String {
    #[derive(serde::Serialize)]
    struct Cite<'a> {
        file: &'a str,
        start: usize,
        end: usize,
        label: &'a str,
    }
    let out: Vec<Cite> = citations
        .iter()
        .map(|c| Cite {
            file: &c.file,
            start: c.start,
            end: c.end,
            label: &c.label,
        })
        .collect();
    serde_json::to_string(&out).unwrap_or_else(|_| "[]".to_string())
}

fn print_help() {
    println!(
        "lean-ctx explore — iterative code exploration → file:line citations\n\n\
         USAGE:\n  lean-ctx explore <query> [OPTIONS]\n\n\
         OPTIONS:\n\
         \x20 -q, --query <text>     Question or symbol names (or pass as the first argument)\n\
         \x20 -t, --max-turns <N>    Exploration depth (1-8, default 3)\n\
         \x20 -p, --path <dir>       Project root to explore (default: cwd)\n\
         \x20     --citation         Print only the <final_answer> block\n\
         \x20     --json             Emit JSON array [{{file,start,end,label}}]\n\
         \x20 -h, --help             Show this help\n\n\
         vs semantic-search: explore follows the call/import graph over multiple turns;\n\
         vs compose: explore returns citations (cheap), compose inlines bodies (one shot)."
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn parses_positional_query_with_defaults() {
        let a = parse_args(&args(&["how does caching work"]));
        assert_eq!(a.query.as_deref(), Some("how does caching work"));
        assert_eq!(a.path, ".");
        assert_eq!(a.max_turns, None);
        assert!(!a.citation);
        assert!(!a.json);
    }

    #[test]
    fn parses_flags() {
        let a = parse_args(&args(&[
            "--query",
            "auth flow",
            "--max-turns",
            "5",
            "--path",
            "/tmp/p",
            "--citation",
            "--json",
        ]));
        assert_eq!(a.query.as_deref(), Some("auth flow"));
        assert_eq!(a.max_turns, Some(5));
        assert_eq!(a.path, "/tmp/p");
        assert!(a.citation);
        assert!(a.json);
    }

    #[test]
    fn explicit_query_flag_beats_bare_token() {
        let a = parse_args(&args(&["--query", "real", "ignored"]));
        assert_eq!(a.query.as_deref(), Some("real"));
    }

    #[test]
    fn json_serializes_citation_fields() {
        let cites = vec![Citation {
            file: "src/main.rs".to_string(),
            start: 12,
            end: 20,
            label: "main (fn)".to_string(),
        }];
        let v: serde_json::Value = serde_json::from_str(&to_json(&cites)).unwrap();
        assert_eq!(v[0]["file"], "src/main.rs");
        assert_eq!(v[0]["start"], 12);
        assert_eq!(v[0]["end"], 20);
        assert_eq!(v[0]["label"], "main (fn)");
    }

    #[test]
    fn empty_citations_serialize_as_empty_array() {
        assert_eq!(to_json(&[]), "[]");
    }
}
