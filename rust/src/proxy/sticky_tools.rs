//! Sticky CCR tool injection for provider prefix-cache stability.
//!
//! Once a conversation has seen at least one CCR handle, the `ctx_expand` tool
//! definition stays in `tools[]` for the remainder of the session, even on turns
//! with no active markers. This prevents the tool-list change from busting the
//! provider prompt-cache prefix.
//!
//! That injection is the developer-mode path only. Under protection the proxy
//! declares nothing: a declaration it adds is no evidence that the host can
//! execute the call. There a recovery stub is issued only per block, for a
//! tool result whose very call is on record in this proxy's store as answered
//! by an Engine of this store, and it names the expand tool the host listed
//! under the same prefix as the tool that produced the block
//! (`expand_tool_for`). Nothing marks a conversation under protection: the
//! mark is read by the injection only.

use std::cell::RefCell;
use std::collections::{BTreeMap, HashSet};
use std::sync::{Mutex, OnceLock};

use serde_json::{Value, json};

#[cfg(feature = "http-server")]
use super::compress::{bare_tool_name, expand_tool_name};

const MAX_TRACKED: usize = 4096;
const EXPAND_TOOL_NAME: &str = "ctx_expand";

fn active_sessions() -> &'static Mutex<HashSet<u64>> {
    static SESSIONS: OnceLock<Mutex<HashSet<u64>>> = OnceLock::new();
    SESSIONS.get_or_init(|| Mutex::new(HashSet::new()))
}

/// Mark a conversation as having used CCR.
pub fn mark_ccr_active(conv_id: u64) {
    if let Ok(mut guard) = active_sessions().lock() {
        if guard.len() >= MAX_TRACKED
            && !guard.contains(&conv_id)
            && let Some(&oldest) = guard.iter().next()
        {
            guard.remove(&oldest);
        }
        guard.insert(conv_id);
    }
}

/// Returns `true` if this conversation has ever used CCR.
pub fn is_ccr_active(conv_id: u64) -> bool {
    active_sessions().lock().is_ok_and(|g| g.contains(&conv_id))
}

/// What the passes of ONE request know about the recovery stubs they hand
/// to the model: under which name the host of the request listed the
/// Engine's expand tool. Without one no stub is issued.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct HandleUse {
    /// The first expand tool exactly as the host registered it in this
    /// request's own tool list. `None`: the list does not name one, the
    /// request does not let its model call it, or — under protection — the
    /// necessary store conditions do not hold
    /// (`store_binding::host_store_bound`). No block may leave with a stub
    /// then. Unprotected it is the name a pass holds; under protection the
    /// name of a stub is decided per block from `correlation`.
    expand: Option<String>,
    /// Whether the tool list was read for this request. Until then `expand`
    /// says nothing; the provider compressor reads it ([`begin`]).
    list_read: bool,
    /// Under protection, for a request that may issue a stub at all: what
    /// the per-block decision reads ([`expand_tool_for`]). `None` when the
    /// request was read unprotected, or when it issues no stub.
    correlation: Option<Correlation>,
}

/// What a protected request holds for the per-block decision, read once
/// from the request document as the host sent it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Correlation {
    /// Every expand tool the host listed, in list order, each once.
    listed: Vec<String>,
    /// Call id of a tool call in the request's own history → the listed
    /// expand tool that has the same host prefix as the tool that call
    /// named. Only calls of a `ctx_*` Engine tool with such a listed tool
    /// are in it ([`expand_of_producer`]).
    by_call: BTreeMap<String, String>,
}

#[cfg(feature = "http-server")]
impl Correlation {
    fn of(listed: Vec<String>, doc: Option<&Value>) -> Self {
        let by_call = doc
            .map(producing_calls)
            .unwrap_or_default()
            .into_iter()
            .filter_map(|(call_id, producer)| {
                Some((call_id.to_owned(), expand_of_producer(producer?, &listed)?))
            })
            .collect();
        Self { listed, by_call }
    }
}

/// The tool calls in the history of a request document, by call id, with
/// the tool name each one named. Read at the place of every shape (the
/// places do not collide):
///
/// - Messages: `messages[].content[]` blocks `{type: "tool_use", id, name}`;
/// - Chat Completions: `messages[].tool_calls[]` `{id, function: {name}}`;
/// - Responses (HTTP and WebSocket): `input[]` items
///   `{type: "function_call", call_id, name}`.
///
/// Gemini is not read: `google.rs` reads no id of a `functionCall` /
/// `functionResponse` pair, so a Gemini block has no call id here. An id
/// that two calls with different tool names carry names no tool (`None`).
#[cfg(feature = "http-server")]
fn producing_calls(doc: &Value) -> BTreeMap<&str, Option<&str>> {
    fn note<'doc>(
        calls: &mut BTreeMap<&'doc str, Option<&'doc str>>,
        call_id: Option<&'doc Value>,
        name: Option<&'doc Value>,
    ) {
        let (Some(call_id), Some(name)) = (
            call_id.and_then(Value::as_str),
            name.and_then(Value::as_str),
        ) else {
            return;
        };
        calls
            .entry(call_id)
            .and_modify(|known| {
                if *known != Some(name) {
                    *known = None;
                }
            })
            .or_insert(Some(name));
    }
    fn items<'doc>(doc: &'doc Value, key: &str) -> &'doc [Value] {
        doc.get(key)
            .and_then(Value::as_array)
            .map_or(&[][..], Vec::as_slice)
    }
    let mut calls = BTreeMap::new();
    for message in items(doc, "messages") {
        let blocks = message.get("content").and_then(Value::as_array);
        for block in blocks.into_iter().flatten() {
            if block.get("type").and_then(Value::as_str) == Some("tool_use") {
                note(&mut calls, block.get("id"), block.get("name"));
            }
        }
        let tool_calls = message.get("tool_calls").and_then(Value::as_array);
        for call in tool_calls.into_iter().flatten() {
            let name = call
                .get("function")
                .and_then(|function| function.get("name"));
            note(&mut calls, call.get("id"), name);
        }
    }
    for item in items(doc, "input") {
        if item.get("type").and_then(Value::as_str) == Some("function_call") {
            note(&mut calls, item.get("call_id"), item.get("name"));
        }
    }
    calls
}

