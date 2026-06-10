//! Metrics-contract drift gate (GL #401).
//!
//! Customer Datadog/Grafana dashboards reference these metric names and label
//! keys — renaming one silently breaks every dashboard built on it. This test
//! freezes the `/metrics` surface (name, type, label keys; HELP text may
//! evolve) against `docs/reference/metrics-contract.json`.
//!
//! Intentional changes: `LEANCTX_UPDATE_METRICS_CONTRACT=1 cargo test --test
//! metrics_contract` regenerates the snapshot — justify the change in the MR
//! and treat removals/renames as breaking (additive entries are fine).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Debug, PartialEq, serde::Serialize, serde::Deserialize)]
struct MetricSpec {
    #[serde(rename = "type")]
    metric_type: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    labels: Vec<String>,
}

fn contract_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../docs/reference/metrics-contract.json")
}

/// Parse the Prometheus text format into name → (type, sorted label keys).
fn parse_exposition(text: &str) -> BTreeMap<String, MetricSpec> {
    let mut types: BTreeMap<String, String> = BTreeMap::new();
    let mut labels: BTreeMap<String, Vec<String>> = BTreeMap::new();

    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("# TYPE ") {
            let mut parts = rest.split_whitespace();
            if let (Some(name), Some(ty)) = (parts.next(), parts.next()) {
                types.insert(name.to_string(), ty.to_string());
            }
            continue;
        }
        if line.starts_with('#') || line.trim().is_empty() {
            continue;
        }
        // Sample line: `name{k1="v",k2="v"} value` or `name value`.
        let name_end = line.find(['{', ' ']).unwrap_or(line.len());
        let name = &line[..name_end];
        let mut keys: Vec<String> = Vec::new();
        if let (Some(open), Some(close)) = (line.find('{'), line.rfind('}')) {
            for pair in line[open + 1..close].split(',') {
                if let Some(eq) = pair.find('=') {
                    keys.push(pair[..eq].trim().to_string());
                }
            }
        }
        keys.sort();
        labels.entry(name.to_string()).or_insert(keys);
    }

    types
        .into_iter()
        .map(|(name, ty)| {
            let l = labels.get(&name).cloned().unwrap_or_default();
            (
                name,
                MetricSpec {
                    metric_type: ty,
                    labels: l,
                },
            )
        })
        .collect()
}

#[test]
fn metrics_surface_matches_committed_contract() {
    let exposition = lean_ctx::core::telemetry::global_metrics().to_prometheus();
    let current = parse_exposition(&exposition);
    assert!(
        !current.is_empty(),
        "exposition parsed to zero metrics — parser or exporter broken"
    );

    let path = contract_path();

    if std::env::var("LEANCTX_UPDATE_METRICS_CONTRACT").as_deref() == Ok("1") {
        let json = serde_json::to_string_pretty(&current).expect("serialize contract");
        std::fs::create_dir_all(path.parent().unwrap()).expect("mkdir reference dir");
        std::fs::write(&path, json + "\n").expect("write contract snapshot");
        eprintln!("metrics contract snapshot updated: {}", path.display());
        return;
    }

    let committed_raw = std::fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!(
            "missing {} ({e}) — generate it once with \
             LEANCTX_UPDATE_METRICS_CONTRACT=1 cargo test --test metrics_contract",
            path.display()
        )
    });
    let committed: BTreeMap<String, MetricSpec> =
        serde_json::from_str(&committed_raw).expect("contract JSON parses");

    let mut problems: Vec<String> = Vec::new();
    for (name, spec) in &committed {
        match current.get(name) {
            None => problems.push(format!("REMOVED metric `{name}` (breaking)")),
            Some(cur) if cur != spec => problems.push(format!(
                "CHANGED `{name}`: committed {spec:?} vs current {cur:?} (breaking)"
            )),
            _ => {}
        }
    }
    for name in current.keys() {
        if !committed.contains_key(name) {
            problems.push(format!(
                "NEW metric `{name}` — additive, but must be committed to the contract"
            ));
        }
    }

    assert!(
        problems.is_empty(),
        "metrics contract drift — customer dashboards depend on these names:\n  {}\n\
         If intentional: LEANCTX_UPDATE_METRICS_CONTRACT=1 cargo test --test metrics_contract \
         and justify in the MR.",
        problems.join("\n  ")
    );
}
