# Public Product Claims Contract v1

Status: Active public governance contract.

This contract is the versioned, public projection of LeanCTX product claims.
It governs public entry points only. Confidential planning material remains
outside the repository and is never read by public CI.

The JSON block below is normative for
`scripts/check-narrative-governance.py`. It intentionally records only public
claims, availability labels, and documents that need an explicit status.

```json narrative-governance-contract
{
  "schema_version": 1,
  "required_text": {
    "README.md": [
      "Control what your AI can see.",
      "LeanCTX — AI Value Gate for AI Coding Agents",
      "Get started (30 seconds)",
      "Real-world scenarios"
    ],
    "VISION.md": [
      "docs/internal/README.md",
      "Context SDK for AI Agents",
      "Select → Shape → Reuse → Recover",
      "first-class Context Kits"
    ],
    "docs/README.md": [
      "Context SDK for AI Agents",
      "internal/README.md",
      "Performance Profiles; first-class Context Kits"
    ],
    "docs/reference/README.md": [
      "Context SDK for existing agents",
      "not a multi-agent platform or orchestration product",
      "No hosted/team/cloud service is publicly available"
    ],
    "docs/guides/README.md": [
      "does not replace the agent or become an agent",
      "Context reduction depends on the file, mode, task, and recovery behavior.",
      "Embed — Preview",
      "Codex, Claude Code, and Cursor are the"
    ],
    "docs/guides/codex-cli.md": [
      "Status: Available first-class local setup path."
    ],
    "docs/guides/claude-code.md": [
      "Status: Available first-class local setup path."
    ],
    "docs/guides/cursor.md": [
      "Status: Available first-class local setup path."
    ],
    "docs/integrations/installation-matrix.md": [
      "Codex, Claude Code, and Cursor are the current first-class local setup paths;"
    ],
    "docs/IMPLEMENTATION_PROTOCOL.md": [
      "Status: orientation index, not a product-status or release record.",
      "docs/internal/README.md"
    ],
    "docs/contracts/http-mcp-contract-v1.md": [
      "Status: Local runtime contract"
    ],
    "docs/releases/v1.0-runbook.md": [
      "Historical — superseded release draft.",
      "OSS Vision Delivery Plan"
    ],
    "docs/ga/release-checklist.md": [
      "Status: active OSS release gate, not a completion record.",
      "standalone W1 customer-proof verifier",
      "Python remains labelled **Preview**",
      "Claim promotion gate"
    ],
    "packages/pi-lean-ctx/README.md": [
      "embedded MCP bridge enabled",
      "Embedded MCP Tools (enabled by default)",
      "diagnostic output, not a general result"
    ]
  },
  "status_guarded_records": [
    "clients/rust/lean-ctx-client/README.md",
    "docs/contracts/wrapped-permalink-v1.md",
    "docs/context-os/guide.md",
    "docs/context-os/cookbook-non-coding.md",
    "docs/reference/08-multi-agent.md",
    "docs/reference/09-team-cloud-ci.md",
    "docs/reference/18-adaptive-learning.md",
    "docs/guides/addons.md",
    "docs/guides/aider.md",
    "docs/guides/gemini-cli.md",
    "docs/guides/hosted-index-slo.md",
    "docs/guides/opencode.md",
    "docs/guides/org-sso-setup.md",
    "docs/guides/pi.md",
    "docs/guides/windsurf.md"
  ],
  "feature_statuses": {
    "ContextWorkspace / Checkpoint / Delta": "Research",
    "Shared project context and handoffs": "Research",
    "Performance Profiles": "Research",
    "Context Kits": "Research",
    "Performance Benchmark": "Research",
    "Named SDK `wrap()` adapters": "Preview"
  }
}
```

## Availability baseline

| Product surface | Status |
| --- | --- |
| Context Workspace / Checkpoint / Delta | Research |
| Shared project context and handoffs | Research |
| Performance Profiles | Research |
| Context Kits | Research |
| Performance Benchmark | Research |
| Named SDK `wrap()` adapters | Preview |
