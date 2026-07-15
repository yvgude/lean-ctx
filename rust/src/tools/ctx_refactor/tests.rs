use serde_json::json;

/// §4.5: inner handle MUST use the (already jailed) abs_path it is given,
/// never re-derive a path from raw args. A raw "../escape.rs" must never
/// reach the filesystem layer; only the provided abs_path does.
#[test]
fn inner_handle_uses_provided_abs_path_not_raw_args() {
    let args = json!({"action": "references", "path": "../escape.rs", "line": 1, "column": 0});
    let out = super::handle(&args, "/proj", "/proj/jailed.rs");
    // open_file fails reading the (nonexistent) jailed file → error names abs_path.
    assert!(out.contains("/proj/jailed.rs"), "abs_path not used: {out}");
    assert!(
        !out.contains("../escape.rs"),
        "raw path leaked to fs layer: {out}"
    );
}

/// `declaration` is a known action: the unknown-action arm must not fire for it,
/// and its help text now advertises `declaration`.
///
/// NOTE (adaptation): the real `handle` opens the file *before* the action
/// match, so reaching the unknown-action help arm requires a backend. We seed
/// a no-op stub backend for `rust` and point at a real temp `.rs` file so
/// dispatch deterministically reaches the help text, offline, without
/// starting rust-analyzer.
#[test]
fn unknown_action_help_lists_declaration() {
    struct StubBackend;
    impl crate::lsp::backend::LspBackend for StubBackend {
        fn open_file(
            &mut self,
            _uri: &lsp_types::Uri,
            _language_id: &str,
            _text: &str,
        ) -> Result<(), String> {
            Ok(())
        }
        fn references(
            &mut self,
            _uri: &lsp_types::Uri,
            _position: lsp_types::Position,
            _scope: &str,
        ) -> Result<Vec<lsp_types::Location>, String> {
            Ok(vec![])
        }
        fn definition(
            &mut self,
            _uri: &lsp_types::Uri,
            _position: lsp_types::Position,
        ) -> Result<lsp_types::GotoDefinitionResponse, String> {
            Ok(lsp_types::GotoDefinitionResponse::Array(vec![]))
        }
        fn implementations(
            &mut self,
            _uri: &lsp_types::Uri,
            _position: lsp_types::Position,
            _scope: &str,
        ) -> Result<Vec<lsp_types::Location>, String> {
            Ok(vec![])
        }
        fn rename(
            &mut self,
            _uri: &lsp_types::Uri,
            _position: lsp_types::Position,
            _new_name: &str,
        ) -> Result<Option<lsp_types::WorkspaceEdit>, String> {
            Ok(None)
        }
    }

    let dir = std::env::temp_dir().join(format!("leanctx_r1_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("x.rs");
    std::fs::write(&file, "fn x() {}\n").unwrap();
    let root = dir.to_string_lossy().to_string();
    let abs = file.to_string_lossy().to_string();

    let _stub = crate::lsp::router::stub_test_lock();
    crate::lsp::router::seed_stub_backend("rust", Box::new(StubBackend));

    let args = json!({"action": "definitely_bogus", "path": "x.rs", "line": 1});
    let out = super::handle(&args, &root, &abs);
    assert!(
        out.contains("declaration"),
        "help text missing declaration: {out}"
    );
    assert!(
        out.contains("inspections"),
        "help text missing inspections: {out}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn type_hierarchy_formats_indented_tree() {
    use crate::lsp::backend::{
        HierarchyDirection, LspBackend, SymbolOverviewItem, TypeHierarchyNode,
    };

    struct HierBackend;
    impl LspBackend for HierBackend {
        fn open_file(&mut self, _u: &lsp_types::Uri, _l: &str, _t: &str) -> Result<(), String> {
            Ok(())
        }
        fn references(
            &mut self,
            _u: &lsp_types::Uri,
            _p: lsp_types::Position,
            _s: &str,
        ) -> Result<Vec<lsp_types::Location>, String> {
            Ok(vec![])
        }
        fn definition(
            &mut self,
            _u: &lsp_types::Uri,
            _p: lsp_types::Position,
        ) -> Result<lsp_types::GotoDefinitionResponse, String> {
            Ok(lsp_types::GotoDefinitionResponse::Array(vec![]))
        }
        fn implementations(
            &mut self,
            _u: &lsp_types::Uri,
            _p: lsp_types::Position,
            _s: &str,
        ) -> Result<Vec<lsp_types::Location>, String> {
            Ok(vec![])
        }
        fn rename(
            &mut self,
            _u: &lsp_types::Uri,
            _p: lsp_types::Position,
            _n: &str,
        ) -> Result<Option<lsp_types::WorkspaceEdit>, String> {
            Ok(None)
        }
        fn type_hierarchy(
            &mut self,
            _u: &lsp_types::Uri,
            _p: lsp_types::Position,
            dir: HierarchyDirection,
        ) -> Result<TypeHierarchyNode, String> {
            assert_eq!(dir, HierarchyDirection::Subtypes);
            Ok(TypeHierarchyNode {
                name: "Animal".into(),
                path: "A.kt".into(),
                line: 1,
                children: vec![TypeHierarchyNode {
                    name: "Dog".into(),
                    path: "A.kt".into(),
                    line: 2,
                    children: vec![],
                }],
            })
        }
        fn symbols_overview(
            &mut self,
            _u: &lsp_types::Uri,
        ) -> Result<Vec<SymbolOverviewItem>, String> {
            Ok(vec![SymbolOverviewItem {
                name: "Animal".into(),
                kind: "interface".into(),
                line: 1,
            }])
        }
    }

    let tree = HierBackend
        .type_hierarchy(
            &crate::lsp::client::file_path_to_uri("/p/A.kt").unwrap(),
            lsp_types::Position::new(0, 0),
            HierarchyDirection::Subtypes,
        )
        .unwrap();
    let out = super::format_type_hierarchy(&tree);
    assert!(out.contains("Animal (A.kt:1)"), "{out}");
    assert!(out.contains("  Dog (A.kt:2)"), "{out}"); // child indented

    let items = HierBackend
        .symbols_overview(&crate::lsp::client::file_path_to_uri("/p/A.kt").unwrap())
        .unwrap();
    let out2 = super::format_symbols_overview(&items);
    assert!(out2.contains("interface Animal (line 1)"), "{out2}");
}

#[test]
fn parse_direction_defaults_to_supertypes() {
    use crate::lsp::backend::HierarchyDirection;
    assert_eq!(
        super::parse_direction(&json!({})),
        HierarchyDirection::Supertypes
    );
    assert_eq!(
        super::parse_direction(&json!({"direction": "subtypes"})),
        HierarchyDirection::Subtypes
    );
    assert_eq!(
        super::parse_direction(&json!({"direction": "supertypes"})),
        HierarchyDirection::Supertypes
    );
}

#[test]
fn resolve_name_path_unique_class() {
    let _lock = crate::core::data_dir::test_env_lock();
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("data");
    std::fs::create_dir_all(&data).unwrap();
    crate::test_env::set_var("LEAN_CTX_DATA_DIR", data.to_string_lossy().to_string());

    let proj = tmp.path().join("proj");
    std::fs::create_dir_all(proj.join("src")).unwrap();
    std::fs::write(
        proj.join("Cargo.toml"),
        "[package]\nname=\"x\"\nversion=\"0.0.0\"\n",
    )
    .unwrap();
    std::fs::write(
        proj.join("src/lib.rs"),
        "pub struct UniqueZqWidget { pub a: u8 }\n",
    )
    .unwrap();
    let root = proj.to_string_lossy().to_string();

    let r = super::resolve_name_path("UniqueZqWidget", &root).expect("unique resolution");
    assert!(r.rel_path.ends_with("lib.rs"), "got: {}", r.rel_path);
    assert!(r.end_line >= r.start_line && r.start_line > 0);

    crate::test_env::remove_var("LEAN_CTX_DATA_DIR");
}

#[test]
fn resolve_name_path_unknown_is_no_symbol() {
    let _lock = crate::core::data_dir::test_env_lock();
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("data");
    std::fs::create_dir_all(&data).unwrap();
    crate::test_env::set_var("LEAN_CTX_DATA_DIR", data.to_string_lossy().to_string());

    let proj = tmp.path().join("proj");
    std::fs::create_dir_all(proj.join("src")).unwrap();
    std::fs::write(
        proj.join("Cargo.toml"),
        "[package]\nname=\"x\"\nversion=\"0.0.0\"\n",
    )
    .unwrap();
    std::fs::write(
        proj.join("src/lib.rs"),
        "pub struct UniqueZqWidget { pub a: u8 }\n",
    )
    .unwrap();
    let root = proj.to_string_lossy().to_string();

    let err = super::resolve_name_path("ZzzNoSuchSymbol123", &root).unwrap_err();
    assert!(err.starts_with("NO_SYMBOL"), "got: {err}");

    crate::test_env::remove_var("LEAN_CTX_DATA_DIR");
}

#[test]
fn resolve_name_path_trait_impl_method() {
    let _lock = crate::core::data_dir::test_env_lock();
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("data");
    std::fs::create_dir_all(&data).unwrap();
    crate::test_env::set_var("LEAN_CTX_DATA_DIR", data.to_string_lossy().to_string());

    let proj = tmp.path().join("proj");
    std::fs::create_dir_all(proj.join("src")).unwrap();
    std::fs::write(
        proj.join("Cargo.toml"),
        "[package]\nname=\"x\"\nversion=\"0.0.0\"\n",
    )
    .unwrap();
    std::fs::write(
        proj.join("src/lib.rs"),
        "pub struct RenderBridge;\n\
             pub trait Exec { fn execute(&self); }\n\
             impl Exec for RenderBridge {\n\
             \x20   fn execute(&self) { let _ = 1; }\n\
             }\n",
    )
    .unwrap();
    let root = proj.to_string_lossy().to_string();

    let r = super::resolve_name_path("RenderBridge/execute", &root)
        .expect("trait-impl method should resolve");
    assert!(r.rel_path.ends_with("lib.rs"), "got: {}", r.rel_path);
    // Muss auf den Impl-Methoden-Body zeigen (Zeile >= 3), nicht auf das
    // struct (Z. 1) oder die Trait-Deklaration (Z. 2).
    assert!(
        r.start_line >= 3,
        "should point at impl method, got L{}",
        r.start_line
    );
    assert!(r.end_line >= r.start_line && r.start_line > 0);

    crate::test_env::remove_var("LEAN_CTX_DATA_DIR");
}

#[test]
fn container_matches_ancestor_cases() {
    use super::container_matches_ancestor as m;
    assert!(m("RenderBridge", "RenderBridge"));
    assert!(m("Exec for RenderBridge", "RenderBridge"));
    assert!(m("Exec for RenderBridge<Wasm>", "RenderBridge"));
    assert!(!m("OtherType", "RenderBridge"));
    assert!(!m("Exec for Other", "RenderBridge"));
}

#[test]
fn resolve_name_path_inherent_impl_method() {
    let _lock = crate::core::data_dir::test_env_lock();
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("data");
    std::fs::create_dir_all(&data).unwrap();
    crate::test_env::set_var("LEAN_CTX_DATA_DIR", data.to_string_lossy().to_string());

    let proj = tmp.path().join("proj");
    std::fs::create_dir_all(proj.join("src")).unwrap();
    std::fs::write(
        proj.join("Cargo.toml"),
        "[package]\nname=\"x\"\nversion=\"0.0.0\"\n",
    )
    .unwrap();
    std::fs::write(
        proj.join("src/lib.rs"),
        "pub struct RenderBridge;\n\
             impl RenderBridge {\n\
             \x20   pub fn run(&self) { let _ = 1; }\n\
             }\n",
    )
    .unwrap();
    let root = proj.to_string_lossy().to_string();

    let r = super::resolve_name_path("RenderBridge/run", &root)
        .expect("inherent-impl method should still resolve");
    assert!(r.rel_path.ends_with("lib.rs"), "got: {}", r.rel_path);
    assert!(r.start_line >= 2 && r.end_line >= r.start_line);

    crate::test_env::remove_var("LEAN_CTX_DATA_DIR");
}

#[test]
fn resolve_name_path_ambiguous_trait_impls() {
    let _lock = crate::core::data_dir::test_env_lock();
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("data");
    std::fs::create_dir_all(&data).unwrap();
    crate::test_env::set_var("LEAN_CTX_DATA_DIR", data.to_string_lossy().to_string());

    let proj = tmp.path().join("proj");
    std::fs::create_dir_all(proj.join("src")).unwrap();
    std::fs::write(
        proj.join("Cargo.toml"),
        "[package]\nname=\"x\"\nversion=\"0.0.0\"\n",
    )
    .unwrap();
    std::fs::write(
        proj.join("src/lib.rs"),
        "pub struct RenderBridge;\n\
             pub trait A { fn execute(&self); }\n\
             pub trait B { fn execute(&self); }\n\
             pub mod a;\n\
             pub mod b;\n",
    )
    .unwrap();
    // a.rs: impl A for RenderBridge — plain targets, multi-line body so fn is indexed
    std::fs::write(
        proj.join("src/a.rs"),
        "impl A for RenderBridge {\n\
             \x20   fn execute(&self) { let _ = 1; }\n\
             }\n",
    )
    .unwrap();
    // b.rs: impl B for RenderBridge — plain targets, multi-line body so fn is indexed
    std::fs::write(
        proj.join("src/b.rs"),
        "impl B for RenderBridge {\n\
             \x20   fn execute(&self) { let _ = 1; }\n\
             }\n",
    )
    .unwrap();
    let root = proj.to_string_lossy().to_string();

    // "RenderBridge/execute": two segments → container_matches_ancestor runs for each hit.
    // "A for RenderBridge" and "B for RenderBridge" both match ancestor "RenderBridge",
    // producing two distinct hits (src/a.rs and src/b.rs) → AMBIGUOUS_SYMBOL.
    let err = super::resolve_name_path("RenderBridge/execute", &root)
        .expect_err("two trait impls (cross-file) with same method must be ambiguous");
    assert!(err.starts_with("AMBIGUOUS_SYMBOL"), "got: {err}");

    crate::test_env::remove_var("LEAN_CTX_DATA_DIR");
}

#[test]
fn anchor_indent_reads_leading_whitespace() {
    let content = "class A {\n    fun b() {}\n}\n";
    assert_eq!(super::anchor_indent(content, 2), "    "); // line 2 (1-based) → 4 spaces
    assert_eq!(super::anchor_indent(content, 1), ""); // line 1 → none
}

#[test]
fn reindent_prefixes_first_line_only() {
    assert_eq!(
        super::reindent_first_line("fun x() {}", "    "),
        "    fun x() {}"
    );
    // Already-indented text is left untouched.
    assert_eq!(
        super::reindent_first_line("    fun x()", "    "),
        "    fun x()"
    );
}

#[test]
fn apply_symbol_edit_headless_replaces_range() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("Foo.txt"), "aaa\nBODY\nccc\n").unwrap();
    let abs = dir.path().join("Foo.txt").to_string_lossy().to_string();
    let edit = crate::lsp::backend::RangeEdit {
        abs_path: abs.clone(),
        rel_path: "Foo.txt".into(),
        range: crate::lsp::backend::TextRange0Based {
            start_line: 1,
            start_char: 0,
            end_line: 1,
            end_char: 4,
        },
        text: "NEW".into(),
        expected_hash: None,
    };
    // No port file under this temp dir → headless apply.
    let res = super::apply_symbol_edit("replace_symbol_body", dir.path().to_str().unwrap(), &edit)
        .unwrap();
    assert!(res.applied);
    assert_eq!(std::fs::read_to_string(&abs).unwrap(), "aaa\nNEW\nccc\n");
}

#[test]
fn handle_replace_symbol_body_via_position_fallback() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.rs"), "fn old() {\n  1\n}\n").unwrap();
    let args = serde_json::json!({
        "action": "replace_symbol_body",
        "path": "a.rs",
        "line": 1,
        "end_line": 3,
        "new_body": "fn new() {\n  2\n}"
    });
    let out = super::handle(&args, dir.path().to_str().unwrap(), "");
    assert!(out.contains("replace_symbol_body applied"), "got: {out}");
    let after = std::fs::read_to_string(dir.path().join("a.rs")).unwrap();
    assert!(after.contains("fn new()"), "file: {after}");
}

