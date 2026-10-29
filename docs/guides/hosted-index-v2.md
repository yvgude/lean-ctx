# Hosted Index V2

Implementation-candidate guide. Publication, platform qualification and service
availability are separate release gates. The commands below describe the local
Gateway candidate, not an already released service.

## What is stored

Index V2 stores an encrypted, project-bound BM25 search generation. The Engine
builds it from freshly admitted original sources and records the source,
classification, role, corpus-rule and Gateway-policy commitments. Existing V1
files cannot be relabelled as a verified V2 generation.

The restored generation is used by local lexical retrieval only after the
current project, sources and permissions have been checked again. Filtered
searches use a fresh admitted corpus. V2 does not currently back up the source
repository, vector embeddings, graphs, or every LeanCTX store. A restore needs
the matching local original sources; it cannot reconstruct a lost repository.

The server stores opaque ciphertext and untrusted structural header claims.
The Runtime authenticates the envelope locally using a random, persistent key
in the operating-system credential store. Logging in again does not derive a
replacement key. Keep an authorized device and its key available before
removing an installation.

## Prerequisites and destination

- An explicitly activated, verified Runtime package advertising
  `pro.runtime.hosted_index@2.0.0`, with its proprietary license accepted.
- A configured commercial lease with the rights required for new keys,
  encryption and pairing; a current server entitlement for uploads.
- An authenticated account whose identity matches the selected destination.
- An explicit global destination. A project file cannot select another upload
  origin or account.

Run the project commands from the selected project's root. Configure the exact
bare HTTPS origin and canonical account UUID using:

```text
lean-ctx cloud index destination set --origin URL --account ID --max-classification public|internal|confidential
lean-ctx cloud index destination show
```

Replace the named values with the approved destination and select one maximum
classification. HTTP is supported only by test fixtures. Redirects are refused.
Credentials cannot be sent to a different origin through a redirect.

## Upload and download

| Command | Effect |
|---|---|
| `lean-ctx cloud index push-initial` | Explicit first write; may create a local key and requires an absent remote slot. |
| `lean-ctx cloud index push` | Updates an existing owned slot after authenticating its envelope; does not silently create a replacement key. |
| `lean-ctx cloud index pull` | Downloads, authenticates, rebuilds from current originals, and restores an admitted local generation. |
| `lean-ctx cloud index status` | Shows bounded remote listings and the server's account storage quota. Header claims remain unauthenticated. |

`lean-ctx sync index` accepts the same subcommands. First creation uses
`If-None-Match: *`; updates use the exact authenticated predecessor ETag through
`If-Match`. A conflict is returned to the caller and is not blindly retried.
Only an acknowledgement of the exact ciphertext digest advances the stored
generation floor.

Uploads join source and manifest classification, apply current egress policy,
and bind the actual encrypted bytes to the persisted admission decision. A
transport receipt reports the observed HTTP attempt; it does not prove that a
model used any context. Background index sync requires global opt-in, an
explicit destination and a selected session project, and uses the same upload
checks as the manual path.

## Owned data and recovery

```text
lean-ctx cloud index download-owned ABSOLUTE_NEW_FILE
lean-ctx cloud index inspect-owned ABSOLUTE_FILE
lean-ctx cloud index open-owned ABSOLUTE_FILE
```

Download requires account authentication. The new ciphertext file must have a
private parent directory. Inspection shows untrusted envelope structure only.
Owned open uses the local Runtime package and existing OS key; it requires no
account credentials, license file or network. An expired paid entitlement does
not revoke access to already owned ciphertext.

Normal open rejects a generation below the known floor. Explicit recovery
(`recover` for the remote slot, or `open-owned FILE --recover-stale` for a local
file) does not lower that floor. If local history was lost, establishing a floor
is a separate action:

```text
lean-ctx cloud index open-owned ABSOLUTE_FILE --establish-history-floor --confirm-envelope-sha256 SHA256
```

Confirm the exact ciphertext digest from the inspected, selected file. This
action requires an absent history floor and may persist an authenticated floor
before local BM25 restoration finishes. After a later restore error, keep the
same ciphertext or a newer envelope; do not erase history to force a rollback.

Authenticated owners retain server read/list/delete access after a downgrade.
`delete --confirm-etag ETAG` deletes the selected project's remote envelope only
if the current ETag matches. It does not remove local keys or other projects.

## Pair another installation

The receiving installation starts `pairing begin --code-file ABSOLUTE_NEW_PRIVATE_FILE`.
Transfer that private code file through a confidential channel. On the owning
installation, `pairing status` identifies the key and
`pairing approve KEY_ID --code-file PRIVATE_FILE` creates the encrypted offer.
On the receiver, use `pairing inspect INVITATION_ID` and then
`pairing complete INVITATION_ID --confirm-offer-sha256 SHA256` for the exact
inspected offer. The selected project namespace must match on both sides.

`pairing cancel` revokes the remote offer and discards the receiver's pending
key; `pairing discard` deletes the remote offer only. `pairing cleanup` retries
bounded pending remote deletions. Read `cloud index pairing --help` for exact
arguments. Key bytes remain inside the Runtime. Two installations sharing one
OS-user keychain do not demonstrate transfer between independent device stores.

## Limits and compatibility

- A ciphertext envelope is bounded to 64 MiB. Account storage quota is a
  separate entitlement returned by `status`; a storage allowance is not a
  supported single-project corpus size or latency guarantee.
- Producer discovery, source bytes, decompression, object counts and strings
  have explicit limits. Oversized or unverifiable input is refused; partial
  discovery cannot be published as a complete generation.
- V1 restoration is explicit: `cloud index legacy-pull --accept-v1`. There is
  no automatic V1 fallback or migration into V2.
- Private-filesystem checks, OS-keyring behavior and installation need separate
  qualification on every supported operating system. The macOS candidate
  checks do not qualify Linux or Windows.