/// The Engine's wrapper tool: the tool that really ran is in its arguments.
#[cfg(feature = "http-server")]
const WRAPPER_TOOL: &str = "ctx_call";

/// The listed expand tool that belongs to the same MCP server, in the host's
/// own naming, as the tool `producer` — the name a tool call of the request
/// named: `producer` is split into the host's prefix and the bare tool
/// segment (`compress::bare_tool_name`); the bare segment has to be a
/// `ctx_*` Engine tool, and the result is the listed tool that is exactly
/// that prefix followed by `ctx_expand`. `None`: a native or foreign tool
/// (`Bash`, `Read`), or no expand tool listed under that prefix.
///
/// The wrapper form is NOT correlated: a block produced through `ctx_call`
/// (or the bare `ctx` surface) names the wrapper, the tool that ran is in
/// its arguments, and nothing here reads them — such a block gets no stub.
#[cfg(feature = "http-server")]
fn expand_of_producer(producer: &str, listed: &[String]) -> Option<String> {
    let bare = bare_tool_name(producer);
    if !bare.starts_with("ctx_") || bare == WRAPPER_TOOL {
        return None;
    }
    let prefix = producer.strip_suffix(bare)?;
    listed
        .iter()
        .find(|tool| tool.strip_suffix(EXPAND_TOOL_NAME) == Some(prefix))
        .cloned()
}

#[cfg(feature = "http-server")]
impl HandleUse {
    /// What a request knows before its first pass. `doc` is the caller's
    /// document as the host sent it, before any change of the proxy;
    /// `route_can_follow` is false for a route whose body is not one of the
    /// shapes read here (Bedrock: the provider's own format). An expand
    /// tool is known only if that document lists it and lets the model call
    /// it; a missing document, an unknown shape or a list that does not name
    /// the tool all leave it unknown — for every pass of the request. So do,
    /// under protection, the necessary store conditions when they fail
    /// (`store_binding::host_store_bound`): read once here, and only for a
    /// request that lists the tool. Under protection the request's tool
    /// calls are read here as well, once ([`Correlation`]).
    pub(crate) fn for_request(doc: Option<&Value>, route_can_follow: bool) -> Self {
        let listed = doc
            .filter(|doc| route_can_follow && can_carry(doc))
            .and_then(|doc| {
                Shape::ALL
                    .iter()
                    .map(|shape| shape.listed_expand(doc))
                    .find(|listed| !listed.is_empty())
            })
            .unwrap_or_default();
        Self::knowing(listed, doc)
    }

    /// A request whose own list names `listed` (already narrowed to what
    /// the request lets its model call) and whose document is `doc`.
    fn knowing(listed: Vec<String>, doc: Option<&Value>) -> Self {
        let protected = super::compress::protected();
        let Some(first) = listed
            .first()
            .cloned()
            .filter(|_| !protected || super::store_binding::host_store_bound())
        else {
            return Self {
                list_read: true,
                ..Self::default()
            };
        };
        Self {
            expand: Some(first),
            list_read: true,
            correlation: protected.then(|| Correlation::of(listed, doc)),
        }
    }

    /// Whether a stub of this request may leave in `doc`, the document that
    /// is about to be sent: the request knows its expand tool, and `doc`
    /// still lets the model call it and still lists it under that very name
    /// — under protection: still lists exactly the expand tools the request
    /// listed, since a stub may name any of them.
    pub(crate) fn follows(&self, doc: &Value) -> bool {
        let Some(name) = self.expand.as_deref() else {
            return false;
        };
        can_carry(doc)
            && Shape::ALL
                .iter()
                .any(|shape| match self.correlation.as_ref() {
                    Some(correlation) => shape.listed_expand(doc) == correlation.listed,
                    None => shape.registered_expand(doc).as_deref() == Some(name),
                })
    }
}

/// The request shapes whose tool list is read, each in its own format.
#[cfg(feature = "http-server")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Shape {
    /// Anthropic Messages: `tools[] = {name, description, input_schema}`.
    Messages,
    /// OpenAI Chat Completions: `tools[] = {type: "function", function}`.
    Chat,
    /// OpenAI Responses, HTTP and WebSocket: `tools[] = {type: "function",
    /// name, description, parameters}`.
    Responses,
    /// Gemini: `tools[] = {functionDeclarations: [..]}`.
    Gemini,
}

#[cfg(feature = "http-server")]
impl Shape {
    /// Every shape. Their places for a tool name do not collide, so a
    /// document of unknown shape can be read with each of them.
    const ALL: [Self; 4] = [Self::Messages, Self::Chat, Self::Responses, Self::Gemini];

    /// The Engine's expand tool as the host listed it in `doc`: the exact
    /// name of the first entry of the request's own `tools` that
    /// [`expand_tool_name`] accepts, read at the place this shape names a
    /// tool. `None` whenever the list does not show, with certainty, a tool
    /// the model can call by that name in THIS request:
    ///
    /// - no `tools` list, or the name is not in it — including tools the
    ///   provider resolves elsewhere (a stored prompt, a remote MCP server,
    ///   a toolset referenced by id), which carry no name here;
    /// - the entry is deferred (`defer_loading`): the model does not hold
    ///   it until a tool search returns it;
    /// - the name is outside what a stub can carry.
    fn registered_expand(self, doc: &Value) -> Option<String> {
        self.listed_expand(doc).into_iter().next()
    }

