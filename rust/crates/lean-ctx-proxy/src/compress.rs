// SPDX-License-Identifier: Apache-2.0
use std::cell::RefCell;
use std::collections::BTreeMap;

use super::ccr;
use super::prose_compress::compress_prose;
use lean_ctx::core::archive::{RecoveryId, RecoveryRefusal, delivered};
use lean_ctx::core::tokens::{COUNTING_FAMILY, TokenizerFamily, count_tokens_for};
use lean_ctx::core::web::distill;

#[path = "compress_gate.rs"]
pub(crate) mod gate;

/// Byte-ish budget for the research-prose squeeze (~5k tokens on English prose).
/// Only oversized prose is truncated; the squeeze's main job is dedup + blank-collapse,
/// not cutting.
const RESEARCH_PROSE_CAP: usize = 20_000;
const RESEARCH_PROSE_CAP_ENV: &str = "LEAN_CTX_RESEARCH_PROSE_CAP";

fn research_prose_cap() -> usize {
    std::env::var(RESEARCH_PROSE_CAP_ENV)
        .ok()
        .and_then(|v| v.trim().parse::<usize>().ok())
        .filter(|cap| *cap > 0)
        .unwrap_or(RESEARCH_PROSE_CAP)
}

/// A tool result up to this size reaches the model whole, from every tool.
///
/// Derived from the budgets of the compressors behind this funnel: the
/// `log_dedup` pattern cuts any block above 30 unique lines to its first 5
/// and last 10, and the shell engine shows at most 40 lines (`SHOWN`) of an
/// output it cannot classify — so without a floor every result above ~35
/// lines left as a fragment, whatever tool produced it. The fragment of a
/// result the model asked for is followed by a re-read that carries the
/// whole original again plus a turn of its own; below ~1k tokens (4 KiB,
/// eight tee floors of [`ccr::MIN_TEE_BYTES`]) the pruning cannot save what
/// that re-read costs.
pub(crate) const VERBATIM_MAX_BYTES: usize = 4 * 1024;
/// A result of at most this many lines also stays whole: one bounded view
/// (`head -60`, `sed -n 'A,Bp'`, a page of a listing) is what the caller
/// sized it to be, and twice log_dedup's 30-line block is room for it.
pub(crate) const VERBATIM_MAX_LINES: usize = 60;
/// Few long lines (minified JSON, a bundle) are no bounded view: past this
/// size the line rule no longer holds and the result is compressed.
const VERBATIM_LINES_MAX_BYTES: usize = 16 * 1024;
/// The output of a filtering command stays whole up to this size: the
/// engine's own verbatim budget for verbatim commands (`MAX_VERBATIM_TOKENS`
/// = 4000 tokens, ~16 KiB). A larger filtered output is compressed and
/// carries its re-read path like any other.
const FILTERED_VERBATIM_MAX_BYTES: usize = 16 * 1024;

fn fits_verbatim(content: &str) -> bool {
    content.len() <= VERBATIM_MAX_BYTES
        || (content.len() <= VERBATIM_LINES_MAX_BYTES
            && content.lines().count() <= VERBATIM_MAX_LINES)
}

thread_local! {
    /// The command line of the shell tool call whose result is compressed
    /// ([`with_tool_command`]).
    static TOOL_COMMAND: RefCell<Option<String>> = const { RefCell::new(None) };
}

/// Run `f` — a compression of one tool result — knowing the command line of
/// the tool call that produced it: the `command` input of the matching
/// `tool_use` (Bash and other shell tools). A result of a filtering command
/// ([`is_filtering_command`]) is then not pruned again: the caller already
/// chose the lines it wants. The previous scope is restored afterwards, also
/// on unwind.
pub fn with_tool_command<R>(command: Option<&str>, f: impl FnOnce() -> R) -> R {
    struct Restore(Option<String>);
    impl Drop for Restore {
        fn drop(&mut self) {
            let previous = self.0.take();
            TOOL_COMMAND.with(|command| *command.borrow_mut() = previous);
        }
    }
    let previous = TOOL_COMMAND.with(|current| current.replace(command.map(str::to_owned)));
    let _restore = Restore(previous);
    f()
}

fn keeps_filtered_output(command: &str, output: &str) -> bool {
    output.len() <= FILTERED_VERBATIM_MAX_BYTES && is_filtering_command(command)
}

/// True when `command` already selects the lines its output shows: a
/// verbatim command of the engine's classifier
/// (`shell::compress::is_verbatim_output`: head/tail, `sed`/`awk`, `jq`,
/// `wc`, file viewers, as the whole command or its last stage), or a
/// filter — the grep family, `head`/`tail`, `sed -n`, `awk`, `cut`, `wc`,
/// `jq`/`yq`, `sort`, `uniq` — as the leading command or the last pipeline
/// stage of any segment of a compound command.
///
/// One filtering segment exempts the whole command line (`cd x && rg …`,
/// `sed -n … | cut; ls`, but also `cargo build; wc -l f`). Deliberate: the
/// exemption is bounded by [`FILTERED_VERBATIM_MAX_BYTES`], and without it
/// the result is compressed with its re-read hint, so either side is
/// lossless — a too-wide match costs tokens, never content, while a strict
/// every-segment rule would prune the bounded views the caller chained.
pub fn is_filtering_command(command: &str) -> bool {
    if lean_ctx::shell::compress::is_verbatim_output(command) {
        return true;
    }
    command
        .split(['\n', ';'])
        .flat_map(|part| part.split("&&"))
        .flat_map(|part| part.split("||"))
        .any(|segment| {
            let mut stages = segment.split('|').map(str::trim).filter(|s| !s.is_empty());
            let first = stages.next();
            let last = stages.next_back().or(first);
            first.is_some_and(is_filter_stage) || last.is_some_and(is_filter_stage)
        })
}

fn is_filter_stage(stage: &str) -> bool {
    let mut words = stage
        .split_whitespace()
        .skip_while(|word| word.contains('=') && !word.starts_with('-'))
        .skip_while(|word| matches!(*word, "sudo" | "command" | "env" | "time" | "nice"));
    let Some(program) = words.next() else {
        return false;
    };
    let program = program.rsplit('/').next().unwrap_or(program);
    let args: Vec<&str> = words.collect();
    match program {
        "grep" | "egrep" | "fgrep" | "rg" | "ag" | "ack" | "awk" | "gawk" | "cut" | "wc" | "jq"
        | "yq" | "sort" | "uniq" | "nl" | "head" => true,
        "tail" => !args
            .iter()
            .any(|arg| *arg == "-f" || *arg == "-F" || arg.starts_with("--follow")),
        "sed" => args
            .iter()
            .any(|arg| arg.starts_with("-n") || *arg == "--quiet" || *arg == "--silent"),
        _ => false,
    }
}

