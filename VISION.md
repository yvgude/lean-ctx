# LeanCTX Vision

**Context Gateway for AI Systems**

**Control what your AI can see.**

LeanCTX sits between AI and the systems it reads. It selects task-relevant
context, applies supported access and content controls before delivery, and
records observable context operations and delivery evidence.

## Select → Control → Learn → Prove

Give AI useful context. Decide what may cross the boundary. Connect local
knowledge across sessions. Inspect what happened. These are four complementary
responsibilities; Learn means context continuity, not model training.

The **LeanCTX Engine** implements the runtime capabilities. The **LeanCTX SDK**
connects applications to supported Engine interfaces. **Context Intelligence**
describes the retrieval, structural understanding and preparation inside the
Gateway; it is not a separate product category.

The technical context lifecycle remains:

```text
Select → Shape → Reuse → Recover
```

The customer owns the application, agent loop, task logic, model and retry policy.
Coding agents are an important adoption path alongside embedded applications.

## Product and status boundaries

| Surface | Status and scope |
|---|---|
| Community Engine | **Available:** local CLI, MCP, supported hooks/proxy integrations, retrieval, structural views, compression, cache, recovery, configured controls and local evidence. Apache-2.0. |
| Pro | **Available:** optional Personal Cloud sync/backup and hosted personal index within the selected plan's limits. This existing commercial plan does not make newer intelligence features available without a named release. |
| Standalone LeanCTX SDK | **Available** Stable lifecycle and Agent Tools within its published compatibility contract. Separate source-available license; production/OEM rights require a commercial agreement. |
| SDK Preview | **Preview:** workspace, checkpoint, delta, handoff and other interfaces explicitly placed in the SDK Preview namespace. |
| Enterprise | Commercial organization controls and deployment under a licensed release and agreement. Its availability is established by Enterprise contracts, not by this OSS repository. |
| Research directions | First-class Context Kits, universal workspace/coordination contracts, Performance Profiles as a promoted product, Performance Benchmark, AutoTune and marketplace directions remain **Research** unless individually promoted through a release gate. |

The stable SDK manifest is independent of older in-tree Python wrappers and
embedding experiments. A source directory or passing unit test does not establish
a release. Conversely, an old OSS research note does not demote an available
commercial capability.

## Integration and visibility

- **Attach:** configure LeanCTX around an existing tool through a supported
  local CLI, MCP, hook or proxy path.
- **Embed:** use the standalone SDK's supported interfaces in your application.
  Stable and Preview APIs retain their own compatibility guarantees.
- **Deploy:** choose the Enterprise controls and operating model covered by
  the licensed release and agreement.

In context-only mode, the application receives prepared context and owns the
model call. Model/provider execution controls apply only to supported requests
actually routed through LeanCTX. Receipts cannot observe unrelated host actions.

## Evidence before claims

Compare the same workload, baseline and treatment with declared measurement
methods and quality criteria. Token reduction, estimated cost, provider-reported
usage and accepted outcomes are different facts. A cheaper failed task is not a
win, and a signed receipt is not a universal savings guarantee.

Local context remains useful without an account or hosted service. Recovery
depends on the retained source, permission and retention policy. Security rules
cover configured detectors and supported paths, not every possible data leak.

The [canonical positioning](docs/POSITIONING_CANONICAL.md) and
[public claims contract](docs/contracts/public-product-claims-v1.md) govern current
public language. [What is LeanCTX?](docs/what-is-leanctx.md) explains the product;
[architecture](ARCHITECTURE.md) maps the supported boundaries.