#[test]
fn handle_replace_symbol_body_conflict_on_stale_hash() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.rs"), "fn old() {\n  1\n}\n").unwrap();
    // Range = full file lines 1..=3; old content = the whole file text.
    let stale = serde_json::json!({
        "action": "replace_symbol_body",
        "path": "a.rs", "line": 1, "end_line": 3,
        "new_body": "fn new() {\n  2\n}",
        "expected_hash": "deadbeefnotahash"
    });
    let out = super::handle(&stale, dir.path().to_str().unwrap(), "");
    assert!(out.contains("CONFLICT"), "got: {out}");
    // file unchanged
    assert!(
        std::fs::read_to_string(dir.path().join("a.rs"))
            .unwrap()
            .contains("fn old()")
    );
}

#[test]
fn references_output_surfaces_truncation_note() {
    use lsp_types::Position;
    struct TruncBackend;
    impl crate::lsp::backend::LspBackend for TruncBackend {
        fn open_file(&mut self, _u: &lsp_types::Uri, _l: &str, _t: &str) -> Result<(), String> {
            Ok(())
        }
        fn references(
            &mut self,
            _u: &lsp_types::Uri,
            _p: lsp_types::Position,
            _s: &str,
        ) -> Result<Vec<lsp_types::Location>, String> {
            let uri = crate::lsp::client::file_path_to_uri("/proj/a.rs").unwrap();
            Ok(vec![lsp_types::Location {
                uri,
                range: lsp_types::Range::default(),
            }])
        }
        fn definition(
            &mut self,
            _u: &lsp_types::Uri,
            _p: lsp_types::Position,
        ) -> Result<lsp_types::GotoDefinitionResponse, String> {
            Ok(lsp_types::GotoDefinitionResponse::Array(vec![]))
        }
        fn implementations(
            &mut self,
            _u: &lsp_types::Uri,
            _p: lsp_types::Position,
            _s: &str,
        ) -> Result<Vec<lsp_types::Location>, String> {
            Ok(vec![])
        }
        fn rename(
            &mut self,
            _u: &lsp_types::Uri,
            _p: lsp_types::Position,
            _n: &str,
        ) -> Result<Option<lsp_types::WorkspaceEdit>, String> {
            Ok(None)
        }
        fn last_truncation(&self) -> Option<crate::lsp::backend::Truncation> {
            Some(crate::lsp::backend::Truncation {
                truncated: true,
                total: 742,
            })
        }
    }
    let _stub = crate::lsp::router::stub_test_lock();
    crate::lsp::router::seed_stub_backend("rust", Box::new(TruncBackend));
    let uri = crate::lsp::client::file_path_to_uri("/proj/a.rs").unwrap();
    let out = super::handle_references(
        "/proj/a.rs",
        "/proj",
        &uri,
        Position {
            line: 0,
            character: 0,
        },
        "project",
    );
    assert!(
        out.contains("truncated"),
        "expected truncation note, got: {out}"
    );
    assert!(out.contains("742"), "expected total in note, got: {out}");
}

