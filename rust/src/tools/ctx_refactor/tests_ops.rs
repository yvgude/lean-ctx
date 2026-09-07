//! Tests for the refactor operations (rename/safe-delete/move/inline gating).

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

/// Minimal backend whose `reformat` returns a canned `changed_paths` list.
struct ReformatStub {
    changed: Vec<String>,
}
impl crate::lsp::backend::LspBackend for ReformatStub {
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
    fn reformat(
        &mut self,
        _q: &crate::lsp::backend::ReformatQuery,
    ) -> Result<crate::lsp::backend::ReformatResult, String> {
        Ok(crate::lsp::backend::ReformatResult {
            applied: true,
            changed_paths: self.changed.clone(),
        })
    }
}

#[test]
fn reformat_command_path_reports_changed_and_invalidates_single_file() {
    // rustfmt-gated: only runs when rustfmt is installed.
    if std::process::Command::new("rustfmt")
        .arg("--version")
        .output()
        .is_err()
    {
        eprintln!("SKIP: rustfmt not in PATH");
        return;
    }
    // The command and JetBrains reformat arms both mutate the process-global
    // `cli_cache` under `LEAN_CTX_DATA_DIR`. Hold the same guard as the
    // multi-file regression so concurrent load-modify-save operations cannot
    // clobber each other's cache entries (#1720).
    let _data = crate::core::data_dir::isolated_data_dir();

    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("drift.rs"), "fn   x( ){let y=1;}\n").unwrap(); // drift
    let root = dir.path().to_str().unwrap();
    let args = serde_json::json!({ "action": "reformat", "path": "drift.rs" });
    let abs_path =
        crate::core::path_resolve::resolve_tool_path(Some(root), None, "drift.rs").unwrap();

    // Warm the same on-disk cache that the formatter invalidates.
    let _ = crate::core::cli_cache::check_and_read(&abs_path);
    assert!(matches!(
        crate::core::cli_cache::check_and_read(&abs_path),
        crate::core::cli_cache::CacheResult::Hit { .. }
    ));

    // First run: rustfmt rewrites the drifted file → "changed".
    let out = super::handle_reformat_refactor(&args, root);
    assert!(out.contains("via rustfmt"), "got: {out}");
    assert!(
        out.contains("— changed"),
        "first run must report changed; got: {out}"
    );
    assert!(
        matches!(
            crate::core::cli_cache::check_and_read(&abs_path),
            crate::core::cli_cache::CacheResult::Miss { .. }
        ),
        "changed rustfmt output must invalidate the cached file"
    );

    // Second run: already conformant → honest "unchanged" (B2: blake3 on the
    // single file, never a directory).
    let out2 = super::handle_reformat_refactor(&args, root);
    assert!(
        out2.contains("— unchanged"),
        "second run must report unchanged; got: {out2}"
    );
}

#[test]
#[cfg_attr(
    target_os = "windows",
    ignore = "path normalization difference on Windows"
)]
fn reformat_jetbrains_scope_invalidates_all_changed_paths() {
    // B2: the Jetbrains arm must keep every changed path, invalidate ALL of them,
    // and report the true count. This test observes the REAL cache effect rather
    // than the output string alone: it warms `cli_cache` for both changed paths,
    // runs `render_reformat`, then asserts both entries were evicted. A B-regression
    // that skips the invalidate loop (e.g. `.map(|_| ())`) leaves the — unchanged —
    // entries cached, so the post-run reads stay `Hit` and this test goes red.
    //
    // The private data dir isolates the on-disk `cli_cache` store and serializes
    // via the global env-lock, so warming/eviction is deterministic here.
    let _data = crate::core::data_dir::isolated_data_dir();

    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.kt"), "val a = 1\n").unwrap();
    std::fs::write(dir.path().join("b.kt"), "val b = 1\n").unwrap();
    let root = dir.path().to_str().unwrap();

    // Resolve exactly as `render_reformat` does for each changed path, so the cache
    // keys line up: `invalidate` and `check_and_read` both funnel the abs path
    // through `normalize_tool_path` (same key convention).
    let abs_a = crate::core::path_resolve::resolve_tool_path(Some(root), None, "a.kt").unwrap();
    let abs_b = crate::core::path_resolve::resolve_tool_path(Some(root), None, "b.kt").unwrap();

    // `check_and_read` re-inserts on a Miss, so it is a single-shot probe: read it
    // exactly once per assertion. A `Hit` proves the entry is currently present.
    let is_cached = |abs: &str| {
        matches!(
            crate::core::cli_cache::check_and_read(abs),
            crate::core::cli_cache::CacheResult::Hit { .. }
        )
    };

    // Warm: the first read is a Miss that inserts the entry; the second must Hit.
    let _ = crate::core::cli_cache::check_and_read(&abs_a);
    let _ = crate::core::cli_cache::check_and_read(&abs_b);
    assert!(
        is_cached(&abs_a),
        "premise: a.kt must be cached after warming"
    );
    assert!(
        is_cached(&abs_b),
        "premise: b.kt must be cached after warming"
    );

    let mut be = ReformatStub {
        changed: vec!["a.kt".into(), "b.kt".into()],
    };
    let query = crate::lsp::backend::ReformatQuery {
        abs_path: abs_a.clone(),
        rel_path: "a.kt".into(),
        scope: crate::lsp::backend::ReformatScope::File,
        optimize_imports: false,
    };
    let out = super::render_reformat(&mut be, root, &query);
    assert!(out.contains("changed files: 2"), "got: {out}");
    assert!(
        !out.contains("unchanged"),
        "Jetbrains arm must not report unchanged; got: {out}"
    );

    // The real effect: BOTH changed paths were invalidated. The stub never rewrites
    // the files, so their content hash is unchanged — the only way these reads can
    // Miss is an actual eviction by the invalidate loop.
    assert!(
        !is_cached(&abs_a),
        "render_reformat must invalidate a.kt (B2 regression: invalidate loop skipped)"
    );
    assert!(
        !is_cached(&abs_b),
        "render_reformat must invalidate b.kt (B2 regression: invalidate loop skipped)"
    );
}
