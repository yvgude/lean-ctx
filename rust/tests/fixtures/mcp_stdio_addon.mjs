#!/usr/bin/env node
// MCP stdio fixture for the deeper-addon-integration E2E (#1102).
//
// A *real* MCP stdio server (newline-delimited JSON-RPC 2.0) whose tools return
// the output shapes the L1–L4 pipeline consumes, so the gateway's production
// proxy → postprocess → adapters path is exercised end to end against a spawned
// child process (no protocol mocks). Tools:
//   echo            -> `echo:<text>` (drives L1/L2/L3 + secret redaction)
//   pack_codebase   -> repomix-shaped JSON {outputId, directoryStructure, …}
//   query_graph     -> code-graph JSON {edges:[{from,to,type}]}
//   search_memories -> memory JSON {results:[{memory,id,score}]}
//   compress_text   -> `compressed:<text>` (drives the compression adapter)
// Only Node.js is required.

import { createInterface } from 'node:readline';

const rl = createInterface({ input: process.stdin });
const send = (msg) => process.stdout.write(JSON.stringify(msg) + '\n');
const textResult = (id, text) =>
  send({ jsonrpc: '2.0', id, result: { content: [{ type: 'text', text }], isError: false } });

const TOOLS = [
  { name: 'echo', description: 'Echo back the provided text',
    inputSchema: { type: 'object', properties: { text: { type: 'string' } }, required: ['text'] } },
  { name: 'pack_codebase', description: 'Pack a repository (repomix-shaped)',
    inputSchema: { type: 'object', properties: { directory: { type: 'string' } } } },
  { name: 'query_graph', description: 'Return code-graph edges',
    inputSchema: { type: 'object', properties: { q: { type: 'string' } } } },
  { name: 'search_memories', description: 'Search stored memories',
    inputSchema: { type: 'object', properties: { query: { type: 'string' } } } },
  { name: 'compress_text', description: 'Compress text',
    inputSchema: { type: 'object', properties: { text: { type: 'string' } }, required: ['text'] } },
];

function callTool(id, name, args) {
  switch (name) {
    case 'echo':
      return textResult(id, 'echo:' + (args.text ?? ''));
    case 'compress_text':
      return textResult(id, 'compressed:' + (args.text ?? ''));
    case 'pack_codebase':
      return textResult(id, JSON.stringify({
        outputId: 'rmx_e2e_001',
        directoryStructure: 'src/\n  auth.rs\n  db.rs\n',
        totalFiles: 2,
        totalTokens: 4321,
      }));
    case 'query_graph':
      return textResult(id, JSON.stringify({
        edges: [
          { from: 'src/auth.rs', to: 'src/db.rs', type: 'calls' },
          { from: 'src/api.rs', to: 'src/auth.rs', type: 'imports' },
        ],
      }));
    case 'search_memories':
      return textResult(id, JSON.stringify({
        results: [
          { memory: 'the user prefers structured logging', id: 'mem-e2e-1', score: 0.88 },
        ],
      }));
    default:
      return send({ jsonrpc: '2.0', id, error: { code: -32602, message: 'unknown tool: ' + name } });
  }
}

rl.on('line', (line) => {
  const trimmed = line.trim();
  if (!trimmed) return;
  let req;
  try { req = JSON.parse(trimmed); } catch { return; }
  const { id, method, params } = req;

  if (method === 'initialize') {
    send({ jsonrpc: '2.0', id, result: {
      protocolVersion: (params && params.protocolVersion) || '2025-06-18',
      capabilities: { tools: {} },
      serverInfo: { name: 'mcp-stdio-addon', version: '0.1.0' },
    } });
  } else if (method === 'notifications/initialized') {
    // no response
  } else if (method === 'tools/list') {
    send({ jsonrpc: '2.0', id, result: { tools: TOOLS } });
  } else if (method === 'tools/call') {
    callTool(id, params && params.name, (params && params.arguments) || {});
  } else if (id !== undefined && id !== null) {
    send({ jsonrpc: '2.0', id, error: { code: -32601, message: 'method not found: ' + method } });
  }
});

rl.on('close', () => process.exit(0));
