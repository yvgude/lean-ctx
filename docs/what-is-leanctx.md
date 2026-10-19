# What is LeanCTX?

**Context Gateway for AI Systems**

**Control what your AI can see.**

LeanCTX sits between AI and the systems it reads. It selects task-relevant
context, applies supported access and content controls before delivery, and
records the context operations and delivery evidence the integration can observe.

## Select → Control → Learn → Prove

| Step | What it does | What you receive |
|---|---|---|
| **Select** | Retrieve and rank candidates; prepare the useful level of detail | Task-relevant context within the configured budget |
| **Control** | Apply supported source permissions, path boundaries and content rules | An allowed result, a warning, redacted content or a refusal |
| **Learn** | Retain local task records and connect code and knowledge under configured access and retention rules | Context continuity across sessions, without training your model |
| **Prove** | Record observable source, operation, policy and usage facts | References, receipts and measurements for inspection |

Inputs can include local files, repositories, tool results, memory and configured
provider integrations. The output is prepared context plus the evidence the
particular interface supplies. Recovery can return to an authorized retained
source or archive when more detail is needed.

## Where it sits

```text
Your sources and tools
        ↓
LeanCTX Engine — select, control and record context
        ↓
Your AI application or agent
        ↓
Your chosen model
```

In **context-only mode**, your application receives prepared context and makes
its own model call. The receipt records preparation, not an unseen later prompt.
In a **supported governed execution path**, model/tool requests also pass through
the configured runtime and its applicable route/budget policies.

These controls cover supported calls routed through LeanCTX. They do not control
an application that reads or sends data through another path.

## One product, different ways to use it

| Use | Component and boundary |
|---|---|
| **Developers** improving existing AI tools | Community LeanCTX Engine: Apache-2.0, local CLI/MCP, supported hooks and proxy paths. Start with [Codex, Claude Code or Cursor](guides/README.md). |
| **Organizations** applying shared controls | Enterprise: licensed identity, organization policy, evidence and deployment capabilities within the supported release and agreement. [Enterprise overview](https://leanctx.com/enterprise/). |
| **Product builders** embedding context infrastructure | LeanCTX SDK: supported language-native interfaces to the Engine; stable lifecycle and Agent Tools plus separate Python `leanctx_sdk.preview` operations. [SDK contract](https://github.com/Thinkery-AG/leanctx-sdk). Production/OEM use requires a signed written commercial agreement under the SDK license. |

**Build your product. Do not rebuild your context infrastructure.** The SDK gives
you a documented integration boundary for supported selection, structural tools,
recovery, permissions and evidence. Your product still owns its workflow, model,
UI and operating responsibilities. Compatibility and packaging are tied to the
specific SDK and Engine release.

## What it replaces—and what it does not

LeanCTX can replace custom code for the context operations and controls covered
by its interfaces. It does not replace your application, agent loop, model,
source systems or a vector database. It can work alongside RAG systems, model
gateways and guardrails; their responsibilities can overlap.

[Where LeanCTX fits](where-leanctx-fits.md) compares those roles.

## Honest limits

- Security asks what AI **may** see; Context Intelligence asks what it **should**
  see; optimization asks how much it **needs**; evidence records what was
  prepared or delivered within the integration's visibility.
- Detector coverage is configured and finite. Warning, redaction and blocking
  are different actions, and local context processing does not mean local inference.
- Token reduction and calculated savings do not establish answer quality or
  invoice savings. Compare your workload with a declared baseline and quality gate.
- Recovery depends on permission, retention and source availability.

See [architecture](../ARCHITECTURE.md), [security](../SECURITY.md) and
[canonical claims policy](POSITIONING_CANONICAL.md).
