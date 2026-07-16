//! Unit tests for the graph index. Extracted from `graph_index/mod.rs`;
//! `super::*` resolves to the `graph_index` module.

use super::*;
use tempfile::tempdir;

#[test]
fn marker_in_ancestry_found_at_repo_root() {
    let tmp = tempdir().unwrap();
    let stop = tmp.path().join("Documents");
    let repo = stop.join("Projects").join("myrepo");
    let sub = repo.join("rust").join("src");
    std::fs::create_dir_all(&sub).unwrap();
    std::fs::create_dir(repo.join(".git")).unwrap();

    // repo/rust/src is a legit scan root: .git lives two levels up (GL#438).
    assert!(has_marker_in_ancestry(&sub, &stop));
    assert!(has_marker_in_ancestry(&repo, &stop));
}

#[test]
fn marker_in_ancestry_stops_at_boundary() {
    let tmp = tempdir().unwrap();
    // Marker at the *stop* dir itself must NOT count: a marker-less
    // ~/Documents tree stays refused even if ~/Documents has a stray .git.
    let stop = tmp.path().join("Documents");
    let sub = stop.join("no-project").join("deep");
    std::fs::create_dir_all(&sub).unwrap();
    std::fs::create_dir(stop.join(".git")).unwrap();

    assert!(!has_marker_in_ancestry(&sub, &stop));
}

#[test]
fn marker_in_ancestry_none_without_markers() {
    let tmp = tempdir().unwrap();
    let stop = tmp.path().join("Documents");
    let sub = stop.join("a").join("b");
    std::fs::create_dir_all(&sub).unwrap();

    assert!(!has_marker_in_ancestry(&sub, &stop));
}

#[test]
fn dir_marker_detects_each_project_type() {
    for marker in ["Cargo.toml", "package.json", "go.mod", "pyproject.toml"] {
        let tmp = tempdir().unwrap();
        assert!(!dir_has_project_marker(tmp.path()), "{marker}: empty dir");
        std::fs::write(tmp.path().join(marker), "x").unwrap();
        assert!(dir_has_project_marker(tmp.path()), "{marker}: present");
    }
}

#[test]
fn test_short_hash_deterministic() {
    let h1 = short_hash("/Users/test/project");
    let h2 = short_hash("/Users/test/project");
    assert_eq!(h1, h2);
    assert_eq!(h1.len(), 8);
}

#[test]
fn test_make_relative() {
    assert_eq!(
        make_relative("/foo/bar/src/main.rs", "/foo/bar"),
        graph_relative_key("/foo/bar/src/main.rs", "/foo/bar")
    );
    assert_eq!(
        make_relative("src/main.rs", "/foo/bar"),
        graph_relative_key("src/main.rs", "/foo/bar")
    );
    assert_eq!(
        make_relative("C:\\repo\\src\\main\\kotlin\\Example.kt", "C:\\repo"),
        graph_relative_key("C:\\repo\\src\\main\\kotlin\\Example.kt", "C:\\repo")
    );
    assert_eq!(
        make_relative("//?/C:/repo/src/main/kotlin/Example.kt", "//?/C:/repo"),
        graph_relative_key("//?/C:/repo/src/main/kotlin/Example.kt", "//?/C:/repo")
    );
}

#[test]
fn test_normalize_project_root() {
    assert_eq!(normalize_project_root("C:\\repo\\"), "C:\\repo");
    assert_eq!(normalize_project_root("C:\\repo\\."), "C:\\repo");
    assert_eq!(normalize_project_root("//?/C:/repo/"), "//?/C:/repo");
}

#[test]
fn test_graph_match_key_normalizes_windows_forms() {
    assert_eq!(
        graph_match_key(r"C:\repo\src\main.rs"),
        "C:/repo/src/main.rs"
    );
    assert_eq!(
        graph_match_key(r"\\?\C:\repo\src\main.rs"),
        "C:/repo/src/main.rs"
    );
    assert_eq!(graph_match_key(r"\src\main.rs"), "src/main.rs");
}

#[test]
fn test_extract_summary() {
    let content = "// comment\nuse std::io;\n\npub fn main() {\n    println!(\"hello\");\n}";
    let summary = extract_summary(content);
    assert_eq!(summary, "pub fn main() {");
}

#[test]
fn test_compute_hash_deterministic() {
    let h1 = compute_hash("hello world");
    let h2 = compute_hash("hello world");
    assert_eq!(h1, h2);
    assert_ne!(h1, compute_hash("hello world!"));
}

#[test]
fn test_project_index_new() {
    let idx = ProjectIndex::new("/test");
    assert_eq!(idx.version, INDEX_VERSION);
    assert_eq!(idx.project_root, "/test");
    assert!(idx.files.is_empty());
}

fn fe(path: &str, content: &str, language: &str) -> FileEntry {
    FileEntry {
        path: path.to_string(),
        hash: compute_hash(content),
        language: language.to_string(),
        line_count: content.lines().count(),
        token_count: crate::core::tokens::count_tokens(content),
        exports: Vec::new(),
        summary: extract_summary(content),
    }
}

