import type { Plugin } from "@opencode-ai/plugin"

export const LeanCtxOpenCodePlugin: Plugin = async ({ $ }) => {
  try {
    await $`which lean-ctx`.quiet()
  } catch {
    console.warn("[lean-ctx] lean-ctx binary not found in PATH — plugin disabled")
    return {}
  }

  return {
    "tool.execute.before": async (input, output) => {
      const tool = String(input?.tool ?? "").toLowerCase()
      if (tool !== "bash" && tool !== "shell") return
      const args = output?.args
      if (!args || typeof args !== "object") return

      const command = (args as Record<string, unknown>).command
      if (typeof command !== "string" || !command) return
      if (command.startsWith("lean-ctx ")) return

      try {
        const result = await $`lean-ctx hook rewrite-inline ${command}`.quiet().nothrow()
        const rewritten = String(result.stdout).trim()
        if (rewritten && rewritten !== command) {
          ;(args as Record<string, unknown>).command = rewritten
        }
      } catch {
        // lean-ctx rewrite failed — pass through unchanged
      }
    },
  }
}
