# Where LeanCTX fits

**Context Gateway for AI Systems**

**Control what your AI can see.**

LeanCTX controls the supported information path from systems and tools into AI:
**Select → Control → Learn → Prove**. The LeanCTX Engine implements that path; the
LeanCTX SDK lets you embed its supported capabilities in your application.

| Category | Primary responsibility | Relationship to LeanCTX |
|---|---|---|
| Vector database / RAG | Store and search knowledge; retrieve candidate information | Can supply candidates. LeanCTX prepares supported context and applies its controls before delivery. |
| AI/model gateway | Govern model/API traffic, routing, reliability and spend | Can receive model calls using prepared context. Supported LeanCTX execution paths may overlap; context-only use leaves the model call with the host. |
| AI security / guardrails | Detect or block unsafe/disallowed input or output | Can complement configured LeanCTX access/content checks; no single detector guarantees complete coverage. |
| Agent framework | Run workflows and agent loops | Remains the host. It can call the Engine through the SDK or a supported tool interface. |
| MCP | Connect applications to tools and resources through a protocol | One integration surface for LeanCTX; the protocol itself is not an access/content policy. |
| Model provider | Run inference | The customer's choice. Local preparation does not imply local inference or prevent independent host egress. |
| LeanCTX | Select, control and make traceable supported context flowing from systems/tools into AI | Provides context operations, configured boundaries and evidence; does not own the whole application. |

These categories overlap. This table describes responsibilities, not exclusive
features or claims that another product cannot implement them.

## Choose the integration boundary

- **Existing coding tool:** configure a supported local [Attach path](guides/README.md).
- **Your application or copilot:** use the [SDK](reference/sdk-surface.md) for
  its supported context lifecycle or Agent Tools; keep your model and workflow.
- **Organization rollout:** scope shared controls, evidence and deployment
  against the [Enterprise release and agreement](https://leanctx.com/enterprise/).

Start with [What is LeanCTX?](what-is-leanctx.md), then inspect
[architecture](../ARCHITECTURE.md) and [availability](../VISION.md).
