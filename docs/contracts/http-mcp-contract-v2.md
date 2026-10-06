# HTTP-MCP Contract v2

This version incorporates [the frozen v1 contract](http-mcp-contract-v1.md)
except for the event-delivery changes below. Endpoint URLs remain `/v1/...`;
the documentation contract version is distinct from the URL namespace.
The runtime advertises `leanctx.contract.http_mcp.contract_version=2`.

## Directed event filtering

```
GET /v1/events?workspaceId=<ws>&channelId=<ch>&since=<cursor>&limit=<n>&agentId=<agent>
```

The new optional string query parameter `agentId` selects directed events for
that recipient. Omission or an empty value returns broadcast events only.
The existing workspace/channel/cursor/limit parameters retain their v1 meaning.
Filtering applies to persisted replay and live delivery, not only one phase.

The event envelope adds `targetAgents`, an array of recipient strings or
`null`; `null` means broadcast. Directed events are returned only when the
requested agent identity matches a target. Callers should use their assigned
agent-bus identity.

An identity supplied as a query parameter is not, by itself, authenticated
recipient authorization. This contract does not claim an authenticated Team
privacy boundary or replace the hosting transport's authentication policy.

## Compatibility and migration

Clients that omit `agentId` continue receiving broadcasts, but no longer receive
directed context. This is a semantic restriction, not a typo in frozen v1.
Clients needing directed context must supply their recipient identity and retain
the normal server access controls. Do not restore broadcast of directed events
to satisfy an older client or a documentation fingerprint.

The frozen v1 file is retained byte-for-byte at its existing SHA-256
`7550166fcefddea0752129e99aaf03a7821589e998a637905388f0392129d240`
(source commit `c417b4c6be161a1bf37512980da5e79cdbedc65f`). The change originally
introduced in `fbc5d6f91d` is described here rather than rewriting that artifact.
This version does not assert V4 product, deployment or release acceptance.
