# Deprecation & Security Advisory Process

## Contract Deprecation

### Timeline

1. **Announce** — Add `deprecated: true` to the schema, publish a release note, and add an SDK warning.
2. **Migration Window** — Support a superseded public wire major for at least 12 months; additive V1 fields remain accepted throughout the V1 support line.
3. **Removal** — Archive the schema file to `docs/contracts/archive/`.

### SDK Impact

- Deprecated types are marked with `@deprecated` (TypeScript), `warnings.warn` (Python), and a `Deprecated` comment (Go).
- SDK N+1 removes deprecated types.
- SDK N continues to compile while emitting deprecation warnings.

## Security Advisories

### Process

1. Private disclosure through `security@leanctx.com` or a GitHub Security Advisory.
2. Develop the fix on a private branch.
3. Assign a CVE when applicable.
4. Coordinate the patched engine, SDK releases, and advisory.
5. Disclose publicly after 72 hours.

### Affected Contracts

- Wire schemas: PATCH version bump with a backward-compatible fix.
- SDK packages: MINOR version bump with the fix.
- Engine binary: PATCH release.

## Version Compatibility Matrix

| Engine | TS SDK | Python SDK | Go SDK | Wire Schema |
|---|---|---|---|---|
| 3.10.x | 0.1.x | 0.1.x | 0.1.x | v1 |

## Contract Pack

The signed contract pack at [ocla-contract-pack-v1.json](ocla-contract-pack-v1.json)
contains content digests for all schemas. SDKs validate against this at build time.
