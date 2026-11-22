// SPDX-License-Identifier: Apache-2.0
//! The engine's copy of the shared data model (`tests/fixtures/model-v1`)
//! is a byte copy of the Cloud contract tree (leanctx-cloud
//! `contracts/model-v1`), made by `scripts/sync-model-v1.sh`.
//!
//! - The copy verifies against its MANIFEST.sha256.
//! - Every `p5` producer the model names exists here: the cited file
//!   contains the type and every cited field as a whole identifier (the
//!   cloud repository checks its own `p37` producers the same way).
//! - A task's process fields are exactly process v1
//!   `task-process.schema.json`: the model's task fixtures read into
//!   [`TaskV1`] and write back unchanged, and [`TaskProcessV1`] requires
//!   exactly the members the contract requires.

use std::path::{Path, PathBuf};

use lean_ctx_protocol::cloud::gate::TaskProcessV1;
use lean_ctx_protocol::cloud::tasks::TaskV1;
use serde_json::Value;

use super::cloud_v2_manifest::verify_copy;

fn model() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/model-v1")
}

/// The repository root the producers' paths are relative to.
fn repo() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..")
}

fn read(path: &Path) -> Value {
    let text = std::fs::read_to_string(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

fn json_files(dir: &Path) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = std::fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("{}: {e}", dir.display()))
        .map(|entry| entry.expect("dir entry").path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .collect();
    out.sort();
    out
}

#[test]
fn the_model_v1_copy_verifies_against_its_manifest() {
    verify_copy(&model(), "scripts/sync-model-v1.sh");
}

/// `word` occurs in `source` not as part of a longer identifier.
fn has_word(source: &str, word: &str) -> bool {
    let ident = |c: char| c.is_ascii_alphanumeric() || c == '_';
    !word.is_empty()
        && source.match_indices(word).any(|(at, _)| {
            let before = source[..at].chars().next_back();
            let after = source[at + word.len()..].chars().next();
            !before.is_some_and(ident) && !after.is_some_and(ident)
        })
}

fn check_producer(where_: &str, producer: &str, problems: &mut Vec<String>) {
    let mut parts = producer.splitn(3, ' ');
    let (Some("p5"), Some(path), Some(symbol)) = (parts.next(), parts.next(), parts.next()) else {
        return;
    };
    let Ok(source) = std::fs::read_to_string(repo().join(path)) else {
        problems.push(format!("{where_}: no file {path}"));
        return;
    };
    // A quoted JSON key or constant.
    if let Some(key) = symbol.strip_prefix('"') {
        let key = key.trim_end_matches('"');
        if !source.contains(key) {
            problems.push(format!("{where_}: {symbol} not in {path}"));
        }
        return;
    }
    for (i, name) in symbol.split('.').enumerate() {
        let found = if i == 0 {
            source.contains(name)
        } else {
            has_word(&source, name)
        };
        if !found {
            let what = if i == 0 { "type" } else { "field" };
            problems.push(format!("{where_}: {what} {name} not in {path}"));
        }
    }
}

/// Every `x-producers` list anywhere in the schema, definitions included.
fn walk(where_: &str, node: &Value, count: &mut usize, problems: &mut Vec<String>) {
    match node {
        Value::Object(map) => {
            for producer in map
                .get("x-producers")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
            {
                if producer.starts_with("p5 ") {
                    *count += 1;
                }
                check_producer(where_, producer, problems);
            }
            for (key, value) in map {
                walk(&format!("{where_}/{key}"), value, count, problems);
            }
        }
        Value::Array(items) => {
            for (i, value) in items.iter().enumerate() {
                walk(&format!("{where_}[{i}]"), value, count, problems);
            }
        }
        _ => {}
    }
}

/// No invention: every field the model says the core produces is produced
/// here, under the cited name.
#[test]
fn every_p5_producer_exists() {
    let mut problems = Vec::new();
    let mut count = 0;
    for path in json_files(&model().join("schemas")) {
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        walk(&name, &read(&path), &mut count, &mut problems);
    }
    assert!(count > 100, "only {count} p5 producers found");
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}

/// The producer check refuses a cited field the file does not have, and a
/// name that only occurs inside a longer identifier.
#[test]
fn the_producer_check_requires_the_field() {
    let cited = |symbol: &str| {
        let mut problems = Vec::new();
        let producer = format!("p5 rust/crates/lean-ctx-protocol/src/cloud/gate.rs {symbol}");
        check_producer("probe", &producer, &mut problems);
        problems
    };
    assert!(cited("TaskProcessGateV1.locked_paths").is_empty());
    assert_eq!(cited("TaskProcessGateV1.no_such_member").len(), 1);
    assert_eq!(cited("TaskProcessGateV1.locked_path").len(), 1);
    assert_eq!(cited("NoSuchType.locked_paths").len(), 1);
}

/// Every model task fixture reads into the core's task view and writes
/// back unchanged; the process task among them carries both parts.
#[test]
fn the_model_task_fixtures_round_trip_through_task_v1() {
    let mut seen_process = false;
    for path in json_files(&model().join("fixtures/valid")) {
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        if !name.starts_with("task.task.") {
            continue;
        }
        let fixture = read(&path);
        let task: TaskV1 =
            serde_json::from_value(fixture.clone()).unwrap_or_else(|e| panic!("{name}: {e}"));
        task.validate().unwrap_or_else(|e| panic!("{name}: {e}"));
        let back = serde_json::to_value(&task).unwrap();
        for member in ["process", "process_gate"] {
            assert_eq!(back.get(member), fixture.get(member), "{name}: {member}");
        }
        seen_process |= task.process.is_some() && task.process_gate.is_some();
    }
    assert!(seen_process, "no model task fixture carries a process");
}

/// The members `task-process.schema.json` requires, as the generated type
/// shows them: a top-level member without `?`.
fn contract_members() -> Vec<(String, bool)> {
    let path = model().join("types/process-v1/task-process.d.ts");
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let body = text
        .split_once("export type TaskProcess = {\n")
        .expect("the TaskProcess type")
        .1;
    body.lines()
        .take_while(|line| *line != "};")
        .filter_map(|line| line.strip_prefix("  "))
        .filter(|line| line.starts_with(|c: char| c.is_ascii_lowercase()))
        .filter_map(|line| line.split_once(':'))
        .map(|(name, _)| match name.strip_suffix('?') {
            Some(name) => (name.to_owned(), false),
            None => (name.to_owned(), true),
        })
        .collect()
}

/// The core reads exactly the contract: without a required member the
/// process fields are refused, without an optional one they still read.
#[test]
fn task_process_v1_requires_what_the_contract_requires() {
    let fixture = read(&model().join("fixtures/valid/task.task.process.json"));
    let process = fixture["process"]
        .as_object()
        .expect("process fields")
        .clone();
    let members = contract_members();
    assert!(members.len() > 15, "{members:?}");
    for (name, required) in &members {
        if !process.contains_key(name) {
            assert!(!required, "the fixture lacks the required member {name}");
            continue;
        }
        let mut without = process.clone();
        without.remove(name);
        let read = serde_json::from_value::<TaskProcessV1>(Value::Object(without));
        assert_eq!(read.is_err(), *required, "{name}: {read:?}");
    }
    for name in process.keys() {
        assert!(
            members.iter().any(|(member, _)| member == name),
            "{name} is not a task-process member"
        );
    }
}
