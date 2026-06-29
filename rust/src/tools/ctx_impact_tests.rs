use super::*;

#[test]
fn format_impact_empty() {
    let impact = ImpactResult {
        root_file: "a.rs".to_string(),
        affected_files: vec![],
        max_depth_reached: 0,
        edges_traversed: 0,
    };
    let result = format_impact(&impact, "a.rs", "/tmp", OutputFormat::Text);
    assert!(result.contains("No files depend on"));
}

#[test]
fn format_impact_with_files() {
    let impact = ImpactResult {
        root_file: "a.rs".to_string(),
        affected_files: vec!["b.rs".to_string(), "c.rs".to_string()],
        max_depth_reached: 2,
        edges_traversed: 3,
    };
    let result = format_impact(&impact, "a.rs", "/tmp", OutputFormat::Text);
    assert!(result.contains("2 affected files"));
    assert!(result.contains("b.rs"));
    assert!(result.contains("c.rs"));
}

#[test]
fn format_chain_display() {
    let chain = DependencyChain {
        path: vec!["a.rs".to_string(), "b.rs".to_string(), "c.rs".to_string()],
        depth: 2,
    };
    let result = format_chain(&chain, "/tmp", OutputFormat::Text);
    assert!(result.contains("depth 2"));
    assert!(result.contains("a.rs"));
    assert!(result.contains("-> b.rs"));
    assert!(result.contains("-> c.rs"));
}

#[test]
fn handle_missing_path() {
    let result = handle("analyze", None, "/tmp", None, None);
    assert!(result.contains("path is required"));
}

#[test]
fn handle_invalid_chain_spec() {
    let result = handle("chain", Some("no_arrow_here"), "/tmp", None, None);
    assert!(result.contains("Invalid chain spec"));
}

#[test]
fn handle_unknown_action() {
    let result = handle("invalid", None, "/tmp", None, None);
    assert!(result.contains("Unknown action"));
}

#[test]
fn graph_target_key_normalizes_windows_styles() {
    let target = graph_target_key(r"C:/repo/src/main.rs", r"C:\repo");
    let expected = if cfg!(windows) {
        "src/main.rs"
    } else {
        "C:/repo/src/main.rs"
    };
    assert_eq!(target, expected);
}

/// End-to-end regression for GH #365: build the property graph from real
/// Python sources and assert that a class which is imported + instantiated
/// cross-file is NOT reported as `dead_code`. This exercises the *builder*
/// (symbol-level `Calls` edge for class instantiation), not just the SQL
/// rule covered by the synthetic test in `core::smells`. The unused class
/// must still be flagged so the test cannot pass vacuously.
#[cfg(feature = "embeddings")]
#[test]
fn dead_code_builder_does_not_flag_instantiated_python_class() {
    // The property-graph DB path is derived from `LEAN_CTX_DATA_DIR`
    // (`graph_dir`), so a concurrent test that mutates that env var between
    // our `build` and `open` would point them at different directories and
    // yield an empty graph. Serialize on the shared lock that every other
    // data-dir-mutating test already uses.
    let _env = crate::core::data_dir::test_env_lock();
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path();
    std::fs::create_dir_all(root.join("models")).unwrap();
    std::fs::write(
        root.join("models/engine.py"),
        "class Engine:\n    def __init__(self, power):\n        self.power = power\n\n\n\
             class Pipeline:\n    def __init__(self, cfg):\n        self.cfg = cfg\n\n\n\
             class UnusedOrphan:\n    pass\n",
    )
    .unwrap();
    std::fs::write(
        root.join("app.py"),
        "from models.engine import Engine, Pipeline\n\n\
             engine = Engine(power=100)\npipeline = Pipeline(cfg={})\n",
    )
    .unwrap();

    let root_str = root.to_string_lossy().to_string();
    let out = handle("build", None, &root_str, None, Some("text"));
    assert!(!out.contains("ERROR"), "graph build failed: {out}");

    let graph =
        crate::core::property_graph::CodeGraph::open(&root_str).expect("open property graph");
    let findings = crate::core::smells::scan_rule(
        graph.connection(),
        "dead_code",
        &crate::core::smells::SmellConfig::default(),
    );
    let dead: Vec<String> = findings.iter().filter_map(|f| f.symbol.clone()).collect();

    assert!(
        !dead.iter().any(|s| s == "Engine"),
        "instantiated class `Engine` must not be dead_code; findings: {dead:?}"
    );
    assert!(
        !dead.iter().any(|s| s == "Pipeline"),
        "instantiated class `Pipeline` must not be dead_code; findings: {dead:?}"
    );
    assert!(
        dead.iter().any(|s| s == "UnusedOrphan"),
        "never-referenced class `UnusedOrphan` should still be flagged (non-vacuous); \
             findings: {dead:?}"
    );
}

