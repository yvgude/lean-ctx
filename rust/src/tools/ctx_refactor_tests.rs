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

#[test]
fn resolve_rename_target_position_fallback() {
    let (rel, sl, el) = super::resolve_rename_target(
        &serde_json::json!({"path": "a.rs", "line": 3, "end_line": 5}),
        "/proj",
    )
    .unwrap();
    assert_eq!(rel, "a.rs");
    assert_eq!((sl, el), (3, 5));
}

#[test]
fn resolve_rename_target_requires_line_in_fallback() {
    let err =
        super::resolve_rename_target(&serde_json::json!({"path": "a.rs"}), "/proj").unwrap_err();
    assert!(err.contains("line"), "got: {err}");
}

#[test]
fn live_backend_absent_is_backend_required() {
    // No port file under an unlikely root → deterministic BACKEND_REQUIRED, no HTTP.
    let err = super::live_jetbrains_backend("/nonexistent/leanctx/proj/zzz")
        .err()
        .expect("expected Err from live_jetbrains_backend");
    assert!(err.starts_with("BACKEND_REQUIRED"), "got: {err}");
}

/// Minimal backend that returns canned rename plans + records apply calls.
struct RenameStub {
    plan: crate::lsp::backend::RenamePlan,
    applied_with_force: std::cell::Cell<Option<bool>>,
}
impl crate::lsp::backend::LspBackend for RenameStub {
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
    fn rename_preview(
        &mut self,
        _q: &crate::lsp::backend::RenameQuery,
    ) -> Result<crate::lsp::backend::RenamePlan, String> {
        Ok(self.plan.clone())
    }
    fn rename_apply(
        &mut self,
        req: &crate::lsp::backend::RenameApply,
    ) -> Result<crate::lsp::backend::RenameResult, String> {
        self.applied_with_force.set(Some(req.force));
        Ok(crate::lsp::backend::RenameResult {
            applied: true,
            changed_paths: vec!["a.rs".into()],
        })
    }
}

fn stub_query(abs: &str) -> crate::lsp::backend::RenameQuery {
    crate::lsp::backend::RenameQuery {
        abs_path: abs.into(),
        rel_path: "a.rs".into(),
        target_range: crate::lsp::backend::TextRange0Based {
            start_line: 0,
            start_char: 4,
            end_line: 0,
            end_char: 7,
        },
        new_name: "bar".into(),
        search_comments: false,
        search_text_occurrences: false,
    }
}

#[test]
fn apply_blocks_on_plan_hash_mismatch() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.rs"), "let foo = 1;\nfoo + foo;\n").unwrap();
    let root = dir.path().to_str().unwrap();
    let usage = crate::lsp::backend::UsageSite {
        path: "a.rs".into(),
        range: crate::lsp::backend::TextRange0Based {
            start_line: 0,
            start_char: 4,
            end_line: 0,
            end_char: 7,
        },
        context: None,
    };
    let mut be = RenameStub {
        plan: crate::lsp::backend::RenamePlan {
            usages: vec![usage],
            conflicts: vec![],
        },
        applied_with_force: std::cell::Cell::new(None),
    };
    let q = stub_query(&dir.path().join("a.rs").to_string_lossy());
    let out = super::render_rename_apply(&mut be, root, &q, "bar", "stalehash", false);
    assert!(out.contains("CONFLICT"), "got: {out}");
    assert_eq!(
        be.applied_with_force.get(),
        None,
        "apply must not run on hash mismatch"
    );
}

#[test]
fn apply_blocks_on_conflicts_without_force_and_passes_with_force() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.rs"), "let foo = 1;\nfoo + foo;\n").unwrap();
    let root = dir.path().to_str().unwrap();
    let usage = crate::lsp::backend::UsageSite {
        path: "a.rs".into(),
        range: crate::lsp::backend::TextRange0Based {
            start_line: 0,
            start_char: 4,
            end_line: 0,
            end_char: 7,
        },
        context: None,
    };
    let plan = crate::lsp::backend::RenamePlan {
        usages: vec![usage.clone()],
        conflicts: vec![crate::lsp::backend::Conflict {
            path: "a.rs".into(),
            range: None,
            message: "clash".into(),
        }],
    };
    let hash = super::plan_hash(root, &plan.usages).unwrap();
    let q = stub_query(&dir.path().join("a.rs").to_string_lossy());

    // force=false → CONFLICT, apply not called.
    let mut be = RenameStub {
        plan: plan.clone(),
        applied_with_force: std::cell::Cell::new(None),
    };
    let out = super::render_rename_apply(&mut be, root, &q, "bar", &hash, false);
    assert!(out.contains("CONFLICT"), "got: {out}");
    assert_eq!(be.applied_with_force.get(), None);

    // force=true → applies, force passed through.
    let mut be2 = RenameStub {
        plan,
        applied_with_force: std::cell::Cell::new(None),
    };
    let out2 = super::render_rename_apply(&mut be2, root, &q, "bar", &hash, true);
    assert!(out2.contains("applied"), "got: {out2}");
    assert_eq!(be2.applied_with_force.get(), Some(true));
}

