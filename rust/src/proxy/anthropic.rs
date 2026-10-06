use axum::{
    body::Body,
    extract::State,
    http::{Request, StatusCode},
    response::Response,
};
use serde_json::Value;

use super::ProxyState;
use super::compress_shared::{self, ToolKind};
use super::forward;
use super::tool_kind::{self, ToolResultKind};
use super::{cache_safety, prefix_cache_stats, prefix_replay, prose, sticky_tools};

std::thread_local! {
    /// Set by `forward.rs` when the current request has the `X-Headroom-Compressed` header.
    static HEADROOM_REQUEST: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Called from `forward.rs` to signal that this request was pre-compressed by Headroom.
pub(super) fn set_headroom_request(val: bool) {
    HEADROOM_REQUEST.set(val);
}
use crate::core::config::{HistoryMode, ProseRole};

/// GH #1472: Detect whether an Anthropic-wire-format request is actually
/// GitHub Copilot Claude (same POST /v1/messages shape, but must route to
/// Copilot hosts, not api.anthropic.com).
///
/// Detection signals (any one is sufficient):
/// - `Editor-Version` header present (Copilot IDE integration)
/// - `Copilot-Integration-Id` header present
/// - `Authorization: Bearer` with Copilot session token shape (tid=…;exp=…)
/// - Configured `openai_upstream` is a githubcopilot.com host
fn is_copilot_request(req: &Request<Body>, openai_upstream: &str) -> bool {
    let headers = req.headers();

    // Signal 1: Copilot IDE headers
    if headers.contains_key("editor-version") || headers.contains_key("copilot-integration-id") {
        return true;
    }

    // Signal 2: Authorization bearer looks like a Copilot token
    if let Some(auth) = headers.get("authorization").and_then(|v| v.to_str().ok()) {
        let token = auth.strip_prefix("Bearer ").unwrap_or("");
        if token.starts_with("ghu_")
            || token.starts_with("gho_")
            || token.contains("tid=")
            || token.contains("proxy-ep=")
        {
            return true;
        }
    }

    // Signal 3: Configured openai_upstream is a Copilot host
    if is_copilot_host(openai_upstream) {
        return true;
    }

    false
}

fn is_copilot_host(url: &str) -> bool {
    let lower = url.to_lowercase();
    lower.contains("githubcopilot.com")
        || lower.contains("copilot-proxy")
        || lower.contains("copilot-api")
}

/// Resolve the upstream for a Copilot-shaped Anthropic request.
/// Prefers the token's `proxy-ep` field, falls back to the configured
/// `openai_upstream` (which users set to their Copilot endpoint).
fn resolve_copilot_upstream(req: &Request<Body>, openai_upstream: &str) -> String {
    // Try extracting proxy-ep from the Authorization token
    if let Some(auth) = req
        .headers()
        .get("authorization")
        .and_then(|v| v.to_str().ok())
    {
        let token = auth.strip_prefix("Bearer ").unwrap_or("");
        if let Some(ep) = extract_proxy_ep(token) {
            return format!("https://{ep}");
        }
    }
    openai_upstream.to_string()
}

/// Extract `proxy-ep=host` from a Copilot session token.
fn extract_proxy_ep(token: &str) -> Option<&str> {
    for part in token.split(';') {
        let trimmed = part.trim();
        if let Some(val) = trimmed.strip_prefix("proxy-ep=") {
            if !val.is_empty() {
                return Some(val);
            }
        }
    }
    None
}

pub async fn handler(
    State(state): State<ProxyState>,
    req: Request<Body>,
) -> Result<Response, StatusCode> {
    let openai_up = state.openai_upstream();

    let (upstream, provider_label) = if is_copilot_request(&req, &openai_up) {
        let copilot_upstream = resolve_copilot_upstream(&req, &openai_up);
        tracing::info!(
            "Copilot Claude detected (Anthropic wire format), routing to {copilot_upstream}"
        );
        (copilot_upstream, "Copilot")
    } else {
        (state.anthropic_upstream(), "Anthropic")
    };

    forward::forward_request(
        State(state),
        req,
        &upstream,
        "/v1/messages",
        compress_request_body,
        provider_label,
        &[],
    )
    .await
}

pub(super) fn compress_request_body(
    parsed: Value,
    original_size: usize,
) -> (Vec<u8>, usize, usize) {
    let mut doc = parsed;
    let mut modified = false;

    // Opt-in per-role prose aggressiveness (#710). Both default to `None`, in
    // which case nothing below fires and the body is byte-for-byte unchanged.
    let cfg = crate::core::config::Config::load();
    let config_headroom = cfg.proxy.is_headroom_compat();
    if config_headroom {
        prefix_cache_stats::record_headroom_compat();
    }
    // Per-request Headroom detection: if this specific request carries the
    // X-Headroom-Compressed header, treat it as headroom-compat even without
    // the global config flag. The header is checked in forward.rs and the
    // result is threaded through via a thread-local set by the caller.
    let _headroom_compat = config_headroom || HEADROOM_REQUEST.get();
    // #1905: input-compression holdout. The forward path decided the arm on the
    // pristine body; the control arm turns off every input-compression stage so
    // the meter sees a real uncompressed baseline. Cache-only features stay as
    // they are in both arms, keeping the comparison about compression alone.
    let compression_control = super::holdout::in_compression_control();
    let system_aggr = cfg
        .proxy
        .resolved_role_aggressiveness(ProseRole::System)
        .filter(|_| !compression_control);
    let user_aggr = cfg
        .proxy
        .resolved_role_aggressiveness(ProseRole::User)
        .filter(|_| !compression_control);
    let live_compress = cfg.proxy.live_compresses() && !compression_control;
    let mode = if compression_control {
        HistoryMode::Off
    } else {
        cfg.proxy.resolved_history_mode()
    };
    // #939: active prompt-cache breakpoint injection (opt-in, Anthropic-only).
    // Resolved up front so the meter-only short-circuit below does not skip the
    // one mutation this mode performs — its whole point is to add a cache anchor
    // to an otherwise byte-passthrough request.
    let inject_breakpoint = cfg.proxy.cache_breakpoint_enabled();
    // #940: cache-aligner volatile-field telemetry (default-on, measurement-only).
    // Also resolved up front so a meter-only proxy still reaches the scan slot —
    // it never mutates the body, it only records how much cache the system prompt
    // leaks, so it ships on for every proxy (#986 premium defaults).
    let align_volatile = cfg.proxy.cache_aligner_enabled();
    // #974: active cache-aligner relocate (opt-in, Anthropic-only). Resolved up
    // front like the telemetry above so a meter-only proxy still reaches the
    // relocate slot — this is the one mutation that moves volatile fields out of
    // the cacheable prefix.
    let relocate_volatile = cfg.proxy.cache_align_relocate_enabled();
    // #986: cache-economics (default-on). Resolved up front so the meter-only
    // short-circuit below still reaches the miss-attribution slot — that
    // telemetry only reads the cacheable prefix, it never mutates the body, and
    // the paired net-cost gate only makes the cold-prefix repack more
    // conservative, so both halves ship on for every proxy (#986 premium
    // defaults).
    let cache_economics = cfg.proxy.cache_policy_enabled();
    // #895 Track B: output-savings holdout arm, from the pristine body (before any
    // mutation below) so it matches the arm the response meter records. Control
    // conversations skip output-shaping (effort + verbosity steer) but are still
    // metered. Default holdout=0 → always Treatment (no behaviour change).
    let arm = super::holdout::assign(
        &super::holdout::anthropic_key(&doc),
        cfg.proxy.output_holdout_fraction(),
    );
    // #493: in-band CCR expansion (opt-in). Splice any <lc_expand:HASH> the model
    // echoed back into the verbatim original from the local tee store. A strict
    // no-op when no marker is present (byte-identical body → cache-safe). Runs
    // before the meter-only short-circuit so an explicit expand request is
    // honored even when the proxy is otherwise byte-passthrough.
    if cfg.proxy.ccr_inband_enabled() {
        modified |= super::ccr::splice_inband_in_place(&mut doc);
    }
    // #834: cache-safe cross-provider effort control. Default off → no-op. The
    // value is a constant, so it never perturbs the prompt-cache prefix; it only
    // dials an *existing* adaptive thinking request (never enables thinking the
    // client didn't ask for).
    if arm == super::holdout::Arm::Treatment {
        if let Some(effort) = cfg.proxy.resolved_effort() {
            modified |= super::effort::apply_anthropic(&mut doc, effort);
        }
        // #895: cache-safe wire verbosity steer (constant suffix after the last
        // cache_control breakpoint). Control arm skips it so the holdout measures
        // its effect.
        if cfg.proxy.verbosity_steer_enabled() {
            modified |= super::verbosity::apply_anthropic(&mut doc);
        }
    }
    // Meter-only (#481): live compression off, no history pruning, no prose
    // rewriting → forward + usage metering still run, but the body is left
    // unchanged so the provider prompt-cache prefix stays byte-stable. A pending
    // in-band splice (`modified`) opts out: the body did change this turn.
    if !live_compress
        && mode == HistoryMode::Off
        && system_aggr.is_none()
        && user_aggr.is_none()
        && !modified
        && !inject_breakpoint
        && !align_volatile
        && !relocate_volatile
        && !cache_economics
    {
        let out = serde_json::to_vec(&doc).unwrap_or_default();
        return (out, original_size, original_size);
    }
    let mut prose_segments: u64 = 0;

    // Length of the client's provider-cached message prefix. Needed both for
    // cache-safe pruning below and to gate top-level system prose: if any
    // message is client-cached, `system` (which precedes every message) is part
    // of that cached prefix and must not be rewritten.
    let cached = doc
        .get("messages")
        .and_then(|m| m.as_array())
        .map_or(0, |m| super::history_prune::cached_prefix_len(m));

    // #480: opt-in big-gap cold-prefix repack. When enabled AND the proxy can
    // confidently predict (from idle time vs the provider cache TTL) that the
    // client-cached prefix is already cold, override the normal "never touch the
    // cached prefix" rule for THIS request and prune/compress the prefix too,
    // re-seeding a leaner cache. Default-off; never fires without a measured idle
    // gap past TTL × margin, so warm caches stay byte-stable (#448).
    // #986: cache-economics miss attribution (opt-in, measurement-only). Classify
    // why this turn hits or misses the provider prompt-cache (TTL lapse vs prefix
    // change) and bump the `/status` gauges. Reads the cacheable prefix only — the
    // body is never touched — so it is strictly cache-safe.
    if cache_economics
        && let Some(m) = doc.get("messages").and_then(|m| m.as_array())
        && let Some(outcome) = super::cache_attribution::record_request(m, cached)
    {
        match outcome {
            super::cache_attribution::CacheOutcome::WarmReuse => {
                prefix_cache_stats::record_hit();
            }
            super::cache_attribution::CacheOutcome::ColdStart => {}
            _ => {
                prefix_cache_stats::record_miss();
            }
        }
    }
    // #480 repack decision, with the #986 net-cost gate folded in: when
    // cache-economics is on, also require the prefix to be large enough to cache
    // (`worth_repacking`). The gate is an extra AND-condition, so it can only make
    // repacking *more* conservative; default-off proxies keep the prior value.
    let repack = cfg.proxy.repacks_cold_prefix()
        && !compression_control
        && doc
            .get("messages")
            .and_then(|m| m.as_array())
            .is_some_and(|m| {
                super::cold_prefix::repack_decision(m, cached)
                    && (!cache_economics
                        || super::cache_policy::worth_repacking(doc.get("system"), m, cached))
            });
    // The prefix length the rewrites below must protect: the full cached prefix
    // normally, or 0 when we are intentionally repacking the cold prefix.
    let protect = if repack { 0 } else { cached };

    // System prose: compressed freely when nothing is client-cached, or when a
    // cold-prefix repack deliberately rewrites it to re-seed a leaner cache.
    // With a warm cached prefix the system block is part of that prefix, so
    // mutating it busts the cache: only the cache-economics gate may approve
    // that, priced on the whole prefix, and a per-conversation latch then keeps
    // every later turn on the compressed prefix (#1912). A `cache_control` on
    // `system` itself always anchors it (unless repacking).
    let warm_gate = if system_aggr.is_some()
        && !repack
        && cached > 0
        && cache_economics
        && doc
            .get("system")
            .is_some_and(|s| !prose::value_has_cache_control(s))
    {
        let messages = doc
            .get("messages")
            .and_then(Value::as_array)
            .map_or(&[][..], Vec::as_slice);
        super::cold_prefix::conversation_key(messages).map(|key| {
            (
                key,
                super::cache_policy::prefix_tokens(doc.get("system"), messages, cached),
                super::cache_policy::observed_prefix_reuse(messages),
                doc.get("model")
                    .and_then(Value::as_str)
                    .unwrap_or("default")
                    .to_owned(),
            )
        })
    } else {
        None
    };
    if let Some(a) = system_aggr
        && let Some(system) = doc.get_mut("system")
        && (repack || !prose::value_has_cache_control(system))
    {
        let segments = if repack || cached == 0 {
            prose::compress_system_value(system, a)
        } else if let Some((key, prefix_before, reuse, model_name)) = warm_gate {
            let mut candidate = system.clone();
            let n = prose::compress_system_value(&mut candidate, a);
            let latched = super::cold_prefix::system_compression_latched(key);
            let approve = n > 0
                && (latched
                    || matches!(
                        super::cache_policy::system_mutation_decision(
                            prefix_before,
                            system,
                            &candidate,
                            reuse,
                            &super::cache_policy::model_cost_for(&model_name),
                        ),
                        super::cache_policy::MutationDecision::Mutate { .. }
                    ));
            if approve {
                if !latched {
                    super::cold_prefix::latch_system_compression(key);
                }
                *system = candidate;
                n
            } else {
                0
            }
        } else {
            0
        };
        if segments > 0 {
            prose_segments += u64::from(segments);
            modified = true;
        }
    }

    if let Some(messages) = doc.get_mut("messages").and_then(|m| m.as_array_mut()) {
        // Resolve tool-call id → tool name so file/source reads can be protected
        // from lossy compression that would force the model to re-read mid-task.
        let tool_names = tool_kind::anthropic_tool_names(messages);

        // Prune at a frozen, cache-aware boundary by default: Anthropic's
        // prompt cache matches exact prefixes, so the boundary must not move
        // every turn (see `history_prune::prune_boundary`). `mode` resolved above.
        let boundary = super::history_prune::prune_boundary(mode, messages.len());
        // Never rewrite content the client has marked with `cache_control`:
        // pruning inside the already-cached prefix invalidates Anthropic's
        // prompt cache from the first changed message (#448). Pruning therefore
        // starts after the last breakpoint; with no breakpoint this is 0, i.e.
        // the previous behaviour.
        modified |=
            super::history_prune::prune_history_range(messages, protect, boundary, &tool_names);

        for msg in messages.iter_mut() {
            let role = msg.get("role").and_then(|r| r.as_str()).unwrap_or("");
            if role != "user" {
                continue;
            }

            if let Some(content) = msg.get_mut("content").and_then(|c| c.as_array_mut()) {
                for block in content.iter_mut() {
                    if compress_shared::classify_tool_kind(block) != ToolKind::ToolResult {
                        continue;
                    }

                    let name = block
                        .get("tool_use_id")
                        .and_then(|v| v.as_str())
                        .and_then(|id| tool_names.get(id))
                        .map(String::as_str);
                    let kind = compress_shared::tool_result_kind(name);

                    // #481: skip live compression when globally off or when the
                    // originating tool is on the exclusion list (Serena default).
                    let excluded =
                        name.is_some_and(|n| cfg.proxy.is_tool_live_compress_excluded(n));
                    if live_compress
                        && !excluded
                        && let Some(inner_content) = block.get_mut("content")
                    {
                        modified |= compress_content_field(inner_content, name, kind);
                    }
                }
            }
        }

        // Frozen-region user prose: free-text `text` blocks of user turns in
        // `[cached, boundary)`. Cache-safe by construction — the cached prefix
        // and the live tail (`>= boundary`) are both left intact, and the
        // rewrite is content-deterministic so the prefix stays byte-stable.
        if let Some(a) = user_aggr {
            let end = boundary.min(messages.len());
            let start = protect.min(end);
            for msg in &mut messages[start..end] {
                if msg.get("role").and_then(|r| r.as_str()) == Some("user")
                    && let Some(content) = msg.get_mut("content").and_then(|c| c.as_array_mut())
                {
                    prose_segments += u64::from(prose::compress_text_blocks(content, a));
                }
            }
        }
    }

    if prose_segments > 0 {
        modified = true;
    }
    // #940: cache-aligner telemetry. On an unanchored system prompt (the prefix a
    // provider would cache), count the volatile fields that would bust that cache
    // turn-to-turn. Pure measurement — runs before any breakpoint injection and
    // never mutates the body — so it is strictly cache-safe. Skipped once the
    // client has anchored the prefix itself.
    if align_volatile
        && cached == 0
        && let Some(system) = doc.get("system")
        && !prose::value_has_cache_control(system)
        && let Some(text) = super::cache_aligner::system_text(system)
    {
        let scan = super::cache_aligner::scan_volatile(&text);
        cache_safety::record_volatile_system(scan.fields as u64);
    }
    // #974: active cache-aligner relocate (Anthropic-only). After the telemetry
    // above measured the leak on the pristine prompt, move the volatile values out
    // of the cacheable prefix into an uncached tail block so the prefix finally
    // caches. Gated exactly like the breakpoint below — Treatment arm, and only
    // when the client anchored nothing of its own. The rewrite adds the
    // `cache_control` itself, so a following #939 injection sees an anchored prefix
    // and stays a no-op: the two compose to exactly one breakpoint on the stable
    // block, with the volatile tail left uncached.
    if relocate_volatile
        && arm == super::holdout::Arm::Treatment
        && cached == 0
        && doc
            .get("system")
            .is_some_and(|s| !prose::value_has_cache_control(s))
    {
        let relocated = super::cache_aligner::apply_anthropic_relocate(&mut doc);
        if relocated > 0 {
            modified = true;
            cache_safety::record_volatile_relocated(relocated as u64);
        }
    }
    // #939: active prompt-cache breakpoint injection (Anthropic-only). When the
    // client anchored no prefix of its own — no message `cache_control` (`cached
    // == 0`) and no breakpoint already on `system` — add one ephemeral breakpoint
    // to `system` so the large, stable system prompt bills later turns at the
    // cached rate (the win a raw API client leaves on the table). Runs after every
    // frozen-region rewrite so the marker anchors the final system bytes and the
    // prefix it creates stays byte-stable across turns (#498). Counted on its own
    // gauge — a pure win, never against the cache-safe ratio.
    if inject_breakpoint
        && cached == 0
        && doc
            .get("system")
            .is_some_and(|s| !prose::value_has_cache_control(s))
        && super::cache_breakpoint::inject_anthropic_system(&mut doc)
    {
        modified = true;
        cache_safety::record_breakpoint_injected();
    }
    // A deliberate cold-prefix repack (#480) is the one sanctioned exception to
    // the frozen-window rule; count it on its own gauge so it never dilutes the
    // cache-safe ratio (which exists to catch *accidental* #448 regressions).
    // Every other rewrite lands strictly inside the cache-safe frozen window.
    if repack {
        cache_safety::record_cold_repack();
    }
    cache_safety::record(prose_segments, true);

    // Sticky CCR tool injection: once a conversation has used CCR, keep
    // ctx_expand in tools[] to avoid prefix-cache-busting tool-list changes.
    let system_val = doc.get("system");
    let messages_for_id = doc.get("messages").and_then(Value::as_array);
    if let Some(msgs) = messages_for_id {
        let conv_id = prefix_replay::conversation_id(system_val, msgs);
        if sticky_tools::ensure_tool_present(conv_id, &mut doc) {
            modified = true;
            prefix_cache_stats::record_sticky_injection();
        }
    }

    prefix_cache_stats::record_frozen_count(cached as u64);

    // Prefix replay: if this is an append-only turn, overlay the cached
    // forwarded prefix bytes with the fresh delta for byte-identical prefix.
    let system_val_replay = doc.get("system").cloned();
    let msgs_replay = doc.get("messages").and_then(Value::as_array);
    let out = if let Some(msgs) = msgs_replay {
        let conv_id = prefix_replay::conversation_id(system_val_replay.as_ref(), msgs);
        if let Some(delta) = prefix_replay::detect_append_only(conv_id, msgs)
            && delta.delta_start < msgs.len()
        {
            let delta_msgs = &msgs[delta.delta_start..];
            if let Some(replayed) = prefix_replay::overlay_prefix(&delta.prefix_bytes, delta_msgs) {
                prefix_cache_stats::record_replay_hit();
                let original_bytes = serde_json::to_vec(&doc).unwrap_or_default();
                prefix_cache_stats::record_delta(
                    original_bytes.len() as u64,
                    replayed.len() as u64,
                );
                replayed
            } else {
                prefix_cache_stats::record_replay_miss();
                serde_json::to_vec(&doc).unwrap_or_default()
            }
        } else {
            prefix_cache_stats::record_replay_miss();
            serde_json::to_vec(&doc).unwrap_or_default()
        }
    } else {
        serde_json::to_vec(&doc).unwrap_or_default()
    };
    let compressed_size = if modified { out.len() } else { original_size };
    (out, original_size, compressed_size)
}

/// Compresses a tool_result `content` field unless it is a protected file/source
/// read, which must reach the model intact (it is what gets edited).
fn compress_content_field(
    content: &mut Value,
    tool_name: Option<&str>,
    kind: ToolResultKind,
) -> bool {
    match content {
        Value::String(s) => super::tool_output::compress_text(s, tool_name, kind),
        Value::Array(arr) => {
            let mut modified = false;
            for item in arr.iter_mut() {
                if compress_shared::should_compress_content(compress_shared::classify_tool_kind(
                    item,
                )) && let Some(Value::String(text)) = item.get_mut("text")
                {
                    modified |= super::tool_output::compress_text(text, tool_name, kind);
                }
            }
            modified
        }
        _ => false,
    }
}

#[cfg(test)]
#[path = "anthropic_tests.rs"]
mod tests;
