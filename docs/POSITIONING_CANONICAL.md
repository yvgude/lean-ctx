# LeanCTX canonical positioning

Status: Active public product and claims policy.

## One product

**LeanCTX — Context Gateway for AI Systems**

**Control what your AI can see.**

LeanCTX sits between your AI and the systems it reads. It selects task-relevant
context, applies supported access and content controls before delivery, and
records the context operations and delivery evidence the integration can observe.

“Records what reached the model” requires a delivery path that LeanCTX observes.
In context-only integrations, LeanCTX records the prepared result; the host owns
the subsequent model call.

## Select → Control → Learn → Prove

- **Select:** give AI the context the task needs, using retrieval, structure,
  ranking, compression, reuse and source recovery.
- **Control:** decide what may cross the boundary, using the permissions,
  content rules and budgets supported by the selected integration and edition.
- **Learn:** connect code and local knowledge, retaining task, finding and
  decision records across sessions under configured access and retention rules.
  This is context continuity, not model training.
- **Prove:** inspect source references, policy decisions, receipts and usage
  evidence within that integration's visibility.

The technical lifecycle **Select → Shape → Reuse → Recover** describes mechanisms
inside the product. Other deeper lifecycles must not replace the primary story.

## Names and boundaries

| Name | Meaning |
|---|---|
| **LeanCTX** | The product and brand |
| **Context Gateway for AI Systems** | The primary category |
| **LeanCTX Engine** | The runtime implementing context capabilities |
| **LeanCTX SDK** | The integration surface for embedding supported Engine capabilities |
| **Context Intelligence** | Context selection and preparation capabilities inside LeanCTX |
| **Community** | The open-source Engine and its supported local paths, under Apache-2.0 |
| **Pro** | The released personal hosted-services plan, including Personal Cloud sync/backup and hosted index capacity within the selected plan's limits |
| **Enterprise** | Commercial organization controls and deployment, scoped to the licensed release and agreement |
| **SDK / OEM** | Embedding and distribution scope under the applicable SDK/runtime licenses and commercial agreement |

Keep the customer's application, agent loop, model choice and UI. Coding agents
are a major use case, not the whole category. LeanCTX can complement retrieval
systems, vector databases, model gateways, guardrails and agent frameworks.
Capabilities overlap; no “only” or “first platform” claim follows.

## Availability

**Available** means a supported user path in a named edition/release.
**Preview** means an explicitly evolving interface with compatibility limits.
**Research** means a direction without a supported public product contract.
**Historical** means a retained, superseded record.

Implementation alone does not establish availability. An OSS status statement
does not determine the status of a separately licensed Enterprise release or
SDK artifact. The standalone SDK's stable manifest governs its Stable APIs;
its Preview namespace remains separately labeled. Old in-tree SDK experiments
do not supersede that manifest.

Local context-only use does not require hosted services. Provider/execution
controls apply only to supported calls routed through the configured runtime.
Enterprise identity, organization policy, retention, connector coverage and
operations must be described against that edition's supported release.
Pro availability does not establish that every newer adaptive-context or
intelligence feature is included in the current public release.

## Evidence and security claims

- Describe configured detector coverage and distinguish warning, redaction
  and blocking. Do not promise detection of every secret or prevention of all
  leakage. Local processing does not imply local model inference.
- Recovery depends on authorization, source/archive availability and retention.
  Do not promise that nothing is ever lost.
- Distinguish representation-token estimates, provider-counted counterfactuals,
  observed provider usage, calculated cost, provider-reported cost and
  invoice-reconciled spend.
- Numerical comparisons require workload, baseline, treatment, methodology,
  measurement type, relevant quality threshold and version/date.
  A signed receipt proves integrity of recorded evidence, not outcome quality.
- Policy templates and technical controls do not establish compliance
  certification, residency or an operational SLA.
- Use Thinkery AG for current operator metadata; preserve the parties named
  in historical legal records.

## Public-surface governance

The [public claims contract](contracts/public-product-claims-v1.md) and its CI
checker govern current entry points. Legacy categories may occur in explicitly
historical records, quoted comparisons and internal API identifiers. They may
not define the current product.

Primary pages and translations share this semantic hierarchy. Translating the
promise is appropriate; changing its product meaning is not.
