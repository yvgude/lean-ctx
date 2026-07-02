// Entry point for the vendored MCP SDK bundle (see build-vendor.mjs).
// Re-exports exactly the SDK surface mcp-bridge.ts consumes, so esbuild
// tree-shakes the server/HTTP/OAuth halves of the SDK out of the bundle.
export { Client } from "@modelcontextprotocol/sdk/client/index.js";
export { StdioClientTransport } from "@modelcontextprotocol/sdk/client/stdio.js";
