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
  reconnectAttempts: number;
  lastError?: string;
  lastHungTool?: string;
  lastRetry?: McpBridgeRetryState;
  error?: string;
};
