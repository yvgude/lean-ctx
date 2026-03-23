import { z } from 'zod';
import type { McpServer } from '@modelcontextprotocol/sdk/server/mcp.js';
import type { SessionCache } from '../core/session-cache.js';
import { getSessionStats } from '../core/token-counter.js';
import { getAllRefs } from '../core/protocol.js';

export function registerCtxMetrics(server: McpServer, cache: SessionCache): void {
  server.tool(
    'ctx_metrics',
    'Token savings statistics for this session. Shows real token counts (via tiktoken), cache stats, and file references.',
    {},
    async () => {
      const cacheStats = cache.getStats();
      const tokenStats = getSessionStats();
      const refs = getAllRefs();

      const hitRate = cacheStats.totalReads > 0
        ? Math.round((cacheStats.cacheHits / cacheStats.totalReads) * 100)
        : 0;

      const lines = [
        'lean-ctx session',
        `files: ${cacheStats.filesTracked} tracked, ${cacheStats.totalReads} reads, ${cacheStats.cacheHits} hits (${hitRate}%)`,
        `tokens: ${tokenStats.totalOriginal} original → ${tokenStats.totalOriginal - tokenStats.totalSaved} sent (−${tokenStats.totalSaved} saved, ${tokenStats.percent}%)`,
      ];

      if (refs.size > 0) {
        lines.push(`refs: ${Array.from(refs.entries()).map(([path, ref]) => `${ref}=${path.split('/').pop()}`).join(' ')}`);
      }

      return {
        content: [{ type: 'text' as const, text: lines.join('\n') }],
      };
    }
  );
}
