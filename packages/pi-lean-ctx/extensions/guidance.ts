// SPDX-License-Identifier: Apache-2.0
// Default prompt guidance that steers the Pi agent toward the ctx_* tools.
//
// Additive by design: nothing here removes or overrides a Pi builtin. The text
// is a fixed function of which tool pairs are active (no timestamps, no
// counters), so the system prompt and the session note stay byte-stable for
// provider prompt caching.

import { closeSync, openSync, readSync } from "node:fs";
import { isAbsolute, join } from "node:path";

/** customType of the one-time session note; also the dedupe key on resume. */
export const GUIDANCE_CUSTOM_TYPE = "pi-lean-ctx-guidance";

/** Each ctx_* tool with the Pi builtin it stands in for, in display order. */
export const CTX_BUILTIN_PAIRS = [
  ["ctx_read", "read"],
  ["ctx_grep", "grep"],
  ["ctx_find", "find"],
  ["ctx_ls", "ls"],
  ["ctx_shell", "bash"],
] as const;

export type CtxPairedTool = (typeof CTX_BUILTIN_PAIRS)[number][0];

const PREFER_GUIDELINES: Record<CtxPairedTool, string> = {
  ctx_read: "Prefer ctx_read over the built-in read tool: it is cached and compressed, and an unchanged re-read costs a few tokens.",
  ctx_grep: "Prefer ctx_grep over the built-in grep tool for code search: results are ranked and compact.",
  ctx_find: "Prefer ctx_find over the built-in find tool: it respects .gitignore and returns a compact list.",
  ctx_ls: "Prefer ctx_ls over the built-in ls tool: it returns a compact directory summary.",
  ctx_shell: "Prefer ctx_shell over the built-in bash tool for builds, tests and git: its output is compressed.",
};

function builtinFor(tool: CtxPairedTool): string {
  return CTX_BUILTIN_PAIRS.find(([ctx]) => ctx === tool)![1];
}

/**
 * The "prefer ctx_X over builtin" bullet for a tool's `promptGuidelines`, or
 * nothing when guidance is off or the builtin is suppressed (then there is no
 * choice to steer, and naming a missing tool would only confuse the model).
 */
export function preferGuidelines(
  tool: CtxPairedTool,
  enabled: boolean,
  suppressedBuiltins: ReadonlySet<string>,
): string[] {
  if (!enabled || suppressedBuiltins.has(builtinFor(tool))) return [];
  return [PREFER_GUIDELINES[tool]];
}

/**
 * The session-start note: names only the pairs where both the ctx_* tool and
 * the builtin are active. Returns undefined when no such pair exists.
 */
export function guidanceNote(activeTools: Iterable<string>): string | undefined {
  const active = new Set(activeTools);
  const pairs = CTX_BUILTIN_PAIRS.filter(([ctx, builtin]) => active.has(ctx) && active.has(builtin));
  if (pairs.length === 0) return undefined;
  const ctxNames = pairs.map(([ctx]) => ctx).join(", ");
  const builtinNames = pairs.map(([, builtin]) => builtin).join(", ");
  // Same direction as the canonical rules block ("built-in tools map to
  // ctx_*"): name the preferred tools, never claim the builtins are the
  // fallback or that ctx_* replaces them.
  return (
    `lean-ctx is active. Prefer ${ctxNames} over the built-in ${builtinNames} tools: `
    + "their output is cached and compressed."
  );
}

/** Prefix of every lean-ctx rules block marker (`<!-- lean-ctx-rules -->`, legacy `-vN`). */
export const RULES_MARKER_PREFIX = "<!-- lean-ctx-rules";

/**
 * `<agent-dir>/AGENTS.md`, the global instruction file Pi loads. Mirrors the
 * engine's `pi_agent_dir`: `PI_CODING_AGENT_DIR` wins (a leading `~` or `~/`
 * is expanded, and `~\` on Windows, as Pi's own `normalizePath` does; a blank
 * or relative value falls back to `~/.pi/agent`). `platform` is a parameter
 * so the Windows rule is tested on every OS.
 */
export function piAgentsPath(
  env: NodeJS.ProcessEnv,
  home: string,
  platform: NodeJS.Platform = process.platform,
): string {
  const fallback = join(home, ".pi", "agent");
  const explicit = env.PI_CODING_AGENT_DIR?.trim();
  if (!explicit) return join(fallback, "AGENTS.md");
  const homeRelative = explicit.startsWith("~/") || (platform === "win32" && explicit.startsWith("~\\"));
  const expanded = explicit === "~"
    ? home
    : homeRelative ? join(home, explicit.slice(2)) : explicit;
  return join(isAbsolute(expanded) ? expanded : fallback, "AGENTS.md");
}