/// The hint a result compressed against a tee copy ends with. It names the
/// copy once: `~/…` under the home (no account name, the rules of
/// `session_elide::locator`, which hold for Bash and PowerShell alike), else
/// the shell-quoted absolute path — plus the store-relative name
/// `ctx_expand` resolves. A pure function of the handle and the home, so
/// byte-stable (#498).
pub(crate) fn tee_hint(handle: &str) -> String {
    let path = std::path::Path::new(handle);
    let location =
        super::session_elide::home_location(path).unwrap_or_else(|| shell_quoted(handle));
    let name = path.file_name().map_or_else(
        || handle.to_owned(),
        |name| name.to_string_lossy().into_owned(),
    );
    format!(
        "[lean-ctx: full original at {location} — `cat` it to re-read it exactly (no MCP), \
         or ctx_expand(id=\"{name}\", head=N|search=\"…\"|json_path=\"…\") for a slice]"
    )
}

/// `path` as one shell word: unquoted when it is plain, single-quoted
/// otherwise. Pure, so the hint that embeds it stays byte-stable (#498).
fn shell_quoted(path: &str) -> String {
    if !path.is_empty()
        && path
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"/._-+:@%".contains(&b))
    {
        path.to_owned()
    } else {
        format!("'{}'", path.replace('\'', r"'\''"))
    }
}

/// Proxy compression funnel: routes a tool result to the right compressor.
///
/// 1. Already-cited research output (from `ctx_url_read` / the web layer) is kept
///    verbatim — it is distilled and citation-stamped, so the shell pipeline must
///    not touch its footer or claim markers.
/// 2. Prose results (web fetches, doc reads, research MCP bridges) are squeezed
///    by the prose-aware research compressor instead of the log/code-tuned shell
///    engine.
/// 3. Everything else (shell/build/search output) flows through the unified
///    `compress_if_beneficial` pipeline. A `$ ...` command hint is extracted so
///    the pattern engine gets the same routing as the CLI and MCP paths.
pub fn compress_tool_result(content: &str, tool_name: Option<&str>) -> String {
    compress_tool_result_for(content, tool_name, COUNTING_FAMILY)
}

/// Compresses a tool result under a task-aware policy.
///
/// A result the policy leaves uncompressed (`compress_tool_output` off,
/// `log_compression_level` 0, code under `code_preserve`) leaves as every
/// funnel leaves a result it keeps whole ([`policy_whole`]): without its
/// `<lc_safe>` markers, under protection on record under the lineage of its
/// original (bd M1 on !646).
pub fn compress_tool_result_with_policy(
    content: &str,
    tool_name: Option<&str>,
    policy: lean_ctx::core::adaptive_policy::CompressionPolicy,
) -> String {
    if policy_keeps_whole(content, policy) {
        return policy_whole(content, tool_name);
    }
    compress_tool_result(content, tool_name)
}

/// Gateway variant of [`compress_tool_result_with_policy`].
pub fn compress_tool_result_gateway_with_policy(
    content: &str,
    tool_name: Option<&str>,
    family: TokenizerFamily,
    policy: lean_ctx::core::adaptive_policy::CompressionPolicy,
) -> String {
    if policy_keeps_whole(content, policy) {
        return policy_whole(content, tool_name);
    }
    compress_tool_result_gateway_for(content, tool_name, family)
}

/// Whether `policy` compresses nothing of `content`.
fn policy_keeps_whole(
    content: &str,
    policy: lean_ctx::core::adaptive_policy::CompressionPolicy,
) -> bool {
    !policy.compress_tool_output
        || policy.log_compression_level == 0
        || policy.code_preserve && looks_like_code(content)
}

/// `content` as a policy wrapper leaves it uncompressed: the bytes the
/// funnels pass through untouched stay untouched — a result that already
/// carries its way back ([`carries_recovery_stub`]) and the result of an
/// Engine tool ([`is_lean_ctx_tool`]); see [`compress_inner`] for why —
/// and every other result is kept whole without its `<lc_safe>` markers
/// ([`whole_without_markers`]).
fn policy_whole(content: &str, tool_name: Option<&str>) -> String {
    if carries_recovery_stub(content) || tool_name.is_some_and(is_lean_ctx_tool) {
        return content.to_owned();
    }
    whole_without_markers(content)
}

fn looks_like_code(content: &str) -> bool {
    content.lines().any(|line| {
        let line = line.trim_start();
        ["fn ", "struct ", "class ", "function ", "def ", "impl "]
            .iter()
            .any(|p| line.starts_with(p))
            || line.contains('{')
                && line.contains('}')
                && (line.contains(';') || line.contains("=>"))
    })
}

pub fn compress_tool_result_for(
    content: &str,
    tool_name: Option<&str>,
    family: TokenizerFamily,
) -> String {
    // Untouched, `<lc_safe>` markers included: see `carries_recovery_stub`.
    if carries_recovery_stub(content) {
        return content.to_owned();
    }
    let compressed = compress_inner(content, tool_name, family);
    attach_ccr(content, compressed, CcrAudience::Local)
}

/// Shape the output of a host-native shell command whose exact command line is
/// known (`ctx_shape`, fed by a host mod after the native tool ran). Unlike the
/// wire funnel, the real command reaches the engine, so its command-gated
/// policies (protected/verbatim commands, build/test guards, per-tool patterns)
/// apply exactly as for `ctx_shell`. With a known `exit_code` the outcome-aware
/// path runs (success compresses, failure keeps its diagnostics); without one
/// the engine's unknown-outcome guard applies. Lossy results carry the same
/// deterministic CCR recovery handle (#482, #498).
///
/// The same rules as the wire funnel hold: an output that already ends with
/// its way back is returned whole, and every shorter form leaves through
/// `attach_ccr` — under protection only with a recovery copy bound to the
/// output's lineage, otherwise whole and noted with its code.
///
/// This route is no request: it holds no tool list of the host that will
/// read the result. It therefore runs as a pass that knows no expand tool
/// (`sticky_tools::in_pass`) instead of outside any pass, where the bare
/// name `ctx_expand` would stand in. Under protection no stub is issued
/// then: the output stays whole with `recovery.handle_not_followable`, and
/// nothing is stored. Unprotected, the Community tee handle is unchanged.
pub fn shape_command_output(command: &str, output: &str, exit_code: Option<i32>) -> String {
    if output.trim().is_empty()
        || output.len() < 200
        || fits_verbatim(output)
        || keeps_filtered_output(command, output)
    {
        // Whole, but without its `<lc_safe>` markers, as the engine and the
        // wire funnel leave every marked output — below 200 bytes as well
        // (bd N3 on !646).
        return whole_without_markers(output);
    }
    // Untouched, `<lc_safe>` markers included: see `carries_recovery_stub`.
    if carries_recovery_stub(output) {
        return output.to_string();
    }
    let compressed = match exit_code {
        Some(code) => {
            lean_ctx::shell::compress::engine::compress_for_outcome(command, output, code)
        }
        None => lean_ctx::shell::compress::engine::compress_if_beneficial_pub(command, output),
    };
    super::sticky_tools::in_pass(|| attach_ccr(output, compressed, CcrAudience::Local))
}