#[test]
fn apply_success_emits_diff_and_evicts() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.rs"), "let foo = 1;\nfoo + foo;\n").unwrap();
    let root = dir.path().to_str().unwrap();
    let usage = crate::lsp::backend::UsageSite {
        path: "a.rs".into(),
        range: crate::lsp::backend::TextRange0Based {
            start_line: 0,
            start_char: 4,
            end_line: 0,
            end_char: 7,
        },
        context: None,
    };
    let plan = crate::lsp::backend::RenamePlan {
        usages: vec![usage],
        conflicts: vec![],
    };
    let hash = super::plan_hash(root, &plan.usages).unwrap();
    let mut be = RenameStub {
        plan,
        applied_with_force: std::cell::Cell::new(None),
    };
    let q = stub_query(&dir.path().join("a.rs").to_string_lossy());
    let out = super::render_rename_apply(&mut be, root, &q, "bar", &hash, false);
    assert!(out.contains("applied"), "got: {out}");
    assert!(out.contains("\"foo\" → \"bar\""), "diff missing: {out}");
}

#[test]
fn preview_renders_plan_hash_and_files() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("usage.rs"), "let foo = 1;\nfoo + foo;\n").unwrap();
    let root = dir.path().to_str().unwrap();
    let usage = crate::lsp::backend::UsageSite {
        path: "usage.rs".into(),
        range: crate::lsp::backend::TextRange0Based {
            start_line: 0,
            start_char: 4,
            end_line: 0,
            end_char: 7,
        },
        context: None,
    };
    let plan = crate::lsp::backend::RenamePlan {
        usages: vec![usage],
        conflicts: vec![],
    };
    let mut be = RenameStub {
        plan,
        applied_with_force: std::cell::Cell::new(None),
    };
    let mut q = stub_query(&dir.path().join("usage.rs").to_string_lossy());
    q.rel_path = "decl.rs".into();
    let out = super::render_rename_preview(&mut be, root, &q, "bar");
    assert!(out.contains("plan_hash:"), "got: {out}");
    assert!(out.contains("usages: 1"), "got: {out}");
    assert!(out.contains("files: 2"), "got: {out}");
    assert!(out.contains("usage.rs: 1 usage"), "got: {out}");
}

#[test]
fn handle_rename_preview_without_ide_is_backend_required() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.rs"), "fn foo() {}\n").unwrap();
    let root = dir.path().to_str().unwrap();
    // No port file under this temp root → BACKEND_REQUIRED before any HTTP.
    let args = serde_json::json!({
        "action": "rename_preview", "path": "a.rs", "line": 1, "new_name": "bar"
    });
    let out = super::handle(&args, root, "");
    assert!(out.contains("BACKEND_REQUIRED"), "got: {out}");
}

#[test]
fn handle_rename_apply_requires_plan_hash() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.rs"), "fn foo() {}\n").unwrap();
    let root = dir.path().to_str().unwrap();
    let args = serde_json::json!({
        "action": "rename_apply", "path": "a.rs", "line": 1, "new_name": "bar"
    });
    let out = super::handle(&args, root, "");
    assert!(out.contains("plan_hash"), "got: {out}");
}

#[test]
fn handle_safe_delete_preview_without_ide_is_backend_required() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.rs"), "fn foo() {}\n").unwrap();
    let root = dir.path().to_str().unwrap();
    let args = serde_json::json!({"action": "safe_delete_preview", "path": "a.rs", "line": 1});
    let out = super::handle(&args, root, "");
    assert!(out.contains("BACKEND_REQUIRED"), "got: {out}");
}

