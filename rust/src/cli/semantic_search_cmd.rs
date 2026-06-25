//! `lean-ctx semantic-search` — structured code search for the CLI & editors.
//!
//! Exposes the same ranker as the `ctx_semantic_search` MCP tool. `--json` emits
//! machine-readable results (what the VS Code / Cursor extensions consume);
//! otherwise a compact human-readable report is printed.
//!
//! The default mode is `bm25`: it is instant and needs no model load, which
//! matters because every CLI invocation is a fresh one-shot process that —
//! unlike the long-lived MCP daemon — cannot amortize loading the embedding
//! model. `--mode hybrid` / `--mode dense` add embedding-based ranking for the
//! best relevance, at the cost of a slower first run while embeddings build.

use crate::core::hybrid_search::HybridResult;
use crate::tools::ctx_semantic_search;

/// Parsed `semantic-search` invocation. Kept separate from execution so the
/// flag handling is unit-testable without spawning a process.
#[derive(Debug, PartialEq)]
struct Args {
    query: Option<String>,
    path: Option<String>, // None = auto-detect
    mode: String,
    top_k: usize,
    json: bool,
    languages: Vec<String>,
    path_glob: Option<String>,
    help: bool,
    explicit_path: bool, // true when --path/-p was given
}

impl Default for Args {
    fn default() -> Self {
        Self {
            query: None,
            path: None,
            // Instant and model-load-free — the right default for a one-shot
            // process. Embedding-based ranking is opt-in via `--mode`.
            mode: "bm25".to_string(),
            top_k: 10,
            json: false,
            languages: Vec::new(),
            path_glob: None,
            help: false,
            explicit_path: false,
        }
    }
}

