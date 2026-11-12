// SPDX-License-Identifier: Apache-2.0
import { expect, mock, test } from "claude-code/testing";

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

// GitHub #2142: while a turn ran, the band answered without next(), so every
// later AbovePrompt mod vanished until the turn ended.
test("a working band draws the handoff line and keeps the mods beneath it", async ($, on) => {
  on("ui.render", { component: "AbovePrompt" }, ($, e) => {
    const { Text } = $.ui.resolve(e);
    return <Text>engine band</Text>;
  });
  mock.clock(on);
  on("turn.start", ($, event) => ({ turnId: event.turnId }));
  await $.turn.start({ text: "build it", turnId: "turn-1" });
  const ui = await $.ui.mount({ ...BAND, props: { ...BAND.props, isWorking: true }, surface: "terminal" });
  expect(await ui.find({ type: "Raster", key: "band-handoff" })).toBeDefined();
  expect(await ui.find({ type: "Text", text: "engine band" })).toBeDefined();
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

// GitHub #2240: every session (daemon spares included) re-verified both hash
// chains every five minutes, on a large trail one pinned core per session.
const RUN_OK = { exitCode: 0, stderr: "", isStdoutTruncated: false, isStderrTruncated: false };

// Starts an interactive session whose `lean-ctx value` answers `answer()`;
// resolves the argv of every such run, in order.
async function startSession($: Parameters<Parameters<typeof test>[1]>[0], on: Parameters<Parameters<typeof test>[1]>[1], answer: () => string): Promise<string[][]> {
  const runs: string[][] = [];
  on("session.start", () => ({ cwd: "/work" }));
  on("command.register", ($, e) => ({ value: { command: e.name } }));
  on("ui.status", () => ({ value: undefined }));
  on("ui.open", () => ({ value: { isPlaced: true as const } }));
  on("process.run", ($, e) => {
    if (e.argv[1] !== "value") return { value: { ...RUN_OK, exitCode: 1, stdout: "" } };
    runs.push([...e.argv]);
    return { value: { ...RUN_OK, stdout: answer() } };
  });
  await $.session.start({ surface: "terminal", isInteractive: true, cwd: "/work" });
  return runs;
}

test("a session whose sidebar is never drawn never re-verifies the chains", async ($, on) => {
  const clock = mock.clock(on, { now: 1_000_000 });
  const runs = await startSession($, on, () => "{}");
  await clock.advance(30 * 60_000);
  expect(runs).toEqual([]);
});

test("a drawn sidebar proves shortly after its first drawing and on the clock", async ($, on) => {
  const clock = mock.clock(on, { now: 1_000_000 });
  const runs = await startSession($, on, () => JSON.stringify({ ledger: { entries: 1, intact: true }, audit: { entries: 1, intact: true } }));
  await (await $.ui.mount({ ...PANE, surface: "terminal" })).unmount();
  await clock.advance(5_000);
  expect(runs.length).toBe(1);
  for (let minute = 0; minute < 10; minute++) await clock.advance(60_000);
  expect(runs.length).toBe(3);
});

test("a failing proof backs off instead of running every five minutes", async ($, on) => {
  const clock = mock.clock(on, { now: 1_000_000 });
  const runs = await startSession($, on, () => "not json");
  await (await $.ui.mount({ ...PANE, surface: "terminal" })).unmount();
  // Runs at 0:04, 10:00 and 20:00 (backoff 5, 10, 20 min) instead of five times;
  // a minute at a time, as the animation frames tick while a proof is shown.
  for (let minute = 0; minute <= 20; minute++) await clock.advance(60_000);
  expect(runs.length).toBe(3);
});
