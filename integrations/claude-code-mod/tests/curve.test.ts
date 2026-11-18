// SPDX-License-Identifier: Apache-2.0
import { expect, test } from "claude-code/testing";

import { curveCells, recentGain } from "../hooks/cockpit";

// A raster cell is 12 bytes: code point, foreground, background (little endian).
function glyphRows(encoded: string, cols: number, rows: number): string[] {
  const bytes = Uint8Array.from(atob(encoded), (c) => c.charCodeAt(0));
  const view = new DataView(bytes.buffer);
  return Array.from({ length: rows }, (_, y) =>
    Array.from({ length: cols }, (_, x) => String.fromCodePoint(view.getUint32((y * cols + x) * 12, true))).join(""),
  );
}

const MIN = 60_000;

test("the curve is true to time: a saving halfway through fills only the second half", () => {
  const start = 1_000_000;
  const now = start + 40 * MIN;
  const points = [
    { at: start, saved: 0, raw: 0 },
    { at: start + 20 * MIN, saved: 1_000, raw: 2_000 },
  ];
  const [, , bottom] = glyphRows(curveCells(40, 3, points, now, 0), 40, 3);
  expect(bottom?.slice(0, 19)).toBe("▁".repeat(19));
  expect(bottom?.slice(20)).not.toContain("▁");
});

test("heights start at zero: a small saving under a large raw total stays low", () => {
  const start = 0;
  const points = [
    { at: start, saved: 100, raw: 10_000 },
    { at: start + MIN, saved: 100, raw: 10_000 },
  ];
  const rows = glyphRows(curveCells(10, 3, points, start + 2 * MIN, 0), 10, 3);
  // The raw envelope reaches the top row; "kept out" (1 %) does not climb above the bottom.
  expect(rows[0]).toBe("█".repeat(10));
  expect(rows[2]).not.toContain(" ");
});

test("recent gain counts only what the window added", () => {
  const points = [
    { at: 0, saved: 1_000 },
    { at: 5 * MIN, saved: 4_000 },
    { at: 15 * MIN, saved: 9_000 },
  ];
  expect(recentGain(points, 16 * MIN, 10 * MIN)).toBe(5_000);
  expect(recentGain(points, 60 * MIN, 10 * MIN)).toBe(0);
});
