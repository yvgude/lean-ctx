//! Read-time code-health annotations.
//!
//! Produces **sparse, deterministic** per-function annotations (`cc=18`) for the
//! over-threshold functions in a file, so an agent reading signatures or the map
//! sees complexity hotspots inline and can decide *not* to read a giant function
//! in full. Only functions above the threshold are annotated, keeping output
//! byte-stable and cheap (#498-safe).

use super::cognitive::cognitive_per_function;
use std::collections::HashMap;

/// One inline annotation for a function in a read view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadAnnotation {
    /// 1-based start line of the function.
    pub line: usize,
    pub name: String,
    /// The compact note to append, e.g. `cc=18`.
    pub note: String,
}

/// Annotations for the over-threshold functions in `source` of file `ext`.
/// Sorted by line then name. Empty when tree-sitter is off, the extension is
/// unsupported, or nothing is over threshold.
pub fn annotations_for_file(source: &str, ext: &str, threshold: u32) -> Vec<ReadAnnotation> {
    let mut out: Vec<ReadAnnotation> = Vec::new();
    if let Some(fns) = cognitive_per_function(source, ext) {
        for f in fns {
            if f.cognitive > threshold {
                out.push(ReadAnnotation {
                    line: f.line,
                    name: f.name,
                    note: format!("cc={}", f.cognitive),
                });
            }
        }
    }
    out.sort_by(|a, b| a.line.cmp(&b.line).then_with(|| a.name.cmp(&b.name)));
    out
}

/// Index annotations by function name for renderers that match on the symbol
/// name (more robust than line numbers when attributes/decorators shift lines).
pub fn by_name(annotations: &[ReadAnnotation]) -> HashMap<String, String> {
    annotations
        .iter()
        .map(|a| (a.name.clone(), a.note.clone()))
        .collect()
}

/// Cognitive complexity of the function named `name` defined nearest `start_line`
/// in `source` (file extension `ext`). Unlike [`annotations_for_file`] this is
/// *not* threshold-gated, so a targeted `ctx_symbol` query can show the exact cc
/// of any function. `None` when tree-sitter is off, the extension is
/// unsupported, or no function with that name is present (e.g. a struct symbol).
pub fn cognitive_for_symbol(source: &str, ext: &str, name: &str, start_line: usize) -> Option<u32> {
    let fns = cognitive_per_function(source, ext)?;
    fns.iter()
        .filter(|f| f.name == name)
        .min_by_key(|f| f.line.abs_diff(start_line))
        .map(|f| f.cognitive)
}

#[cfg(all(test, feature = "tree-sitter"))]
mod tests {
    use super::*;

    #[test]
    fn annotates_only_over_threshold() {
        // `deep` = 1+2+3+4 = 10 cognitive; `flat` = 0.
        let src = "fn flat() {}\nfn deep(a: bool) { if a { if a { if a { if a {} } } } }\n";
        let anns = annotations_for_file(src, "rs", 5);
        assert_eq!(anns.len(), 1);
        assert_eq!(anns[0].name, "deep");
        assert_eq!(anns[0].note, "cc=10");
    }

    #[test]
    fn nothing_when_under_threshold() {
        let src = "fn small(a: bool) { if a {} }\n";
        assert!(annotations_for_file(src, "rs", 15).is_empty());
    }

    #[test]
    fn deterministic_across_runs() {
        let src = "fn deep(a: bool) { if a { if a { if a { if a {} } } } }\n";
        assert_eq!(
            annotations_for_file(src, "rs", 5),
            annotations_for_file(src, "rs", 5)
        );
    }

    #[test]
    fn by_name_lookup() {
        let src = "fn deep(a: bool) { if a { if a { if a { if a {} } } } }\n";
        let anns = annotations_for_file(src, "rs", 5);
        let map = by_name(&anns);
        assert_eq!(map.get("deep").map(String::as_str), Some("cc=10"));
    }

    #[test]
    fn cognitive_for_symbol_reports_any_function() {
        let src = "fn flat() {}\nfn deep(a: bool) { if a { if a { if a {} } } }\n";
        // Not threshold-gated: even the flat function resolves (cc=0).
        assert_eq!(cognitive_for_symbol(src, "rs", "flat", 1), Some(0));
        assert_eq!(cognitive_for_symbol(src, "rs", "deep", 2), Some(6));
        assert_eq!(cognitive_for_symbol(src, "rs", "missing", 1), None);
    }

    #[test]
    fn cognitive_for_symbol_disambiguates_by_line() {
        // Two same-named functions: pick the one nearest the queried line.
        let src = "fn dup(a: bool) { if a {} }\nfn other() {}\nfn dup(a: bool) { if a { if a { if a {} } } }\n";
        assert_eq!(cognitive_for_symbol(src, "rs", "dup", 1), Some(1));
        assert_eq!(cognitive_for_symbol(src, "rs", "dup", 3), Some(6));
    }
}