/// End-to-end regression for GH #398: C# files in the same namespace use
/// each other's types **without any `using` directive**, and dependency
/// injection means the type is often never `new`-ed by its consumer. With
/// import- and call-edges only, the consumed class is a false-negative
/// leaf node. Type-usage edges (`TypeRef`) must connect consumer -> definer
/// so impact analysis reports the real blast radius.
#[cfg(feature = "embeddings")]
#[test]
fn csharp_same_namespace_type_use_is_not_a_leaf() {
    let _env = crate::core::data_dir::test_env_lock();
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path();
    std::fs::create_dir_all(root.join("Models")).unwrap();
    std::fs::create_dir_all(root.join("Services")).unwrap();

    // The small class under change — no consumer imports it via `using`.
    std::fs::write(
        root.join("Models/Engine.cs"),
        "namespace App.Core;\n\n\
             public class Engine\n{\n    public int Power { get; set; }\n}\n",
    )
    .unwrap();
    // DI-style consumer: field + constructor parameter, never `new Engine()`.
    std::fs::write(
        root.join("Services/Motor.cs"),
        "namespace App.Core;\n\n\
             public class Motor\n{\n    private readonly Engine _engine;\n\n\
             \x20   public Motor(Engine engine)\n    {\n        _engine = engine;\n    }\n}\n",
    )
    .unwrap();
    // Inheritance consumer in a nested namespace part, also without `using`.
    std::fs::write(
        root.join("Services/TurboEngine.cs"),
        "namespace App.Core;\n\n\
             public class TurboEngine : Engine\n{\n    public int Boost { get; set; }\n}\n",
    )
    .unwrap();
    // Unrelated file: must NOT appear in the blast radius (non-vacuous).
    std::fs::write(
        root.join("Services/Logger.cs"),
        "namespace App.Core;\n\n\
             public class Logger\n{\n    public void Log(string msg) { }\n}\n",
    )
    .unwrap();

    let root_str = root.to_string_lossy().to_string();
    let out = handle("build", None, &root_str, None, Some("text"));
    assert!(!out.contains("ERROR"), "graph build failed: {out}");

    let graph =
        crate::core::property_graph::CodeGraph::open(&root_str).expect("open property graph");
    let impact = graph
        .impact_analysis("Models/Engine.cs", 5)
        .expect("impact analysis");

    assert!(
        impact
            .affected_files
            .contains(&"Services/Motor.cs".to_string()),
        "DI consumer (field + ctor param, no using, no new) must be affected; got: {:?}",
        impact.affected_files
    );
    assert!(
        impact
            .affected_files
            .contains(&"Services/TurboEngine.cs".to_string()),
        "subclass (base_list, no using) must be affected; got: {:?}",
        impact.affected_files
    );
    assert!(
        !impact
            .affected_files
            .contains(&"Services/Logger.cs".to_string()),
        "unrelated file must NOT be affected; got: {:?}",
        impact.affected_files
    );

    // Same root cause, second symptom: a class consumed only as a type
    // (DI) was flagged `dead_code` because nothing ever *called* it. The
    // symbol-level TypeRef edge must clear it; the genuinely unreferenced
    // Logger keeps the rule honest.
    let findings = crate::core::smells::scan_rule(
        graph.connection(),
        "dead_code",
        &crate::core::smells::SmellConfig::default(),
    );
    let dead: Vec<String> = findings.iter().filter_map(|f| f.symbol.clone()).collect();
    assert!(
        !dead.iter().any(|s| s == "Engine"),
        "type-consumed class `Engine` must not be dead_code; findings: {dead:?}"
    );
    assert!(
        dead.iter().any(|s| s == "Logger"),
        "never-referenced class `Logger` should still be flagged (non-vacuous); \
             findings: {dead:?}"
    );
}

/// GH #398 end-to-end regression for the wipe every prior fix missed:
/// `ctx_impact` builds precise `type_ref` edges, but a routine background
/// reindex (`graph_index::scan` -> `ProjectIndex::save` -> `mirror_index`)
/// clears the PropertyGraph and repopulates it from graph_index. Before the
/// fix, graph_index emitted no type-usage edges, so the C# same-namespace
/// consumer was silently dropped to a false-negative leaf right after the
/// first dashboard/daemon reindex. The mirror must now preserve the blast
/// radius. Gated on `tree-sitter`: needs the C# grammar, not embeddings.
#[cfg(feature = "tree-sitter")]
#[test]
fn csharp_blast_radius_survives_background_reindex() {
    let _env = crate::core::data_dir::test_env_lock();
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path();
    // A project marker so graph_index accepts the root as safe to scan.
    std::fs::create_dir_all(root.join(".git")).unwrap();
    std::fs::create_dir_all(root.join("Models")).unwrap();
    std::fs::create_dir_all(root.join("Services")).unwrap();

    std::fs::write(
        root.join("Models/Engine.cs"),
        "namespace App.Core;\n\n\
             public class Engine\n{\n    public int Power { get; set; }\n}\n",
    )
    .unwrap();
    // DI-style consumer: field + ctor param, no `using`, no `new Engine()`.
    std::fs::write(
        root.join("Services/Motor.cs"),
        "namespace App.Core;\n\n\
             public class Motor\n{\n    private readonly Engine _engine;\n\n\
             \x20   public Motor(Engine engine)\n    {\n        _engine = engine;\n    }\n}\n",
    )
    .unwrap();

    let root_str = root.to_string_lossy().to_string();

    // 1) ctx_impact's own builder writes the type_ref edges.
    let out = handle("build", None, &root_str, None, Some("text"));
    assert!(!out.contains("ERROR"), "graph build failed: {out}");

    // 2) A background reindex mirrors graph_index over the PropertyGraph,
    //    clearing it first — exactly what the daemon / dashboard / ctx_graph
    //    trigger via ProjectIndex::save(). If the mirror dropped type_ref the
    //    consumer would vanish; an aborted (empty) scan would clear the graph
    //    entirely. Either failure mode makes the assertion below fail loudly.
    let _ = crate::core::graph_index::scan(&root_str);

    // 3) The blast radius must survive the mirror (the actual GH #398 bug).
    let graph =
        crate::core::property_graph::CodeGraph::open(&root_str).expect("open property graph");
    let impact = graph
        .impact_analysis("Models/Engine.cs", 5)
        .expect("impact analysis");
    assert!(
        impact
            .affected_files
            .contains(&"Services/Motor.cs".to_string()),
        "same-namespace consumer must survive a background reindex; got: {:?}",
        impact.affected_files
    );
}