#[test]
fn inspections_run_and_list_dispatch_and_truncation() {
    struct InspBackend;
    impl crate::lsp::backend::LspBackend for InspBackend {
        fn open_file(&mut self, _u: &lsp_types::Uri, _l: &str, _t: &str) -> Result<(), String> {
            Ok(())
        }
        fn references(
            &mut self,
            _u: &lsp_types::Uri,
            _p: lsp_types::Position,
            _s: &str,
        ) -> Result<Vec<lsp_types::Location>, String> {
            Ok(vec![])
        }
        fn definition(
            &mut self,
            _u: &lsp_types::Uri,
            _p: lsp_types::Position,
        ) -> Result<lsp_types::GotoDefinitionResponse, String> {
            Ok(lsp_types::GotoDefinitionResponse::Array(vec![]))
        }
        fn implementations(
            &mut self,
            _u: &lsp_types::Uri,
            _p: lsp_types::Position,
            _s: &str,
        ) -> Result<Vec<lsp_types::Location>, String> {
            Ok(vec![])
        }
        fn rename(
            &mut self,
            _u: &lsp_types::Uri,
            _p: lsp_types::Position,
            _n: &str,
        ) -> Result<Option<lsp_types::WorkspaceEdit>, String> {
            Ok(None)
        }
        fn inspections(
            &mut self,
            _u: &lsp_types::Uri,
        ) -> Result<Vec<crate::lsp::backend::InspectionDiag>, String> {
            Ok(vec![crate::lsp::backend::InspectionDiag {
                path: "A.kt".into(),
                line: 7,
                severity: "WARNING".into(),
                message: "unused".into(),
            }])
        }
        fn list_inspections(&mut self) -> Result<Vec<crate::lsp::backend::InspectionInfo>, String> {
            Ok(vec![crate::lsp::backend::InspectionInfo {
                id: "UnusedSymbol".into(),
                name: "Unused declaration".into(),
                severity: "WARNING".into(),
            }])
        }
        fn last_truncation(&self) -> Option<crate::lsp::backend::Truncation> {
            Some(crate::lsp::backend::Truncation {
                truncated: true,
                total: 99,
            })
        }
    }
    let _stub = crate::lsp::router::stub_test_lock();
    crate::lsp::router::seed_stub_backend("rust", Box::new(InspBackend));
    let uri = crate::lsp::client::file_path_to_uri("/proj/a.rs").unwrap();

    // run mode (default): formats path:line SEVERITY message + truncation note
    let run_out = super::handle_inspections(
        &json!({"action": "inspections"}),
        "/proj/a.rs",
        "/proj",
        &uri,
    );
    assert!(run_out.contains("A.kt:7"), "run diag missing: {run_out}");
    assert!(
        run_out.contains("WARNING"),
        "run severity missing: {run_out}"
    );
    assert!(run_out.contains("unused"), "run message missing: {run_out}");
    assert!(
        run_out.contains("truncated"),
        "run truncation missing: {run_out}"
    );
    assert!(run_out.contains("99"), "run total missing: {run_out}");

    // list mode: formats id name severity
    let list_out = super::handle_inspections(
        &json!({"action": "inspections", "mode": "list"}),
        "/proj/a.rs",
        "/proj",
        &uri,
    );
    assert!(
        list_out.contains("UnusedSymbol"),
        "list id missing: {list_out}"
    );
    assert!(
        list_out.contains("Unused declaration"),
        "list name missing: {list_out}"
    );

    // unknown mode → defined ERROR
    let bad_out = super::handle_inspections(
        &json!({"action": "inspections", "mode": "bogus"}),
        "/proj/a.rs",
        "/proj",
        &uri,
    );
    assert!(
        bad_out.contains("ERROR"),
        "unknown mode not rejected: {bad_out}"
    );
}