#[test]
fn handle_safe_delete_apply_requires_plan_hash() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.rs"), "fn foo() {}\n").unwrap();
    let root = dir.path().to_str().unwrap();
    let args = serde_json::json!({"action": "safe_delete_apply", "path": "a.rs", "line": 1});
    let out = super::handle(&args, root, "");
    assert!(out.contains("plan_hash"), "got: {out}");
}

#[test]
fn resolve_move_target_requires_exactly_one_field() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("app/moved")).unwrap();
    let root = dir.path().to_str().unwrap();

    // Neither set → INVALID_TARGET.
    let err = super::resolve_move_target(&serde_json::json!({}), root).unwrap_err();
    assert!(err.starts_with("INVALID_TARGET"), "got: {err}");

    // Both set → INVALID_TARGET.
    let err2 = super::resolve_move_target(
        &serde_json::json!({"target_path": "app/moved", "target_parent": "Other"}),
        root,
    )
    .unwrap_err();
    assert!(err2.starts_with("INVALID_TARGET"), "got: {err2}");
}

// Jail rejection only happens when the jail is compiled in. `--all-features`
// pulls in `no-jail` (jail disabled), so skip there like every other jail
// assertion (see e.g. server::multi_path tests).
#[cfg(not(feature = "no-jail"))]
#[test]
fn resolve_move_target_path_is_jailed() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("app/moved")).unwrap();
    let root = dir.path().to_str().unwrap();

    // In-jail path resolves to a MoveTarget::Path.
    let t =
        super::resolve_move_target(&serde_json::json!({"target_path": "app/moved"}), root).unwrap();
    match t {
        crate::lsp::backend::MoveTarget::Path { rel_path, .. } => {
            assert_eq!(rel_path, "app/moved");
        }
        other @ crate::lsp::backend::MoveTarget::Parent { .. } => {
            panic!("expected Path, got {other:?}")
        }
    }

    // Escape attempt → INVALID_TARGET (jail violation, before any backend call).
    let err =
        super::resolve_move_target(&serde_json::json!({"target_path": "../../etc/skel"}), root)
            .unwrap_err();
    assert!(err.starts_with("INVALID_TARGET"), "got: {err}");
}

/// Minimal backend for the move renderers: canned plan + recorded apply flags + changed paths.
struct MoveStub {
    plan: crate::lsp::backend::RenamePlan,
    applied_with_force: std::cell::Cell<Option<bool>>,
}
impl crate::lsp::backend::LspBackend for MoveStub {
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
    fn move_preview(
        &mut self,
        _q: &crate::lsp::backend::MoveQuery,
    ) -> Result<crate::lsp::backend::RenamePlan, String> {
        Ok(self.plan.clone())
    }
    fn move_apply(
        &mut self,
        req: &crate::lsp::backend::MoveApply,
    ) -> Result<crate::lsp::backend::RenameResult, String> {
        self.applied_with_force.set(Some(req.force));
        Ok(crate::lsp::backend::RenameResult {
            applied: true,
            changed_paths: vec!["app/moved/Widget.kt".into()],
        })
    }
}

fn move_query(abs: &str) -> crate::lsp::backend::MoveQuery {
    crate::lsp::backend::MoveQuery {
        abs_path: abs.into(),
        rel_path: "a.rs".into(),
        src_range: crate::lsp::backend::TextRange0Based {
            start_line: 0,
            start_char: 4,
            end_line: 0,
            end_char: 7,
        },
        target: crate::lsp::backend::MoveTarget::Path {
            abs_path: "/p/app/moved".into(),
            rel_path: "app/moved".into(),
        },
    }
}

