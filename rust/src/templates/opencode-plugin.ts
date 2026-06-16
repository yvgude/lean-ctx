import { type Plugin, tool } from "@opencode-ai/plugin"
import { Client } from "@modelcontextprotocol/sdk/client/index.js"
import { StdioClientTransport } from "@modelcontextprotocol/sdk/client/stdio.js"

/** Lazy-init MCP client connected to lean-ctx via stdio.
 *  _clientPromise guards against concurrent getClient() calls. */
let _client: Client | null = null
let _clientPromise: Promise<Client> | null = null

async function getClient(): Promise<Client> {
  if (_client) return _client
  if (_clientPromise) return _clientPromise

  _clientPromise = (async () => {
    const transport = new StdioClientTransport({
      command: "lean-ctx",
      args: [],
      stderr: "pipe",
    })

    const client = new Client(
      { name: "opencode-lean-ctx-plugin", version: "1.0.0" },
      { capabilities: {} },
    )

    await client.connect(transport)
    _client = client
    return client
  })()

  try {
    return await _clientPromise
  } catch (err) {
    // Clean up on failure so subsequent calls retry
    _client = null
    _clientPromise = null
    throw err
  } finally {
    _clientPromise = null
  }
}

/** Call a lean-ctx MCP tool by name */
async function callTool(
  name: string,
  args: Record<string, unknown>,
): Promise<string> {
  try {
    const client = await getClient()
    const result = await client.callTool({ name, arguments: args })
    const content = result.content as Array<{ type: string; text?: string;[key: string]: unknown }>
    const parts: string[] = []
    for (const c of content) {
      if (c.type === "text" && typeof c.text === "string") {
        parts.push(c.text)
      } else if (c.type === "resource" || c.type === "image") {
        parts.push(`\n[Attached ${c.type} skipped]\n`)
      }
    }
    return parts.join("")
  } catch (err) {
    // Reset client so the next call triggers a fresh connection.
    // This handles server crashes, restarts, and transport-level failures.
    _client = null
    _clientPromise = null
    const msg = err instanceof Error ? err.message : String(err)
    return `[lean-ctx error] ${name} failed: ${msg}`
  }
}

/** Convert a JSON Schema property to a Zod schema field.
 *  Handles string, number, integer, boolean, array, object, and anyOf/oneOf. */
// eslint-disable-next-line @typescript-eslint/no-explicit-any
type ZodField = any

function jsonSchemaToZod(prop: Record<string, unknown>): ZodField {
  const z = tool.schema

  if (Array.isArray(prop.anyOf) || Array.isArray(prop.oneOf)) {
    const variants = (prop.anyOf ?? prop.oneOf) as Record<string, unknown>[]
    const nonNull = variants.find((v) => v.type !== "null")
    if (nonNull) return jsonSchemaToZod(nonNull)
    return z.any()
  }

  switch (prop.type) {
    case "string":
      return z.string()
    case "number":
      return z.number()
    case "integer":
      return z.number()
    case "boolean":
      return z.boolean()
    case "array": {
      const items = prop.items as Record<string, unknown> | undefined
      return z.array(items ? jsonSchemaToZod(items) : z.any())
    }
    case "object":
      return z.record(z.string(), z.any())
    default:
      return z.any()
  }
}

/** Convert MCP tool inputSchema (JSON Schema) to opencode args (Zod map). */
function mcpSchemaToArgs(
  schema: { properties?: Record<string, object>; required?: string[] },
): Record<string, ZodField> {
  const properties = schema.properties ?? {}
  const required = new Set(schema.required ?? [])
  const args: Record<string, ZodField> = {}

  for (const [key, prop] of Object.entries(properties)) {
    let field = jsonSchemaToZod(prop as Record<string, unknown>)
    if (!required.has(key)) field = field.optional()
    if ((prop as Record<string, unknown>).description) {
      field = field.describe(String((prop as Record<string, unknown>).description))
    }
    if ((prop as Record<string, unknown>).default !== undefined) {
      field = field.default((prop as Record<string, unknown>).default)
    }
    args[key] = field
  }

  return args
}

/** Build dynamic tools from MCP server. */
async function buildDynamicTools(): Promise<Record<string, ReturnType<typeof tool>>> {
  const client = await getClient()
  const { tools: mcpTools } = await client.listTools()
  const dynamic: Record<string, ReturnType<typeof tool>> = {}
  const excludedTools = new Set(["ctx_read", "ctx_edit", "ctx_search", "ctx_glob", "ctx_shell"])
  const allowedTools = mcpTools.filter(mcpTool => !excludedTools.has(mcpTool.name))

  for (const mcpTool of allowedTools) {
    const args = mcpSchemaToArgs(mcpTool.inputSchema)
    dynamic[mcpTool.name] = tool({
      description: mcpTool.description ?? mcpTool.name,
      args,
      async execute(toolArgs) {
        return await callTool(mcpTool.name, toolArgs as Record<string, unknown>)
      },
    })
  }

  return dynamic
}

