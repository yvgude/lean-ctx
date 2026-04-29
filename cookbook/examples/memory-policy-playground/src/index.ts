import { LeanCtxClient, LeanCtxHttpError } from "@leanctx/sdk";

function env(name: string): string | undefined {
  const v = process.env[name];
  return v?.trim() ? v.trim() : undefined;
}

function stripCtxLineNumbers(s: string): string {
  return s
    .split("\n")
    .map((line) => line.replace(/^\s*\d+\|\s?/, ""))
    .join("\n");
}

function extractCargoVersion(ctxReadOutput: string): string | undefined {
  const text = stripCtxLineNumbers(ctxReadOutput);
  const lines = text.split("\n");

  const start = Math.max(
    0,
    lines.findIndex((l) => l.trim() === "[package]")
  );
  const window = start >= 0 ? lines.slice(start, start + 80) : lines;

  for (const l of window) {
    const m = l.match(/^\s*version\s*=\s*"([^"]+)"/);
    if (m?.[1]) return m[1];
  }

  return undefined;
}

function extractHttpRoutes(ctxReadOutput: string): string[] {
  const text = stripCtxLineNumbers(ctxReadOutput);
  const routes = new Set<string>();

  const re = /\.route\("([^"]+)"/g;
  for (;;) {
    const m = re.exec(text);
    if (!m) break;
    if (m[1]) routes.add(m[1]);
  }

  return Array.from(routes).sort();
}

async function main(): Promise<void> {
  const baseUrl = env("LEANCTX_BASE_URL") ?? "http://127.0.0.1:8080";
  const bearerToken = env("LEANCTX_BEARER_TOKEN");
  const client = new LeanCtxClient({ baseUrl, bearerToken });

  process.stdout.write(`LeanCTX baseUrl: ${client.baseUrl}\n`);

  const cargoToml = await client.callToolText("ctx_read", {
    path: "rust/Cargo.toml",
    mode: "lines:1-120",
  });
  const version = extractCargoVersion(cargoToml);
  if (!version) {
    throw new Error(
      'Could not extract crate version from rust/Cargo.toml (expected [package] version = "...")'
    );
  }

  const httpServer = await client.callToolText("ctx_read", {
    path: "rust/src/http_server/mod.rs",
    mode: "lines:300-370",
  });
  const routes = extractHttpRoutes(httpServer);
  if (routes.length === 0) {
    throw new Error(
      'Could not extract HTTP routes from rust/src/http_server/mod.rs (expected .route("/...") entries)'
    );
  }

  const factVersion = `lean-ctx crate version: ${version}`;
  const factRoutes = `HTTP routes: ${routes.join(", ")}`;

  process.stdout.write("\n--- Remember 2 facts ---\n");
  process.stdout.write(
    await client.callToolText("ctx_knowledge", {
      action: "remember",
      category: "cookbook",
      key: "rust_crate_version",
      value: factVersion,
    })
  );
  process.stdout.write("\n");
  process.stdout.write(
    await client.callToolText("ctx_knowledge", {
      action: "remember",
      category: "cookbook",
      key: "http_routes",
      value: factRoutes,
    })
  );
  process.stdout.write("\n");

  process.stdout.write("\n--- Recall category=cookbook ---\n");
  process.stdout.write(
    await client.callToolText("ctx_knowledge", {
      action: "recall",
      category: "cookbook",
    })
  );

  process.stdout.write("\n--- Feedback up on rust_crate_version ---\n");
  process.stdout.write(
    await client.callToolText("ctx_knowledge", {
      action: "feedback",
      category: "cookbook",
      key: "rust_crate_version",
      value: "up",
    })
  );
  process.stdout.write("\n");

  process.stdout.write(
    "\n--- Relate rust_crate_version -> http_routes (supports) ---\n"
  );
  process.stdout.write(
    await client.callToolText("ctx_knowledge", {
      action: "relate",
      category: "cookbook",
      key: "rust_crate_version",
      value: "supports",
      query: "cookbook/http_routes",
    })
  );
  process.stdout.write("\n");

  process.stdout.write("\n--- Relations diagram (Mermaid) ---\n");
  process.stdout.write(
    await client.callToolText("ctx_knowledge", {
      action: "relations_diagram",
      category: "cookbook",
      key: "rust_crate_version",
      query: "all",
    })
  );
  process.stdout.write("\n");
}

main().catch((e: unknown) => {
  if (e instanceof LeanCtxHttpError) {
    process.stderr.write(`LeanCTX HTTP error: ${e.message}\n`);
    process.exit(1);
  }
  process.stderr.write(`${String(e)}\n`);
  process.exit(1);
});
