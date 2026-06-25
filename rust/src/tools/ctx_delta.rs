use crate::core::cache::SessionCache;
use crate::tools::CrpMode;

/// Thin redirect: delegates to `ctx_read` mode=diff.
pub fn handle(cache: &mut SessionCache, path: &str) -> String {
    crate::tools::ctx_read::handle(cache, path, "diff", CrpMode::effective())
}