    /// Every entry of the request's own `tools` that [`registered_expand`]
    /// would accept, in list order, each name once. A host that runs several
    /// Engines lists one expand tool per MCP server, each under that
    /// server's prefix; which of them a stub names is decided per block
    /// ([`expand_of_producer`]).
    ///
    /// [`registered_expand`]: Self::registered_expand
    fn listed_expand(self, doc: &Value) -> Vec<String> {
        let mut listed: Vec<String> = Vec::new();
        let tools = doc.get("tools").and_then(Value::as_array);
        for tool in tools.into_iter().flatten() {
            if tool.get("defer_loading").and_then(Value::as_bool) == Some(true) {
                continue;
            }
            let names: Vec<String> = match self {
                Self::Messages | Self::Responses => expand_name_of(tool).into_iter().collect(),
                Self::Chat => tool
                    .get("function")
                    .and_then(expand_name_of)
                    .into_iter()
                    .collect(),
                Self::Gemini => GEMINI_DECLARATIONS
                    .iter()
                    .filter_map(|key| tool.get(*key)?.as_array())
                    .flatten()
                    .filter_map(expand_name_of)
                    .collect(),
            };
            for name in names {
                if !listed.contains(&name) {
                    listed.push(name);
                }
            }
        }
        listed
    }
}

/// The `name` of a tool entry, if it is the Engine's expand tool.
#[cfg(feature = "http-server")]
fn expand_name_of(entry: &Value) -> Option<String> {
    entry
        .get("name")
        .and_then(Value::as_str)
        .filter(|name| expand_tool_name(name))
        .map(str::to_owned)
}

/// The two spellings of a Gemini tool's function list.
#[cfg(feature = "http-server")]
const GEMINI_DECLARATIONS: [&str; 2] = ["functionDeclarations", "function_declarations"];

/// Whether the settings of a request let its model call a tool the request
/// lists. Read from the document alone, whatever its shape (the keys of the
/// shapes do not collide):
///
/// - tool use switched off or narrowed to a list for this request
///   (`tool_choice` `none` / `allowed_tools`; Gemini's function-calling mode
///   `NONE` or `allowedFunctionNames`);
/// - the legacy function-calling fields of Chat Completions, which do not
///   mix with `tools`;
/// - Gemini cached content, which carries its own tools and refuses others;
/// - a `tools` value that is no list, or a Gemini list of built-in tools
///   only, which does not mix with function declarations.
#[cfg(feature = "http-server")]
fn can_carry(doc: &Value) -> bool {
    let named = |value: Option<&Value>, name: &str| {
        value
            .and_then(Value::as_str)
            .is_some_and(|given| given.eq_ignore_ascii_case(name))
    };
    let choice = doc.get("tool_choice");
    let choice_type = choice.and_then(|choice| choice.get("type"));
    let calling = doc
        .get("toolConfig")
        .or_else(|| doc.get("tool_config"))
        .and_then(|config| {
            config
                .get("functionCallingConfig")
                .or_else(|| config.get("function_calling_config"))
        });
    let narrowed = calling
        .and_then(|calling| {
            calling
                .get("allowedFunctionNames")
                .or_else(|| calling.get("allowed_function_names"))
        })
        .and_then(Value::as_array)
        .is_some_and(|names| !names.is_empty());
    let forbidden = named(choice, "none")
        || named(choice_type, "none")
        || named(choice_type, "allowed_tools")
        || named(calling.and_then(|calling| calling.get("mode")), "none")
        || narrowed
        || [
            "functions",
            "function_call",
            "cachedContent",
            "cached_content",
        ]
        .iter()
        .any(|key| doc.get(*key).is_some_and(|value| !value.is_null()));
    if forbidden {
        return false;
    }
    match doc.get("tools") {
        None => true,
        Some(Value::Array(tools)) => {
            doc.get("contents").is_none()
                || tools.is_empty()
                || tools.iter().any(|tool| {
                    GEMINI_DECLARATIONS
                        .iter()
                        .any(|key| tool.get(*key).is_some())
                })
        }
        Some(_) => false,
    }
}

thread_local! {
    /// The request pass running on this thread. Every pass is synchronous
    /// from the route down to the place that writes a stub.
    static PASS: RefCell<Option<HandleUse>> = const { RefCell::new(None) };
}

/// Run one synchronous pass of a request that already knows `so_far`, and
/// return what the request knows after it. A request runs several passes
/// with awaits between them (possibly on different threads); the caller
/// carries the value from one pass into the next.
pub(crate) fn within<T>(so_far: HandleUse, pass: impl FnOnce() -> T) -> (T, HandleUse) {
    struct Restore(Option<HandleUse>);
    impl Drop for Restore {
        fn drop(&mut self) {
            let outer = self.0.take();
            PASS.with(|slot| *slot.borrow_mut() = outer);
        }
    }
    let restore = Restore(PASS.with(|slot| slot.replace(Some(so_far))));
    let result = pass();
    let known = PASS
        .with(|slot| slot.borrow_mut().take())
        .unwrap_or_default();
    drop(restore);
    (result, known)
}

/// [`within`] for a provider compressor: joins the pass of the request that
/// called it, or is its own pass when it runs alone.
pub(crate) fn in_pass<T>(pass: impl FnOnce() -> T) -> T {
    if PASS.with(|slot| slot.borrow().is_some()) {
        pass()
    } else {
        within(HandleUse::default(), pass).0
    }
}