#[test]
fn move_apply_gates_then_evicts() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("app/moved")).unwrap();
    std::fs::write(dir.path().join("a.rs"), "let foo = 1;\nfoo + foo;\n").unwrap();
    std::fs::write(dir.path().join("app/moved/Widget.kt"), "// moved\n").unwrap();
    let root = dir.path().to_str().unwrap();
    let usage = crate::lsp::backend::UsageSite {
        path: "a.rs".into(),
        range: crate::lsp::backend::TextRange0Based {
            start_line: 0,
            start_char: 4,
            end_line: 0,
            end_char: 7,
        },
        context: None,
    };
    let plan = crate::lsp::backend::RenamePlan {
        usages: vec![usage],
        conflicts: vec![],
    };
    let hash = super::plan_hash(root, &plan.usages).unwrap();
    let q = move_query(&dir.path().join("a.rs").to_string_lossy());

    // hash mismatch → CONFLICT, apply not called.
    let mut be = MoveStub {
        plan: plan.clone(),
        applied_with_force: std::cell::Cell::new(None),
    };
    let out = super::render_move_apply(&mut be, root, &q, "stalehash", false);
    assert!(out.contains("CONFLICT"), "got: {out}");
    assert_eq!(be.applied_with_force.get(), None);

    // matching hash + force → applies, force passed through, changed path jailed+evicted.
    let mut be2 = MoveStub {
        plan,
        applied_with_force: std::cell::Cell::new(None),
    };
    let out2 = super::render_move_apply(&mut be2, root, &q, &hash, true);
    assert!(out2.contains("applied"), "got: {out2}");
    assert_eq!(be2.applied_with_force.get(), Some(true));
}

// See above: jail rejection requires the jail compiled in (skipped under no-jail).
#[cfg(not(feature = "no-jail"))]
#[test]
fn move_apply_rejects_out_of_jail_changed_path() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.rs"), "let foo = 1;\nfoo + foo;\n").unwrap();
    let root = dir.path().to_str().unwrap();
    let usage = crate::lsp::backend::UsageSite {
        path: "a.rs".into(),
        range: crate::lsp::backend::TextRange0Based {
            start_line: 0,
            start_char: 4,
            end_line: 0,
            end_char: 7,
        },
        context: None,
    };
    // Stub returns an out-of-jail changed path (stage-3 jail must reject it post-apply).
    struct EscapeStub {
        plan: crate::lsp::backend::RenamePlan,
    }
    impl crate::lsp::backend::LspBackend for EscapeStub {
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
        fn move_preview(
            &mut self,
            _q: &crate::lsp::backend::MoveQuery,
        ) -> Result<crate::lsp::backend::RenamePlan, String> {
            Ok(self.plan.clone())
        }
        fn move_apply(
            &mut self,
            _r: &crate::lsp::backend::MoveApply,
        ) -> Result<crate::lsp::backend::RenameResult, String> {
            Ok(crate::lsp::backend::RenameResult {
                applied: true,
                changed_paths: vec!["../../etc/passwd".into()],
            })
        }
    }
    let plan = crate::lsp::backend::RenamePlan {
        usages: vec![usage],
        conflicts: vec![],
    };
    let hash = super::plan_hash(root, &plan.usages).unwrap();
    let mut be = EscapeStub { plan };
    let q = move_query(&dir.path().join("a.rs").to_string_lossy());
    let out = super::render_move_apply(&mut be, root, &q, &hash, false);
    assert!(out.contains("jail"), "expected jail rejection, got: {out}");
}

#[test]
fn handle_move_preview_invalid_target_before_backend() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.rs"), "fn foo() {}\n").unwrap();
    let root = dir.path().to_str().unwrap();
    // No target → INVALID_TARGET, and crucially BEFORE BACKEND_REQUIRED (no live IDE here).
    let args = serde_json::json!({"action": "move_preview", "path": "a.rs", "line": 1});
    let out = super::handle(&args, root, "");
    assert!(out.contains("INVALID_TARGET"), "got: {out}");
    assert!(
        !out.contains("BACKEND_REQUIRED"),
        "target gate must precede backend gate: {out}"
    );
}

#[test]
fn handle_move_apply_requires_plan_hash() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("x")).unwrap();
    std::fs::write(dir.path().join("a.rs"), "fn foo() {}\n").unwrap();
    let root = dir.path().to_str().unwrap();
    let args =
        serde_json::json!({"action": "move_apply", "path": "a.rs", "line": 1, "target_path": "x"});
    let out = super::handle(&args, root, "");
    assert!(out.contains("plan_hash"), "got: {out}");
}

#[test]
fn unknown_action_help_lists_rename_actions() {
    // Resolution happens before backend selection for rename actions, so an
    // empty new_name short-circuits with a clear ERROR mentioning new_name.
    let args = serde_json::json!({"action": "rename_preview", "path": "a.rs", "line": 1});
    let out = super::handle(&args, "/proj", "");
    assert!(out.contains("new_name"), "got: {out}");
}

