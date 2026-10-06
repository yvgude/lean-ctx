# Capability registry schema v2

Schema v2 separates price, source visibility, runtime delivery and commercial
rights. Each capability has 18 required fields. The source registry and packaged
mirror must match byte-for-byte; the Rust generator renders the same fields.

All 60 existing IDs and 23 legacy lookup aliases are preserved. Historical
`pro.`/`team.` prefixes are identifiers, not price or source authority. Aliases
live in a separate table and cannot shadow IDs or refer to other aliases.

## Access migration

- Local reference capabilities, local coordination and the classified Aha
  paths are free. The bounded Work Graph no longer consults billing; agent
  registration, project isolation, leases, policy and execution bounds remain.
- Hosted free Team capabilities require current account credentials and a
  permitted deployment. They do not require a paid entitlement. The server
  remains authoritative for workspace membership and configurable resource
  allowances; a client price classification grants neither membership nor
  unlimited hosted capacity.
- Paid Cloud access still requires an explicit, current signed capability.
  Legacy plan names remain wire compatibility values, not separate pricing
  sources. CloudPaid projects to the legacy Pro floor; signed subsets and
  resource ceilings continue to constrain access.
- New SSO and governance audit classifications are Enterprise. Explicit signed
  v1 Team grants for `team.sso_oidc` and `team.audit_retention` remain honored
  only when issued before 2026-09-13 00:00:00 UTC, within their original bindings
  and validity windows. A Team plan without
  those signed keys grants neither. Issuers must use the v2 classification for
  new commercial offers instead of issuing legacy Team governance grants.
- Free Runtime use does not grant source, redistribution or OEM rights.
  No historical Apache license or published source is reclassified as secret.

## Verification and maturity

The registry validates unknown fields/enums, duplicate or dangling aliases,
missing classification, Aha paywalls, public crown-jewel classification,
license/rights mismatches and inconsistent delivery modes. Negative tests cover
private rights leakage, missing fields and unknown telemetry classes.

`source_path` identifies implementation ownership, not a release attestation.
Private paths use the target repository names; deployments may use older clone
names. Existing research/preview/private maturity markers are not promoted by
this migration. The registry alone does not prove runtime packaging, Free Team
E2E, managed allowances, signature verification or production readiness.
