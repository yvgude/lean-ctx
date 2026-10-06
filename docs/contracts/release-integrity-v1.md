# Release Integrity v1

`leanctx.release-manifest/v1` binds a published lean-ctx release to its source
tag and commit. The release job builds artifacts from the tagged source, emits
the CycloneDX inventory in `SBOM.cdx.json`, writes `SHA256SUMS`, and
then writes `release-manifest.json`. All three metadata files are release
assets alongside the archives.

## Artifact chain

```
source commit → build artifacts + SBOM → SHA256SUMS → release-manifest.json
```

`SHA256SUMS` records every inventoried archive and supplemental file digest. `SBOM.cdx.json` is the
CycloneDX dependency inventory. The deterministic manifest records the tag,
full source commit, per-artifact digest and size, optional extracted-binary
digest, and digests of the SBOM and checksum file. It intentionally contains no
wall-clock timestamp.

## Manifest schema

The manifest has exactly these fields:

- `schema_version`: `leanctx.release-manifest/v1`
- `tag`: the Git tag used for the release
- `commit`: 40-character source commit ID
- `artifacts`: map of asset name to `{ "sha256", "size" }`, plus
  `payload_sha256` for binary archives
- `sbom_sha256`: SHA-256 of `SBOM.cdx.json`
- `checksums_sha256`: SHA-256 of `SHA256SUMS`

Newly generated v1 manifests add a `kind` to every artifact: `binary`, `source`,
or `supplemental`. Binary payload digests remain mandatory. The verifier derives
the expected kind independently from the canonical filename, not the claim.
Legacy v1 manifests without kinds retain their original binary/source checks;
mixing typed and legacy records is rejected.
Removing all typed records and supplemental assets cannot be distinguished
from a genuine legacy manifest; the publication path requires typed inventory.

`scripts/release_inventory.py` is the single inventory authority for checksum,
manifest and upload membership. Supplemental records also bind `source_path`:
`SBOM.cdx.json`, `THIRD_PARTY_NOTICES`, `LICENSE.md`, and the preserved Apache
text under `LICENSES/Apache-2.0.txt`, uploaded as `LICENSES-Apache-2.0.txt`.
New Apache-host inventories do not include unused commercial draft terms.
Verification still recognizes the exact former commercial supplemental name
and source path in older signed inventories; it remains bound to their checksums
and manifest. These legacy supplemental assets are refused by the new upload-list
path. Unknown supplemental paths remain invalid. No license grant changes.
Conflicting pre-existing flat files, symlinks and unsafe names fail closed.

Generation writes checksums before computing the manifest's checksum digest.
After signing, `upload-list` verifies the artifact chain and the presence of all
six checksum/manifest signature-envelope files, then supplies the exact upload
list. Presence is not signature authentication; the existing Cosign boundary
still applies. Public-source filtering and legal rollout are separate gates.

## Downstream verification

Download all release assets and run:

```bash
python3 scripts/verify-release-integrity.py verify \
  --tag v3.9.14 --dir ./release-files
```

For a clean offline directory, first retrieve the metadata and checksum-listed
artifacts from the public GitHub release, then repeat verification:

```bash
python3 scripts/verify-release-integrity.py download \
  --tag v3.9.14 --dir ./release-files
python3 scripts/verify-release-integrity.py verify \
  --tag v3.9.14 --dir ./release-files
```

The verifier emits a JSON report, checks the requested tag, validates the
closed manifest schema, hashes the SBOM and checksum file, and checks every
archive's digest and size against both metadata records.

## Failure handling

Any missing file, malformed metadata, unexpected artifact set, tag mismatch,
or digest/size mismatch returns exit code 1. Consumers must reject the release,
delete the untrusted download directory, and obtain assets again from the
release URL. The workflow additionally signs `release-manifest.json` with
keyless Cosign/OIDC and publishes its `.sig` and `.pem`. Consumers must verify
the certificate issuer and pinned release-workflow/tag identity before trusting
the manifest; the secure updater enforces this boundary.
