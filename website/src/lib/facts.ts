// ─────────────────────────────────────────────────────────────────────────────
// FACTS SSOT — the single source of truth for every product number on the site.
//
// Why this file exists: counts (tools, languages, roles, integration modes …)
// were duplicated across dozens of pages and drifted apart (e.g. "12" vs "18"
// vs "21" languages, "Two" vs "Three" integration modes, "MIT + Apache-2.0" vs
// "Apache-2.0"). Every page and the facts linter (scripts/validate-facts.mjs)
// must read from here so a number can only ever be changed in one place.
//
// Dynamic counts (tools, read modes) come from the generated MCP manifest so
// they can never drift from the binary. Static counts are verified against the
// Rust source and annotated with their provenance.
// ─────────────────────────────────────────────────────────────────────────────
import { getMcpManifest } from './mcpManifest';

export interface Facts {
  /** Granular MCP tool count — from generated/mcp-tools.json (counts.granular). */
  mcpToolCount: number;
  /** Unified (lazy-set) MCP tool count — from generated/mcp-tools.json. */
  unifiedToolCount: number;
  /** Read modes — from generated/mcp-tools.json (read_modes.count). */
  readModeCount: number;
  /** Tree-sitter languages — verified: rust/Cargo.toml tree-sitter-* deps = 18. */
  languageCount: number;
  /** Built-in roles — verified: rust/src/core/roles.rs (admin, coder, debugger, reviewer, ops). */
  roleCount: number;
  roles: string[];
  /** Integration modes — verified: CLI-Redirect, Hybrid, Full MCP. */
  integrationModeCount: number;
  integrationModes: string[];
  /** Shell patterns — the binary's own manifest (generated/mcp-tools.json, ctx_shell) says "95+". */
  shellPatterns: string;
  /** SPDX license — verified: rust/Cargo.toml `license = "Apache-2.0"`. */
  license: string;
  /** Current release — verified: rust/Cargo.toml + packages/lean-ctx-bin/package.json. */
  version: string;
  /** Canonical token-savings phrasing (use verbatim, never invent new ranges). */
  tokenSavingsPerRead: string;
  tokenSavingsCache: string;
  tokenSavingsReread: string;
}

/**
 * The two distinct axes people confuse:
 *  - HELP MECHANISMS = how lean-ctx helps your AI (MCP tools + shell hooks) → 2
 *  - INTEGRATION MODES = how you wire it into an editor (CLI-Redirect/Hybrid/Full MCP) → 3
 * Keep them separate; never collapse "2" and "3" into one contradictory claim.
 */
export const HELP_MECHANISMS = ['MCP tools', 'Shell hooks'] as const;

const STATIC_FACTS = {
  languageCount: 18,
  roleCount: 5,
  roles: ['admin', 'coder', 'debugger', 'reviewer', 'ops'],
  integrationModeCount: 3,
  integrationModes: ['CLI-Redirect', 'Hybrid', 'Full MCP'],
  shellPatterns: '95+',
  license: 'Apache-2.0',
  version: '3.7.0',
  tokenSavingsPerRead: '60–90% per read',
  tokenSavingsCache: 'up to 99% from cache',
  tokenSavingsReread: '~13 tokens on re-read',
} as const;

let cached: Facts | null = null;

/** Returns the resolved facts, merging dynamic manifest counts with verified static facts. */
export function getFacts(): Facts {
  if (cached) return cached;
  const m = getMcpManifest();
  cached = {
    mcpToolCount: m.counts.granular,
    unifiedToolCount: m.counts.unified,
    readModeCount: m.read_modes.count,
    ...STATIC_FACTS,
    roles: [...STATIC_FACTS.roles],
    integrationModes: [...STATIC_FACTS.integrationModes],
  };
  return cached;
}

/**
 * i18n replacement map: every `{factKey}` placeholder a translation string may
 * contain. Pages pass this to the translator so copy renders live numbers.
 */
export function getFactReplacements(): Record<string, string> {
  const f = getFacts();
  return {
    mcpToolCount: String(f.mcpToolCount),
    unifiedToolCount: String(f.unifiedToolCount),
    readModeCount: String(f.readModeCount),
    languageCount: String(f.languageCount),
    roleCount: String(f.roleCount),
    integrationModeCount: String(f.integrationModeCount),
    shellPatterns: f.shellPatterns,
    license: f.license,
    version: f.version,
  };
}
