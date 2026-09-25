// Parsing and presentation of `lean-ctx prompt-segment --json` (schema 1).
// Pure: no `vscode` import, so it runs under `node --test`.

export interface SpeedLine {
  phrase: string;
  detail: string;
}

export interface ValuePayload {
  schema: 1;
  display: string;
  /** null when nothing fresh was measured — the item hides, never shows 0. */
  segment: string | null;
  tooltip: string[];
  speed: SpeedLine | null;
  /** Directory whose files change when the numbers change. */
  watch: string | null;
  verify: string;
}

const isString = (v: unknown): v is string => typeof v === "string";

/** The payload, or null for anything that is not schema 1 (an older binary, a crash). */
export function parsePayload(stdout: string): ValuePayload | null {
  let raw: unknown;
  try {
    raw = JSON.parse(stdout);
  } catch {
    return null;
  }
  if (typeof raw !== "object" || raw === null) return null;
  const o = raw as Record<string, unknown>;
  if (o.schema !== 1) return null;
  const speed = o.speed as Record<string, unknown> | null | undefined;
  return {
    schema: 1,
    display: isString(o.display) ? o.display : "minimal",
    segment: isString(o.segment) && o.segment.length > 0 ? o.segment : null,
    tooltip: Array.isArray(o.tooltip) ? o.tooltip.filter(isString) : [],
    speed:
      speed && isString(speed.phrase) && isString(speed.detail)
        ? { phrase: speed.phrase, detail: speed.detail }
        : null,
    watch: isString(o.watch) ? o.watch : null,
    verify: isString(o.verify) ? o.verify : "lean-ctx value",
  };
}

/**
 * Status bar text. `$(` starts a codicon in VS Code status bar text, so it is
 * defused even though the binary never emits it.
 */
export function statusText(p: ValuePayload): string | null {
  if (p.display === "off" || p.segment === null) return null;
  return p.segment.replace(/\$\(/g, "$​(");
}

/** Escapes Markdown so a tooltip line renders literally. */
export function escapeMarkdown(s: string): string {
  return s.replace(/[\\`*_{}[\]()#+\-.!|<>~]/g, (c) => `\\${c}`);
}

/** Tooltip Markdown: the labelled breakdown, the speed proof if any, how to verify. */
export function tooltipMarkdown(p: ValuePayload): string {
  const lines = ["**lean-ctx** · this project", ""];
  for (const line of p.tooltip) lines.push(`${escapeMarkdown(line)}  `);
  if (p.speed) {
    lines.push("", `⚡ ${escapeMarkdown(p.speed.phrase)}  `, `_${escapeMarkdown(p.speed.detail)}_`);
  }
  lines.push(
    "",
    "✓ counted from the ledger and audit trail · ≈ derived from counted values",
    "",
    `Verify: \`${p.verify.replace(/`/g, "")}\``,
  );
  return lines.join("\n");
}
