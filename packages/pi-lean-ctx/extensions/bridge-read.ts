// SPDX-License-Identifier: Apache-2.0
// Bridge-backed ctx_read without console output (#2256).
//
// Kept free of Pi imports so it is unit-testable on its own (importing Pi's
// runtime drags in an undici that cannot load under vitest).
//
// Pi draws its prompt editor in the same terminal, so a `console.error` from a
// tool call lands inside the editor. A failed bridge call is not the user's
// business: the read falls back to the CLI, and the reason is kept for
// `/lean-ctx` instead. Errors about the file itself (a missing path) are not
// bridge failures; callers read the file outside this function so those reach
// the transcript as the tool's error.

export interface BridgeReader {
  callTool(
    name: string,
    args: Record<string, unknown>,
    signal?: AbortSignal,
  ): Promise<{ content: Array<{ type: "text"; text: string }> }>;
}

let lastFallback: string | undefined;

/**
 * Run `ctx_read` through the MCP bridge. Returns the text, or `undefined` when
 * there is no bridge or the call failed (the caller then uses the CLI).
 */
export async function readViaBridge(
  bridge: BridgeReader | null,
  args: Record<string, unknown> & { mode: string },
  signal?: AbortSignal,
): Promise<string | undefined> {
  if (!bridge) return undefined;
  try {
    const result = await bridge.callTool("ctx_read", args, signal);
    return result.content.map((block) => block.text).join("");
  } catch (err) {
    const reason = err instanceof Error ? err.message : String(err);
    lastFallback = `ctx_read(${args.mode}) fell back to the CLI: ${reason}`;
    return undefined;
  }
}

/** The latest bridge fallback, for the `/lean-ctx` status. */
export function lastReadFallback(): string | undefined {
  return lastFallback;
}