/// GH #398 bug class (Go): files in the same package (== same directory) use
/// each other's types with **no import at all**, so import edges leave the
/// consumed type a false-negative leaf — and the coarse `package` edge is
/// not even a structural impact edge. The directory-scoped `type_ref` edge
/// must connect consumer -> definer, while a same-directory file that does
/// *not* use the type stays out (precision the package edge cannot give).
#[cfg(feature = "embeddings")]
#[test]
fn go_same_package_type_use_is_not_a_leaf() {
    let _env = crate::core::data_dir::test_env_lock();
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path();
    std::fs::create_dir_all(root.join("core")).unwrap();

    // The small type under change.
    std::fs::write(
        root.join("core/engine.go"),
        "package core\n\ntype Engine struct {\n\tPower int\n}\n",
    )
    .unwrap();
    // Same-package consumer: field + parameter, never imported.
    std::fs::write(
        root.join("core/motor.go"),
        "package core\n\ntype Motor struct {\n\tengine Engine\n}\n\n\
             func NewMotor(e Engine) *Motor {\n\treturn &Motor{engine: e}\n}\n",
    )
    .unwrap();
    // Same directory, but does NOT use Engine -> must stay out (precision).
    std::fs::write(
        root.join("core/logger.go"),
        "package core\n\ntype Logger struct{}\n\nfunc (l *Logger) Log(m string) {}\n",
    )
    .unwrap();

    let root_str = root.to_string_lossy().to_string();
    let out = handle("build", None, &root_str, None, Some("text"));
    assert!(!out.contains("ERROR"), "graph build failed: {out}");

    let graph =
        crate::core::property_graph::CodeGraph::open(&root_str).expect("open property graph");
    let impact = graph
        .impact_analysis("core/engine.go", 5)
        .expect("impact analysis");

    assert!(
        impact.affected_files.contains(&"core/motor.go".to_string()),
        "same-package consumer (no import) must be affected; got: {:?}",
        impact.affected_files
    );
    assert!(
        !impact
            .affected_files
            .contains(&"core/logger.go".to_string()),
        "same-directory non-consumer must NOT be affected; got: {:?}",
        impact.affected_files
    );
}

/// GH #398 bug class (Go), mirror path: the durable `graph_index` ->
/// PropertyGraph mirror must reproduce the Go same-package `type_ref` edge so
/// a background reindex cannot wipe the blast radius (the original #398 wipe,
/// now guarded for Go too). Gated on `tree-sitter`: needs the Go grammar.
#[cfg(feature = "tree-sitter")]
#[test]
fn go_blast_radius_survives_background_reindex() {
    let _env = crate::core::data_dir::test_env_lock();
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path();
    std::fs::create_dir_all(root.join(".git")).unwrap();
    std::fs::create_dir_all(root.join("core")).unwrap();

    std::fs::write(
        root.join("core/engine.go"),
        "package core\n\ntype Engine struct {\n\tPower int\n}\n",
    )
    .unwrap();
    std::fs::write(
        root.join("core/motor.go"),
        "package core\n\ntype Motor struct {\n\tengine Engine\n}\n",
    )
    .unwrap();

    let root_str = root.to_string_lossy().to_string();

    let out = handle("build", None, &root_str, None, Some("text"));
    assert!(!out.contains("ERROR"), "graph build failed: {out}");

    // Background reindex: clears the PropertyGraph and repopulates from
    // graph_index. The mirror must re-emit the Go type_ref edge.
    let _ = crate::core::graph_index::scan(&root_str);

    let graph =
        crate::core::property_graph::CodeGraph::open(&root_str).expect("open property graph");
    let impact = graph
        .impact_analysis("core/engine.go", 5)
        .expect("impact analysis");
    assert!(
        impact.affected_files.contains(&"core/motor.go".to_string()),
        "Go same-package consumer must survive a background reindex; got: {:?}",
        impact.affected_files
    );
}

