//! Unit tests for the graph index. Extracted from `graph_index/mod.rs`;
//! `super::*` resolves to the `graph_index` module.

use std::sync::Arc;

use super::edges::build_edges_cached;
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
fn test_project_index_new() {
    let idx = ProjectIndex::new("/test");
    assert_eq!(idx.version, INDEX_VERSION);
    assert_eq!(idx.project_root, "/test");
    assert!(idx.files.is_empty());
}

fn fe(path: &str, content: &str, language: &str) -> FileEntry {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut hasher = DefaultHasher::new();
    content.hash(&mut hasher);
    let hash = format!("{:016x}", hasher.finish());
    let line_count = content.lines().count();
    let token_count = crate::core::tokens::count_tokens(content);
    let summary = content
        .lines()
        .map(str::trim)
        .find(|l| {
            !l.is_empty()
                && !l.starts_with("//")
                && !l.starts_with('#')
                && !l.starts_with("/*")
                && !l.starts_with('*')
                && !l.starts_with("use ")
                && !l.starts_with("import ")
                && !l.starts_with("from ")
                && !l.starts_with("require(")
                && !l.starts_with("package ")
        })
        .unwrap_or("")
        .chars()
        .take(120)
        .collect();
    FileEntry {
        path: path.to_string(),
        hash,
        language: language.to_string(),
        line_count,
        token_count,
        exports: Vec::new(),
        summary,
    }
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
fn test_parse_stale_index_version() {
    let json = format!(
        r#"{{"version":{},"project_root":"/test","last_scan":"now","files":{{}},"edges":[],"symbols":{{}}}}"#,
        INDEX_VERSION - 1
    );
    let parsed: ProjectIndex = serde_json::from_str(&json).unwrap();
    assert_ne!(parsed.version, INDEX_VERSION);
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
fn csharp_graph_edges_end_to_end() {
    // Full edge pipeline for a small C# project: `using` resolution (import
    // edges) + namespace cohesion (namespace edges). Regression for the empty
    // C# Call Graph / sparse graph report (NINA).
    const USER_SERVICE: &str = "namespace App.Services;\n\
using App.Data;\n\
\n\
public class UserService\n{\n    \
private readonly OrderRepository _repo = new OrderRepository();\n    \
public void Save() { _repo.Persist(); }\n}\n";
    const ORDER_SERVICE: &str = "namespace App.Services;\n\
\n\
public class OrderService { public void Process() {} }\n";
    const ORDER_REPO: &str = "namespace App.Data;\n\
\n\
public class OrderRepository { public void Persist() {} }\n";

    let files = [
        ("src/App/Services/UserService.cs", USER_SERVICE),
        ("src/App/Services/OrderService.cs", ORDER_SERVICE),
        ("src/App/Data/OrderRepository.cs", ORDER_REPO),
    ];

    let mut index = ProjectIndex::new("/proj-does-not-need-to-exist");
    let mut cache: std::collections::HashMap<String, Arc<String>> =
        std::collections::HashMap::new();
    for (path, content) in files {
        index
            .files
            .insert(path.to_string(), fe(path, content, "cs"));
        cache.insert(path.to_string(), Arc::new(content.to_string()));
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

    // Two files in `App.Services` are linked by a namespace cohesion edge.
    assert!(
        index.edges.iter().any(|e| e.kind == "namespace"
            && (e.from == "src/App/Services/OrderService.cs"
                && e.to == "src/App/Services/UserService.cs"
                || e.from == "src/App/Services/UserService.cs"
                    && e.to == "src/App/Services/OrderService.cs")),
        "expected a C# namespace cohesion edge, got {:?}",
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
    let mut cache: std::collections::HashMap<String, Arc<String>> =
        std::collections::HashMap::new();
    for (path, content) in files {
        index
            .files
            .insert(path.to_string(), fe(path, content, "cs"));
        cache.insert(path.to_string(), Arc::new(content.to_string()));
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
    let mut cache: std::collections::HashMap<String, Arc<String>> =
        std::collections::HashMap::new();
    for (path, content) in files {
        index
            .files
            .insert(path.to_string(), fe(path, content, "gd"));
        cache.insert(path.to_string(), Arc::new(content.to_string()));
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
    let mut cache: std::collections::HashMap<String, Arc<String>> =
        std::collections::HashMap::new();
    for (path, content, lang) in files {
        index
            .files
            .insert(path.to_string(), fe(path, content, lang));
        cache.insert(path.to_string(), Arc::new(content.to_string()));
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
