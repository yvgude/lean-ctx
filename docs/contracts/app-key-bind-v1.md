# App-key bind v1 (f3129 S2+S3)

Status: **draft contract**, target 3.12.9 (engine) and the matching Cloud
release. Types: `lean-ctx-protocol` `cloud::app_keys` and `cloud::enroll`.
Fixtures (generated, every signature real):
`rust/crates/lean-ctx-protocol/tests/fixtures/cloud/`

| File | What |
|---|---|
| `device_code_app_keys.request.json` | S2 `POST /v1/device/code` with `app_keys` |
| `device_code_app_keys.response.json` | the code answer listing `device_app_keys` |
| `device_token_app_keys.request.json` | S3 `POST /v1/device/token`: three offers and `app_keys_bind_signature` |
| `app_key_bind.vectors.json` | test-only seeds, statement, exact signing input, labels, user text, and `negative`: every 400 with the request that draws it |

The seeds in `app_key_bind.vectors.json` (`keys.*_seed_hex`) are
**test-only**. They are valid nowhere outside fixtures and fake services.
The generator is `tests/cloud_fixtures.rs`:
`CLOUD_FIXTURES_WRITE=1 cargo test -p lean-ctx-protocol fixtures_are_current`.
`app_key_fixtures_verify_from_disk` reads the committed files and checks
every signature against the documented keys. It also runs the Cloud's
check order (`reference_token_check`) over each negative case.

App keys are the app's no-prompt signing keys. Today there are three kinds:

| `kind` | Offer member | Own bind domain / scope | Page label |
|---|---|---|---|
| `chat` | `chat_key` | `leanctx.cloud.chat_key_bind.v1\n` / `person_publish.v1` | Chat messages |
| `optin` | `optin_key` | `leanctx.cloud.optin_key_bind.v1\n` / `optin.v1` | Task pickup |
| `task_propose` | `task_propose_key` | `leanctx.cloud.task_propose_key_bind.v1\n` / `task_propose_person.v1` | Task proposals |

## Encoding

- App public keys and every signature in this contract are **base64url
  without padding**. Padding, the standard alphabet and non-canonical
  trailing bits are refused.
- `key_id` = `hex(sha256(raw 32-byte public key))[..16]`, lowercase. The
  Cloud recomputes it and never trusts the sent value.
- `instance_public_key` enters every statement exactly as the token request
  carries it (standard padded base64, as everywhere else in Cloud).

## 1. Capability

- `DeviceCodeV1.features` lists `device_app_keys` on a Cloud that implements
  this contract (`FEATURE_DEVICE_APP_KEYS`).
- The engine announces `app_keys` with the device-code request, before the
  code answer can show the capability. An older Cloud refuses the unknown
  member (`400 invalid_request`) and the pairing ends; it is never sent again
  without the announcement.
- The engine sends the offers and `app_keys_bind_signature` with the token
  request only when the code answer lists `device_app_keys`; otherwise it
  stops before polling.
- Without the capability, nothing is bound: no app key, by any path. This
  fails closed. No older path binds an app key without S3.

## 2. S2: `POST /v1/device/code`

The request takes a new optional member, `app_keys`. It is absent when empty,
so older requests stay byte-identical:

```json
"app_keys": [{"kind": "chat", "alg": "ed25519", "key_id": "…16 hex…", "public_key": "…base64url…"}, …]
```

- `kind` must be one of `chat`, `optin` or `task_propose`.
- `alg` must be `ed25519`.
- At most one entry per kind, at most 3 entries, sorted by `kind` (byte order
  of the wire name).
- `public_key` is base64url without padding of the raw 32 bytes.
- The Cloud recomputes `key_id`.
- Any failure → **400** `{"error": {"code": "invalid_field", "field":
  "app_keys", …}}`, and no code is issued. Failures include an unknown kind, a
  duplicate kind, wrong order, too many entries, a wrong alg, bad encoding
  (padding, the standard alphabet, non-canonical trailing bits, wrong
  length), a `key_id` mismatch or an unknown member.
- Decode errors inside `app_keys` (an unknown `kind`, an unknown member) are
  this same answer, never a generic schema error (`invalid_request`). The
  vectors carry `unknown_kind` and `unknown_member`.

The Cloud stores the validated list with the pending code (the *announced
set*).

