//! Edit-time cognitive-complexity delta.
//!
//! Computes how an edit changes per-function cognitive complexity **without
//! touching the index** — pure and deterministic, so it can run inside the
//! edit path (`ctx_edit`, `ctx_patch`) and the native-edit hook to prevent
//! complexity drift at the moment it is introduced. Functions are matched by
//! name (their stable identity across an in-place edit).

use super::cognitive::cognitive_per_function;
use std::collections::BTreeMap;

/// One function's cognitive-complexity change across an edit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CognitiveDelta {
    pub name: String,
    pub before: u32,
    pub after: u32,
}

impl CognitiveDelta {
    /// Signed change (positive = got more complex).
    pub fn increase(&self) -> i64 {
        i64::from(self.after) - i64::from(self.before)
    }

    /// True when the edit pushed a previously-acceptable function over
    /// `threshold` (the case `gate="block"` refuses).
    pub fn crosses_threshold(&self, threshold: u32) -> bool {
        self.before <= threshold && self.after > threshold
    }
}

/// Compare cognitive complexity of every function between `old` and `new`
/// source for file `ext`. Returns only functions whose complexity changed,
/// sorted by name. Empty when tree-sitter is disabled or nothing changed.
///
/// Only functions present in `new` are reported (a deleted function is not a
/// regression the editor needs to hear about).
pub fn cognitive_delta(old: &str, new: &str, ext: &str) -> Vec<CognitiveDelta> {
    let before = map_by_name(old, ext);
    let after = map_by_name(new, ext);

    let mut out: Vec<CognitiveDelta> = after
        .iter()
        .filter_map(|(name, &after_cc)| {
            let before_cc = before.get(name).copied().unwrap_or(0);
            (before_cc != after_cc).then(|| CognitiveDelta {
                name: name.clone(),
                before: before_cc,
                after: after_cc,
            })
        })
        .collect();
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// The most significant increase that ends above `threshold`, if any. This is
/// what the edit-gate surfaces (one line, the worst offender).
pub fn worst_regression(deltas: &[CognitiveDelta], threshold: u32) -> Option<&CognitiveDelta> {
    deltas
        .iter()
        .filter(|d| d.after > d.before && d.after > threshold)
        .max_by_key(|d| {
            (
                d.after - d.before,
                d.after,
                std::cmp::Reverse(d.name.clone()),
            )
        })
}

/// Deterministic one-line edit-gate notice. No timestamps/counters → #498-safe.
pub fn format_gate_notice(delta: &CognitiveDelta, threshold: u32) -> String {
    format!(
        "[CODE HEALTH] fn {}: cognitive {}->{} (+{}, >{}) — consider extracting helpers",
        delta.name,
        delta.before,
        delta.after,
        delta.after.saturating_sub(delta.before),
        threshold
    )
}

fn map_by_name(source: &str, ext: &str) -> BTreeMap<String, u32> {
    let mut map: BTreeMap<String, u32> = BTreeMap::new();
    if let Some(fns) = cognitive_per_function(source, ext) {
        for f in fns {
            let entry = map.entry(f.name).or_insert(0);
            *entry = (*entry).max(f.cognitive);
        }
    }
    map
}

#[cfg(all(test, feature = "tree-sitter"))]
mod tests {
    use super::*;

    #[test]
    fn reports_increase_for_edited_function() {
        let old = "fn f(a: bool) { if a {} }";
        let new = "fn f(a: bool, b: bool) { if a { if b {} } }";
        let deltas = cognitive_delta(old, new, "rs");
        assert_eq!(deltas.len(), 1);
        assert_eq!(deltas[0].name, "f");
        assert_eq!(deltas[0].before, 1);
        assert_eq!(deltas[0].after, 3);
        assert_eq!(deltas[0].increase(), 2);
    }

    #[test]
    fn ignores_unchanged_functions() {
        let src = "fn stable(a: bool) { if a {} }";
        assert!(cognitive_delta(src, src, "rs").is_empty());
    }

    #[test]
    fn new_function_starts_from_zero() {
        let old = "fn a() {}";
        let new = "fn a() {}\nfn b(x: bool) { if x { if x {} } }";
        let deltas = cognitive_delta(old, new, "rs");
        let b = deltas.iter().find(|d| d.name == "b").unwrap();
        assert_eq!(b.before, 0);
        assert_eq!(b.after, 3);
    }

    #[test]
    fn worst_regression_picks_threshold_crosser() {
        let deltas = vec![
            CognitiveDelta {
                name: "small".into(),
                before: 2,
                after: 5,
            },
            CognitiveDelta {
                name: "big".into(),
                before: 10,
                after: 20,
            },
        ];
        let worst = worst_regression(&deltas, 15).unwrap();
        assert_eq!(worst.name, "big");
        assert!(deltas[1].crosses_threshold(15));
        assert!(!deltas[0].crosses_threshold(15));
    }

    #[test]
    fn notice_is_deterministic() {
        let d = CognitiveDelta {
            name: "foo".into(),
            before: 8,
            after: 16,
        };
        let n1 = format_gate_notice(&d, 15);
        let n2 = format_gate_notice(&d, 15);
        assert_eq!(n1, n2);
        assert!(n1.contains("cognitive 8->16 (+8, >15)"));
    }
}
