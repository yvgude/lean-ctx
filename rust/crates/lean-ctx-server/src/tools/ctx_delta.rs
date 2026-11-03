// SPDX-License-Identifier: Apache-2.0
use crate::tools::CrpMode;
use lean_ctx::core::cache::SessionCache;

/// Thin redirect: delegates to ctx_read mode=diff.
pub fn handle(cache: &mut SessionCache, path: &str) -> String {
    crate::tools::ctx_read::handle(cache, path, "diff", CrpMode::effective())
}