/// GH #398 bug class (Kotlin): same-*package* types need no import, and the
/// package is independent of the directory. A consumer in a different
/// directory but the same package must land in the blast radius (proving
/// package-, not directory-based resolution), while a different-package file
/// stays out.
#[cfg(feature = "embeddings")]
#[test]
fn kotlin_same_package_type_use_is_not_a_leaf() {
    let _env = crate::core::data_dir::test_env_lock();
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path();
    std::fs::create_dir_all(root.join("domain")).unwrap();
    std::fs::create_dir_all(root.join("service")).unwrap();
    std::fs::create_dir_all(root.join("other")).unwrap();

    std::fs::write(
        root.join("domain/Engine.kt"),
        "package app.core\n\nclass Engine {\n    var power: Int = 0\n}\n",
    )
    .unwrap();
    // Different directory, same package, no import — the hard case.
    std::fs::write(
        root.join("service/Motor.kt"),
        "package app.core\n\nclass Motor(private val engine: Engine)\n",
    )
    .unwrap();
    // Different package -> must NOT be affected (non-vacuous).
    std::fs::write(
        root.join("other/Logger.kt"),
        "package app.other\n\nclass Logger\n",
    )
    .unwrap();

    let root_str = root.to_string_lossy().to_string();
    let out = handle("build", None, &root_str, None, Some("text"));
    assert!(!out.contains("ERROR"), "graph build failed: {out}");

    let graph =
        crate::core::property_graph::CodeGraph::open(&root_str).expect("open property graph");
    let impact = graph
        .impact_analysis("domain/Engine.kt", 5)
        .expect("impact analysis");

    assert!(
        impact
            .affected_files
            .contains(&"service/Motor.kt".to_string()),
        "same-package consumer in another directory must be affected; got: {:?}",
        impact.affected_files
    );
    assert!(
        !impact
            .affected_files
            .contains(&"other/Logger.kt".to_string()),
        "different-package file must NOT be affected; got: {:?}",
        impact.affected_files
    );
}

/// Regression for GH #398's upgrade path: the v3.8.3 `type_ref` fix only
/// helps if an *existing* graph is rebuilt after upgrading. A graph built by
/// an older engine keeps `node_count > 0`, so without an engine-version gate
/// `analyze` silently serves it — leaving the C# same-namespace consumer a
/// false-negative leaf. We build a correct graph, stamp its meta back to
/// engine version 0 (simulating a pre-`type_ref` build), and assert the next
/// query self-heals: it rebuilds, surfaces the consumer, and re-stamps the
/// current engine version.
#[cfg(feature = "embeddings")]
#[test]
fn stale_engine_graph_is_rebuilt_before_query() {
    let _env = crate::core::data_dir::test_env_lock();
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path();
    std::fs::create_dir_all(root.join("Models")).unwrap();
    std::fs::create_dir_all(root.join("Services")).unwrap();

    std::fs::write(
        root.join("Models/Engine.cs"),
        "namespace App.Core;\n\n\
             public class Engine\n{\n    public int Power { get; set; }\n}\n",
    )
    .unwrap();
    // DI-style consumer: field + constructor parameter, no `using`, no `new`.
    std::fs::write(
        root.join("Services/Motor.cs"),
        "namespace App.Core;\n\n\
             public class Motor\n{\n    private readonly Engine _engine;\n\n\
             \x20   public Motor(Engine engine)\n    {\n        _engine = engine;\n    }\n}\n",
    )
    .unwrap();

    let root_str = root.to_string_lossy().to_string();

    // Build a correct graph, then simulate a graph produced by an engine
    // that predates `type_ref` by stamping its meta back to version 0.
    let out = handle("build", None, &root_str, None, Some("text"));
    assert!(!out.contains("ERROR"), "graph build failed: {out}");
    let mut meta = crate::core::property_graph::load_meta(&root_str).expect("meta after build");
    assert_eq!(
        meta.engine_version,
        crate::core::property_graph::GRAPH_ENGINE_VERSION,
        "a fresh build must stamp the current engine version"
    );
    meta.engine_version = 0;
    crate::core::property_graph::write_meta(&root_str, &meta).expect("downgrade meta");
    assert!(
        crate::core::property_graph::engine_outdated(&root_str),
        "downgraded graph must read as outdated"
    );

    // The query path must transparently rebuild the stale graph.
    let analysis = handle(
        "analyze",
        Some("Models/Engine.cs"),
        &root_str,
        None,
        Some("text"),
    );
    assert!(
        analysis.contains("Services/Motor.cs"),
        "stale graph must be rebuilt so the DI consumer surfaces; got: {analysis}"
    );
    let healed = crate::core::property_graph::load_meta(&root_str).expect("meta after self-heal");
    assert_eq!(
        healed.engine_version,
        crate::core::property_graph::GRAPH_ENGINE_VERSION,
        "self-heal must re-stamp the current engine version"
    );
}

