use serde_json::json;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn context_engine_call_tool_text_reads_file() {
    let dir = tempfile::tempdir().expect("tempdir");
    let file_path = dir.path().join("a.txt");
    std::fs::write(&file_path, "hello-engine\n").expect("write file");

    let engine = lean_ctx::engine::ContextEngine::with_project_root(dir.path());
    let out = engine
        .call_tool_text(
            "ctx_read",
            Some(json!({
                "path": file_path.to_string_lossy().to_string(),
                "mode": "full"
            })),
        )
        .await
        .expect("call tool");

    assert!(out.contains("hello-engine"));
}

/// #271 regression: many concurrent reads must all return through the
/// `spawn_blocking` dispatch path without starving the core workers or hanging.
/// Before the dispatch watchdog, a single hung handler could swallow its
/// response (client crash: "Cannot read properties of undefined (reading
/// 'invoke')"); this exercises the path under real concurrency.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn context_engine_handles_concurrent_reads_without_hang() {
    let dir = tempfile::tempdir().expect("tempdir");
    // Distinct files/contents per call so the session's auto-dedup (which
    // returns a compact stub for a *re-read* of the same file) does not mask the
    // body — here we assert each concurrent read returns real content.
    const CALLS: usize = 16;
    for i in 0..CALLS {
        std::fs::write(
            dir.path().join(format!("f{i}.txt")),
            format!("content-{i}-unique\n"),
        )
        .expect("write file");
    }

    let engine = std::sync::Arc::new(lean_ctx::engine::ContextEngine::with_project_root(
        dir.path(),
    ));
    let mut handles = Vec::with_capacity(CALLS);
    for i in 0..CALLS {
        let engine = engine.clone();
        let path = dir
            .path()
            .join(format!("f{i}.txt"))
            .to_string_lossy()
            .to_string();
        handles.push(tokio::spawn(async move {
            engine
                .call_tool_text("ctx_read", Some(json!({ "path": path, "mode": "full" })))
                .await
        }));
    }

    // A generous bound: the reads are trivial, so completion well under this
    // means no hang/starvation. A regression (dropped response) would time out.
    let texts = tokio::time::timeout(std::time::Duration::from_secs(30), async {
        let mut out = Vec::with_capacity(CALLS);
        for h in handles {
            out.push(h.await.expect("task joined").expect("tool call ok"));
        }
        out
    })
    .await
    .expect("concurrent reads must not hang");

    assert_eq!(texts.len(), CALLS);
    for text in texts {
        assert!(text.contains("content-"), "each read returns its file body");
    }
}