/// [`compress_tool_result`] for a caller that is no request of this proxy:
/// the output of a host-native tool handed in after it ran (`ctx_shape`,
/// every tool whose command line is not known).
///
/// Such a caller holds no tool list of the host that will read the result,
/// so no expand tool is known to be followable there. The compression runs
/// as a pass that knows no tool (`sticky_tools::in_pass`), never outside any
/// pass, where the bare name `ctx_expand` would stand in. Under protection
/// no stub is issued then: the text stays whole with
/// `recovery.handle_not_followable`, and nothing is stored. Unprotected, the
/// result and its Community tee handle are those of [`compress_tool_result`].
pub fn shape_tool_output(output: &str, tool_name: Option<&str>) -> String {
    super::sticky_tools::in_pass(|| compress_tool_result(output, tool_name))
}

/// [`compress_tool_result`] for the `/v1/compress` gateway contract (#702):
/// identical compression, but a lossy result advertises its retrieval hash in
/// LiteLLM's regex-locked `hash=<24hex>` form, so a gateway running the
/// headroom-guardrail CCR loop (BerriAI/litellm#31681) can inject its retrieve
/// tool and resolve the original via `GET /v1/retrieve/{hash}`.
///
/// Any ambient savings footer is stripped *before* the marker is attached:
/// savings figures belong in the caller's structured `stats` (#498), while the
/// marker is functional content that must survive as the last line.
pub fn compress_tool_result_gateway(content: &str, tool_name: Option<&str>) -> String {
    compress_tool_result_gateway_for(content, tool_name, COUNTING_FAMILY)
}

pub fn compress_tool_result_gateway_for(
    content: &str,
    tool_name: Option<&str>,
    family: TokenizerFamily,
) -> String {
    // Before the footer strip below: a local stub has the footer's shape.
    // Untouched, `<lc_safe>` markers included: see `carries_recovery_stub`.
    if carries_recovery_stub(content) {
        return content.to_owned();
    }
    let compressed = compress_inner(content, tool_name, family);
    let clean = lean_ctx::core::protocol::strip_trailing_savings_footer(&compressed).to_string();
    attach_ccr(content, clean, CcrAudience::Gateway)
}

/// Who reads a CCR stub: a local agent (path handle / in-band marker) or a
/// remote gateway whose agentic loop scans for `hash=<24hex>` (#702).
#[derive(Clone, Copy, PartialEq)]
enum CcrAudience {
    Local,
    Gateway,
}

/// Who reads what a compression pass sends, as the route that runs the pass
/// knows it ([`observing`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Reader {
    /// A model behind a provider: it can call `ctx_expand`, never a route of
    /// this proxy.
    #[cfg(feature = "http-server")]
    Model,
    /// The caller of `/v1/compress`: it can call this proxy's routes.
    GatewayCaller,
}

/// The blocks a compression pass left whole under protection because no
/// recovery copy could back a shorter form, and why. Content-free towards
/// every reader: a block is held by its digest only to count it once, and
/// only codes and counts leave this type.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct KeptWhole {
    blocks: BTreeMap<[u8; 32], &'static str>,
}

impl KeptWhole {
    /// The first code noted for a block stands: a later pass over the same
    /// bytes does not count it again.
    fn note(&mut self, block: &str, code: &'static str) {
        self.blocks
            .entry(*blake3::hash(block.as_bytes()).as_bytes())
            .or_insert(code);
    }

    #[cfg(feature = "http-server")]
    pub(crate) fn merge(&mut self, other: Self) {
        for (block, code) in other.blocks {
            self.blocks.entry(block).or_insert(code);
        }
    }

    #[cfg(feature = "http-server")]
    pub(crate) fn is_empty(&self) -> bool {
        self.blocks.is_empty()
    }

    /// Blocks per code, in code order.
    #[cfg(feature = "http-server")]
    pub(crate) fn counts(&self) -> BTreeMap<&'static str, usize> {
        let mut counts = BTreeMap::new();
        for code in self.blocks.values() {
            *counts.entry(*code).or_insert(0) += 1;
        }
        counts
    }

    /// The distinct codes, comma-separated in code order — the value of the
    /// `x-leanctx-reason-codes` header of a reply that was NOT refused.
    /// `None`: nothing stayed whole for want of a recovery copy.
    #[cfg(feature = "http-server")]
    pub(crate) fn codes(&self) -> Option<String> {
        let counts = self.counts();
        (!counts.is_empty()).then(|| counts.keys().copied().collect::<Vec<_>>().join(","))
    }

    /// `code=count` pairs for a log line.
    #[cfg(feature = "http-server")]
    pub(crate) fn summary(&self) -> String {
        self.counts()
            .iter()
            .map(|(code, count)| format!("{code}={count}"))
            .collect::<Vec<_>>()
            .join(", ")
    }
}

struct Observation {
    reader: Reader,
    kept: KeptWhole,
}

thread_local! {
    /// The compression pass running on this thread, if a route observes it.
    /// Compression is synchronous from the route down to [`attach_ccr`].
    static OBSERVATION: RefCell<Option<Observation>> = const { RefCell::new(None) };
}

