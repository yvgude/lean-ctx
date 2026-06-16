//! Parent ⇄ sub-agent contract roundtrip (GL#450): brief produces a
//! deterministic, budgeted pack; return distills the report into recallable
//! parent knowledge.

use lean_ctx::core::knowledge::ProjectKnowledge;
use lean_ctx::core::memory_policy::MemoryPolicy;

#[allow(clippy::too_many_arguments)]
fn agent(
    action: &str,
    project_root: &str,
    message: Option<&str>,
    priority: Option<&str>,
) -> String {
    lean_ctx::tools::ctx_agent::handle(
        action,
        None,
        None,
        project_root,
        Some("parent-1"),
        message,
        None,
        None,
        None,
        None,
        priority,
        None,
        None,
        false,
        None,
    )
}

#[test]
fn brief_then_return_roundtrip() {
    let _g = lean_ctx::core::data_dir::test_env_lock();
    let tmp = tempfile::tempdir().expect("tempdir");
    let data_dir = tmp.path().join("data");
    std::fs::create_dir_all(&data_dir).expect("create data dir");
    unsafe { std::env::set_var("LEAN_CTX_DATA_DIR", data_dir.to_string_lossy().to_string()) };

    let project_root = tmp.path().join("proj");
    std::fs::create_dir_all(&project_root).expect("create project root");
    let root = project_root.to_string_lossy().to_string();

    let policy = MemoryPolicy::default();
    let mut knowledge = ProjectKnowledge::load_or_create(&root);
    knowledge.remember(
        "architecture",
        "auth",
        "JWT RS256 authentication",
        "s1",
        0.9,
        &policy,
    );
    knowledge.remember("deploy", "host", "AWS eu-central-1", "s1", 0.8, &policy);
    knowledge.save().expect("save knowledge");

    // Briefing pack: deterministic, budgeted, contract v1, relevant fact in.
    let pack1 = agent("brief", &root, Some("fix authentication bug"), Some("800"));
    let pack2 = agent("brief", &root, Some("fix authentication bug"), Some("800"));
    assert_eq!(pack1, pack2, "briefing pack must be byte-identical");

    let parsed: serde_json::Value = serde_json::from_str(&pack1).expect("pack is valid JSON");
    assert_eq!(parsed["contract_version"], 1);
    assert_eq!(parsed["budget_tokens"], 800);
    assert!(
        parsed["used_tokens"].as_u64().unwrap() <= 800,
        "pack must respect the budget"
    );
    assert!(
        pack1.contains("JWT RS256"),
        "relevant auth fact must be briefed: {pack1}"
    );
    assert!(
        parsed["return_format"]
            .as_str()
            .unwrap()
            .contains("category/key"),
        "pack must carry the return contract"
    );

    // Return synthesis: contract lines become recallable parent facts.
    let out = agent(
        "return",
        &root,
        Some(
            "finding/root-cause: token clock skew breaks RS256 validation\n\
             decision/fix: allow 30s leeway in JWT validation\n\
             free-form chatter that is not contract formatted",
        ),
        None,
    );
    assert!(
        out.contains("2 fact(s) distilled"),
        "return must report distilled count: {out}"
    );
    assert!(
        out.contains("1 line(s) rejected"),
        "return must surface rejected lines: {out}"
    );

    let after = ProjectKnowledge::load(&root).expect("knowledge after return");
    let recalled = after.recall("clock skew");
    assert!(
        recalled.iter().any(|f| f.key == "root-cause"),
        "distilled fact must be recallable in parent knowledge"
    );

    unsafe { std::env::remove_var("LEAN_CTX_DATA_DIR") };
}
