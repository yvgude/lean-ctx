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

/**
 * The `GET /v1/capabilities` discovery document (`capabilities-contract-v1`).
 * Only the stable top-level keys are typed; the rest stays open for forward
 * compatibility.
 */
export interface CapabilitiesV1 {
  contract_version: number;
  server: { name: string; version: string; persona?: string };
  plane: string;
  transports: string[];
  presets: string[];
  read_modes: JsonValue;
  tools: { total: number; names: string[] };
  features: JsonObject;
  extensions: JsonObject;
  contracts: JsonObject;
}

export type ConsistencyLevel = 'local' | 'eventual' | 'strong';

export interface ContextEventV1 {
  id: number;
  workspaceId: string;
  channelId: string;
  kind: string;
  actor?: string | null;
  timestamp: string;
  version: number;
  parentId: number | null;
  consistencyLevel: ConsistencyLevel;
  payload: JsonValue;
}
