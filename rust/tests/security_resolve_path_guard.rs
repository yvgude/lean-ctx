fn extract_arm_body<'a>(src: &'a str, tool: &str) -> Option<&'a str> {
    let needle = format!("\"{tool}\" => {{");
    let start = src.find(&needle)?;
    let brace_start = src[start..].find('{')? + start;
    let mut depth = 0u32;
    for (i, ch) in src[brace_start..].char_indices() {
        match ch {
            '{' => depth += 1,
            '}' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    let end = brace_start + i + 1;
                    return Some(&src[brace_start..end]);
                }
            }
            _ => {}
        }
    }
    None
}

#[test]
fn server_fs_tools_use_resolve_path_chokepoint() {
    let sources = [
        include_str!("../src/server/dispatch/read_tools.rs"),
        include_str!("../src/server/dispatch/shell_tools.rs"),
        include_str!("../src/server/dispatch/session_tools.rs"),
        include_str!("../src/server/dispatch/utility_tools.rs"),
    ];
    let src = sources.join("\n");
    let tools = [
        "ctx_read",
        "ctx_multi_read",
        "ctx_tree",
        "ctx_search",
        "ctx_benchmark",
        "ctx_analyze",
        "ctx_smart_read",
        "ctx_delta",
        "ctx_edit",
        "ctx_fill",
        "ctx_outline",
        "ctx_semantic_search",
        "ctx_prefetch",
        "ctx_cache",
        "ctx_graph",
        "ctx_compress_memory",
        "ctx_handoff",
        "ctx_execute",
    ];
    for t in tools {
        let body = extract_arm_body(&src, t).unwrap_or_else(|| panic!("missing tool arm: {t}"));
        assert!(
            body.contains("resolve_path("),
            "{t} arm must call resolve_path() for path arguments"
        );
    }
}