#[test]
fn usage_range_text_reads_jailed_slice() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.rs"), "let foo = 1;\nfoo + foo;\n").unwrap();
    let root = dir.path().to_str().unwrap();
    let u = crate::lsp::backend::UsageSite {
        path: "a.rs".into(),
        range: crate::lsp::backend::TextRange0Based {
            start_line: 0,
            start_char: 4,
            end_line: 0,
            end_char: 7,
        },
        context: None,
    };
    assert_eq!(super::usage_range_text(root, &u).unwrap(), "foo");
}

// Jail rejection only happens when the jail is compiled in. `--all-features`
// pulls in `no-jail` (jail disabled), so skip there like the move/resolve jail
// assertions below.
#[cfg(not(feature = "no-jail"))]
#[test]
fn usage_range_text_rejects_jail_escape() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_str().unwrap();
    let u = crate::lsp::backend::UsageSite {
        path: "../../etc/passwd".into(),
        range: crate::lsp::backend::TextRange0Based {
            start_line: 0,
            start_char: 0,
            end_line: 0,
            end_char: 1,
        },
        context: None,
    };
    assert!(super::usage_range_text(root, &u).is_err());
}

#[test]
fn plan_hash_is_deterministic_and_order_independent() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.rs"), "let foo = 1;\nfoo + foo;\n").unwrap();
    let root = dir.path().to_str().unwrap();
    let u1 = crate::lsp::backend::UsageSite {
        path: "a.rs".into(),
        range: crate::lsp::backend::TextRange0Based {
            start_line: 0,
            start_char: 4,
            end_line: 0,
            end_char: 7,
        },
        context: Some("ignored-in-hash".into()),
    };
    let u2 = crate::lsp::backend::UsageSite {
        path: "a.rs".into(),
        range: crate::lsp::backend::TextRange0Based {
            start_line: 1,
            start_char: 0,
            end_line: 1,
            end_char: 3,
        },
        context: None,
    };
    let h1 = super::plan_hash(root, &[u1.clone(), u2.clone()]).unwrap();
    let h2 = super::plan_hash(root, std::slice::from_ref(&u2)).unwrap(); // subset → differs
    let h3 = super::plan_hash(root, &[u2, u1]).unwrap(); // reversed → SAME (sorted canonical)
    assert_eq!(h1.len(), 64);
    assert_eq!(h1, h3, "hash must be order-independent");
    assert_ne!(h1, h2, "different usage set must differ");
}