/// End-to-end regression for GH #398 (expression-position follow-up): a C#
/// file that consumes types only through static calls, enum values or
/// attributes — no `using`, no `new`, no field/param of that type — must
/// still land in the blast radius of the defining file.
///
/// Gated on `tree-sitter` (not `embeddings`) on purpose: the existing #398
/// e2e tests only run under `embeddings`, leaving the
/// `index_graph_file_minimal` builder path untested. This test exercises
/// both builder paths (it runs whenever the C# grammar is available).
#[cfg(feature = "tree-sitter")]
#[test]
fn csharp_expression_position_type_use_is_not_a_leaf() {
    let _env = crate::core::data_dir::test_env_lock();
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path();
    std::fs::create_dir_all(root.join("Models")).unwrap();
    std::fs::create_dir_all(root.join("Attributes")).unwrap();
    std::fs::create_dir_all(root.join("Services")).unwrap();

    // Static factory + static field, used by a consumer via `Engine.X`.
    std::fs::write(
        root.join("Models/Engine.cs"),
        "namespace App.Core;\n\n\
             public class Engine\n{\n\
             \x20   public static Engine Create() => new Engine();\n\
             \x20   public static readonly int Default = 0;\n}\n",
    )
    .unwrap();
    // Enum, consumed only as `Status.Active`.
    std::fs::write(
        root.join("Models/Status.cs"),
        "namespace App.Core;\n\npublic enum Status { Active, Inactive }\n",
    )
    .unwrap();
    // Attribute class, consumed only as `[ApiController]`.
    std::fs::write(
        root.join("Attributes/ApiControllerAttribute.cs"),
        "using System;\n\nnamespace App.Core;\n\n\
             public class ApiControllerAttribute : Attribute { }\n",
    )
    .unwrap();
    // Consumer: ONLY expression-position usage — no using/new/field/param.
    std::fs::write(
        root.join("Services/Garage.cs"),
        "namespace App.Core;\n\n\
             [ApiController]\n\
             public class Garage\n{\n\
             \x20   public void Boot()\n    {\n\
             \x20       var e = Engine.Create();\n\
             \x20       var s = Status.Active;\n    }\n}\n",
    )
    .unwrap();
    // Unrelated control: must NOT appear in any blast radius (non-vacuous).
    std::fs::write(
        root.join("Services/Logger.cs"),
        "namespace App.Core;\n\n\
             public class Logger\n{\n    public void Log(string m) { }\n}\n",
    )
    .unwrap();

    let root_str = root.to_string_lossy().to_string();
    let out = handle("build", None, &root_str, None, Some("text"));
    assert!(!out.contains("ERROR"), "graph build failed: {out}");

    let graph =
        crate::core::property_graph::CodeGraph::open(&root_str).expect("open property graph");
    let affected = |file: &str| -> Vec<String> {
        graph
            .impact_analysis(file, 5)
            .expect("impact analysis")
            .affected_files
    };

    let engine_aff = affected("Models/Engine.cs");
    assert!(
        engine_aff.contains(&"Services/Garage.cs".to_string()),
        "static-call consumer (no using/new) must be affected by Engine.cs; got: {engine_aff:?}"
    );
    assert!(
        !engine_aff.contains(&"Services/Logger.cs".to_string()),
        "unrelated file must NOT be affected by Engine.cs; got: {engine_aff:?}"
    );

    let status_aff = affected("Models/Status.cs");
    assert!(
        status_aff.contains(&"Services/Garage.cs".to_string()),
        "enum-value consumer must be affected by Status.cs; got: {status_aff:?}"
    );

    let attr_aff = affected("Attributes/ApiControllerAttribute.cs");
    assert!(
        attr_aff.contains(&"Services/Garage.cs".to_string()),
        "attribute consumer must be affected by ApiControllerAttribute.cs; got: {attr_aff:?}"
    );
}

/// GH #398 follow-up (extension methods, #642): an extension method
/// `value.WordCount()` makes the consuming file depend on the file that
/// *defines* the extension — the receiver is an instance and the definer's
/// type name is never written, so neither import- nor type-usage edges
/// connect them. Only a method-host edge does. Gated on `tree-sitter` so
/// both builder paths (embeddings + minimal) are exercised.
#[cfg(feature = "tree-sitter")]
#[test]
fn csharp_extension_method_host_is_in_blast_radius() {
    let _env = crate::core::data_dir::test_env_lock();
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path();
    std::fs::create_dir_all(root.join("Extensions")).unwrap();
    std::fs::create_dir_all(root.join("Services")).unwrap();

    // Extension host: static class, first parameter carries `this`. Same
    // namespace as the consumer, so it is callable without a `using` —
    // which keeps the test free of any import edge.
    std::fs::write(
        root.join("Extensions/StringExtensions.cs"),
        "namespace App.Core;\n\n\
             public static class StringExtensions\n{\n\
             \x20   public static int WordCount(this string s) => s.Length;\n}\n",
    )
    .unwrap();
    // Consumer: calls the extension as if it were an instance method.
    std::fs::write(
        root.join("Services/Report.cs"),
        "namespace App.Core;\n\n\
             public class Report\n{\n\
             \x20   public int Count(string text) => text.WordCount();\n}\n",
    )
    .unwrap();
    // Unrelated control: must NOT be linked (non-vacuous).
    std::fs::write(
        root.join("Services/Logger.cs"),
        "namespace App.Core;\n\n\
             public class Logger\n{\n    public void Log(string m) { }\n}\n",
    )
    .unwrap();

    let root_str = root.to_string_lossy().to_string();
    let out = handle("build", None, &root_str, None, Some("text"));
    assert!(!out.contains("ERROR"), "graph build failed: {out}");

    let graph =
        crate::core::property_graph::CodeGraph::open(&root_str).expect("open property graph");
    let affected = graph
        .impact_analysis("Extensions/StringExtensions.cs", 5)
        .expect("impact analysis")
        .affected_files;
    assert!(
        affected.contains(&"Services/Report.cs".to_string()),
        "extension-method consumer must be in the host's blast radius; got: {affected:?}"
    );
    assert!(
        !affected.contains(&"Services/Logger.cs".to_string()),
        "unrelated file must NOT be affected; got: {affected:?}"
    );
}