/// Change what the pass on this thread knows. Outside a pass there is
/// nothing to change.
#[cfg(feature = "http-server")]
fn update(change: impl FnOnce(&mut HandleUse)) {
    PASS.with(|slot| {
        if let Some(known) = slot.borrow_mut().as_mut() {
            change(known);
        }
    });
}

/// A provider compressor of `shape` starts on `doc`, before it touches any
/// block. A compressor that runs alone reads the request here as
/// [`HandleUse::for_request`] does, at this shape's place: its tool list,
/// and under protection the necessary store conditions and the request's
/// tool calls — once, and only if the list names the tool. Inside a request
/// that read them on the caller's document, what it knows only holds if the
/// compressor's document still lists the same expand tool(s) at this
/// shape's place and still lets the model call them; otherwise no block of
/// the pass leaves with a stub.
#[cfg(feature = "http-server")]
pub(crate) fn begin(shape: Shape, doc: &Value) {
    update(|known| {
        let listed = if can_carry(doc) {
            shape.listed_expand(doc)
        } else {
            Vec::new()
        };
        if !known.list_read {
            *known = HandleUse::knowing(listed, Some(doc));
            return;
        }
        let unchanged = match known.correlation.as_ref() {
            Some(correlation) => correlation.listed == listed,
            None => known.expand.as_ref() == listed.first(),
        };
        if !unchanged {
            known.expand = None;
            known.correlation = None;
        }
    });
}

thread_local! {
    /// The call id of the tool-result block the compressor on this thread
    /// is working on. `None` outside such a block.
    static BLOCK: RefCell<Option<String>> = const { RefCell::new(None) };
}

/// The tool-result block a compressor is working on, until dropped.
#[must_use = "the block ends where this is dropped"]
pub(crate) struct BlockScope {
    outer: Option<String>,
}

impl Drop for BlockScope {
    fn drop(&mut self) {
        let outer = self.outer.take();
        BLOCK.with(|slot| *slot.borrow_mut() = outer);
    }
}

/// A compressor starts on ONE tool-result block whose call id, as the
/// request carries it, is `call_id` (Messages `tool_result.tool_use_id`,
/// Chat `tool_call_id`, Responses `function_call_output.call_id`). Every
/// shortening decided on this thread until the returned value is dropped is
/// a shortening of text of that block. A site that shortens text which is
/// not one tool-result block (a summary, joined text, system or user prose,
/// a cut message) opens no block: it has no call id, and under protection
/// its text stays whole.
pub(crate) fn block(call_id: Option<&str>) -> BlockScope {
    let call_id = call_id.map(str::to_owned);
    BlockScope {
        outer: BLOCK.with(|slot| slot.replace(call_id)),
    }
}

/// The name a recovery stub of the pass on this thread would give the
/// Engine's expand tool, before this proxy's store is asked about the block
/// ([`expand_tool_for`] is the decision):
///
/// - No pass on this thread (a direct call of the compressor or the gate):
///   nothing is known against the bare name `ctx_expand`. No product route
///   is in this case: the proxy's model-facing routes run in a pass
///   (`/v1/compress` runs none, and its stubs name a route, not a tool —
///   this name is never asked for there), and the host-side shaping route of the `ctx_shape` tool
///   (`compress::shape_command_output`, `compress::shape_tool_output`) —
///   no request, no tool list — runs as a pass that knows no tool
///   ([`in_pass`]), so under protection it issues no stub and its text
///   stays whole (`recovery.handle_not_followable`). What remains outside
///   a pass are direct calls in tests.
/// - A pass that knows no tool: `None`.
/// - Unprotected: the first expand tool as the host listed it.
/// - Under protection: the listed expand tool with the host prefix of the
///   tool that produced the current block ([`block`], [`Correlation`]);
///   `None` without a current block, without a producing call of that id in
///   the request, or for a pass that was read unprotected (the mode turned
///   protected inside the request).
fn expand_tool() -> Option<String> {
    PASS.with(|slot| {
        let slot = slot.borrow();
        let Some(known) = slot.as_ref() else {
            return Some(EXPAND_TOOL_NAME.to_owned());
        };
        let first = known.expand.as_ref()?;
        match known.correlation.as_ref() {
            Some(correlation) => BLOCK.with(|block| {
                let call_id = block.borrow();
                correlation.by_call.get(call_id.as_deref()?).cloned()
            }),
            None if super::compress::protected() => None,
            None => Some(first.clone()),
        }
    })
}

