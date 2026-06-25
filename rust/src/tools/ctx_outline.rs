use crate::core::cache::SessionCache;
use crate::core::signatures::{Signature, extract_signatures};
use crate::core::tokens::count_tokens;
use crate::tools::CrpMode;

/// Thin redirect: delegates to `ctx_read` mode=signatures with optional kind filter.
#[must_use]
pub fn handle(path: &str, kind_filter: Option<&str>) -> (String, usize) {
    let p = std::path::Path::new(path);
    if p.symlink_metadata()
        .is_ok_and(|m| m.file_type().is_symlink())
    {
        return (
            format!("ERROR: {path} is a symlink (skipped for security)"),
            0,
        );
    }
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) => return (format!("ERROR: Cannot read {path}: {e}"), 0),
    };
    let full_tokens = count_tokens(&content);
    let ext = std::path::Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("");

    let sigs = extract_signatures(&content, ext);
    if sigs.is_empty() {
        return (format!("No symbols found in {path}"), 0);
    }

    let filtered = filter_by_kind(&sigs, kind_filter);
    let crp = CrpMode::effective();
    let mut outline: String = filtered
        .iter()
        .map(|s| {
            if crp.is_tdd() {
                s.to_tdd()
            } else {
                s.to_compact()
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    // Self-describing outputs (GL #580): symbol notation carries its legend.
    if crp.is_tdd() {
        let legend = crate::core::signatures::tdd_legend(&filtered);
        if !legend.is_empty() {
            outline = format!("{legend}\n{outline}");
        }
    }

    let sent = count_tokens(&outline);
    let savings = crate::core::protocol::format_savings(full_tokens, sent);
    (format!("{outline}\n{savings}"), full_tokens)
}

/// Also available via `ctx_read` mode=signatures. This adapts to the `SessionCache` path.
pub fn handle_via_read(
    cache: &mut SessionCache,
    path: &str,
    kind_filter: Option<&str>,
    crp_mode: CrpMode,
) -> String {
    if kind_filter.is_none() || kind_filter == Some("all") {
        return crate::tools::ctx_read::handle(cache, path, "signatures", crp_mode);
    }
    let (result, _) = handle(path, kind_filter);
    result
}

#[must_use]
pub fn filter_by_kind<'a>(sigs: &'a [Signature], kind: Option<&str>) -> Vec<&'a Signature> {
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
