# Egress lifecycle receipts v1

Status: experimental implementation candidate; verification and SDK rollout pending.

This companion contract adds egress preparation and per-attempt transport evidence. It does not alter the serialized shape or meaning of ContextDecisionReceiptV1.

## Admission receipt

EgressAdmissionReceiptV1 records whether a request was prepared for dispatch:

- Prepared means policy admitted the exact outbound bytes and prepared_bytes_digest binds those bytes. It does not mean they were sent, received, or used by a model.
- Withheld means policy denied dispatch; no prepared bytes digest is present.
- Failed means admission could not produce a dispatchable request; no prepared bytes digest is present.

The record may link the associated ContextDecisionReceiptV1 by its canonical digest. Prepared and Withheld records require that link; Failed records may include it if a decision receipt was already created.

## Transport observation

EgressTransportObservationV1 is a separate, append-only record for one attempt, keyed by its own canonical digest and linked to the admission receipt digest. Retries produce one observation per attempt; they do not rewrite the admission receipt.

ResponseObserved means a response was observed. An HTTP status, including a 4xx or 5xx, records only the response status; it does not prove model consumption. NoResponseObserved means no response was observed, not that the request was definitely unsent. Failure values are bounded classes, not raw error strings.

For a `web_socket` message there is no response to observe. There,
ResponseObserved (without a status) means only that the Engine's socket
accepted the message for sending — not that the peer received or read it — and
NoResponseObserved with `other` means the socket refused it.

EngineObserved identifies evidence captured by the Engine transport. HostReported identifies an observation supplied by an external caller. HostReported is a caller assertion and this contract does not independently attest it.

An observation is valid only for the matching Prepared admission receipt. A Withheld or Failed admission has no transport observation.

## Engine operations and compatibility

`engine egress-admit` retains its strict V1 response shape. Existing SDKs reject
unknown fields, so lifecycle fields are not appended to that response.

`engine egress-prepare` is an explicit opt-in operation. It accepts the same
admission request and returns `EngineEgressPrepareResponseV2`: outer
`schema_version: 2`, the original V1 `decision`, and for a dispatchable request
`prepared_body`. Hosts must send the UTF-8 bytes of `prepared_body` verbatim.
Reserializing `decision.body` can change whitespace or key order and would break
the exact-byte evidence. Validation checks structural equality with the admitted
JSON and equality with the decision receipt's final byte digest when present.
A refused result has no prepared body. `admission_receipt_digest` is present only
when durable admission evidence exists; its absence must not be invented away.

`engine egress-report` accepts a bounded V1 report naming that admission digest,
attempt, transport, result, status or failure class. It sets `HostReported`
itself. Clients cannot supply `EngineObserved`. Reports are untrusted claims by
the local host, not independent transport, billing or model-use attestations.

## Storage and inspection

Admission and observation objects are content addressed. Decision-to-admission
and admission-to-attempt links are immutable: an identical retry is idempotent;
a conflicting observation for the same attempt is rejected. Cross-process
locking protects link creation. Lookup examines at most 32 attempt slots,
and lifecycle records/pointers have a 16 KiB read cap.

The HTTP transport records response headers or bounded failure classes for each
actual attempt. The Responses WebSocket adapter currently sends its upstream
request through HTTP, so that upstream observation is labelled HTTP. The
separate count-tokens request has its own admission and observation. Failure to
persist after a send cannot undo the send; it prevents the HTTP retry and leaves
missing evidence explicit. Cancellation or a crash before recording may leave
preparation with no observation; this remains unknown.

The proxy's reply states such a gap: when an attempt was sent but its
observation could not be persisted, the reply carries
`x-leanctx-egress-evidence: receipt.observation_unavailable`. The ChatGPT
backend WebSocket tunnel admits every client frame on its own admission and
records one `web_socket` observation per frame as attempt 1, with the meaning
given above. Where the receipt is owed and that observation cannot be
persisted, the tunnel ends with `receipt.observation_unavailable`; the frame
may already have left.

## Request target and head

Before the body, the proxy admits the caller-chosen path and query
(percent-decoded) and the values of the caller headers it relays, as text. A
finding refuses the request (`egress.target_withheld` or the detector's code);
a URL is never masked. Not admitted as content: `authorization` in the shape
of a credential (`Bearer`, `Basic` or AWS SigV4 and a token — free text in it
is admitted), the other credential headers of the destination the request goes
to only (ChatGPT: `cookie`; Bedrock: `x-amz-date`, `x-amz-content-sha256`,
`x-amz-security-token`; Google: `x-goog-api-key`; Azure: `api-key`; Anthropic:
`x-api-key`; other destinations: `x-api-key`, `api-key`, `x-oauth-token`,
`x-xai-token-auth`), and a destination's
own credential query parameter where the configured upstream is that
destination, it occurs once and has that credential's shape (Google `key`
`AIza…` on `*.googleapis.com`, Azure `api-key` on Azure OpenAI hosts). A
standalone millisecond timestamp (13 digits, 2001–2049, not touching a letter,
digit, `-`, `_` or `.`) is not read as a card number; every other digit run is
inspected as it is. On the forward routes the path and query are those of the
request the proxy received — a superset of what is sent where a route
replaces the path (cross-shape translation).

The method, the final URL and the relayed header set are bound with the body:
the decision receipt's trace sidecar names them by a keyed reference
(`request_head_ref`), and a finished request whose method, URL or header set
differs from the bound one is refused (`egress.head_unbound`). The binding
covers the headers the proxy relays. It does not cover what the HTTP client or
WebSocket library adds when the request is executed: `host`,
`content-length`/`transfer-encoding`, `accept-encoding`, client default headers
and the WebSocket handshake headers (`connection`, `upgrade`,
`sec-websocket-key`, `sec-websocket-version`). For the WebSocket tunnel the
bound head is evidence only: the handshake is built from the same header set,
but not checked again after it is built.

Not closed: the ChatGPT backend passthrough relays every caller header except
hop-by-hop headers and the proxy's own `x-leanctx-*` namespace (a deny-list).
Which headers chatgpt.com requires is not established, so it is not an
allow-list; the relayed values are admitted and bound as above.

Task lineage and human inspection show outbound legacy `Delivered` receipts as
preparation without rewriting stored V1 receipts. Missing admission links and
missing observations remain visible gaps. Host-reported responses retain their
provenance and do not establish task acceptance. Existing count fields describe
the recorded boundary; they do not establish model consumption. The experimental
lineage projection adds delivery semantics and optional egress evidence; the
stored receipt and frozen egress-admit wire contract stay unchanged.

Learning evidence excludes outbound preparation from host-delivery token and
security aggregates. Mixed or incomplete evidence stays unknown, rather than
being reported as a complete measured saving.