export const LeanCtxOpenCodePlugin: Plugin = async (_ctx) => {
  // ── Static tools that replace native opencode tools ──────────
  const staticTools: Record<string, ReturnType<typeof tool>> = {
    // ── read → lean-ctx ctx_read ───────────────────────────
    read: tool({
      description: `Read a file with caching and compression. Unchanged re-reads cost ~13 tokens.

        Mode: auto (default), full, map, signatures, diff, aggressive, entropy, task, reference, lines:N-M
      - auto: best-effort selection
      - full: complete content
      - map: deps + API signatures
      - signatures: function/type signatures via tree-sitter
      - diff: changed lines only
      - aggressive: syntax stripped
      - entropy: low-info lines removed
      - task: task-filtered with graph context
      - reference: ultra-compact pointer
      - lines:N-M: specific ranges (e.g. lines:10-80)

      Use read for files you'll edit or analyze. Use grep for content search, glob for filename patterns.`,
      args: {
        path: tool.schema.string().describe("Absolute file path to read"),
        mode: tool.schema
          .string()
          .optional()
          .default("auto")
          .describe("Compression mode (default: auto). Options: full, map, signatures, diff, aggressive, entropy, task, reference, lines:N-M"),
        fresh: tool.schema
          .boolean()
          .optional()
          .default(false)
          .describe("Force re-read from disk (bypasses lean-ctx cache). Use after external file modifications."),
      },
      async execute({ path, mode, fresh }) {
        return await callTool("ctx_read", { path, mode, fresh })
      },
    }),

    // ── grep → lean-ctx ctx_search ─────────────────────────
    grep: tool({
      description: `Search file contents by regex. Compact, token-efficient results. Respects .gitignore.

        Use grep to find code patterns, function definitions, variable usages. Use glob for filename patterns, read for full file content.`,
      args: {
        pattern: tool.schema.string().describe("Regex pattern to search for in file contents"),
        path: tool.schema.string().optional().default(".").describe("Directory to search in (default: current directory)"),
        include: tool.schema
          .string()
          .optional()
          .describe('file filter glob (e.g. "*.ts", "*.{rs,ts}", "src/**/*.tsx")'),
        max_results: tool.schema
          .number()
          .optional()
          .default(20)
          .describe("max results (default: 20)"),
      },
      async execute({ pattern, path, include, max_results }) {
        return await callTool("ctx_search", {
          pattern,
          path,
          ...(include ? { include } : {}),
          max_results,
        })
      },
    }),

    // ── glob → lean-ctx ctx_glob ────────────────────────────
    glob: tool({
      description: `Find files by glob pattern. Fast matching for any codebase size. Respects .gitignore.

          Use glob for filename patterns. Use grep for content search, read for file content.`,
      args: {
        pattern: tool.schema.string().describe("glob pattern to match files against"),
        path: tool.schema.string().optional().default(".").describe("directory to search in. If not specified, the current working directory will be used"),
      },
      async execute({ pattern, path }) {
        return await callTool("ctx_glob", { pattern, path })
      },
    }),

    // ── edit → lean-ctx ctx_edit ───────────────────────────
    edit: tool({
      description: `Edit a file via search-and-replace. oldString must be unique unless replaceAll=true.

        Use edit for modifications. Use write for new files, read to view content first.`,
      args: {
        filePath: tool.schema.string().describe("absolute file path"),
        oldString: tool.schema.string().describe("text to replace"),
        newString: tool.schema.string().describe("replacement text"),
        replaceAll: tool.schema
          .boolean()
          .optional()
          .default(false)
          .describe("Replace all occurrences (default: false)"),
      },
      async execute({ filePath, oldString, newString, replaceAll }) {
        return await callTool("ctx_edit", {
          path: filePath,
          old_string: oldString,
          new_string: newString,
          replace_all: replaceAll,
        })
      },
    }),

    // ── bash → lean-ctx ctx_shell ──────────────────────────
    bash: tool({
      description: `Execute a shell command. Set raw=true for verbatim output.`,
      args: {
        command: tool.schema.string().describe("Shell command to execute"),
        raw: tool.schema
          .boolean()
          .optional()
          .default(false)
          .describe("Skip output compression. Use when exact verbatim output is required."),
        cwd: tool.schema
          .string()
          .optional()
          .describe("Working directory for the command. Defaults to current directory."),
      },
      async execute({ command, raw, cwd }) {
        return await callTool("ctx_shell", {
          command,
          raw,
          ...(cwd ? { cwd } : {}),
        })
      },
    }),
  }

  // ── Dynamic tools from MCP ────────────────
  let dynamicTools: Record<string, ReturnType<typeof tool>> = {}
  try {
    dynamicTools = await buildDynamicTools()
  } catch {
    console.error("[lean-ctx] failed to dynamically load mcp tools")
  }

  return {
    dispose: async () => {
      if (_clientPromise) {
        await _clientPromise.catch(() => { })
      }
      if (_client) {
        await _client.close().catch(() => { })
        _client = null
      }
      _clientPromise = null
    },

    tool: { ...dynamicTools, ...staticTools },
  }
}
