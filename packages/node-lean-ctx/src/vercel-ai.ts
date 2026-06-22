import { LeanCtxClient, LeanCtxOptions } from "./client";
import { LeanCtxError } from "./errors";
import { ProxyClient, type CompressOptions, type Message } from "./proxy";

/**
 * Create a Vercel AI SDK compatible tool that wraps lean-ctx search.
 * Usage with `ai` package:
 *
 * ```ts
 * import { generateText } from 'ai';
 * import { createLeanCtxTool } from 'lean-ctx-sdk';
 *
 * const result = await generateText({
 *   model: myModel,
 *   tools: { search: createLeanCtxTool() },
 *   prompt: 'Find the auth implementation',
 * });
 * ```
 */
export function createLeanCtxTool(options?: LeanCtxOptions) {
  const client = new LeanCtxClient(options);

  return {
    description: "Search code using lean-ctx hybrid search (BM25 + vector + SPLADE)",
    parameters: {
      type: "object" as const,
      properties: {
        query: {
          type: "string" as const,
          description: "The search query",
        },
        path: {
          type: "string" as const,
          description: "Optional path scope",
        },
      },
      required: ["query"] as const,
    },
    execute: async ({ query, path }: { query: string; path?: string }) => {
      return client.search(query, path);
    },
  };
}

/** Structural view of the `transformParams` argument from `wrapLanguageModel`. */
interface TransformParamsArgs {
  type?: "generate" | "stream";
  params: { prompt?: unknown; [key: string]: unknown };
}

/** The subset of `LanguageModelV*Middleware` that lean-ctx implements. */
export interface LeanCtxLanguageModelMiddleware {
  transformParams: (args: TransformParamsArgs) => Promise<Record<string, unknown>>;
}

/**
 * Vercel AI SDK language-model middleware that compresses the prompt before it
 * reaches the provider. Wrap any model with it:
 *
 * ```ts
 * import { wrapLanguageModel } from "ai";
 * import { leanCtxMiddleware } from "lean-ctx-sdk";
 *
 * const model = wrapLanguageModel({
 *   model: openai("gpt-4o"),
 *   middleware: leanCtxMiddleware({ model: "gpt-4o" }),
 * });
 * ```
 *
 * A compaction failure (proxy down, auth, malformed response) never breaks the
 * generation — the original, uncompressed prompt is sent instead.
 */
export function leanCtxMiddleware(options: CompressOptions = {}): LeanCtxLanguageModelMiddleware {
  const { model, ...clientOptions } = options;
  const client = new ProxyClient(clientOptions);
  return {
    transformParams: async ({ params }) => {
      const prompt = params?.prompt;
      if (!Array.isArray(prompt)) return params;
      try {
        const { messages } = await client.compress(prompt as Message[], model);
        return { ...params, prompt: messages };
      } catch (error) {
        if (error instanceof LeanCtxError) return params;
        throw error;
      }
    },
  };
}

/**
 * Convenience wrapper: `withLeanCtx(model)` returns a model whose prompts are
 * compressed via {@link leanCtxMiddleware}. Requires the optional `ai` peer
 * dependency; prefer {@link leanCtxMiddleware} with your own `wrapLanguageModel`
 * if you'd rather not rely on `require("ai")`.
 *
 * ```ts
 * import { withLeanCtx } from "lean-ctx-sdk";
 * const model = withLeanCtx(openai("gpt-4o"), { model: "gpt-4o" });
 * ```
 */
export function withLeanCtx<TModel>(model: TModel, options: CompressOptions = {}): TModel {
  let wrap: ((args: { model: unknown; middleware: unknown }) => TModel) | undefined;
  try {
    wrap = (require("ai") as { wrapLanguageModel?: typeof wrap }).wrapLanguageModel;
  } catch {
    wrap = undefined;
  }
  if (typeof wrap !== "function") {
    throw new LeanCtxError(
      "withLeanCtx requires the 'ai' package (npm install ai). " +
        "Alternatively wrap manually: wrapLanguageModel({ model, middleware: leanCtxMiddleware() }).",
    );
  }
  return wrap({ model, middleware: leanCtxMiddleware(options) });
}
