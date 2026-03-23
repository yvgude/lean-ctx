import { readFile } from 'node:fs/promises';
import { resolve } from 'node:path';
import { z } from 'zod';
import type { McpServer } from '@modelcontextprotocol/sdk/server/mcp.js';
import type { SessionCache } from '../core/session-cache.js';
import { Compressor } from '../core/compressor.js';
import { computeDiff, formatDiff } from '../core/differ.js';
import { extractSignatures } from '../core/signature-extractor.js';
import { trackSavings } from '../core/token-counter.js';
import { formatCacheHit, formatFileHeader, formatCompactSignature, shortenPath } from '../core/protocol.js';

const compressor = new Compressor();

export function registerCtxRead(server: McpServer, cache: SessionCache): void {
  server.tool(
    'ctx_read',
    `Smart file read with session caching and multiple modes.
Modes:
- "full" (default): Returns file content, cached on re-read if unchanged.
- "signatures": Returns only function signatures, interfaces, types, and exports. Ideal for dependency files where only the API surface is needed. Saves 60-80% tokens.
- "diff": If the file was previously read and has changed, returns only the diff. Saves 70-95% tokens on changed files.
- "aggressive": Full content but with aggressive syntax stripping (reduced indentation, stripped obvious types). Saves 20-40% extra.`,
    {
      path: z.string().describe('Absolute or relative file path to read'),
      mode: z
        .enum(['full', 'signatures', 'diff', 'aggressive'])
        .optional()
        .describe('Read mode: full (default), signatures (API only), diff (changes only), aggressive (stripped syntax)'),
      query: z
        .string()
        .optional()
        .describe('Optional relevance filter — only return sections matching this query'),
      force: z
        .boolean()
        .optional()
        .describe('Force re-read even if cached (default: false)'),
    },
    async ({ path: filePath, mode = 'full', query, force }) => {
      const absPath = resolve(filePath);

      if (!force) {
        const check = await cache.checkFile(absPath);

        if (check.cached && !check.changed && check.entry) {
          return handleCacheHit(absPath, check.entry, mode, query, cache);
        }

        if (check.cached && check.changed && check.entry && mode === 'diff') {
          return handleDiffMode(absPath, check.entry, cache);
        }
      }

      let content: string;
      try {
        content = await readFile(absPath, 'utf-8');
      } catch (err) {
        return errorResult(`Error reading file: ${err instanceof Error ? err.message : String(err)}`);
      }

      cache.store(absPath, content);

      if (mode === 'signatures') {
        return handleSignaturesMode(content, absPath);
      }

      if (mode === 'diff') {
        const lineCount = content.split('\n').length;
        return textResult(`First read of file (${lineCount} lines) — no previous version to diff against.\n\n${compressor.compressCode(content).output}`);
      }

      const aggressive = mode === 'aggressive';
      const compressed = compressor.compressCode(content, aggressive);

      let output = compressed.output;

      if (query) {
        const filtered = filterByQuery(output, query);
        if (filtered) {
          output = `Filtered for "${query}":\n\n${filtered}`;
        }
      }

      const meta = buildMeta(compressed.reductionPercent, content.split('\n').length, mode);
      return textResult(output + meta);
    }
  );
}

function handleCacheHit(
  absPath: string,
  entry: import('../core/session-cache.js').CacheEntry,
  mode: string,
  query: string | undefined,
  cache: SessionCache
): { content: { type: 'text'; text: string }[] } {
  const turnsAgo = cache.turnsAgo(entry);
  const lines = entry.lineCount;

  if (mode === 'signatures') {
    return handleSignaturesMode(entry.content, absPath);
  }

  const summary = formatCacheHit(absPath, turnsAgo, lines);
  const verbose = `File already in context (read ${turnsAgo} turns ago, ${lines} lines, unchanged).`;
  const savings = trackSavings(verbose, summary);

  if (query) {
    const filtered = filterByQuery(entry.content, query);
    if (filtered) {
      return textResult(`${summary}\n"${query}":\n${filtered}`);
    }
  }

  return textResult(`${summary} ${savings}`);
}

async function handleDiffMode(
  absPath: string,
  oldEntry: { content: string },
  cache: SessionCache
): Promise<{ content: { type: 'text'; text: string }[] }> {
  let newContent: string;
  try {
    newContent = await readFile(absPath, 'utf-8');
  } catch (err) {
    return errorResult(`Error reading file: ${err instanceof Error ? err.message : String(err)}`);
  }

  cache.store(absPath, newContent);

  const diff = computeDiff(oldEntry.content, newContent);

  if (diff.addedLines === 0 && diff.removedLines === 0) {
    return textResult('File unchanged (no diff).');
  }

  const formatted = formatDiff(diff);
  const fullFile = newContent;
  const tokSavings = trackSavings(fullFile, formatted);

  return textResult(`${formatted}\n${diff.addedLines}+ ${diff.removedLines}- ${tokSavings}`);
}

function handleSignaturesMode(
  content: string,
  absPath: string
): { content: { type: 'text'; text: string }[] } {
  const result = extractSignatures(content, absPath);

  const compactSigs = result.signatures
    .map(formatCompactSignature)
    .filter(Boolean)
    .join('\n');

  const header = formatFileHeader(absPath, result.originalLines, 'new');
  const compactOutput = `${header}\n${compactSigs}`;
  const tokSavings = trackSavings(content, compactOutput);

  return textResult(`${compactOutput}\n${tokSavings}`);
}

function buildMeta(reductionPercent: number, lineCount: number, _mode: string): string {
  if (reductionPercent <= 0) return '';
  return `\n[${reductionPercent}% ${lineCount}L]`;
}

function textResult(text: string) {
  return { content: [{ type: 'text' as const, text }] };
}

function errorResult(text: string) {
  return { content: [{ type: 'text' as const, text }], isError: true };
}

function filterByQuery(content: string, query: string): string | null {
  const queryTerms = query.toLowerCase().split(/\s+/);
  const lines = content.split('\n');
  const matchingRanges: [number, number][] = [];

  const CONTEXT_BEFORE = 3;
  const CONTEXT_AFTER = 5;

  for (let i = 0; i < lines.length; i++) {
    const lower = lines[i].toLowerCase();
    if (queryTerms.some((term) => lower.includes(term))) {
      const start = Math.max(0, i - CONTEXT_BEFORE);
      const end = Math.min(lines.length - 1, i + CONTEXT_AFTER);
      matchingRanges.push([start, end]);
    }
  }

  if (matchingRanges.length === 0) return null;

  const merged = mergeRanges(matchingRanges);
  const sections: string[] = [];

  for (const [start, end] of merged) {
    const section = lines.slice(start, end + 1);
    sections.push(`[lines ${start + 1}-${end + 1}]\n${section.join('\n')}`);
  }

  return sections.join('\n\n...\n\n');
}

function mergeRanges(ranges: [number, number][]): [number, number][] {
  if (ranges.length === 0) return [];

  const sorted = [...ranges].sort((a, b) => a[0] - b[0]);
  const merged: [number, number][] = [sorted[0]];

  for (let i = 1; i < sorted.length; i++) {
    const last = merged[merged.length - 1];
    if (sorted[i][0] <= last[1] + 1) {
      last[1] = Math.max(last[1], sorted[i][1]);
    } else {
      merged.push(sorted[i]);
    }
  }

  return merged;
}
