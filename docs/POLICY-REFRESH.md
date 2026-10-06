# Current policy at admission and release

The policy runtime reads the current local policy, signed organisation policy
and organisation trust inputs for each security decision. It hashes the actual
bytes and their project/source selection before reusing prepared rules. File
size, modification time and a time-to-live are not authorization shortcuts.
The nearest project policy also applies when the working directory is nested
inside that project; the signed organisation policy remains an enforcement floor.

Each policy/trust input must be a regular UTF-8 file no larger than 1 MiB.
Symlink leaves and Windows reparse-point leaves are rejected. Configured missing,
malformed or unverifiable inputs deny access. A pinned organisation with no signed
policy also denies access; disabling an organisation floor requires a valid,
trusted signed artifact with `enforced = false`. These locations must be managed
by the deployment; this does not protect against an administrator replacing the
installation or trust configuration.

One refresh worker serializes bounded reads and compilation. Its queue is bounded
at 16 requests and callers have a two-second refresh deadline. Saturation, worker
failure or timeout denies admission; the process never spawns a replacement
reader for every timeout. Unchanged bytes reuse one cached compiled snapshot.
Rule count, label length, pattern length and regex compilation size are bounded.
This is a resource limit, not an end-to-end latency promise for every filesystem.

MCP requests carry their own project scope, including an adopted client root.
The request does not change the process working directory. Policy and egress
authority is explicitly carried into synchronous tool handlers on the blocking
pool; Tokio task locals otherwise disappear at that boundary. Tool and egress
permissions are checked again before primitive dispatch after context preparation
has awaited other work. Whole-response cache keys include resolved policy rules.
Immediately before returning a result, current rules are checked again across
text, structured output and metadata, including lifecycle replays. If an already
finalized representation needs alteration, it is withheld rather than changing
its signed or cached bytes. Uninspectable content is withheld in protected mode.
Protected RPC failures return a content-free error.

Legacy cross-agent delivery receipts do not bind policy or subject authority.
Protected requests therefore neither publish nor reuse those receipts; they
materialize the result again for inspection. The policy-bound MCP response cache
remains available. Previously issued unprotected receipts are not treated as
authorization after a policy is installed.

Policy changes do not retrospectively undo an operation that already ran or
revoke an exported copy. Background store/index writers and managed host hook
coverage require their own project authority and storage checks; this mechanism
alone is not proof that every v4 integration or storage path is qualified.

The isolated same-process MCP check is:

```sh
python3 scripts/tests/check_policy_refresh.py --binary /absolute/path/to/lean-ctx --output /tmp/policy-refresh-report.json
```

It exercises client-root adoption, policy addition/change/repair, repeated
search, revoked reads and old idempotent results without restarting the server.
It uses local fixture data and no model provider.
