import { LeanCtxHttpError } from "./errors.js";
import { toolResultToText } from "./toolText.js";
import type {
  JsonObject,
  JsonValue,
  ListToolsResponse,
  ToolArguments,
  ToolCallResponse,
} from "./types.js";

export interface LeanCtxClientOptions {
  baseUrl: string;
  bearerToken?: string;
  fetchImpl?: typeof fetch;
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

  constructor(opts: LeanCtxClientOptions) {
    this.baseUrl = normalizeBaseUrl(opts.baseUrl);
    this.bearerToken = opts.bearerToken?.trim() || undefined;
    this.fetchImpl = opts.fetchImpl ?? fetch;
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

  async callToolResult(name: string, args?: ToolArguments): Promise<unknown> {
    const body: Record<string, unknown> = { name };
    if (args !== undefined) {
      if (!isJsonObject(args)) {
        throw new Error(
          "LeanCtxClient.callToolResult: arguments must be a JSON object"
        );
      }
      body.arguments = args;
    }

    const res = await this.fetchImpl(`${this.baseUrl}/v1/tools/call`, {
      method: "POST",
      headers: this.authHeaders({
        accept: "application/json",
        contentType: "application/json",
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

  async callToolText(name: string, args?: ToolArguments): Promise<string> {
    const result = await this.callToolResult(name, args);
    return toolResultToText(result);
  }

  private authHeaders(extra: {
    accept?: string;
    contentType?: string;
  }): HeadersInit {
    const h: Record<string, string> = {};
    if (extra.accept) h.Accept = extra.accept;
    if (extra.contentType) h["Content-Type"] = extra.contentType;
    if (this.bearerToken) h.Authorization = `Bearer ${this.bearerToken}`;
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
      body,
    });
  }
}
