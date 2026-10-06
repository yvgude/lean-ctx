// SPDX-License-Identifier: Apache-2.0
//! Tests for `call_graph` (split out for the LOC gate).

use super::*;

#[test]
fn callers_of_empty_graph() {
    let graph = CallGraph::new("/tmp");
    assert!(graph.callers_of("foo").is_empty());
}

#[test]
fn callers_of_finds_edges() {
    let mut graph = CallGraph::new("/tmp");
    graph.edges.push(CallEdge {
        caller_file: "a.rs".to_string(),
        caller_symbol: "bar".to_string(),
        caller_line: 10,
        callee_name: "foo".to_string(),
        ..Default::default()
    });
    graph.edges.push(CallEdge {
        caller_file: "b.rs".to_string(),
        caller_symbol: "baz".to_string(),
        caller_line: 20,
        callee_name: "foo".to_string(),
        ..Default::default()
    });
    graph.edges.push(CallEdge {
        caller_file: "c.rs".to_string(),
        caller_symbol: "qux".to_string(),
        caller_line: 30,
        callee_name: "other".to_string(),
        ..Default::default()
    });
    let callers = graph.callers_of("foo");
    assert_eq!(callers.len(), 2);
}

#[test]
fn callees_of_finds_edges() {
    let mut graph = CallGraph::new("/tmp");
    graph.edges.push(CallEdge {
        caller_file: "a.rs".to_string(),
        caller_symbol: "main".to_string(),
        caller_line: 5,
        callee_name: "init".to_string(),
        ..Default::default()
    });
    graph.edges.push(CallEdge {
        caller_file: "a.rs".to_string(),
        caller_symbol: "main".to_string(),
        caller_line: 6,
        callee_name: "run".to_string(),
        ..Default::default()
    });
    graph.edges.push(CallEdge {
        caller_file: "a.rs".to_string(),
        caller_symbol: "other".to_string(),
        caller_line: 15,
        callee_name: "init".to_string(),
        ..Default::default()
    });
    let callees = graph.callees_of("main");
    assert_eq!(callees.len(), 2);
}

fn sym(name: &str, file: &str) -> SymbolSpan {
    SymbolSpan {
        file: file.to_string(),
        name: name.to_string(),
        start_line: 1,
        end_line: 2,
        ..Default::default()
    }
}

#[test]
fn resolve_callee_file_scopes_same_named_methods() {
    // `Run` is defined in two files (two classes). Each caller must resolve
    // to its *own* file, never to both.
    let inputs = CallGraphInputs {
        project_root: "/p".to_string(),
        symbols: vec![sym("Run", "a.rs"), sym("Run", "b.rs")],
        ..Default::default()
    };
    let imports: HashMap<String, std::collections::HashSet<String>> = HashMap::new();

    assert_eq!(
        resolve_callee_file("Run", "a.rs", &inputs, &imports).as_deref(),
        Some("a.rs")
    );
    assert_eq!(
        resolve_callee_file("Run", "b.rs", &inputs, &imports).as_deref(),
        Some("b.rs")
    );
    // A caller that neither defines nor imports `Run` stays ambiguous.
    assert_eq!(resolve_callee_file("Run", "c.rs", &inputs, &imports), None);
}

#[test]
fn resolve_callee_file_prefers_imported_definition() {
    let inputs = CallGraphInputs {
        project_root: "/p".to_string(),
        symbols: vec![sym("Run", "lib.rs"), sym("Run", "other.rs")],
        ..Default::default()
    };
    let mut imports: HashMap<String, std::collections::HashSet<String>> = HashMap::new();
    imports.insert(
        "main.rs".to_string(),
        std::collections::HashSet::from(["lib.rs".to_string()]),
    );
    // `main.rs` imports only `lib.rs`, so `Run` resolves there despite the
    // global ambiguity with `other.rs`.
    assert_eq!(
        resolve_callee_file("Run", "main.rs", &inputs, &imports).as_deref(),
        Some("lib.rs")
    );
}

#[test]
fn resolve_callee_files_drops_cross_scope_ambiguity() {
    let inputs = CallGraphInputs {
        project_root: "/p".to_string(),
        symbols: vec![
            sym("Run", "a.rs"),
            sym("Run", "b.rs"),
            sym("Unique", "u.rs"),
        ],
        ..Default::default()
    };
    let edges = vec![
        CallEdge {
            caller_file: "a.rs".into(),
            caller_symbol: "fa".into(),
            caller_line: 1,
            callee_name: "Run".into(),
            ..Default::default()
        },
        CallEdge {
            caller_file: "b.rs".into(),
            caller_symbol: "fb".into(),
            caller_line: 1,
            callee_name: "Run".into(),
            ..Default::default()
        },
        CallEdge {
            caller_file: "x.rs".into(),
            caller_symbol: "fx".into(),
            caller_line: 1,
            callee_name: "Unique".into(),
            ..Default::default()
        },
    ];
    let map = resolve_callee_files(&inputs, &edges);
    // `Run` resolves to a.rs from a and b.rs from b → two files → omitted.
    assert!(!map.contains_key("Run"));
    // `Unique` is globally unique → resolved.
    assert_eq!(map.get("Unique").map(String::as_str), Some("u.rs"));
}

