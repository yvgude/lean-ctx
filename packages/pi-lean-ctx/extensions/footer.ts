import type { CompressionStats } from "./types.js";

/**
 * Pure footer/marker logic for the Pi extension (#1578).
 *
 * Lives in its own module (no Pi-runtime imports) so it can be unit-tested:
 * importing `index.ts` drags in `@earendil-works/pi-coding-agent`, whose
 * nested undici cannot load under vitest's Node environment.
 */

export function estimateTokens(text: string) {
  return Math.ceil(text.length / 4);
}

/**
 * Parses a CLI token count that may be abbreviated the way the savings banner
 * abbreviates: "917", "1,474", "17.0k", "1.2M" (#1578).
 */
function parseTokenCount(raw: string): number {
  const cleaned = raw.replace(/,/g, "");
  const scale = cleaned.endsWith("M") ? 1_000_000 : cleaned.endsWith("k") ? 1_000 : 1;
  const digits = scale === 1 ? cleaned : cleaned.slice(0, -1);
  const value = Number.parseFloat(digits);
  return Number.isFinite(value) && value > 0 ? Math.round(value * scale) : 0;
}

function clampStats(original: number, compressed: number): CompressionStats {
  const orig = Math.max(0, original);
  const comp = Math.max(0, Math.min(orig, compressed));
  const saved = Math.max(0, orig - comp);
  const percentSaved = orig > 0 ? Math.round((saved / orig) * 100) : 0;
  return { originalTokens: orig, compressedTokens: comp, percentSaved };
}

export function parseLeanCtxOutput(text: string) {
  const lines = text.replace(/\r\n/g, "\n").split("\n");
  let stats: CompressionStats | undefined;
  const kept: string[] = [];

  for (const line of lines) {
    const trimmed = line.trim();

    // CLI marker formats (#1578). The Rust CLI emits several footer shapes:
    //   ─── 917 → 239 tok (↓~70%) ───              savings banner (numbers may
    //     be abbreviated to 17.0k / 1.2M, percent exact or quantized, optional
    //     "| mode: … | …" parts)
    //   [lean-ctx: 17001→1474 tok, verbatim truncated]  reason bracket
    //   [lean-ctx: 12 tok saved (3%)] / [12 tok saved]  saved brackets
    // Parse them all; the pre-#1578 regexes matched none of these, so stats
    // stayed undefined and the fabricated fallback printed "N → N (0%)".
    const bannerMatch = trimmed.match(
      /^─+\s*([\d.,]+[kM]?)\s*→\s*([\d.,]+[kM]?)\s*tok\s*\(↓~?\d+%\)(?:\s*\|[^─]*)?\s*─+\s*$/,
    );
    if (bannerMatch) {
      stats = clampStats(parseTokenCount(bannerMatch[1]), parseTokenCount(bannerMatch[2]));
      continue;
    }

    const shellMatch = trimmed.match(
      /^\[lean-ctx:\s*([\d.,]+[kM]?)\s*→\s*([\d.,]+[kM]?)\s*tok(?:,\s*[^\]]*)?\]$/,
    );
    if (shellMatch) {
      stats = clampStats(parseTokenCount(shellMatch[1]), parseTokenCount(shellMatch[2]));
      continue;
    }

    const savedMatch = trimmed.match(/^\[(?:lean-ctx:\s*)?(\d+)\s+tok saved(?:\s+\((\d+)%\))?\]$/);
    if (savedMatch) {
      const saved = Number(savedMatch[1]);
      const pct = savedMatch[2] ? Number(savedMatch[2]) : 0;
      if (pct > 0) {
        const original = Math.round((saved * 100) / pct);
        stats = clampStats(original, Math.max(0, original - saved));
      } else {
        stats = clampStats(saved, saved);
      }
      continue;
    }

    kept.push(line);
  }

  return { text: kept.join("\n").replace(/\n{3,}/g, "\n\n").trimEnd(), stats };
}

function formatFooter(stats: CompressionStats) {
  const pct = stats.percentSaved > 0 ? `-${stats.percentSaved}%` : "0%";
  return `Compressed ${stats.originalTokens} → ${stats.compressedTokens} tokens (${pct})`;
}

export function withFooter(text: string, opts?: {
  originalText?: string;
  limit?: number;
  always?: boolean;
  preferEstimate?: boolean;
  suppressIfNoSaving?: boolean;
}) {
  const parsed = parseLeanCtxOutput(text);
  const limited = limitLines(parsed.text, opts?.limit);

  let stats = parsed.stats;
  if (opts?.originalText !== undefined && (opts.preferEstimate || !stats)) {
    stats = clampStats(estimateTokens(opts.originalText), estimateTokens(limited.text));
  }
  // #1578: never fabricate stats. The old `always` fallback measured the
  // already-compressed text against itself, so it could only ever print
  // "N → N tokens (0%)". No parsed marker and no original text ⇒ no footer.
  if (!stats) return { text: limited.text, stats: undefined, truncated: limited.truncated };

  // On tiny files compression cannot beat the envelope, so a "0%" footer would
  // be pure overhead — larger payload than the source for no gain (#361). Keep
  // the computed stats for telemetry (`details.compression`) but drop the
  // visible footer when nothing was actually saved.
  if (opts?.suppressIfNoSaving && stats.percentSaved <= 0) {
    return { text: limited.text, stats, truncated: limited.truncated };
  }

  const footer = formatFooter(stats);
  const base = limited.text.trimEnd();
  return {
    text: base ? `${base}\n\n${footer}` : footer,
    stats,
    truncated: limited.truncated,
  };
}

function limitLines(text: string, limit?: number) {
  if (!limit || limit <= 0) return { text, truncated: false };
  const lines = text.split("\n");
  if (lines.length <= limit) return { text, truncated: false };
  return {
    text: lines.slice(0, limit).join("\n") + `\n\n[Output truncated to ${limit} lines]`,
    truncated: true,
  };
}
