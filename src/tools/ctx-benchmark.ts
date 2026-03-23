import { readFile } from 'node:fs/promises';
import { resolve } from 'node:path';
import { z } from 'zod';
import type { McpServer } from '@modelcontextprotocol/sdk/server/mcp.js';
import type { SessionCache } from '../core/session-cache.js';
import { countTokens } from '../core/token-counter.js';
import { Compressor } from '../core/compressor.js';
import { extractSignatures } from '../core/signature-extractor.js';
import { formatCompactSignature, formatFileHeader } from '../core/protocol.js';

export function registerCtxBenchmark(server: McpServer, cache: SessionCache): void {
  server.tool(
    'ctx_benchmark',
    'Benchmark all compression strategies on a file. Shows exact token counts for each strategy to find the most efficient approach.',
    {
      path: z.string().describe('File path to benchmark'),
    },
    async ({ path: filePath }) => {
      const absPath = resolve(filePath);

      let content: string;
      try {
        content = await readFile(absPath, 'utf-8');
      } catch (err) {
        return {
          content: [{ type: 'text' as const, text: `Error: ${err instanceof Error ? err.message : String(err)}` }],
          isError: true,
        };
      }

      const compressor = new Compressor();
      const lines = content.split('\n').length;
      const fileName = absPath.split('/').pop() || absPath;

      const strategies: { name: string; output: string }[] = [];

      strategies.push({ name: 'raw', output: content });

      const compressed = compressor.compressCode(content, false);
      strategies.push({ name: 'full (default)', output: compressed.output });

      const aggressive = compressor.compressCode(content, true);
      strategies.push({ name: 'aggressive', output: aggressive.output });

      const sigResult = extractSignatures(content, absPath);
      strategies.push({ name: 'signatures (verbose)', output: sigResult.formatted });

      const compactSigs = sigResult.signatures
        .map(formatCompactSignature)
        .filter(Boolean)
        .join('\n');
      const compactHeader = formatFileHeader(absPath, lines, 'new');
      strategies.push({ name: 'signatures (compact)', output: `${compactHeader}\n${compactSigs}` });

      strategies.push({ name: 'cache hit', output: `F1 [cached 1t ${lines}L ∅]` });

      const results = strategies.map((s) => {
        const tokens = countTokens(s.output);
        return { ...s, tokens };
      });

      const rawTokens = results[0].tokens;

      const output = [
        `Benchmark: ${fileName} (${lines}L)`,
        '',
        'Strategy             Tokens  Savings',
        '─'.repeat(42),
      ];

      for (const r of results) {
        const saved = rawTokens - r.tokens;
        const pct = rawTokens > 0 ? Math.round((saved / rawTokens) * 100) : 0;
        const bar = pct > 0 ? `−${saved} (${pct}%)` : '—';
        output.push(`${r.name.padEnd(21)}${String(r.tokens).padStart(6)}  ${bar}`);
      }

      output.push('─'.repeat(42));

      const best = results.reduce((a, b) => (a.tokens < b.tokens ? a : b));
      const bestSaved = rawTokens - best.tokens;
      const bestPct = rawTokens > 0 ? Math.round((bestSaved / rawTokens) * 100) : 0;
      output.push(`Best: "${best.name}" saves ${bestSaved} tokens (${bestPct}%)`);

      return {
        content: [{ type: 'text' as const, text: output.join('\n') }],
      };
    }
  );
}