/// GH #398 follow-up (namespace-aware resolution, #641): two project types
/// share a name in different namespaces. A consumer that sees only one of
/// them (its own namespace) must link to *that* definer and not the
/// homonym in an unrelated namespace. The pre-fix resolver linked both.
#[cfg(feature = "tree-sitter")]
#[test]
fn csharp_type_use_namespace_disambiguation() {
    let _env = crate::core::data_dir::test_env_lock();
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path();
    std::fs::create_dir_all(root.join("Foo")).unwrap();
    std::fs::create_dir_all(root.join("Bar")).unwrap();

    // Same type name, two namespaces.
    std::fs::write(
        root.join("Foo/Engine.cs"),
        "namespace App.Foo;\n\n\
             public class Engine\n{\n    public int Power { get; set; }\n}\n",
    )
    .unwrap();
    std::fs::write(
        root.join("Bar/Engine.cs"),
        "namespace App.Bar;\n\n\
             public class Engine\n{\n    public int Torque { get; set; }\n}\n",
    )
    .unwrap();
    // Consumer in App.Foo, no `using` — only `App.Foo.Engine` is visible.
    std::fs::write(
        root.join("Foo/Garage.cs"),
        "namespace App.Foo;\n\n\
             public class Garage\n{\n    private readonly Engine _engine;\n\n\
             \x20   public Garage(Engine engine)\n    {\n        _engine = engine;\n    }\n}\n",
    )
    .unwrap();

    let root_str = root.to_string_lossy().to_string();
    let out = handle("build", None, &root_str, None, Some("text"));
    assert!(!out.contains("ERROR"), "graph build failed: {out}");

    let graph =
        crate::core::property_graph::CodeGraph::open(&root_str).expect("open property graph");
    let affected = |file: &str| -> Vec<String> {
        graph
            .impact_analysis(file, 5)
            .expect("impact analysis")
            .affected_files
    };

    assert!(
        affected("Foo/Engine.cs").contains(&"Foo/Garage.cs".to_string()),
        "consumer must depend on the same-namespace Engine; got: {:?}",
        affected("Foo/Engine.cs")
    );
    assert!(
        !affected("Bar/Engine.cs").contains(&"Foo/Garage.cs".to_string()),
        "consumer must NOT depend on the homonym in another namespace; got: {:?}",
        affected("Bar/Engine.cs")
    );
}

/// GH #398 follow-up (cap bypass, #641): a type name defined in more files
/// than the failsafe fallback cap is normally dropped as too generic. But
/// when exactly one definer sits in the consumer's visible namespace, that
/// unambiguous match must still be linked — the namespace evidence beats
/// the global cap. The pre-fix resolver dropped the name entirely.
#[cfg(feature = "tree-sitter")]
#[test]
fn csharp_type_use_namespace_cap_bypass() {
    let _env = crate::core::data_dir::test_env_lock();
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path();

    // Six definers of `Widget` — past the fallback cap (5).
    let homonym_dirs = ["N1", "N2", "N3", "N4", "N5"];
    for d in homonym_dirs {
        std::fs::create_dir_all(root.join(d)).unwrap();
        std::fs::write(
                root.join(d).join("Widget.cs"),
                format!(
                    "namespace App.{d};\n\npublic class Widget\n{{\n    public int Id {{ get; set; }}\n}}\n"
                ),
            )
            .unwrap();
    }
    // The visible one, in the consumer's own namespace.
    std::fs::create_dir_all(root.join("Foo")).unwrap();
    std::fs::write(
        root.join("Foo/Widget.cs"),
        "namespace App.Foo;\n\npublic class Widget\n{\n    public int Tag { get; set; }\n}\n",
    )
    .unwrap();
    // Consumer in App.Foo, no `using`.
    std::fs::write(
        root.join("Foo/Dashboard.cs"),
        "namespace App.Foo;\n\n\
             public class Dashboard\n{\n    private readonly Widget _widget;\n\n\
             \x20   public Dashboard(Widget widget)\n    {\n        _widget = widget;\n    }\n}\n",
    )
    .unwrap();

    let root_str = root.to_string_lossy().to_string();
    let out = handle("build", None, &root_str, None, Some("text"));
    assert!(!out.contains("ERROR"), "graph build failed: {out}");

    let graph =
        crate::core::property_graph::CodeGraph::open(&root_str).expect("open property graph");
    let affected = |file: &str| -> Vec<String> {
        graph
            .impact_analysis(file, 5)
            .expect("impact analysis")
            .affected_files
    };

    assert!(
        affected("Foo/Widget.cs").contains(&"Foo/Dashboard.cs".to_string()),
        "unambiguous same-namespace match must bypass the cap; got: {:?}",
        affected("Foo/Widget.cs")
    );
    // The homonyms in other namespaces stay unlinked.
    assert!(
        !affected("N1/Widget.cs").contains(&"Foo/Dashboard.cs".to_string()),
        "out-of-namespace homonym must NOT be linked; got: {:?}",
        affected("N1/Widget.cs")
    );
}