#[test]
fn test_index_looks_stale_when_any_file_missing() {
    let td = tempdir().expect("tempdir");
    let root = td.path();
    std::fs::write(root.join("a.rs"), "pub fn a() {}\n").expect("write a.rs");

    let root_s = normalize_project_root(&root.to_string_lossy());
    let mut idx = ProjectIndex::new(&root_s);
    idx.files
        .insert("a.rs".to_string(), fe("a.rs", "pub fn a() {}\n", "rs"));
    idx.files.insert(
        "missing.rs".to_string(),
        fe("missing.rs", "pub fn m() {}\n", "rs"),
    );

    assert!(index_looks_stale(&idx, &root_s));
}

#[test]
fn test_index_looks_fresh_when_all_files_exist() {
    let td = tempdir().expect("tempdir");
    let root = td.path();
    std::fs::write(root.join("a.rs"), "pub fn a() {}\n").expect("write a.rs");

    let root_s = normalize_project_root(&root.to_string_lossy());
    let mut idx = ProjectIndex::new(&root_s);
    idx.files
        .insert("a.rs".to_string(), fe("a.rs", "pub fn a() {}\n", "rs"));

    assert!(!index_looks_stale(&idx, &root_s));
}

#[test]
fn test_reverse_deps() {
    let mut idx = ProjectIndex::new("/test");
    idx.edges.push(IndexEdge {
        from: "a.rs".to_string(),
        to: "b.rs".to_string(),
        kind: "import".to_string(),
        weight: 1.0,
    });
    idx.edges.push(IndexEdge {
        from: "c.rs".to_string(),
        to: "b.rs".to_string(),
        kind: "import".to_string(),
        weight: 1.0,
    });

    let deps = idx.get_reverse_deps("b.rs", 1);
    assert_eq!(deps.len(), 2);
    assert!(deps.contains(&"a.rs".to_string()));
    assert!(deps.contains(&"c.rs".to_string()));
}

#[test]
fn get_forward_deps_follows_import_edges_outward() {
    let mut idx = ProjectIndex::new("/tmp/fwd");
    idx.edges.push(IndexEdge {
        from: "a.rs".into(),
        to: "b.rs".into(),
        kind: "import".into(),
        weight: 1.0,
    });
    idx.edges.push(IndexEdge {
        from: "b.rs".into(),
        to: "c.rs".into(),
        kind: "import".into(),
        weight: 1.0,
    });
    let deps = idx.get_forward_deps("a.rs", 2);
    assert!(deps.contains(&"b.rs".to_string()), "got: {deps:?}");
    assert!(deps.contains(&"c.rs".to_string()), "got: {deps:?}");
    // reverse direction must NOT appear
    assert!(
        idx.get_forward_deps("c.rs", 2).is_empty(),
        "leaf has no forward deps"
    );
}

#[test]
fn test_find_symbol_range_kotlin_function() {
    let content = r#"
package com.example

class UserService {
    fun greet(name: String): String {
        return "hi $name"
    }
}
"#;
    let sig = signatures::Signature {
        kind: "method",
        name: "greet".to_string(),
        params: "name:String".to_string(),
        return_type: "String".to_string(),
        is_async: false,
        is_exported: true,
        indent: 2,
        ..signatures::Signature::no_span()
    };
    let (start, end) = find_symbol_range(content, &sig);
    assert_eq!(start, 5);
    assert!(end >= start);
}

#[test]
fn test_signature_spans_override_fallback_range() {
    let sig = signatures::Signature {
        kind: "method",
        name: "release".to_string(),
        params: "id:String".to_string(),
        return_type: "Boolean".to_string(),
        is_async: true,
        is_exported: true,
        indent: 2,
        start_line: Some(42),
        end_line: Some(43),
    };

    let (start, end) = sig
        .start_line
        .zip(sig.end_line)
        .unwrap_or_else(|| find_symbol_range("ignored", &sig));
    assert_eq!((start, end), (42, 43));
}

#[test]
fn test_parse_stale_index_version() {
    let json = format!(
        r#"{{"version":{},"project_root":"/test","last_scan":"now","files":{{}},"edges":[],"symbols":{{}}}}"#,
        INDEX_VERSION - 1
    );
    let parsed: ProjectIndex = serde_json::from_str(&json).unwrap();
    assert_ne!(parsed.version, INDEX_VERSION);
}

#[test]
fn test_kotlin_package_name() {
    let content = "package com.example.feature\n\nclass UserService";
    assert_eq!(
        kotlin_package_name(content).as_deref(),
        Some("com.example.feature")
    );
}

#[test]
fn safe_scan_root_rejects_fs_root() {
    assert!(!is_safe_scan_root("/"));
    assert!(!is_safe_scan_root("\\"));
    #[cfg(windows)]
    {
        assert!(!is_safe_scan_root("C:\\"));
        assert!(!is_safe_scan_root("D:\\"));
    }
}

#[test]
fn safe_scan_root_rejects_home() {
    if let Some(home) = dirs::home_dir() {
        let home_str = home.to_string_lossy().to_string();
        assert!(
            !is_safe_scan_root(&home_str),
            "home dir should be rejected: {home_str}"
        );
    }
}