/// The followability decision for `original`, the exact text a site is
/// about to replace by a shorter form with a recovery stub: the name the
/// stub has to give the Engine's expand tool, or `None` — the text then
/// stays whole (`recovery.handle_not_followable`) and nothing is stored.
///
/// Unprotected, and outside any pass, this is [`expand_tool`] and nothing
/// else. Under protection, in a pass, a stub is issued only if ALL of this
/// is shown. A stub's id resolves only in an Engine that uses this proxy's
/// data directory and project (`tools/ctx_expand.rs`), and neither a listed
/// tool NAME nor a digest hit shows that: two Engines that read the same
/// file deliver the same bytes. So the proof ties THIS tool call of THIS
/// host to an Engine that writes THIS store:
///
/// 1. Once per request: the host's own list names an expand tool and lets
///    the model call it; a live MCP Engine of this installation is
///    registered for the proxy's project; the listener is loopback
///    (`store_binding::host_store_bound` — necessary, not a proof).
/// 2. `original` is text of ONE tool-result block that carries a call id
///    ([`block`]).
/// 3. This store's delivered register lists that very call id on the entry
///    of exactly `original`'s bytes for the proxy's project
///    (`store_binding::delivered_for_call`). Only an Engine writing this
///    store records that, at the moment it answers a call for which the
///    host named this id.
/// 4. The request's own history holds the tool call with that id, and it
///    named a `ctx_*` Engine tool ([`producing_calls`]).
/// 5. The host listed an expand tool under the same prefix as that tool
///    ([`expand_of_producer`]); the stub names exactly that one.
///
/// Who gets NO compression under protection, plainly:
///
/// - every host that does not send its call id to the Engine with the MCP
///   call (`server/call_identity.rs`; one `_meta` key, taken from host
///   behaviour and not verified by source) — today that is every host but
///   the one that key belongs to, and that one only once it is confirmed;
/// - every block the Engine delivered outside an MCP call: hook-delivered
///   `lean-ctx read` output and other CLI deliveries carry no call id;
/// - every block delivered before call ids were recorded, served from the
///   Engine's response cache or a replay (no delivery record is made for
///   those), or recorded without the tool-delivery entry (text with a
///   caller-presented lineage);
/// - Gemini (no call id is read in that shape) and Bedrock;
/// - a surface that only lists the wrapper (`ctx_call`), and every block
///   produced through it;
/// - a Responses turn that does not carry the producing `function_call`
///   item itself (a turn continued by `previous_response_id`, most
///   WebSocket turns);
/// - a block whose text is a JSON payload shortened string by string, a cut
///   chat message, and every text site without a block.
///
/// The live compressor leaves the results of `ctx_*` tools untouched in
/// every mode (`compress::compress_inner`, #479), and the result of any
/// other tool has no correlation. Under protection the live compressor
/// therefore issues no stub at all; what remains is the pruning of old
/// `ctx_*` results (`history_prune`, `prune_responses_input`).
///
/// What the correlation does NOT prove:
///
/// - The id is chosen by the host or its provider, not by this
///   installation. A replayed id helps a caller only if this store's Engine
///   wrote exactly this digest for it; a caller that has both was served by
///   that Engine, or copies one that was.
/// - A deliberate collision — another local process of the same OS user
///   that sends a known id with the same bytes and lists a different
///   Engine's expand tool under the same prefix — is outside what this
///   closes. It gets a handle its own Engine refuses; nothing is disclosed.
/// - That the host routes the prefixed `ctx_expand` to the same MCP server
///   as the producing `ctx_*` call rests on the host's own naming (same
///   prefix = same server). Nothing in a request shows the routing.
/// - The mode is read when the request starts. A request read unprotected
///   issues no stub once the mode is protected ([`expand_tool`]); the
///   opposite change inside one request is not covered.
pub(crate) fn expand_tool_for(original: &str) -> Option<String> {
    let tool = expand_tool()?;
    let correlated = PASS.with(|slot| {
        slot.borrow()
            .as_ref()
            .is_some_and(|known| known.correlation.is_some())
    });
    if !correlated {
        return Some(tool);
    }
    let call_id = BLOCK.with(|slot| slot.borrow().clone())?;
    super::store_binding::delivered_for_call(original, &call_id).then_some(tool)
}

fn expand_tool_definition() -> Value {
    json!({
        "name": EXPAND_TOOL_NAME,
        "description": "Retrieve the original uncompressed content of a compressed tool result by its tee handle or hash.",
        "input_schema": {
            "type": "object",
            "properties": {
                "id": {
                    "type": "string",
                    "description": "The tee path or hash handle from the compressed output."
                }
            },
            "required": ["id"]
        }
    })
}

