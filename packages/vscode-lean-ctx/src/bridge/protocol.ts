// SPDX-License-Identifier: Apache-2.0
// Wire protocol of the semantic bridge — the read-only navigation subset of
// the JetBrains plugin's HTTP protocol, so lean-ctx uses one client for both.
// No `vscode` import: the editor side is behind `Navigator`.
//
// Coordinates: `line`/`character` 0-based, `character` in UTF-16 code units
// (the editor's own unit); `line` inside `type_hierarchy` trees is 1-based.
// Paths: project-relative with `/` inside the workspace folder, absolute
// outside it (libraries, SDKs).

import * as fs from "fs";
import * as path from "path";

export interface Position {
  line: number;
  character: number;
}

export interface Range {
  start: Position;
  end: Position;
}

/** A location the editor reported; `file` is an absolute file-system path. */
export interface Location {
  file: string;
  range: Range;
}

/** A type in a hierarchy; `line` 0-based. */
export interface TypeNode {
  name: string;
  file: string;
  line: number;
}

export type Direction = "supertypes" | "subtypes";

/** The editor's language features, as the bridge needs them. */
export interface Navigator {
  definition(file: string, pos: Position): Promise<Location[]>;
  declaration(file: string, pos: Position): Promise<Location[]>;
  references(file: string, pos: Position): Promise<Location[]>;
  implementations(file: string, pos: Position): Promise<Location[]>;
  /** `null`: no type at that position (or the language has no hierarchy). */
  typeHierarchy(
    file: string,
    pos: Position,
    direction: Direction,
  ): Promise<{ root: TypeNode; related: TypeNode[] } | null>;
}

export interface Reply {
  status: number;
  body: unknown;
}

const error = (code: string, message: string, status = 200): Reply => ({
  status,
  body: { error: { code, message } },
});

/** `file` as the wire spells it: relative with `/` inside `root`, else absolute. */
export function wirePath(root: string, file: string, p: typeof path = path): string {
  const rel = p.relative(root, file);
  if (rel === "" || rel.startsWith("..") || p.isAbsolute(rel)) return file;
  return rel.split(p.sep).join("/");
}

/**
 * The absolute file a request names, confined to `root`: a relative path is
 * resolved against it, and nothing may escape it — lexically, and (with
 * `realpath`) after resolving symlinks and junctions, so a link inside the
 * folder cannot reach outside it. `null` when it does, or the file does not
 * exist.
 */
export function requestFile(
  root: string,
  wire: string,
  p: typeof path = path,
  realpath: ((f: string) => string | null) | null = realpathOrNull,
): string | null {
  const inside = (base: string, target: string) => {
    const rel = p.relative(base, target);
    return rel !== "" && !rel.startsWith("..") && !p.isAbsolute(rel);
  };
  const abs = p.resolve(root, wire);
  if (!inside(root, abs)) return null;
  if (!realpath) return abs;
  const realRoot = realpath(root);
  const realFile = realpath(abs);
  return realRoot && realFile && inside(realRoot, realFile) ? abs : null;
}

function realpathOrNull(f: string): string | null {
  try {
    return fs.realpathSync.native(f);
  } catch {
    return null;
  }
}

function position(body: Record<string, unknown>): Position | null {
  const { line, character } = body;
  const ok = (v: unknown): v is number => typeof v === "number" && Number.isInteger(v) && v >= 0;
  return ok(line) && ok(character) ? { line, character } : null;
}

const locationsReply = (root: string, locs: Location[]): Reply => ({
  status: 200,
  body: {
    locations: locs.map((l) => ({ path: wirePath(root, l.file), range: l.range })),
    truncated: false,
    total: locs.length,
  },
});

/** Routes one request. `body` is the parsed JSON object (POST) or `{}`. */
export async function handle(
  nav: Navigator,
  root: string,
  info: { editor: string; version: string },
  method: string,
  route: string,
  body: Record<string, unknown>,
): Promise<Reply> {
  if (method === "GET" && route === "/health") {
    return { status: 200, body: { status: "ok", ...info } };
  }
  // Fixed routes only: a client-chosen string never selects what is called.
  const known = ["/definition", "/declaration", "/references", "/implementations", "/type_hierarchy"];
  if (method !== "POST" || !known.includes(route)) {
    return error("NOT_FOUND", `no route ${method} ${route}`, 404);
  }
  const file = typeof body.path === "string" ? requestFile(root, body.path) : null;
  if (!file) return error("FILE_NOT_FOUND", "path is missing or outside the workspace folder");
  const pos = position(body);
  if (!pos) return error("POSITION_OUT_OF_RANGE", "line/character must be non-negative integers");

  switch (route) {
    case "/definition":
      return locationsReply(root, await nav.definition(file, pos));
    case "/declaration":
      return locationsReply(root, await nav.declaration(file, pos));
    case "/references":
      return locationsReply(root, await nav.references(file, pos));
    case "/implementations":
      return locationsReply(root, await nav.implementations(file, pos));
    default:
      return typeHierarchyReply(nav, root, file, pos, body);
  }
}

async function typeHierarchyReply(
  nav: Navigator,
  root: string,
  file: string,
  pos: Position,
  body: Record<string, unknown>,
): Promise<Reply> {
  const direction: Direction = body.direction === "subtypes" ? "subtypes" : "supertypes";
  const tree = await nav.typeHierarchy(file, pos, direction);
  // No `tree` key at all for "nothing here": lean-ctx reads that as "not
  // answered yet", whereas a tree without children is a definitive answer.
  if (!tree) return { status: 200, body: { truncated: false } };
  const node = (n: TypeNode, children: unknown[]) => ({
    name: n.name,
    path: wirePath(root, n.file),
    line: n.line + 1,
    children,
  });
  return {
    status: 200,
    body: { tree: node(tree.root, tree.related.map((r) => node(r, []))), truncated: false },
  };
}
