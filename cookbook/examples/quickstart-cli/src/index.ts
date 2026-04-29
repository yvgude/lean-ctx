import { LeanCtxClient, LeanCtxHttpError } from "@leanctx/sdk";

function env(name: string): string | undefined {
  const v = process.env[name];
  return v?.trim() ? v.trim() : undefined;
}

function toolNameFromUnknown(t: unknown): string | undefined {
  if (!t || typeof t !== "object") return undefined;
  const name = (t as Record<string, unknown>).name;
  return typeof name === "string" ? name : undefined;
}

async function main(): Promise<void> {
  const baseUrl = env("LEANCTX_BASE_URL") ?? "http://127.0.0.1:8080";
  const bearerToken = env("LEANCTX_BEARER_TOKEN");

  const client = new LeanCtxClient({ baseUrl, bearerToken });

  process.stdout.write(`LeanCTX baseUrl: ${client.baseUrl}\n`);

  const health = await client.health();
  process.stdout.write(`health: ${health}`);

  const tools = await client.listTools({ offset: 0, limit: 20 });
  const toolNames = tools.tools
    .map(toolNameFromUnknown)
    .filter((n): n is string => typeof n === "string");

  process.stdout.write(
    `tools: showing ${tools.tools.length}/${tools.total} (first names: ${toolNames
      .slice(0, 8)
      .join(", ")})\n`
  );

  const readme = await client.callToolText("ctx_read", {
    path: "README.md",
    mode: "lines:1-40",
  });

  process.stdout.write("\n--- ctx_read README.md (lines:1-40) ---\n");
  process.stdout.write(readme);
  if (!readme.endsWith("\n")) process.stdout.write("\n");
}

main().catch((e: unknown) => {
  if (e instanceof LeanCtxHttpError) {
    process.stderr.write(`LeanCTX HTTP error: ${e.message}\n`);
    process.exit(1);
  }
  process.stderr.write(`${String(e)}\n`);
  process.exit(1);
});