## 3. `/device` page (documentation only)

- Before **Allow**, the page names each announced key by its label with a
  short fingerprint (`key_id`), for example "Chat messages · `1a2b3c4d5e6f7a8b`".
- LeanCTX Code shows the same fingerprint while pairing, so the person can
  compare the two.
- With no app keys announced, the page says nothing about keys.

## 4. S3: `POST /v1/device/token`

- **Offers.** The token request carries `chat_key`, `optin_key` and
  `task_propose_key`. Each is `{alg, public_key, key_id, bind_signature}`
  (`AppKeyOfferV1`) and is present only for an announced kind.
  - `bind_signature` is the app key's own signature over its kind's bind
    domain ‖ canonical_json({alg, instance_public_key, key_id, public_key,
    scope}), unchanged from !622/!627/!628.
  - An offer that does not decode (an unknown member) or is malformed →
    **400** `invalid_field`, `field` = its member (`chat_key`, …), no
    `reason` (vector `offer_unknown_member`).
  - The set of offers must **equal** the announced set: the same kinds, and
    identical `alg`, `key_id` and `public_key`. Otherwise → **400**
    `invalid_field`, `reason: "app_key_not_announced"`, `field` = the first
    differing member (in kind order). A kind offered twice is refused the
    same way (`offered_equals_announced`).
  - Offers without any announcement → `app_key_not_announced` (vector
    `offers_without_announcement`). An old path that checked only
    `bind_signature` would bind them, because the per-key domains are
    unchanged since !622/!627/!628.
- **`app_keys_bind_signature`.** It is required whenever keys were announced,
  and absent otherwise. It is the Ed25519 signature of the instance key (the
  `instance_public_key` of the same request), base64url without padding, over:

  ```text
  "leanctx.cloud.app_key_bind.v1\n" ‖ canonical_json({
    "device_code_sha256": hex(sha256(device_code)),
    "instance_public_key": <as sent>,
    "keys": [{"alg", "kind", "key_id", "public_key"}, …]   // the announced set, sorted by kind
  })
  ```

  - The engine builds it with `app_key_bind_signing_input`.
    `app_key_bind.vectors.json` holds the exact bytes (`signing_input`) and
    the signature.
  - A missing signature, one that does not verify, or one present although
    nothing was announced → **400** `invalid_field`,
    `field: "app_keys_bind_signature"`, `reason: "app_key_bind_invalid"`.
    Nothing is bound. A signature without an announcement is vector
    `signature_without_announcement`.
- **Strict Ed25519.** Every signature in this contract (S3 and each
  `bind_signature`) is verified strictly, as `verify_strict` does: exactly 64
  canonical bytes (`s` reduced), and no small-order public key or `R`. A
  signature that verifies only under the lenient rule does not verify.
  Vector `offer_small_order_key` is such a signature: the `chat` key is the
  identity point (small order) and its `bind_signature` is `R` = base point,
  `s` = 1, which a plain `verify` accepts for any message. Only
  `verify_strict` refuses it → `chat_key.bind_signature`. The key passes S2,
  which checks encoding only, so the refusal falls to the signature check.
- **`canonical_json` is RFC 8785 (JCS)** for this domain. It is the same
  `signing_input` as every other Cloud domain: keys sorted by byte at every
  depth, no whitespace, integers only, `serde_json` escaping.
  - In general that form differs from JCS only for keys outside the BMP and
    for non-integer numbers. For these statements it is byte-identical to
    JCS: every key and value is printable ASCII without `"` or `\` (hex,
    base64/base64url, kind names), so JCS's UTF-16 key order equals the byte
    order used here, and neither form escapes anything.
  - `app_key_fixtures_verify_from_disk` checks the precondition (printable
    ASCII without `"` or `\` throughout the statement). It does not compare
    against an independent JCS implementation.

## 5. One-time nonce

- `device_code` is the one-time server nonce that S3 signs (as its SHA-256,
  so the secret is never in a statement).
- The successful token exchange consumes it atomically, in **one
  transaction** with creating the instance, enrolling it and binding the keys.
- A second poll with the same code gets **400** (`expired_token`) and binds
  nothing (vector `device_code_used`).

## 6. Cloud check order

0. Code status, before anything else: an unknown, expired or already used
   `device_code` → `expired_token` (§5). Vectors `device_code_unknown`,
   `device_code_expired` and `device_code_used`.
