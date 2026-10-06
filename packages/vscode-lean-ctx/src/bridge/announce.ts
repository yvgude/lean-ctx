// SPDX-License-Identifier: Apache-2.0
// The bridge's announcement file: how lean-ctx finds a running bridge.
// No `vscode` import.

import { createHash, randomBytes } from "crypto";
import * as fs from "fs";
import * as path from "path";

export interface Announcement {
  port: number;
  token: string;
  pid: number;
  project_root: string;
  editor: string;
  editor_version: string;
  /** Digest of the installed extensions — the language providers answering. */
  provider_fingerprint: string;
  extension_version: string;
  started_at: string;
}

/** One file per (editor process, workspace folder). */
export function announcementName(editor: string, root: string, pid: number): string {
  const slug = editor.toLowerCase().replace(/[^a-z0-9]+/g, "-") || "editor";
  const hash = createHash("sha256").update(root).digest("hex").slice(0, 16);
  return `${slug}-${hash}-${pid}.json`;
}

/**
 * Creates `dir` owner-only, or confirms an existing one is: a real directory
 * owned by this user that nobody else can write to. Announcements carry a
 * token and name an endpoint lean-ctx trusts, so a directory another user
 * could write to is refused (throws). On Windows the data directory lives in
 * the user profile, protected by its ACL.
 */
export function ensurePrivateDir(dir: string): void {
  fs.mkdirSync(dir, { recursive: true, mode: 0o700 });
  const st = fs.lstatSync(dir);
  if (!st.isDirectory()) throw new Error(`${dir} is not a directory`);
  if (process.platform === "win32") return;
  if (typeof process.getuid === "function" && st.uid !== process.getuid()) {
    throw new Error(`${dir} belongs to another user`);
  }
  if ((st.mode & 0o077) !== 0) fs.chmodSync(dir, 0o700);
}

/**
 * Writes the announcement atomically and owner-only: a random temp name
 * created exclusively (never following a planted file), then renamed into
 * place. Returns its path.
 */
export function announce(dir: string, a: Announcement): string {
  ensurePrivateDir(dir);
  const file = path.join(dir, announcementName(a.editor, a.project_root, a.pid));
  const tmp = path.join(dir, `.${randomBytes(12).toString("hex")}.tmp`);
  fs.writeFileSync(tmp, JSON.stringify(a), { mode: 0o600, flag: "wx" });
  try {
    fs.renameSync(tmp, file);
  } catch (e) {
    withdraw(tmp);
    throw e;
  }
  return file;
}

export function withdraw(file: string): void {
  try {
    fs.unlinkSync(file);
  } catch {
    // Already gone.
  }
}

function alive(pid: number): boolean {
  try {
    process.kill(pid, 0);
    return true;
  } catch (e) {
    // EPERM: the process exists but belongs to someone else.
    return (e as NodeJS.ErrnoException).code === "EPERM";
  }
}

/**
 * Removes announcements left behind by editor processes that are gone (a
 * crash skips `withdraw`). Best effort; returns how many were removed.
 */
export function pruneStale(dir: string, isAlive: (pid: number) => boolean = alive): number {
  let removed = 0;
  let names: string[];
  try {
    names = fs.readdirSync(dir);
  } catch {
    return 0;
  }
  for (const name of names.filter((n) => n.endsWith(".json"))) {
    const file = path.join(dir, name);
    try {
      const pid = (JSON.parse(fs.readFileSync(file, "utf8")) as Partial<Announcement>).pid;
      if (typeof pid === "number" && !isAlive(pid)) {
        fs.unlinkSync(file);
        removed++;
      }
    } catch {
      // Unreadable or concurrently removed: leave it to its owner.
    }
  }
  return removed;
}
