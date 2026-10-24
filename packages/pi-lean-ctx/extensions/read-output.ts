// SPDX-License-Identifier: Apache-2.0
// Output bound for successful ctx_read results (#2138).
//
// Kept free of Pi imports so it is unit-testable on its own (importing Pi's
// runtime drags in an undici that cannot load under vitest).
//
// The MCP path is already capped by the engine's turn budget, but the one-shot
// `lean-ctx read` CLI has no such cap, so a CLI-only Pi session
// (LEAN_CTX_PI_ENABLE_MCP=0) handed a 4 MB `mode=full` read to the model in
// one result. This bound applies to every ctx_read text result regardless of
// path, and leaves anything within it byte-identical.

/** Same line limit as Pi's own read tool (`DEFAULT_MAX_LINES`). */
export const READ_OUTPUT_MAX_LINES = 2000;
/** Same byte limit as Pi's own read tool (`DEFAULT_MAX_BYTES`, 50 KB). */
export const READ_OUTPUT_MAX_BYTES = 50 * 1024;

/** Field-compatible with Pi's `TruncationResult`, minus `content`. */
export interface ReadTruncation {
  truncated: true;
  truncatedBy: "lines" | "bytes";
  totalLines: number;
  totalBytes: number;
  outputLines: number;
  outputBytes: number;
  lastLinePartial: boolean;
  firstLineExceedsLimit: boolean;
  maxLines: number;
  maxBytes: number;
}

export interface BoundReadOptions {
  /**
   * The file's text, for outputs without line numbers (`mode=full`/`raw`).
   * When the kept lines are exactly the file's first lines, the hint names
   * the exact line to continue from; otherwise (compressed or reshaped
   * output) it stays generic rather than guess.
   */
  sourceText?: string;
  maxLines?: number;
  maxBytes?: number;
}

export interface BoundReadResult {
  text: string;
  truncation?: ReadTruncation;
}

/** Human-readable size in the style of Pi's `formatSize`. */
export function formatSize(bytes: number): string {
  if (bytes < 1024) return `${bytes}B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)}KB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)}MB`;
}

/** Longest prefix of `line` within `maxBytes` UTF-8 bytes, cut on a character boundary. */
function utf8Prefix(line: string, maxBytes: number): string {
  const buf = Buffer.from(line, "utf8");
  if (buf.length <= maxBytes) return line;
  let end = maxBytes;
  while (end > 0 && (buf[end] & 0xc0) === 0x80) end--;
  return buf.subarray(0, end).toString("utf8");
}

// `lines:N-M` output numbers each line as `   N| text`.
const NUMBERED_LINE = /^\s*(\d+)\|/;

/** Whether `kept` are exactly the first lines of `source` (CRLF-insensitive). */
function isSourcePrefix(kept: string[], source: string | undefined): boolean {
  if (source === undefined) return false;
  let pos = 0;
  for (const line of kept) {
    if (!source.startsWith(line, pos)) return false;
    pos += line.length;
    if (source.startsWith("\r\n", pos)) pos += 2;
    else if (source.startsWith("\n", pos)) pos += 1;
    else return false;
  }
  return true;
}

/**
 * Cap a ctx_read result at Pi's read limits, keeping whole lines from the
 * head and appending a notice with the original size and a continuation.
 */
export function boundReadOutput(text: string, opts: BoundReadOptions = {}): BoundReadResult {
  const maxLines = opts.maxLines ?? READ_OUTPUT_MAX_LINES;
  const maxBytes = opts.maxBytes ?? READ_OUTPUT_MAX_BYTES;
  const totalBytes = Buffer.byteLength(text, "utf8");
  const lines = text.split("\n");
  const totalLines = lines.length;
  if (totalLines <= maxLines && totalBytes <= maxBytes) return { text };

  const kept: string[] = [];
  let outputBytes = 0;
  let truncatedBy: "lines" | "bytes" = "lines";
  for (const line of lines) {
    if (kept.length >= maxLines) break;
    const cost = Buffer.byteLength(line, "utf8") + (kept.length > 0 ? 1 : 0);
    if (outputBytes + cost > maxBytes) {
      truncatedBy = "bytes";
      break;
    }
    kept.push(line);
    outputBytes += cost;
  }

  const firstLineExceedsLimit = kept.length === 0;
  let body: string;
  let continuation: string;
  if (firstLineExceedsLimit) {
    // A single line (e.g. minified JSON) is over the limit on its own. Pi's
    // read tool returns nothing here; a prefix is more useful, and a line
    // window cannot help, so point at the structural modes instead.
    body = utf8Prefix(lines[0], maxBytes);
    outputBytes = Buffer.byteLength(body, "utf8");
    continuation =
      'The first line alone exceeds the limit; use ctx_grep on this path or ctx_read with mode="map" or mode="signatures" for a smaller view.';
  } else {
    body = kept.join("\n");
    const lastNumbered = kept[kept.length - 1].match(NUMBERED_LINE);
    const next = lastNumbered
      ? Number(lastNumbered[1]) + 1
      : isSourcePrefix(kept, opts.sourceText)
        ? kept.length + 1
        : undefined;
    continuation =
      next !== undefined
        ? `Continue with ctx_read(path, start_line=${next}, limit=${maxLines}) or mode="lines:${next}-${next + maxLines - 1}".`
        : 'Narrow the read with ctx_read(path, start_line=N, limit=M) or mode="lines:N-M".';
  }

  const outputLines = firstLineExceedsLimit ? 0 : kept.length;
  const notice =
    `[ctx_read output truncated: showing ${outputLines} of ${totalLines} lines, `
    + `${formatSize(outputBytes)} of ${formatSize(totalBytes)} `
    + `(limit ${maxLines} lines / ${formatSize(maxBytes)}). ${continuation}]`;

  return {
    text: `${body}\n\n${notice}`,
    truncation: {
      truncated: true,
      truncatedBy: firstLineExceedsLimit ? "bytes" : truncatedBy,
      totalLines,
      totalBytes,
      outputLines,
      outputBytes,
      lastLinePartial: firstLineExceedsLimit,
      firstLineExceedsLimit,
      maxLines,
      maxBytes,
    },
  };
}