#[cfg(test)]
thread_local! {
    /// Every call of [`note_kept_whole`] on this thread. [`KeptWhole`]
    /// counts a block once however often it is noted, so only this shows a
    /// note made twice.
    static NOTE_CALLS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// The calls of [`note_kept_whole`] on this thread so far.
#[cfg(all(test, feature = "http-server"))]
pub(crate) fn note_calls() -> usize {
    NOTE_CALLS.with(std::cell::Cell::get)
}

/// Run one synchronous compression pass for `reader` and return, next to its
/// result, the blocks it left whole under protection. An inner pass reports
/// to its own caller only.
#[cfg(feature = "http-server")]
pub(crate) fn observing<T>(reader: Reader, compression: impl FnOnce() -> T) -> (T, KeptWhole) {
    struct Restore(Option<Observation>);
    impl Drop for Restore {
        fn drop(&mut self) {
            let outer = self.0.take();
            OBSERVATION.with(|slot| *slot.borrow_mut() = outer);
        }
    }
    let restore = Restore(OBSERVATION.with(|slot| {
        slot.replace(Some(Observation {
            reader,
            kept: KeptWhole::default(),
        }))
    }));
    let result = compression();
    let kept = OBSERVATION
        .with(|slot| slot.borrow_mut().take())
        .map_or_else(KeptWhole::default, |observation| observation.kept);
    drop(restore);
    (result, kept)
}

/// The reader of the pass running on this thread; `None` outside a route.
fn observed_reader() -> Option<Reader> {
    OBSERVATION.with(|slot| slot.borrow().as_ref().map(|observation| observation.reader))
}

/// `block` stays whole under protection for the content-free reason `code`.
/// Counted for the route that observes the pass; logged otherwise.
pub(crate) fn note_kept_whole(block: &str, code: &'static str) {
    #[cfg(test)]
    NOTE_CALLS.with(|calls| calls.set(calls.get() + 1));
    OBSERVATION.with(|slot| {
        if let Some(observation) = slot.borrow_mut().as_mut() {
            observation.kept.note(block, code);
        } else {
            tracing::debug!("lean-ctx proxy: block kept whole, no recovery copy ({code})");
        }
    });
}

/// Whether nothing may be shortened without a recovery copy bound to its
/// lineage: a policy pack, governed or sovereign mode, or an unloadable
/// configuration.
pub(crate) fn protected() -> bool {
    lean_ctx::core::context_admission::stored::source_authority_required()
}

/// How a lineage-bound recovery stub opens. Deliberately not the
/// savings-footer shape (`[lean-ctx: `): a footer strip must never eat it.
///
/// The grammar of the stub, stated once: one line,
/// `[lean-ctx recovery: full original archived — <tool>(id="<64 hex>",
/// head=N|search="…"|json_path="…") for a slice]`. `<tool>` is the Engine's
/// expand tool under the name the host of the request registered it with
/// ([`expand_tool_name`]: at most 128 characters of `[A-Za-z0-9_.:/-]`), so
/// it can hold neither `]` nor a line break. Everything that recognises a
/// stub ([`carries_recovery_stub`]) reads the fixed opening and the closing
/// bracket only, whatever the tool is called.
const RECOVERY_STUB_OPEN: &str = "[lean-ctx recovery: ";

/// The Engine tool that resolves a recovery id, by its bare name.
const EXPAND_TOOL: &str = "ctx_expand";

/// The last line of a block that was compressed under protection, for a
/// model: `tool` — the Engine's expand tool as the host registered it —
/// resolves the 64-hex `id` under the authority that holds then. A pure
/// function of the name and the id, so the block stays byte-stable while the
/// host keeps the name.
pub(crate) fn recovery_stub_for(tool: &str, id: &RecoveryId) -> String {
    format!(
        "{RECOVERY_STUB_OPEN}full original archived — \
         {tool}(id=\"{}\", head=N|search=\"…\"|json_path=\"…\") for a slice]",
        id.as_str()
    )
}

/// [`recovery_stub_for`] with the tool's bare name.
#[cfg(test)]
pub(crate) fn recovery_stub(id: &RecoveryId) -> String {
    recovery_stub_for(EXPAND_TOOL, id)
}

/// [`recovery_stub_for`] for the caller of `/v1/compress`, which can follow a
/// route of this proxy. No `hash=`: the id is not a tee hash, and a LiteLLM
/// guardrail must not send it to `/v1/retrieve`.
fn reference_stub(id: &RecoveryId) -> String {
    format!(
        "[lean-ctx CCR: full original archived — GET /v1/references/{} on this gateway \
         returns it while its sources are admitted]",
        id.as_str()
    )
}

/// Upper bound of the length in bytes of a stub with the bare tool name: a
/// shrink below it gains nothing. A stub with a longer, prefixed name is
/// caught where it is attached — a block that would not get shorter with its
/// stub stays as it is.
const STUB_MAX_BYTES: usize = 256;

/// Text that already ends with a way back to its original. A second pass
/// passes it through: compressing it again could drop the handle, and the
/// local stubs have the shape of a savings footer the gateway funnel strips.
///
/// It passes through byte for byte, `<lc_safe>` markers included (bd S1 on
/// !646). Such a text is what an earlier pass of a funnel sent, and that
/// pass already left it without the markers it handled: the stub is
/// appended to a compressed form, which the marker handling of
/// [`compress_inner`] produced. Marker text that is still there is content
/// of the original block itself, not a marker of this pass. And the bytes
/// are those on record: under protection the earlier pass recorded exactly
/// them under the original's lineage (`delivered::record_derived`), and the
/// provider's cache prefix holds them (#448). A rewrite here would make the
/// text one of unknown lineage and break that prefix, for no gain.
fn carries_recovery_stub(content: &str) -> bool {
    let last = content
        .trim_end_matches('\n')
        .rsplit('\n')
        .next()
        .unwrap_or_default();
    last.ends_with(']')
        && [
            RECOVERY_STUB_OPEN,
            "[lean-ctx CCR: full original ",
            "[lean-ctx: full original ",
        ]
        .iter()
        .any(|open| last.starts_with(open))
}

/// Make a live-compressed `tool_result` non-lossy (#482): when compression
/// removed a meaningful amount, tee the verbatim original to the shared
/// content-addressed store and append a deterministic recovery handle. The
/// handle is a pure function of the content hash, so the rewritten result is
/// byte-stable across turns and never invalidates the provider cache prefix
/// (#448). Passthrough / verbatim results (no real shrink) keep their bytes.
/// When no recovery copy can be written (an active policy keeps no originless
/// copies, or the store fails) the original is returned instead of the lossy
/// form: a fresh result is never shortened without a way back.
/// "The original" on every such path is the original kept whole as it leaves
/// a funnel: without its `<lc_safe>` markers ([`keep_whole`]).
fn attach_ccr(original: &str, result: String, audience: CcrAudience) -> String {
    use super::sticky_tools;
    if result == original {
        return result;
    }
    // Every path below that keeps the original keeps it whole, but without
    // its `<lc_safe>` markers (bd N1 on !646): the form a marked result
    // leaves the funnel in when nothing of it is shortened.
    let unmarked = unmarked_form(original);
    let whole = || keep_whole(original, unmarked.as_deref());
    // Only the `<lc_safe>` markers were dropped: nothing was lost, so no way
    // back is needed, and the rule below must not hand the markers back.
    if unmarked.as_deref() == Some(result.as_str()) {
        return whole();
    }
    let shrink = original.len().saturating_sub(result.len());
    if original.len() < ccr::MIN_TEE_BYTES || shrink < ccr::MIN_TEE_BYTES {
        // Too small for a tee, so no way back could be named. Before, an
        // unprotected result left here shortened as it was — a lossy one
        // (`[N lines omitted]` from a dedup of short lines) without any
        // re-read path. A shrink this small is not worth a loss: the
        // original leaves whole, as it already did under protection.
        if !protected() || shrink <= STUB_MAX_BYTES {
            return whole();
        }
    }
    // The gateway funnel on a route whose reader is a model (the forward
    // pipeline) or is unknown: neither the tee hash nor a proxy route is a
    // way back that reader can follow. Neither is a stub naming a tool the
    // host of this request did not list itself, or — under protection — a
    // stub for a block whose own tool call is not on record in this store
    // as answered by an Engine of this store (`sticky_tools::expand_tool_for`
    // states the whole decision). Decided before anything is stored.
    let tool = (audience == CcrAudience::Local)
        .then(|| sticky_tools::expand_tool_for(original))
        .flatten();
    let followable =
        tool.is_some() || observed_reader() == Some(Reader::GatewayCaller) || !protected();
    if !followable {
        note_kept_whole(original, HANDLE_NOT_FOLLOWABLE);
        return whole();
    }
    match recovery_copy(original) {
        Some(RecoveryCopy::Bound {
            id,
            lineage,
            project,
        }) => {
            let stub = if audience == CcrAudience::Local {
                recovery_stub_for(tool.as_deref().unwrap_or(EXPAND_TOOL), &id)
            } else {
                reference_stub(&id)
            };
            let sent = format!("{result}\n{stub}");
            if sent.len() >= original.len() {
                return whole();
            }
            // The one record a site other than an Engine release point makes
            // (`delivered::record_derived`). Guaranteed here:
            // 1. `lineage` came from `lineage_of` for exactly `original`, in
            //    this request, inside the same project scope (`bound_copy`);
            // 2. `sent` derives from `original` alone: `result` is the
            //    compressor's output for that one block, and the rest is the
            //    fixed stub text with the id of its recovery copy;
            // 3. no second block takes part, so there is nothing to join —
            //    a block without a lineage never reaches this arm.
            let recorded = super::request_project::in_project(&project, || {
                delivered::record_derived(&delivered::digest(&sent), &lineage)
            });
            if recorded {
                // The model's stub names the expand tool the host listed:
                // the request's pass holds the stubbed block next to its
                // original until the request has left (`gate::Issued`). The
                // gateway caller's stub names a route, not a tool.
                if audience == CcrAudience::Local {
                    gate::note_issued(original, &sent);
                }
                sent
            } else {
                // The shortened block would be of unknown lineage for every
                // later turn; the original is not.
                kept_after_refused_record(original, unmarked.as_deref())
            }
        }
        // Gateway (#702): the reader is a model behind a LiteLLM-style gateway.
        // A local path is useless there; advertise the 24-hex retrieval hash in
        // the exact shape the guardrail's marker regex captures. The hash is a
        // pure function of the content, so the stub stays byte-stable (#498).
        // The `[lean-ctx CCR:` prefix is deliberately NOT the savings-footer
        // shape (`[lean-ctx: `): this line is functional content — a footer
        // strip must never eat it.
        Some(RecoveryCopy::Tee(handle)) if audience == CcrAudience::Gateway => {
            sticky_tools::mark_ccr_active(0);
            let hash = ccr::litellm_hash(original);
            format!(
                "{result}\n[lean-ctx CCR: full original elided to save tokens — call the \
                 retrieve tool with hash={hash}, or read {handle} locally]"
            )
        }
        Some(RecoveryCopy::Tee(handle)) => {
            sticky_tools::mark_ccr_active(0);
            match ccr::inband_locator(&handle) {
                Some(marker) => format!(
                    "{result}\n[lean-ctx: full original elided to save tokens — echo {marker} \
                     on your next turn to get the verbatim original spliced back inline]"
                ),
                None => format!("{result}\n{}", tee_hint(&handle)),
            }
        }
        None => whole(),
    }
}

/// The text `original` leaves as when it carries `<lc_safe>` markers and is
/// kept whole: the markers dropped, every byte between them verbatim.
/// `None`: there are no markers.
fn unmarked_form(original: &str) -> Option<String> {
    lean_ctx::core::protect::has_markers(original)
        .then(|| lean_ctx::core::protect::compress_preserving(original, str::to_owned))
}

/// `original` kept whole, as it leaves a funnel: without its `<lc_safe>`
/// markers (`unmarked`, from [`unmarked_form`]) — [`whole_form`], with a
/// refused record noted `DERIVED_NOT_RECORDED`.
fn keep_whole(original: &str, unmarked: Option<&str>) -> String {
    let whole = whole_form(original, unmarked);
    if whole.record_refused {
        note_kept_whole(original, DERIVED_NOT_RECORDED);
    }
    whole.text
}

/// What [`whole_form`] keeps.
struct WholeForm {
    text: String,
    /// The unmarked form could not be put on record, so `text` is
    /// `original` with its markers. Not noted yet.
    record_refused: bool,
}

/// `original` kept whole, without its `<lc_safe>` markers where that keeps
/// its lineage. Notes nothing: the caller notes a refused record, once.
///
/// Under protection the unmarked form is a text derived from `original`,
/// and a derived text without a record is of unknown lineage for every
/// later turn (`sealed_origin::Lineage::Unknown`) even where `original`
/// is not. So (bd N2 on !646):
/// - `original` has a complete lineage in the proxy's project: the unmarked
///   form is put on record under it (`delivered::record_derived`). Its
///   preconditions hold — the lineage is `lineage_of` for exactly
///   `original`, in this request and inside the proxy's project scope; the
///   unmarked form derives from that one block alone, with no text added.
///   Without the record `original` stays with its markers
///   (`record_refused`): a known lineage is never traded for a cosmetic
///   change.
/// - `original` has no lineage, or only an incomplete one (bd N5): there is
///   nothing to keep. An incomplete lineage makes a text as unknown as none
///   does (`Lineage::is_complete`; `record_derived` would refuse it too), so
///   both are treated alike: the unmarked form is no less known than the
///   original, and nothing is recorded or stored.
/// - The proxy has no project (bd S2): nothing is looked up or recorded, as
///   in [`bound_copy`], which refuses a copy then (`NO_PROJECT`). Without a
///   project no lineage can be bound for this request at all — a lookup
///   outside a project scope is no lineage of it, and `record_derived`
///   records nothing without a request project — so the case is that of no
///   lineage: the unmarked form leaves, unrecorded. Nothing is noted: no
///   shorter form was refused, and nothing of the block was lost.
///
/// The record is written again whenever the same block is kept whole again
/// (each request, each block). That is idempotent (bd N6):
/// `delivered::record_derived` is `record_lineage`, whose `store_at` keeps
/// one entry per derived digest and project and joins a repeat into it
/// (`Lineage::join`). A union with the same lineage adds no source, and the
/// presenter reference only moves to the newest place, so a repeat adds
/// nothing but a fresh time.
fn whole_form(original: &str, unmarked: Option<&str>) -> WholeForm {
    let kept = |text: &str, record_refused| WholeForm {
        text: text.to_owned(),
        record_refused,
    };
    let Some(unmarked) = unmarked else {
        return kept(original, false);
    };
    if !protected() {
        return kept(unmarked, false);
    }
    let Some(project) = super::request_project::current_source_project() else {
        return kept(unmarked, false);
    };
    let recorded = super::request_project::in_project(&project, || {
        delivered::lineage_of(&delivered::digest(original))
            .filter(delivered::Lineage::is_complete)
            .map(|lineage| delivered::record_derived(&delivered::digest(unmarked), &lineage))
    });
    if recorded == Some(false) {
        return kept(original, true);
    }
    kept(unmarked, false)
}

/// `original` kept whole because the record of its shortened form was
/// refused: noted `DERIVED_NOT_RECORDED` once (bd S3 on !646). The note
/// stands for the refused shorter form, so it is made whatever happens to
/// the unmarked form; [`whole_form`] notes nothing, so a refused record of
/// the unmarked form as well adds no second note.
fn kept_after_refused_record(original: &str, unmarked: Option<&str>) -> String {
    note_kept_whole(original, DERIVED_NOT_RECORDED);
    whole_form(original, unmarked).text
}

/// [`keep_whole`] for a text whose unmarked form is not at hand yet.
fn whole_without_markers(original: &str) -> String {
    keep_whole(original, unmarked_form(original).as_deref())
}

/// The one place a compressed result gets its way back: store a recovery
/// copy of `original` and return its handle, or `None` when no copy may
/// exist — in which case the caller forwards the original instead of the
/// lossy form.
///
/// - Unprotected (Community, developer mode): the originless tee store, as
///   before.
/// - Protected (a policy pack, governed or sovereign mode, an unloadable
///   configuration): a copy exists only for a block whose sources are known
///   ([`bound_copy`]). The proxy only sees a request body and observed no
///   read; permission to read is not permission to retain. Without a known,
///   currently admitted lineage there is no copy and the caller keeps the
///   original.
fn recovery_copy(original: &str) -> Option<RecoveryCopy> {
    if !lean_ctx::core::context_admission::stored::source_authority_required() {
        return ccr::persist(original).map(RecoveryCopy::Tee);
    }
    match bound_copy(original) {
        Ok(copy) => Some(copy),
        Err(code) => {
            note_kept_whole(original, code);
            None
        }
    }
}

/// The gate of every transformation other than the compressor that replaces
/// a block by a shorter form under protection (history pruning, truncation):
/// `shortened` followed by the stub of a recovery copy of `original`, on
/// record under the original's lineage — or `None`, and the caller keeps
/// `original` whole. `None` is noted with its code, except where the form
/// with its stub would not be shorter at all.
pub(crate) fn shorten_with_copy(original: &str, shortened: &str) -> Option<String> {
    // Decided before anything is stored: a request whose host did not list
    // the expand tool itself hands out no stub that names it, and under
    // protection neither does text that is not one tool-result block whose
    // call this store's Engine answered (`sticky_tools::expand_tool_for`).
    let Some(tool) = super::sticky_tools::expand_tool_for(original) else {
        note_kept_whole(original, HANDLE_NOT_FOLLOWABLE);
        return None;
    };
    let copy = match bound_copy(original) {
        Ok(copy) => copy,
        Err(code) => {
            note_kept_whole(original, code);
            return None;
        }
    };
    let RecoveryCopy::Bound {
        id,
        lineage,
        project,
    } = copy
    else {
        return None;
    };
    let sent = format!("{shortened}\n{}", recovery_stub_for(&tool, &id));
    if sent.len() >= original.len() {
        return None;
    }
    // `record_derived`, as in `attach_ccr`: the lineage is that of exactly
    // `original`, looked up in this request and scope; `sent` is a cut of
    // that one block plus fixed text of the cutting code and the stub.
    let recorded = super::request_project::in_project(&project, || {
        delivered::record_derived(&delivered::digest(&sent), &lineage)
    });
    if recorded {
        // This stub names the host's expand tool as well (see `attach_ccr`).
        gate::note_issued(original, &sent);
        Some(sent)
    } else {
        note_kept_whole(original, DERIVED_NOT_RECORDED);
        None
    }
}

/// A block that an earlier request already carried may not be replaced by a
/// pointer to that request: nothing in THIS request holds its content.
pub(crate) const DEDUP_WITHOUT_COPY: &str = "recovery.dedup_without_copy";
/// The conversation may not be replaced by a summary: no copy backs it.
#[cfg(feature = "http-server")]
pub(crate) const SUMMARY_WITHOUT_COPY: &str = "recovery.summary_without_copy";

/// The reader of this pass cannot follow any handle a copy would have.
const HANDLE_NOT_FOLLOWABLE: &str = "recovery.handle_not_followable";
/// The copy exists, but the shortened block could not be put on record
/// under the lineage of its original.
const DERIVED_NOT_RECORDED: &str = "recovery.derived_not_recorded";
/// No project of the proxy that sources could be admitted for.
const NO_PROJECT: &str = "recovery.no_project";
/// The block is not text the Engine delivered with a complete lineage.
const UNKNOWN_LINEAGE: &str = "recovery.unknown_lineage";

/// The recovery copy of `original` under protection, or the content-free
/// code of why there is none. Inside the scope of the proxy's own project
/// (operator configuration, `request_project`): the Engine's lineage of
/// exactly this text, and a copy stored under it — which the store accepts
/// only while every source of that lineage is admitted now. A file-only
/// lineage lands on the Engine's existing source-bound archive entry for the
/// same text, so no second copy is written.
fn bound_copy(original: &str) -> Result<RecoveryCopy, &'static str> {
    let project = super::request_project::current_source_project().ok_or(NO_PROJECT)?;
    super::request_project::in_project(&project, || {
        let lineage = delivered::lineage_of(&delivered::digest(original))
            .filter(delivered::Lineage::is_complete)
            .ok_or(UNKNOWN_LINEAGE)?;
        let id = lean_ctx::core::archive::store_recovery_with_lineage(original, &lineage)
            .map_err(recovery_code)?;
        Ok((id, lineage))
    })
    .map(|(id, lineage)| RecoveryCopy::Bound {
        id,
        lineage: Box::new(lineage),
        project,
    })
}

