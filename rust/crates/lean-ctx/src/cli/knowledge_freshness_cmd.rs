// SPDX-License-Identifier: Apache-2.0
//! `lean-ctx knowledge stale | verify | dedupe | outdated | conflicts --json`:
//! the freshness listings and the weekly clean-up primitives, one
//! `local-read/1` document each (shapes: `ctx_knowledge::freshness`), printed
//! and failed exactly as the other Local Read documents (`cli::local_read`).
//!
//! Writes: `verify <id>`, `dedupe --apply` (acts only on a project the call
//! names with `--project-root`; `dedupe` alone is a dry run), and `stale`,
//! which writes back stored anchor marks that disagree with the live check.

use lean_ctx_server::tools::ctx_knowledge::freshness::{self, FreshnessError};

use super::local_read::{self, Code, Failure};

pub(crate) fn run(action: &str, args: &[String]) {
    let project_root = match local_read::project_root(args) {
        Ok(root) => root,
        Err(failure) => local_read::fail(&failure),
    };
    let kind = format!("knowledge.{action}");
    let root = project_root.as_str();
    match action {
        "stale" => local_read::emit_released("ctx_knowledge", root, &kind, || {
            let limit = local_read::limit(args, freshness::DEFAULT_LIMIT, freshness::MAX_LIMIT)?;
            Ok(freshness::stale(root, Some(limit)))
        }),
        "verify" => local_read::emit_released("ctx_knowledge", root, &kind, || {
            let id = local_read::positionals(args)
                .into_iter()
                .skip_while(|a| *a != "verify")
                .nth(1)
                .ok_or_else(|| Failure::invalid("verify needs a fact id"))?;
            freshness::verify(root, id).map_err(failure)
        }),
        "dedupe" => local_read::emit_released("ctx_knowledge", root, &kind, || {
            let apply = args.iter().any(|a| a == "--apply");
            if apply && args.iter().any(|a| a == "--dry-run") {
                return Err(Failure::invalid("--apply and --dry-run exclude each other"));
            }
            if apply && !names_project(args) {
                return Err(Failure::invalid(
                    "dedupe --apply acts only on a project the call names: add --project-root",
                ));
            }
            let threshold = number::<f32>(args, "--threshold")?;
            freshness::dedupe(root, threshold, apply).map_err(failure)
        }),
        "outdated" => local_read::emit_released("ctx_knowledge", root, &kind, || {
            let limit = local_read::limit(args, freshness::DEFAULT_LIMIT, freshness::MAX_LIMIT)?;
            let days = number::<u32>(args, "--days")?;
            freshness::outdated(root, days, Some(limit), chrono::Utc::now()).map_err(failure)
        }),
        "conflicts" => local_read::emit_released("ctx_knowledge", root, &kind, || {
            Ok(freshness::conflicts(root))
        }),
        other => local_read::fail(&Failure::invalid(format!("unknown action {other}"))),
    }
}

fn failure(error: FreshnessError) -> Failure {
    let code = match error.code {
        "invalid_argument" => Code::InvalidArgument,
        "not_found" => Code::NotFound,
        "anchor_gone" => Code::AnchorGone,
        "unavailable" => Code::Unavailable,
        _ => Code::Internal,
    };
    Failure::new(code, error.message)
}

fn names_project(args: &[String]) -> bool {
    args.iter().any(|a| {
        a == "--project-root"
            || a == "--root"
            || a.starts_with("--project-root=")
            || a.starts_with("--root=")
    })
}

fn number<T: std::str::FromStr>(args: &[String], flag: &str) -> Result<Option<T>, Failure> {
    local_read::flag(args, flag)?
        .map(|raw| {
            raw.parse::<T>()
                .map_err(|_| Failure::invalid(format!("{flag} {raw:?} is not a number")))
        })
        .transpose()
}
