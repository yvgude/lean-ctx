# Context gateway coverage by carrier

What the context gateway does with each kind of payload ("carrier") it can
meet on its way to a model or a host. The host matrix
([generated/host-coverage.md](generated/host-coverage.md)) says *which*
traffic passes the gateway; this one says what happens to a payload once it
does. A carrier the gateway cannot inspect is never claimed as clean: in
governed mode it is refused before anything connects; in developer mode a
model request leaves marked with its reason code, and a media block of a tool
result is handed to the host unchanged.

| Verdict | Meaning |
|---|---|
| forward | Sent (or handed to the host). Content the gateway can read was inspected and, where a rule says so, masked first. |
| forward-marked | Sent uninspected; the receipt names the reason code, so the payload is never claimed as clean. |
| refuse | Not sent. The caller gets a typed, content-free refusal naming the reason code; the decision is receipted. |

| Carrier | What it is | Decided in | developer | governed | Reason code |
|---|---|---|---|---|---|
| `json-text` | A model request whose text the gateway reads: HTTP JSON body (after gzip/zstd decoding), passthrough text body, WebSocket text frame | `egress::admit_request`, `proxy::admitted::admit_text` | forward | forward | - |
| `media-block` | An image, audio or document block inside a model request | `egress::admit_request` | forward-marked | refuse | `coverage.unsupported_media` |
| `provider-sealed` | Provider-sealed material in a request: thinking signatures, `redacted_thinking`, Gemini `thoughtSignature`, OpenAI `encrypted_content` | `egress::admit_request` | forward-marked | refuse | `egress.opaque_provider_material` |
| `opaque-body` | A body the gateway cannot read: binary, undecodable, an unknown `Content-Encoding`, a WebSocket binary frame | `egress::admit_opaque` via `proxy::admitted::admit_passthrough` and the WebSocket relay | forward-marked | refuse | `egress.opaque_payload` |
| `tool-result-media` | An image, audio or binary resource block in an MCP tool result handed to the host | `carrier::ReleaseGate::media` | forward | refuse | `coverage.unsupported_media` |

Notes:

- `forward` for `json-text` covers clean text. Text with findings follows the
  content rules (`secrets`, `pii`, `classification`, …): masked where a span
  can be masked, refused where it cannot or where the rule says `block`.
- Responses streamed back from the model (SSE events, WebSocket server
  frames) are not egress and are relayed as received.
- Every row is checked against the code by
  `core::context_admission::egress_tests::the_carrier_matrix_is_what_each_carrier_gets`;
  a row that no longer matches the behaviour fails that test.
