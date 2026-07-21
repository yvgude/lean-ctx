#[cfg(feature = "embeddings")]
mod tests {
    use serde_json::Value;

    fn write_file(path: &std::path::Path, content: &str) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("mkdir parent");
        }
        std::fs::write(path, content).expect("write file");
    }

    fn parse_json(s: &str) -> Value {
        serde_json::from_str(s).expect("valid json output")
    }

    #[test]
    #[cfg_attr(not(unix), ignore = "import resolution assumes forward-slash paths")]
    fn property_graph_outputs_are_deterministic_and_bounded() {
        let td = tempfile::tempdir().expect("tempdir");
        let root = td.path();

        write_file(
            &root.join("src/a.ts"),
            r#"import { b } from "./b";
export const a = () => b();
"#,
        );
        write_file(
            &root.join("src/b.ts"),
            r#"import { c } from "./c";
export const b = () => c();
"#,
        );
        write_file(&root.join("src/c.ts"), r#"export const c = () => "ok";"#);

        let root_s = root.to_string_lossy().to_string();

        // Build graph (deterministic file ordering + edge insertion).
        let build = lean_ctx::tools::ctx_impact::handle("build", None, &root_s, None, Some("json"));
        let build_v = parse_json(&build);
        assert_eq!(build_v["tool"], "ctx_impact");
        assert_eq!(build_v["action"], "build");

        let status =
            lean_ctx::tools::ctx_impact::handle("status", None, &root_s, None, Some("json"));
        let status_v = parse_json(&status);
        assert_eq!(status_v["tool"], "ctx_impact");
        assert_eq!(status_v["action"], "status");
        assert_ne!(status_v["freshness"], "empty");

        let arch =
            lean_ctx::tools::ctx_architecture::handle("overview", None, &root_s, Some("json"));
        let arch_v = parse_json(&arch);
        assert_eq!(arch_v["tool"], "ctx_architecture");
        assert_eq!(arch_v["action"], "overview");
        assert_eq!(arch_v["files_total"], 3);
        assert_eq!(arch_v["import_edges"], 2);

        // Clusters: all files under src/ → single cluster.
        assert_eq!(arch_v["clusters_total"], 1);
        assert_eq!(arch_v["clusters"][0]["dir"], "src");
        assert_eq!(arch_v["clusters"][0]["file_count"], 3);

        // Layers: c=0, b=1, a=2 → 3 layers.
        assert_eq!(arch_v["layers_total"], 3);
        assert_eq!(arch_v["layers"][0]["depth"], 0);
        assert_eq!(arch_v["layers"][1]["depth"], 1);
        assert_eq!(arch_v["layers"][2]["depth"], 2);

        // Entrypoints: a.ts has no dependents.
        assert_eq!(arch_v["entrypoints_total"], 1);
        assert_eq!(arch_v["entrypoints"][0]["file"], "src/a.ts");

        // Dependency chain should be deterministic.
        let chain = lean_ctx::tools::ctx_impact::handle(
            "chain",
            Some("src/a.ts->src/c.ts"),
            &root_s,
            None,
            Some("json"),
        );
        let chain_v = parse_json(&chain);
        assert_eq!(chain_v["found"], true);
        assert_eq!(
            chain_v["path"],
            serde_json::json!(["src/a.ts", "src/b.ts", "src/c.ts"])
        );

        // Impact should be stable-sorted.
        let impact = lean_ctx::tools::ctx_impact::handle(
            "analyze",
            Some("src/c.ts"),
            &root_s,
            Some(10),
            Some("json"),
        );
        let impact_v = parse_json(&impact);
        assert_eq!(impact_v["affected_files_total"], 2);
        assert_eq!(
            impact_v["affected_files"],
            serde_json::json!(["src/a.ts", "src/b.ts"])
        );
        assert_eq!(impact_v["truncated"], false);
    }
}
