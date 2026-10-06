// SPDX-License-Identifier: Apache-2.0
import { strict as assert } from "node:assert";
import * as fs from "node:fs";
import * as http from "node:http";
import * as os from "node:os";
import * as path from "node:path";
import { test } from "node:test";
import { announce, announcementName, ensurePrivateDir, pruneStale, withdraw } from "../bridge/announce";
import { Location, Navigator, Position, requestFile, wirePath } from "../bridge/protocol";
import { startBridge } from "../bridge/server";

// A real folder: the bridge resolves requested files on disk.
const root = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "lc-bridge-root-")));
for (const f of ["src/app.ts", "src/mem.ts", "src/none.ts"]) {
  fs.mkdirSync(path.dirname(path.join(root, f)), { recursive: true });
  fs.writeFileSync(path.join(root, f), "");
}
const range = { start: { line: 2, character: 11 }, end: { line: 2, character: 15 } };

/** Answers like an editor would; records what it was asked. */
function fakeNavigator(asked: { file: string; pos: Position }[]): Navigator {
  const at = (file: string, pos: Position, out: Location[]) => {
    asked.push({ file, pos });
    return Promise.resolve(out);
  };
  return {
    definition: (f, p) =>
      at(f, p, [
        { file: path.join(root, "src", "b.ts"), range },
        { file: "/usr/lib/node_modules/typescript/lib/lib.es5.d.ts", range },
      ]),
    declaration: (f, p) => at(f, p, []),
    references: (f, p) => at(f, p, [{ file: path.join(root, "src", "app.ts"), range }]),
    implementations: (f, p) => at(f, p, []),
    typeHierarchy: (f, _p, direction) =>
      Promise.resolve(
        f.endsWith("none.ts")
          ? null
          : {
              root: { name: "Mem", file: f, line: 2 },
              related:
                direction === "supertypes"
                  ? [{ name: "Base", file: path.join(root, "src", "base.ts"), line: 0 }]
                  : [],
            },
      ),
  };
}

function call(
  port: number,
  method: string,
  route: string,
  body: unknown,
  token: string | null,
): Promise<{ status: number; json: any }> {
  return new Promise((resolve, reject) => {
    const text = body === undefined ? "" : JSON.stringify(body);
    const req = http.request(
      {
        host: "127.0.0.1",
        port,
        method,
        path: route,
        headers: {
          ...(token ? { "X-LeanCtx-Token": token } : {}),
          "Content-Type": "application/json",
          "Content-Length": Buffer.byteLength(text),
        },
      },
      (res) => {
        let data = "";
        res.on("data", (c) => (data += c));
        res.on("end", () => resolve({ status: res.statusCode ?? 0, json: JSON.parse(data) }));
      },
    );
    req.on("error", reject);
    req.end(text);
  });
}

test("wire paths: relative with / inside the folder, absolute outside", () => {
  assert.equal(wirePath("/w", "/w/src/a.ts", path.posix), "src/a.ts");
  assert.equal(wirePath("/w", "/lib/x.d.ts", path.posix), "/lib/x.d.ts");
  assert.equal(wirePath("/w", "/w-other/a.ts", path.posix), "/w-other/a.ts");
  assert.equal(wirePath("C:\\w", "C:\\w\\src\\a.ts", path.win32), "src/a.ts");
});

test("requested files stay inside the workspace folder", () => {
  assert.equal(requestFile("/w", "src/a.ts", path.posix, null), "/w/src/a.ts");
  assert.equal(requestFile("/w", "/w/src/a.ts", path.posix, null), "/w/src/a.ts");
  assert.equal(requestFile("/w", "../etc/passwd", path.posix, null), null);
  assert.equal(requestFile("/w", "/etc/passwd", path.posix, null), null);
  assert.equal(requestFile("/w", ".", path.posix, null), null);
  assert.equal(requestFile("C:\\w", "src/a.ts", path.win32, null), "C:\\w\\src\\a.ts");
  assert.equal(requestFile("C:\\w", "D:\\x.ts", path.win32, null), null);
});

test("a symlink inside the folder cannot reach outside it", () => {
  const outside = fs.mkdtempSync(path.join(os.tmpdir(), "lc-outside-"));
  fs.writeFileSync(path.join(outside, "secret.ts"), "");
  const link = path.join(root, "linked");
  fs.symlinkSync(outside, link, process.platform === "win32" ? "junction" : "dir");
  try {
    assert.equal(requestFile(root, "linked/secret.ts"), null);
    assert.equal(requestFile(root, "src/app.ts"), path.join(root, "src", "app.ts"));
    assert.equal(requestFile(root, "src/missing.ts"), null);
  } finally {
    fs.unlinkSync(link);
  }
});

