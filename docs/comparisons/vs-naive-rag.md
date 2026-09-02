# LeanCTX and basic retrieval pipelines

> **Status: historical comparison note — not canonical product copy.** Current
> LeanCTX scope and status are governed by
> `docs/internal/README.md` (internal, not in this repository). This note makes no
> blanket retrieval-quality, locality, or cost claim.

Retrieval and context shaping solve different constraints. A retrieval system
can locate material from a large corpus; a local LeanCTX Runtime can help an
existing coding agent choose and shape project material before inference. A
workflow may need either or both, depending on the source, task, and evidence.

## Evaluation boundary

Use representative queries and a defined quality measure. Do not assume that
one retrieval method, mode, index, or source structure is best for every
repository. Earlier statements about universal structure awareness, benchmark
floors, or local behavior are historical design material, not availability or
performance promises.

LeanCTX is **The Context SDK for AI Agents**, not a hosted RAG platform. See the
internal Product Architecture (`docs/internal/vision/PRODUCT-ARCHITECTURE.md`, internal — not in this repository).