/// The content-free code of a recovery refusal.
pub(crate) fn recovery_code(refusal: RecoveryRefusal) -> &'static str {
    match refusal {
        RecoveryRefusal::UnknownLineage => UNKNOWN_LINEAGE,
        RecoveryRefusal::SourceNotPresented => "recovery.source_not_presented",
        RecoveryRefusal::SourceNoLongerAdmitted => "recovery.source_no_longer_admitted",
        RecoveryRefusal::NotStorable => "recovery.not_storable",
        RecoveryRefusal::Missing => "recovery.missing",
    }
}

/// Resolve a recovery id under the authority that holds now, for the proxy's
/// own project. `None`: `id` is not a recovery id at all. Without a project
/// nothing is looked up.
#[cfg(feature = "http-server")]
pub(crate) fn resolve_recovery(id: &str) -> Option<Result<String, &'static str>> {
    let id = RecoveryId::parse(id)?;
    let Some(project) = super::request_project::current_source_project() else {
        // Unprotected deployments hold no recovery copies: not a recovery id.
        return lean_ctx::core::context_admission::stored::source_authority_required()
            .then_some(Err(NO_PROJECT));
    };
    Some(super::request_project::in_project(&project, || {
        lean_ctx::core::archive::retrieve_recovery_checked(&id).map_err(recovery_code)
    }))
}

