import { LeanCtxHttpError } from "./errors.js";
import { toolResultToText } from "./toolText.js";
import type {
  CapabilitiesV1,
  JsonObject,
  JsonValue,
  ContextEventV1,
  ListToolsResponse,
  ToolArguments,
  ToolCallResponse,
} from "./types.js";

export interface LeanCtxClientOptions {
  baseUrl: string;
  bearerToken?: string;
  fetchImpl?: typeof fetch;
  workspaceId?: string;
  channelId?: string;
}

function normalizeBaseUrl(baseUrl: string): string {
  const trimmed = baseUrl.trim();
  if (!trimmed) throw new Error("LeanCtxClient: baseUrl is required");
  return trimmed.endsWith("/") ? trimmed.slice(0, -1) : trimmed;
}

function isJsonObject(v: unknown): v is JsonObject {
  return !!v && typeof v === "object" && !Array.isArray(v);
}

export class LeanCtxClient {
  readonly baseUrl: string;
  private readonly bearerToken: string | undefined;
  private readonly fetchImpl: typeof fetch;
  private readonly workspaceId: string | undefined;
  private readonly channelId: string | undefined;

  constructor(opts: LeanCtxClientOptions) {
    this.baseUrl = normalizeBaseUrl(opts.baseUrl);
    this.bearerToken = opts.bearerToken?.trim() || undefined;
    this.fetchImpl = opts.fetchImpl ?? fetch;
    this.workspaceId = opts.workspaceId?.trim() || undefined;
    this.channelId = opts.channelId?.trim() || undefined;
  }

  async health(): Promise<string> {
    const res = await this.fetchImpl(`${this.baseUrl}/health`, {
      method: "GET",
      headers: this.authHeaders({ accept: "text/plain" }),
    });
    if (!res.ok) {
      throw await this.toHttpError(res, "GET", "/health");
    }
    return await res.text();
  }

  async manifest(): Promise<unknown> {
    return await this.getJson("/v1/manifest");
  }

  /** Runtime capability discovery document (`GET /v1/capabilities`). */
  async capabilities(): Promise<CapabilitiesV1> {
    const v = await this.getJson("/v1/capabilities");
    if (!isJsonObject(v)) {
      throw new Error("LeanCtxClient.capabilities: unexpected response shape");
    }
    return v as unknown as CapabilitiesV1;
  }

  /** OpenAPI 3.0 description of the public `/v1` surface (`GET /v1/openapi.json`). */
  async openapi(): Promise<JsonObject> {
    const v = await this.getJson("/v1/openapi.json");
    if (!isJsonObject(v)) {
      throw new Error("LeanCtxClient.openapi: unexpected response shape");
    }
    return v;
  }

  async listTools(params?: {
    offset?: number;
    limit?: number;
  }): Promise<ListToolsResponse> {
    const q = new URLSearchParams();
    if (params?.offset !== undefined) q.set("offset", String(params.offset));
    if (params?.limit !== undefined) q.set("limit", String(params.limit));
    const suffix = q.toString() ? `?${q}` : "";
    const v = await this.getJson(`/v1/tools${suffix}`);

    if (!isJsonObject(v)) {
      throw new Error("LeanCtxClient.listTools: unexpected response shape");
    }
    return v as unknown as ListToolsResponse;
  }

  async callToolResult(
    name: string,
    args?: ToolArguments,
    ctx?: { workspaceId?: string; channelId?: string }
  ): Promise<unknown> {
    const body: Record<string, unknown> = { name };
    if (args !== undefined) {
      if (!isJsonObject(args)) {
        throw new Error(
          "LeanCtxClient.callToolResult: arguments must be a JSON object"
        );
      }
      body.arguments = args;
    }
    const ws = ctx?.workspaceId?.trim() || this.workspaceId;
    const ch = ctx?.channelId?.trim() || this.channelId;
    if (ws) body.workspaceId = ws;
    if (ch) body.channelId = ch;

    const res = await this.fetchImpl(`${this.baseUrl}/v1/tools/call`, {
      method: "POST",
      headers: this.authHeaders({
        accept: "application/json",
        contentType: "application/json",
        workspaceId: ws,
      }),
      body: JSON.stringify(body),
    });

    if (!res.ok) {
      throw await this.toHttpError(res, "POST", "/v1/tools/call");
    }

    const json = (await res.json()) as unknown;
    if (!isJsonObject(json)) {
      throw new Error(
        "LeanCtxClient.callToolResult: unexpected response shape"
      );
    }
    const resp = json as unknown as ToolCallResponse;
    return resp.result;
  }

