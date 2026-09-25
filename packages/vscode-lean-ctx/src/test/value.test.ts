import { strict as assert } from "node:assert";
import { test } from "node:test";
import { escapeMarkdown, parsePayload, statusText, tooltipMarkdown } from "../value";

const sample = {
  schema: 1,
  display: "minimal",
  segment: "◆ −1.2M tok ⛨ 3",
  tooltip: [
    "✓ 1.2M tokens kept out of context",
    "≈ 60% of 2.0M tokens of tool output",
    "✓ 3 secrets kept out of context",
  ],
  speed: null,
  watch: "/home/u/.local/share/lean-ctx/value/projects",
  verify: "lean-ctx value",
};

test("parses the binary's schema-1 payload", () => {
  const p = parsePayload(JSON.stringify(sample));
  assert.ok(p);
  assert.equal(statusText(p), "◆ −1.2M tok ⛨ 3");
  assert.deepEqual(p.tooltip, sample.tooltip);
  assert.equal(p.watch, sample.watch);
});

test("anything but schema 1 is rejected, so an older binary shows nothing", () => {
  assert.equal(parsePayload(""), null);
  assert.equal(parsePayload("Usage: lean-ctx prompt-segment"), null);
  assert.equal(parsePayload(JSON.stringify({ ...sample, schema: 2 })), null);
  assert.equal(parsePayload("null"), null);
});

test("nothing measured, stale or display off hides the item instead of showing 0", () => {
  for (const patch of [{ segment: null }, { segment: "" }, { display: "off" }]) {
    const p = parsePayload(JSON.stringify({ ...sample, ...patch }));
    assert.ok(p);
    assert.equal(statusText(p), null);
  }
});

test("status text cannot smuggle in a codicon", () => {
  const p = parsePayload(JSON.stringify({ ...sample, segment: "$(alert) x" }));
  assert.ok(p);
  assert.ok(!statusText(p)!.includes("$("));
});

test("tooltip is labelled, escaped and ends with how to verify", () => {
  const md = tooltipMarkdown(parsePayload(JSON.stringify(sample))!);
  assert.match(md, /✓ 1\\\.2M tokens kept out of context/);
  assert.match(md, /≈ 60% of 2\\\.0M tokens/);
  assert.match(md, /Verify: `lean-ctx value`$/);
  assert.ok(!md.includes("neutraliz"));
});

test("speed appears only when the binary vouches for it", () => {
  const without = tooltipMarkdown(parsePayload(JSON.stringify(sample))!);
  assert.ok(!without.includes("⚡"));
  const withSpeed = tooltipMarkdown(
    parsePayload(
      JSON.stringify({
        ...sample,
        speed: { phrase: "model answered 24% faster", detail: "measured 2026-09-25 · 12 tasks × 3 runs · m" },
      }),
    )!,
  );
  assert.match(withSpeed, /⚡ model answered 24% faster/);
  // A malformed speed block is dropped, not half-shown.
  const p = parsePayload(JSON.stringify({ ...sample, speed: { phrase: "x" } }));
  assert.equal(p!.speed, null);
});

test("markdown escaping covers links and emphasis", () => {
  assert.equal(escapeMarkdown("[a](b) *c*"), "\\[a\\]\\(b\\) \\*c\\*");
});