/// Where the way back of a compressed result leads.
enum RecoveryCopy {
    /// A tee file of the originless store; the value is its handle.
    Tee(String),
    /// A copy in the Engine's archive, bound to the lineage of the original
    /// and stored for `project`.
    Bound {
        id: RecoveryId,
        lineage: Box<delivered::Lineage>,
        project: std::path::PathBuf,
    },
}

fn compress_inner(content: &str, tool_name: Option<&str>, family: TokenizerFamily) -> String {
    if content.trim().is_empty() {
        return content.to_string();
    }
    // #479: lean-ctx's own MCP tools already applied their compression policy at
    // the tool boundary — honouring `raw=true`/`bypass`, `<lc_safe>` spans and
    // the configured aggressiveness. Their `raw` intent lives in the originating
    // `tool_use` input, which is invisible here, so re-compressing on the wire
    // would silently undo an explicit `raw=true` and double-compress everything
    // else. Pass results from `ctx_*` tools through untouched.
    //
    // Untouched includes any `<lc_safe>` text in them (bd S1 on !646). The
    // Engine handles its markers at the tool boundary: its shell compressor
    // splices protected spans and drops the markers
    // (`shell::compress::engine`), so a `ctx_shell` result leaves without
    // them. Marker text that remains is content the tool was asked to
    // deliver — e.g. a `ctx_read` of a file that itself contains
    // `<lc_safe>`, verbatim under `raw=true` — and under protection the
    // Engine recorded exactly these bytes with their lineage
    // (`delivered::record_tool_delivery`). Stripping it here would alter a
    // raw read and turn a known lineage into an unknown one.
    if tool_name.is_some_and(is_lean_ctx_tool) {
        return content.to_string();
    }

    // #709: honour explicit <lc_safe>…</lc_safe> spans on the proxy path too.
    // Protected spans pass through verbatim; each unprotected segment flows back
    // through the normal funnel (markers are stripped, so this never recurses).
    if lean_ctx::core::protect::has_markers(content) {
        return lean_ctx::core::protect::compress_preserving(content, |seg| {
            compress_inner(seg, tool_name, family)
        });
    }

    // After the marker handling, so a result of any size leaves without its
    // `<lc_safe>` markers — below 200 bytes as well (bd N3 on !646): each
    // unprotected segment meets these rules itself.
    if content.len() < 200 || fits_verbatim(content) {
        return content.to_string();
    }
    if TOOL_COMMAND.with(|command| {
        command
            .borrow()
            .as_deref()
            .is_some_and(|command| keeps_filtered_output(command, content))
    }) {
        return content.to_string();
    }

    if is_cited_research_output(content) {
        if looks_like_markdown(content) {
            let prose = compress_prose(content, None);
            if prose.compressed_tokens < prose.original_tokens {
                return prose.compressed;
            }
        }
        return content.to_string();
    }

    if extract_command_hint(content).is_none()
        && looks_like_prose(content)
        && let Some(out) = squeeze_research_prose(content, family)
    {
        return out;
    }

    let cmd = infer_command(content, tool_name);

    // Proxy fidelity guard. A foreign shell tool gives us at most a generic
    // command (`"shell"`) or none, so the engine's command-gated build/test
    // verbatim guards never fire. When the *output* is unmistakably a build or
    // test run, preserve it verbatim (bounded by safety-line-preserving
    // truncation) so compiler errors, panics and test summaries reach the model
    // intact — the exact signal a bug-fix task depends on.
    let generic_command = cmd.is_empty() || cmd == "shell";
    if generic_command
        && (output_looks_like_test_run(content) || output_looks_like_build_failure(content))
    {
        return lean_ctx::shell::compress::engine::preserve_verbatim_pub_for(content, family);
    }

    lean_ctx::shell::compress::engine::compress_if_beneficial_for(&cmd, content, family)
}