#[test]
fn safe_scan_root_accepts_project_dir() {
    let tmp = tempdir().unwrap();
    std::fs::write(
        tmp.path().join("Cargo.toml"),
        "[package]\nname = \"test\"\n",
    )
    .unwrap();
    let root = tmp.path().to_string_lossy().to_string();
    assert!(is_safe_scan_root(&root));
}

#[test]
fn safe_scan_root_rejects_broad_dir() {
    let tmp = tempdir().unwrap();
    for i in 0..55 {
        std::fs::create_dir(tmp.path().join(format!("dir{i}"))).unwrap();
    }
    let root = tmp.path().to_string_lossy().to_string();
    assert!(!is_safe_scan_root(&root));
}

#[test]
fn no_index_env_skips_scan() {
    let _env = crate::core::data_dir::test_env_lock();
    let tmp = tempdir().unwrap();
    std::fs::write(tmp.path().join("Cargo.toml"), "").unwrap();
    std::fs::write(tmp.path().join("main.rs"), "fn main() {}").unwrap();

    crate::test_env::set_var("LEAN_CTX_NO_INDEX", "1");
    let idx = scan(&tmp.path().to_string_lossy());
    crate::test_env::remove_var("LEAN_CTX_NO_INDEX");
    assert!(idx.files.is_empty(), "LEAN_CTX_NO_INDEX should skip scan");
}

#[test]
fn stale_index_detected_by_contamination() {
    let root_s = "/home/testuser/myproject";
    let mut idx = ProjectIndex::new(root_s);
    // Simulate a contaminated index with Desktop files
    idx.files.insert(
        "Desktop/random.py".to_string(),
        fe("Desktop/random.py", "x = 1\n", "py"),
    );
    idx.files.insert(
        "src/main.rs".to_string(),
        fe("src/main.rs", "fn main() {}\n", "rs"),
    );
    assert!(
        index_looks_stale(&idx, root_s),
        "Index with Desktop/ files should be considered stale"
    );
}

#[test]
fn stale_index_detected_by_age() {
    let td = tempdir().expect("tempdir");
    let root = td.path();
    std::fs::write(root.join("a.rs"), "fn a() {}\n").unwrap();

    let root_s = normalize_project_root(&root.to_string_lossy());
    let mut idx = ProjectIndex::new(&root_s);
    idx.files
        .insert("a.rs".to_string(), fe("a.rs", "fn a() {}\n", "rs"));
    // Set last_scan to 100 hours ago (default max_age_hours is 48)
    let old_time = chrono::Local::now().naive_local() - chrono::Duration::hours(100);
    idx.last_scan = old_time.format("%Y-%m-%d %H:%M:%S").to_string();

    assert!(
        index_looks_stale(&idx, &root_s),
        "Index older than max_age_hours should be stale"
    );
}