test("an announcement directory others could write to is made private", { skip: process.platform === "win32" }, () => {
  const dir = path.join(fs.mkdtempSync(path.join(os.tmpdir(), "lc-bridges-")), "editor-bridges");
  fs.mkdirSync(dir, { mode: 0o777 });
  fs.chmodSync(dir, 0o777);
  ensurePrivateDir(dir);
  assert.equal(fs.statSync(dir).mode & 0o777, 0o700);
  assert.throws(() => ensurePrivateDir(path.join(root, "src", "app.ts")), /not a directory|EEXIST|ENOTDIR/);
});

test("the bridge serves the navigation protocol behind its token", async () => {
  const asked: { file: string; pos: Position }[] = [];
  const bridge = await startBridge({
    root,
    token: "secret",
    navigator: fakeNavigator(asked),
    info: { editor: "vscode", version: "1.99.0" },
  });
  try {
    const health = await call(bridge.port, "GET", "/health", undefined, "secret");
    assert.deepEqual(health, { status: 200, json: { status: "ok", editor: "vscode", version: "1.99.0" } });

    assert.equal((await call(bridge.port, "GET", "/health", undefined, null)).status, 401);
    assert.equal((await call(bridge.port, "GET", "/health", undefined, "secreT")).status, 401);
    assert.equal((await call(bridge.port, "POST", "/renameApply", {}, "secret")).status, 404);
    assert.equal((await call(bridge.port, "POST", "/constructor", {}, "secret")).status, 404);

    const def = await call(bridge.port, "POST", "/definition", { path: "src/app.ts", line: 2, character: 7 }, "secret");
    assert.deepEqual(asked, [{ file: path.join(root, "src", "app.ts"), pos: { line: 2, character: 7 } }]);
    assert.deepEqual(def.json.locations, [
      { path: "src/b.ts", range },
      { path: "/usr/lib/node_modules/typescript/lib/lib.es5.d.ts", range },
    ]);

    const escape = await call(bridge.port, "POST", "/references", { path: "../x.ts", line: 0, character: 0 }, "secret");
    assert.equal(escape.json.error.code, "FILE_NOT_FOUND");
    const badPos = await call(bridge.port, "POST", "/references", { path: "src/app.ts", line: -1, character: 0 }, "secret");
    assert.equal(badPos.json.error.code, "POSITION_OUT_OF_RANGE");

    // A type with supertypes; and "no type here", which carries no tree at
    // all (lean-ctx must not read it as a definitive empty answer).
    const th = await call(bridge.port, "POST", "/type_hierarchy", { path: "src/mem.ts", line: 2, character: 13, direction: "supertypes" }, "secret");
    assert.deepEqual(th.json.tree, {
      name: "Mem",
      path: "src/mem.ts",
      line: 3,
      children: [{ name: "Base", path: "src/base.ts", line: 1, children: [] }],
    });
    const none = await call(bridge.port, "POST", "/type_hierarchy", { path: "src/none.ts", line: 0, character: 0 }, "secret");
    assert.equal("tree" in none.json, false);
  } finally {
    await bridge.close();
  }
});

test("announcements are written atomically, owner-only, and withdrawn", () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "lc-bridges-"));
  const file = announce(dir, {
    port: 1234,
    token: "t",
    pid: process.pid,
    project_root: "/w",
    editor: "Cursor",
    editor_version: "1.0",
    provider_fingerprint: "abc",
    extension_version: "0.4.0",
    started_at: "2026-10-03T00:00:00Z",
  });
  assert.equal(path.basename(file), announcementName("Cursor", "/w", process.pid));
  assert.match(path.basename(file), /^cursor-[0-9a-f]{16}-\d+\.json$/);
  assert.equal(JSON.parse(fs.readFileSync(file, "utf8")).port, 1234);
  assert.deepEqual(fs.readdirSync(dir), [path.basename(file)], "no temp file left behind");
  if (process.platform !== "win32") assert.equal(fs.statSync(file).mode & 0o777, 0o600);
  withdraw(file);
  assert.deepEqual(fs.readdirSync(dir), []);
});

test("announcements of editor processes that are gone are pruned", () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "lc-bridges-"));
  const write = (name: string, pid: number) => fs.writeFileSync(path.join(dir, name), JSON.stringify({ pid }));
  write("live.json", 1);
  write("dead.json", 2);
  fs.writeFileSync(path.join(dir, "broken.json"), "{");
  assert.equal(pruneStale(dir, (pid) => pid === 1), 1);
  assert.deepEqual(fs.readdirSync(dir).sort(), ["broken.json", "live.json"]);
});
