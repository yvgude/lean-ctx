# Session usage v1

Status: Experimental · Since: 3.12.3 · Schema id: `leanctx.session-usage/v1`

The LeanCTX proxy keeps a token ledger per app session. A launching app
(LeanCTX Code, for example) tags each agent's provider requests with the
`x-leanctx-session-id` header; the proxy books every finalized turn on that
session. An app reads the ledger through this contract and does not need to
work out token usage from agent transcripts.

The ledger is content-free: it holds token and request counts, model names,
session ids and times. It never holds a prompt, a request or response body,
or a file path.

## What the proxy records

For every finalized provider turn, the proxy adds to one bucket:

- the session named by `x-leanctx-session-id` (1-128 characters of
  `A-Z a-z 0-9 . _ : -`), or
- the **unattributed** bucket, when the request has no header or an invalid
  one.

Each bucket keeps, per model: requests, input, output, cache-read,
cache-write and reasoning tokens (as the provider reported them), the wire
saving (the request bytes the proxy's compression removed, divided by 4),
the provider-verified saving pair when the free `count_tokens` probe
answered, and the provider-reported USD charge when the response carried
one.

Storage: `<data dir>/proxy_usage.json` (`sessions`, `unattributed`), written
atomically (temporary file, then rename) after every turn and reloaded when
the proxy starts. Memory is bounded: the proxy keeps the 256 most recently
seen sessions (`max_sessions`); when a new session arrives at the limit,
the least recently seen one is dropped (ties: the smallest id). Model
buckets per session are capped by the proxy's model cap. The unattributed
bucket is never dropped.

## Reading the ledger

Local Read v1 kind `session.usage` ([local-read-v1](local-read-v1.md)):

| Transport | Request |
| --- | --- |
| CLI | `lean-ctx session usage --json [--session ID]` |
| Local service | `GET /v1/local/session/usage[?session=ID]` ([local-service-v1](local-service-v1.md)) |

Both return the same bytes. Without `--session`, every kept session is
listed; with it, `sessions` holds that one session or is empty (the proxy
never saw it, or no longer keeps it). An invalid id is an
`invalid_argument` error document.

```json
{
  "contract": "local-read/1",
  "kind": "session.usage",
  "schema": "leanctx.session-usage/v1",
  "source": "proxy",
  "available": true,
  "max_sessions": 256,
  "sessions": [
    {
      "session_id": "app-7f3a",
      "last_seen": "2026-11-15T09:46:40Z",
      "model": "claude-sonnet-4.5",
      "requests": 3,
      "input_tokens": 3500,
      "output_tokens": 30,
      "cache_read_tokens": 0,
      "cache_write_tokens": 0,
      "reasoning_tokens": 0,
      "saved_tokens": 400,
      "verified_saved_tokens": null,
      "cost_usd": 0.011,
      "measured_usd": 0.0,
      "per_model": [
        {
          "model": "claude-sonnet-4.5",
          "requests": 2,
          "input_tokens": 3000,
          "output_tokens": 20,
          "cache_read_tokens": 0,
          "cache_write_tokens": 0,
          "reasoning_tokens": 0,
          "saved_tokens": 400,
          "cost_usd": 0.0093,
          "pricing_estimated": false
        }
      ]
    }
  ],
  "unattributed": {
    "last_seen": "2026-11-15T09:40:00Z",
    "model": "gpt-5",
    "requests": 4,
    "...": "the same totals and per_model rows, without session_id"
  }
}
```

Fields:

- `available`: `false` until the proxy has written a ledger on this machine;
  `sessions` is then empty and `unattributed` is all zero.
- `sessions`: sorted by `session_id`. Each row carries the bucket totals
  below plus `session_id`.
- `unattributed`: the same totals for turns without a valid session header.
  Sessions plus `unattributed` add up to every turn the proxy metered since
  the ledger started (minus sessions dropped at the limit).
- Totals: `last_seen` (RFC 3339 UTC, seconds; `null` without turns),
  `model` (the model with the most billable tokens, ties: smallest name;
  `null` without turns), the token and request sums, `saved_tokens` (wire
  estimate, bytes / 4), `verified_saved_tokens` (provider-counted
  `count_tokens` saving, counterfactual minus billed; `null` when no turn
  was probed; can be negative), `cost_usd` (rounded to 6 decimals; priced
  from the shared model table at read time, the provider-reported charge
  where there is one), `measured_usd` (the provider-reported part of
  `cost_usd`) and `per_model` (sorted by model name).

The body holds no read time: the same ledger and price table give the same
bytes. `last_seen` is data (the time of the bucket's newest turn).

## Compatibility

Readers ignore fields they do not know. New fields may be added within v1;
renaming or removing one, or changing a meaning, needs v2.

Implementation: `rust/src/core/proxy_usage.rs` (record),
`rust/crates/lean-ctx-proxy/src/usage_meter.rs` (booking),
`rust/crates/lean-ctx/src/cli/local_read/session_usage.rs` (document).
