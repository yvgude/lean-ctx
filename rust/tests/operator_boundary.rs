// SPDX-License-Identifier: Apache-2.0

use lean_ctx::engine::operators::{
    CrpMode, OperatorCache, ReadRequest, SearchRequest, compress_shell_output, read, redact_output,
    search,
};

#[test]
fn public_operator_boundary_is_usable_without_local_tool_or_mcp_types() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("fixture.txt");
    std::fs::write(&file, "alpha needle omega\n").unwrap();
    let mut cache = OperatorCache::new();

    let read_result = read(
        &mut cache,
        ReadRequest {
            path: &file,
            project_root: dir.path(),
            mode: "full",
            fresh: false,
            crp_mode: CrpMode::Off,
            task: None,
            aggressiveness: None,
            protect: &[],
        },
    )
    .unwrap();
    assert!(read_result.content.contains("needle"));

    let search_result = search(SearchRequest {
        pattern: "needle",
        directory: dir.path(),
        project_root: dir.path(),
        include: Some("*.txt"),
        max_results: 10,
        crp_mode: CrpMode::Off,
        respect_gitignore: true,
        allow_secret_paths: false,
        anchored: false,
        exclude: None,
        exclude_pattern: None,
    })
    .unwrap();
    assert!(search_result.text.contains("needle"));

    assert_eq!(compress_shell_output("printf ok", "ok\n", 0), "ok\n");
    assert!(
        !redact_output("Authorization: Bearer abcdefghijklmnopqrstuvwxyz")
            .contains("abcdefghijklmnopqrstuvwxyz")
    );
}