/// Minimal backend for the safe_delete renderers: canned plan + recorded apply flags.
struct SafeDeleteStub {
    plan: crate::lsp::backend::RenamePlan,
    applied: std::cell::Cell<Option<(bool, bool)>>, // (force, propagate)
}
impl crate::lsp::backend::LspBackend for SafeDeleteStub {
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
    fn safe_delete_preview(
        &mut self,
        _q: &crate::lsp::backend::SafeDeleteQuery,
    ) -> Result<crate::lsp::backend::RenamePlan, String> {
        Ok(self.plan.clone())
    }
    fn safe_delete_apply(
        &mut self,
        req: &crate::lsp::backend::SafeDeleteApply,
    ) -> Result<crate::lsp::backend::RenameResult, String> {
        self.applied.set(Some((req.force, req.propagate)));
        Ok(crate::lsp::backend::RenameResult {
            applied: true,
            changed_paths: vec!["Widget.kt".into()],
        })
    }
}

fn safe_delete_query(abs: &str) -> crate::lsp::backend::SafeDeleteQuery {
    crate::lsp::backend::SafeDeleteQuery {
        abs_path: abs.into(),
        rel_path: "a.rs".into(),
        src_range: crate::lsp::backend::TextRange0Based {
            start_line: 0,
            start_char: 4,
            end_line: 0,
            end_char: 7,
        },
    }
}

#[test]
fn safe_delete_apply_blocks_on_remaining_refs_without_force() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.rs"), "let foo = 1;\nfoo + foo;\n").unwrap();
    let root = dir.path().to_str().unwrap();
    let usage = crate::lsp::backend::UsageSite {
        path: "a.rs".into(),
        range: crate::lsp::backend::TextRange0Based {
            start_line: 0,
            start_char: 4,
            end_line: 0,
            end_char: 7,
        },
        context: None,
    };
    // A remaining reference = a blocking conflict (spec §5.4).
    let plan = crate::lsp::backend::RenamePlan {
        usages: vec![usage.clone()],
        conflicts: vec![crate::lsp::backend::Conflict {
            path: "a.rs".into(),
            range: None,
            message: "still referenced".into(),
        }],
    };
    let hash = super::plan_hash(root, &plan.usages).unwrap();
    let q = safe_delete_query(&dir.path().join("a.rs").to_string_lossy());

    // force=false → CONFLICT, apply not called.
    let mut be = SafeDeleteStub {
        plan: plan.clone(),
        applied: std::cell::Cell::new(None),
    };
    let out = super::render_safe_delete_apply(&mut be, root, &q, &hash, false, false);
    assert!(out.contains("CONFLICT"), "got: {out}");
    assert_eq!(be.applied.get(), None);

    // force=true → applies, force+propagate passed through.
    let mut be2 = SafeDeleteStub {
        plan,
        applied: std::cell::Cell::new(None),
    };
    let out2 = super::render_safe_delete_apply(&mut be2, root, &q, &hash, true, true);
    assert!(
        out2.contains("deleted") || out2.contains("applied"),
        "got: {out2}"
    );
    assert_eq!(be2.applied.get(), Some((true, true)));
}

#[test]
fn safe_delete_apply_blocks_on_plan_hash_mismatch() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.rs"), "let foo = 1;\nfoo + foo;\n").unwrap();
    let root = dir.path().to_str().unwrap();
    let usage = crate::lsp::backend::UsageSite {
        path: "a.rs".into(),
        range: crate::lsp::backend::TextRange0Based {
            start_line: 0,
            start_char: 4,
            end_line: 0,
            end_char: 7,
        },
        context: None,
    };
    let mut be = SafeDeleteStub {
        plan: crate::lsp::backend::RenamePlan {
            usages: vec![usage],
            conflicts: vec![],
        },
        applied: std::cell::Cell::new(None),
    };
    let q = safe_delete_query(&dir.path().join("a.rs").to_string_lossy());
    let out = super::render_safe_delete_apply(&mut be, root, &q, "stalehash", false, false);
    assert!(out.contains("CONFLICT"), "got: {out}");
    assert_eq!(be.applied.get(), None);
}

