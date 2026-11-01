// SPDX-License-Identifier: Apache-2.0

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use tree_sitter::{Language, Query};

use super::queries::{get_language, get_query};

pub(crate) fn get_cached_sig_query(file_ext: &str) -> Option<Arc<Query>> {
    static SIG_QUERY_CACHE: OnceLock<Mutex<HashMap<Language, Arc<Query>>>> = OnceLock::new();

    let language = get_language(file_ext)?;
    let source = get_query(file_ext)?;
    let mut cache = match SIG_QUERY_CACHE
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
    {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };

    if let Some(query) = cache.get(&language) {
        return Some(Arc::clone(query));
    }

    let query = Arc::new(Query::new(&language, source).ok()?);
    cache.insert(language, Arc::clone(&query));
    Some(query)
}
