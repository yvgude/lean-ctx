// SPDX-License-Identifier: Apache-2.0
import { expect, test } from "claude-code/testing";

import { pickSession } from "../hooks/cockpit";

const snap = (sessionId: string, hostPid: number | undefined, projectRoot: string, updatedAt: string) => ({
  sessionId,
  hostPid,
  projectRoot,
  updatedAt,
});

// GitHub #2181: Claude Code started in $HOME, the agent then worked in a
// project. The session is this host's, so the cockpit shows it instead of 0.
test("a session of this Claude Code counts even when it started outside the project", () => {
  const work = snap("work", 4242, "/home/me/code/app", "2026-11-03T15:00:00Z");
  expect(pickSession([work], [4242], "/home/me")?.sessionId).toBe("work");
});

test("another Claude Code's session never shows here", () => {
  const other = snap("other", 7, "/home/me", "2026-11-03T15:00:00Z");
  expect(pickSession([other], [4242], "/home/me")).toBeUndefined();
});

test("among this host's sessions the working directory's project wins, then the newest", () => {
  const snaps = [
    snap("app", 4242, "/home/me/code/app", "2026-11-03T14:00:00Z"),
    snap("lib", 4242, "/home/me/code/lib", "2026-11-03T15:00:00Z"),
  ];
  expect(pickSession(snaps, [4242], "/home/me/code/app/src")?.sessionId).toBe("app");
  expect(pickSession(snaps, [4242], "/home/me")?.sessionId).toBe("lib");
});

test("the nearest ancestor decides before a farther one", () => {
  const snaps = [
    snap("near", 10, "/w", "2026-11-03T14:00:00Z"),
    snap("far", 1, "/w", "2026-11-03T15:00:00Z"),
  ];
  expect(pickSession(snaps, [10, 1], "/w")?.sessionId).toBe("near");
});