/// GH #398 (real-world cross-namespace DI): every existing e2e test keeps
/// consumer and definer in the *same* namespace. Real C# services live in
/// their own namespace and reach a model/contract through a `using`
/// directive, injected by interface (never `new`-ed). This exercises the
/// full `handle("analyze")` text path — exactly what the MCP tool runs —
/// for the interface contract changing:
///   * the concrete implementor (reached via the base list), and
///   * the cross-namespace DI consumer (reached via `using` + ctor param).
#[cfg(feature = "tree-sitter")]
#[test]
fn csharp_cross_namespace_using_di_blast_radius() {
    let _env = crate::core::data_dir::test_env_lock();
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path();
    std::fs::create_dir_all(root.join("Models")).unwrap();
    std::fs::create_dir_all(root.join("Services")).unwrap();

    // Contract interface in the Models namespace.
    std::fs::write(
        root.join("Models/IEngine.cs"),
        "namespace MyApp.Models;\n\npublic interface IEngine\n{\n    int Power { get; }\n}\n",
    )
    .unwrap();
    // Concrete class implementing the interface (base_list -> IEngine).
    std::fs::write(
        root.join("Models/Engine.cs"),
        "namespace MyApp.Models;\n\n\
             public class Engine : IEngine\n{\n    public int Power => 1;\n}\n",
    )
    .unwrap();
    // Service in a DIFFERENT namespace, reaching the contract via `using`,
    // injected by interface — never `new Engine()`.
    std::fs::write(
        root.join("Services/Motor.cs"),
        "using MyApp.Models;\n\nnamespace MyApp.Services;\n\n\
             public class Motor\n{\n    private readonly IEngine _engine;\n\n\
             \x20   public Motor(IEngine engine)\n    {\n        _engine = engine;\n    }\n}\n",
    )
    .unwrap();
    // Unrelated control: must NOT appear in the blast radius (non-vacuous).
    std::fs::write(
        root.join("Services/Logger.cs"),
        "namespace MyApp.Services;\n\n\
             public class Logger\n{\n    public void Log(string m) { }\n}\n",
    )
    .unwrap();

    let root_str = root.to_string_lossy().to_string();

    // Full MCP path: build then analyze (text), exactly like the tool runs.
    let build = handle("build", None, &root_str, None, Some("text"));
    assert!(!build.contains("ERROR"), "graph build failed: {build}");

    let iface = handle(
        "analyze",
        Some("Models/IEngine.cs"),
        &root_str,
        None,
        Some("text"),
    );
    assert!(
        iface.contains("Models/Engine.cs"),
        "implementor (base list) must be impacted by IEngine.cs; got: {iface}"
    );
    assert!(
        iface.contains("Services/Motor.cs"),
        "cross-namespace DI consumer (using + interface ctor param) must be \
             impacted by IEngine.cs; got: {iface}"
    );
    assert!(
        !iface.contains("Services/Logger.cs"),
        "unrelated file must NOT be impacted; got: {iface}"
    );
}

