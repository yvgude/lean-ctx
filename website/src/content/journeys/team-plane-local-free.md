You want shared coordination, RBAC and a procurement-grade ROI number — *without* taking away anything your developers get for free locally. Most tools monetize by gating features; LeanCTX does the opposite. Local stays best-in-class and ungated; only team coordination is paid.

---

## 1. Local stays free — billing is informational only

Billing *describes* plans and *meters* local savings; it never gates a local
capability:

```bash
lean-ctx billing plans               # free · team · enterprise (additive entitlements)
lean-ctx billing usage --json        # metered from the signed ledger, read-only
```

## 2. Team coordination, with real RBAC

Issue role-scoped tokens and serve a shared, audited team endpoint:

```bash
lean-ctx team token create --config team.json --id alice --role viewer   # viewer·member·admin·owner
lean-ctx team serve --config team.json
```

A `viewer` may search but is denied mutations and audit (`403 scope_denied`); an
`admin` has the full scope set — every decision written to an audit log.

## 3. Prove the value — a reproducible, signed ROI artifact

```bash
lean-ctx savings roi                 # net tokens, USD, top tools — SHA-256 chain + Ed25519 signature
```

## 4. Under the hood — `http_server/team/` + `core/billing/`

- The Team/Org plane handles bearer auth, `TeamRole` → scope expansion and a
  per-request audit log.
- Billing's `entitlement_allows` returns **`true` for every local feature on every
  plan** — the billing-layer expression of the Local-Free Invariant.
- The invariant is not a promise but a **CI gate**: the build fails if any local
  capability is ever placed behind an account, license or plan.

## Payoff

A genuinely monetizable team plane that adds coordination, governance and scale —
while the local engine every developer runs stays free, ungated and best-in-class,
forever.