/** Only this prefix of AGENTS.md is scanned, so a huge file costs at most 1 MiB. */
export const AGENTS_READ_CAP_BYTES = 1024 * 1024;

/**
 * True when Pi's global AGENTS.md carries the lean-ctx rules block (written by
 * `lean-ctx setup`). That block is Pi's one rules channel, so the session note
 * is skipped then. An unreadable or missing file counts as absent.
 */
export function agentsRulesBlockPresent(env: NodeJS.ProcessEnv, home: string): boolean {
  let fd: number | undefined;
  try {
    fd = openSync(piAgentsPath(env, home), "r");
    const buf = Buffer.alloc(AGENTS_READ_CAP_BYTES);
    let len = 0;
    while (len < buf.length) {
      const n = readSync(fd, buf, len, buf.length - len, null);
      if (n === 0) break;
      len += n;
    }
    return buf.subarray(0, len).includes(RULES_MARKER_PREFIX);
  } catch {
    return false;
  } finally {
    if (fd !== undefined) closeSync(fd);
  }
}

/** True when the session already carries the note (resume, fork, reload). */
export function hasGuidanceNote(entries: readonly unknown[]): boolean {
  return entries.some(
    (entry) =>
      typeof entry === "object"
      && entry !== null
      && (entry as { type?: unknown }).type === "custom_message"
      && (entry as { customType?: unknown }).customType === GUIDANCE_CUSTOM_TYPE,
  );
}

// ── Skills section in replace mode (#2260) ──────────────────────────────────
// Pi renders its <skills> prompt section only when a reader it knows (`read`
// or `bash`) is selected. Replace mode suppresses both, so the model was never
// told any skill exists. When ctx_read is the only reader left, lean-ctx puts
// the section back with the same layout, naming ctx_read as the loader.

/** The Pi builtins whose presence makes Pi render <skills> itself. */
const PI_SKILL_READERS = ["read", "bash"] as const;

/** The fields of a Pi `Skill` the prompt section needs. */
export interface PromptSkill {
  name: string;
  description: string;
  filePath: string;
  disableModelInvocation?: boolean;
}

/** The fields of Pi's `systemPromptOptions` the skills fallback reads. */
export interface SkillsPromptOptions {
  selectedTools?: readonly string[];
  hiddenTools?: readonly string[];
  sections?: Readonly<Record<string, string>>;
  skills?: readonly PromptSkill[];
}

function escapeXml(value: string): string {
  return value
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;")
    .replace(/'/g, "&apos;");
}

/**
 * The <skills> section body to add to Pi's system prompt, or undefined when
 * Pi renders it itself (a `read`/`bash` builtin is selected, even hidden), an
 * extension already set one, ctx_read is not declared, or no skill may be
 * invoked by the model. Mirrors Pi's `formatSkillsForPrompt` layout so the
 * section reads the same as in additive mode; deterministic for caching.
 */
export function skillsSectionFallback(options: SkillsPromptOptions): string | undefined {
  const selected = options.selectedTools ?? [];
  const hidden = new Set(options.hiddenTools ?? []);
  if (PI_SKILL_READERS.some((tool) => selected.includes(tool))) return undefined;
  if (!selected.includes("ctx_read") || hidden.has("ctx_read")) return undefined;
  if (options.sections?.skills) return undefined;
  const skills = (options.skills ?? []).filter((skill) => !skill.disableModelInvocation);
  if (skills.length === 0) return undefined;
  const lines = [
    "The following skills provide specialized instructions for specific tasks.",
    "Use the ctx_read tool to load a skill's file when the task matches its description.",
    "Resolve relative paths against the skill's directory (parent of SKILL.md). Use absolute paths in tool calls.",
    "",
    "<available_skills>",
  ];
  for (const skill of skills) {
    lines.push("  <skill>");
    lines.push(`    <name>${escapeXml(skill.name)}</name>`);
    lines.push(`    <description>${escapeXml(skill.description)}</description>`);
    lines.push(`    <location>${escapeXml(skill.filePath)}</location>`);
    lines.push("  </skill>");
  }
  lines.push("</available_skills>");
  return lines.join("\n");
}
