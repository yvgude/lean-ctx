// SPDX-License-Identifier: Apache-2.0
//! Test fixture for cross-project isolation (I08): one canary in every store
//! of a project, checked against tool calls from another project. The
//! dashboard inspector's half of the check runs on top of the same fixture.

use crate::tools::LeanCtxServer;

/// The term planted in the first project.
pub(crate) const CANARY: &str = "zqcanaryeight";
/// A term no project has ever held: the baseline an answer is compared to.
pub(crate) const ABSENT: &str = "zqnowhereeight";

/// Two fresh project roots, each marked by a `.git` dir.
pub(crate) fn git_projects() -> (tempfile::TempDir, tempfile::TempDir) {
    let projects = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    for project in [&projects.0, &projects.1] {
        std::fs::create_dir(project.path().join(".git")).unwrap();
    }
    projects
}

/// The text a tool call answers, or its error message.
pub(crate) async fn call_text(
    server: &LeanCtxServer,
    tool: &str,
    args: &serde_json::Value,
) -> String {
    let result = super::call_tool::dispatch_and_post_process(
        server,
        tool,
        args.as_object(),
        false,
        crate::core::config::Config::load_arc(),
        false,
        None,
        None,
        format!("{tool}:{args}"),
        None,
    )
    .await;
    match result {
        Ok(result) => result
            .content
            .iter()
            .filter_map(|content| content.as_text().map(|t| t.text.clone()))
            .collect(),
        Err(error) => error.message.to_string(),
    }
}

/// The world after [`canary_invisible_to_tools`]. Fields drop in order:
/// servers and projects first, the isolation guards last.
pub(crate) struct CanaryWorld {
    _owner: LeanCtxServer,
    _other: LeanCtxServer,
    _first: tempfile::TempDir,
    _second: tempfile::TempDir,
    /// The other project's root as its session resolved it.
    pub(crate) second_root: String,
    _community: crate::core::policy::runtime::TestPolicyOverride,
    _data: crate::core::data_dir::IsolatedDataDir,
}

/// Plant [`CANARY`] in every store of a first project, then assert that
/// tool calls from a second project answer for it exactly as for
/// [`ABSENT`].
pub(crate) async fn canary_invisible_to_tools() -> CanaryWorld {
    let _data = crate::core::data_dir::isolated_data_dir();
    let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
    let (first, second) = git_projects();
    let owner = LeanCtxServer::new_with_project_root(Some(&first.path().to_string_lossy()));
    let first_root = owner.session.read().await.project_root.clone().unwrap();
    let source = std::path::Path::new(&first_root).join(format!("{CANARY}_vault.rs"));
    std::fs::write(
        &source,
        format!("pub fn {CANARY}_vault() -> u8 {{ {CANARY}_seal() }}\nfn {CANARY}_seal() -> u8 {{ 8 }}\n"),
    )
    .unwrap();
    let read = call_text(&owner, "ctx_read", &serde_json::json!({"path": source})).await;
    assert!(read.contains(CANARY), "{read}");
    let indexed = call_text(
        &owner,
        "ctx_search",
        &serde_json::json!({"action": "semantic", "query": CANARY}),
    )
    .await;
    assert!(indexed.contains(CANARY), "{indexed}");
    let graph =
        crate::core::call_graph::CallGraph::build(&crate::core::call_graph::CallGraphInputs {
            project_root: first_root.clone(),
            file_paths: vec![format!("{CANARY}_vault.rs")],
            ..Default::default()
        });
    graph.save().unwrap();
    let fact = format!("{CANARY} keys live in the vault");
    call_text(
        &owner,
        "ctx_knowledge",
        &serde_json::json!({"action": "remember", "category": "decision", "key": CANARY, "value": fact}),
    )
    .await;
    crate::core::policy::runtime::REQUEST_PROJECT.sync_scope(
        std::cell::RefCell::new(Some(std::path::PathBuf::from(&first_root))),
        || {
            let knowledge = crate::core::knowledge::ProjectKnowledge::load(&first_root)
                .expect("the canary fact was stored");
            let stored: Vec<_> = knowledge
                .facts
                .into_iter()
                .filter(|f| f.key == CANARY)
                .collect();
            assert_eq!(stored.len(), 1);
            crate::core::memory_lifecycle::archive_facts(&stored).unwrap();
        },
    );
    crate::core::shared_context::SharedContext::for_project(&first_root)
        .and_then(|store| store.put(format!("{CANARY} shared note"), "a1", "fact"))
        .unwrap();

    let other = LeanCtxServer::new_with_project_root(Some(&second.path().to_string_lossy()));
    let second_root = other.session.read().await.project_root.clone().unwrap();
    let probes = |term: &str| {
        [
            (
                "ctx_knowledge",
                serde_json::json!({"action": "recall", "query": term}),
            ),
            (
                "ctx_knowledge",
                serde_json::json!({"action": "search", "query": term}),
            ),
            ("ctx_search", serde_json::json!({"pattern": term})),
            (
                "ctx_search",
                serde_json::json!({"action": "semantic", "query": term}),
            ),
            (
                "ctx_memory",
                serde_json::json!({"action": "get", "query": term}),
            ),
            (
                "ctx_callgraph",
                serde_json::json!({"action": "callers", "symbol": format!("{term}_seal")}),
            ),
        ]
    };
    for ((tool, canary_args), (_, absent_args)) in probes(CANARY).into_iter().zip(probes(ABSENT)) {
        // A one-time setup tip may trail whichever call comes first; it
        // is session state, not project content.
        let untipped = |text: String| match text.find("\n\n--- tip:") {
            Some(at) => text[..at].to_string(),
            None => text,
        };
        let seen = untipped(call_text(&other, tool, &canary_args).await);
        let baseline = untipped(call_text(&other, tool, &absent_args).await);
        // An answer may echo the caller's own query term; whether it
        // leaks is decided against a term no project has ever held.
        assert_eq!(
            seen.replace(CANARY, "TERM"),
            baseline.replace(ABSENT, "TERM"),
            "{tool} answers differently for a foreign canary"
        );
    }
    // The content cache holds the first project's file: a read of it
    // from the other project is refused, not served from the cache.
    // `no-jail` turns the path jail off on purpose, so only with it on.
    #[cfg(not(feature = "no-jail"))]
    {
        let foreign_read =
            call_text(&other, "ctx_read", &serde_json::json!({"path": source})).await;
        assert!(
            !foreign_read.contains(&format!("{CANARY}_seal")),
            "{foreign_read}"
        );
    }

    CanaryWorld {
        _owner: owner,
        _other: other,
        _first: first,
        _second: second,
        second_root,
        _community,
        _data,
    }
}
