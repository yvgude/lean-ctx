Your policy is non-negotiable: credentials and customer PII must never leave the building inside an LLM prompt — even by accident, even in a stack trace an agent happened to `cat`. This journey sets one policy floor and enforces it at the pre-prompt choke point, uniformly, for every item heading to the model.

---

## 1. Set a policy floor once, globally

```toml
[sensitivity]
enabled = true                # no-op until set. Env: LEAN_CTX_SENSITIVITY
policy_floor = "confidential" # public < internal < confidential < secret
action = "redact"             # redact (mask spans) | drop (withhold whole item)
```

## 2. Enforced at the choke point

From then on, every item heading to the model is classified and enforced. With
`redact`, a leaked AWS key or card number is masked in place; with `drop`, the
offending item is withheld entirely.

## 3. Under the hood — `rust/src/core/sensitivity/`

- Ordered levels `Public < Internal < Confidential < Secret` drive a single
  `level >= floor` comparison.
- **Honest classification only** — no speculative heuristics. Secret-like paths and
  detected secrets → `Secret`; **Luhn-validated** card numbers and **ISO-7064**
  IBANs → `Confidential`. This keeps false positives from silently degrading good
  context.
- One `enforce_text()` entry point is applied uniformly to **tool outputs** and
  **knowledge injection** — including downstream results coming back through the MCP
  Tool-Catalog Gateway.

## Payoff

A uniform, auditable guarantee that sensitive data is handled *before* it reaches
the model — off by default, so nothing changes for users who don't opt in.
