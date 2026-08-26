import { describe, expect, it } from "vitest";

import { parseLeanCtxOutput, withFooter } from "../extensions/footer.js";

// #1578: the extension footer reported "Compressed N → N tokens (0%)" on every
// call because parseLeanCtxOutput matched none of the CLI's real marker
// formats and the `always` fallback then measured the already-compressed text
// against itself. These tests pin the real formats and the no-fabrication rule.
describe("parseLeanCtxOutput (#1578)", () => {
  it("parses the savings banner and strips it from the text", () => {
    const text = "✔︎ Bottle tesseract (5.5.3)\n─── 917 → 239 tok (↓~70%) ───";
    const { text: kept, stats } = parseLeanCtxOutput(text);
    expect(stats).toEqual({ originalTokens: 917, compressedTokens: 239, percentSaved: 74 });
    expect(kept).not.toContain("───");
  });

  it("parses abbreviated banner counts (17.0k, thousands separators)", () => {
    const { stats } = parseLeanCtxOutput("─── 17.0k → 1,474 tok (↓91%) ───");
    expect(stats?.originalTokens).toBe(17000);
    expect(stats?.compressedTokens).toBe(1474);
  });

  it("parses banners carrying mode/detail parts", () => {
    const { stats } = parseLeanCtxOutput("─── 4,200 → 840 tok (↓80%) | mode: full | cached ───");
    expect(stats?.originalTokens).toBe(4200);
    expect(stats?.compressedTokens).toBe(840);
  });

  it("parses the reason bracket without a percentage", () => {
    const { stats } = parseLeanCtxOutput("[lean-ctx: 17001→1474 tok, verbatim truncated]");
    expect(stats).toEqual({ originalTokens: 17001, compressedTokens: 1474, percentSaved: 91 });
  });

  it("parses saved brackets with and without the lean-ctx prefix", () => {
    expect(parseLeanCtxOutput("[lean-ctx: 12 tok saved (3%)]").stats?.originalTokens).toBe(400);
    expect(parseLeanCtxOutput("[0 tok saved]").stats?.percentSaved).toBe(0);
  });
});

describe("withFooter (#1578)", () => {
  it("never fabricates a 0% footer when no marker matched", () => {
    const out = withFooter("plain output with no lean-ctx marker", { always: true });
    expect(out.stats).toBeUndefined();
    expect(out.text).not.toContain("Compressed");
  });

  it("re-renders a parsed banner as the single extension footer", () => {
    const out = withFooter("body\n─── 917 → 239 tok (↓~70%) ───", { always: true });
    expect(out.text).toContain("Compressed 917 → 239 tokens (-74%)");
    expect(out.text.match(/tok/g)?.length).toBe(1);
  });
});