/// Ensure `ctx_expand` is present in the `tools` array when CCR is active.
/// Returns `true` if the tool was injected.
pub fn ensure_tool_present(conv_id: u64, doc: &mut Value) -> bool {
    if !is_ccr_active(conv_id) {
        return false;
    }

    let tools = match doc.get_mut("tools") {
        Some(Value::Array(arr)) => arr,
        Some(_) => return false,
        None => {
            doc["tools"] = Value::Array(Vec::new());
            doc["tools"]
                .as_array_mut()
                .expect("tools just initialized as array")
        }
    };

    let already_present = tools.iter().any(|t| {
        t.get("name")
            .and_then(Value::as_str)
            .is_some_and(|n| n == EXPAND_TOOL_NAME)
    });

    if already_present {
        return false;
    }

    tools.push(expand_tool_definition());
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // High conv IDs reserved for these unit tests. Parallel tests in compress.rs
    // and history_prune.rs call mark_ccr_active(0); clearing the global set
    // races with them, so each test marks its own ID instead.
    const NO_CCR: u64 = 0xAA01;
    const INJECT: u64 = 0xAA02;
    const DEDUP: u64 = 0xAA03;
    const STABLE: u64 = 0xAA04;
    const STICKY: u64 = 0xAA05;

    #[test]
    fn tool_not_injected_without_ccr() {
        let mut doc = json!({"tools": [], "messages": []});
        assert!(!ensure_tool_present(NO_CCR, &mut doc));
        {
            let found = &doc["tools"].as_array().unwrap();
            assert!(found.is_empty(), "{found:?}");
        }
    }

    #[test]
    fn tool_injected_after_ccr_activation() {
        mark_ccr_active(INJECT);
        assert!(is_ccr_active(INJECT));
        let mut doc = json!({"tools": [], "messages": []});
        assert!(ensure_tool_present(INJECT, &mut doc));
        assert_eq!(doc["tools"].as_array().unwrap().len(), 1);
        assert_eq!(doc["tools"][0]["name"], "ctx_expand");
    }

    #[test]
    fn tool_not_duplicated() {
        mark_ccr_active(DEDUP);
        let mut doc = json!({"tools": [expand_tool_definition()], "messages": []});
        assert!(!ensure_tool_present(DEDUP, &mut doc));
        assert_eq!(doc["tools"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn tools_array_stays_stable_after_ccr_activation() {
        mark_ccr_active(STABLE);
        let existing = json!({"name": "other_tool", "description": "test"});

        let mut doc1 = json!({"tools": [existing.clone()], "messages": []});
        ensure_tool_present(STABLE, &mut doc1);
        let snap1 = serde_json::to_string(&doc1["tools"]).unwrap();

        let mut doc2 = json!({"tools": [existing], "messages": []});
        ensure_tool_present(STABLE, &mut doc2);
        let snap2 = serde_json::to_string(&doc2["tools"]).unwrap();

        assert_eq!(
            snap1, snap2,
            "tool list must be byte-identical across turns"
        );
    }

    #[test]
    fn sticky_survives_turn_without_markers() {
        mark_ccr_active(STICKY);
        assert!(is_ccr_active(STICKY));
        let mut doc = json!({"tools": [], "messages": []});
        assert!(ensure_tool_present(STICKY, &mut doc));
    }

    #[test]
    fn max_tracked_does_not_panic() {
        for i in 0..(MAX_TRACKED + 100) {
            mark_ccr_active(i as u64);
        }
        assert!(active_sessions().lock().unwrap().len() <= MAX_TRACKED);
    }

    /// Developer mode (the default of an isolated data directory): the tool
    /// list is read as it is, no store binding is asked.
    fn developer_mode() {
        assert!(!crate::proxy::compress::protected(), "developer mode");
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
        assert!(crate::proxy::compress::protected(), "governed mode");
    }

    /// What a request knows is carried from one pass into the next, a
    /// compressor joins the pass of the request that called it and is its
    /// own pass when it runs alone, and nothing of a pass leaks into the
    /// next request on the thread. No pass marks a conversation: the mark
    /// belongs to the developer-mode tee sites alone.
    #[test]
    fn passes_of_a_request_carry_the_expand_tool_and_nest() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
        developer_mode();
        let prefixed = "mcp__lean-ctx__ctx_expand";
        let doc = json!({"tools": [{"name": prefixed}]});
        let known = HandleUse::for_request(Some(&doc), true);

        // First pass, an await, a later pass: the name is still held.
        let (first, known) = within(known, expand_tool);
        assert_eq!(first.as_deref(), Some(prefixed));
        let (later, known) = within(known, expand_tool);
        assert_eq!(later.as_deref(), Some(prefixed));
        assert_eq!(known, HandleUse::for_request(Some(&doc), true));

        // A compressor joins the pass of the request that called it …
        let (joined, known) = within(known, || {
            in_pass(|| {
                begin(Shape::Messages, &doc);
                expand_tool()
            })
        });
        assert_eq!(joined.as_deref(), Some(prefixed));
        assert_eq!(known.expand.as_deref(), Some(prefixed));
        // … and is its own pass when it runs alone: it knows nothing of the
        // request before it, and leaves nothing behind.
        let alone = in_pass(|| {
            begin(Shape::Messages, &json!({"tools": [{"name": "bash"}]}));
            expand_tool()
        });
        assert_eq!(alone, None);
        assert_eq!(expand_tool().as_deref(), Some("ctx_expand"), "no pass");

        // A pass inside a pass gives the outer one back what it knew.
        let ((), outer) = within(known, || {
            let (inner, _) = within(HandleUse::for_request(None, true), expand_tool);
            assert_eq!(inner, None);
            assert_eq!(expand_tool().as_deref(), Some(prefixed), "outer pass again");
        });
        assert_eq!(outer.expand.as_deref(), Some(prefixed));
    }

    /// Under protection a listed expand tool is not held on the list alone,
    /// and the necessary store conditions (a live MCP Engine registered for
    /// the proxy's project, behind a loopback listener) only let the request
    /// know its tools: a NAME is given per block — to the block of a call
    /// that named a `ctx_*` tool of the same prefix — and the decision
    /// additionally needs this store's record of that very call.
    #[test]
    fn under_protection_a_listed_tool_is_named_only_for_a_correlated_block() {
        use std::net::{IpAddr, Ipv4Addr};

        use crate::proxy::store_binding::{bound_host, engine_delivers, listening_on};

        let _data = crate::core::data_dir::isolated_data_dir();
        let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
        let directory = tempfile::tempdir().expect("project");
        let root = directory.path().canonicalize().expect("canonical root");
        governed_project(&root);
        let prefixed = "mcp__lean-ctx__ctx_expand";
        let other = "mcp__other__ctx_expand";
        let use_of = |id: &str, name: &str| json!({"type": "tool_use", "id": id, "name": name});
        let doc = json!({"tools": [{"name": prefixed}, {"name": other}], "messages": [
            {"role": "assistant", "content": [
                use_of("toolu_X", "mcp__lean-ctx__ctx_read"),
                use_of("toolu_O", "mcp__other__ctx_shell"),
                use_of("toolu_B", "Bash"),
                use_of("toolu_W", "mcp__lean-ctx__ctx_call"),
                use_of("toolu_T", "mcp__third__ctx_read"),
                use_of("toolu_D", "mcp__lean-ctx__ctx_read"),
                use_of("toolu_D", "mcp__other__ctx_read"),
            ]},
        ]});
        let alone = || {
            in_pass(|| {
                begin(Shape::Messages, &doc);
                expand_tool()
            })
        };
        // The name a pass of the request gives the block of the call `id`.
        let named = |known: &HandleUse, id: Option<&str>| {
            within(known.clone(), || {
                let _block = block(id);
                expand_tool()
            })
            .0
        };

        // Listed, but nothing shows which store the tool's Engine reads.
        let unbound = HandleUse::for_request(Some(&doc), true);
        assert_eq!(unbound.expand, None);
        assert_eq!(named(&unbound, Some("toolu_X")), None);
        assert_eq!(alone(), None);
        {
            // A loopback listener alone: no Engine on record.
            let _loopback = listening_on(IpAddr::V4(Ipv4Addr::LOCALHOST));
            assert_eq!(HandleUse::for_request(Some(&doc), true).expand, None);
            assert_eq!(alone(), None);
        }
        {
            // A bound standard host: the request knows its expand tools,
            // and names one only for the block of a `ctx_*` call of the
            // same prefix.
            let _bound = bound_host(&root);
            let known = HandleUse::for_request(Some(&doc), true);
            assert_eq!(known.expand.as_deref(), Some(prefixed));
            assert!(known.follows(&doc));
            assert!(
                !known.follows(&json!({"tools": [{"name": prefixed}]})),
                "a stub may name either listed tool: both have to leave listed"
            );
            assert_eq!(alone(), None, "no block, no name");
            assert_eq!(named(&known, None), None);
            assert_eq!(named(&known, Some("toolu_X")).as_deref(), Some(prefixed));
            assert_eq!(named(&known, Some("toolu_O")).as_deref(), Some(other));
            assert_eq!(named(&known, Some("toolu_B")), None, "a native tool");
            assert_eq!(named(&known, Some("toolu_W")), None, "the wrapper");
            assert_eq!(named(&known, Some("toolu_T")), None, "prefix not listed");
            assert_eq!(named(&known, Some("toolu_D")), None, "one id, two tools");
            assert_eq!(named(&known, Some("toolu_unknown")), None);
            // The block scope ends where its guard is dropped, and nests.
            let ((), _) = within(known.clone(), || {
                let outer = block(Some("toolu_X"));
                {
                    let _inner = block(Some("toolu_O"));
                    assert_eq!(expand_tool().as_deref(), Some(other));
                }
                assert_eq!(expand_tool().as_deref(), Some(prefixed));
                drop(outer);
                assert_eq!(expand_tool(), None);
            });

            // The decision asks this store about the block's exact bytes
            // and its call: a name alone decides nothing.
            let text = "pub fn kept() {}\n";
            let decided = |id: &str, text: &str| {
                within(known.clone(), || {
                    let _block = block(Some(id));
                    expand_tool_for(text)
                })
                .0
            };
            assert_eq!(decided("toolu_X", text), None, "nothing on record");
            engine_delivers(&root, "other.rs", text, text, None);
            assert_eq!(decided("toolu_X", text), None, "a digest hit alone");
            engine_delivers(&root, "kept.rs", text, text, Some("toolu_X"));
            assert_eq!(decided("toolu_X", text).as_deref(), Some(prefixed));
            assert_eq!(decided("toolu_X", "other bytes"), None);
            assert_eq!(decided("toolu_O", text), None, "another call");
            assert_eq!(decided("toolu_B", text), None, "a native tool");
            // The compressor's own document no longer lists both tools.
            let (narrowed, _) = within(known.clone(), || {
                begin(Shape::Messages, &json!({"tools": [{"name": prefixed}]}));
                let _block = block(Some("toolu_X"));
                expand_tool_for(text)
            });
            assert_eq!(narrowed, None);
            // A pass that was read unprotected names nothing once the mode
            // is protected.
            let unprotected = HandleUse {
                correlation: None,
                ..known.clone()
            };
            assert_eq!(named(&unprotected, Some("toolu_X")), None);
        }
        // The Engine is still on record; the listener is not loopback.
        {
            let _reachable = listening_on(IpAddr::V4(Ipv4Addr::UNSPECIFIED));
            let known = HandleUse::for_request(Some(&doc), true);
            assert_eq!(known.expand, None);
            assert_eq!(named(&known, Some("toolu_X")), None);
            assert_eq!(alone(), None);
        }
        // Unknown listener again: not bound.
        assert_eq!(HandleUse::for_request(Some(&doc), true).expand, None);
    }

    /// The producing call of a result block is read at each shape's place,
    /// and the expand tool of its server is the listed one with exactly the
    /// same prefix.
    #[test]
    fn the_producing_call_and_its_servers_expand_tool_are_read_per_shape() {
        let messages = json!({"messages": [
            {"role": "user", "content": "go"},
            {"role": "assistant", "content": [
                {"type": "text", "text": "reading"},
                {"type": "tool_use", "id": "toolu_1", "name": "mcp__a__ctx_read"},
            ]},
        ]});
        let chat = json!({"messages": [
            {"role": "assistant", "tool_calls": [
                {"id": "call_1", "type": "function", "function": {"name": "a.ctx_read"}},
                {"id": "call_2", "type": "function", "function": {}},
            ]},
        ]});
        let responses = json!({"input": [
            {"type": "function_call", "call_id": "c1", "name": "ctx_read"},
            {"type": "function_call_output", "call_id": "c1", "output": "text"},
            {"type": "message", "call_id": "c2", "name": "ctx_read"},
        ]});
        let gemini = json!({"contents": [
            {"role": "model", "parts": [{"functionCall": {"id": "g1", "name": "ctx_read"}}]},
        ]});
        assert_eq!(
            producing_calls(&messages),
            BTreeMap::from([("toolu_1", Some("mcp__a__ctx_read"))])
        );
        assert_eq!(
            producing_calls(&chat),
            BTreeMap::from([("call_1", Some("a.ctx_read"))])
        );
        assert_eq!(
            producing_calls(&responses),
            BTreeMap::from([("c1", Some("ctx_read"))])
        );
        assert!(producing_calls(&gemini).is_empty(), "no call id is read");
        assert!(producing_calls(&json!({"messages": "none"})).is_empty());

        let listed = [
            "mcp__a__ctx_expand".to_owned(),
            "a.ctx_expand".to_owned(),
            "ctx_expand".to_owned(),
        ];
        for (producer, expand) in [
            ("mcp__a__ctx_read", Some("mcp__a__ctx_expand")),
            ("mcp__a__ctx_expand", Some("mcp__a__ctx_expand")),
            ("a.ctx_shell", Some("a.ctx_expand")),
            ("ctx_read", Some("ctx_expand")),
            ("mcp__b__ctx_read", None),
            ("b.ctx_read", None),
            ("mcp__a__ctx_call", None),
            ("ctx_call", None),
            ("ctx", None),
            ("Bash", None),
            ("mcp__a__read", None),
            ("mcp__a__", None),
            ("", None),
        ] {
            assert_eq!(
                expand_of_producer(producer, &listed).as_deref(),
                expand,
                "{producer}"
            );
        }
        assert_eq!(expand_of_producer("ctx_read", &listed[..2]), None);

        // Every listed expand tool is read, each once, deferred ones not.
        let doc = json!({"tools": [
            {"name": "bash"},
            {"name": "mcp__a__ctx_expand"},
            {"name": "mcp__b__ctx_expand", "defer_loading": true},
            {"name": "mcp__c__ctx_expand"},
            {"name": "mcp__a__ctx_expand"},
        ]});
        assert_eq!(
            Shape::Messages.listed_expand(&doc),
            ["mcp__a__ctx_expand", "mcp__c__ctx_expand"]
        );
        assert_eq!(
            Shape::Messages.registered_expand(&doc).as_deref(),
            Some("mcp__a__ctx_expand")
        );
    }

    /// The expand tool of a request is what the host's own list names, read
    /// at each shape's place, under exactly the registered name — and
    /// nothing else: a deferred entry, a name a stub cannot carry, a list
    /// without the tool and a request that forbids tool calls name none.
    /// Read in developer mode, where the list alone decides.
    #[test]
    fn the_expand_tool_is_only_what_the_hosts_own_list_names() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
        developer_mode();
        let prefixed = "mcp__lean-ctx__ctx_expand";
        let listed = [
            (Shape::Messages, json!({"tools": [{"name": prefixed}]})),
            (
                Shape::Chat,
                json!({"tools": [{"type": "function", "function": {"name": prefixed}}]}),
            ),
            (
                Shape::Responses,
                json!({"tools": [{"type": "function", "name": prefixed}]}),
            ),
            (
                Shape::Gemini,
                json!({"contents": [], "tools": [{"functionDeclarations": [{"name": prefixed}]}]}),
            ),
            (
                Shape::Gemini,
                json!({"contents": [], "tools": [{"function_declarations": [{"name": prefixed}]}]}),
            ),
        ];
        for (shape, doc) in &listed {
            assert_eq!(shape.registered_expand(doc).as_deref(), Some(prefixed));
            let known = HandleUse::for_request(Some(doc), true);
            assert_eq!(known.expand.as_deref(), Some(prefixed), "{doc}");
            assert!(known.follows(doc));
            assert!(!known.follows(&json!({"tools": []})), "no longer listed");
            assert_eq!(
                HandleUse::for_request(Some(doc), false).expand,
                None,
                "a route whose body is not read"
            );
            // Inside the pass the compressor confirms the name; alone it
            // reads it.
            let ((), confirmed) = within(known, || begin(*shape, doc));
            assert_eq!(confirmed.expand.as_deref(), Some(prefixed));
            assert_eq!(
                in_pass(|| {
                    begin(*shape, doc);
                    expand_tool()
                })
                .as_deref(),
                Some(prefixed)
            );
        }
        // A name is read at its own shape's place only.
        assert_eq!(Shape::Chat.registered_expand(&listed[0].1), None);
        assert_eq!(Shape::Messages.registered_expand(&listed[1].1), None);

        for name in [
            "ctx_expand",
            "lean-ctx:ctx_expand",
            "lean-ctx/ctx_expand",
            "a.ctx_expand",
        ] {
            let doc = json!({"tools": [{"name": "other"}, {"name": name}]});
            assert_eq!(
                Shape::Messages.registered_expand(&doc).as_deref(),
                Some(name)
            );
        }

        let long = format!("{}__ctx_expand", "a".repeat(128));
        for doc in [
            json!({"messages": []}),
            json!({"tools": []}),
            json!({"tools": [{"name": "ctx_call"}, {"name": "ctx"}, {"name": "ctx_expander"}]}),
            json!({"tools": [{"name": "mcp__lean ctx__ctx_expand"}]}),
            json!({"tools": [{"name": "x]\n__ctx_expand"}]}),
            json!({"tools": [{"name": long}]}),
            json!({"tools": [{"name": prefixed, "defer_loading": true}]}),
            json!({"tools": [{"type": "mcp", "server_label": "lean-ctx"}]}),
            json!({"tools": [{"name": prefixed}], "tool_choice": {"type": "none"}}),
            json!({"tools": [{"name": prefixed}], "tool_choice": "none"}),
            json!({"tools": {"name": prefixed}}),
        ] {
            let known = HandleUse::for_request(Some(&doc), true);
            assert_eq!(known.expand, None, "{doc}");
            assert!(!known.follows(&doc));
            let (tool, _) = within(known, || {
                begin(Shape::Messages, &doc);
                expand_tool()
            });
            assert_eq!(tool, None, "{doc}");
        }
        assert_eq!(HandleUse::for_request(None, true).expand, None);

        // The compressor's document no longer lists what the caller's did.
        let ((), narrowed) = within(HandleUse::for_request(Some(&listed[0].1), true), || {
            begin(Shape::Messages, &json!({"tools": [{"name": "ctx_expand"}]}));
        });
        assert_eq!(narrowed.expand, None);

        // Outside any pass nothing is known against the bare name.
        assert_eq!(expand_tool().as_deref(), Some("ctx_expand"));
    }
}
