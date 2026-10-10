// SPDX-License-Identifier: Apache-2.0
import { expect, test } from "claude-code/testing";

const PANE = {
  plugin: "lean-ctx",
  component: "Pane",
  requestId: "leanctx-cockpit",
  props: {
    title: "LeanCTX",
    isFocused: false,
    bodyColumns: 60,
    placement: "dock",
    scroll: { offset: 0, bodyRows: 120 },
    view: {},
  },
} as const;

const BAND = {
  plugin: "lean-ctx",
  component: "AbovePrompt",
  props: {
    hasSurvey: false,
    isWorking: false,
    maxRows: 6,
    bodyColumns: 100,
    scroll: { offset: 0, bodyRows: 6 },
    view: {},
  },
} as const;

test("the sidebar shows what Claude saw, tokens kept out, the two checks and receipts", async ($) => {
  const ui = await $.ui.mount({ ...PANE, surface: "terminal" });
  for (const text of ["CLAUDE SAW", "TOKENS", "TWO CHECKS", "RECEIPTS", "tokens kept out", "May it be read?", "May it be delivered?", "Savings ledger"]) {
    expect(await ui.find({ type: "Text", text })).toBeDefined();
  }
  for (const key of ["pane-handoff", "figure", "depth-mix", "g-context"]) {
    expect(await ui.find({ type: "Raster", key })).toBeDefined();
  }
  await ui.unmount();
});

test("a narrow sidebar stacks the hero instead of breaking", async ($) => {
  const ui = await $.ui.mount({ ...PANE, props: { ...PANE.props, bodyColumns: 40 }, surface: "terminal" });
  expect(await ui.find({ type: "Raster", key: "figure" })).toBeDefined();
  expect(await ui.find({ type: "Text", text: "CLAUDE SAW" })).toBeDefined();
  await ui.unmount();
});

test("before any tool call RECENT says so instead of showing zeros", async ($) => {
  const ui = await $.ui.mount({ ...PANE, surface: "terminal" });
  expect(await ui.find({ type: "Text", text: "No tool calls yet" })).toBeDefined();
  await ui.unmount();
});

test("an idle band leaves the engine its own band: no number twice", async ($, on) => {
  on("ui.render", { component: "AbovePrompt" }, ($, e) => {
    const { Text } = $.ui.resolve(e);
    return <Text>engine band</Text>;
  });
  const ui = await $.ui.mount({ ...BAND, surface: "terminal" });
  expect(await ui.find({ type: "Text", text: "engine band" })).toBeDefined();
  expect(await ui.find({ type: "Raster", key: "band-handoff" })).toBeUndefined();
  await ui.unmount();
});

test("the band yields to a survey", async ($, on) => {
  on("ui.render", { component: "AbovePrompt" }, ($, e) => {
    const { Text } = $.ui.resolve(e);
    return <Text>engine band</Text>;
  });
  const ui = await $.ui.mount({ ...BAND, props: { ...BAND.props, hasSurvey: true }, surface: "terminal" });
  expect(await ui.find({ type: "Text", text: "engine band" })).toBeDefined();
  await ui.unmount();
});

test("the spinner keeps the engine words", async ($) => {
  const ui = await $.ui.mount({
    plugin: "lean-ctx",
    component: "Spinner",
    props: { word: "Sauteing", message: null, suffix: "…", mode: "responding" },
    surface: "terminal",
  });
  expect(await ui.find({ type: "Text", text: /Sauteing/ })).toBeDefined();
  await ui.unmount();
});

test("a turn line without lean-ctx work stays the engine’s", async ($, on) => {
  on("ui.render", { component: "TurnDuration" }, ($, e) => {
    const { Text } = $.ui.resolve(e);
    return <Text>engine turn line</Text>;
  });
  const ui = await $.ui.mount({
    plugin: "lean-ctx",
    component: "TurnDuration",
    props: { word: "Baked", durationMs: 4_000 },
    surface: "terminal",
  });
  expect(await ui.find({ type: "Text", text: "engine turn line" })).toBeDefined();
  await ui.unmount();
});
