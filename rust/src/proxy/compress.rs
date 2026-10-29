use std::cell::RefCell;
use std::collections::BTreeMap;

use super::ccr;
use super::prose_compress::compress_prose;
use crate::core::archive::{RecoveryId, RecoveryRefusal, delivered};
use crate::core::tokens::{COUNTING_FAMILY, TokenizerFamily, count_tokens_for};
use crate::core::web::distill;

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
pub fn compress_tool_result_with_policy(
    content: &str,
    tool_name: Option<&str>,
    policy: crate::proxy::adaptive_policy::CompressionPolicy,
) -> String {
    if !policy.compress_tool_output || policy.log_compression_level == 0 {
        return content.to_owned();
    }
    if policy.code_preserve && looks_like_code(content) {
        return content.to_owned();
    }
    compress_tool_result(content, tool_name)
}

/// Gateway variant of [`compress_tool_result_with_policy`].
pub fn compress_tool_result_gateway_with_policy(
    content: &str,
    tool_name: Option<&str>,
    family: TokenizerFamily,
    policy: crate::proxy::adaptive_policy::CompressionPolicy,
) -> String {
    if !policy.compress_tool_output || policy.log_compression_level == 0 {
        return content.to_owned();
    }
    if policy.code_preserve && looks_like_code(content) {
        return content.to_owned();
    }
    compress_tool_result_gateway_for(content, tool_name, family)
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
    if output.trim().is_empty() || output.len() < 200 {
        return output.to_string();
    }
    if carries_recovery_stub(output) {
        return output.to_string();
    }
    let compressed = match exit_code {
        Some(code) => crate::shell::compress::engine::compress_for_outcome(command, output, code),
        None => crate::shell::compress::engine::compress_if_beneficial_pub(command, output),
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
    if carries_recovery_stub(content) {
        return content.to_owned();
    }
    let compressed = compress_inner(content, tool_name, family);
    let clean = crate::core::protocol::strip_trailing_savings_footer(&compressed).to_string();
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
    crate::core::context_admission::stored::source_authority_required()
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
fn attach_ccr(original: &str, result: String, audience: CcrAudience) -> String {
    use super::sticky_tools;
    if result == original {
        return result;
    }
    let shrink = original.len().saturating_sub(result.len());
    if original.len() < ccr::MIN_TEE_BYTES || shrink < ccr::MIN_TEE_BYTES {
        // Too small for a tee: unprotected, the result leaves as it is.
        if !protected() {
            return result;
        }
        // Under protection a changed block leaves only with the stub of a
        // recovery copy. Where the stub costs more than this pass saved,
        // nothing is gained and nothing is changed — not a refusal, no code.
        if shrink <= STUB_MAX_BYTES {
            return original.to_string();
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
        return original.to_string();
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
                return original.to_string();
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
                note_kept_whole(original, DERIVED_NOT_RECORDED);
                original.to_string()
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
                None => format!(
                    "{result}\n[lean-ctx: full original at {handle} — read it directly (no MCP), or \
                     ctx_expand(id=\"{handle}\", head=N|search=\"…\"|json_path=\"…\") for a slice]"
                ),
            }
        }
        None => original.to_string(),
    }
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
    if !crate::core::context_admission::stored::source_authority_required() {
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
        let id = crate::core::archive::store_recovery_with_lineage(original, &lineage)
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
        return crate::core::context_admission::stored::source_authority_required()
            .then_some(Err(NO_PROJECT));
    };
    Some(super::request_project::in_project(&project, || {
        crate::core::archive::retrieve_recovery_checked(&id).map_err(recovery_code)
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
    if content.trim().is_empty() || content.len() < 200 {
        return content.to_string();
    }

    // #479: lean-ctx's own MCP tools already applied their compression policy at
    // the tool boundary — honouring `raw=true`/`bypass`, `<lc_safe>` spans and
    // the configured aggressiveness. Their `raw` intent lives in the originating
    // `tool_use` input, which is invisible here, so re-compressing on the wire
    // would silently undo an explicit `raw=true` and double-compress everything
    // else. Pass results from `ctx_*` tools through untouched.
    if tool_name.is_some_and(is_lean_ctx_tool) {
        return content.to_string();
    }

    // #709: honour explicit <lc_safe>…</lc_safe> spans on the proxy path too.
    // Protected spans pass through verbatim; each unprotected segment flows back
    // through the normal funnel (markers are stripped, so this never recurses).
    if crate::core::protect::has_markers(content) {
        return crate::core::protect::compress_preserving(content, |seg| {
            compress_inner(seg, tool_name, family)
        });
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
        return crate::shell::compress::engine::preserve_verbatim_pub_for(content, family);
    }

    crate::shell::compress::engine::compress_if_beneficial_for(&cmd, content, family)
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
    Some(crate::core::protocol::append_savings_with_info(
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
/// ([`crate::proxy::prose_ranker`]), so the cold→warm engine transition never
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

#[cfg(test)]
#[path = "compress_recovery_tests.rs"]
mod recovery_tests;
