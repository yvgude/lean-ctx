/**
 * Environment sanitisation for spawns that go through the host's bash tool.
 *
 * Host bash tools commonly validate environment variable names against the
 * POSIX identifier shape and reject anything else outright. Windows has
 * carried `ProgramFiles(x86)` / `CommonProgramFiles(x86)` since forever, and
 * those parentheses make such a host refuse the spawn before the command ever
 * runs — every `ctx_shell` call failing with `Invalid bash env name` (#1799).
 *
 * The filter belongs at that boundary rather than in `leanCtxEnv`: only the
 * host bash tool validates, while the MCP bridge and the engine's own
 * executor still see the complete environment — which is why `ctx_shell` over
 * MCP worked on the very same machine.
 *
 * Dropping these names costs nothing in practice. A POSIX shell cannot expand
 * `$ProgramFiles(x86)` by name anyway, because parentheses are not valid in an
 * identifier; the value is reachable only through `env`/`printenv`.
 */

/** The name shape a POSIX shell (and the hosts that validate) will accept. */
export const POSIX_ENV_NAME = /^[A-Za-z_][A-Za-z0-9_]*$/;

/** Drops every entry whose *name* a validating host would reject. */
export function posixSafeEnv(env: NodeJS.ProcessEnv): NodeJS.ProcessEnv {
  const safe: NodeJS.ProcessEnv = {};
  for (const [key, value] of Object.entries(env)) {
    if (POSIX_ENV_NAME.test(key)) safe[key] = value;
  }
  return safe;
}
