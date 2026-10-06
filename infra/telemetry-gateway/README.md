# Telemetry gateway

The gateway authenticates clients, validates the versioned allowlist, strips
unknown fields, batches bounded events, and enforces retention/deletion policy.
It rejects content-bearing or oversized payloads before forwarding to analytics.
