// SPDX-License-Identifier: Apache-2.0
//
// pi-lens keeps its own read ledger for its read-before-edit guard and records
// reads only for the tools it wraps. Co-process extensions publish their reads
// through a process-wide bridge (pi-lens dist/clients/read-bridge.js). Without
// this, pi-lens refuses every edit that follows a ctx_read with
// "Edit without read" (GH #2200).
//
// Publishing through the global symbol keeps pi-lean-ctx free of any pi-lens
// dependency. The call is advisory: an absent or changed bridge is a no-op and
// a throwing bridge never fails the read.

export const LENS_READ_BRIDGE_KEY = Symbol.for("pi-lens:read-bridge");

const LENS_READ_BRIDGE_VERSION = 1;
const LENS_CONSUMER = "lean-ctx";

interface LensReadBridge {
  version?: unknown;
  recordRead?: unknown;
}

export function notifyLensRead(filePath: string, offset?: number, limit?: number): void {
  try {
    const bridge = (globalThis as Record<symbol, unknown>)[LENS_READ_BRIDGE_KEY] as LensReadBridge | undefined;
    if (!bridge || typeof bridge !== "object") return;
    if (bridge.version !== LENS_READ_BRIDGE_VERSION || typeof bridge.recordRead !== "function") return;
    const entry: Record<string, unknown> = {
      filePath,
      requestedOffset: typeof offset === "number" && Number.isInteger(offset) && offset >= 1 ? offset : 1,
      consumer: LENS_CONSUMER,
    };
    if (typeof limit === "number" && Number.isInteger(limit) && limit >= 0) {
      entry.requestedLimit = limit;
    }
    (bridge.recordRead as (entry: Record<string, unknown>) => void).call(bridge, entry);
  } catch {
    // Advisory only: never break a read because pi-lens is absent or changed.
  }
}