/// Minimal backend for the inline renderers: canned preview plan (with
/// optional conflicts) + a no-op apply. Mirrors SafeDeleteStub above, but the
/// inline path has NO force flag, so the stub records nothing.
struct InlineStub {
    conflicts: Vec<crate::lsp::backend::Conflict>,
}
impl crate::lsp::backend::LspBackend for InlineStub {
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
    fn inline_preview(
        &mut self,
        _q: &crate::lsp::backend::InlineQuery,
    ) -> Result<crate::lsp::backend::RenamePlan, String> {
        Ok(crate::lsp::backend::RenamePlan {
            usages: vec![],
            conflicts: self.conflicts.clone(),
        })
    }
    fn inline_apply(
        &mut self,
        _r: &crate::lsp::backend::InlineApply,
    ) -> Result<crate::lsp::backend::RenameResult, String> {
        Ok(crate::lsp::backend::RenameResult {
            applied: true,
            changed_paths: vec![],
        })
    }
}

fn inline_query(abs: &str) -> crate::lsp::backend::InlineQuery {
    crate::lsp::backend::InlineQuery {
        abs_path: abs.to_string(),
        rel_path: "Calc.kt".to_string(),
        src_range: crate::lsp::backend::TextRange0Based {
            start_line: 0,
            start_char: 0,
            end_line: 0,
            end_char: 0,
        },
        keep_definition: false,
    }
}

#[test]
fn handle_inline_apply_requires_plan_hash() {
    let args = serde_json::json!({ "action": "inline_apply", "name_path": "Calc/tmp" });
    let out = super::handle_inline_refactor("inline_apply", &args, "/nonexistent-root");
    assert!(out.contains("plan_hash"), "got: {out}");
}

#[test]
fn handle_inline_preview_without_ide_is_backend_required() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("Calc.kt"), "val tmp = 1\n").unwrap();
    let root = dir.path().to_str().unwrap();
    // File exists → flow reaches the live-IDE gate; no port file → BACKEND_REQUIRED.
    let args = serde_json::json!({ "action": "inline_preview", "path": "Calc.kt", "line": 1 });
    let out = super::handle_inline_refactor("inline_preview", &args, root);
    assert!(out.contains("BACKEND_REQUIRED"), "got: {out}");
}

#[test]
fn inline_apply_blocks_on_conflicts_with_no_force_path() {
    // A conflicting plan must ALWAYS produce CONFLICT — there is no force arg to pass.
    let mut be = InlineStub {
        conflicts: vec![crate::lsp::backend::Conflict {
            path: "Calc.kt".into(),
            range: None,
            message: "recursive".into(),
        }],
    };
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("Calc.kt");
    std::fs::write(&f, "val tmp = 1\n").unwrap();
    let q = inline_query(f.to_str().unwrap());
    // expected_hash is irrelevant: the conflict gate fires regardless.
    let out = super::render_inline_apply(&mut be, dir.path().to_str().unwrap(), &q, "deadbeef");
    assert!(out.contains("CONFLICT"), "got: {out}");
}

#[test]
fn reformat_invalid_target_when_no_address() {
    let args = serde_json::json!({ "action": "reformat" });
    let out = super::handle_reformat_refactor(&args, env!("CARGO_MANIFEST_DIR"));
    assert!(out.contains("INVALID_TARGET"), "got: {out}");
}

#[test]
fn reformat_address_dispatch_resolves_scope() {
    // path alone → File; path+line → Region; name_path → Symbol.
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("M.kt");
    std::fs::write(&f, "fun a(){}\nfun b(){}\n").unwrap();
    let root = dir.path().to_str().unwrap();

    let file_args = serde_json::json!({ "action": "reformat", "path": "M.kt" });
    let (_abs, _rel, scope) = super::resolve_reformat_scope(&file_args, root).unwrap();
    assert!(matches!(scope, crate::lsp::backend::ReformatScope::File));

    let region_args =
        serde_json::json!({ "action": "reformat", "path": "M.kt", "line": 1, "end_line": 2 });
    let (_a, _r, scope) = super::resolve_reformat_scope(&region_args, root).unwrap();
    assert!(matches!(
        scope,
        crate::lsp::backend::ReformatScope::Region { .. }
    ));
}

#[test]
fn reformat_without_ide_is_backend_required() {
    let args = serde_json::json!({ "action": "reformat", "path": "M.kt" });
    let out = super::handle_reformat_refactor(&args, env!("CARGO_MANIFEST_DIR"));
    // Either resolved scope then BACKEND_REQUIRED, or FILE_NOT_FOUND if M.kt absent in manifest.
    assert!(
        out.contains("BACKEND_REQUIRED") || out.contains("FILE_NOT_FOUND"),
        "got: {out}"
    );
}
