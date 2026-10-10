// SPDX-License-Identifier: Apache-2.0
/** One lean-ctx session's value snapshot (`<data>/value/sessions/<id>.json`). */
export type ValueSnap = {
  sessionId: string
  projectRoot: string
  /** The lean-ctx server process that wrote it. */
  pid?: number
  /** The agent process that started that server, when lean-ctx records it. */
  hostPid?: number
  startedAt?: string
  updatedAt?: string
  toolCalls: number
  tokensInput: number
  tokensSaved: number
  cacheHits: number
  filesRead: number
  commandsRun: number
  secretsRedacted: number
  shellBlocked: number
  pathBlocked: number
  injectionFlagged: number
}

/** `lean-ctx value --json`: both hash chains re-verified end to end. */
export type Proof = {
  isRunning: boolean
  checkedAt?: number
  ledgerEntries?: number
  ledgerIntact?: boolean
  auditEntries?: number
  auditIntact?: boolean
  /** Ledger entries recorded for this lean-ctx session. */
  sessionEntries?: number
  error?: string
}

/** What lean-ctx did during one finished main-loop turn. */
export type TurnStat = {
  n: number
  durationMs: number
  saved: number
  calls: number
  files: number
  commands: number
  /** Sources Claude saw this turn, how many through the gateway, how many as structure. */
  sources: number
  governed: number
  structure: number
  at: number
}

export type Overlay =
  | { kind: 'turn'; turn: TurnStat; at: number }
  /** A crossing worth a moment: a token amount, or a gateway first (title, detail). */
  | { kind: 'milestone'; title: string; detail: string; at: number }

/** How deep a source reached Claude: the read depth the website's flow names. */
export type Depth = 'structure' | 'passage' | 'full' | 'search' | 'command'

export type FeedItem = {
  id: string
  tool: string
  label: string
  isSubagent: boolean
  startedAt: number
  ms?: number
  isError?: boolean
  /** Set when the call brought a source to Claude. */
  depth?: Depth
  /** Through lean-ctx (the gateway) rather than one of Claude's own tools. */
  governed: boolean
}

/** One source Claude saw this conversation, by path, pattern or command. */
export type SourceStat = { label: string; depth: Depth; governed: boolean; n: number; last: number }

export type Board = {
  snap?: ValueSnap
  milestone: number
  turns: TurnStat[]
  overlay?: Overlay
  /** The counters as the running turn started. */
  turnStart?: { sessionId?: string; saved: number; calls: number; files: number; commands: number }
  isWorking: boolean
  /** lean-ctx tool calls in flight right now. */
  leanRunning: number
  proof?: Proof
  /** The session's running total over time; reset when the session changes. */
  history?: { sessionId: string; points: { at: number; saved: number }[] }
  feed: FeedItem[]
  /** Sources Claude saw this conversation, keyed by path / pattern / command. */
  sources: Record<string, SourceStat>
  /** Sources seen since the running turn started. */
  turnSeen: { sources: number; governed: number; structure: number }
  /** Gateway milestones already celebrated (first secret kept out, …). */
  celebrated: string[]
  contextPct?: number
  contextTokens?: number
  window?: number
  cacheHitPct?: number
  costUsd?: number
}

/** The animation clock: one tick per frame while anything moves. */
export type Frame = { tick: number }

declare module 'claude-code' {
  interface PluginState {
    'lean-ctx': { gateway: Board; clock: Frame }
  }
}
