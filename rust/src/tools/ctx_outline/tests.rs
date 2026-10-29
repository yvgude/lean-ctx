use super::{OutlineOpts, filter_signatures, run};
use crate::core::signatures::Signature;

fn sig(kind: &'static str, name: &str, exported: bool) -> Signature {
    Signature {
        kind,
        name: name.to_string(),
        params: String::new(),
        return_type: String::new(),
        is_async: false,
        is_exported: exported,
        indent: 0,
        ..Signature::no_span()
    }
}

fn sample() -> Vec<Signature> {
    vec![
        sig("fn", "main", false),
        sig("struct", "Config", true),
        sig("fn", "load_config", true),
    ]
}

fn write(dir: &std::path::Path, rel: &str, body: &str) {
    let p = dir.join(rel);
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(p, body).unwrap();
}

// --- filtering -----------------------------------------------------------

#[test]
fn filter_kind_matches_case_insensitively() {
    let sigs = sample();
    let opts = OutlineOpts {
        kind: Some("FN"),
        ..Default::default()
    };
    assert_eq!(filter_signatures(&sigs, &opts).len(), 2);
}

#[test]
fn filter_all_and_none_return_everything() {
    let sigs = sample();
    let all = OutlineOpts {
        kind: Some("all"),
        ..Default::default()
    };
    assert_eq!(filter_signatures(&sigs, &all).len(), 3);
    assert_eq!(filter_signatures(&sigs, &OutlineOpts::default()).len(), 3);
}

#[test]
fn filter_name_match_is_substring_case_insensitive() {
    let sigs = sample();
    let opts = OutlineOpts {
        name_match: Some("CONFIG"),
        ..Default::default()
    };
    let names: Vec<&str> = filter_signatures(&sigs, &opts)
        .iter()
        .map(|s| s.name.as_str())
        .collect();
    assert_eq!(names, vec!["Config", "load_config"]);
}

#[test]
fn filter_kind_and_match_compose() {
    let sigs = sample();
    let opts = OutlineOpts {
        kind: Some("fn"),
        name_match: Some("config"),
        ..Default::default()
    };
    let got = filter_signatures(&sigs, &opts);
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].name, "load_config");
}

// --- single file ---------------------------------------------------------

#[test]
fn file_outline_includes_line_spans() {
    let tmp = tempfile::tempdir().unwrap();
    write(tmp.path(), "a.rs", "pub fn alpha() {}\npub fn beta() {}\n");
    let path = tmp.path().join("a.rs");
    let (out, original) = run(path.to_str().unwrap(), &OutlineOpts::default());
    assert!(out.contains("alpha"), "{out}");
    // The whole point of an outline is navigation, so spans are always rendered
    // (located variant) regardless of CRP mode.
    assert!(out.contains("@L1"), "{out}");
    assert!(original > 0);
}

#[test]
fn file_outline_emits_handle_hint() {
    // Located symbols are addressable as stable handles (#607): the outline
    // carries one self-describing usage hint.
    let tmp = tempfile::tempdir().unwrap();
    write(tmp.path(), "a.rs", "pub fn alpha() {}\n");
    let path = tmp.path().join("a.rs");
    let (out, _) = run(path.to_str().unwrap(), &OutlineOpts::default());
    assert!(out.contains("handle=\"path#name@Lstart\""), "{out}");
}

#[test]
fn rust_impl_has_impl_kind_not_class() {
    let tmp = tempfile::tempdir().unwrap();
    write(
        tmp.path(),
        "a.rs",
        "pub struct Foo;\nimpl Clone for Foo {\n    fn clone(&self) -> Self { Foo }\n}\n",
    );
    let path = tmp.path().join("a.rs");
    let (out, _) = run(
        path.to_str().unwrap(),
        &OutlineOpts {
            as_json: true,
            ..Default::default()
        },
    );
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    let syms = v["symbols"].as_array().unwrap();
    let impl_sym = syms
        .iter()
        .find(|s| s["kind"] == "impl")
        .expect("impl rendered with its own kind");
    assert!(
        impl_sym["name"].as_str().unwrap().contains("Clone for Foo"),
        "{out}"
    );
    assert!(
        !syms.iter().any(|s| s["kind"] == "class"),
        "impl must never be labelled class: {out}"
    );
}

#[test]
fn no_match_message_is_informative() {
    let tmp = tempfile::tempdir().unwrap();
    write(tmp.path(), "a.rs", "pub fn alpha() {}\n");
    let path = tmp.path().join("a.rs");
    let opts = OutlineOpts {
        name_match: Some("zzz"),
        ..Default::default()
    };
    let (out, original) = run(path.to_str().unwrap(), &opts);
    assert!(out.contains("No symbols matching 'zzz'"), "{out}");
    assert_eq!(original, 0);
}

