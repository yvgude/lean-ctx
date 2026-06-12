# The Context Stack — Ecosystem Overview

> Public overview of how the products compose. Per-product manifestos:
> [`lean-ctx/VISION.md`](VISION.md) · `ctxpkg-org/VISION.md` ·
> `ctxpkg-com/VISION.md`.

Software ate the world. Agents are eating software. And every agent is exactly
as good as the context it is given — context decides what an agent knows, what
it may do, and what it provably did. Today that context is unmanaged: untyped
markdown, copy-pasted prompts, vendor-locked memory, zero provenance.

**The Context Stack makes context infrastructure**: efficient, verifiable,
tradable, organizational — managed with the same rigor as code.

## The four layers

| # | Layer | Product | Question it answers |
|---|-------|---------|--------------------|
| 1 | **Law** | **ctxpkg.org** — the open standard | What is valid context, and can any tool verify it offline? |
| 2 | **Distribution** | **ctxpkg.com** — the registry & marketplace | Where does context come from, and why trust the download? |
| 3 | **Enforcement** | **LeanCTX** — the context engine | What may *this* agent see, at what cost, and can we prove what it saw? |
| 4 | **Connection** | **CTXFabric** — the organizational context platform | How does an organization's knowledge become — and stay — governed, AI-ready context? |

Read bottom-up it is a supply chain: *distill → seal → publish → verify →
enforce* (told on [ctxpkg.com/governance/](https://ctxpkg.com/governance/)).
Read top-down it is a control plane: law constrains distribution, distribution
feeds enforcement, the fabric feeds and consumes both.

Each layer is independently replaceable **by design** — that is what makes the
whole credible, and why adopting one layer never forces buying another. The
interfaces are open (spec, registry protocol, RFC process); the implementations
compete on quality.

## CTXFabric in one paragraph

LeanCTX distills context from *code and sessions*; CTXFabric distills it from
*the organization* — policies, processes, compliance rules, historical
decisions — without requiring technical skills. Imported knowledge is curated,
compiled into signed `.ctxpkg` packages and governed (who uses what, is it
still valid, which agent consumed it). Its second act connects engines into a
fleet: shared organizational memory, policy-routed in real time, with evidence
at fleet scale. Packages stay on the registry; the fabric keeps them alive.

## How the layers compound

1. **LeanCTX** makes every session cheaper and distills knowledge as a
   byproduct → raw material.
2. **CTXPKG** makes that knowledge portable and verifiable → assets instead of
   session artifacts. This includes *learned optimization profiles* — the
   engine learns locally (zero telemetry, always) and the results travel as
   signed packages, not as harvested data.
3. **ctxpkg.com** makes the assets distributable and worth money → publishers
   are paid to produce exactly what makes engines smarter.
4. **CTXFabric** makes the assets organizational and alive → the value of
   every package multiplies by the number of connected agents → more engines
   adopt → back to 1.

**The engine creates supply, the standard creates trust, the registry creates
a market, the fabric creates network effects.**

## Doctrines

- **Zero telemetry, absolutely** — nothing leaves a machine automatically;
  explicit, locally computed, user-invoked shares only.
- **Trust is never for sale** — no paid placement, ranking or verification on
  any surface.
- **No layer merges** — the standard never grows vendor hooks, the registry
  never requires our engine, the fabric never requires our registry.
- **Distilled, typed, signed knowledge only** — never raw transcripts.

---

*The stack in one line:*
**LeanCTX compresses it. CTXPKG seals it. ctxpkg.com ships it. CTXFabric keeps it alive.**
