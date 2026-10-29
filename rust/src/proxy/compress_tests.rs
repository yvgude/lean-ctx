// SPDX-License-Identifier: Apache-2.0
use super::ccr;
use super::{
    BTreeMap, NO_PROJECT, RESEARCH_PROSE_CAP, RESEARCH_PROSE_CAP_ENV, Reader, RecoveryCopy,
    RecoveryId, bound_copy, carries_recovery_stub, compress_tool_result,
    compress_tool_result_gateway, delivered, extract_command_hint, infer_command, looks_like_prose,
    observing, output_looks_like_build_failure, output_looks_like_test_run, recovery_copy,
    recovery_stub, reference_stub, research_prose_cap, resolve_recovery, shape_command_output,
    shape_tool_output,
};
use serial_test::serial;

/// #980/#985: search tool results must compress without corrupting source.
#[test]
fn search_tool_result_is_compressed_without_corrupting_source() {
    let _data = crate::core::data_dir::isolated_data_dir();
    let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
    let raw = (0..60)
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
    let _data = crate::core::data_dir::isolated_data_dir();
    let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
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
    let _data = crate::core::data_dir::isolated_data_dir();
    let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
    let para = "Rust is a multi-paradigm systems programming language that \
                emphasizes performance, type safety, and fearless concurrency, \
                achieving memory safety without a garbage collector at runtime.";
    // Repeated paragraph (well over the 600-char prose floor) → dedup keeps one.
    let input = format!("{}\n", [para; 8].join("\n\n"));
    assert!(input.len() > 600);
    let out = compress_tool_result(&input, Some("web_fetch"));
    assert_eq!(out.matches("fearless concurrency").count(), 1);
    assert!(out.contains("performance, type safety"));
}

#[test]
#[serial]
fn research_prose_cap_env_overrides_default() {
    let _lock = crate::core::data_dir::test_env_lock();
    crate::test_env::set_var(RESEARCH_PROSE_CAP_ENV, "1234");
    assert_eq!(research_prose_cap(), 1234);
    crate::test_env::remove_var(RESEARCH_PROSE_CAP_ENV);
}