1. Schema of the token request. Unknown members are refused, and each offer
   must be well formed.
2. `app_keys_bind_signature` against the announced set →
   `app_key_bind_invalid`.
3. Each offer's own `bind_signature` → `invalid_field`, `field` = the
   signature's request path (`chat_key.bind_signature`,
   `optin_key.bind_signature`, `task_propose_key.bind_signature`), no
   `reason`. The offers are named members, not an array, so there is no
   `app_keys[i]` index in S3.
4. Announced-set equality → `app_key_not_announced`.
5. Bind, inside the enrollment transaction of §5.

## 7. No bind outside the device flow

- v1 has no other route that binds an app key, for any of the three kinds.
  There is no `PUT …-key` route.
- p37 refuses `PUT /v1/…/chat-key`, `…/optin-key` and `…/task-propose-key`
  with **404** or **405** and has a test for it. An offer carries no nonce,
  so a captured offer could be replayed on such a route.
- To change a key, remove it (DELETE / the S1 buttons) and pair again. This
  also covers a lost Keychain.

## 8. Re-pair with `replaces`

- `replaces` never moves keys. The replaced instance's app keys are
  removed together with it.
- The new instance gets only the set it announced in S2 and proved in S3.
  The same computer has the same app keys and announces them again, so
  nothing needs to move.
- Moving keys would bind them to an instance key that never proved them,
  while their own `bind_signature` names the old `instance_public_key`
  (contrary to §7). For a stolen laptop replaced via `replaces`, its keys
  would keep signing person posts.

## 9. Audit

- Each bound event carries `proof: "instance_key"` and the bound `key_id`s.
- The bind e-mail stays as it is.

## 10. Residual risk

- Anyone who can read the engine's instance private key can sign S3.
- That risk is covered by the existing protection of the instance key, not by
  this contract.
- **Device-code phishing** now yields person-level keys. An attacker starts
  a pairing from their own instance with their own app keys and gets the
  victim to approve the `user_code`; the victim's account then accepts
  chat posts, task pickup and task proposals signed by the attacker's keys.
  Mitigation: the `/device` page names every announced key with its label
  and fingerprint before **Allow** (§3), and LeanCTX Code shows the same
  fingerprint. The page must make this prominent (p37 follow-up).

## 11. Engine error text

- For both new reasons, the engine and LeanCTX Code show: "Pairing failed,
  please start connecting again." (`PAIRING_FAILED_TEXT`,
  `app_key_refusal_text`).
- Other refusals keep their own text.

## Wire notes

- `invalid_field` is `CloudErrorCode::InvalidField` (400). The variant is
  additive: any code the engine does not know still decodes as `Unknown`.
  `lean-ctx-cloud` keeps it as `Server { status: 400, code: "invalid_field" }`;
  `field` and `reason` stay in the body. On every route that answers
  `invalid_field`, the engine's `event_code` is now `invalid_field` instead
  of `unknown` (changelog fragment `changelog.d/f3129-invalid-field.md`).
- `CloudErrorCode` is not `#[non_exhaustive]`. For Rust code outside this
  repository that matches it exhaustively, the new variant needs one more
  arm. Inside this repository the only exhaustive match is
  `CloudErrorCode::http_status`, and `lean-ctx-cloud` matches with a
  catch-all arm.
- `field` values, exactly as the request names them:

  | Refusal | `field` | `reason` |
  |---|---|---|
  | S2 schema (any `app_keys` failure, decode errors included) | `app_keys` | — |
  | S3 malformed or undecodable offer | `chat_key` / `optin_key` / `task_propose_key` | — |
  | S3 instance signature | `app_keys_bind_signature` | `app_key_bind_invalid` |
  | S3 per-key signature | `chat_key.bind_signature` / `optin_key.bind_signature` / `task_propose_key.bind_signature` | — |
  | S3 set mismatch | `chat_key` / `optin_key` / `task_propose_key` (first differing kind) | `app_key_not_announced` |
  | Second poll | — (`code: "expired_token"`) | — |
- The engine's typed `DeviceTokenRequestV1` refuses offers without
  `app_keys_bind_signature`, and the reverse. The Cloud still answers such a
  request per §6, because the requirement depends on the announced set.
