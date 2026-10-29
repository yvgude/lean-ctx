# Team context outcome report v1

Status: implementation candidate. End-to-end acceptance and deployment are pending.

This API records a host's report about the result of a task for an explicitly selected Team. It does not establish that the model used context, verify the referenced receipt, measure savings, or create billing or learning evidence. The stored provenance is `host_reported`.

## Request

`POST /api/team/{organization_uuid}/context-outcomes`

The organization is an explicit canonical, non-nil UUID. It is not inferred from a model, a license organization label, or a receipt. The client uses its existing enrolled device credential at its provisioned HTTPS origin. It must not substitute an account bearer, follow a response-provided destination, or disable TLS validation.

Maximum JSON request size: 4096 bytes. Unknown fields and explicit `null` optional values are rejected.

| Field | Contract |
|---|---|
| `schema_version` | Integer `1`. |
| `receipt_digest` | `sha256:` followed by exactly 64 lowercase hexadecimal characters. Hash the full canonical signed `ReceiptDocumentV1`, including its derived ID and signature. Do not hash only signing bytes, an SDK egress response, or a legacy receipt type. |
| `outcome` | Exactly `accepted`, `rejected`, or `unknown`, as reported by the host. |
| `observed_at` | UTC timestamp no more than 300 seconds ahead of server time. Preserve the original receipt observation/issuance timestamp on retries. |
| `task_kind` | Optional, omitted when absent; 1–64 ASCII bytes in `[a-z0-9_.-]`. |
| `host_model` | Optional, omitted when absent; 1–128 printable ASCII bytes with no leading or trailing whitespace. It describes the host's observation and does not choose a model. |

Do not include member/account/device identities, costs, token counts, savings, arbitrary metrics, or caller-selected provenance. The service obtains account and device identity from authentication, checks current Team membership and the organization's plan, and rechecks authority before commit.

An active signed device lease is an identity prerequisite. `PersonalSync`, `ModelRouting`, or another unrelated feature is not Team write authority. Team membership and plan remain service-side decisions. Membership is checked across services; no atomic cross-service revocation guarantee is claimed.

## Response and retries

HTTP 200 contains exactly the version, status and matching digest:

```json
{
  "schema_version": 1,
  "status": "recorded",
  "receipt_digest": "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
}
```

The digest above is illustrative. `status` is `recorded` or `replayed`. Clients validate the version, status and exact digest echo and bound response bytes. They recheck local device authority after I/O before reporting success.

The idempotency key is the organization plus receipt digest. Repeating the same report from the same account returns `replayed`; conflicting content or another account receives HTTP 409. A conflict is not a successful replay. Keep the request fields stable when retrying. Local receipt publication remains valid independently of whether the remote report succeeds.

## Separate from egress transport outcomes

The SDK's egress lifecycle reports `delivered`, `withheld`, or `failed` about transport. Those values are not valid task outcomes here. Delivery must never be converted automatically into `accepted`. A host that has no task-result evidence reports `unknown` or does not report.

## Acceptance gates

- Real client → Edge → Cloud request with an actual canonical signed receipt digest.
- Recorded/replayed/conflicting reports, organization isolation, device and membership revocation.
- Strict request/response size limits, TLS trust and hostname failures, no redirect/proxy credential leakage.
- Retention and organization deletion wired into the deployed lifecycle.
- Entitlement catalog and version migration checked before deployment.
