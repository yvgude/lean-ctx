// Finding and running the `lean-ctx` binary. No `vscode` import.

import { execFile } from "child_process";
import { accessSync, constants } from "fs";
import * as path from "path";

/**
 * Where to look, in order: the configured path, then `PATH`, then the usual
 * install locations — an editor started from the Dock does not inherit the
 * login shell's `PATH`.
 */
export function candidatePaths(
  configured: string,
  envPath: string,
  home: string,
  platform: NodeJS.Platform,
): string[] {
  const exe = platform === "win32" ? "lean-ctx.exe" : "lean-ctx";
  const sep = platform === "win32" ? ";" : ":";
  const join = platform === "win32" ? path.win32.join : path.posix.join;
  const out: string[] = [];
  if (configured.trim()) out.push(configured.trim());
  for (const dir of envPath.split(sep)) if (dir) out.push(join(dir, exe));
  if (home) {
    out.push(join(home, ".local", "bin", exe), join(home, ".cargo", "bin", exe));
  }
  if (platform !== "win32") {
    out.push("/opt/homebrew/bin/lean-ctx", "/usr/local/bin/lean-ctx");
  }
  return [...new Set(out)];
}

function isExecutable(file: string): boolean {
  try {
    accessSync(file, process.platform === "win32" ? constants.F_OK : constants.X_OK);
    return true;
  } catch {
    return false;
  }
}

export function resolveBinary(configured: string): string | null {
  const candidates = candidatePaths(
    configured,
    process.env.PATH ?? "",
    process.env.HOME ?? process.env.USERPROFILE ?? "",
    process.platform,
  );
  return candidates.find(isExecutable) ?? null;
}

/** Runs the binary without a shell; stdout, or null on any failure. */
export function run(bin: string, args: string[], timeoutMs = 3000): Promise<string | null> {
  return new Promise((resolve) => {
    execFile(
      bin,
      args,
      { timeout: timeoutMs, env: { ...process.env, NO_COLOR: "1" }, maxBuffer: 256 * 1024 },
      (err, stdout) => resolve(err ? null : stdout),
    );
  });
}
