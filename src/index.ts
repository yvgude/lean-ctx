#!/usr/bin/env node

import { McpServer } from '@modelcontextprotocol/sdk/server/mcp.js';
import { StdioServerTransport } from '@modelcontextprotocol/sdk/server/stdio.js';
import { SessionCache } from './core/session-cache.js';
import { registerCtxRead } from './tools/ctx-read.js';
import { registerCtxTree } from './tools/ctx-tree.js';
import { registerCtxShell } from './tools/ctx-shell.js';
import { registerCtxMetrics } from './tools/ctx-metrics.js';
import { registerCtxBenchmark } from './tools/ctx-benchmark.js';

const server = new McpServer({
  name: 'lean-ctx',
  version: '0.2.0',
});

const cache = new SessionCache();

registerCtxRead(server, cache);
registerCtxTree(server, cache);
registerCtxShell(server, cache);
registerCtxMetrics(server, cache);
registerCtxBenchmark(server, cache);

const transport = new StdioServerTransport();
await server.connect(transport);
