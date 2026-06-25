//! Minimum Description Length–style selection among abstract read modes (proxy compressed lengths).

use super::compressor::aggressive_compress;
use super::entropy::entropy_compress;
use super::signatures::{Signature, extract_file_map, extract_signatures};
use super::tokens::count_tokens;

#[derive(Clone, Copy)]
struct ModeSpec {
    name: &'static str,
    /// Prior cost added to compressed token estimate (mode complexity).
    model_cost: usize,
}

const MODES: [ModeSpec; 5] = [
    ModeSpec {
        name: "full",
        model_cost: 0,
    },
    ModeSpec {
        name: "map",
        model_cost: 50,
    },
    ModeSpec {
        name: "signatures",
        model_cost: 80,
    },
    ModeSpec {
        name: "aggressive",
        model_cost: 120,
    },
    ModeSpec {
        name: "entropy",
        model_cost: 140,
    },
];

fn synthetic_path_for(content: &str) -> &'static str {
    if content.contains("def ")
        && content
            .lines()
            .next()
            .is_some_and(|l| l.trim_start().starts_with("def "))
    {
        "snippet.py"
    } else if content.contains("package ")
        || content.contains("func ")
        || content.lines().any(|l| l.starts_with("func "))
    {
        "snippet.go"
    } else {
        "snippet.rs"
    }
}

fn ext_from_path(path: &str) -> &str {
    path.rsplit_once('.').map_or("rs", |(_, e)| e)
}

fn render_signatures(compact: &[String]) -> String {
    compact.join("\n")
}

fn compressed_tokens_for(mode: &str, content: &str, path: &str) -> usize {
    let ext = ext_from_path(path);
    match mode {
        "map" => count_tokens(&extract_file_map(path, content)),
        "signatures" => {
            let sigs = extract_signatures(content, ext);
            let lines: Vec<String> = sigs.iter().map(Signature::to_compact_located).collect();
            count_tokens(&render_signatures(&lines))
        }
        "aggressive" => count_tokens(&aggressive_compress(content, Some(ext))),
        "entropy" => count_tokens(&entropy_compress(content).output),
        _ => count_tokens(content),
    }
}

/// Pick read mode minimizing MDL proxy `compressed_tokens + model_cost` among modes whose compressed size fits `budget_tokens`.
#[must_use]
pub fn select_mode(content: &str, budget_tokens: usize) -> &'static str {
    if content.is_empty() {
        return "full";
    }
    let path = synthetic_path_for(content);

    let mut best_feasible: Option<(&'static str, usize)> = None;
    let mut best_fallback: Option<(&'static str, usize)> = None;

    for m in &MODES {
        let ct = compressed_tokens_for(m.name, content, path);
        let dl = ct.saturating_add(m.model_cost);

        let cand_opt = best_fallback.map_or(Some((m.name, dl)), |(bn, bd)| {
            Some(if dl < bd || (dl == bd && m.name < bn) {
                (m.name, dl)
            } else {
                (bn, bd)
            })
        });
        best_fallback = cand_opt;

        if ct <= budget_tokens {
            best_feasible = Some(match best_feasible {
                None => (m.name, dl),
                Some((bn, bd)) => {
                    if dl < bd || (dl == bd && m.name < bn) {
                        (m.name, dl)
                    } else {
                        (bn, bd)
                    }
                }
            });
        }
    }

    best_feasible.or(best_fallback).map_or("full", |(n, _)| n)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_returns_full() {
        assert_eq!(select_mode("", 100), "full");
    }

    #[test]
    fn large_budget_picks_some_mode() {
        let code = "pub fn foo() -> i32 { 1 }\npub fn bar(x: u32) {}\n";
        let ub = count_tokens(code) + 50_000;
        let m = select_mode(code, ub);
        assert!(matches!(
            m,
            "full" | "map" | "signatures" | "aggressive" | "entropy"
        ));
    }

    #[test]
    fn tight_budget_avoids_full() {
        let repetitive = "a ".repeat(200);
        let m = select_mode(&repetitive, 5);
        assert_ne!(m, "full");
    }

    #[test]
    fn respects_budget_when_full_fits() {
        let py = "def foo():\n    pass\n";
        let t_full = compressed_tokens_for("full", py, "snippet.py");
        let mode = select_mode(py, t_full);
        let ct = compressed_tokens_for(mode, py, "snippet.py");
        assert!(ct <= t_full);
    }
}