/// Regression: a call bound to a same-named definition in a language the
/// caller cannot call — a Rust `measure()` landing on a shell script's
/// `measure`. Only same-family definitions are candidates; JS calls TS.
#[test]
fn calls_never_bind_across_language_families() {
    let inputs = CallGraphInputs {
        project_root: "/p".to_string(),
        symbols: vec![
            sym("measure", "scripts/bench.sh"),
            sym("render", "web/view.ts"),
        ],
        ..Default::default()
    };
    let call = |file: &str, callee: &str| CallEdge {
        caller_file: file.into(),
        caller_symbol: "f".into(),
        caller_line: 1,
        callee_name: callee.into(),
        ..Default::default()
    };
    let targets = resolve_edge_callee_targets(
        &inputs,
        &[call("src/main.rs", "measure"), call("web/app.js", "render")],
    );
    assert_eq!(
        targets[0],
        StructuralTarget::Unknown,
        "no Rust → shell edge"
    );
    assert_eq!(targets[1].file(), Some("web/view.ts"), "JS → TS binds");
}

/// Regression: call lines were shifted by one (`CallSite.line` is already
/// 1-based), so a call in a one-line function was attributed to the next
/// symbol; and the edge carried no callee position for semantic lookup.
#[cfg(feature = "tree-sitter")]
#[test]
fn build_attributes_call_to_its_own_line_and_locates_callee() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("a.rs"),
        "fn caller() { repo.save(); }\nfn other() {}\n",
    )
    .unwrap();
    let span = |name: &str, line: usize| SymbolSpan {
        file: "a.rs".into(),
        name: name.into(),
        start_line: line,
        end_line: line,
        ..Default::default()
    };
    let inputs = CallGraphInputs {
        project_root: dir.path().to_string_lossy().to_string(),
        file_paths: vec!["a.rs".into()],
        symbols: vec![span("caller", 1), span("other", 2)],
        ..Default::default()
    };

    let graph = CallGraph::build(&inputs);
    let edge = graph
        .edges
        .iter()
        .find(|e| e.callee_name == "save")
        .unwrap();
    assert_eq!(edge.caller_symbol, "caller");
    assert_eq!(edge.caller_line, 1);
    assert_eq!(edge.callee_pos, Some((1, 19)), "must point at `save`");
    assert_eq!(edge.receiver.as_deref(), Some("repo"));
    assert!(edge.is_method);
}

/// A path call (`db::save`) is never bound structurally, but must reach
/// semantic escalation when its last segment is a project symbol.
#[test]
fn path_callees_are_semantic_candidates_never_structural_guesses() {
    let inputs = CallGraphInputs {
        symbols: vec![sym("save", "db.rs")],
        ..Default::default()
    };
    let call = |callee: &str| CallEdge {
        caller_file: "main.rs".into(),
        callee_name: callee.into(),
        ..Default::default()
    };
    let targets = resolve_edge_callee_targets(
        &inputs,
        &[call("crate::db::save"), call("std::fs::read"), call("save")],
    );
    assert_eq!(targets[0], StructuralTarget::Ambiguous);
    assert_eq!(targets[1], StructuralTarget::Unknown);
    assert_eq!(
        targets[2],
        StructuralTarget::Resolved {
            file: "db.rs".into(),
            via: ScopeMatch::UniqueInProject
        }
    );
}

#[test]
fn find_enclosing_picks_narrowest() {
    let outer = SymbolSpan {
        file: "a.rs".to_string(),
        name: "Outer".to_string(),
        start_line: 1,
        end_line: 50,
        ..Default::default()
    };
    let inner = SymbolSpan {
        file: "a.rs".to_string(),
        name: "inner_fn".to_string(),
        start_line: 10,
        end_line: 20,
        ..Default::default()
    };
    let syms = vec![outer, inner];
    let result = find_enclosing_symbol_owned(Some(&syms), 15);
    assert_eq!(result, "inner_fn");
}

#[test]
fn find_enclosing_returns_module_when_no_match() {
    let sym = SymbolSpan {
        file: "a.rs".to_string(),
        name: "foo".to_string(),
        start_line: 10,
        end_line: 20,
        ..Default::default()
    };
    let syms = vec![sym];
    let result = find_enclosing_symbol_owned(Some(&syms), 5);
    assert_eq!(result, "<module>");
}

#[test]
fn resolve_path_trims_rooted_relative_prefix() {
    let resolved = resolve_path(r"\src\main\kotlin\Example.kt", r"C:\repo");
    assert_eq!(
        resolved,
        Path::new(r"C:\repo")
            .join(r"src\main\kotlin\Example.kt")
            .to_string_lossy()
            .to_string()
    );
}

