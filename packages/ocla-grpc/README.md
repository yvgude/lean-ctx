# LeanCTX OCLA gRPC v1

This package is a real, bounded gRPC projection of the public OCLA v1 token
and agent envelope contracts. It links only the standalone public Rust client,
not the LeanCTX engine.

```bash
cargo run --locked --manifest-path packages/ocla-grpc/Cargo.toml
```

The unauthenticated v1 listener is deliberately loopback-only. The default is
`127.0.0.1:50051`; `--listen=[::1]:50051` is also accepted. Remote exposure,
TLS termination, identity, authorization, deployment and production readiness
are outside this package and are not implied by a successful conformance run.

Each gRPC request is limited to 64 KiB, responses to 4 KiB and verification to
two seconds. One connection accepts at most 16 concurrent calls. Across every
server instance in one process, at most 64 token/agent verifier calls run at
once; saturation fails immediately without queueing. Accepted TCP sockets and
standard gRPC health calls are outside that process-wide verifier-call cap.
Semantic rejections contain only a stable enum and the fixed `ocla/v1` API
version; transport rejections and the executable's fixed failure line never
echo input content.

This is a first-party transport adapter. It is not an independent external
OCLA implementation, certification, package publication, G6 completion or
evidence of cross-version remote interoperability.