/// #803: when `name_path` resolves to a file that differs from the caller's
/// explicit `path`, the edit must be rejected with WORKTREE_MISMATCH instead
/// of silently writing to the wrong checkout. Simulates the real scenario
/// where a git worktree lives INSIDE the project root (e.g.
/// `/repo/.claude/worktrees/wt/`) so the path jail does not block it.
#[test]
fn handle_replace_symbol_worktree_mismatch_blocks_write() {
    let _lock = crate::core::data_dir::test_env_lock();
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("data");
    std::fs::create_dir_all(&data).unwrap();
    crate::test_env::set_var("LEAN_CTX_DATA_DIR", data.to_string_lossy().to_string());

    // Main checkout root with a symbol.
    let main_root = tmp.path().join("repo");
    std::fs::create_dir_all(main_root.join("src")).unwrap();
    std::fs::write(
        main_root.join("Cargo.toml"),
        "[package]\nname=\"x\"\nversion=\"0.0.0\"\n",
    )
    .unwrap();
    std::fs::write(
        main_root.join("src/lib.rs"),
        "fn worktree_canary_zz() { let _ = 1; }\n",
    )
    .unwrap();
    let root = main_root.to_string_lossy().to_string();

    // Worktree INSIDE the project root (realistic layout).
    let wt_dir = main_root.join(".claude/worktrees/wt/src");
    std::fs::create_dir_all(&wt_dir).unwrap();
    std::fs::write(
        wt_dir.join("lib.rs"),
        "fn worktree_canary_zz() { let _ = 2; }\n",
    )
    .unwrap();

    // The symbol resolves via the main-root index to "src/lib.rs".
    let resolved = super::resolve_name_path("worktree_canary_zz", &root);
    assert!(resolved.is_ok(), "symbol should resolve: {resolved:?}");

    // Caller provides a path inside the worktree that differs from the
    // index-resolved location — this must be caught.
    let wt_file = main_root.join(".claude/worktrees/wt/src/lib.rs");
    let args = serde_json::json!({
        "action": "replace_symbol_body",
        "name_path": "worktree_canary_zz",
        "path": wt_file.to_string_lossy().to_string(),
        "new_body": "fn worktree_canary_zz() { panic!(\"CANARY\"); }"
    });
    let out = super::handle(&args, &root, "");
    assert!(
        out.contains("WORKTREE_MISMATCH"),
        "should detect mismatch, got: {out}"
    );
    // Verify neither file was modified.
    let main_content = std::fs::read_to_string(main_root.join("src/lib.rs")).unwrap();
    assert!(
        !main_content.contains("CANARY"),
        "main checkout must NOT be modified: {main_content}"
    );
    let wt_content = std::fs::read_to_string(&wt_file).unwrap();
    assert!(
        !wt_content.contains("CANARY"),
        "worktree must NOT be modified: {wt_content}"
    );

    crate::test_env::remove_var("LEAN_CTX_DATA_DIR");
}

/// #803: when `path` matches the resolved symbol (same file), the edit succeeds.
#[test]
fn handle_replace_symbol_same_path_succeeds() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.rs"), "fn old() {\n  1\n}\n").unwrap();
    let abs = dir.path().join("a.rs").to_string_lossy().to_string();
    let args = serde_json::json!({
        "action": "replace_symbol_body",
        "path": "a.rs",
        "line": 1,
        "end_line": 3,
        "new_body": "fn new() {\n  2\n}"
    });
    let out = super::handle(&args, dir.path().to_str().unwrap(), &abs);
    assert!(out.contains("replace_symbol_body applied"), "got: {out}");
    let after = std::fs::read_to_string(dir.path().join("a.rs")).unwrap();
    assert!(after.contains("fn new()"), "file: {after}");
}