fn build_chain_graph() -> CallGraph {
    // A -> B -> C -> D
    let mut graph = CallGraph::new("/tmp");
    graph.edges.push(CallEdge {
        caller_file: "a.rs".into(),
        caller_symbol: "fn_a".into(),
        caller_line: 1,
        callee_name: "fn_b".into(),
        ..Default::default()
    });
    graph.edges.push(CallEdge {
        caller_file: "b.rs".into(),
        caller_symbol: "fn_b".into(),
        caller_line: 10,
        callee_name: "fn_c".into(),
        ..Default::default()
    });
    graph.edges.push(CallEdge {
        caller_file: "c.rs".into(),
        caller_symbol: "fn_c".into(),
        caller_line: 20,
        callee_name: "fn_d".into(),
        ..Default::default()
    });
    graph
}

#[test]
fn bfs_callees_depth_1_returns_direct() {
    let graph = build_chain_graph();
    let nodes = graph.bfs_callees("fn_a", 1);
    assert_eq!(nodes.len(), 1);
    assert_eq!(nodes[0].symbol, "fn_b");
    assert_eq!(nodes[0].depth, 1);
}

#[test]
fn bfs_callees_depth_3_returns_chain() {
    let graph = build_chain_graph();
    let nodes = graph.bfs_callees("fn_a", 3);
    assert_eq!(nodes.len(), 3);
    let syms: Vec<&str> = nodes.iter().map(|n| n.symbol.as_str()).collect();
    assert!(syms.contains(&"fn_b"));
    assert!(syms.contains(&"fn_c"));
    assert!(syms.contains(&"fn_d"));
}

#[test]
fn bfs_callers_depth_2_returns_transitive() {
    let graph = build_chain_graph();
    let nodes = graph.bfs_callers("fn_c", 2);
    assert_eq!(nodes.len(), 2);
    let syms: Vec<&str> = nodes.iter().map(|n| n.symbol.as_str()).collect();
    assert!(syms.contains(&"fn_b"));
    assert!(syms.contains(&"fn_a"));
}

#[test]
fn find_call_path_direct() {
    let graph = build_chain_graph();
    let path = graph.find_call_path("fn_a", "fn_b");
    assert!(path.is_some());
    let hops = path.unwrap();
    assert_eq!(hops.len(), 2);
    assert_eq!(hops[0].symbol, "fn_a");
    assert_eq!(hops[1].symbol, "fn_b");
}

#[test]
fn find_call_path_multi_hop() {
    let graph = build_chain_graph();
    let path = graph.find_call_path("fn_a", "fn_d");
    assert!(path.is_some());
    let hops = path.unwrap();
    assert_eq!(hops.len(), 4);
    assert_eq!(hops[0].symbol, "fn_a");
    assert_eq!(hops[3].symbol, "fn_d");
}

#[test]
fn find_call_path_no_connection() {
    let graph = build_chain_graph();
    let path = graph.find_call_path("fn_d", "fn_a");
    assert!(path.is_none());
}

#[test]
fn find_call_path_same_symbol() {
    let graph = build_chain_graph();
    let path = graph.find_call_path("fn_a", "fn_a");
    assert!(path.is_some());
    assert_eq!(path.unwrap().len(), 1);
}

#[test]
fn transitive_caller_count_returns_unique() {
    let mut graph = CallGraph::new("/tmp");
    // x -> target, y -> target, z -> x (so z is transitive caller of target)
    graph.edges.push(CallEdge {
        caller_file: "x.rs".into(),
        caller_symbol: "x".into(),
        caller_line: 1,
        callee_name: "target".into(),
        ..Default::default()
    });
    graph.edges.push(CallEdge {
        caller_file: "y.rs".into(),
        caller_symbol: "y".into(),
        caller_line: 2,
        callee_name: "target".into(),
        ..Default::default()
    });
    graph.edges.push(CallEdge {
        caller_file: "z.rs".into(),
        caller_symbol: "z".into(),
        caller_line: 3,
        callee_name: "x".into(),
        ..Default::default()
    });
    assert_eq!(graph.transitive_caller_count("target", 5), 3);
}

#[test]
fn risk_level_classification() {
    assert_eq!(RiskLevel::from_caller_count(0), RiskLevel::Low);
    assert_eq!(RiskLevel::from_caller_count(1), RiskLevel::Low);
    assert_eq!(RiskLevel::from_caller_count(3), RiskLevel::Medium);
    assert_eq!(RiskLevel::from_caller_count(7), RiskLevel::High);
    assert_eq!(RiskLevel::from_caller_count(15), RiskLevel::Critical);
}

#[test]
fn bfs_handles_cycle_without_infinite_loop() {
    let mut graph = CallGraph::new("/tmp");
    graph.edges.push(CallEdge {
        caller_file: "a.rs".into(),
        caller_symbol: "a".into(),
        caller_line: 1,
        callee_name: "b".into(),
        ..Default::default()
    });
    graph.edges.push(CallEdge {
        caller_file: "b.rs".into(),
        caller_symbol: "b".into(),
        caller_line: 2,
        callee_name: "a".into(),
        ..Default::default()
    });
    let nodes = graph.bfs_callees("a", 5);
    // Should visit b once (depth 1), then a is already visited
    assert_eq!(nodes.len(), 1);
    assert_eq!(nodes[0].symbol, "b");
}
