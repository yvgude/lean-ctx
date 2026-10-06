# Telemetry threat model

Telemetry is an allowlisted, content-free projection. Threats include prompt
or source leakage, installation correlation, tenant cross-talk, replay,
unauthorized export, and retention failure.

Mitigations: schema validation, deny-unknown fields, redaction before enqueue,
per-installation pseudonyms, TLS/authenticated gateway transport, bounded
batching, deletion/purge controls, access audit, and automated secret/content
regression tests. Product analytics and operational logs remain separate.