#[test]
#[cfg(unix)]
fn symlink_in_tree_is_followed() {
    // `run` outlines the symlink's real target; escape protection (a symlink
    // pointing outside the project root) is the resolution layer's job and is
    // covered by the PathJail tests in `server::tool_trait`. Here we lock in that
    // an *in-tree* symlink is treated like the file it points at, not rejected
    // with a misleading "skipped for security" message that never fires on the
    // live MCP path.
    let tmp = tempfile::tempdir().unwrap();
    let target = tmp.path().join("real.rs");
    std::fs::write(&target, "pub fn alpha() {}\n").unwrap();
    let link = tmp.path().join("link.rs");
    std::os::unix::fs::symlink(&target, &link).unwrap();
    let (out, _) = run(link.to_str().unwrap(), &OutlineOpts::default());
    assert!(out.contains("alpha"), "{out}");
}

// --- directory -----------------------------------------------------------

#[test]
fn directory_outline_is_sorted_and_grouped() {
    let tmp = tempfile::tempdir().unwrap();
    write(tmp.path(), "z_last.rs", "pub fn zeta() {}\n");
    write(tmp.path(), "a_first.rs", "pub fn alpha() {}\n");
    let (out, _) = run(tmp.path().to_str().unwrap(), &OutlineOpts::default());
    let a_pos = out.find("a_first.rs").expect("a_first listed");
    let z_pos = out.find("z_last.rs").expect("z_last listed");
    assert!(a_pos < z_pos, "files must be sorted by path:\n{out}");
    assert!(out.contains("alpha") && out.contains("zeta"), "{out}");
}

#[test]
fn directory_outline_is_deterministic() {
    let tmp = tempfile::tempdir().unwrap();
    write(tmp.path(), "a.rs", "pub fn a() {}\n");
    write(tmp.path(), "b.ts", "export function b(): void {}\n");
    let d = tmp.path().to_str().unwrap();
    let (o1, _) = run(d, &OutlineOpts::default());
    let (o2, _) = run(d, &OutlineOpts::default());
    assert_eq!(o1, o2, "directory outline must be byte-stable");
}

#[test]
fn directory_outline_skips_vendor_dirs() {
    let tmp = tempfile::tempdir().unwrap();
    write(tmp.path(), "src/app.rs", "pub fn app() {}\n");
    write(
        tmp.path(),
        "node_modules/lib/index.js",
        "export function vendor() {}\n",
    );
    let (out, _) = run(tmp.path().to_str().unwrap(), &OutlineOpts::default());
    assert!(out.contains("app"), "{out}");
    assert!(
        !out.contains("vendor"),
        "vendor dir must be skipped:\n{out}"
    );
}

#[test]
fn match_filter_narrows_directory_outline() {
    let tmp = tempfile::tempdir().unwrap();
    write(
        tmp.path(),
        "a.rs",
        "pub fn handle_request() {}\npub fn helper() {}\n",
    );
    let opts = OutlineOpts {
        name_match: Some("request"),
        ..Default::default()
    };
    let (out, _) = run(tmp.path().to_str().unwrap(), &opts);
    assert!(out.contains("handle_request"), "{out}");
    assert!(!out.contains("helper"), "{out}");
}

// --- JSON ----------------------------------------------------------------

#[test]
fn file_json_is_valid_and_labels_backend() {
    let tmp = tempfile::tempdir().unwrap();
    write(
        tmp.path(),
        "a.rs",
        "pub fn alpha(x: i32) -> bool { x > 0 }\n",
    );
    let path = tmp.path().join("a.rs");
    let (out, _) = run(
        path.to_str().unwrap(),
        &OutlineOpts {
            as_json: true,
            ..Default::default()
        },
    );
    let v: serde_json::Value = serde_json::from_str(&out).expect("valid json");
    assert_eq!(v["language"], "rust");
    // Default build ships the tree-sitter feature → AST backend, verifiably.
    assert_eq!(v["backend"], "tree-sitter");
    let syms = v["symbols"].as_array().unwrap();
    let alpha = syms.iter().find(|s| s["name"] == "alpha").expect("alpha");
    assert_eq!(alpha["kind"], "fn");
    assert_eq!(alpha["exported"], true);
}

