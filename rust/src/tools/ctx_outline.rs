use std::path::Path;

use crate::core::protocol;
use crate::core::signatures::{extract_signatures, Signature};
use crate::core::tokens::count_tokens;
use crate::tools::CrpMode;

pub fn handle(path: &str, kind_filter: Option<&str>) -> (String, usize) {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) => return (format!("ERROR: Cannot read {path}: {e}"), 0),
    };

    let ext = Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("");

    let sigs = extract_signatures(&content, ext);

    if sigs.is_empty() {
        return (format!("No symbols found in {path}"), 0);
    }

    let filtered = filter_by_kind(&sigs, kind_filter);

    let crp = CrpMode::from_env();
    let outline = format_outline(&filtered, path, crp);

    let full_tokens = count_tokens(&content);
    let outline_tokens = count_tokens(&outline);
    let savings = protocol::format_savings(full_tokens, outline_tokens);

    let filename = Path::new(path)
        .file_name()
        .and_then(|f| f.to_str())
        .unwrap_or(path);

    let line_count = content.lines().count();

    let header = format!(
        "{filename} ({line_count}L, {full_tokens} tok) — {} symbols{}",
        filtered.len(),
        kind_filter
            .map(|k| format!(" [filter: {k}]"))
            .unwrap_or_default()
    );

    (format!("{header}\n{outline}\n{savings}"), full_tokens)
}

fn filter_by_kind<'a>(sigs: &'a [Signature], kind: Option<&str>) -> Vec<&'a Signature> {
    match kind {
        None | Some("all") => sigs.iter().collect(),
        Some(k) => {
            let k_lower = k.to_lowercase();
            sigs.iter()
                .filter(|s| s.kind.to_lowercase() == k_lower)
                .collect()
        }
    }
}

fn format_outline(sigs: &[&Signature], _path: &str, crp: CrpMode) -> String {
    sigs.iter()
        .map(|s| {
            if crp.is_tdd() {
                s.to_tdd()
            } else {
                s.to_compact()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::signatures::Signature;

    fn sample_sigs() -> Vec<Signature> {
        vec![
            Signature {
                kind: "fn",
                name: "main".to_string(),
                params: String::new(),
                return_type: String::new(),
                is_async: false,
                is_exported: false,
                indent: 0,
                ..Signature::no_span()
            },
            Signature {
                kind: "struct",
                name: "Config".to_string(),
                params: String::new(),
                return_type: String::new(),
                is_async: false,
                is_exported: true,
                indent: 0,
                ..Signature::no_span()
            },
            Signature {
                kind: "fn",
                name: "load".to_string(),
                params: "path: &str".to_string(),
                return_type: "Self".to_string(),
                is_async: false,
                is_exported: true,
                indent: 2,
                ..Signature::no_span()
            },
        ]
    }

    #[test]
    fn filter_fn_only() {
        let sigs = sample_sigs();
        let filtered = filter_by_kind(&sigs, Some("fn"));
        assert_eq!(filtered.len(), 2);
    }

    #[test]
    fn filter_struct_only() {
        let sigs = sample_sigs();
        let filtered = filter_by_kind(&sigs, Some("struct"));
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].name, "Config");
    }

    #[test]
    fn filter_all_returns_everything() {
        let sigs = sample_sigs();
        let filtered = filter_by_kind(&sigs, Some("all"));
        assert_eq!(filtered.len(), 3);
    }

    #[test]
    fn filter_none_returns_everything() {
        let sigs = sample_sigs();
        let filtered = filter_by_kind(&sigs, None);
        assert_eq!(filtered.len(), 3);
    }
}
