//! `OpenAPI` stability gate (GL #394): the public `/v1` surface is frozen.
//!
//! Compares the in-code endpoint inventory (`core::openapi::endpoints()`, the
//! SSOT behind `GET /v1/openapi.json`) against the committed snapshot
//! `docs/reference/openapi-v1.snapshot.json`:
//!
//! * **additive** diffs (new routes) are allowed — refresh the snapshot via
//!   `LEANCTX_UPDATE_OPENAPI_SNAPSHOT=1 cargo test --test openapi_stability`;
//! * **breaking** diffs (a snapshot route disappears or changes its auth
//!   requirement) fail CI: frozen surfaces evolve via `/v2`, not in place.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use lean_ctx::core::openapi::endpoints;

fn snapshot_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../docs/reference/openapi-v1.snapshot.json")
}

/// "METHOD path" → auth. Summaries are documentation, not contract — they are
/// deliberately excluded so wording fixes never trip the gate.
fn current_surface() -> BTreeMap<String, String> {
    endpoints()
        .iter()
        .map(|e| (format!("{} {}", e.method, e.path), e.auth.to_string()))
        .collect()
}

#[test]
fn public_v1_surface_never_shrinks_or_mutates() {
    let current = current_surface();
    let snap_path = snapshot_path();

    if std::env::var_os("LEANCTX_UPDATE_OPENAPI_SNAPSHOT").is_some() {
        let json = serde_json::to_string_pretty(&current).expect("serialize snapshot");
        std::fs::write(&snap_path, json + "\n").expect("write snapshot");
        eprintln!("openapi-v1.snapshot.json regenerated");
        return;
    }

    let snapshot: BTreeMap<String, String> =
        serde_json::from_str(&std::fs::read_to_string(&snap_path).unwrap_or_else(|e| {
            panic!(
                "missing {} — generate it once via \
                 LEANCTX_UPDATE_OPENAPI_SNAPSHOT=1 cargo test --test openapi_stability ({e})",
                snap_path.display()
            )
        }))
        .expect("openapi snapshot is valid JSON");

    for (route, auth) in &snapshot {
        match current.get(route) {
            None => panic!(
                "BREAKING: route `{route}` was removed from the public /v1 surface.\n\
                 Frozen routes cannot be deleted (CONTRACTS.md § Stability matrix); \
                 deprecate per policy and serve it until a /v2 surface exists."
            ),
            Some(current_auth) if current_auth != auth => panic!(
                "BREAKING: route `{route}` changed auth `{auth}` → `{current_auth}`.\n\
                 Auth semantics of frozen routes are immutable."
            ),
            Some(_) => {}
        }
    }
    // New routes (in `current` but not in the snapshot) are additive and pass.
}
