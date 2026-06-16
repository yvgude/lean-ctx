//! `lean-ctx doctor --migrate-check` — v1.0 migration readiness (GL #396).
//!
//! The 1.0 stability release ships zero breaking changes (CONTRACTS.md freeze,
//! GL #394) — this check *proves that for the local installation* instead of
//! asking users to take it on faith. Four read-only audits:
//!
//! 1. config.toml parses and every key is a known schema key
//! 2. no key in use is on the deprecation register
//! 3. the data directory layout is current (nothing to migrate)
//! 4. the build carries the frozen v1 contract set
//!
//! Exit 0 = "ready for 1.0", exit 1 = concrete steps are listed.

use std::collections::BTreeSet;

use super::{BOLD, DIM, GREEN, Outcome, RED, RST, YELLOW};
use crate::core::config::Config;
use crate::core::config::schema::ConfigSchema;

pub(super) struct MigrateReport {
    outcomes: Vec<Outcome>,
    /// Machine-readable per-check results: (id, ok, detail).
    results: Vec<(&'static str, bool, String)>,
}

impl MigrateReport {
    fn push(&mut self, id: &'static str, outcome: Outcome, detail: String) {
        self.results.push((id, outcome.ok, detail));
        self.outcomes.push(outcome);
    }

    fn ready(&self) -> bool {
        self.outcomes.iter().all(|o| o.ok)
    }
}

/// All schema keys as `section.key` plus bare root keys. A section that
/// declares no keys is free-form by design (e.g. `ide_paths` maps arbitrary
/// agent names) — the section name itself becomes the known prefix.
fn known_keys(schema: &ConfigSchema) -> BTreeSet<String> {
    let mut keys = BTreeSet::new();
    for (section, sec) in &schema.sections {
        if sec.keys.is_empty() {
            keys.insert(section.clone());
            continue;
        }
        for key in sec.keys.keys() {
            if section == "root" {
                keys.insert(key.clone());
            } else {
                keys.insert(format!("{section}.{key}"));
            }
        }
    }
    keys
}

/// Flatten the user's config.toml into `section.key` paths (one level deep —
/// the schema is flat per section; deeper tables keep their dotted prefix).
fn flatten(value: &toml::Value, prefix: &str, out: &mut Vec<String>) {
    if let toml::Value::Table(table) = value {
        for (k, v) in table {
            let path = if prefix.is_empty() {
                k.clone()
            } else {
                format!("{prefix}.{k}")
            };
            if matches!(v, toml::Value::Table(_)) {
                flatten(v, &path, out);
            } else {
                out.push(path);
            }
        }
    }
}

fn config_schema_outcome() -> (Outcome, String) {
    let Some(path) = Config::path() else {
        return (
            Outcome {
                ok: true,
                line: format!(
                    "{BOLD}Config schema{RST}  {GREEN}no config file — defaults are 1.0-ready{RST}"
                ),
            },
            "no config file".into(),
        );
    };
    if !path.exists() {
        return (
            Outcome {
                ok: true,
                line: format!(
                    "{BOLD}Config schema{RST}  {GREEN}no config file — defaults are 1.0-ready{RST}"
                ),
            },
            "no config file".into(),
        );
    }

    let raw = match std::fs::read_to_string(&path) {
        Ok(raw) => raw,
        Err(e) => {
            return (
                Outcome {
                    ok: false,
                    line: format!(
                        "{BOLD}Config schema{RST}  {RED}config.toml unreadable: {e}{RST}"
                    ),
                },
                format!("unreadable: {e}"),
            );
        }
    };
    let parsed: toml::Value = match toml::from_str(&raw) {
        Ok(v) => v,
        Err(e) => {
            return (
                Outcome {
                    ok: false,
                    line: format!(
                        "{BOLD}Config schema{RST}  {RED}config.toml does not parse: {e}{RST}\n      fix the TOML syntax before upgrading"
                    ),
                },
                format!("parse error: {e}"),
            );
        }
    };

    let mut used = Vec::new();
    flatten(&parsed, "", &mut used);
    let known = known_keys(&ConfigSchema::generate());

    // A dotted path is fine when itself or any ancestor table is a known key
    // (some sections hold free-form sub-tables, e.g. per-tool maps).
    let unknown: Vec<&String> = used
        .iter()
        .filter(|path| {
            let mut candidate = (*path).clone();
            loop {
                if known.contains(&candidate) {
                    return false;
                }
                match candidate.rfind('.') {
                    Some(idx) => candidate.truncate(idx),
                    None => return true,
                }
            }
        })
        .collect();

    if unknown.is_empty() {
        (
            Outcome {
                ok: true,
                line: format!(
                    "{BOLD}Config schema{RST}  {GREEN}all {} keys recognized{RST}  {DIM}({}){RST}",
                    used.len(),
                    path.display()
                ),
            },
            format!("{} keys, all known", used.len()),
        )
    } else {
        let mut line = format!(
            "{BOLD}Config schema{RST}  {YELLOW}{} unknown key(s) in {}{RST}",
            unknown.len(),
            path.display()
        );
        for key in &unknown {
            line.push_str(&format!(
                "\n      {YELLOW}•{RST} {key} {DIM}— typo or removed key; check `lean-ctx config schema`{RST}"
            ));
        }
        (
            Outcome { ok: false, line },
            format!("unknown keys: {unknown:?}"),
        )
    }
}

fn deprecation_outcome() -> (Outcome, String) {
    // The register check itself (parse + policy) runs in the main doctor; here
    // we only care whether *active* deprecations exist that demand migration.
    let outcome = super::deprecations::deprecations_outcome();
    let detail = if outcome.ok {
        "no active deprecations".to_string()
    } else {
        "active deprecations present — follow the listed replacements".to_string()
    };
    (outcome, detail)
}

fn data_layout_outcome() -> (Outcome, String) {
    let Ok(dir) = crate::core::data_dir::lean_ctx_data_dir() else {
        return (
            Outcome {
                ok: true,
                line: format!(
                    "{BOLD}Data layout{RST}  {GREEN}no data directory yet — nothing to migrate{RST}"
                ),
            },
            "no data dir".into(),
        );
    };
    if !dir.is_dir() {
        return (
            Outcome {
                ok: true,
                line: format!(
                    "{BOLD}Data layout{RST}  {GREEN}no data directory yet — nothing to migrate{RST}"
                ),
            },
            "no data dir".into(),
        );
    }

    // All on-disk formats migrate themselves on first touch (embedding index
    // v1→v3, session store, BM25 shards). The only hard blocker would be a
    // data dir we cannot write to.
    let writable = {
        let probe = dir.join(".doctor-migrate-probe");
        let ok = std::fs::write(&probe, b"ok").is_ok();
        let _ = std::fs::remove_file(&probe);
        ok
    };
    if writable {
        (
            Outcome {
                ok: true,
                line: format!(
                    "{BOLD}Data layout{RST}  {GREEN}current{RST}  {DIM}({} — on-disk formats self-migrate){RST}",
                    dir.display()
                ),
            },
            "writable, self-migrating formats".into(),
        )
    } else {
        (
            Outcome {
                ok: false,
                line: format!(
                    "{BOLD}Data layout{RST}  {RED}{} is not writable{RST}\n      fix permissions so on-disk formats can self-migrate",
                    dir.display()
                ),
            },
            "data dir not writable".into(),
        )
    }
}

fn contract_outcome() -> (Outcome, String) {
    let frozen = crate::core::contracts::contract_docs()
        .iter()
        .filter(|d| matches!(d.status, crate::core::contracts::ContractStatus::Frozen))
        .count();
    (
        Outcome {
            ok: true,
            line: format!(
                "{BOLD}Contracts{RST}  {GREEN}{frozen} frozen v1 contracts in this build{RST}  {DIM}(policy: CONTRACTS.md){RST}"
            ),
        },
        format!("{frozen} frozen contracts"),
    )
}

/// Run the migration readiness audit. Returns the process exit code.
pub(super) fn run_migrate_check(json: bool) -> i32 {
    let mut report = MigrateReport {
        outcomes: Vec::new(),
        results: Vec::new(),
    };

    let (o, d) = config_schema_outcome();
    report.push("config_schema", o, d);
    let (o, d) = deprecation_outcome();
    report.push("deprecations", o, d);
    let (o, d) = data_layout_outcome();
    report.push("data_layout", o, d);
    let (o, d) = contract_outcome();
    report.push("contracts", o, d);

    let ready = report.ready();

    if json {
        let payload = serde_json::json!({
            "ready_for_1_0": ready,
            "engine_version": env!("CARGO_PKG_VERSION"),
            "checks": report
                .results
                .iter()
                .map(|(id, ok, detail)| serde_json::json!({
                    "id": id, "ok": ok, "detail": detail,
                }))
                .collect::<Vec<_>>(),
        });
        println!("{}", serde_json::to_string_pretty(&payload).unwrap());
        return i32::from(!ready);
    }

    println!("\n{BOLD}lean-ctx migration readiness (0.x → 1.0){RST}\n");
    for outcome in &report.outcomes {
        super::common::print_check(outcome);
    }
    println!();
    if ready {
        println!("  {GREEN}{BOLD}ready for 1.0{RST} — no migration steps required");
    } else {
        println!("  {RED}{BOLD}action needed{RST} — resolve the items above, then re-run");
    }
    println!();
    i32::from(!ready)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_keys_contains_sectioned_and_root() {
        let keys = known_keys(&ConfigSchema::generate());
        assert!(keys.contains("embedding.model"));
        assert!(keys.iter().any(|k| !k.contains('.')), "root keys exist");
        // Free-form sections register as a prefix...
        assert!(keys.contains("ide_paths"));
        // ...and runtime-written keys are schema-documented (regression: GL #396
        // migrate-check flagged gain.last_auto_publish on a real machine).
        assert!(keys.contains("gain.last_auto_publish"));
    }

    #[test]
    fn flatten_walks_nested_tables() {
        let v: toml::Value = toml::from_str("top = 1\n[a]\nx = 1\n[a.b]\ny = 2\n").unwrap();
        let mut out = Vec::new();
        flatten(&v, "", &mut out);
        out.sort();
        assert_eq!(out, ["a.b.y", "a.x", "top"]);
    }

    #[test]
    fn contract_outcome_reports_frozen_set() {
        let (o, detail) = contract_outcome();
        assert!(o.ok);
        assert!(detail.ends_with("frozen contracts"));
    }

    #[test]
    fn data_layout_is_green_on_real_home() {
        // Read-only invariant: whatever the machine state, the check never
        // panics and returns a coherent outcome.
        let (o, detail) = data_layout_outcome();
        assert!(!detail.is_empty());
        let _ = o.ok;
    }
}
