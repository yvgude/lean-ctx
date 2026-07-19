# Release Key Rotation v1

`leanctx.release-key-rotation/v1` is a deterministic offline compatibility
contract. It is canonical JSON, limited to 16 KiB, and contains exactly:

- `old_trust_root` and `new_trust_root`: distinct confined regular files bound
  by `path`, canonical-file `sha256`, and Ed25519 public-key `key_id`.
- `transition`: exactly `activation`, `overlap`, and `revocation`.

Only these state tuples are valid:

| activation | overlap | revocation | Accepted receipt key |
|---|---|---|---|
| `pending` | `inactive` | `not-started` | old |
| `complete` | `active` | `pending` | old or new |
| `complete` | `complete` | `old-key-revoked` | new |

Unknown fields, mixed states, identical roots or keys, digest drift, unsafe or
symlinked paths, non-canonical JSON, oversized input, and a receipt signed by a
key outside the selected state fail closed. After role selection, the existing
canonical Base64, main-subgroup, payload-binding, and Ed25519 checks still run.

Verification returns only content identifiers and the accepted key role. The
plan stores no private material and proves no production ceremony, wall-clock
activation, secret destruction, deployment, or completed operational rotation.
