export type CompressionStats = {
  originalTokens: number;
  compressedTokens: number;
  percentSaved: number;
};

export type McpBridgeRetryState = {
  toolName: string;
  reason: "timeout";
  retried: boolean;
  timestamp: string;
};

export type McpBridgeStatus = {
  mode: "embedded" | "adapter" | "disabled";
  connected: boolean;
  toolCount: number;
  toolNames: string[];
  /** Tools skipped because another extension already claimed the name (#359). */
  skippedTools: string[];
  /** Tools not registered because the user disabled them via config (#359). */
  disabledTools: string[];
  /** Active prefix applied to bridge tool names, if any (#359). */
  toolPrefix?: string;
  reconnectAttempts: number;
  lastError?: string;
  lastHungTool?: string;
  lastRetry?: McpBridgeRetryState;
  error?: string;
};
