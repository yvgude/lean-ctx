export type CompressionStats = {
  originalTokens: number;
  compressedTokens: number;
  percentSaved: number;
};

export type McpToolInterruption = {
  clientName: string;
  toolName: string;
  reason: "aborted" | "rejected" | "disconnected";
  timestamp: string;
};

export type McpBridgeStatus = {
  mode: "embedded" | "adapter" | "disabled";
  connected: boolean;
  toolCount: number;
  toolNames: string[];
  reconnectAttempts: number;
  lastError?: string;
  lastToolError?: string;
  lastCancellation?: McpToolInterruption;
  recentInterruptions: McpToolInterruption[];
  error?: string;
};
