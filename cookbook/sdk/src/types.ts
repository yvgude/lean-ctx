export type JsonValue =
  | null
  | boolean
  | number
  | string
  | JsonValue[]
  | { [key: string]: JsonValue };

export type JsonObject = { [key: string]: JsonValue };

export type ToolArguments = JsonObject;

export interface ListToolsResponse {
  tools: unknown[];
  total: number;
  offset: number;
  limit: number;
}

export interface ToolCallResponse {
  result: unknown;
}
