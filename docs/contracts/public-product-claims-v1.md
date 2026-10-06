# Public product claims contract v1

Status: Active public claims policy.

This contract defines LeanCTX's current product identity and the pages and
metadata checked by CI. Primary product pages state the category and promise
within their opening 1,500 visible characters, rather than bury them in technical detail.
The component names and primary story are required on the canonical positioning
page and the product overview page. Feature documentation may describe a
bounded capability without repeating the full product message.

Current product identity:

- **Product:** LeanCTX
- **Category:** Context Gateway for AI Systems
- **Promise:** Control what your AI can see.
- **Components:** LeanCTX Engine and LeanCTX SDK
- **Primary story:** Select → Control → Prove

The checker discovers public README files, Markdown pages linked from the
listed entry-point indexes, and descriptions in public package manifests. It
checks introductions, headings and explicit later product definitions for
competing categories, and visible prose for unsupported blanket claims.
Code examples and HTML comments do not satisfy the identity requirement.
It prunes private, archived, vendored, and test
data directories before walking them.

A legacy category can appear in a retained record only when the record has a
prominent `Status: Historical` header and links to the current canonical
positioning page. Scoped technical discussion in the body remains available;
explicit current product definitions remain checked throughout the page.
Versioned changelog entries retain their historical text while their current
introduction and Unreleased section remain checked.
One path-specific technical heading is retained for the existing configuration
reference; that exception does not authorize a product category claim or broader
use of a retired label.

Numerical or security outcomes require evidence in the same
paragraph: workload, baseline, treatment, methodology, quality threshold, and
a version or date. A signed receipt proves integrity of recorded evidence, not
outcome quality.

```json narrative-governance-contract
{
  "schema_version": 1,
  "product": {
    "name": "LeanCTX",
    "category": "Context Gateway for AI Systems",
    "promise": "Control what your AI can see.",
    "components": ["LeanCTX Engine", "LeanCTX SDK"],
    "primary_story": ["Select", "Control", "Prove"]
  },
  "primary_entrypoints": [
    "README.md",
    "VISION.md",
    "ARCHITECTURE.md",
    "docs/README.md",
    "docs/reference/README.md",
    "docs/guides/README.md",
    "docs/POSITIONING_CANONICAL.md",
    "docs/what-is-leanctx.md",
    "docs/where-leanctx-fits.md"
  ],
  "component_story_surfaces": [
    "docs/POSITIONING_CANONICAL.md",
    "docs/what-is-leanctx.md"
  ],
  "required_text": {
    "README.md": ["Get started", "Real-world scenarios"],
    ".github/workflows/release.yml": ["desc \"Local engine for the LeanCTX Context Gateway for AI Systems\""],
    "aur/lean-ctx/.SRCINFO": ["pkgdesc = LeanCTX Engine — open-source Context Gateway for AI Systems. Context selection, supported controls, and evidence through local integration paths."]
  },
  "forbidden_text": {
    "README.md": [
      "83 MCP tools",
      "79 MCP tools",
      "lean-ctx addon search",
      "Used in production by teams",
      "have shipped (see above)"
    ]
  },
  "status_guarded_records": [
    "docs/cognition-interface.md",
    "docs/cognition-lab/plan-v1.md",
    "docs/context-os/guide.md",
    "docs/context-os/rfc-v1.md",
    "docs/context-os/cookbook-non-coding.md",
    "docs/comparisons/README.md",
    "docs/ga/README.md",
    "docs/integrations/datadog.md",
    "docs/integrations/finops.md",
    "docs/reference/08-multi-agent.md",
    "docs/specs/unified-distribution-v1.md"
  ],
  "feature_statuses": {
    "Multi-agent": "Research",
    "ContextKits": "Research",
    "Workspace": "Research",
    "Handoff": "Research",
    "Standalone SDK Stable API": "Available",
    "Agent Tools": "Available",
    "SDK Preview namespace": "Preview"
  },
  "canonical_reference": "docs/POSITIONING_CANONICAL.md",
  "historical_release_logs": ["CHANGELOG.md"],
  "discovery": {
    "entrypoint_indexes": [
      "README.md",
      "docs/README.md",
      "docs/reference/README.md",
      "docs/guides/README.md",
      "llms.txt",
      "skills/lean-ctx/SKILL.md",
      "rust/src/templates/SKILL.md"
    ],
    "readme_globs": ["README.md", "**/README.md"],
    "metadata_globs": ["**/package.json", "**/manifest.json", "**/Cargo.toml", "**/PKGBUILD"],
    "excluded_prefixes": [
      "_archive",
      "rust/crates/vendor",
      "rust/data",
      "rust/eval/testbench/repos"
    ]
  },
  "legacy_definitions": [
    "AI Value Gate",
    "Context SDK for AI Agents",
    "Context OS",
    "Context Engineering Layer",
    "Cognitive Context Layer",
    "Context Intelligence for AI Systems"
  ],
  "scoped_technical_heading_exceptions": [
    {
      "path": "docs/reference/appendix-paths-and-config.md",
      "term": "AI Value Gate",
      "heading": "AI Value Gate configuration"
    }
  ],
  "claim_evidence_terms": [
    "workload",
    "baseline",
    "treatment",
    "methodology",
    "quality threshold",
    "version/date"
  ],
  "unsupported_claims": [
    {
      "name": "unscoped numerical savings",
      "pattern": "(?:\\b(?:saves?|saving|savings of)\\s+(?:about\\s+|up to\\s+|~)?[0-9]+(?:\\.[0-9]+)?\\s*%|(?<![0-9])[0-9]+(?:\\.[0-9]+)?\\s*%\\s+(?:savings|fewer tokens|less context|token reduction))"
    },
    {
      "name": "60–90%",
      "pattern": "(?<![0-9])60\\s*[-–—]\\s*90\\s*(?:%|percent)(?![A-Za-z])"
    },
    {
      "name": "5–10x",
      "pattern": "(?<![0-9])5\\s*[-–—]\\s*10\\s*[x×](?![A-Za-z])"
    },
    {
      "name": "nothing ever lost",
      "pattern": "\\b(?:nothing\\s+(?:(?:is|will be)\\s+)?ever\\s+lost|never\\s+[\"“]?cold[ -]starts)\\b"
    },
    {
      "name": "zero telemetry",
      "pattern": "\\bzero\\s+telemetry\\b"
    },
    {
      "name": "universal secret detection or data-leak prevention",
      "pattern": "\\b(?:detects?\\s+all\\s+secrets|prevents?\\s+(?:all\\s+)?data\\s+leakage|guarantees?\\s+(?:answer\\s+)?quality)\\b"
    }
  ]
}
```
