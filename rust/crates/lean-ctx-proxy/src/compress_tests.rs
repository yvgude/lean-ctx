// SPDX-License-Identifier: Apache-2.0
use super::ccr;
#[cfg(feature = "http-server")]
use super::{
    BTreeMap, NO_PROJECT, Reader, bound_copy, delivered, observing, resolve_recovery,
    shape_tool_output,
};
use super::{
    RESEARCH_PROSE_CAP, RESEARCH_PROSE_CAP_ENV, RecoveryCopy, RecoveryId, carries_recovery_stub,
    compress_tool_result, compress_tool_result_gateway, extract_command_hint, infer_command,
    looks_like_prose, output_looks_like_build_failure, output_looks_like_test_run, recovery_copy,
    recovery_stub, reference_stub, research_prose_cap, shape_command_output,
};
use serial_test::serial;

/// #980/#985: search tool results must compress without corrupting source.
#[test]
fn search_tool_result_is_compressed_without_corrupting_source() {
    let _data = lean_ctx::core::data_dir::isolated_data_dir();
    let _community = lean_ctx::core::policy::runtime::TestPolicyOverride::set(None);
    // Above the verbatim budget (60 lines / 4 KiB): this one is pruned.
    let raw = (0..90)
        .map(|i| {
            format!(
                "src/h.go:{i}:func handler{i}(ctx context.Context) (api.Result, error) \
                 {{ return doWork(ctx) }}"
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let out = compress_tool_result(&raw, Some("search_files"));

    assert!(
        out.len() < raw.len(),
        "search results must still compress ({} -> {})",
        raw.len(),
        out.len()
    );
    for keyword in ["context.Context", "(api.Result, error)", "return"] {
        assert!(
            out.contains(keyword),
            "the terse dictionary rewrote `{keyword}` out of a search result:\n{out}"
        );
    }
}

#[test]
fn short_content_unchanged() {
    let short = "hello world";
    assert_eq!(compress_tool_result(short, None), short);
}

#[test]
fn empty_content_unchanged() {
    assert_eq!(compress_tool_result("", None), "");
    assert_eq!(compress_tool_result("   ", None), "   ");
}

#[test]
fn command_hint_extraction() {
    assert_eq!(
        extract_command_hint("$ cargo build\nCompiling foo"),
        Some("cargo build".to_string())
    );
    assert_eq!(extract_command_hint("no prefix here"), None);
}

#[test]
fn tool_name_inference() {
    assert_eq!(infer_command("some text", Some("bash_execute")), "shell");
    assert_eq!(infer_command("some text", Some("search_files")), "grep");
    assert_eq!(infer_command("some text", Some("unknown_tool")), "");
}

#[test]
fn lean_ctx_tool_results_pass_through_verbatim() {
    let _data = lean_ctx::core::data_dir::isolated_data_dir();
    let _community = lean_ctx::core::policy::runtime::TestPolicyOverride::set(None);
    // A ctx_shell result the tool already produced (raw=true, or its own
    // compression). The proxy must NOT re-compress / re-truncate it — that
    // was the #479 defect where `raw=true` was silently undone on the wire.
    let raw = (1..=120)
        .map(|i| format!("Line {i:04}: the quick brown fox jumps over the lazy dog"))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(raw.len() > 200);
    // Bare names AND the namespaced forms real MCP clients emit (Claude Code
    // `mcp__lean-ctx__ctx_shell`, colon-style `lean-ctx:ctx_read`).
    for tool in [
        "ctx_shell",
        "ctx_read",
        "ctx_search",
        "ctx_grep",
        "mcp__lean-ctx__ctx_shell",
        "lean-ctx:ctx_read",
    ] {
        assert_eq!(
            compress_tool_result(&raw, Some(tool)),
            raw,
            "{tool} output must pass through the proxy verbatim"
        );
    }
    // A foreign tool with identical output is still compressed: the proxy
    // keeps adding value for non-lean-ctx tools.
    assert_ne!(
        compress_tool_result(&raw, Some("bash")),
        raw,
        "foreign-tool output should still be compressed by the proxy"
    );
}

#[test]
fn cited_research_output_is_preserved_verbatim() {
    let cited = format!(
        "Rust is a language.\n\n---\nSource: Rust — https://x.com/a\n\
         Site: x.com · Retrieved: 2026-06-06T00:00:00Z\n{}",
        "Extra body line that would otherwise be touched. ".repeat(20)
    );
    assert_eq!(compress_tool_result(&cited, Some("ctx_url_read")), cited);
}

#[test]
fn prose_is_squeezed_and_deduped() {
    let _data = lean_ctx::core::data_dir::isolated_data_dir();
    let _community = lean_ctx::core::policy::runtime::TestPolicyOverride::set(None);
    let para = "Rust is a multi-paradigm systems programming language that \
                emphasizes performance, type safety, and fearless concurrency, \
                achieving memory safety without a garbage collector at runtime.";
    // Repeated paragraph (past the 60-line / 4 KiB verbatim budget) → dedup
    // keeps one.
    let input = format!("{}\n", [para; 40].join("\n\n"));
    assert!(input.len() > 600);
    let out = compress_tool_result(&input, Some("web_fetch"));
    assert_eq!(out.matches("fearless concurrency").count(), 1);
    assert!(out.contains("performance, type safety"));
}

#[test]
#[serial]
fn research_prose_cap_env_overrides_default() {
    let _lock = lean_ctx::core::data_dir::test_env_lock();
    lean_ctx::test_env::set_var(RESEARCH_PROSE_CAP_ENV, "1234");
    assert_eq!(research_prose_cap(), 1234);
    lean_ctx::test_env::remove_var(RESEARCH_PROSE_CAP_ENV);
}

#[test]
#[serial]
fn research_prose_cap_env_invalid_falls_back() {
    let _lock = lean_ctx::core::data_dir::test_env_lock();
    for value in ["", "not_a_number", "0"] {
        lean_ctx::test_env::set_var(RESEARCH_PROSE_CAP_ENV, value);
        assert_eq!(research_prose_cap(), RESEARCH_PROSE_CAP);
    }
    lean_ctx::test_env::remove_var(RESEARCH_PROSE_CAP_ENV);
}

#[test]
fn code_output_is_not_treated_as_prose() {
    let code = "fn main() {\n    let x = vec![1, 2, 3];\n    \
                for i in &x { println!(\"{}\", i); }\n}\n"
        .repeat(20);
    assert!(!looks_like_prose(&code));
}

#[test]
fn shell_log_is_not_treated_as_prose() {
    let log = "$ cargo build\n   Compiling foo v0.1.0\n    Finished dev\n".repeat(20);
    assert!(!looks_like_prose(&log));
}

#[test]
fn foreign_shell_build_failure_preserved_verbatim() {
    // A forge/pi-style shell tool: the name says "shell" and the output has
    // no `$ cmd` hint, so the engine's command-gated guards cannot fire. The
    // compiler error must still reach the model intact for a bug-fix task.
    let mut log = String::from("gcc -O2 -c src/versioncmp.c -o versioncmp.o\n");
    log.push_str("src/versioncmp.c: In function 'version_cmp':\n");
    log.push_str(
        "src/versioncmp.c:142:17: error: invalid operands to binary < (have 'char *' and 'int')\n",
    );
    for i in 0..40 {
        log.push_str(&format!("  note: expansion context line {i}\n"));
    }
    log.push_str("make: *** [Makefile:23: versioncmp.o] Error 1\n");

    let out = compress_tool_result(&log, Some("shell"));
    assert!(
        out.contains("versioncmp.c:142:17: error:"),
        "compiler error must survive the proxy"
    );
    assert!(
        out.contains("make: ***"),
        "make failure summary must survive"
    );
}

#[test]
fn foreign_shell_test_failure_preserved_verbatim() {
    let mut log = String::from("running 3 tests\n");
    log.push_str("test version::tests::sorts_numeric ... FAILED\n");
    for i in 0..40 {
        log.push_str(&format!("note line {i} with some filler content here\n"));
    }
    log.push_str("test result: FAILED. 2 passed; 1 failed; 0 ignored\n");

    let out = compress_tool_result(&log, Some("bash"));
    assert!(
        out.contains("test result: FAILED"),
        "test summary must survive the proxy"
    );
    assert!(out.contains("sorts_numeric ... FAILED"));
}

#[test]
fn plain_shell_log_not_forced_verbatim() {
    let log = "Listening on port 8080\nRequest received from 10.0.0.2\n".repeat(20);
    assert!(!output_looks_like_test_run(&log));
    assert!(!output_looks_like_build_failure(&log));
}

fn big_compressible_log() -> String {
    (1..=400)
        .map(|i| format!("[info] processed item {i:04} ok"))
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn live_compression_is_recoverable_via_ccr_handle() {
    let _lock = lean_ctx::core::data_dir::test_env_lock();
    let _community = lean_ctx::core::policy::runtime::TestPolicyOverride::set(None);
    let log = big_compressible_log();
    let out = compress_tool_result(&log, Some("bash"));
    assert!(
        out.len() < log.len(),
        "a large foreign log must be compressed"
    );

    // The compressed result carries the content-addressed handle, and the
    // handle points at the *verbatim* original — live compression is now
    // non-lossy (#482), recoverable with a plain native file read.
    let handle = ccr::persist(&log).expect("same content -> same handle");
    assert!(
        out.ends_with(&super::tee_hint(&handle)),
        "CCR handle must be embedded: {out}"
    );
    let recovered = std::fs::read_to_string(&handle).expect("tee file readable");
    assert!(
        recovered.contains("processed item 0007 ok")
            && recovered.contains("processed item 0400 ok"),
        "verbatim original must be fully recoverable"
    );
}

/// An active policy keeps no originless recovery copy. The fresh result
/// must then arrive whole, not shortened without a handle.
#[test]
fn live_compression_without_a_recovery_copy_keeps_the_original() {
    let _data = lean_ctx::core::data_dir::isolated_data_dir();
    let _policy = lean_ctx::core::policy::runtime::TestPolicyOverride::set(Some(
        lean_ctx::core::policy::load("name='no-tee'\nversion='1.0.0'\ndescription='test'\n")
            .unwrap(),
    ));
    let log = big_compressible_log();
    assert!(ccr::persist(&log).is_none());
    assert_eq!(compress_tool_result(&log, Some("bash")), log);
    assert_eq!(compress_tool_result_gateway(&log, Some("bash")), log);
}

/// Governed mode protects without a policy pack: the proxy stores no
/// recovery copy of a request body there, and with no source-bound copy
/// to point at the fresh result arrives whole. Developer mode still
/// compresses with its tee handle.
#[test]
fn governed_mode_stores_no_recovery_copy_and_keeps_the_original() {
    let _data = lean_ctx::core::data_dir::isolated_data_dir();
    let _community = lean_ctx::core::policy::runtime::TestPolicyOverride::set(None);
    let config = lean_ctx::core::paths::config_dir_read_only().expect("config directory");
    std::fs::create_dir_all(&config).expect("config directory");
    let mode = |mode: &str| {
        std::fs::write(
            config.join("config.toml"),
            format!("[context_gateway]\nmode = \"{mode}\"\n"),
        )
        .expect("gateway config");
    };
    let log = big_compressible_log();

    mode("governed");
    assert!(ccr::persist(&log).is_none());
    assert!(recovery_copy(&log).is_none());
    assert_eq!(compress_tool_result(&log, Some("bash")), log);
    assert_eq!(compress_tool_result_gateway(&log, Some("bash")), log);

    mode("developer");
    assert!(matches!(recovery_copy(&log), Some(RecoveryCopy::Tee(_))));
    let compressed = compress_tool_result(&log, Some("bash"));
    assert!(
        compressed.len() < log.len(),
        "developer mode still compresses"
    );
}

#[cfg(feature = "http-server")]
/// Governed mode with `root` as the proxy's configured project root.
fn governed_project(root: &std::path::Path) {
    let config = lean_ctx::core::paths::config_dir_read_only().expect("config directory");
    std::fs::create_dir_all(&config).expect("config directory");
    std::fs::write(
        config.join("config.toml"),
        format!(
            "[context_gateway]\nmode = \"governed\"\n\n[proxy]\nproject_root = '{}'\n",
            root.display()
        ),
    )
    .expect("gateway config");
}

#[cfg(feature = "http-server")]
/// The test plays the Engine for the project `root`: an admitted read of
/// `source`, recorded as delivered with its file lineage. Returns the
/// delivered text.
fn engine_delivers_file(root: &std::path::Path, source: &std::path::Path) -> String {
    super::super::request_project::in_project(root, || {
        let path = source.to_str().expect("utf-8 path");
        let admitted = lean_ctx::core::rooted_read::read_file_for_tool_rooted(
            path,
            root.to_str().expect("utf-8 root"),
            "ctx_read",
        )
        .expect("admitted read");
        let authority = lean_ctx::core::archive::authority::ArchiveAuthority::file(
            root, path, "ctx_read", &admitted,
        )
        .expect("protection is active: the read is bound");
        assert!(delivered::record_lineage(
            &delivered::digest(&admitted),
            &delivered::Lineage::of_files(authority)
        ));
        admitted
    })
}

fn recovery_id_in(sent: &str) -> String {
    let (_, rest) = sent
        .rsplit_once("ctx_expand(id=\"")
        .expect("a recovery stub");
    rest.split('"').next().expect("an id").to_owned()
}

#[cfg(feature = "http-server")]
/// [real register and store] Under protection a block is compressed only
/// with a recovery copy bound to its lineage. A block the Engine
/// delivered from an admitted project file is compressed; its stub
/// resolves to the exact original bytes; the block that is sent is on
/// record under the same lineage. After the source is deleted the id
/// resolves to a typed refusal and nothing else, and the same block is
/// no longer compressed. A block without a lineage is never compressed.
#[test]
fn file_lineage_block_is_compressed_with_a_resolvable_copy_until_its_source_is_gone() {
    let _data = lean_ctx::core::data_dir::isolated_data_dir();
    let _community = lean_ctx::core::policy::runtime::TestPolicyOverride::set(None);
    let directory = tempfile::tempdir().expect("project");
    let root = directory.path().canonicalize().expect("canonical root");
    governed_project(&root);
    let source = root.join("build.log");
    std::fs::write(&source, big_compressible_log()).expect("source");
    let original = engine_delivers_file(&root, &source);

    let sent = compress_tool_result(&original, Some("bash"));
    assert!(sent.len() < original.len(), "compressed under protection");
    let id = recovery_id_in(&sent);
    assert_eq!(resolve_recovery(&id), Some(Ok(original.clone())));
    assert_eq!(
        compress_tool_result(&original, Some("bash")),
        sent,
        "stable"
    );
    let derived = super::super::request_project::in_project(&root, || {
        delivered::lineage_of(&delivered::digest(&sent))
    });
    assert!(
        derived.is_some(),
        "the sent block keeps the lineage of its origin"
    );
    assert_eq!(
        compress_tool_result_gateway(&original, Some("bash")),
        original,
        "a gateway audience cannot follow the stub and gets the original"
    );

    let unknown = format!("{original}one line the Engine never delivered\n");
    assert_eq!(compress_tool_result(&unknown, Some("bash")), unknown);
    assert_eq!(resolve_recovery("not-an-id"), None);
    assert_eq!(
        resolve_recovery(&"0".repeat(64)),
        Some(Err("recovery.missing"))
    );

    std::fs::remove_file(&source).expect("delete the source");
    assert_eq!(
        resolve_recovery(&id),
        Some(Err("recovery.source_no_longer_admitted"))
    );
    assert_eq!(
        compress_tool_result(&original, Some("bash")),
        original,
        "without a copy that can be stored now the block stays whole"
    );
}

#[cfg(feature = "http-server")]
/// [real register and store] A source the project policy restricts is in
/// no lineage: the Engine records nothing for it, so its text is not
/// compressed and nothing is stored. And without a project of the proxy
/// nothing is looked up at all.
#[test]
fn restricted_source_and_missing_project_leave_the_block_whole() {
    let _data = lean_ctx::core::data_dir::isolated_data_dir();
    let _policy = lean_ctx::core::policy::runtime::TestPolicyOverride::set(Some(
        lean_ctx::core::policy::load(
            "name='recovery-test'\nversion='1.0.0'\ndescription='test'\n[filters]\n\
             classification='block'\nblocked_labels=['CONFIDENTIAL']\n",
        )
        .expect("policy"),
    ));
    let directory = tempfile::tempdir().expect("project");
    let root = directory.path().canonicalize().expect("canonical root");
    governed_project(&root);
    let restricted = format!("CONFIDENTIAL\n{}", big_compressible_log());
    assert!(bound_copy(&restricted).is_err());
    assert_eq!(compress_tool_result(&restricted, Some("bash")), restricted);
    assert!(
        lean_ctx::core::archive::bound_recovery_id(&restricted).is_none(),
        "nothing was stored"
    );

    let source = root.join("build.log");
    std::fs::write(&source, big_compressible_log()).expect("source");
    let original = engine_delivers_file(&root, &source);
    let config = lean_ctx::core::paths::config_dir_read_only().expect("config directory");
    std::fs::write(
        config.join("config.toml"),
        "[context_gateway]\nmode = \"governed\"\n",
    )
    .expect("gateway config");
    assert_eq!(bound_copy(&original).err(), Some(NO_PROJECT));
    assert_eq!(compress_tool_result(&original, Some("bash")), original);
    assert_eq!(resolve_recovery(&"0".repeat(64)), Some(Err(NO_PROJECT)));
}

#[cfg(feature = "http-server")]
/// [real registers and store] Prepared context from a caller-supplied
/// source that is presented and admitted now: compressed, and its stub
/// resolves to the exact original. (That a presentation older than its
/// validity, or one under another policy, refuses is the Engine's rule;
/// it needs a clock and is not exercised from here.)
#[test]
fn presented_lineage_block_is_compressed_and_resolvable_while_presented() {
    let _data = lean_ctx::core::data_dir::isolated_data_dir();
    let _community = lean_ctx::core::policy::runtime::TestPolicyOverride::set(None);
    let directory = tempfile::tempdir().expect("project");
    let root = directory.path().canonicalize().expect("canonical root");
    governed_project(&root);
    let original = big_compressible_log();
    super::super::request_project::in_project(&root, || {
        let binding = lean_ctx::core::archive::presented::PresentedBinding::admitted_now(
            &root,
            "object://caller/build-log",
            "source-one",
            "sha256:7d1a54127b222502f5b79b5fb0803061152a44f92b37e23c6527baf665d4da9a",
        )
        .expect("the policy identity of the project resolves");
        assert!(lean_ctx::core::archive::presented::record_presentation(
            &binding
        ));
        assert!(delivered::record_lineage(
            &delivered::digest(&original),
            &delivered::Lineage::of_presented(vec![binding])
        ));
    });
    let sent = compress_tool_result(&original, Some("bash"));
    assert!(sent.len() < original.len(), "compressed under protection");
    assert_eq!(resolve_recovery(&recovery_id_in(&sent)), Some(Ok(original)));
}

#[test]
fn live_compression_output_is_byte_stable_across_turns() {
    let _lock = lean_ctx::core::data_dir::test_env_lock();
    let _community = lean_ctx::core::policy::runtime::TestPolicyOverride::set(None);
    let log = big_compressible_log();
    let a = compress_tool_result(&log, Some("bash"));
    let b = compress_tool_result(&log, Some("bash"));
    assert_eq!(
        a, b,
        "the CCR handle is content-addressed, so the rewritten result must be \
         byte-identical across turns (provider cache prefix stays valid, #448)"
    );
}

#[cfg(feature = "http-server")]
/// Contract test for #702, pinned to LiteLLM's marker regex: the headroom
/// guardrail scans compressed text with `hash=([a-f0-9]{24})`
/// (BerriAI/litellm#31681). If our gateway stub ever drifts from that
/// shape, the CCR agentic loop silently stops firing — this test fails
/// first.
#[test]
fn gateway_stub_matches_litellm_marker_regex_and_retrieves() {
    let _lock = lean_ctx::core::data_dir::test_env_lock();
    let _community = lean_ctx::core::policy::runtime::TestPolicyOverride::set(None);
    let log = big_compressible_log();
    let out = compress_tool_result_gateway(&log, Some("bash"));
    assert!(out.len() < log.len(), "gateway funnel must still compress");

    // Exactly the pattern LiteLLM compiles (`_HASH_PATTERN`).
    let litellm_regex = regex::Regex::new(r"hash=([a-f0-9]{24})").unwrap();
    let captured = litellm_regex
        .captures(&out)
        .unwrap_or_else(|| panic!("gateway stub must carry a hash= marker: {out}"))
        .get(1)
        .unwrap()
        .as_str();
    assert_eq!(captured, ccr::litellm_hash(&log));

    // The captured hash resolves through the /v1/retrieve/{hash} resolver
    // to the verbatim original — the full guardrail round-trip.
    let recovered = ccr::retrieve_litellm(captured).expect("captured hash must resolve");
    assert!(
        recovered.contains("processed item 0007 ok")
            && recovered.contains("processed item 0400 ok"),
        "retrieve must return the verbatim original"
    );

    // Byte-stable across calls (#498): the marker is content-addressed.
    assert_eq!(out, compress_tool_result_gateway(&log, Some("bash")));

    // The functional CCR line must survive the savings-footer strip that
    // /v1/compress applies to every payload.
    assert_eq!(
        lean_ctx::core::protocol::strip_trailing_savings_footer(&out),
        out
    );
}

#[test]
fn gateway_stub_absent_for_passthrough_and_ctx_output() {
    let _lock = lean_ctx::core::data_dir::test_env_lock();
    // Below the compress floor: no marker.
    assert!(!compress_tool_result_gateway("short output", Some("bash")).contains("hash="));
    // lean-ctx tool output passes through verbatim — a gateway must never
    // see a retrieval marker for content that was not rewritten.
    let raw = (1..=120)
        .map(|i| format!("Line {i:04}: lorem ipsum dolor sit amet consectetur"))
        .collect::<Vec<_>>()
        .join("\n");
    let out = compress_tool_result_gateway(&raw, Some("ctx_shell"));
    assert_eq!(out, raw);
}

#[test]
fn small_or_passthrough_output_gets_no_ccr_handle() {
    let _lock = lean_ctx::core::data_dir::test_env_lock();
    // Below the 200-char compress floor: passes through, no handle.
    let tiny = "ok\n".repeat(10);
    assert!(!compress_tool_result(&tiny, Some("bash")).contains("full original at"));
    // lean-ctx tool output passes through verbatim (no handle either).
    let raw = (1..=120)
        .map(|i| format!("Line {i:04}: lorem ipsum dolor sit amet consectetur"))
        .collect::<Vec<_>>()
        .join("\n");
    let out = compress_tool_result(&raw, Some("ctx_shell"));
    assert_eq!(out, raw, "lean-ctx tool result must stay verbatim (no CCR)");
}

/// The stub a model reads is a pure function of the recovery id, pinned
/// here byte for byte: `ctx_expand` takes exactly this `id` (64 lowercase
/// hex, `RecoveryId::parse`) and the selectors the stub names. Neither
/// stub has the shape of a savings footer, and the gateway one carries no
/// `hash=` a LiteLLM guardrail would send to the tee route.
#[test]
fn recovery_stub_text_is_pinned_for_a_given_id() {
    let hex = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    let id = RecoveryId::parse(hex).expect("canonical id");
    let stub = recovery_stub(&id);
    assert_eq!(
        stub,
        "[lean-ctx recovery: full original archived — ctx_expand(id=\"0123456789abcdef\
         0123456789abcdef0123456789abcdef0123456789abcdef\", head=N|search=\"…\"|\
         json_path=\"…\") for a slice]"
    );
    assert_eq!(recovery_id_in(&stub), hex);
    assert!(RecoveryId::parse(&recovery_id_in(&stub)).is_some());

    let gateway = reference_stub(&id);
    assert_eq!(
        gateway,
        "[lean-ctx CCR: full original archived — GET /v1/references/0123456789abcdef\
         0123456789abcdef0123456789abcdef0123456789abcdef on this gateway returns it \
         while its sources are admitted]"
    );
    assert!(!gateway.contains("hash="));
    for line in [&stub, &gateway] {
        let block = format!("shortened body\n{line}");
        assert_eq!(
            lean_ctx::core::protocol::strip_trailing_savings_footer(&block),
            block,
            "a footer strip must not eat the way back"
        );
        assert!(carries_recovery_stub(&block));
    }
    assert!(!carries_recovery_stub(
        "shortened body\n[lean-ctx: saved 40 tokens]"
    ));
}

/// Regression: a block that already ends with its way back is not
/// compressed again. Before, the second pass of the next turn (the
/// gateway funnel of the forward pipeline) stripped a local stub as if it
/// were a savings footer, and the block left shortened without an id.
#[test]
fn a_block_that_carries_its_stub_passes_every_later_pass_untouched() {
    let _data = lean_ctx::core::data_dir::isolated_data_dir();
    let _community = lean_ctx::core::policy::runtime::TestPolicyOverride::set(None);
    let id = RecoveryId::parse(&"a".repeat(64)).expect("canonical id");
    let tee = "[lean-ctx: full original at /state/tee/proxy_0123456789abcdef.log — read it \
               directly (no MCP), or ctx_expand(id=\"proxy_0123456789abcdef.log\") for a slice]";
    for stub in [recovery_stub(&id), reference_stub(&id), tee.to_owned()] {
        let block = format!("{}\n{stub}", big_compressible_log());
        assert_eq!(compress_tool_result(&block, Some("bash")), block);
        assert_eq!(compress_tool_result_gateway(&block, None), block);
    }
}

#[cfg(feature = "http-server")]
/// Under protection every block that stays whole for want of a recovery
/// copy is counted once, with its content-free code, for the route that
/// observes the pass — however often a pass tries it. Developer mode
/// notes nothing.
#[test]
fn blocks_kept_whole_are_counted_once_with_their_code() {
    let _data = lean_ctx::core::data_dir::isolated_data_dir();
    let _community = lean_ctx::core::policy::runtime::TestPolicyOverride::set(None);
    let config = lean_ctx::core::paths::config_dir_read_only().expect("config directory");
    std::fs::create_dir_all(&config).expect("config directory");
    let mode = |mode: &str| {
        std::fs::write(
            config.join("config.toml"),
            format!("[context_gateway]\nmode = \"{mode}\"\n"),
        )
        .expect("gateway config");
    };
    let log = big_compressible_log();
    let second = format!("{log}\n[info] one more line");
    let third = format!("{log}\n[info] and another");

    mode("governed");
    let ((), kept) = observing(Reader::Model, || {
        for _ in 0..2 {
            assert_eq!(compress_tool_result(&log, Some("bash")), log);
        }
        assert_eq!(compress_tool_result(&second, Some("bash")), second);
        // The gateway funnel on a model's route: the first code stands
        // for a block already counted; a new block gets the funnel's.
        assert_eq!(compress_tool_result_gateway(&log, Some("bash")), log);
        assert_eq!(compress_tool_result_gateway(&third, Some("bash")), third);
    });
    assert_eq!(
        kept.counts(),
        BTreeMap::from([
            ("recovery.handle_not_followable", 1),
            ("recovery.no_project", 2),
        ])
    );
    assert_eq!(
        kept.codes().as_deref(),
        Some("recovery.handle_not_followable,recovery.no_project")
    );
    assert_eq!(
        kept.summary(),
        "recovery.handle_not_followable=1, recovery.no_project=2"
    );

    mode("developer");
    let (compressed, kept) = observing(Reader::Model, || compress_tool_result(&log, Some("bash")));
    assert!(compressed.len() < log.len());
    assert!(kept.is_empty());
    assert_eq!(kept.codes(), None);
}

/// Output of a successful `cargo build` as a host-native shell tool hands it
/// to `ctx_shape`: progress lines the engine folds, then the verdict.
fn noisy_build_output() -> String {
    use std::fmt::Write as _;
    let mut noisy = String::new();
    for i in 0..400 {
        let _ = writeln!(noisy, "   Compiling crate-{i} v0.1.{i} (/tmp/x)");
    }
    noisy.push_str("    Finished `dev` profile [unoptimized + debuginfo] target(s) in 42.17s\n");
    noisy
}

/// `shape_command_output` (the entry point of `ctx_shape`) follows the rule
/// of the wire funnel: an output that already ends with its way back is
/// returned whole, with or without a known exit code — shaping it again
/// could drop the handle.
#[test]
fn shaping_returns_an_output_that_carries_its_stub_whole() {
    let _data = lean_ctx::core::data_dir::isolated_data_dir();
    let _community = lean_ctx::core::policy::runtime::TestPolicyOverride::set(None);
    let noisy = noisy_build_output();
    assert!(
        shape_command_output("cargo build", &noisy, Some(0)).len() < noisy.len(),
        "without a stub this output is shaped"
    );
    let id = RecoveryId::parse(&"a".repeat(64)).expect("canonical id");
    let tee = "[lean-ctx: full original at /state/tee/proxy_0123456789abcdef.log — read it \
               directly (no MCP), or ctx_expand(id=\"proxy_0123456789abcdef.log\") for a slice]";
    for stub in [recovery_stub(&id), reference_stub(&id), tee.to_owned()] {
        let block = format!("{noisy}{stub}");
        assert_eq!(shape_command_output("cargo build", &block, Some(0)), block);
        assert_eq!(shape_command_output("cargo build", &block, None), block);
    }
}

#[cfg(feature = "http-server")]
/// `shape_command_output` under protection: the route knows no tool list of
/// the host that reads the result, so no stub may be issued and no recovery
/// copy is asked for — the output arrives whole, the reason is counted, and
/// no tee file is written. Developer mode shapes it and names its tee handle.
#[test]
fn shaping_under_protection_without_a_recovery_copy_keeps_the_output_whole() {
    let _data = lean_ctx::core::data_dir::isolated_data_dir();
    let _community = lean_ctx::core::policy::runtime::TestPolicyOverride::set(None);
    let config = lean_ctx::core::paths::config_dir_read_only().expect("config directory");
    std::fs::create_dir_all(&config).expect("config directory");
    let mode = |mode: &str| {
        std::fs::write(
            config.join("config.toml"),
            format!("[context_gateway]\nmode = \"{mode}\"\n"),
        )
        .expect("gateway config");
    };
    let noisy = noisy_build_output();

    mode("governed");
    for exit_code in [Some(0), None] {
        let (shaped, kept) = observing(Reader::Model, || {
            shape_command_output("cargo build", &noisy, exit_code)
        });
        assert_eq!(shaped, noisy, "whole (exit code {exit_code:?})");
        assert_eq!(
            kept.counts(),
            BTreeMap::from([("recovery.handle_not_followable", 1)])
        );
    }
    let tee = lean_ctx::core::paths::state_dir_read_only()
        .expect("state directory")
        .join("tee");
    assert!(
        !std::fs::read_dir(&tee).is_ok_and(|mut entries| entries.next().is_some()),
        "no originless copy is written under protection"
    );

    mode("developer");
    let (shaped, kept) = observing(Reader::Model, || {
        shape_command_output("cargo build", &noisy, Some(0))
    });
    assert!(shaped.len() < noisy.len(), "developer mode shapes it");
    assert!(shaped.contains("Finished"), "{shaped}");
    assert!(shaped.contains("full original"), "{shaped}");
    assert!(kept.is_empty());
}

#[cfg(feature = "http-server")]
/// [real registers and store] `shape_command_output` in governed mode WITH
/// a project and a known, admitted lineage of the output — the case in
/// which a copy could be bound. The route holds no tool list of the host,
/// so the name of the expand tool is not known to be followable: no stub
/// is issued under the bare name, the output stays whole with
/// `recovery.handle_not_followable`, and no copy is stored for it.
#[test]
fn shaping_under_protection_issues_no_stub_without_a_known_host_tool() {
    let _data = lean_ctx::core::data_dir::isolated_data_dir();
    let _community = lean_ctx::core::policy::runtime::TestPolicyOverride::set(None);
    let directory = tempfile::tempdir().expect("project");
    let root = directory.path().canonicalize().expect("canonical root");
    governed_project(&root);
    let noisy = noisy_build_output();
    super::super::request_project::in_project(&root, || {
        let binding = lean_ctx::core::archive::presented::PresentedBinding::admitted_now(
            &root,
            "object://caller/build-output",
            "source-one",
            "sha256:7d1a54127b222502f5b79b5fb0803061152a44f92b37e23c6527baf665d4da9a",
        )
        .expect("the policy identity of the project resolves");
        assert!(lean_ctx::core::archive::presented::record_presentation(
            &binding
        ));
        assert!(delivered::record_lineage(
            &delivered::digest(&noisy),
            &delivered::Lineage::of_presented(vec![binding])
        ));
    });

    for exit_code in [Some(0), None] {
        let (shaped, kept) = observing(Reader::Model, || {
            shape_command_output("cargo build", &noisy, exit_code)
        });
        assert_eq!(shaped, noisy, "whole (exit code {exit_code:?})");
        assert!(!shaped.contains("[lean-ctx recovery: "));
        assert!(!shaped.contains("ctx_expand"));
        assert_eq!(
            kept.counts(),
            BTreeMap::from([("recovery.handle_not_followable", 1)])
        );
    }
    assert!(
        lean_ctx::core::archive::bound_recovery_id(&noisy).is_none(),
        "nothing was stored for a stub that was never issued"
    );
    assert!(
        bound_copy(&noisy).is_ok(),
        "the lineage is known: a copy could have been bound"
    );
}

#[cfg(feature = "http-server")]
/// [real registers and store] `shape_tool_output` — the entry point of
/// `ctx_shape` for a host tool without a known command line — in governed
/// mode WITH a project and a known, admitted lineage. The same text handed
/// to the compressor directly, outside any pass, leaves with a stub under
/// the bare name; through this route no host tool list is known, so no stub
/// is issued: the text stays whole with `recovery.handle_not_followable`.
#[test]
fn shaping_a_tool_output_under_protection_issues_no_stub_without_a_known_host_tool() {
    let _data = lean_ctx::core::data_dir::isolated_data_dir();
    let _community = lean_ctx::core::policy::runtime::TestPolicyOverride::set(None);
    let directory = tempfile::tempdir().expect("project");
    let root = directory.path().canonicalize().expect("canonical root");
    governed_project(&root);
    let original = big_compressible_log();
    super::super::request_project::in_project(&root, || {
        let binding = lean_ctx::core::archive::presented::PresentedBinding::admitted_now(
            &root,
            "object://caller/build-log",
            "source-one",
            "sha256:7d1a54127b222502f5b79b5fb0803061152a44f92b37e23c6527baf665d4da9a",
        )
        .expect("the policy identity of the project resolves");
        assert!(lean_ctx::core::archive::presented::record_presentation(
            &binding
        ));
        assert!(delivered::record_lineage(
            &delivered::digest(&original),
            &delivered::Lineage::of_presented(vec![binding])
        ));
    });

    let (shaped, kept) = observing(Reader::Model, || {
        shape_tool_output(&original, Some("Terminal"))
    });
    assert_eq!(shaped, original, "whole");
    assert!(!shaped.contains("[lean-ctx recovery: "));
    assert_eq!(
        kept.counts(),
        BTreeMap::from([("recovery.handle_not_followable", 1)])
    );

    let direct = compress_tool_result(&original, Some("Terminal"));
    assert!(
        direct.len() < original.len() && direct.contains("[lean-ctx recovery: "),
        "the same text is compressible with a stub where a tool is assumed: {direct}"
    );
}

/// Lines `1..=n` of a short-lined listing, as `head -n | cut` or `sed -n`
/// print them — the bench shapes that left as `[N lines omitted]` before.
fn listing(n: usize, width: usize) -> String {
    (1..=n)
        .map(|i| {
            format!(
                "{i:04} {}",
                "entry of a bounded view"
                    .chars()
                    .cycle()
                    .take(width)
                    .collect::<String>()
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The size floor holds for every tool, not only shell tools: 25 to 60
/// lines (the bench: `grep|head -60` 37 lines, `sed -n|cut` 25 and 42,
/// `head -40|cut; ls` 50, an ArtifactData listing of 40) or up to 4 KiB of
/// any shape reach the model whole, through both funnels.
#[test]
fn results_within_the_verbatim_budget_stay_whole_for_every_tool() {
    let _data = lean_ctx::core::data_dir::isolated_data_dir();
    let _community = lean_ctx::core::policy::runtime::TestPolicyOverride::set(None);
    let tools = [
        Some("Bash"),
        Some("ArtifactData"),
        Some("Artifact"),
        Some("Read"),
        Some("search_files"),
        None,
    ];
    let mut shapes: Vec<String> = [25, 37, 42, 46, 50, 60]
        .into_iter()
        .map(|n| listing(n, 60))
        .collect();
    // A 70-line JSON document under 4 KiB (`git show …:file.json`).
    shapes.push(format!(
        "{{\n{}\n}}",
        (1..=68)
            .map(|i| format!("  \"k{i}\": {i},"))
            .collect::<Vec<_>>()
            .join("\n")
    ));
    for shape in &shapes {
        assert!(
            shape.len() <= super::VERBATIM_LINES_MAX_BYTES,
            "fixture within the line rule"
        );
        for tool in tools {
            assert_eq!(&compress_tool_result(shape, tool), shape, "{tool:?}");
            assert_eq!(
                &compress_tool_result_gateway(shape, tool),
                shape,
                "{tool:?}"
            );
        }
        assert_eq!(&shape_command_output("ls -la", shape, Some(0)), shape);
    }

    // Past both rules the funnel compresses again, with its way back.
    let long = big_compressible_log();
    assert!(long.len() > super::VERBATIM_MAX_BYTES && long.lines().count() > 60);
    let out = compress_tool_result(&long, Some("ArtifactData"));
    assert!(
        out.len() < long.len() && out.contains("full original at"),
        "{out}"
    );
    // Few huge lines are no bounded view: the line rule stops at 16 KiB.
    let minified = format!("{}\n", "{\"a\":1,\"b\":[1,2,3]}".repeat(1200)).repeat(2);
    assert!(!super::fits_verbatim(&minified));
}

/// Regression (bench `head -40 x.md | cut; ls`): a lossy result whose
/// shrink stays below the tee floor left without any recovery hint. Now no
/// result is shortened without a way back: below the floor the original
/// leaves whole; above it the hint names an exact re-read command.
#[test]
fn a_shortened_result_never_leaves_without_its_re_read_path() {
    let _data = lean_ctx::core::data_dir::isolated_data_dir();
    let _community = lean_ctx::core::policy::runtime::TestPolicyOverride::set(None);
    let original = listing(80, 40);
    let lossy = format!(
        "{}\n[8 lines omitted]\n{}",
        original.lines().take(5).collect::<Vec<_>>().join("\n"),
        original.lines().skip(13).collect::<Vec<_>>().join("\n")
    );
    let shrink = original.len() - lossy.len();
    assert!(
        shrink < ccr::MIN_TEE_BYTES,
        "the hint-less case: shrink {shrink}"
    );
    assert_eq!(
        super::attach_ccr(&original, lossy, super::CcrAudience::Local),
        original,
        "a small shrink is not worth a loss without a way back"
    );

    // Every shortened result of the funnel carries the exact command. The
    // expected hint is built by the code's own helper, so the platform's
    // path shape (`\` and a drive on Windows) is the code's to quote.
    let log = big_compressible_log();
    let handle = ccr::persist(&log).expect("tee handle");
    let hint = super::tee_hint(&handle);
    let name = std::path::Path::new(&handle)
        .file_name()
        .and_then(|name| name.to_str())
        .expect("tee file name");
    assert_eq!(
        hint.matches(name).count(),
        2,
        "the copy is named once, plus its ctx_expand id: {hint}"
    );
    assert!(
        hint.contains(&format!("ctx_expand(id=\"{name}\", ")),
        "{hint}"
    );
    for tool in [Some("Bash"), Some("ArtifactData"), None] {
        let out = compress_tool_result(&log, tool);
        assert!(out.len() < log.len(), "{tool:?}");
        assert!(out.ends_with(&hint), "{out}");
        assert!(carries_recovery_stub(&out), "a later pass leaves it alone");
        assert_eq!(out, compress_tool_result(&log, tool), "byte-stable (#498)");
    }
}

/// The hint names its copy home-relative where it can (no account name) and
/// by its shell-quoted absolute path elsewhere.
#[test]
fn the_tee_hint_names_its_copy_once() {
    let outside = "/var/state/lean-ctx/tee/proxy_ab12.log";
    if crate::session_elide::home_location(std::path::Path::new(outside)).is_none() {
        assert_eq!(
            super::tee_hint(outside),
            "[lean-ctx: full original at /var/state/lean-ctx/tee/proxy_ab12.log — `cat` it to \
             re-read it exactly (no MCP), or ctx_expand(id=\"proxy_ab12.log\", \
             head=N|search=\"…\"|json_path=\"…\") for a slice]"
        );
    }
    if let Some(home) = std::env::var_os("HOME").filter(|home| !home.is_empty()) {
        let inside = std::path::Path::new(&home).join(".local/state/lean-ctx/tee/proxy_ab12.log");
        let hint = super::tee_hint(inside.to_str().expect("utf-8 home"));
        assert!(
            hint.starts_with(
                "[lean-ctx: full original at ~/.local/state/lean-ctx/tee/proxy_ab12.log — "
            ),
            "{hint}"
        );
        assert!(!hint.contains(home.to_str().expect("utf-8 home")), "{hint}");
    }
}

#[test]
fn the_re_read_command_quotes_a_path_only_when_it_must() {
    assert_eq!(
        super::shell_quoted("/Users/me/.local/state/lean-ctx/tee/proxy_ab12.log"),
        "/Users/me/.local/state/lean-ctx/tee/proxy_ab12.log"
    );
    assert_eq!(
        super::shell_quoted("/Users/me/My State/tee/proxy_ab12.log"),
        "'/Users/me/My State/tee/proxy_ab12.log'"
    );
    assert_eq!(super::shell_quoted("/a/it's/x.log"), r"'/a/it'\''s/x.log'");
}

#[test]
fn filtering_commands_are_recognised_as_leading_command_or_last_stage() {
    for filtering in [
        "grep -rn handler src | head -60",
        "grep -A45 'fn main' src/main.rs | cut -c1-200",
        "sed -n '100,126p' src/lib.rs | cut -c1-120; ls",
        "sed -n '6,30p' notes.md | cut -c1-80",
        "head -40 x.md | cut -c1-100; ls",
        "rg -n TODO",
        "FOO=1 /usr/bin/egrep -n x f",
        "cat Cargo.lock | sort | uniq -c",
        "git log --oneline | wc -l",
        "curl -s localhost/api | jq .items",
        "cd rust && grep -c x src/a.rs",
    ] {
        assert!(super::is_filtering_command(filtering), "{filtering}");
    }
    for unfiltered in [
        "git show origin/main:package.json",
        "cargo test --lib",
        "tail -f server.log",
        "sed -i 's/a/b/' f",
        "npm run build",
        "ls -la",
    ] {
        assert!(!super::is_filtering_command(unfiltered), "{unfiltered}");
    }
}

/// The exemption is Bash-specific: it needs the `command` of the matching
/// `tool_use`. With a filtering command the result stays whole up to the
/// engine's 16 KiB verbatim budget; without one, or with a command that
/// filters nothing, the same text is compressed (with its hint).
#[test]
fn a_filtered_shell_result_is_not_pruned_again() {
    let _data = lean_ctx::core::data_dir::isolated_data_dir();
    let _community = lean_ctx::core::policy::runtime::TestPolicyOverride::set(None);
    let log = big_compressible_log();
    assert!(log.len() <= 16 * 1024 && !super::fits_verbatim(&log));

    let filtered = super::with_tool_command(Some("grep -n processed app.log | head -400"), || {
        compress_tool_result(&log, Some("Bash"))
    });
    assert_eq!(filtered, log);
    assert_eq!(
        shape_command_output("grep -n processed app.log", &log, Some(0)),
        log
    );

    let unfiltered = super::with_tool_command(Some("cat app.log"), || {
        compress_tool_result(&log, Some("Bash"))
    });
    // `cat` is a verbatim command of the engine's classifier as well.
    assert_eq!(unfiltered, log);
    let built = super::with_tool_command(Some("./run-job.sh"), || {
        compress_tool_result(&log, Some("Bash"))
    });
    assert!(built.len() < log.len() && built.contains("full original at"));
    assert!(
        compress_tool_result(&log, Some("Bash")).len() < log.len(),
        "scope restored"
    );

    // Past the budget a filtered result is compressed, never without a hint.
    let huge = (1..=1500)
        .map(|i| format!("[info] processed item {i:05} ok"))
        .collect::<Vec<_>>()
        .join("\n");
    let out = super::with_tool_command(Some("grep processed app.log"), || {
        compress_tool_result(&huge, Some("Bash"))
    });
    assert!(
        out.len() < huge.len() && out.contains("full original at") && out.contains("`cat` it"),
        "{out}"
    );
}

/// `lines` lines of `a` joined by `\n`, exactly `bytes` long.
fn sized(bytes: usize, lines: usize) -> String {
    let content = bytes - (lines - 1);
    let (each, extra) = (content / lines, content % lines);
    let out = (0..lines)
        .map(|i| "a".repeat(each + usize::from(i < extra)))
        .collect::<Vec<_>>()
        .join("\n");
    assert_eq!((out.len(), out.lines().count()), (bytes, lines));
    out
}

/// Each threshold is inclusive: the byte rule at 4096, the line rule at 60
/// lines up to 16384 bytes (the 4–16 KiB branch), and the filtered-output
/// budget at 16384 bytes.
#[test]
fn verbatim_thresholds_are_inclusive_at_each_boundary() {
    use super::{
        FILTERED_VERBATIM_MAX_BYTES, VERBATIM_LINES_MAX_BYTES, VERBATIM_MAX_BYTES,
        VERBATIM_MAX_LINES, fits_verbatim, keeps_filtered_output,
    };
    assert_eq!(
        (
            VERBATIM_MAX_BYTES,
            VERBATIM_MAX_LINES,
            VERBATIM_LINES_MAX_BYTES
        ),
        (4096, 60, 16384)
    );
    // Byte rule, with too many lines for the line rule.
    assert!(fits_verbatim(&sized(4096, 100)));
    assert!(!fits_verbatim(&sized(4097, 100)));
    // Line rule between 4 and 16 KiB.
    assert!(fits_verbatim(&sized(8000, 60)));
    assert!(!fits_verbatim(&sized(8000, 61)));
    assert!(fits_verbatim(&sized(16384, 60)));
    assert!(!fits_verbatim(&sized(16385, 60)));

    assert_eq!(FILTERED_VERBATIM_MAX_BYTES, 16384);
    let at = sized(16384, 400);
    let past = sized(16385, 400);
    assert!(!fits_verbatim(&at));
    assert!(keeps_filtered_output("grep -n a f", &at));
    assert!(!keeps_filtered_output("grep -n a f", &past));
    assert!(!keeps_filtered_output("make build", &at));
}

/// The 4–16 KiB branch of the line rule end to end: a 60-line result of
/// ~8 KiB reaches the model whole through both funnels and the shaper.
#[test]
fn a_bounded_view_between_4_and_16_kib_stays_whole() {
    let _data = lean_ctx::core::data_dir::isolated_data_dir();
    let _community = lean_ctx::core::policy::runtime::TestPolicyOverride::set(None);
    let view = listing(60, 130);
    assert!(view.len() > super::VERBATIM_MAX_BYTES && view.len() <= 16 * 1024);
    for tool in [Some("Bash"), Some("Read"), None] {
        assert_eq!(compress_tool_result(&view, tool), view, "{tool:?}");
        assert_eq!(compress_tool_result_gateway(&view, tool), view, "{tool:?}");
    }
    assert_eq!(shape_command_output("ls -la", &view, Some(0)), view);
}

/// Regression (bd review of !646): a result within the verbatim budget left
/// with its raw `<lc_safe>` markers, because the size rule returned before
/// the marker handling. It is handled as before: the protected span passes
/// verbatim, the markers are stripped, and the small unprotected rest stays
/// whole — on every route, with a filtering command as well.
#[test]
fn a_small_result_with_lc_safe_markers_leaves_without_them() {
    let _data = lean_ctx::core::data_dir::isolated_data_dir();
    let _community = lean_ctx::core::policy::runtime::TestPolicyOverride::set(None);
    let before = listing(20, 40);
    let kept = listing(8, 30);
    let after = listing(12, 40);
    let marked = format!("{before}\n<lc_safe>{kept}</lc_safe>\n{after}");
    let stripped = format!("{before}\n{kept}\n{after}");
    assert!(marked.len() > 1024 && marked.len() <= super::VERBATIM_MAX_BYTES);
    assert!(
        super::fits_verbatim(&marked),
        "the case the early return took"
    );

    for tool in [Some("Bash"), Some("ArtifactData"), None] {
        assert_eq!(compress_tool_result(&marked, tool), stripped, "{tool:?}");
        assert_eq!(
            compress_tool_result_gateway(&marked, tool),
            stripped,
            "{tool:?}"
        );
        let filtered = super::with_tool_command(Some("grep -n entry f"), || {
            compress_tool_result(&marked, tool)
        });
        assert_eq!(filtered, stripped, "{tool:?}");
    }
    assert_eq!(shape_command_output("ls -la", &marked, Some(0)), stripped);
    assert_eq!(
        shape_command_output("grep -n entry f", &marked, Some(0)),
        stripped
    );
}

/// Regression (bd N1 on !646): a marked result past the verbatim budget
/// whose unprotected part shrank for real, but by less than a tee floor,
/// left `attach_ccr` as the original — with its raw `<lc_safe>` markers.
/// Every path that keeps the original keeps it without them.
#[test]
fn a_marked_result_with_a_small_lossy_shrink_leaves_whole_without_markers() {
    let _data = lean_ctx::core::data_dir::isolated_data_dir();
    let _community = lean_ctx::core::policy::runtime::TestPolicyOverride::set(None);
    let before = listing(40, 60);
    let kept = listing(10, 40);
    let after = listing(40, 60);
    let marked = format!("{before}\n<lc_safe>{kept}</lc_safe>\n{after}");
    let stripped = format!("{before}\n{kept}\n{after}");
    assert!(
        (5 * 1024..=6 * 1024).contains(&marked.len()),
        "{}",
        marked.len()
    );
    let lossy = format!(
        "{before}\n{kept}\n{}\n[5 lines omitted]\n{}",
        after.lines().take(5).collect::<Vec<_>>().join("\n"),
        after.lines().skip(10).collect::<Vec<_>>().join("\n")
    );
    let shrink = marked.len().saturating_sub(lossy.len());
    assert!(
        lossy.len() < stripped.len() && shrink > 0 && shrink < ccr::MIN_TEE_BYTES,
        "a real but small lossy shrink: {shrink}"
    );
    for audience in [super::CcrAudience::Local, super::CcrAudience::Gateway] {
        let out = super::attach_ccr(&marked, lossy.clone(), audience);
        assert_eq!(out, stripped, "whole, protected span intact");
        assert!(!out.contains("lc_safe"), "{out}");
    }
    for tool in [Some("Bash"), Some("ArtifactData"), None] {
        for out in [
            compress_tool_result(&marked, tool),
            compress_tool_result_gateway(&marked, tool),
        ] {
            assert!(!out.contains("lc_safe"), "{tool:?}: {out}");
            assert!(
                out.contains(&kept),
                "{tool:?}: the protected span is verbatim"
            );
        }
    }
}

/// bd N3 on !646: below the 200-byte floor a marked result leaves without
/// its markers as well, through both funnels and the shaper.
#[test]
fn a_tiny_result_with_lc_safe_markers_leaves_without_them() {
    let _data = lean_ctx::core::data_dir::isolated_data_dir();
    let _community = lean_ctx::core::policy::runtime::TestPolicyOverride::set(None);
    let marked = "ok\n<lc_safe>let key = 1;</lc_safe>\ndone";
    let stripped = "ok\nlet key = 1;\ndone";
    assert!(marked.len() < 200);
    for tool in [Some("Bash"), None] {
        assert_eq!(compress_tool_result(marked, tool), stripped, "{tool:?}");
        assert_eq!(
            compress_tool_result_gateway(marked, tool),
            stripped,
            "{tool:?}"
        );
    }
    assert_eq!(shape_command_output("ls", marked, Some(0)), stripped);
    assert_eq!(compress_tool_result("ok", None), "ok");
}

#[cfg(feature = "http-server")]
/// [real register] bd N2 on !646: under protection the unmarked form of a
/// result kept whole is a derived text. It leaves only on record under the
/// lineage of its original, so a later turn finds it as known as the
/// original was; a marked result of unknown lineage leaves unmarked and
/// records nothing; and where the record is refused (the source is gone)
/// the original leaves with its markers rather than with unknown lineage.
#[test]
fn under_protection_an_unmarked_whole_result_keeps_the_lineage_of_its_original() {
    let _data = lean_ctx::core::data_dir::isolated_data_dir();
    let _community = lean_ctx::core::policy::runtime::TestPolicyOverride::set(None);
    let directory = tempfile::tempdir().expect("project");
    let root = directory.path().canonicalize().expect("canonical root");
    governed_project(&root);
    assert!(super::protected());
    let source = root.join("notes.txt");
    std::fs::write(
        &source,
        format!(
            "{}\n<lc_safe>{}</lc_safe>\n{}\n",
            listing(10, 40),
            listing(4, 30),
            listing(10, 40)
        ),
    )
    .expect("source");
    let original = engine_delivers_file(&root, &source);
    assert!(lean_ctx::core::protect::has_markers(&original));
    let unmarked = lean_ctx::core::protect::compress_preserving(&original, str::to_owned);
    let known = |text: &str| {
        super::super::request_project::in_project(&root, || {
            delivered::lineage_of(&delivered::digest(text)).is_some()
        })
    };
    assert!(!known(&unmarked), "nothing on record before");

    let sent = compress_tool_result(&original, Some("Read"));
    assert_eq!(sent, unmarked, "whole, without markers");
    assert!(known(&sent), "on record under the original's lineage");

    let foreign = "foreign\n<lc_safe>never delivered by the Engine</lc_safe>\nend\n".repeat(6);
    let foreign_unmarked = lean_ctx::core::protect::compress_preserving(&foreign, str::to_owned);
    assert_eq!(
        compress_tool_result(&foreign, Some("Read")),
        foreign_unmarked
    );
    assert!(!known(&foreign_unmarked), "unknown stays unknown");

    std::fs::remove_file(&source).expect("delete the source");
    assert_eq!(
        compress_tool_result(&original, Some("Read")),
        original,
        "a refused record keeps the original, markers and all"
    );
}

/// The three policies under which the policy wrappers compress nothing of
/// a marked code result (bd M1 on !646).
fn keeping_policies() -> [(
    &'static str,
    lean_ctx::core::adaptive_policy::CompressionPolicy,
); 3] {
    use lean_ctx::core::adaptive_policy::CompressionPolicy;
    let base = CompressionPolicy::default();
    [
        (
            "compress_tool_output off",
            CompressionPolicy {
                compress_tool_output: false,
                code_preserve: false,
                ..base
            },
        ),
        (
            "log_compression_level 0",
            CompressionPolicy {
                log_compression_level: 0,
                code_preserve: false,
                ..base
            },
        ),
        (
            "code_preserve on code",
            CompressionPolicy {
                code_preserve: true,
                ..base
            },
        ),
    ]
}

/// A marked code result of `n` body lines and its form without markers.
fn marked_code(n: usize) -> (String, String) {
    let body = (0..n)
        .map(|i| format!("    let value_{i} = compute({i}); // step {i} of the body"))
        .collect::<Vec<_>>()
        .join("\n");
    let span = "    let key = derive(secret_ref); // kept verbatim";
    (
        format!("fn main() {{\n{body}\n<lc_safe>{span}</lc_safe>\n}}\n"),
        format!("fn main() {{\n{body}\n{span}\n}}\n"),
    )
}

/// Regression (bd M1 on !646): the policy wrappers returned a result they
/// leave uncompressed as it came — with its raw `<lc_safe>` markers — when
/// `compress_tool_output` is off, when `log_compression_level` is 0, and
/// for code under `code_preserve`. Each path now keeps it whole without
/// them, the protected span verbatim, through both wrappers. The bytes a
/// funnel passes through untouched (an Engine tool's result, a result that
/// carries its way back) stay untouched there as well.
#[test]
fn the_policy_wrappers_keep_a_marked_result_whole_without_markers() {
    let _data = lean_ctx::core::data_dir::isolated_data_dir();
    let _community = lean_ctx::core::policy::runtime::TestPolicyOverride::set(None);
    let (marked, stripped) = marked_code(40);
    assert!(super::looks_like_code(&marked));
    let stub =
        format!("{marked}[lean-ctx: full original at /tmp/x.log — `cat` it to re-read it exactly]");
    assert!(carries_recovery_stub(&stub));
    for (path, policy) in keeping_policies() {
        assert!(super::policy_keeps_whole(&marked, policy), "{path}");
        for tool in [Some("Bash"), Some("Read"), None] {
            for out in [
                super::compress_tool_result_with_policy(&marked, tool, policy),
                super::compress_tool_result_gateway_with_policy(
                    &marked,
                    tool,
                    lean_ctx::core::tokens::COUNTING_FAMILY,
                    policy,
                ),
            ] {
                assert!(!out.contains("lc_safe"), "{path} {tool:?}: {out}");
                assert_eq!(out, stripped, "{path} {tool:?}: whole, span intact");
            }
        }
        assert_eq!(
            super::compress_tool_result_with_policy(&marked, Some("ctx_read"), policy),
            marked,
            "{path}: an Engine tool's result passes through"
        );
        assert_eq!(
            super::compress_tool_result_with_policy(&stub, Some("Bash"), policy),
            stub,
            "{path}: a result with its way back passes through"
        );
    }
}

#[cfg(feature = "http-server")]
/// [real register] bd M1 on !646, under protection: on each of the three
/// policy paths, through both wrappers, the unmarked whole result is on
/// record under the lineage of its original, as on every funnel path
/// (`under_protection_an_unmarked_whole_result_keeps_the_lineage_of_its_original`).
#[test]
fn under_protection_the_policy_wrappers_record_the_unmarked_result() {
    let _data = lean_ctx::core::data_dir::isolated_data_dir();
    let _community = lean_ctx::core::policy::runtime::TestPolicyOverride::set(None);
    let directory = tempfile::tempdir().expect("project");
    let root = directory.path().canonicalize().expect("canonical root");
    governed_project(&root);
    assert!(super::protected());
    let known = |text: &str| {
        super::super::request_project::in_project(&root, || {
            delivered::lineage_of(&delivered::digest(text)).is_some()
        })
    };
    let mut case = 0;
    for (path, policy) in keeping_policies() {
        for gateway in [false, true] {
            case += 1;
            let (marked, _) = marked_code(20 + case);
            let source = root.join(format!("case_{case}.rs"));
            std::fs::write(&source, &marked).expect("source");
            let original = engine_delivers_file(&root, &source);
            assert!(lean_ctx::core::protect::has_markers(&original), "{path}");
            assert!(super::policy_keeps_whole(&original, policy), "{path}");
            let unmarked = lean_ctx::core::protect::compress_preserving(&original, str::to_owned);
            assert!(!known(&unmarked), "{path}: nothing on record before");
            let out = if gateway {
                super::compress_tool_result_gateway_with_policy(
                    &original,
                    Some("Read"),
                    lean_ctx::core::tokens::COUNTING_FAMILY,
                    policy,
                )
            } else {
                super::compress_tool_result_with_policy(&original, Some("Read"), policy)
            };
            assert_eq!(out, unmarked, "{path} gateway={gateway}: whole, no markers");
            assert!(
                out.contains("let key = derive(secret_ref);"),
                "{path}: span intact"
            );
            assert!(
                known(&out),
                "{path} gateway={gateway}: on record under the original's lineage"
            );
        }
    }
}

#[cfg(feature = "http-server")]
/// [real register] bd S2 on !646: without a project of the proxy nothing
/// is looked up or recorded outside a project scope, as `bound_copy`
/// refuses a copy then (`NO_PROJECT`). The case is that of no lineage: the
/// result leaves whole without its markers, unrecorded and not noted.
/// Before, the lookup outside any scope found the text's only lineage, the
/// record outside a request project was refused, and the original left
/// with its markers, noted `recovery.derived_not_recorded`.
#[test]
fn under_protection_without_a_project_the_unmarked_result_is_not_recorded() {
    let _data = lean_ctx::core::data_dir::isolated_data_dir();
    let _community = lean_ctx::core::policy::runtime::TestPolicyOverride::set(None);
    let directory = tempfile::tempdir().expect("project");
    let root = directory.path().canonicalize().expect("canonical root");
    governed_project(&root);
    let source = root.join("notes.txt");
    std::fs::write(
        &source,
        format!(
            "{}\n<lc_safe>{}</lc_safe>\n{}\n",
            listing(10, 40),
            listing(4, 30),
            listing(10, 40)
        ),
    )
    .expect("source");
    let original = engine_delivers_file(&root, &source);
    let unmarked = lean_ctx::core::protect::compress_preserving(&original, str::to_owned);
    assert!(
        delivered::lineage_of(&delivered::digest(&original)).is_some(),
        "outside any scope the lookup finds the only lineage"
    );

    let config = lean_ctx::core::paths::config_dir_read_only().expect("config directory");
    std::fs::write(
        config.join("config.toml"),
        "[context_gateway]\nmode = \"governed\"\n",
    )
    .expect("gateway config without a project");
    assert!(super::protected());
    assert!(super::super::request_project::current_source_project().is_none());
    assert_eq!(bound_copy(&original).err(), Some(NO_PROJECT));

    let (sent, kept) = observing(Reader::Model, || {
        compress_tool_result(&original, Some("Read"))
    });
    assert_eq!(sent, unmarked, "whole, without markers");
    assert!(kept.is_empty(), "nothing noted: {}", kept.summary());
    let known = super::super::request_project::in_project(&root, || {
        delivered::lineage_of(&delivered::digest(&unmarked)).is_some()
    });
    assert!(!known, "nothing recorded");
}

#[cfg(feature = "http-server")]
/// [real register] bd S3 on !646: where the record of a shortened form is
/// refused, the block is noted `recovery.derived_not_recorded` exactly once
/// — also where the record of its unmarked form is refused as well. Before,
/// `whole()` noted the same code a second time.
#[test]
fn a_refused_record_of_the_shortened_form_is_noted_once() {
    let _data = lean_ctx::core::data_dir::isolated_data_dir();
    let _community = lean_ctx::core::policy::runtime::TestPolicyOverride::set(None);
    let directory = tempfile::tempdir().expect("project");
    let root = directory.path().canonicalize().expect("canonical root");
    governed_project(&root);
    let source = root.join("notes.txt");
    std::fs::write(
        &source,
        format!(
            "{}\n<lc_safe>{}</lc_safe>\n{}\n",
            listing(10, 40),
            listing(4, 30),
            listing(10, 40)
        ),
    )
    .expect("source");
    let original = engine_delivers_file(&root, &source);
    let unmarked = lean_ctx::core::protect::compress_preserving(&original, str::to_owned);
    let kept_once = |expected: &str, case: &str| {
        let ((out, notes), kept) = observing(Reader::Model, || {
            let before = super::note_calls();
            let out = super::kept_after_refused_record(&original, Some(&unmarked));
            (out, super::note_calls().saturating_sub(before))
        });
        assert_eq!(out, expected, "{case}");
        assert_eq!(notes, 1, "{case}: one note");
        assert_eq!(
            kept.counts(),
            BTreeMap::from([(super::DERIVED_NOT_RECORDED, 1)]),
            "{case}"
        );
    };
    kept_once(&unmarked, "the unmarked form gets on record");
    std::fs::remove_file(&source).expect("delete the source");
    kept_once(&original, "its record is refused as well");
}

/// The choice documented on `is_filtering_command`: one filtering segment
/// exempts the command line, also behind a command that filters nothing —
/// bounded by the 16 KiB budget, and lossless on either side.
#[test]
fn one_filtering_segment_exempts_a_compound_command() {
    for compound in [
        "cd rust && rg -n TODO src",
        "make build | grep -n error",
        "cargo build; wc -l f",
        "sed -n '1,40p' a.rs | cut -c1-80; ls",
    ] {
        assert!(super::is_filtering_command(compound), "{compound}");
    }
    for compound in ["cd rust && cargo build", "make build | tee out.log; ls"] {
        assert!(!super::is_filtering_command(compound), "{compound}");
    }
}
