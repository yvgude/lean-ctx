use std::collections::HashMap;
use std::sync::OnceLock;

use tree_sitter::Query;

use super::queries::{get_language, get_query};

pub(crate) fn get_cached_sig_query(file_ext: &str) -> Option<&'static Query> {
    static SIG_QUERY_CACHE: OnceLock<HashMap<&'static str, Query>> = OnceLock::new();

    let cache = SIG_QUERY_CACHE.get_or_init(|| {
        let mut map = HashMap::new();
        let exts: &[&str] = &[
            "rs", "ts", "tsx", "js", "jsx", "py", "go", "java", "c", "h", "cpp", "cc", "cxx",
            "hpp", "rb", "cs", "kt", "kts", "swift", "php", "sh", "bash", "dart", "scala", "sc",
            "ex", "exs", "zig", "gd", "lua", "luau", "ml", "mli", "hs", "jl", "sol", "nix", "ps1",
            "psm1",
        ];
        for &ext in exts {
            if let (Some(lang), Some(src)) = (get_language(ext), get_query(ext))
                && let Ok(q) = Query::new(&lang, src)
            {
                map.insert(ext, q);
            }
        }
        map
    });

    cache.get(file_ext)
}