/// GH #398 (root cause of the persistent reports): `ctx_impact analyze` is
/// asked for the impact of a *class name* — `ctx_impact analyze ArcPoint` —
/// not a file path. Before the symbol-name fallback the bare name matched no
/// file node, so impact returned a misleading "leaf node / no impact". The
/// fallback must resolve the class to its defining file and surface the real
/// consumers, while a genuinely unknown target gets an actionable
/// diagnostic rather than a false "no impact". Exercises the full
/// `handle("analyze")` path on both builder paths (gated on `tree-sitter`).
#[cfg(feature = "tree-sitter")]
#[test]
fn csharp_analyze_by_class_name_resolves_to_file() {
    let _env = crate::core::data_dir::test_env_lock();
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path();
    std::fs::create_dir_all(root.join("Models")).unwrap();
    std::fs::create_dir_all(root.join("Services")).unwrap();

    std::fs::write(
        root.join("Models/Engine.cs"),
        "namespace App.Core;\n\n\
             public class Engine\n{\n    public int Power { get; set; }\n}\n",
    )
    .unwrap();
    std::fs::write(
        root.join("Services/Motor.cs"),
        "namespace App.Core;\n\n\
             public class Motor\n{\n    private readonly Engine _engine;\n\n\
             \x20   public Motor(Engine engine)\n    {\n        _engine = engine;\n    }\n}\n",
    )
    .unwrap();

    let root_str = root.to_string_lossy().to_string();
    let build = handle("build", None, &root_str, None, Some("text"));
    assert!(!build.contains("ERROR"), "graph build failed: {build}");

    // The actual user/LLM call: a bare class name, NOT a file path.
    let by_name = handle("analyze", Some("Engine"), &root_str, None, Some("text"));
    assert!(
        by_name.contains("Services/Motor.cs"),
        "class-name analyze must resolve 'Engine' to its file and surface the \
             DI consumer; got: {by_name}"
    );
    assert!(
        by_name.contains("defined in") && by_name.contains("Models/Engine.cs"),
        "class-name analyze must disclose the resolved definer file; got: {by_name}"
    );

    // A file path must still work unchanged (regression guard).
    let by_path = handle(
        "analyze",
        Some("Models/Engine.cs"),
        &root_str,
        None,
        Some("text"),
    );
    assert!(
        by_path.contains("Services/Motor.cs"),
        "file-path analyze must keep working; got: {by_path}"
    );

    // An unknown target gets a diagnostic, not a false "leaf node".
    let unknown = handle(
        "analyze",
        Some("NoSuchThing"),
        &root_str,
        None,
        Some("text"),
    );
    assert!(
        unknown.contains("not a known file or symbol"),
        "unknown target must produce the actionable diagnostic; got: {unknown}"
    );

    // JSON shape carries the resolution provenance for programmatic callers.
    let json = handle("analyze", Some("Engine"), &root_str, None, Some("json"));
    assert!(
        json.contains("\"resolved_from\": \"symbol\""),
        "json must mark symbol-resolved analyses; got: {json}"
    );
}

/// The decisive real-world #398 case: the class name does **not** match its
/// file name (`ArcPoint` lives in `Shapes.cs`), which is exactly why a
/// user/LLM types `ctx_impact ArcPoint` instead of a path — and why the old
/// file-only lookup always missed. Both a cross-namespace `using` consumer
/// and a same-namespace no-`using` consumer must surface; an unrelated file
/// must not (non-vacuous).
#[test]
fn csharp_analyze_by_class_name_when_filename_differs() {
    let _env = crate::core::data_dir::test_env_lock();
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path();
    std::fs::create_dir_all(root.join("Geometry")).unwrap();
    std::fs::create_dir_all(root.join("Rendering")).unwrap();
    std::fs::create_dir_all(root.join("Misc")).unwrap();

    // Class name != file name — the crux of the reporter's scenario.
    std::fs::write(
        root.join("Geometry/Shapes.cs"),
        "namespace App.Geometry;\n\n\
             public class ArcPoint\n{\n    public double X { get; set; }\n\
             \x20   public double Y { get; set; }\n}\n",
    )
    .unwrap();
    // Cross-namespace consumer WITH a `using`, DI via constructor.
    std::fs::write(
        root.join("Rendering/Canvas.cs"),
        "using App.Geometry;\n\nnamespace App.Rendering;\n\n\
             public class Canvas\n{\n    private readonly ArcPoint _origin;\n\n\
             \x20   public Canvas(ArcPoint origin)\n    {\n        _origin = origin;\n    }\n}\n",
    )
    .unwrap();
    // Same-namespace consumer WITHOUT any `using`, field injection.
    std::fs::write(
        root.join("Geometry/Grid.cs"),
        "namespace App.Geometry;\n\n\
             public class Grid\n{\n    private ArcPoint _topLeft = new();\n}\n",
    )
    .unwrap();
    // Unrelated file in a third namespace: must stay out of the blast radius.
    std::fs::write(
        root.join("Misc/Clock.cs"),
        "namespace App.Misc;\n\n\
             public class Clock\n{\n    public long Ticks { get; set; }\n}\n",
    )
    .unwrap();

    let root_str = root.to_string_lossy().to_string();
    let build = handle("build", None, &root_str, None, Some("text"));
    assert!(!build.contains("ERROR"), "graph build failed: {build}");

    let out = handle("analyze", Some("ArcPoint"), &root_str, None, Some("text"));
    assert!(
        out.contains("Geometry/Shapes.cs"),
        "name must resolve to the (differently named) definer file; got: {out}"
    );
    assert!(
        out.contains("Rendering/Canvas.cs"),
        "cross-namespace `using` DI consumer must be in the blast radius; got: {out}"
    );
    assert!(
        out.contains("Geometry/Grid.cs"),
        "same-namespace no-`using` consumer must be in the blast radius; got: {out}"
    );
    assert!(
        !out.contains("Misc/Clock.cs"),
        "unrelated file must NOT appear (non-vacuous); got: {out}"
    );
}