#[test]
fn content_aware_staleness_detects_edits_and_additions() {
    let _env = crate::core::data_dir::test_env_lock();
    let td = tempdir().expect("tempdir");
    std::fs::write(
        td.path().join("Cargo.toml"),
        "[package]\nname = \"t\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    std::fs::write(td.path().join("a.rs"), "fn a() {}\n").unwrap();
    let root_s = normalize_project_root(&td.path().to_string_lossy());

    // Build + persist the index, then it must look fresh.
    let idx = scan(&root_s);
    assert!(!idx.files.is_empty(), "scan should index a.rs");
    assert!(
        !index_looks_stale(&idx, &root_s),
        "a just-built index must be fresh"
    );

    // mtime resolution can be coarse; ensure the next writes are strictly newer.
    std::thread::sleep(std::time::Duration::from_millis(1100));

    // Edit detection.
    std::fs::write(td.path().join("a.rs"), "fn a() { let _x = 1; }\n").unwrap();
    assert!(
        index_looks_stale(&idx, &root_s),
        "an edited source file must mark the index stale"
    );

    // Addition detection.
    std::fs::write(td.path().join("b.rs"), "fn b() {}\n").unwrap();
    assert!(
        index_looks_stale(&idx, &root_s),
        "a new source file must mark the index stale"
    );
}

#[test]
fn touch_without_content_change_keeps_index_fresh() {
    // #324: an mtime bump with identical bytes (touch / git checkout / no-op
    // format) must NOT trigger a rescan — only a real content change does.
    let _env = crate::core::data_dir::test_env_lock();
    let td = tempdir().expect("tempdir");
    std::fs::write(
        td.path().join("Cargo.toml"),
        "[package]\nname = \"t\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    std::fs::write(td.path().join("a.rs"), "fn a() {}\n").unwrap();
    let root_s = normalize_project_root(&td.path().to_string_lossy());

    let idx = scan(&root_s);
    assert!(!idx.files.is_empty(), "scan should index a.rs");
    assert!(
        !index_looks_stale(&idx, &root_s),
        "a just-built index must be fresh"
    );

    // mtime resolution can be coarse; ensure the rewrite is strictly newer.
    std::thread::sleep(std::time::Duration::from_millis(1100));

    // Rewrite identical bytes: mtime advances but the content hash is unchanged.
    std::fs::write(td.path().join("a.rs"), "fn a() {}\n").unwrap();
    assert!(
        !index_looks_stale(&idx, &root_s),
        "a content-identical rewrite must NOT mark the index stale"
    );
}

#[test]
fn safe_scan_root_rejects_home_downloads() {
    if let Some(home) = dirs::home_dir() {
        let downloads = home.join("Downloads");
        // Only test if Downloads doesn't contain a .git (unlikely but possible)
        if !downloads.join(".git").exists() {
            let downloads_str = downloads.to_string_lossy().to_string();
            assert!(
                !is_safe_scan_root(&downloads_str),
                "~/Downloads should be rejected without project markers"
            );
        }
    }
}

#[test]
fn safe_scan_root_rejects_cloud_sync_roots() {
    // ~/OneDrive (and friends) must never be a scan root: walking them forces
    // OneDrive/Dropbox/Drive to hydrate every on-demand placeholder (#363).
    if let Some(home) = dirs::home_dir() {
        for dir in ["OneDrive", "Dropbox", "Google Drive"] {
            let cloud = home.join(dir);
            if cloud.join(".git").exists() {
                continue; // a real repo there legitimately overrides the block
            }
            let cloud_str = cloud.to_string_lossy().to_string();
            assert!(
                !is_safe_scan_root(&cloud_str),
                "~/{dir} should be rejected as a scan root"
            );
        }
    }
}

#[test]
#[cfg(target_os = "macos")]
#[serial_test::serial]
fn safe_scan_root_refused_for_standalone_under_documents() {
    // #356: a launchd-standalone process (daemon/proxy, ppid 1) must refuse to
    // scan *any* path under ~/Documents — including a real nested project —
    // before normalize/marker-probe/read_dir touches the filesystem. Editor- and
    // CLI-attached processes (covered by the other tests) keep indexing them.
    let Some(home) = dirs::home_dir() else {
        return;
    };
    let doc_proj = home.join("Documents/some-project");
    let doc_proj_str = doc_proj.to_string_lossy().to_string();

    crate::test_env::set_var("LEAN_CTX_TCC_STANDALONE", "1");
    assert!(
        !is_safe_scan_root(&doc_proj_str),
        "standalone process must refuse ~/Documents scan roots"
    );
    assert!(!is_safe_scan_root_public(&doc_proj_str));
    crate::test_env::remove_var("LEAN_CTX_TCC_STANDALONE");
}

#[test]
fn safe_scan_root_accepts_multi_repo_parent() {
    let tmp = tempdir().unwrap();
    let parent = tmp.path().join("code");
    std::fs::create_dir_all(&parent).unwrap();

    // Create 2 child repos
    std::fs::create_dir_all(parent.join("repo-a").join(".git")).unwrap();
    std::fs::create_dir_all(parent.join("repo-b").join(".git")).unwrap();

    // Add >50 empty subdirs to trigger the breadth guard
    for i in 0..55 {
        std::fs::create_dir(parent.join(format!("dir-{i}"))).unwrap();
    }

    let parent_str = parent.to_string_lossy().to_string();
    assert!(
        is_safe_scan_root(&parent_str),
        "Multi-repo parent with >50 subdirs should be accepted"
    );
}

#[test]
fn csharp_graph_edges_end_to_end() {
    // Full edge pipeline for a small C# project: `using` resolution (import
    // edges) + same-namespace type usage (type_ref edges, GH #398). Regression
    // for the empty C# Call Graph / sparse graph report (NINA) and the false
    // "leaf node" blast radius that the old alphabetical namespace chain caused.
    const USER_SERVICE: &str = "namespace App.Services;\n\
using App.Data;\n\
\n\
public class UserService\n{\n    \
private readonly OrderRepository _repo = new OrderRepository();\n    \
public void Save() { _repo.Persist(); }\n}\n";
    // Consumes a same-namespace type with no `using` and no `new` — exactly the
    // dependency that only a type_ref edge can capture.
    const ORDER_SERVICE: &str = "namespace App.Services;\n\
\n\
public class OrderService\n{\n    \
private readonly UserService _users;\n    \
public void Process() {}\n}\n";
    const ORDER_REPO: &str = "namespace App.Data;\n\
\n\
public class OrderRepository { public void Persist() {} }\n";

    let files = [
        ("src/App/Services/UserService.cs", USER_SERVICE),
        ("src/App/Services/OrderService.cs", ORDER_SERVICE),
        ("src/App/Data/OrderRepository.cs", ORDER_REPO),
    ];

    let mut index = ProjectIndex::new("/proj-does-not-need-to-exist");
    let mut cache: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    for (path, content) in files {
        index
            .files
            .insert(path.to_string(), fe(path, content, "cs"));
        cache.insert(path.to_string(), content.to_string());
    }

    build_edges_cached(&mut index, &cache);

    // `using App.Data` resolves to the representative file of that namespace.
    assert!(
        index.edges.iter().any(|e| e.kind == "import"
            && e.from == "src/App/Services/UserService.cs"
            && e.to == "src/App/Data/OrderRepository.cs"),
        "expected a C# `using` import edge, got {:?}",
        index.edges
    );

    // Same-namespace type usage without `using` yields a precise type_ref edge
    // (consumer -> definer), the durable signal a background reindex preserves.
    assert!(
        index.edges.iter().any(|e| e.kind == "type_ref"
            && e.from == "src/App/Services/OrderService.cs"
            && e.to == "src/App/Services/UserService.cs"),
        "expected a C# same-namespace type_ref edge, got {:?}",
        index.edges
    );
}

/// GH #398 regression — the durable mirror must carry the C# same-namespace
/// blast radius. The bug every previous fix missed: `ctx_impact` wrote precise
/// `type_ref` edges into the PropertyGraph, but every `ProjectIndex::save()`
/// (daemon reindex, dashboard, `ctx_graph`) mirrors graph_index over it via
/// `clear_code_graph()` — and graph_index emitted none, silently wiping the
/// blast radius and leaving the consumer a false-negative leaf. With type_ref
/// edges now produced here, the mirrored graph keeps the consumer reachable.
#[cfg(feature = "tree-sitter")]
#[test]
fn csharp_same_namespace_type_ref_survives_mirror() {
    use crate::core::property_graph::{CodeGraph, populate_from_project_index};

    const ENGINE: &str = "namespace App.Core;\n\npublic class Engine { public int Power; }\n";
    // DI-style consumer: field + ctor param, no `using`, never `new Engine()`.
    const MOTOR: &str = "namespace App.Core;\n\
\n\
public class Motor\n{\n    \
private readonly Engine _engine;\n    \
public Motor(Engine engine) { _engine = engine; }\n}\n";
    // Unrelated same-namespace file in its own directory: it neither uses Engine
    // nor shares a directory with a connected file, so the directory-local
    // "sibling" orphan-rescue heuristic cannot pull it in either — its absence
    // proves the blast radius comes from the precise type_ref edge, not noise.
    const LOGGER: &str = "namespace App.Core;\n\npublic class Logger { public void Log() {} }\n";

    let files = [
        ("Models/Engine.cs", ENGINE),
        ("Services/Motor.cs", MOTOR),
        ("Utils/Logger.cs", LOGGER),
    ];
    let mut index = ProjectIndex::new("/proj-mirror-398");
    let mut cache: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    for (path, content) in files {
        index
            .files
            .insert(path.to_string(), fe(path, content, "cs"));
        cache.insert(path.to_string(), content.to_string());
    }
    build_edges_cached(&mut index, &cache);

    // Mirror exactly as the daemon/dashboard reindex does.
    let pg = CodeGraph::open_in_memory().expect("in-memory graph");
    populate_from_project_index(&pg, &index).expect("mirror index");

    let impact = pg
        .impact_analysis("Models/Engine.cs", 5)
        .expect("impact analysis");
    assert!(
        impact
            .affected_files
            .contains(&"Services/Motor.cs".to_string()),
        "DI consumer must survive the graph_index mirror; got {:?}",
        impact.affected_files
    );
    assert!(
        !impact
            .affected_files
            .contains(&"Utils/Logger.cs".to_string()),
        "unrelated same-namespace file must NOT be in the blast radius; got {:?}",
        impact.affected_files
    );
}

/// GH #398 bug class (Go): the durable mirror must carry a *directory-scoped*
/// same-package `type_ref` edge — Go files in one package (== one directory)
/// use each other's types with no import. The decisive correctness property:
/// a homonymous type in another directory (another package) must NOT be linked,
/// which is exactly what a namespace-free global fallback would get wrong for a
/// common type name.
#[cfg(feature = "tree-sitter")]
#[test]
fn go_same_package_type_ref_edge_is_directory_scoped() {
    const ENGINE: &str = "package core\n\ntype Engine struct {\n\tPower int\n}\n";
    const MOTOR: &str = "package core\n\ntype Motor struct {\n\tengine Engine\n}\n";
    // Different directory => different package: the same simple name must stay
    // unlinked (strict Go scope, no cross-package leak).
    const OTHER_ENGINE: &str = "package other\n\ntype Engine struct{}\n";

    let files = [
        ("core/engine.go", ENGINE),
        ("core/motor.go", MOTOR),
        ("other/engine.go", OTHER_ENGINE),
    ];
    let mut index = ProjectIndex::new("/proj-go-398");
    let mut cache: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    for (path, content) in files {
        index
            .files
            .insert(path.to_string(), fe(path, content, "go"));
        cache.insert(path.to_string(), content.to_string());
    }
    build_edges_cached(&mut index, &cache);

    assert!(
        index
            .edges
            .iter()
            .any(|e| e.kind == "type_ref" && e.from == "core/motor.go" && e.to == "core/engine.go"),
        "expected a Go same-package type_ref edge, got {:?}",
        index.edges
    );
    assert!(
        !index
            .edges
            .iter()
            .any(|e| e.kind == "type_ref" && e.to == "other/engine.go"),
        "cross-package homonym must NOT be linked (Go strict scope), got {:?}",
        index.edges
    );
}

#[test]
fn csharp_using_resolves_declared_namespace_not_matching_folder() {
    // The real-world fix: namespaces that do NOT mirror the folder layout.
    // `Foo.cs` lives in `src/` but declares `namespace Acme.Core`; `Bar.cs` lives
    // in `lib/` but declares `namespace Acme.Data`. Folder-suffix matching alone
    // cannot link them — only the *declared* namespace can.
    const FOO: &str = "namespace Acme.Core;\n\
using Acme.Data;\n\
public class Foo { private readonly Bar _b = new Bar(); }\n";
    const BAR: &str = "namespace Acme.Data;\n\
public class Bar { }\n";

    let files = [("src/Foo.cs", FOO), ("lib/Bar.cs", BAR)];
    let mut index = ProjectIndex::new("/proj-x");
    let mut cache: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    for (path, content) in files {
        index
            .files
            .insert(path.to_string(), fe(path, content, "cs"));
        cache.insert(path.to_string(), content.to_string());
    }

    build_edges_cached(&mut index, &cache);

    assert!(
        index
            .edges
            .iter()
            .any(|e| e.kind == "import" && e.from == "src/Foo.cs" && e.to == "lib/Bar.cs"),
        "`using Acme.Data` must resolve via the declared namespace (folder != namespace), got {:?}",
        index.edges
    );
}

#[test]
fn safe_scan_root_accepts_dotnet_project() {
    // A `*.csproj` at the root must mark a valid scan root even with many
    // subdirectories that would otherwise be rejected as a broad directory.
    let tmp = tempdir().unwrap();
    std::fs::write(tmp.path().join("MyApp.csproj"), "<Project></Project>\n").unwrap();
    for i in 0..55 {
        std::fs::create_dir(tmp.path().join(format!("dir{i}"))).unwrap();
    }
    let root = tmp.path().to_string_lossy().to_string();
    assert!(
        is_safe_scan_root(&root),
        "a .csproj should mark a valid .NET scan root"
    );
}

#[test]
fn gdscript_scene_edges_end_to_end() {
    // #315: `preload/load("res://…tscn")` yields import edges even though the
    // `.tscn` isn't indexed yet, `extends "res://…gd"` resolves to the base
    // script, and `graph related <scene>` finds the importing script.
    const MAIN: &str = "extends Node\n\n\
const Enemy = preload(\"res://scenes/Enemy.tscn\")\n\n\
func _ready():\n\tvar level = load(\"res://scenes/Main.tscn\")\n";
    const PLAYER: &str = "extends \"res://actors/Base.gd\"\n\nfunc _ready():\n\tpass\n";
    const BASE: &str = "extends Node\n\nfunc _ready():\n\tpass\n";

    let files = [
        ("main.gd", MAIN),
        ("actors/Player.gd", PLAYER),
        ("actors/Base.gd", BASE),
    ];

    let mut index = ProjectIndex::new("/godot-proj");
    let mut cache: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    for (path, content) in files {
        index
            .files
            .insert(path.to_string(), fe(path, content, "gd"));
        cache.insert(path.to_string(), content.to_string());
    }

    build_edges_cached(&mut index, &cache);

    // AC1: preload of an unindexed `.tscn` still produces an import edge.
    assert!(
        index
            .edges
            .iter()
            .any(|e| e.kind == "import" && e.from == "main.gd" && e.to == "scenes/Enemy.tscn"),
        "expected preload(.tscn) import edge, got {:?}",
        index.edges
    );

    // `extends "res://actors/Base.gd"` resolves to the indexed base script.
    assert!(
        index.edges.iter().any(|e| e.kind == "import"
            && e.from == "actors/Player.gd"
            && e.to == "actors/Base.gd"),
        "expected extends import edge, got {:?}",
        index.edges
    );

    // AC2: `graph related scenes/Main.tscn` surfaces the importing script.
    let related = index.get_related("scenes/Main.tscn", 2);
    assert!(
        related.contains(&"main.gd".to_string()),
        "graph related <scene> should surface the importer, got {related:?}"
    );
}

#[test]
fn lua_require_edges_end_to_end() {
    // #360: a Lua project is no longer an empty graph — `require("a.b")` produces
    // an import edge to `a/b.lua`, `require("pkg")` resolves to `pkg/init.lua`,
    // and `graph related` surfaces the importer.
    const MAIN: &str = "local util = require(\"lib.util\")\n\
local pkg = require(\"pkg\")\n\n\
local function run()\n\treturn util.add(1, 2)\nend\n";
    const UTIL: &str = "local M = {}\n\nfunction M.add(a, b)\n\treturn a + b\nend\n\nreturn M\n";
    const PKG: &str = "return { version = 1 }\n";

    let files = [
        ("main.lua", MAIN, "lua"),
        ("lib/util.lua", UTIL, "lua"),
        ("pkg/init.lua", PKG, "lua"),
    ];

    let mut index = ProjectIndex::new("/lua-proj");
    let mut cache: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    for (path, content, lang) in files {
        index
            .files
            .insert(path.to_string(), fe(path, content, lang));
        cache.insert(path.to_string(), content.to_string());
    }

    build_edges_cached(&mut index, &cache);

    // Dotted `require` maps to a project file.
    assert!(
        index
            .edges
            .iter()
            .any(|e| e.kind == "import" && e.from == "main.lua" && e.to == "lib/util.lua"),
        "expected require('lib.util') edge, got {:?}",
        index.edges
    );
    // Package `require` falls back to `pkg/init.lua`.
    assert!(
        index
            .edges
            .iter()
            .any(|e| e.kind == "import" && e.from == "main.lua" && e.to == "pkg/init.lua"),
        "expected require('pkg') -> init.lua edge, got {:?}",
        index.edges
    );

    // `graph related lib/util.lua` surfaces the importer.
    let related = index.get_related("lib/util.lua", 2);
    assert!(
        related.contains(&"main.lua".to_string()),
        "graph related should surface the importer, got {related:?}"
    );
}

#[test]
fn tscn_scene_indexed_with_script_edges() {
    // #316: a real on-disk Godot project. The `.tscn` scene is indexed as a
    // graph node, its `[ext_resource]` script becomes a Scene→Script import
    // edge, and GDScript member symbols (`@export var`) surface in the graph.
    // Acquire the env lock (scan reads LEAN_CTX_DATA_DIR) but do not mutate it,
    // so we never race data-dir-sensitive tests.
    let _env = crate::core::data_dir::test_env_lock();
    let td = tempdir().expect("tempdir");
    let root = td.path();

    // `project.godot` is the sole project marker → also exercises detection.
    std::fs::write(root.join("project.godot"), "config_version=5\n").unwrap();
    std::fs::create_dir_all(root.join("actors")).unwrap();
    std::fs::create_dir_all(root.join("scenes")).unwrap();
    std::fs::write(
        root.join("actors/Player.gd"),
        "extends CharacterBody2D\n\n@export var speed: float = 200.0\n\nfunc _ready():\n\tpass\n",
    )
    .unwrap();
    std::fs::write(
        root.join("scenes/Main.tscn"),
        "[gd_scene load_steps=2 format=3]\n\n\
         [ext_resource type=\"Script\" path=\"res://actors/Player.gd\" id=\"1_p\"]\n\n\
         [node name=\"Main\" type=\"Node2D\"]\n\
         script = ExtResource(\"1_p\")\n",
    )
    .unwrap();

    let root_s = normalize_project_root(&root.to_string_lossy());
    let idx = scan(&root_s);

    // AC1: the `.tscn` scene is indexed as a graph node.
    assert!(
        idx.files.contains_key("scenes/Main.tscn"),
        "scene must be indexed; files: {:?}",
        idx.files.keys().collect::<Vec<_>>()
    );

    // AC1/AC2: Scene→Script import edge from the scene to its attached script.
    assert!(
        idx.edges.iter().any(|e| e.kind == "import"
            && e.from == "scenes/Main.tscn"
            && e.to == "actors/Player.gd"),
        "expected Scene→Script edge, got {:?}",
        idx.edges
    );

    // AC2: GDScript `@export var` member symbol surfaces in the graph.
    assert!(
        idx.symbols.values().any(|s| s.name == "speed"),
        "expected @export member symbol `speed` in the graph"
    );
}

#[test]
fn scan_skips_node_modules_without_git_dir() {
    // #400: a monorepo without a top-level `.git` used to index node_modules
    // wholesale, because the ignore crate only applies `.gitignore` files
    // inside git repositories. The vendor-dir walk filter must skip them
    // regardless of git state.
    let _env = crate::core::data_dir::test_env_lock();
    let td = tempdir().expect("tempdir");
    let root = td.path();

    std::fs::create_dir_all(root.join("api/node_modules/lodash")).unwrap();
    std::fs::create_dir_all(root.join("api/src")).unwrap();
    std::fs::write(root.join("api/package.json"), "{\"name\":\"api\"}").unwrap();
    std::fs::write(
        root.join("api/node_modules/lodash/index.js"),
        "module.exports = 1;",
    )
    .unwrap();
    std::fs::write(root.join("api/src/server.js"), "const x = 1;").unwrap();

    let root_s = normalize_project_root(&root.to_string_lossy());
    let idx = scan(&root_s);

    assert!(
        idx.files.keys().any(|p| p.contains("server.js")),
        "real source must be indexed; files: {:?}",
        idx.files.keys().collect::<Vec<_>>()
    );
    assert!(
        !idx.files.keys().any(|p| p.contains("node_modules")),
        "node_modules must never be indexed; files: {:?}",
        idx.files.keys().collect::<Vec<_>>()
    );
}

#[test]
fn scan_respects_gitignore_without_git_dir() {
    // #400 second half: `require_git(false)` makes `.gitignore` files
    // effective even when no `.git` directory exists.
    let _env = crate::core::data_dir::test_env_lock();
    let td = tempdir().expect("tempdir");
    let root = td.path();

    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::create_dir_all(root.join("generated")).unwrap();
    std::fs::write(root.join(".gitignore"), "generated/\n").unwrap();
    std::fs::write(root.join("src/app.js"), "const y = 2;").unwrap();
    std::fs::write(root.join("generated/bundle.js"), "var b = 3;").unwrap();

    let root_s = normalize_project_root(&root.to_string_lossy());
    let idx = scan(&root_s);

    assert!(
        idx.files.keys().any(|p| p.contains("app.js")),
        "real source must be indexed; files: {:?}",
        idx.files.keys().collect::<Vec<_>>()
    );
    assert!(
        !idx.files.keys().any(|p| p.contains("bundle.js")),
        "gitignored dir must be skipped without .git; files: {:?}",
        idx.files.keys().collect::<Vec<_>>()
    );
}

#[test]
fn safe_scan_root_rejects_broad_dir_without_repos() {
    let tmp = tempdir().unwrap();
    let broad = tmp.path().join("broad");
    std::fs::create_dir_all(&broad).unwrap();

    // Create >50 subdirs but no project markers
    for i in 0..55 {
        std::fs::create_dir(broad.join(format!("dir-{i}"))).unwrap();
    }

    let broad_str = broad.to_string_lossy().to_string();
    assert!(
        !is_safe_scan_root(&broad_str),
        "Broad dir without project markers should be rejected"
    );
}

// ---- #934: parallel scan determinism ----

/// Write a varied multi-language corpus and return the sorted scan targets
/// `(absolute_path, rel, ext)`.
fn write_scan_corpus(root: &std::path::Path, n: usize) -> Vec<(String, String, String)> {
    std::fs::create_dir_all(root.join("src")).expect("mkdir src");
    let mut targets: Vec<(String, String, String)> = Vec::new();
    for i in 0..n {
        let rel = format!("src/m{i:02}.rs");
        let body = format!(
            "pub fn handler_{i}(x: u32) -> u32 {{ x + {i} }}\n\
             pub struct State{i} {{ n: usize }}\n\
             impl State{i} {{ pub fn run(&self) {{}} }}\n\
             fn helper() {{}}\n"
        );
        std::fs::write(root.join(&rel), &body).expect("write rs");
        targets.push((
            root.join(&rel).to_string_lossy().to_string(),
            rel.clone(),
            "rs".to_string(),
        ));
    }
    let ts = "src/api.ts".to_string();
    std::fs::write(
        root.join(&ts),
        "export function getUser(id: string) { return id; }\nexport class Client {}\n",
    )
    .expect("write ts");
    targets.push((
        root.join(&ts).to_string_lossy().to_string(),
        ts,
        "ts".to_string(),
    ));

    let py = "src/util.py".to_string();
    std::fs::write(root.join(&py), "def compute(a, b):\n    return a + b\n").expect("write py");
    targets.push((
        root.join(&py).to_string_lossy().to_string(),
        py,
        "py".to_string(),
    ));

    targets.sort();
    targets
}

#[test]
fn scan_targets_parallel_matches_sequential() {
    let td = tempdir().expect("tempdir");
    let targets = write_scan_corpus(td.path(), 40);
    let previous = PreviousSymbols::new();

    let seq = process_scan_targets(&targets, &previous, None, false);
    let par = process_scan_targets(&targets, &previous, None, true);

    assert_eq!(par.len(), targets.len(), "all files processed");
    assert!(par.iter().all(|r| !r.reused), "cold scan parses every file");
    assert!(
        par.iter().any(|r| r.symbols.len() >= 3),
        "multi-symbol files expected"
    );
    assert_eq!(seq, par, "parallel scan must equal sequential scan");
}

#[test]
fn scan_targets_parallel_reuse_path_matches_sequential() {
    let td = tempdir().expect("tempdir");
    let targets = write_scan_corpus(td.path(), 36);

    // Cold scan, then synthesize the prior-index state it would have produced.
    let cold = process_scan_targets(&targets, &PreviousSymbols::new(), None, false);
    let mut existing = ProjectIndex::new(&td.path().to_string_lossy());
    for r in &cold {
        existing.files.insert(r.rel.clone(), r.file_entry.clone());
        for (k, s) in &r.symbols {
            existing.symbols.insert(k.clone(), s.clone());
        }
    }

    // Unchanged files must be reused identically on both paths without cloning
    // the complete previous symbol table.
    let previous = previous_symbols_by_file(&existing);
    let seq = process_scan_targets(&targets, &previous, Some(&existing), false);
    let par = process_scan_targets(&targets, &previous, Some(&existing), true);
    assert!(par.iter().all(|r| r.reused), "unchanged files are reused");
    assert_eq!(seq, par, "parallel reuse must equal sequential reuse");
}

#[test]
fn build_edges_cached_is_deterministic_across_runs() {
    // Cross-file imports so the (now parallel) import pass produces edges; run it
    // twice on identical inputs and assert byte-identical edge vectors. Edges are
    // sorted + deduped, so any parallel nondeterminism would surface here.
    let files = [
        (
            "src/a.rs",
            "mod b;\nuse crate::b::go;\npub fn a() { go(); }\n",
        ),
        ("src/b.rs", "pub fn go() {}\n"),
        ("src/c.rs", "use crate::b::go;\npub fn c() { go(); }\n"),
    ];

    let build = || {
        let mut index = ProjectIndex::new("/proj-does-not-need-to-exist");
        let mut cache: HashMap<String, String> = HashMap::new();
        for (path, content) in files {
            index
                .files
                .insert(path.to_string(), fe(path, content, "rs"));
            cache.insert(path.to_string(), content.to_string());
        }
        build_edges_cached(&mut index, &cache);
        index.edges
    };

    assert_eq!(build(), build(), "edge build must be deterministic");
}