#[test]
#[serial]
fn research_prose_cap_env_invalid_falls_back() {
    let _lock = crate::core::data_dir::test_env_lock();
    for value in ["", "not_a_number", "0"] {
        crate::test_env::set_var(RESEARCH_PROSE_CAP_ENV, value);
        assert_eq!(research_prose_cap(), RESEARCH_PROSE_CAP);
    }
    crate::test_env::remove_var(RESEARCH_PROSE_CAP_ENV);
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
    let _lock = crate::core::data_dir::test_env_lock();
    let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
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
    assert!(out.contains(&handle), "CCR handle must be embedded: {out}");
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
    let _data = crate::core::data_dir::isolated_data_dir();
    let _policy = crate::core::policy::runtime::TestPolicyOverride::set(Some(
        crate::core::policy::load("name='no-tee'\nversion='1.0.0'\ndescription='test'\n").unwrap(),
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
    let _data = crate::core::data_dir::isolated_data_dir();
    let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
    let config = crate::core::paths::config_dir_read_only().expect("config directory");
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

/// Governed mode with `root` as the proxy's configured project root.
fn governed_project(root: &std::path::Path) {
    let config = crate::core::paths::config_dir_read_only().expect("config directory");
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

/// The test plays the Engine for the project `root`: an admitted read of
/// `source`, recorded as delivered with its file lineage. Returns the
/// delivered text.
fn engine_delivers_file(root: &std::path::Path, source: &std::path::Path) -> String {
    super::super::request_project::in_project(root, || {
        let path = source.to_str().expect("utf-8 path");
        let admitted = crate::tools::ctx_read::read_file_for_tool_rooted(
            path,
            root.to_str().expect("utf-8 root"),
            "ctx_read",
        )
        .expect("admitted read");
        let authority = crate::core::archive::authority::ArchiveAuthority::file(
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

/// [real register and store] Under protection a block is compressed only
/// with a recovery copy bound to its lineage. A block the Engine
/// delivered from an admitted project file is compressed; its stub
/// resolves to the exact original bytes; the block that is sent is on
/// record under the same lineage. After the source is deleted the id
/// resolves to a typed refusal and nothing else, and the same block is
/// no longer compressed. A block without a lineage is never compressed.
#[test]
fn file_lineage_block_is_compressed_with_a_resolvable_copy_until_its_source_is_gone() {
    let _data = crate::core::data_dir::isolated_data_dir();
    let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
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

/// [real register and store] A source the project policy restricts is in
/// no lineage: the Engine records nothing for it, so its text is not
/// compressed and nothing is stored. And without a project of the proxy
/// nothing is looked up at all.
#[test]
fn restricted_source_and_missing_project_leave_the_block_whole() {
    let _data = crate::core::data_dir::isolated_data_dir();
    let _policy = crate::core::policy::runtime::TestPolicyOverride::set(Some(
        crate::core::policy::load(
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
        crate::core::archive::bound_recovery_id(&restricted).is_none(),
        "nothing was stored"
    );

    let source = root.join("build.log");
    std::fs::write(&source, big_compressible_log()).expect("source");
    let original = engine_delivers_file(&root, &source);
    let config = crate::core::paths::config_dir_read_only().expect("config directory");
    std::fs::write(
        config.join("config.toml"),
        "[context_gateway]\nmode = \"governed\"\n",
    )
    .expect("gateway config");
    assert_eq!(bound_copy(&original).err(), Some(NO_PROJECT));
    assert_eq!(compress_tool_result(&original, Some("bash")), original);
    assert_eq!(resolve_recovery(&"0".repeat(64)), Some(Err(NO_PROJECT)));
}

/// [real registers and store] Prepared context from a caller-supplied
/// source that is presented and admitted now: compressed, and its stub
/// resolves to the exact original. (That a presentation older than its
/// validity, or one under another policy, refuses is the Engine's rule;
/// it needs a clock and is not exercised from here.)
#[test]
fn presented_lineage_block_is_compressed_and_resolvable_while_presented() {
    let _data = crate::core::data_dir::isolated_data_dir();
    let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
    let directory = tempfile::tempdir().expect("project");
    let root = directory.path().canonicalize().expect("canonical root");
    governed_project(&root);
    let original = big_compressible_log();
    super::super::request_project::in_project(&root, || {
        let binding = crate::core::archive::presented::PresentedBinding::admitted_now(
            &root,
            "object://caller/build-log",
            "source-one",
            "sha256:7d1a54127b222502f5b79b5fb0803061152a44f92b37e23c6527baf665d4da9a",
        )
        .expect("the policy identity of the project resolves");
        assert!(crate::core::archive::presented::record_presentation(
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
    let _lock = crate::core::data_dir::test_env_lock();
    let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
    let log = big_compressible_log();
    let a = compress_tool_result(&log, Some("bash"));
    let b = compress_tool_result(&log, Some("bash"));
    assert_eq!(
        a, b,
        "the CCR handle is content-addressed, so the rewritten result must be \
         byte-identical across turns (provider cache prefix stays valid, #448)"
    );
}

/// Contract test for #702, pinned to LiteLLM's marker regex: the headroom
/// guardrail scans compressed text with `hash=([a-f0-9]{24})`
/// (BerriAI/litellm#31681). If our gateway stub ever drifts from that
/// shape, the CCR agentic loop silently stops firing — this test fails
/// first.
#[test]
fn gateway_stub_matches_litellm_marker_regex_and_retrieves() {
    let _lock = crate::core::data_dir::test_env_lock();
    let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
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
        crate::core::protocol::strip_trailing_savings_footer(&out),
        out
    );
}

#[test]
fn gateway_stub_absent_for_passthrough_and_ctx_output() {
    let _lock = crate::core::data_dir::test_env_lock();
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
    let _lock = crate::core::data_dir::test_env_lock();
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
            crate::core::protocol::strip_trailing_savings_footer(&block),
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
    let _data = crate::core::data_dir::isolated_data_dir();
    let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
    let id = RecoveryId::parse(&"a".repeat(64)).expect("canonical id");
    let tee = "[lean-ctx: full original at /state/tee/proxy_0123456789abcdef.log — read it \
               directly (no MCP), or ctx_expand(id=\"proxy_0123456789abcdef.log\") for a slice]";
    for stub in [recovery_stub(&id), reference_stub(&id), tee.to_owned()] {
        let block = format!("{}\n{stub}", big_compressible_log());
        assert_eq!(compress_tool_result(&block, Some("bash")), block);
        assert_eq!(compress_tool_result_gateway(&block, None), block);
    }
}

/// Under protection every block that stays whole for want of a recovery
/// copy is counted once, with its content-free code, for the route that
/// observes the pass — however often a pass tries it. Developer mode
/// notes nothing.
#[test]
fn blocks_kept_whole_are_counted_once_with_their_code() {
    let _data = crate::core::data_dir::isolated_data_dir();
    let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
    let config = crate::core::paths::config_dir_read_only().expect("config directory");
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
    let _data = crate::core::data_dir::isolated_data_dir();
    let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
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

/// `shape_command_output` under protection: the route knows no tool list of
/// the host that reads the result, so no stub may be issued and no recovery
/// copy is asked for — the output arrives whole, the reason is counted, and
/// no tee file is written. Developer mode shapes it and names its tee handle.
#[test]
fn shaping_under_protection_without_a_recovery_copy_keeps_the_output_whole() {
    let _data = crate::core::data_dir::isolated_data_dir();
    let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
    let config = crate::core::paths::config_dir_read_only().expect("config directory");
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
    let tee = crate::core::paths::state_dir_read_only()
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

/// [real registers and store] `shape_command_output` in governed mode WITH
/// a project and a known, admitted lineage of the output — the case in
/// which a copy could be bound. The route holds no tool list of the host,
/// so the name of the expand tool is not known to be followable: no stub
/// is issued under the bare name, the output stays whole with
/// `recovery.handle_not_followable`, and no copy is stored for it.
#[test]
fn shaping_under_protection_issues_no_stub_without_a_known_host_tool() {
    let _data = crate::core::data_dir::isolated_data_dir();
    let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
    let directory = tempfile::tempdir().expect("project");
    let root = directory.path().canonicalize().expect("canonical root");
    governed_project(&root);
    let noisy = noisy_build_output();
    super::super::request_project::in_project(&root, || {
        let binding = crate::core::archive::presented::PresentedBinding::admitted_now(
            &root,
            "object://caller/build-output",
            "source-one",
            "sha256:7d1a54127b222502f5b79b5fb0803061152a44f92b37e23c6527baf665d4da9a",
        )
        .expect("the policy identity of the project resolves");
        assert!(crate::core::archive::presented::record_presentation(
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
        crate::core::archive::bound_recovery_id(&noisy).is_none(),
        "nothing was stored for a stub that was never issued"
    );
    assert!(
        bound_copy(&noisy).is_ok(),
        "the lineage is known: a copy could have been bound"
    );
}

/// [real registers and store] `shape_tool_output` — the entry point of
/// `ctx_shape` for a host tool without a known command line — in governed
/// mode WITH a project and a known, admitted lineage. The same text handed
/// to the compressor directly, outside any pass, leaves with a stub under
/// the bare name; through this route no host tool list is known, so no stub
/// is issued: the text stays whole with `recovery.handle_not_followable`.
#[test]
fn shaping_a_tool_output_under_protection_issues_no_stub_without_a_known_host_tool() {
    let _data = crate::core::data_dir::isolated_data_dir();
    let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
    let directory = tempfile::tempdir().expect("project");
    let root = directory.path().canonicalize().expect("canonical root");
    governed_project(&root);
    let original = big_compressible_log();
    super::super::request_project::in_project(&root, || {
        let binding = crate::core::archive::presented::PresentedBinding::admitted_now(
            &root,
            "object://caller/build-log",
            "source-one",
            "sha256:7d1a54127b222502f5b79b5fb0803061152a44f92b37e23c6527baf665d4da9a",
        )
        .expect("the policy identity of the project resolves");
        assert!(crate::core::archive::presented::record_presentation(
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
