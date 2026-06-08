import type { LeanCtxClient } from "./client.js";

/**
 * Shared SDK conformance kit (EPIC 12.5).
 *
 * A language-agnostic client-side check that any lean-ctx SDK can run against a
 * live server to prove it speaks the `/v1` contract correctly. It is the mirror
 * of the server-side `lean-ctx conformance` scorecard: the server proves it
 * honors its contracts; this proves the SDK + server interoperate. The Python
 * SDK ships the same checks so both stay in lockstep.
 */

export interface ConformanceCheck {
  name: string;
  passed: boolean;
  detail: string;
}

export interface ConformanceScorecard {
  passed: number;
  total: number;
  allPassed: boolean;
  checks: ConformanceCheck[];
}

function check(
  name: string,
  passed: boolean,
  detail = ""
): ConformanceCheck {
  return { name, passed, detail };
}

/**
 * Run the conformance kit against a live client. Network/contract failures are
 * captured as failed checks rather than thrown, so the scorecard is always
 * complete and comparable across SDKs.
 */
export async function runConformance(
  client: LeanCtxClient
): Promise<ConformanceScorecard> {
  const checks: ConformanceCheck[] = [];

  // 1. health
  try {
    const h = await client.health();
    checks.push(check("health", typeof h === "string"));
  } catch (e) {
    checks.push(check("health", false, String(e)));
  }

  // 2. capabilities — stable top-level shape
  try {
    const caps = await client.capabilities();
    const ok =
      typeof caps.contract_version === "number" &&
      !!caps.server?.version &&
      typeof caps.plane === "string" &&
      Array.isArray(caps.transports) &&
      typeof caps.features === "object";
    checks.push(check("capabilities_shape", ok));
  } catch (e) {
    checks.push(check("capabilities_shape", false, String(e)));
  }

  // 3. openapi — well-formed 3.x doc with paths
  try {
    const doc = await client.openapi();
    const version = typeof doc.openapi === "string" ? doc.openapi : "";
    const ok = version.startsWith("3.") && typeof doc.paths === "object";
    checks.push(check("openapi_shape", ok));
  } catch (e) {
    checks.push(check("openapi_shape", false, String(e)));
  }

  // 4. tools listing — consistent counters
  try {
    const list = await client.listTools({ limit: 1 });
    const ok =
      Array.isArray(list.tools) &&
      typeof list.total === "number" &&
      list.total >= 0;
    checks.push(check("tools_list", ok));
  } catch (e) {
    checks.push(check("tools_list", false, String(e)));
  }

  const passed = checks.filter((c) => c.passed).length;
  return {
    passed,
    total: checks.length,
    allPassed: passed === checks.length,
    checks,
  };
}
