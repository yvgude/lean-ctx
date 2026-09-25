import { strict as assert } from "node:assert";
import { test } from "node:test";
import { candidatePaths } from "../binary";

test("configured path first, then PATH, then the usual install locations", () => {
  const got = candidatePaths(" /opt/lc/lean-ctx ", "/usr/bin:/bin", "/home/u", "linux");
  assert.deepEqual(got, [
    "/opt/lc/lean-ctx",
    "/usr/bin/lean-ctx",
    "/bin/lean-ctx",
    "/home/u/.local/bin/lean-ctx",
    "/home/u/.cargo/bin/lean-ctx",
    "/opt/homebrew/bin/lean-ctx",
    "/usr/local/bin/lean-ctx",
  ]);
});

test("duplicates collapse and an empty PATH still finds install locations", () => {
  const got = candidatePaths("", "/usr/local/bin::", "", "darwin");
  assert.deepEqual(got, ["/usr/local/bin/lean-ctx", "/opt/homebrew/bin/lean-ctx"]);
});

test("windows uses the .exe and ; separated PATH", () => {
  const got = candidatePaths("", "C:\\tools;D:\\bin", "C:\\Users\\u", "win32");
  assert.deepEqual(got, [
    "C:\\tools\\lean-ctx.exe",
    "D:\\bin\\lean-ctx.exe",
    "C:\\Users\\u\\.local\\bin\\lean-ctx.exe",
    "C:\\Users\\u\\.cargo\\bin\\lean-ctx.exe",
  ]);
});