/// Strong, ecosystem-spanning signals that an output is a *test run* (passing or
/// failing). Conservative — matches the summary/result lines a bug-fix task must
/// never lose. Only consulted on the proxy path when the real command is unknown.
fn output_looks_like_test_run(content: &str) -> bool {
    const NEEDLES: &[&str] = &[
        "test result:",            // rust
        "short test summary info", // pytest
        " passed in ",             // pytest summary
        " failed in ",             // pytest summary
        "=== RUN",                 // go
        "--- FAIL:",               // go
        "--- PASS:",               // go
        "Test Suites:",            // jest
        " examples, ",             // rspec ("5 examples, 0 failures")
        "FAILED",                  // generic test failure marker
    ];
    NEEDLES.iter().any(|n| content.contains(n))
}

/// Strong, specific signals of a build / compile / runtime failure across the
/// major toolchains. Used only on the proxy path for generically-named tools so
/// the failing diagnostics (paths, lines, messages) survive intact.
fn output_looks_like_build_failure(content: &str) -> bool {
    const NEEDLES: &[&str] = &[
        "error[",                            // rustc (E0277 …)
        ": error:",                          // gcc / clang "file.c:12:5: error:"
        "fatal error:",                      // gcc / clang
        "undefined reference to",            // linker
        "panicked at",                       // rust runtime
        "could not compile",                 // cargo
        "Traceback (most recent call last)", // python
        "AssertionError",                    // python / junit
        "make: ***",                         // make
        "Build FAILED",
        "BUILD FAILED",
        "Segmentation fault",
    ];
    NEEDLES.iter().any(|n| content.contains(n))
}

