# Gateway: nobody is locked out

Status: implemented on `engine/gateway-lockout` (C line, from `eed5bff8c3`).

## What happened (26.10., also 25.10.)

A Claude Code subagent routed through the local proxy stopped with
`403 request withheld by the context gateway (classification.flagged,
destination.remote_restricted)` and "Please run /login". Every resume failed
the same way, because the flagged block stays in the conversation history.

Evidence (metadata only; the refusal receipts themselves were not persisted —
the evidence store was full, `1024 of 1024 MB`):

- proxy log: refusals at `2026-10-26T11:50:11Z` and `11:50:18Z` (resume),
  `12:18:15Z`, `12:19:23Z`, and `2026-10-25T14:00:16Z`, `17:24:26Z`; all
  `classification.flagged, destination.remote_restricted` — no `secret.*`
  code, so no key material or detector of secrets was involved.
- transcript scan of the four affected subagent sessions (detector regexes
  re-run, only position, line shape and a short hash printed): in every case
  a `ctx_shell` tool result containing a directory listing with an entry that
  is the lowercase word for the top marking level — the Hive dev stack keeps
  its local credentials in such a directory (`.dev/<name>`,
  `target/dev-design/<name>`). The banner rule of `input_filters/classification.rs` matches
  case-insensitively and needs no decoration, so the line became a marking of
  level `restricted`.
- `egress_walk.rs`: a remote destination plus `restricted` called
  `Walk::refuse`, i.e. the whole request was refused, not the block.
- `proxy/admitted.rs`: a withheld request answered `403 permission_error`,
  which Claude Code reports as `authentication_failed` / "run /login".

## Behaviour after the change

1. **Block, not request.** Content that may not go to a remote model is
   replaced in place, like every other withheld block:
   `[lean-ctx gateway: content withheld — <codes> · lean-ctx gateway why <id>]`.
   The request is refused only where nothing can be replaced (provider-signed
   or encrypted text, tool names/keys, project policy blocks, every decided
   block withheld). A block withheld in place no longer classifies the
   forwarded request, so the proxy's joined check (G02: restricted never to a
   non-local destination, also after a transformation) does not refuse it
   whole again; transformations run on the already-withheld body. A refused
   request still states the classification of everything it carried.
2. **Stable ids.** `<id>` is `w-` plus the first 12 hex digits of the keyed
   object digest (HMAC-SHA256 under the installation's receipt key — the
   receipt v2 object digest). Same bytes, same id, same placeholder on every
   re-send; the remote side cannot confirm a guess of the content from it.
3. **Local explanation.** Each withheld block is recorded in
   `<data>/gateway/withheld/<id>.json`: reason codes, position (system,
   message N, tool name), destination provider and locality, mode. No
   content, no timestamps; bounded (oldest files pruned past 1024), and
   independent of the receipt store ceiling.
   `lean-ctx gateway why <id>` and `lean-ctx gateway list` read it.
4. **Actionable refusal.** A refused request is `400` (`invalid_request_error`
   for Anthropic/OpenAI shapes, `FAILED_PRECONDITION` for Gemini), never
   `403`/`permission_error`, so hosts do not ask for a new login. The message
   keeps the parsable prefix `request withheld by the context gateway (codes)`
   and adds the withheld block ids and the commands to inspect or allow them;
   `lean_ctx.blocks` and `x-leanctx-withheld-blocks` carry the ids.
5. **Human override.** `lean-ctx gateway allow-once <id> [--hours N]` lets
   exactly that block (exact bytes) go to the provider it was withheld from,
   for N hours (default 24), so the re-sends of a conversation stay
   consistent. It requires a terminal on stdin, no agent CLI among the
   process's ancestors (the Hive consent channel check), and the id typed on
   `/dev/tty`. It is recorded in the audit trail (`x-gateway_override`) and
   in the receipt of every request it applies to
   (`destination.remote_allowed_once`). It waives only
   `destination.remote_restricted`, only in developer mode (governed and
   sovereign ignore grants), and only for the same provider.
   `lean-ctx gateway revoke <id>` withdraws it (the block is withheld again).
   Residual, as for consent: an agent with a native shell that spoofs a TTY.
6. **Fewer false positives.** A banner or stamp marking must not be written
   all in lowercase (`SECRET`, `Secret`, `*** TOP SECRET ***` still mark; a
   bare lowercase word in a listing does not). The explicit
   `Classification:` field form and every secret/PII detector are unchanged.

## Remaining refusal paths (follow-ups)

- Provider-signed text (a thinking block) and tool names/keys cannot be
  replaced; a restricted value there still refuses the request — now with a
  400 naming the block, which `allow-once` resolves. Dropping an old thinking
  block instead is possible on the Anthropic wire but not done here.
- Media: developer mode forwards images (marked `coverage.unsupported_media`),
  so none of the observed lockouts involved them. Governed/sovereign mode
  still refuses a request with an image; replacing the image block by a text
  notice is the analogous fix and is left as a follow-up.
- A full evidence store (`1024 of 1024 MB` on 26.10.) answers every request
  with 503 where receipts are mandatory (governed/sovereign) — a lockout of
  every session at once. Developer mode only warns.

## Determinism (#498)

Placeholders and refusal messages are functions of (content, policy, key):
no timestamps or counters. Grant expiry is evaluated at admission time and
changes the decision, not the bytes of a placeholder.