#[test]
fn dir_json_is_deterministic_and_sorted() {
    let tmp = tempfile::tempdir().unwrap();
    write(tmp.path(), "z.rs", "pub fn z() {}\n");
    write(tmp.path(), "a.rs", "pub fn a() {}\n");
    let d = tmp.path().to_str().unwrap();
    let opts = OutlineOpts {
        as_json: true,
        ..Default::default()
    };
    let (o1, _) = run(d, &opts);
    let (o2, _) = run(d, &opts);
    assert_eq!(o1, o2, "json must be byte-stable");
    let v: serde_json::Value = serde_json::from_str(&o1).unwrap();
    assert_eq!(v["version"], 1);
    let files = v["files"].as_array().unwrap();
    assert_eq!(files[0]["path"], "a.rs");
    assert_eq!(files[1]["path"], "z.rs");
}

// --- admission before extraction (G01) -----------------------------------

fn policy(body: &str) -> crate::core::policy::ResolvedPolicy {
    crate::core::policy::load(&format!(
        "name = \"ctx-outline-test\"\nversion = \"1.0.0\"\ndescription = \"test\"\n{body}"
    ))
    .unwrap()
}

/// The old outline read every file raw: a source the project policy blocks
/// was parsed and its symbols listed. Now it contributes nothing and is
/// reported as withheld — in the text listing, in JSON and for a single file.
#[test]
fn a_blocked_source_contributes_no_symbol_and_is_reported_as_withheld() {
    use crate::core::policy::runtime::TestPolicyOverride;
    let _data = crate::core::data_dir::isolated_data_dir();
    let tmp = tempfile::tempdir().unwrap();
    write(tmp.path(), "open.rs", "pub fn visible() {}\n");
    write(
        tmp.path(),
        "plan.rs",
        "CONFIDENTIAL\npub fn hidden_marker() {}\n",
    );
    let dir = tmp.path().to_str().unwrap();
    let json = OutlineOpts {
        as_json: true,
        ..Default::default()
    };
    {
        // Control: without a pack both files are outlined and nothing in the
        // output mentions a withheld source.
        let _community = TestPolicyOverride::set(None);
        let (out, _) = run(dir, &OutlineOpts::default());
        assert!(
            out.contains("visible") && out.contains("hidden_marker"),
            "{out}"
        );
        assert!(!out.contains("withheld"), "{out}");
        let (out, _) = run(dir, &json);
        assert!(!out.contains("withheld"), "{out}");
    }
    let _policy = TestPolicyOverride::set(Some(policy(
        "[filters]\nclassification = \"block\"\nblocked_labels = [\"CONFIDENTIAL\"]\n",
    )));
    let (out, _) = run(dir, &OutlineOpts::default());
    assert!(out.contains("visible"), "{out}");
    assert!(!out.contains("hidden_marker"), "{out}");
    assert!(!out.contains("plan.rs"), "{out}");
    assert!(
        out.contains("[1 file(s) withheld by policy — not outlined]"),
        "{out}"
    );

    let (out, _) = run(dir, &json);
    assert!(!out.contains("hidden_marker"), "{out}");
    let v: serde_json::Value = serde_json::from_str(&out).expect("valid json");
    assert_eq!(v["withheld"], 1);
    assert_eq!(v["files"].as_array().unwrap().len(), 1);

    let plan = tmp.path().join("plan.rs");
    let (out, original) = run(plan.to_str().unwrap(), &OutlineOpts::default());
    assert!(out.starts_with("ERROR: Cannot read"), "{out}");
    assert!(out.contains("withheld"), "{out}");
    assert!(!out.contains("hidden_marker"), "{out}");
    assert_eq!(original, 0);
}

/// Symbols are extracted from the admitted text: a name a project rule masks
/// is not listed under its original spelling (the old raw read listed it).
#[test]
fn symbols_are_extracted_from_the_admitted_text() {
    use crate::core::policy::runtime::TestPolicyOverride;
    let _data = crate::core::data_dir::isolated_data_dir();
    let _policy = TestPolicyOverride::set(Some(policy(
        "[redaction]\ncodename = \"bluefin[0-9]{4}\"\n",
    )));
    let tmp = tempfile::tempdir().unwrap();
    write(
        tmp.path(),
        "a.rs",
        "pub fn launch_bluefin1234() {}\npub fn helper() {}\n",
    );
    let (out, _) = run(tmp.path().to_str().unwrap(), &OutlineOpts::default());
    assert!(out.contains("helper"), "{out}");
    assert!(!out.contains("bluefin1234"), "{out}");
    let path = tmp.path().join("a.rs");
    let (out, _) = run(path.to_str().unwrap(), &OutlineOpts::default());
    assert!(out.contains("helper"), "{out}");
    assert!(!out.contains("bluefin1234"), "{out}");
}