/// True when `content` is a lean-ctx web read: distilled body + citation footer
/// (`Source: …\nSite: … · Retrieved: …`). Such output is re-compression-hostile.
fn is_cited_research_output(content: &str) -> bool {
    content.contains("· Retrieved: ") && content.contains("\nSource: ")
}

/// Code/shell symbols whose density cleanly separates source/logs from prose.
const CODE_SYMBOLS: &str = "{}<>;=|\\$`";

/// Conservative prose detector: substantial, letter-dense, low code-symbol, with
/// real sentences and long lines. Code, logs, tables and JSON all fail this.
fn looks_like_prose(content: &str) -> bool {
    let sample: String = content.chars().take(4000).collect();
    let total = sample.chars().count();
    if total < 600 {
        return false;
    }
    let total_f = total as f32;
    let alpha = sample.chars().filter(|c| c.is_alphabetic()).count() as f32;
    let spaces = sample.chars().filter(|c| *c == ' ').count() as f32;
    let symbols = sample.chars().filter(|c| CODE_SYMBOLS.contains(*c)).count() as f32;

    if alpha / total_f < 0.6 || spaces / total_f < 0.12 || symbols / total_f > 0.06 {
        return false;
    }
    if sample.matches(['.', '!', '?']).count() < 4 {
        return false;
    }

    let non_empty: Vec<&str> = sample.lines().filter(|l| !l.trim().is_empty()).collect();
    if non_empty.is_empty() {
        return false;
    }
    let avg_len =
        non_empty.iter().map(|l| l.chars().count()).sum::<usize>() as f32 / non_empty.len() as f32;
    avg_len >= 40.0
}

fn looks_like_markdown(content: &str) -> bool {
    content.lines().any(|line| {
        let trimmed = line.trim_start();
        trimmed.starts_with("# ")
            || trimmed.starts_with("## ")
            || trimmed.starts_with("```")
            || trimmed.starts_with("~~~")
            || matches!(trimmed.as_bytes(), [b'-' | b'*' | b'+', b' ', ..])
            || trimmed.matches('|').count() >= 2
    })
}

/// Apply the prose squeeze, returning a footer-stamped result only when it
/// actually saves tokens; otherwise `None` so the normal pipeline can try.
fn squeeze_research_prose(content: &str, family: TokenizerFamily) -> Option<String> {
    let before = count_tokens_for(content, family);
    let squeezed = squeeze_research_prose_body(content);
    if squeezed.trim().is_empty() {
        return None;
    }
    let after = count_tokens_for(&squeezed, family);
    if after + 2 >= before {
        return None;
    }
    Some(lean_ctx::core::protocol::append_savings_with_info(
        &squeezed,
        before,
        after,
        Some("research"),
        None,
    ))
}

/// Choose the prose-squeeze body. Only when the content would actually be
/// TRUNCATED (over the cap) do we upgrade from FIFO prefix truncation to
/// extractive centrality ranking — which keeps the most representative sentences
/// instead of just the first ones — via the cache-safe, memoized wire squeeze
/// ([`crate::prose_ranker`]), so the cold→warm engine transition never
/// changes a frozen-region rewrite (#448/#498). Below the cap the squeeze is a
/// lossless dedup pass, so the cheaper truncating squeeze is used.
fn squeeze_research_prose_body(content: &str) -> String {
    let cap = research_prose_cap();
    if content.len() > cap {
        return super::prose_ranker::squeeze(content, cap);
    }
    distill::squeeze_prose(content, cap)
}

/// True when `name` refers to one of lean-ctx's own `ctx_*` MCP tools, whose
/// results are already compressed at the tool boundary and must not be touched
/// again by the proxy (#479).
///
/// Clients namespace MCP tools differently, so a plain `starts_with("ctx_")`
/// misses the real-world callers: Claude Code (the reporter's setup) sends
/// `mcp__lean-ctx__ctx_shell`, others use `lean-ctx:ctx_read`. Strip the client
/// prefix down to the bare tool segment before matching.
fn is_lean_ctx_tool(name: &str) -> bool {
    bare_tool_name(name).starts_with("ctx_") || name.starts_with("ctx_")
}

/// The tool segment of a name a client namespaced: what follows the last
/// `__` and, in that, the last `:`, `/` or `.`.
pub(crate) fn bare_tool_name(name: &str) -> &str {
    name.rsplit("__")
        .next()
        .unwrap_or(name)
        .rsplit([':', '/', '.'])
        .next()
        .unwrap_or(name)
}

/// Whether `name` — a tool name as a host listed it — is the Engine's expand
/// tool under a name a recovery stub can carry: the bare segment is
/// `ctx_expand` (the stripping of [`is_lean_ctx_tool`]) and the whole name
/// is at most 128 characters of `[A-Za-z0-9_.:/-]`, so it cannot break the
/// one-line grammar of the stub. Anything else is not the tool.
#[cfg(feature = "http-server")]
pub(crate) fn expand_tool_name(name: &str) -> bool {
    bare_tool_name(name) == EXPAND_TOOL
        && name.len() <= 128
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"_.:/-".contains(&byte))
}

fn infer_command(content: &str, tool_name: Option<&str>) -> String {
    if let Some(cmd) = extract_command_hint(content) {
        return cmd;
    }

    if let Some(name) = tool_name {
        let nl = name.to_lowercase();
        if nl.contains("bash") || nl.contains("shell") || nl.contains("terminal") {
            return "shell".to_string();
        }
        if nl.contains("search") || nl.contains("grep") || nl.contains("find") {
            return "grep".to_string();
        }
    }

    String::new()
}

fn extract_command_hint(content: &str) -> Option<String> {
    for line in content.lines().take(3) {
        let trimmed = line.trim();
        if let Some(cmd) = trimmed.strip_prefix("$ ") {
            return Some(cmd.to_string());
        }
        if let Some(cmd) = trimmed.strip_prefix("% ") {
            return Some(cmd.to_string());
        }
    }
    None
}

#[cfg(test)]
#[path = "compress_tests.rs"]
mod tests;

#[cfg(all(test, feature = "http-server"))]
#[path = "compress_recovery_tests.rs"]
mod recovery_tests;
