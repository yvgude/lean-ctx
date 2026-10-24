// SPDX-License-Identifier: Apache-2.0
import { describe, expect, it } from "vitest";

import {
  boundReadOutput,
  READ_OUTPUT_MAX_BYTES,
  READ_OUTPUT_MAX_LINES,
} from "../extensions/read-output.js";

// The issue's repro: a pretty-printed JSON file of 30000 rows read with
// mode=full through the CLI-only path came back as one ~4 MB result.
function syntheticJson(rows: number): string {
  const body = Array.from({ length: rows }, (_, i) =>
    `    {\n      "i": ${i},\n      "value": "${"x".repeat(90)}"\n    }`,
  ).join(",\n");
  return `{\n  "rows": [\n${body}\n  ]\n}`;
}

describe("boundReadOutput (#2138)", () => {
  it("leaves results within the limits byte-identical, without truncation details", () => {
    for (const text of [
      "",
      "a\nb\nc\n",
      "x".repeat(READ_OUTPUT_MAX_BYTES),
      Array.from({ length: READ_OUTPUT_MAX_LINES }, () => "l").join("\n"),
      "ünïcødé\r\n\n\n\tend\n\nCompressed 10 → 5 tokens (-50%)",
    ]) {
      const result = boundReadOutput(text);
      expect(result.text).toBe(text);
      expect(result.truncation).toBeUndefined();
    }
  });

  it("caps a multi-megabyte full read and names the exact line to continue from", () => {
    const source = syntheticJson(30_000);
    expect(Buffer.byteLength(source)).toBeGreaterThan(3 * 1024 * 1024);

    const result = boundReadOutput(source, { sourceText: source });
    const truncation = result.truncation!;
    expect(truncation.truncated).toBe(true);
    expect(truncation.truncatedBy).toBe("bytes");
    expect(truncation.totalBytes).toBe(Buffer.byteLength(source));
    expect(truncation.outputBytes).toBeLessThanOrEqual(READ_OUTPUT_MAX_BYTES);
    // Whole lines only: the body is exactly the file's first outputLines lines.
    const sourceLines = source.split("\n");
    const body = sourceLines.slice(0, truncation.outputLines).join("\n");
    expect(result.text.startsWith(`${body}\n\n[ctx_read output truncated:`)).toBe(true);

    // The notice states the original size and a working continuation.
    const next = truncation.outputLines + 1;
    expect(result.text).toContain(`of ${sourceLines.length} lines`);
    expect(result.text).toMatch(/of \d+\.\dMB/);
    expect(result.text).toContain(`start_line=${next}, limit=${READ_OUTPUT_MAX_LINES}`);
    expect(result.text).toContain(`mode="lines:${next}-${next + READ_OUTPUT_MAX_LINES - 1}"`);
    // Notice included, the result stays near the cap rather than the file size.
    expect(Buffer.byteLength(result.text)).toBeLessThan(READ_OUTPUT_MAX_BYTES + 512);
  });

  it("caps by line count with Pi's 2000-line limit", () => {
    const source = Array.from({ length: 5000 }, (_, i) => `${i}`).join("\n");
    const result = boundReadOutput(source, { sourceText: source });
    expect(result.truncation).toMatchObject({
      truncatedBy: "lines",
      totalLines: 5000,
      outputLines: READ_OUTPUT_MAX_LINES,
      firstLineExceedsLimit: false,
    });
    expect(result.text).toContain("start_line=2001");
  });

  it("continues from the last numbered line of a lines:N-M window", () => {
    const window = [
      "big.txt 9000L lines:100-9000",
      ...Array.from({ length: 8901 }, (_, i) => `${String(100 + i).padStart(5)}| row`),
    ].join("\n");
    const result = boundReadOutput(window);
    // Header + 1999 numbered lines (100..2098) fit the 2000-line limit.
    expect(result.truncation?.outputLines).toBe(READ_OUTPUT_MAX_LINES);
    expect(result.text).toContain("start_line=2099");
  });

  it("does not guess a line when the output is not the file's own text", () => {
    // Compressed/reshaped output (map, signatures, collapsed blank lines).
    // withFooter collapses blank-line runs, so line N of the output is no
    // longer line N of the file.
    const source = Array.from({ length: 3000 }, (_, i) => `line ${i}\n\n\n`).join("");
    const reshaped = source.replace(/\n{3,}/g, "\n\n");
    expect(reshaped).not.toBe(source);
    const result = boundReadOutput(reshaped, { sourceText: source });
    expect(result.truncation?.truncated).toBe(true);
    expect(result.text).not.toMatch(/start_line=\d/);
    expect(result.text).toContain('start_line=N, limit=M) or mode="lines:N-M"');

    const unaided = boundReadOutput(reshaped);
    expect(unaided.text).toContain('start_line=N, limit=M) or mode="lines:N-M"');
  });

  it("delivers a prefix of a single over-long line, cut on a UTF-8 boundary", () => {
    const line = "é".repeat(READ_OUTPUT_MAX_BYTES); // 2 bytes per character
    const result = boundReadOutput(line);
    expect(result.truncation).toMatchObject({
      truncatedBy: "bytes",
      firstLineExceedsLimit: true,
      outputLines: 0,
      totalLines: 1,
    });
    const body = result.text.slice(0, result.text.indexOf("\n\n[ctx_read output truncated:"));
    expect(body).toBe("é".repeat(READ_OUTPUT_MAX_BYTES / 2));
    expect(body).not.toContain("�");
    expect(result.text).toContain('mode="map"');
  });

  it("matches the source across CRLF line endings", () => {
    const source = Array.from({ length: 2500 }, (_, i) => `r${i}`).join("\r\n");
    const delivered = source.replace(/\r\n/g, "\n");
    expect(boundReadOutput(delivered, { sourceText: source }).text).toContain("start_line=2001");
  });
});
