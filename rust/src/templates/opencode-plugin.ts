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

const ctxToNative: Record<string, string> = {
  ctx_read: "read",
  ctx_search: "grep",
  ctx_glob: "glob",
  ctx_shell: "bash",
}

/// Build tools dynamically from MCP.
async function buildDynamicTools(): Promise<Record<string, ReturnType<typeof tool>>> {
  const client = await getClient()
  const { tools: mcpTools } = await client.listTools()
  const dynamic: Record<string, ReturnType<typeof tool>> = {}

  for (const mcpTool of mcpTools) {
    const toolName = ctxToNative[mcpTool.name] ?? mcpTool.name
    const args = mcpSchemaToArgs(mcpTool.inputSchema)

    dynamic[toolName] = tool({
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
  let allTools: Record<string, ReturnType<typeof tool>> = {}
  try {
    allTools = await buildDynamicTools()
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

    tool: allTools,
  }
}
