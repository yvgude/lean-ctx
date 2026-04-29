import { LeanCtxClient } from "@leanctx/sdk";

export function createLeanCtxClient(opts: {
  bearerToken?: string;
}): LeanCtxClient {
  const baseUrl = new URL("/leanctx", window.location.origin)
    .toString()
    .replace(/\/$/, "");
  return new LeanCtxClient({ baseUrl, bearerToken: opts.bearerToken });
}