fn parse_args(args: &[String]) -> Args {
    let mut parsed = Args::default();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--json" => parsed.json = true,
            "--help" | "-h" => parsed.help = true,
            "--query" | "-q" => {
                i += 1;
                parsed.query = args.get(i).cloned();
            }
            "--path" | "-p" => {
                i += 1;
                if let Some(v) = args.get(i) {
                    parsed.path = Some(v.clone());
                    parsed.explicit_path = true;
                }
            }
            "--mode" | "-m" => {
                i += 1;
                if let Some(v) = args.get(i) {
                    parsed.mode.clone_from(v);
                }
            }
            "--top-k" | "-k" => {
                i += 1;
                if let Some(v) = args.get(i).and_then(|s| s.parse::<usize>().ok()) {
                    parsed.top_k = v.clamp(1, 100);
                }
            }
            "--lang" | "--language" => {
                i += 1;
                if let Some(v) = args.get(i) {
                    parsed.languages.push(v.clone());
                }
            }
            "--glob" => {
                i += 1;
                parsed.path_glob = args.get(i).cloned();
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

pub(crate) fn cmd_semantic_search(args: &[String]) {
    let parsed = parse_args(args);

    if parsed.help {
        print_help();
        return;
    }

    let Some(query) = parsed.query.filter(|q| !q.trim().is_empty()) else {
        eprintln!(
            "usage: lean-ctx semantic-search <query> [--json] [--mode hybrid|bm25|dense] \
             [--top-k N] [--path DIR] [--lang rust] [--glob 'src/**']"
        );
        std::process::exit(2);
    };

    // Resolve search root: use explicit --path if given, otherwise detect
    // project root the same way as `index build` (promote CWD to git root).
    // Without this, `vectors_dir()` produces a different namespace hash for
    // "." vs the git-root path, causing semantic-search to miss the index.
    let root = if parsed.explicit_path {
        parsed.path.clone().unwrap_or_else(|| ".".to_string())
    } else {
        super::common::detect_project_root(args)
    };

    let languages = if parsed.languages.is_empty() {
        None
    } else {
        Some(parsed.languages.as_slice())
    };

    match ctx_semantic_search::search_hits(
        &query,
        &root,
        parsed.top_k,
        &parsed.mode,
        languages,
        parsed.path_glob.as_deref(),
    ) {
        Ok(hits) => {
            if parsed.json {
                println!("{}", to_json(&hits));
            } else {
                // Compact one-line-per-result format
                println!(
                    "semantic_search({},{}) \u{2192} {} results",
                    parsed.mode,
                    parsed.top_k,
                    hits.len()
                );
                for h in &hits {
                    let symbol = if h.symbol_name.is_empty() {
                        "?"
                    } else {
                        h.symbol_name.as_str()
                    };
                    println!(
                        "{}:{}  {symbol}  {:.4}",
                        h.file_path, h.start_line, h.rrf_score
                    );
                }
            }
        }
        Err(e) => {
            // In JSON mode the caller (extension) parses stdout; surface the
            // error on stderr and exit non-zero so it is never mistaken for an
            // empty-but-valid result.
            eprintln!("semantic-search: {e}");
            std::process::exit(1);
        }
    }
}

/// Serializes results with stable field names. `file`/`line`/`content`/`score`
/// are the contract the editor extensions depend on; the rest is additive.
fn to_json(hits: &[HybridResult]) -> String {
    #[derive(serde::Serialize)]
    struct Hit<'a> {
        file: &'a str,
        line: usize,
        end_line: usize,
        symbol: &'a str,
        kind: String,
        source: &'static str,
        score: f64,
        content: &'a str,
    }

    let out: Vec<Hit> = hits
        .iter()
        .map(|r| Hit {
            file: &r.file_path,
            line: r.start_line,
            end_line: r.end_line,
            symbol: &r.symbol_name,
            kind: format!("{:?}", r.kind),
            // Derive the source from which score is present rather than from the
            // optional ranks: BM25-only results don't carry a rank but are still
            // unambiguously "bm25".
            source: match (r.bm25_score.is_some(), r.dense_score.is_some()) {
                (true, true) => "hybrid",
                (false, true) => "dense",
                (true, false) => "bm25",
                (false, false) => "unknown",
            },
            score: r.rrf_score,
            content: &r.snippet,
        })
        .collect();

    serde_json::to_string(&out).unwrap_or_else(|_| "[]".to_string())
}

fn print_help() {
    println!(
        "lean-ctx semantic-search — hybrid (BM25 + embeddings) code search\n\n\
         USAGE:\n  lean-ctx semantic-search <query> [OPTIONS]\n\n\
         OPTIONS:\n\
         \x20 -q, --query <text>     Search query (or pass as the first argument)\n\
         \x20 -m, --mode <mode>      bm25 (default, instant) | hybrid | dense\n\
         \x20                        hybrid/dense add embedding ranking (slower first run)\n\
         \x20 -k, --top-k <N>        Number of results (1-100, default 10)\n\
         \x20 -p, --path <dir>       Project root to search (default: cwd)\n\
         \x20     --lang <language>  Restrict to a language (repeatable), e.g. rust\n\
         \x20     --glob <pattern>   Restrict to paths matching a glob, e.g. 'src/**'\n\
         \x20     --json             Emit JSON array [{{file,line,content,score,...}}]\n\
         \x20 -h, --help             Show this help"
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::chunk_data::ChunkKind;

    fn args(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn parses_positional_query_with_defaults() {
        let a = parse_args(&args(&["find the parser"]));
        assert_eq!(a.query.as_deref(), Some("find the parser"));
        assert_eq!(a.mode, "bm25");
        assert_eq!(a.top_k, 10);
        assert!(!a.json);
        assert_eq!(a.path, None, "path defaults to None (auto-detect)");
        assert!(!a.explicit_path, "no --path flag given");
    }

    #[test]
    fn parses_flags() {
        let a = parse_args(&args(&[
            "--json",
            "--query",
            "auth flow",
            "--mode",
            "bm25",
            "--top-k",
            "5",
            "--path",
            "/tmp/p",
            "--lang",
            "rust",
            "--glob",
            "src/**",
        ]));
        assert!(a.json);
        assert_eq!(a.query.as_deref(), Some("auth flow"));
        assert_eq!(a.mode, "bm25");
        assert_eq!(a.top_k, 5);
        assert_eq!(a.path.as_deref(), Some("/tmp/p"));
        assert!(a.explicit_path, "--path was given");
        assert_eq!(a.languages, vec!["rust".to_string()]);
        assert_eq!(a.path_glob.as_deref(), Some("src/**"));
    }

    #[test]
    fn top_k_is_clamped() {
        assert_eq!(parse_args(&args(&["q", "--top-k", "0"])).top_k, 1);
        assert_eq!(parse_args(&args(&["q", "--top-k", "9999"])).top_k, 100);
    }

    #[test]
    fn explicit_query_flag_beats_bare_token() {
        // A bare token only fills the query if --query was not given.
        let a = parse_args(&args(&["--query", "real", "ignored"]));
        assert_eq!(a.query.as_deref(), Some("real"));
    }

    #[test]
    fn json_uses_extension_contract_fields() {
        let hits = vec![HybridResult {
            file_path: "src/main.rs".to_string(),
            symbol_name: "main".to_string(),
            kind: ChunkKind::Function,
            start_line: 12,
            end_line: 20,
            snippet: "fn main() {}".to_string(),
            rrf_score: 0.5,
            bm25_score: Some(0.5),
            dense_score: None,
            bm25_rank: Some(0),
            dense_rank: None,
        }];
        let json = to_json(&hits);
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        let first = &v[0];
        assert_eq!(first["file"], "src/main.rs");
        assert_eq!(first["line"], 12);
        assert_eq!(first["content"], "fn main() {}");
        assert_eq!(first["score"], 0.5);
        assert_eq!(first["source"], "bm25");
    }

    #[test]
    fn empty_results_serialize_as_empty_array() {
        assert_eq!(to_json(&[]), "[]");
    }
}
