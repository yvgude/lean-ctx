# LeanCTX public host licensing

`LICENSE_MATRIX.toml` is the deterministic, path-level authority for this
repository. Its `apache-host` distribution contains the public host and open
interfaces. Product capability or directory names never change a file's license.
The existing root `LICENSE`, `NOTICE` and `CLA.md` remain byte-for-byte unchanged.

## Public host and private components

- `public_trust_core`, `sdk_protocol`, `docs_assets`, `tests_examples`, and
  `historical` paths are Apache-2.0 unless the matrix says otherwise.
- `commercial`, `free_runtime` and `private_service` paths are forbidden in
  this public tree. Their implementations and applicable terms belong to
  separately delivered private components, not to this source distribution.
- `generated` paths require recorded generator ownership and provenance.
- `third_party` paths retain upstream terms and require reconciled notices.

Apache rights already granted to the source and its contributors remain
unchanged. This distribution does not activate the unused mixed-license,
Free-Runtime, trademark or CLA-v2 drafts. Those drafts are preserved in the
private development record. Removing them from this source tree grants no
new rights to private code, names or third-party material and is not approval
of private commercial terms.

The immutable CLA-v1 signature inventory is external to this candidate: commit
`f09ea6d8a28066007678342056ac8be95df756d7` at
`signatures/v1/cla.json`, SHA-256
`831846e0db4c8d1d097eb8988e23685bb5990030641c8b6c9f5cae83ced6a56e`, with 19
signers. The older 18-signer digest is rejected. The public contribution flow
continues to use the unchanged `CLA.md` and its v1 signature store; no v1
signature is treated as CLA-v2 acceptance.

The matrix retains its existing pre-v4 classification cut
`b4429b80f3bc75bab4c2d7a959c9ae81ba5a2666` and the immutable CLA evidence above.
The audit rejects changes to the preserved grants and ambiguous or unclassified
tracked paths. Third-party
notices, their exact-content approval and the release SBOM remain required.
Passing the public path check alone does not authorize publication.