  async callToolText(
    name: string,
    args?: ToolArguments,
    ctx?: { workspaceId?: string; channelId?: string }
  ): Promise<string> {
    const result = await this.callToolResult(name, args, ctx);
    return toolResultToText(result);
  }

  async *subscribeEvents(params?: {
    workspaceId?: string;
    channelId?: string;
    since?: number;
    limit?: number;
  }): AsyncIterable<ContextEventV1> {
    const ws = params?.workspaceId?.trim() || this.workspaceId;
    const ch = params?.channelId?.trim() || this.channelId;
    const q = new URLSearchParams();
    if (ws) q.set("workspaceId", ws);
    if (ch) q.set("channelId", ch);
    if (params?.since !== undefined) q.set("since", String(params.since));
    if (params?.limit !== undefined) q.set("limit", String(params.limit));
    const suffix = q.toString() ? `?${q}` : "";

    const res = await this.fetchImpl(`${this.baseUrl}/v1/events${suffix}`, {
      method: "GET",
      headers: this.authHeaders({
        accept: "text/event-stream",
        workspaceId: ws,
      }),
    });
    if (!res.ok) {
      throw await this.toHttpError(res, "GET", `/v1/events${suffix}`);
    }
    if (!res.body) {
      throw new Error("LeanCtxClient.subscribeEvents: missing response body");
    }

    const decoder = new TextDecoder();
    const reader = res.body.getReader();
    let buf = "";

    for (;;) {
      const { value, done } = await reader.read();
      if (done) break;
      buf += decoder.decode(value, { stream: true });

      for (;;) {
        const idx = buf.indexOf("\n\n");
        if (idx < 0) break;
        const chunk = buf.slice(0, idx);
        buf = buf.slice(idx + 2);

        const ev = parseSseChunk(chunk);
        if (!ev?.data) continue;
        try {
          const parsed = JSON.parse(ev.data) as ContextEventV1;
          if (parsed && typeof parsed === "object") yield parsed;
        } catch {
          // ignore parse errors
        }
      }
    }
  }

  private authHeaders(extra: {
    accept?: string;
    contentType?: string;
    workspaceId?: string;
  }): HeadersInit {
    const h: Record<string, string> = {};
    if (extra.accept) h.Accept = extra.accept;
    if (extra.contentType) h["Content-Type"] = extra.contentType;
    if (this.bearerToken) h.Authorization = `Bearer ${this.bearerToken}`;
    if (extra.workspaceId) h["x-leanctx-workspace"] = extra.workspaceId;
    return h;
  }

  private async getJson(path: string): Promise<unknown> {
    const res = await this.fetchImpl(`${this.baseUrl}${path}`, {
      method: "GET",
      headers: this.authHeaders({ accept: "application/json" }),
    });
    if (!res.ok) {
      throw await this.toHttpError(res, "GET", path);
    }
    return (await res.json()) as unknown;
  }

  private async toHttpError(
    res: Response,
    method: string,
    path: string
  ): Promise<LeanCtxHttpError> {
    const url = `${this.baseUrl}${path}`;

    let body: JsonValue | string | undefined;
    let errorCode: string | undefined;
    let message = `HTTP ${res.status} ${method} ${url}`;

    const ct = res.headers.get("content-type") ?? "";
    try {
      if (ct.includes("application/json")) {
        const parsed = (await res.json()) as unknown;
        body = parsed as JsonValue;
        if (
          isJsonObject(parsed) &&
          typeof parsed.error === "string" &&
          parsed.error.trim()
        ) {
          message = parsed.error;
        }
        if (isJsonObject(parsed) && typeof parsed.error_code === "string") {
          const c = parsed.error_code.trim();
          if (c) errorCode = c;
        }
      } else {
        const txt = await res.text();
        body = txt;
        if (txt.trim()) message = txt.trim();
      }
    } catch {
      // ignore parse errors
    }

    return new LeanCtxHttpError({
      status: res.status,
      method,
      url,
      message,
      errorCode,
      body,
    });
  }
}

function parseSseChunk(
  chunk: string
): { id?: string; event?: string; data?: string } | null {
  const out: { id?: string; event?: string; data?: string } = {};
  const dataLines: string[] = [];
  for (const line of chunk.split("\n")) {
    const trimmed = line.trimEnd();
    if (!trimmed) continue;
    if (trimmed.startsWith(":")) continue; // comment
    if (trimmed.startsWith("id:")) out.id = trimmed.slice(3).trim();
    else if (trimmed.startsWith("event:")) out.event = trimmed.slice(6).trim();
    else if (trimmed.startsWith("data:")) dataLines.push(trimmed.slice(5).trimStart());
  }
  if (dataLines.length) out.data = dataLines.join("\n");
  return out;
}
