# OCLA external consumer (Rust)

This standalone crate decodes the public canonical-token and agent-envelope
fixtures through `lean-ctx-client` without linking the LeanCTX engine or using
internal types.

The fixture carries a manifest template, committed lock and source file. The
SDK integration test materializes them beneath its isolated target directory,
then validates the dependency graph and runs the consumer without network
access:

```bash
cargo test --locked --test external_consumer
```

The test runs from both the repository and an extracted crate package. It
asserts content-free deterministic output:

```text
ocla_api=ocla/v1;token_schema_version=1;agent_schema_version=1;gateway_admissible=true
```

The materialized path dependency resolves only to the surrounding public client
package; every other graph node must resolve from crates.io.
This proves a packaged, in-repository downstream compile/run boundary only; it
does not prove crates.io publication, organizational independence, remote
interoperability, certification, or G6.

The example bounds bytes read from each document but does not implement atomic
special-file or symlink handling. For hostile filesystem paths, use the
`lean-ctx-ocla-verify` CLI.
