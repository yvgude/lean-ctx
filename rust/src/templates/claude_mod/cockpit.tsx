// SPDX-License-Identifier: Apache-2.0
import { atom, read, update } from 'claude-code'
import type { Elements, EngineInterface, On, RenderChildren } from 'claude-code'

import type { Board, Depth, FeedItem, Frame, Overlay, Proof, SourceStat, TurnStat, ValueSnap } from '../types'

// The LeanCTX cockpit, in the product's own terms: a Context Gateway between
// Claude and the sources it reads. It shows what Claude saw and how deep, the
// two checks before the handoff (may it be read? may it be delivered?), the
// tokens kept out, and the receipts behind them. A handoff line runs above the
// prompt while Claude works; short cards mark turns and milestones. lean-ctx's
// status line below the prompt keeps the one-line totals.

const PANE = 'leanctx-cockpit'
const DOCK_COLUMNS = 62
// lean-ctx writes a session's snapshot at most once a second.
const REFRESH_AFTER_TOOL_MS = 1_300
const REFRESH_MS = 15_000
// `lean-ctx value` re-verifies both hash chains end to end (~8 s): rarely, in the background.
const PROVE_FIRST_MS = 4_000
const PROVE_MS = 5 * 60_000
const PROVE_TIMEOUT_MS = 120_000
const FRAME_MS = 70
const COUNT_EASE = 0.16
const TURN_OVERLAY_MS = 6_500
const MILESTONE_OVERLAY_MS = 8_000
// A call travels the handoff line: into the gate, then on to Claude.
const PACKET_IN_MS = 700
const PACKET_OUT_MS = 1_600
// Same freshness window as lean-ctx's own status line.
const ACTIVE_MS = 12 * 3_600_000
const TURNS_KEPT = 60
const HISTORY_KEPT = 480
const FEED_KEPT = 30
const FEED_SHOWN = 5
const SOURCES_KEPT = 200
const TOKEN_MILESTONES = [10e3, 25e3, 50e3, 100e3, 250e3, 500e3, 1e6, 2.5e6, 5e6, 10e6, 25e6]
const SOURCE_MILESTONES = [25, 100, 250, 1000]
const LEAN_CTX_TOOL = /^mcp__lean[-_]ctx__/

// Keys are versioned by name: $.state outlives reloads, older shapes stay behind.
const board = atom({ plugin: 'lean-ctx', key: 'gateway' } as const, {
  milestone: 0,
  turns: [],
  isWorking: false,
  leanRunning: 0,
  feed: [],
  sources: {},
  turnSeen: { sources: 0, governed: 0, structure: 0 },
  celebrated: [],
} as Board)
const frame = atom({ plugin: 'lean-ctx', key: 'clock' } as const, { tick: 0 } as Frame)

// ── LeanCTX brand: leanctx.com premium-tokens.css (dark theme) ─────────────
const CANVAS = 0x111626
const PANEL = 0x1b2034
const FOREGROUND = 0xf1f3ff
const SECONDARY = 0xafb8d0
const HAIRLINE = 0x353e5a
const SIGNATURE = 0x668cff
const BLUE_FIELD = 0x31416c
// Derived within the family for light and motion; status colours only where earned.
const ICE = 0xc9d6ff
const VIOLET = 0xa08cff
const AMBER = 0xf5b84b
const DANGER = 0xff6b81
const OK = 0x7ee0b5
const DEFAULT_BG = 0x01000000
const BRAND_SWEEP = [BLUE_FIELD, SIGNATURE, ICE, VIOLET, SIGNATURE, BLUE_FIELD]
const DATA = [BLUE_FIELD, SIGNATURE, ICE]
const hex = (c: number) => `#${c.toString(16).padStart(6, '0')}`

// Each read depth keeps its colour everywhere: mix bar, legend, receipts.
const DEPTHS: readonly Depth[] = ['structure', 'passage', 'full', 'search', 'command']
const DEPTH_COLOR: Record<Depth, number> = {
  structure: SIGNATURE,
  passage: ICE,
  full: VIOLET,
  search: 0x7fc8ff,
  command: 0x8fa9ff,
}

function mix(a: number, b: number, t: number): number {
  const k = Math.min(1, Math.max(0, t))
  const ch = (s: number) => {
    const x = (a >> s) & 255
    const y = (b >> s) & 255
    return Math.round(x + (y - x) * k) << s
  }
  return ch(16) | ch(8) | ch(0)
}

function ramp(stops: readonly number[], t: number): number {
  const u = Math.min(1, Math.max(0, t)) * (stops.length - 1)
  const i = Math.min(stops.length - 2, Math.floor(u))
  return mix(stops[i] ?? 0, stops[i + 1] ?? 0, u - i)
}
const cycle = (stops: readonly number[], t: number) => ramp(stops, ((t % 1) + 1) % 1)

// Budgets stay brand blue and warm only under real pressure.
const pressure = (frac: number) =>
  frac < 0.7 ? SIGNATURE : frac < 0.9 ? mix(SIGNATURE, AMBER, (frac - 0.7) / 0.2) : mix(AMBER, DANGER, (frac - 0.9) / 0.1)

// ── raster cells ───────────────────────────────────────────────────────────
const B64 = 'ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/'

function toBase64(bytes: Uint8Array): string {
  let out = ''
  for (let i = 0; i < bytes.length; i += 3) {
    const a = bytes[i] ?? 0
    const b = bytes[i + 1]
    const c = bytes[i + 2]
    const n = (a << 16) | ((b ?? 0) << 8) | (c ?? 0)
    out += B64.charAt((n >> 18) & 63) + B64.charAt((n >> 12) & 63)
    out += b === undefined ? '=' : B64.charAt((n >> 6) & 63)
    out += c === undefined ? '=' : B64.charAt(n & 63)
  }
  return out
}

class Grid {
  private readonly view: DataView
  private readonly bytes: Uint8Array
  constructor(
    readonly cols: number,
    readonly rows: number,
  ) {
    this.bytes = new Uint8Array(cols * rows * 12)
    this.view = new DataView(this.bytes.buffer)
    for (let i = 0; i < cols * rows; i++) this.put(i, 0x20, DEFAULT_BG, DEFAULT_BG)
  }
  private put(i: number, ch: number, fg: number, bg: number) {
    this.view.setUint32(i * 12, ch, true)
    this.view.setUint32(i * 12 + 4, fg, true)
    this.view.setUint32(i * 12 + 8, bg, true)
  }
  set(x: number, y: number, ch: string, fg: number, bg = DEFAULT_BG) {
    if (x < 0 || y < 0 || x >= this.cols || y >= this.rows) return
    this.put(y * this.cols + x, ch.charCodeAt(0), fg, bg)
  }
  encode(): string {
    return toBase64(this.bytes)
  }
}

// The handoff line: sources on the left, Claude on the right, the gateway ◆ in
// between. While Claude works the line breathes in brand colours; every call
// travels it as a light — governed calls pass through the gate, direct calls
// (Claude's own tools) run past it dimmed, failures turn red.
function handoffCells(cols: number, tick: number, feed: readonly FeedItem[], now: number, isWorking: boolean): string {
  const g = new Grid(cols, 1)
  const gate = Math.floor(cols * 0.5)
  const breath = (Math.sin(tick * 0.09) + 1) / 2
  for (let x = 0; x < cols; x++) {
    const base = isWorking
      ? mix(HAIRLINE, cycle(BRAND_SWEEP, x / cols / 1.6 - tick * 0.004), 0.3 + breath * 0.3)
      : HAIRLINE
    g.set(x, 0, '─', base)
  }
  const gatePulse = isWorking ? (Math.sin(tick * 0.25) + 1) / 2 : 0
  const draw = (x: number, color: number, isHead: boolean) => {
    const at = Math.round(x)
    g.set(at, 0, isHead ? '●' : '━', color)
    if (isHead) {
      g.set(at - 1, 0, '━', mix(color, HAIRLINE, 0.35))
      g.set(at - 2, 0, '─', mix(color, HAIRLINE, 0.65))
    }
  }
  for (const item of feed) {
    const color = item.isError ? DANGER : item.governed ? ICE : SECONDARY
    if (item.ms === undefined) {
      const age = now - item.startedAt
      // Direct calls never enter the gate: they cross the whole line.
      if (!item.governed) {
        if (age < PACKET_IN_MS + PACKET_OUT_MS) draw(((age / (PACKET_IN_MS + PACKET_OUT_MS)) % 1) * (cols - 1), color, true)
      } else if (age < PACKET_IN_MS) draw((age / PACKET_IN_MS) * gate, color, true)
      continue
    }
    const age = now - (item.startedAt + item.ms)
    if (age < 0 || age > PACKET_OUT_MS) continue
    const from = item.governed ? gate : (age / PACKET_OUT_MS) * gate
    draw(from + ((cols - 1 - from) * age) / PACKET_OUT_MS, color, true)
  }
  const waiting = feed.some(f => f.governed && f.ms === undefined && now - f.startedAt >= PACKET_IN_MS)
  g.set(gate, 0, '◆', waiting ? mix(SIGNATURE, FOREGROUND, gatePulse) : isWorking ? mix(SIGNATURE, ICE, gatePulse * 0.6) : BLUE_FIELD)
  return g.encode()
}

// A light that sweeps once across the band as a turn card opens.
function sweepCells(cols: number, progress: number, tick: number): string {
  const g = new Grid(cols, 1)
  const head = progress * (cols + 12) - 6
  for (let x = 0; x < cols; x++) {
    const near = Math.max(0, 1 - Math.abs(x - head) / 8)
    const base = cycle(BRAND_SWEEP, x / cols / 1.5 - tick * 0.01)
    g.set(x, 0, '━', mix(mix(HAIRLINE, base, 0.75), FOREGROUND, near))
  }
  return g.encode()
}

// Twinkling field for milestones: deterministic per cell, phased by the tick.
function sparkleCells(cols: number, tick: number, seed: number): string {
  const g = new Grid(cols, 1)
  const glyphs = ['·', '✧', '✦', '✧', '·']
  for (let x = 0; x < cols; x++) {
    const h = Math.sin((x + 1) * 12.9898 + seed * 78.233) * 43758.5453
    const r = h - Math.floor(h)
    if (r < 0.55) continue
    const phase = (Math.sin(tick * 0.35 + r * 40) + 1) / 2
    g.set(x, 0, glyphs[Math.floor(phase * (glyphs.length - 1) + 0.5)] ?? '·', mix(BLUE_FIELD, r > 0.8 ? ICE : SIGNATURE, phase))
  }
  return g.encode()
}

// 4×7 display figures, folded into half blocks: four terminal rows.
const FONT: Record<string, readonly string[]> = {
  '0': ['.##.', '#..#', '#..#', '#..#', '#..#', '#..#', '.##.'],
  '1': ['.#..', '##..', '.#..', '.#..', '.#..', '.#..', '###.'],
  '2': ['.##.', '#..#', '...#', '..#.', '.#..', '#...', '####'],
  '3': ['###.', '...#', '...#', '.##.', '...#', '...#', '###.'],
  '4': ['#..#', '#..#', '#..#', '####', '...#', '...#', '...#'],
  '5': ['####', '#...', '###.', '...#', '...#', '#..#', '.##.'],
  '6': ['.##.', '#...', '#...', '###.', '#..#', '#..#', '.##.'],
  '7': ['####', '...#', '..#.', '..#.', '.#..', '.#..', '.#..'],
  '8': ['.##.', '#..#', '#..#', '.##.', '#..#', '#..#', '.##.'],
  '9': ['.##.', '#..#', '#..#', '.###', '...#', '...#', '.##.'],
  '.': ['.', '.', '.', '.', '.', '.', '#'],
  K: ['#..#', '#.#.', '##..', '#...', '##..', '#.#.', '#..#'],
  M: ['#...#', '##.##', '#.#.#', '#...#', '#...#', '#...#', '#...#'],
  B: ['###.', '#..#', '#..#', '###.', '#..#', '#..#', '###.'],
}
const FIGURE_ROWS = 4
const figureWidth = (text: string) => [...text].reduce((w, ch) => w + (FONT[ch]?.[0]?.length ?? 0) + 1, -1)

// A display figure: a slow brand sweep runs across it; brighter while it counts.
function figureCells(text: string, cols: number, phase: number, lift: number): string {
  const g = new Grid(cols, FIGURE_ROWS)
  const width = Math.max(8, figureWidth(text))
  let x0 = 0
  for (const ch of text) {
    const glyph = FONT[ch]
    if (!glyph) continue
    const w = glyph[0]?.length ?? 0
    for (let x = 0; x < w; x++) {
      const color = mix(cycle(BRAND_SWEEP, (x0 + x) / width / 2.2 - phase), FOREGROUND, lift)
      for (let r = 0; r < FIGURE_ROWS; r++) {
        const top = glyph[r * 2]?.charAt(x) === '#'
        const bottom = glyph[r * 2 + 1]?.charAt(x) === '#'
        const cell = top && bottom ? '█' : top ? '▀' : bottom ? '▄' : ''
        if (cell) g.set(x0 + x, r, cell, color)
      }
    }
    x0 += w + 1
  }
  return g.encode()
}

const EIGHTHS = ['', '▏', '▎', '▍', '▌', '▋', '▊', '▉']

// Meter: fractional blocks on a dim track, a gradient along the fill.
function meterCells(cols: number, frac: number, color: (t: number) => number): string {
  const g = new Grid(cols, 1)
  const filled = Math.min(1, Math.max(0, frac)) * cols
  const full = Math.floor(filled)
  const part = Math.floor((filled - full) * 8)
  for (let x = 0; x < cols; x++) {
    const c = color(x / Math.max(1, cols - 1))
    if (x < full) g.set(x, 0, '█', c)
    else if (x === full && part > 0) g.set(x, 0, EIGHTHS[part] ?? '▏', c, PANEL)
    else g.set(x, 0, '█', PANEL)
  }
  return g.encode()
}

// The depth mix: one bar, a segment per read depth in its colour.
function mixCells(cols: number, counts: Record<Depth, number>): string {
  const g = new Grid(cols, 1)
  const total = DEPTHS.reduce((n, d) => n + counts[d], 0)
  if (total === 0) {
    for (let x = 0; x < cols; x++) g.set(x, 0, '█', PANEL)
    return g.encode()
  }
  let x = 0
  DEPTHS.forEach((d, i) => {
    const isLast = i === DEPTHS.length - 1
    const width = isLast ? cols - x : Math.round((counts[d] / total) * cols)
    for (let k = 0; k < width && x < cols; k++, x++) g.set(x, 0, '█', DEPTH_COLOR[d])
  })
  return g.encode()
}

const LEVELS = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█']

// Area chart: filled columns rising from blue field to ice; the newest column bright.
function areaCells(cols: number, rows: number, values: readonly number[], pulse: number): string {
  const g = new Grid(cols, rows)
  const shown = values.slice(-cols)
  const min = Math.min(...shown)
  const max = Math.max(min + 1, ...shown)
  const offset = cols - shown.length
  for (let x = 0; x < cols; x++) g.set(x, rows - 1, '▁', HAIRLINE)
  shown.forEach((v, i) => {
    // A running total: scale from the window's floor so its growth reads.
    const height = Math.max(1, ((v - min) / (max - min)) * (rows * 8 - 1) + 1)
    const isLast = i === shown.length - 1
    for (let r = 0; r < rows; r++) {
      const level = Math.min(8, Math.max(0, Math.round(height - r * 8)))
      if (level === 0) continue
      const base = ramp(DATA, (r * 8 + level) / (rows * 8))
      g.set(offset + i, rows - 1 - r, LEVELS[level - 1] ?? '█', isLast ? mix(base, FOREGROUND, 0.4 + pulse * 0.6) : base)
    }
  })
  return g.encode()
}

// ── formatting (lean-ctx's own style: 49.1K, 1.2M) ─────────────────────────
function compact(n: number): string {
  const a = Math.abs(n)
  const f = (v: number, s: string) => `${v >= 100 ? v.toFixed(0) : v.toFixed(1)}${s}`
  if (a >= 1e9) return f(n / 1e9, 'B')
  if (a >= 1e6) return f(n / 1e6, 'M')
  if (a >= 1e3) return f(n / 1e3, 'K')
  return Math.round(n).toString()
}
const grouped = (n: number) => Math.round(n).toLocaleString('en-US')
const pct = (n: number | undefined) => (n === undefined ? '—' : `${Math.round(n)}%`)
const dur = (ms: number) => (ms < 1000 ? `${ms}ms` : ms < 60_000 ? `${(ms / 1000).toFixed(1)}s` : `${Math.round(ms / 60_000)}m`)
// The engine's own turn-duration style: 3s, 1m 4s.
function clock(ms: number): string {
  const s = Math.max(0, Math.round(ms / 1000))
  return s < 60 ? `${s}s` : `${Math.floor(s / 60)}m ${s % 60}s`
}
function ago(ms: number): string {
  const s = Math.max(0, Math.round(ms / 1000))
  return s < 60 ? `${s}s ago` : s < 3600 ? `${Math.round(s / 60)}m ago` : `${Math.round(s / 3600)}h ago`
}
function span(ms: number): string {
  const m = Math.max(0, Math.round(ms / 60_000))
  return m < 60 ? `${m}m` : `${Math.floor(m / 60)}h ${m % 60}m`
}
const plural = (n: number, one: string, many: string) => `${compact(n)} ${n === 1 ? one : many}`

function shortTool(tool: string): string {
  return /^mcp__.+?__(.+)$/.exec(tool)?.[1] ?? tool
}

function tail(path: string): string {
  return path.split('/').filter(Boolean).at(-1) ?? path
}

type Rec = Record<string, unknown>
const isRec = (v: unknown): v is Rec => typeof v === 'object' && v !== null && !Array.isArray(v)
const num = (r: Rec, k: string) => (typeof r[k] === 'number' ? (r[k] as number) : 0)
const str = (r: Rec, k: string) => (typeof r[k] === 'string' ? (r[k] as string) : undefined)

function describe(args: Rec): string {
  for (const key of ['path', 'file_path', 'command', 'pattern', 'query', 'task', 'symbol', 'url', 'description', 'prompt']) {
    const v = args[key]
    if (typeof v !== 'string' || v.length === 0) continue
    const line = v.split('\n')[0] ?? ''
    return key.endsWith('path') ? tail(line) : line.slice(0, 80)
  }
  const paths = args['paths']
  if (Array.isArray(paths) && typeof paths[0] === 'string') return `${tail(paths[0])} +${paths.length - 1}`
  return ''
}

// The read depth a call brought a source to Claude at, or none for calls that
// bring no source (edits, sessions, agents). `ctx_read` modes map onto the
// website's depths: structure (map, signatures), a passage (lines, reference,
// task, compressed modes), or the whole file.
function depthOf(tool: string, args: Rec): Depth | undefined {
  const t = shortTool(tool)
  if (/^(ctx_read|ctx_multi_read|Read)$/.test(t)) {
    const mode = str(args, 'mode') ?? ''
    if (/^(map|signatures)$/.test(mode)) return 'structure'
    if (/^(lines|reference|task|aggressive|entropy|diff)/.test(mode)) return 'passage'
    return 'full'
  }
  if (/search|grep|glob|tree|compose|callgraph|graph|semantic/i.test(t)) return 'search'
  if (/shell|bash|execute/i.test(t)) return 'command'
  return undefined
}

const SPINNER = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏']

// ── lean-ctx value snapshots ───────────────────────────────────────────────
function parseSnap(text: string): ValueSnap | undefined {
  const r: unknown = JSON.parse(text)
  if (!isRec(r)) return undefined
  const sessionId = str(r, 'session_id')
  const projectRoot = str(r, 'project_root')
  if (!sessionId || !projectRoot) return undefined
  const sec = isRec(r['security']) ? r['security'] : {}
  // lean-ctx session ids end in `p<pid>s<n>`; newer builds also record `host_pid`.
  const pid = /p(\d+)s\d+$/.exec(sessionId)?.[1]
  return {
    sessionId,
    projectRoot,
    pid: pid ? Number(pid) : undefined,
    hostPid: typeof r['host_pid'] === 'number' ? (r['host_pid'] as number) : undefined,
    startedAt: str(r, 'started_at'),
    updatedAt: str(r, 'updated_at'),
    toolCalls: num(r, 'tool_calls'),
    tokensInput: num(r, 'tokens_input'),
    tokensSaved: num(r, 'tokens_saved'),
    cacheHits: num(r, 'cache_hits'),
    filesRead: num(r, 'files_read'),
    commandsRun: num(r, 'commands_run'),
    secretsRedacted: num(sec, 'secrets_redacted'),
    shellBlocked: num(sec, 'shell_blocked'),
    pathBlocked: num(sec, 'path_blocked'),
    injectionFlagged: num(sec, 'injection_flagged'),
  }
}

const inProject = (cwd: string, root: string) => cwd === root || cwd.startsWith(`${root}/`)
const newest = (snaps: ValueSnap[]) => [...snaps].sort((a, b) => (b.updatedAt ?? '').localeCompare(a.updatedAt ?? ''))[0]

const countersOf = (s: ValueSnap | undefined) => ({
  sessionId: s?.sessionId,
  saved: s?.tokensSaved ?? 0,
  calls: s?.toolCalls ?? 0,
  files: s?.filesRead ?? 0,
  commands: s?.commandsRun ?? 0,
})

// ── module state (a reload starts it over; what drawing reads lives in $.state) ──
let valueDir: string | undefined
let cwd: string | undefined
let refreshing = false
let refreshAgain = false
let pendingRefresh: { cancel: () => void } | undefined
let frames: { cancel: () => void } | undefined
let isWorking = false
let overlayUntil = 0
let packetsUntil = 0
let proving = false
// The token figure counts toward the session total.
let shown = -1
let target = 0

// $.state outlives reloads: fill in anything an older build left out.
async function readBoard($: EngineInterface): Promise<Board> {
  const b: Partial<Board> = (await read($, board)) ?? {}
  return {
    ...b,
    milestone: typeof b.milestone === 'number' ? b.milestone : 0,
    turns: Array.isArray(b.turns) ? b.turns : [],
    isWorking: b.isWorking === true,
    leanRunning: typeof b.leanRunning === 'number' ? Math.max(0, b.leanRunning) : 0,
    feed: Array.isArray(b.feed) ? b.feed : [],
    sources: isRec(b.sources) ? (b.sources as Record<string, SourceStat>) : {},
    turnSeen: isRec(b.turnSeen) ? b.turnSeen : { sources: 0, governed: 0, structure: 0 },
    celebrated: Array.isArray(b.celebrated) ? b.celebrated : [],
  }
}

// The data directory lean-ctx reports itself, so XDG and custom homes hold.
async function locate($: EngineInterface): Promise<{ dir: string; cwd: string }> {
  if (!valueDir) {
    const { exitCode, stdout } = await $.process.run(['lean-ctx', 'prompt-segment', '--json'], { timeoutMs: 15_000 })
    if (exitCode !== 0) throw new Error('lean-ctx prompt-segment failed')
    const parsed: unknown = JSON.parse(stdout)
    const watch = isRec(parsed) ? str(parsed, 'watch') : undefined
    if (!watch) throw new Error('lean-ctx reports no value directory')
    valueDir = watch.replace(/\/projects\/?$/, '')
  }
  cwd ??= await $.session.cwd()
  return { dir: valueDir, cwd }
}

// Our ancestors, nearest first, each with the lean-ctx servers it started.
// The agent process is among them, whether hooks run in it or in a worker.
type Ancestry = { host: number; servers: Set<number> }[]
let chain: Ancestry | undefined
async function ancestry($: EngineInterface): Promise<Ancestry> {
  const [table, self] = await Promise.all([
    $.process.run(['ps', '-A', '-o', 'pid=,ppid=,comm='], { timeoutMs: 5_000 }),
    $.process.run(['sh', '-c', 'echo $PPID'], { timeoutMs: 5_000 }),
  ])
  const parent = new Map<number, number>()
  const servers = new Map<number, Set<number>>()
  for (const line of table.stdout.split('\n')) {
    const m = /^\s*(\d+)\s+(\d+)\s+(.*)$/.exec(line)
    if (!m) continue
    const [pid, ppid] = [Number(m[1]), Number(m[2])]
    parent.set(pid, ppid)
    if (/(^|\/)lean-ctx$/.test(m[3] ?? '')) servers.set(ppid, (servers.get(ppid) ?? new Set()).add(pid))
  }
  const found: Ancestry = []
  for (let pid = Number(self.stdout.trim()); pid > 1 && found.length < 8; pid = parent.get(pid) ?? 0) {
    if (found.some(a => a.host === pid)) break
    found.push({ host: pid, servers: servers.get(pid) ?? new Set() })
  }
  return found
}

function scheduleRefresh($: EngineInterface) {
  if (pendingRefresh) return
  pendingRefresh = $.clock.after(REFRESH_AFTER_TOOL_MS, () => {
    pendingRefresh = undefined
    void refresh($).catch(() => {})
  })
}

// A gateway first worth a moment, celebrated once per conversation.
function gatewayFirst(prev: ValueSnap | undefined, snap: ValueSnap, sources: number, done: string[]): { key: string; title: string; detail: string } | undefined {
  const firsts = [
    {
      key: 'first-secret',
      hit: snap.secretsRedacted > 0 && (prev?.secretsRedacted ?? 0) === 0,
      title: 'FIRST SECRET KEPT OUT OF CLAUDE’S CONTEXT',
      detail: 'Redacted before the handoff · see lean-ctx value',
    },
    {
      key: 'first-block',
      hit: snap.shellBlocked + snap.pathBlocked > 0 && (prev ? prev.shellBlocked + prev.pathBlocked : 0) === 0,
      title: 'FIRST READ STOPPED AT THE GATE',
      detail: 'A command or path outside your rules was blocked',
    },
    ...SOURCE_MILESTONES.map(n => ({
      key: `sources-${n}`,
      hit: sources >= n,
      title: `${grouped(n)} SOURCES THROUGH THE GATEWAY`,
      detail: 'Each one selected, checked and recorded',
    })),
  ]
  return firsts.find(f => f.hit && !done.includes(f.key))
}

// Reads the snapshot of the lean-ctx session serving this conversation (the
// same one lean-ctx's status line shows), keeps its history, raises milestones.
async function refresh($: EngineInterface) {
  if (refreshing) {
    refreshAgain = true
    return
  }
  refreshing = true
  try {
    // Claude's side (window, cost) rides along: refresh only runs from timers.
    await refreshUsage($)
    const where = await locate($)
    const sessions = `${where.dir}/sessions`
    const now = Date.now()
    const entries = (await $.fs.list(sessions)).filter(
      e => e.kind === 'file' && e.name.endsWith('.json') && now - e.mtimeMs < ACTIVE_MS,
    )
    const snaps: ValueSnap[] = []
    for (const e of entries) {
      try {
        const s = parseSnap(await $.fs.read(`${sessions}/${e.name}`))
        if (s && inProject(where.cwd, s.projectRoot)) snaps.push(s)
      } catch {
        // a snapshot mid-write or gone: skip it this round
      }
    }
    // Nearest ancestor first: its recorded `host_pid`, or (older lean-ctx) the
    // pid of a lean-ctx server it started, which session ids end with.
    chain ??= await ancestry($)
    let snap: ValueSnap | undefined
    for (const a of chain) {
      snap = newest(snaps.filter(s => s.hostPid === a.host || (s.pid !== undefined && a.servers.has(s.pid))))
      if (snap) break
    }
    if (!snap) {
      chain = undefined // a reconnect starts a new server: look again next time
      return
    }

    const b = await readBoard($)
    const at = Date.now()
    const governedSources = Object.values(b.sources).filter(s => s.governed).length
    // The first look only records where the session stands; later crossings celebrate.
    const isLive = b.milestone > 0 || b.turns.length > 0 || b.snap !== undefined
    const reached = TOKEN_MILESTONES.filter(m => m <= snap.tokensSaved).pop() ?? 0
    const first = isLive && b.snap?.sessionId === snap.sessionId ? gatewayFirst(b.snap, snap, governedSources, b.celebrated) : undefined
    let celebration: Overlay | undefined
    if (first) {
      celebration = { kind: 'milestone', title: first.title, detail: first.detail, at }
    } else if (reached > b.milestone && isLive) {
      celebration = {
        kind: 'milestone',
        title: `${compact(reached)} TOKENS KEPT OUT OF CONTEXT`,
        detail: 'Selected, not sent · numbers from lean-ctx value',
        at,
      }
    }
    await update($, board, x => {
      const same = x.history?.sessionId === snap.sessionId ? x.history.points : []
      const last = same.at(-1)
      // A point per change, and one a minute while flat, so time stays honest.
      const isNew = !last || last.saved !== snap.tokensSaved || at - last.at >= 60_000
      const points = isNew ? [...same, { at, saved: snap.tokensSaved }].slice(-HISTORY_KEPT) : same
      return {
        ...x,
        snap,
        history: { sessionId: snap.sessionId, points },
        milestone: Math.max(x.milestone ?? 0, reached),
        celebrated: first ? [...(Array.isArray(x.celebrated) ? x.celebrated : []), first.key] : x.celebrated,
        overlay: celebration ?? x.overlay,
      }
    })
    if (shown < 0) shown = 0
    target = snap.tokensSaved
    if (celebration) {
      overlayUntil = at + MILESTONE_OVERLAY_MS
      const title = celebration.kind === 'milestone' ? celebration.title : ''
      $.ui.toast(`★ LeanCTX · ${title.toLowerCase()}`, { timeoutMs: 6000 })
    }
    if (celebration || shown !== target) startFrames($)
  } catch {
    // the cockpit is cosmetic; lean-ctx's status line still carries the numbers
  } finally {
    refreshing = false
    if (refreshAgain) {
      refreshAgain = false
      scheduleRefresh($)
    }
  }
}

// Prove: re-derive the evidence from both hash chains, the way `lean-ctx value` does.
async function prove($: EngineInterface) {
  if (proving) return
  proving = true
  const before = (await readBoard($)).proof
  await update($, board, x => ({ ...x, proof: { ...(x.proof ?? {}), isRunning: true } }))
  try {
    const b = await readBoard($)
    const argv = ['lean-ctx', 'value', '--json', ...(b.snap ? ['--session', b.snap.sessionId] : [])]
    const { stdout } = await $.process.run(argv, { timeoutMs: PROVE_TIMEOUT_MS })
    const r: unknown = JSON.parse(stdout)
    if (!isRec(r)) throw new Error('lean-ctx value returned no report')
    const ledger = isRec(r['ledger']) ? r['ledger'] : {}
    const audit = isRec(r['audit']) ? r['audit'] : {}
    const evidence = isRec(r['ledger_evidence']) ? r['ledger_evidence'] : {}
    const proof: Proof = {
      isRunning: false,
      checkedAt: Date.now(),
      ledgerEntries: num(ledger, 'entries'),
      ledgerIntact: ledger['intact'] === true,
      auditEntries: num(audit, 'entries'),
      auditIntact: audit['intact'] === true,
      sessionEntries: num(evidence, 'entries'),
    }
    await update($, board, x => ({ ...x, proof }))
  } catch (err) {
    const message = err instanceof Error ? err.message : String(err)
    await update($, board, x => ({ ...x, proof: { ...(before ?? {}), isRunning: false, error: message } }))
  } finally {
    proving = false
  }
}

async function refreshUsage($: EngineInterface) {
  try {
    const u = await $.session.usage()
    await update($, board, x => ({
      ...x,
      contextPct: u.context.percent,
      contextTokens: u.context.tokens,
      window: u.context.window,
      costUsd: u.cost?.usd ?? x.costUsd,
    }))
  } catch {
    // usage is cosmetic; never disturb the session for it
  }
}

// The turn just ended: sum up what lean-ctx did and what Claude saw. Runs from a
// `$.clock.after` timer (scheduled by the `turn.complete` hook) once lean-ctx's
// last write is in: work that outlives a hook's dispatch must not hang off the
// hook itself. Without a snapshot the card still shows, with the turn alone.
async function finishTurn($: EngineInterface, durationMs: number) {
  await refresh($).catch(() => {})
  const b = await readBoard($)
  const now = countersOf(b.snap)
  // A turn that spans a session change (a reconnect) counts from the new
  // session's start, never as the difference of two sessions' counters.
  const start =
    b.turnStart && b.turnStart.sessionId === now.sessionId
      ? b.turnStart
      : { sessionId: now.sessionId, saved: 0, calls: 0, files: 0, commands: 0 }
  const turn: TurnStat = {
    n: b.turns.length + 1,
    durationMs,
    saved: Math.max(0, now.saved - start.saved),
    calls: Math.max(0, now.calls - start.calls),
    files: Math.max(0, now.files - start.files),
    commands: Math.max(0, now.commands - start.commands),
    sources: b.turnSeen.sources,
    governed: b.turnSeen.governed,
    structure: b.turnSeen.structure,
    at: Date.now(),
  }
  await update($, board, x => {
    // A milestone on show keeps the band; the turn card does not cut it short.
    const isMilestoneShowing = x.overlay?.kind === 'milestone' && Date.now() - x.overlay.at < MILESTONE_OVERLAY_MS
    const overlay: Overlay | undefined = isMilestoneShowing ? x.overlay : { kind: 'turn', turn, at: Date.now() }
    const turns = [...(Array.isArray(x.turns) ? x.turns : []), turn].slice(-TURNS_KEPT)
    return { ...x, turns, overlay }
  })
  overlayUntil = Math.max(overlayUntil, Date.now() + TURN_OVERLAY_MS)
  startFrames($)
}

// Tool calls as the conversation records them: an assistant row's `tool_use`
// blocks start calls (with the source and depth they bring), a `tool_result`
// ends one. Reading rows keeps the cockpit off the tool chain the rest of the
// mod shapes.
async function observeRow($: EngineInterface, row: Rec) {
  const message = isRec(row['message']) ? row['message'] : {}
  const blocks = (Array.isArray(message['content']) ? message['content'] : []).filter(isRec)
  const now = Date.now()
  const started: FeedItem[] = []
  const ended = new Map<string, boolean>()
  for (const block of blocks) {
    if (block['type'] === 'tool_use' && typeof block['id'] === 'string' && typeof block['name'] === 'string') {
      const input = isRec(block['input']) ? block['input'] : {}
      started.push({
        id: block['id'],
        tool: block['name'],
        label: describe(input),
        isSubagent: typeof row['agentId'] === 'string',
        startedAt: now,
        depth: depthOf(block['name'], input),
        governed: LEAN_CTX_TOOL.test(block['name']),
      })
    } else if (block['type'] === 'tool_result' && typeof block['tool_use_id'] === 'string') {
      ended.set(block['tool_use_id'], block['is_error'] === true)
    }
  }
  if (started.length === 0 && ended.size === 0) return
  await update($, board, x => {
    const feed = [...(Array.isArray(x.feed) ? x.feed : []), ...started].slice(-FEED_KEPT)
    let running = (x.leanRunning ?? 0) + started.filter(f => f.governed).length
    const done = feed.map(f => {
      const isError = ended.get(f.id)
      if (isError === undefined || f.ms !== undefined) return f
      if (f.governed) running -= 1
      return { ...f, ms: now - f.startedAt, isError }
    })
    // Sources Claude saw: one entry per path / pattern / command, deepest read kept.
    const sources = { ...(isRec(x.sources) ? (x.sources as Record<string, SourceStat>) : {}) }
    const seen = isRec(x.turnSeen) ? { ...x.turnSeen } : { sources: 0, governed: 0, structure: 0 }
    for (const f of started) {
      if (!f.depth || !f.label) continue
      const key = `${f.depth === 'command' ? 'cmd' : 'src'}:${f.label}`
      const prev = sources[key]
      sources[key] = { label: f.label, depth: f.depth, governed: f.governed || (prev?.governed ?? false), n: (prev?.n ?? 0) + 1, last: now }
      seen.sources += 1
      if (f.governed) seen.governed += 1
      if (f.depth === 'structure') seen.structure += 1
    }
    const kept = Object.entries(sources)
      .sort(([, a], [, b]) => b.last - a.last)
      .slice(0, SOURCES_KEPT)
    return { ...x, feed: done, leanRunning: Math.max(0, running), sources: Object.fromEntries(kept), turnSeen: seen }
  })
  // Keep the handoff line moving while calls travel it.
  packetsUntil = now + PACKET_IN_MS + PACKET_OUT_MS
  startFrames($)
  if (ended.size > 0) scheduleRefresh($)
}

// One frame clock for every effect; it runs only while something moves.
function startFrames($: EngineInterface) {
  if (frames) return
  frames = $.clock.every(FRAME_MS, () => {
    if (shown >= 0 && shown !== target) {
      const gap = target - shown
      const step = Math.sign(gap) * Math.max(1, Math.abs(gap) * COUNT_EASE)
      shown = Math.abs(step) >= Math.abs(gap) ? target : Math.round(shown + step)
    }
    const now = Date.now()
    if (!isWorking && !proving && shown === target && now > overlayUntil && now > packetsUntil) {
      frames?.cancel()
      frames = undefined
    }
    void update($, frame, f => ({ tick: (f?.tick ?? 0) + 1 })).catch(() => {})
  })
}

// Opens the sidebar and starts the refresh and proof clocks.
async function cockpitStart($: EngineInterface): Promise<void> {
  await $.command.register({ name: 'cockpit', description: 'Open the LeanCTX cockpit' })
  $.ui.status(undefined)
  // Fire-and-forget, and never an unhandled rejection: a surface that draws
  // no panes (or none yet) simply leaves the sidebar closed.
  $.ui.open({ id: PANE, title: 'LeanCTX', columns: DOCK_COLUMNS }).catch(() => {})
  // Work that outlives this hook runs from timers, never off the hook itself.
  $.clock.after(300, () => void refresh($).catch(() => {}))
  $.clock.every(REFRESH_MS, () => void refresh($).catch(() => {}))
  $.clock.after(PROVE_FIRST_MS, () => void prove($).catch(() => {}))
  $.clock.every(PROVE_MS, () => void prove($).catch(() => {}))
}

// ── drawing helpers ────────────────────────────────────────────────────────
type Ui = Elements['terminal']

// Text with a soft light travelling through it (Charm-style shimmer).
function shimmer(ui: Ui, text: string, tick: number, base: number, light: number, bold = false) {
  const { Text } = ui
  const head = (tick * 0.9) % (text.length + 14)
  return (
    <Text bold={bold}>
      {[...text].map((ch, i) => (
        <Text color={hex(mix(base, light, Math.max(0, 1 - Math.abs(i - head + 7) / 5)))}>{ch}</Text>
      ))}
    </Text>
  )
}

// A pill: a short label on a filled ground, the way Lip Gloss badges sit.
function pill(ui: Ui, text: string, fg: number, bg: number, bold = false) {
  const { Text } = ui
  return (
    <Text color={hex(fg)} backgroundColor={hex(bg)} bold={bold}>
      {` ${text} `}
    </Text>
  )
}

type Part = { node: RenderChildren; rows: number }
const GAP: Part = { node: '', rows: 1 }

// A panel, btop-style: the title sits in the top border (╭─┐TITLE┌──── meta ─╮),
// a blank row of air inside top and bottom, and a blank row before the next.
function panel(ui: Ui, width: number, title: string, meta: string, parts: Part[]) {
  const { Box, Text } = ui
  const inner = width - 4
  const line = hex(HAIRLINE)
  const metaText = meta ? ` ${meta} ` : ''
  const fill = Math.max(1, width - 6 - title.length - metaText.length - 1)
  const side = (rows: number) => <Text color={line}>{Array.from({ length: rows }, () => '│').join('\n')}</Text>
  return (
    <Box flexDirection="column" marginTop={1}>
      <Text>
        <Text color={line}>╭─┐</Text>
        <Text bold color={hex(SIGNATURE)}>
          {title}
        </Text>
        <Text color={line}>┌{'─'.repeat(fill)}</Text>
        <Text color={hex(SECONDARY)}>{metaText}</Text>
        <Text color={line}>─╮</Text>
      </Text>
      {[GAP, ...parts, GAP].map(p => (
        <Box>
          {side(p.rows)}
          <Box width={inner + 2} paddingX={1} flexDirection="column">
            {p.node === '' ? <Text> </Text> : p.node}
          </Box>
          {side(p.rows)}
        </Box>
      ))}
      <Text color={line}>╰{'─'.repeat(width - 2)}╯</Text>
    </Box>
  )
}

// The handoff line with its two ends named: SOURCES ──◆── CLAUDE.
function handoff(ui: Ui, key: string, cols: number, b: Board, tick: number, now: number) {
  const { Box, Text, Raster } = ui
  const lane = Math.max(8, cols - 16)
  const ends = hex(b.isWorking ? SECONDARY : HAIRLINE)
  return (
    <Box>
      <Text color={ends}>sources </Text>
      <Raster key={key} columns={lane} rows={1} cells={handoffCells(lane, tick, b.feed, now, b.isWorking)} />
      <Text color={ends}> claude</Text>
    </Box>
  )
}

// ── the cockpit ────────────────────────────────────────────────────────────
export function registerCockpit(on: On): void {
  // A matcher, because the mod's own `session.start` hook has none (one per
  // event and plugin); headless runs (`claude -p`) get no cockpit at all.
  on('session.start', { isInteractive: true }, async ($, e, next) => {
    try {
      await cockpitStart($)
    } catch {
      // the cockpit is cosmetic; the session starts regardless
    }
    return next(e)
  })

  on('command.run', { command: 'cockpit' }, async $ => {
    const { isPlaced } = await $.ui.open({ id: PANE, title: 'LeanCTX', columns: DOCK_COLUMNS })
    $.clock.after(0, () => void refresh($).catch(() => {}))
    $.clock.after(0, () => void prove($).catch(() => {}))
    return { text: isPlaced ? 'LeanCTX cockpit opened.' : 'LeanCTX cockpit is waiting for room (fullscreen, ~110+ columns).' }
  })

  on('turn.start', async ($, e, next) => {
    try {
      isWorking = true
      const b = await readBoard($)
      await update($, board, x => ({
        ...x,
        isWorking: true,
        leanRunning: 0,
        turnStart: countersOf(b.snap),
        turnSeen: { sources: 0, governed: 0, structure: 0 },
      }))
      startFrames($)
    } catch {
      // the cockpit is cosmetic
    }
    return next(e)
  })

  on('turn.complete', async ($, e, next) => {
    if (e.agentId) return next(e)
    isWorking = false
    const usage = 'usage' in e ? e.usage : undefined
    await update($, board, x => {
      let cacheHitPct = x.cacheHitPct
      if (usage) {
        const total = usage.input_tokens + usage.cache_read_input_tokens + usage.cache_creation_input_tokens
        if (total > 0) cacheHitPct = (usage.cache_read_input_tokens / total) * 100
      }
      return { ...x, isWorking: false, leanRunning: 0, cacheHitPct }
    })
    if (!e.isAborted) {
      const durationMs = e.durationMs
      $.clock.after(REFRESH_AFTER_TOOL_MS, () => void finishTurn($, durationMs).catch(() => {}))
    }
    return next(e)
  })

  // The row is stored first and handed back unchanged; the cockpit only reads it.
  on('session.append', async ($, e, next) => {
    const stored = await next(e)
    try {
      await observeRow($, e as unknown as Rec)
    } catch {
      // the cockpit is cosmetic
    }
    return stored
  })

  // ── above the prompt: the handoff line while working; a card when a turn ends or a milestone falls
  on('ui.render', { component: 'AbovePrompt' }, async ($, e, next) => {
    if (e.props.hasSurvey || e.surface !== 'terminal') return next(e)
    const b = await readBoard($)
    const f = await read($, frame)
    const tick = f?.tick ?? 0
    const now = Date.now()
    const o = b.overlay
    const ui = $.ui.resolve(e)
    const { Box, Text, Raster } = ui
    const cols = Math.max(24, e.props.bodyColumns)

    if (o?.kind === 'milestone' && now - o.at < MILESTONE_OVERLAY_MS && typeof o.title === 'string') {
      const title = `★  ${o.title}  ★`
      return (
        <Box flexDirection="column">
          <Raster key="spark-a" columns={cols} rows={1} cells={sparkleCells(cols, tick, 1)} />
          <Box justifyContent="center">{shimmer(ui, title, tick, SIGNATURE, FOREGROUND, true)}</Box>
          <Box justifyContent="center">
            <Text color={hex(SECONDARY)}>LeanCTX · {o.detail}</Text>
          </Box>
          <Raster key="spark-b" columns={cols} rows={1} cells={sparkleCells(cols, tick + 7, 2)} />
        </Box>
      )
    }

    if (o?.kind === 'turn' && now - o.at < TURN_OVERLAY_MS && !b.isWorking) {
      const t = o.turn
      const gap = <Text> </Text>
      return (
        <Box flexDirection="column">
          <Raster key="sweep" columns={cols} rows={1} cells={sweepCells(cols, Math.min(1, (now - o.at) / 900), tick)} />
          <Box>
            <Text bold color={hex(OK)}>
              ✓{' '}
            </Text>
            <Text bold color={hex(FOREGROUND)}>
              Turn {t.n}
            </Text>
            <Text color={hex(SECONDARY)}> · {clock(t.durationMs)}   </Text>
            {t.sources > 0 ? pill(ui, `Claude saw ${plural(t.sources, 'source', 'sources')}`, CANVAS, SIGNATURE, true) : null}
            {t.sources > 0 && t.structure > 0 ? gap : null}
            {t.sources > 0 && t.structure > 0 ? pill(ui, `${t.structure} as structure`, FOREGROUND, PANEL) : null}
            {t.saved > 0 ? gap : null}
            {t.saved > 0 ? pill(ui, `▲ ${compact(t.saved)} tokens kept out`, FOREGROUND, BLUE_FIELD, true) : null}
            {t.sources === 0 && t.saved === 0 ? pill(ui, plural(t.calls, 'lean-ctx call', 'lean-ctx calls'), FOREGROUND, PANEL) : null}
          </Box>
        </Box>
      )
    }

    if (b.isWorking) return handoff(ui, 'band-handoff', cols, b, tick, now)
    return next(e)
  })

  // ── the spinner row: brand spinner, shimmering words, the turn's live gateway work
  on('ui.render', { component: 'Spinner' }, async ($, e, next) => {
    if (e.surface !== 'terminal') return next(e)
    const f = await read($, frame)
    const b = await readBoard($)
    const ui = $.ui.resolve(e)
    const { Box, Text } = ui
    const tick = f?.tick ?? 0
    const words = `${e.props.message ?? e.props.word}${e.props.suffix}`
    const start = b.turnStart
    const gained = start && b.snap && start.sessionId === b.snap.sessionId ? Math.max(0, b.snap.tokensSaved - start.saved) : 0
    const seen = b.turnSeen.sources
    return (
      <Box>
        <Text bold color={hex(cycle(BRAND_SWEEP, tick / 30))}>
          {SPINNER[tick % SPINNER.length]}{' '}
        </Text>
        {shimmer(ui, words, tick, SECONDARY, ICE)}
        {seen > 0 || gained > 0 ? <Text>   </Text> : null}
        {seen > 0 ? pill(ui, `◆ ${plural(seen, 'source', 'sources')}`, FOREGROUND, PANEL) : null}
        {seen > 0 && gained > 0 ? <Text> </Text> : null}
        {gained > 0 ? pill(ui, `▲ ${compact(gained)} kept out`, FOREGROUND, BLUE_FIELD) : null}
      </Box>
    )
  })

  // ── the "done in" row: keep the engine's words, add what passed the gateway that turn
  on('ui.render', { component: 'TurnDuration' }, async ($, e, next) => {
    if (e.surface !== 'terminal') return next(e)
    const b = await readBoard($)
    const turn = b.turns.find(t => Math.abs(t.durationMs - e.props.durationMs) < 1_000)
    if (!turn || (turn.saved === 0 && turn.calls === 0 && (turn.sources ?? 0) === 0)) return next(e)
    const { Box, Text } = $.ui.resolve(e)
    return (
      <Box>
        <Text color={hex(SECONDARY)}>
          ✻ {e.props.word} for {clock(e.props.durationMs)}
        </Text>
        <Text color={hex(HAIRLINE)}>  │  </Text>
        <Text bold color={hex(SIGNATURE)}>
          ◆ LeanCTX
        </Text>
        <Text color={hex(SECONDARY)}>
          {(turn.sources ?? 0) > 0 ? ` Claude saw ${plural(turn.sources, 'source', 'sources')}` : ''}
          {turn.saved > 0 ? (
            <Text>
              {(turn.sources ?? 0) > 0 ? ' ·' : ''} <Text bold color={hex(FOREGROUND)}>{compact(turn.saved)}</Text> tokens kept out
            </Text>
          ) : null}
        </Text>
      </Box>
    )
  })

  // ── the docked sidebar: what Claude saw, tokens, the two checks, receipts
  on('ui.render', { component: 'Pane', requestId: PANE }, async ($, e) => {
    const b = await readBoard($)
    const f = await read($, frame)
    const tick = f?.tick ?? 0
    const s = b.snap
    const now = Date.now()
    const savedPct = s && s.tokensInput > 0 ? (s.tokensSaved / s.tokensInput) * 100 : undefined
    const sources = Object.values(b.sources)
    const governed = sources.filter(x => x.governed).length
    if (shown < 0 && s) {
      shown = 0
      target = s.tokensSaved
    }

    if (e.surface !== 'terminal') {
      const { Box, Text } = $.ui.resolve(e)
      return (
        <Box flexDirection="column">
          <Text bold color={hex(SIGNATURE)}>LeanCTX · Context Gateway</Text>
          <Text>Claude saw {sources.length} sources · {governed} through the gateway</Text>
          <Text>{compact(s?.tokensSaved ?? 0)} tokens kept out ({pct(savedPct)} leaner)</Text>
        </Box>
      )
    }

    const ui = $.ui.resolve(e)
    const { Box, Text, Raster } = ui
    const W = Math.max(40, e.props.bodyColumns)
    const inner = W - 4
    const pulse = (Math.sin(tick * 0.4) + 1) / 2
    const fg = hex(FOREGROUND)
    const sec = hex(SECONDARY)
    if (shown !== target) startFrames($)

    // ── CLAUDE SAW: sources, how deep, through the gateway or direct
    const counts = Object.fromEntries(DEPTHS.map(d => [d, sources.filter(x => x.depth === d).length])) as Record<Depth, number>
    const direct = sources.length - governed
    const legend = (
      <Text wrap="truncate">
        {DEPTHS.filter(d => counts[d] > 0).map(d => (
          <Text>
            <Text color={hex(DEPTH_COLOR[d])}>■ </Text>
            <Text color={sec}>
              {counts[d]} {d}
              {'   '}
            </Text>
          </Text>
        ))}
      </Text>
    )
    const latest = [...sources].sort((a, x) => x.last - a.last).slice(0, 3)
    const saw: Part[] = [
      {
        node: (
          <Text>
            <Text bold color={fg}>
              {sources.length}
            </Text>
            <Text color={sec}> sources   </Text>
            <Text bold color={hex(SIGNATURE)}>
              {governed}
            </Text>
            <Text color={sec}> through the gateway   </Text>
            <Text bold color={direct > 0 ? fg : sec}>
              {direct}
            </Text>
            <Text color={sec}> direct</Text>
          </Text>
        ),
        rows: 1,
      },
      GAP,
      { node: <Raster key="depth-mix" columns={inner} rows={1} cells={mixCells(inner, counts)} />, rows: 1 },
      { node: sources.length > 0 ? legend : <Text color={sec}>Sources appear as Claude reads them.</Text>, rows: 1 },
      GAP,
      ...latest.map(x => ({
        node: (
          <Box>
            <Box width={2}>
              <Text color={hex(DEPTH_COLOR[x.depth])}>■</Text>
            </Box>
            <Box flexGrow={1}>
              <Text color={fg} wrap="truncate">
                {x.label}
              </Text>
            </Box>
            <Text color={sec}>
              {' '}
              {x.depth}
              {x.governed ? '' : ' · direct'}
            </Text>
          </Box>
        ),
        rows: 1,
      })),
    ]

    // ── TOKENS: kept out of Claude's context, and the budget it used
    const figure = compact(Math.max(0, shown < 0 ? target : shown))
    const isCounting = shown !== target
    const figCols = Math.min(inner, Math.max(12, figureWidth(figure) + 1))
    const sideBySide = inner - figCols >= 24
    const sent = Math.max(0, (s?.tokensInput ?? 0) - (s?.tokensSaved ?? 0))
    const caption = (
      <Box flexDirection="column" marginLeft={sideBySide ? 3 : 0}>
        <Text bold color={fg}>
          tokens kept out
        </Text>
        <Text color={sec}>of Claude’s context</Text>
        <Text>
          <Text bold color={hex(SIGNATURE)}>
            {pct(savedPct)}
          </Text>
          <Text color={sec}> leaner</Text>
        </Text>
        <Text color={sec}>
          {compact(s?.tokensInput ?? 0)} raw → {compact(sent)} delivered
        </Text>
      </Box>
    )
    const figureNode = (
      <Raster key="figure" columns={figCols} rows={FIGURE_ROWS} cells={figureCells(figure, figCols, tick / 90, isCounting ? 0.45 : 0)} />
    )
    const points = b.history?.sessionId === s?.sessionId ? (b.history?.points ?? []) : []
    const values = points.map(pt => pt.saved)
    const ctxFrac = (b.contextPct ?? 0) / 100
    const budgetW = Math.max(8, inner - 16 - 6)
    const tokens: Part[] = [
      ...(sideBySide
        ? [{ node: <Box>{figureNode}{caption}</Box>, rows: FIGURE_ROWS }]
        : [{ node: figureNode, rows: FIGURE_ROWS }, GAP, { node: caption, rows: 4 }]),
      GAP,
      {
        node:
          values.length >= 2 ? (
            <Raster key="history" columns={inner} rows={2} cells={areaCells(inner, 2, values, b.isWorking ? pulse : 0)} />
          ) : (
            <Text color={sec}>{'The curve starts with the next lean-ctx call.\n'}</Text>
          ),
        rows: 2,
      },
      GAP,
      {
        node: (
          <Box>
            <Box width={16}>
              <Text color={sec}>Context budget</Text>
            </Box>
            <Raster key="g-context" columns={budgetW} rows={1} cells={meterCells(budgetW, ctxFrac, () => pressure(ctxFrac))} />
            <Box width={6} justifyContent="flex-end">
              <Text bold color={hex(ctxFrac >= 0.7 ? pressure(ctxFrac) : FOREGROUND)}>
                {pct(b.contextPct)}
              </Text>
            </Box>
          </Box>
        ),
        rows: 1,
      },
      {
        node: (
          <Text color={sec} wrap="truncate">
            {b.contextTokens !== undefined && b.window
              ? `${compact(b.contextTokens)} / ${compact(b.window)} in the window · cache ${pct(b.cacheHitPct)}${b.costUsd !== undefined ? ` · $${b.costUsd.toFixed(2)}` : ''}`
              : 'The window fills in after the first reply'}
          </Text>
        ),
        rows: 1,
      },
    ]

    // ── TWO CHECKS: may it be read? may it be delivered?
    const half = Math.floor(inner / 2)
    const calls = s?.toolCalls ?? 0
    // A redacted result is still delivered, with the secret removed; only a
    // blocked read never reaches Claude.
    const blocked = (s?.shellBlocked ?? 0) + (s?.pathBlocked ?? 0)
    const check = (glyph: string, color: number, value: number, label: string) => (
      <Text>
        <Text color={hex(value > 0 ? color : HAIRLINE)}>{glyph} </Text>
        <Text bold color={value > 0 ? fg : sec}>
          {compact(value)}
        </Text>
        <Text color={sec}> {label}</Text>
      </Text>
    )
    const column = (title: string, rows: RenderChildren[]) => (
      <Box width={half} flexDirection="column">
        <Text bold color={fg}>
          {title}
        </Text>
        {rows}
      </Box>
    )
    const checks: Part[] = [
      {
        node: (
          <Box>
            {column('May it be read?', [
              check('✓', OK, Math.max(0, calls - blocked), 'allowed'),
              check('⊘', VIOLET, s?.pathBlocked ?? 0, 'path outside rules'),
              check('⊘', VIOLET, s?.shellBlocked ?? 0, 'command blocked'),
            ])}
            {column('May it be delivered?', [
              check('✓', OK, Math.max(0, calls - blocked), 'delivered'),
              check('⛨', VIOLET, s?.secretsRedacted ?? 0, 'secrets redacted'),
              check('⚑', VIOLET, s?.injectionFlagged ?? 0, 'injections flagged'),
            ])}
          </Box>
        ),
        rows: 4,
      },
    ]

    // ── RECEIPTS: both chains, and the last calls as source · depth · decision
    const p = b.proof
    const chainRow = (name: string, entries: number | undefined, intact: boolean | undefined) => (
      <Box>
        <Text bold color={hex(intact === undefined ? HAIRLINE : intact ? OK : DANGER)}>
          {intact === undefined ? '○' : intact ? '✓' : '✕'}{'  '}
        </Text>
        <Box width={15}>
          <Text color={fg}>{name}</Text>
        </Box>
        <Text color={sec} wrap="truncate">
          {entries !== undefined ? `${grouped(entries)} entries · ` : ''}
          {intact === undefined ? 'not checked yet' : intact ? 'intact' : 'BROKEN'}
        </Text>
      </Box>
    )
    const calls5 = b.feed.slice(-FEED_SHOWN).reverse()
    const receipts: Part[] = [
      { node: chainRow('Savings ledger', p?.ledgerEntries, p?.ledgerIntact), rows: 1 },
      { node: chainRow('Audit trail', p?.auditEntries, p?.auditIntact), rows: 1 },
      {
        node: p?.isRunning ? (
          <Text color={hex(mix(SIGNATURE, ICE, pulse))}>{SPINNER[tick % SPINNER.length]} verifying both chains…</Text>
        ) : (
          <Text color={sec} wrap="truncate">
            {p?.error ? `verification failed: ${p.error}` : p?.checkedAt ? `verified ${ago(now - p.checkedAt)} · lean-ctx value` : 'first check runs shortly'}
          </Text>
        ),
        rows: 1,
      },
      GAP,
      ...(calls5.length
        ? calls5.map((item, i) => {
            const isRunning = item.ms === undefined
            const isFresh = i === 0 && now - item.startedAt < 2_500
            const decision = isRunning ? 'at the gate' : item.isError ? 'failed' : item.governed ? 'delivered' : 'direct'
            const mark = isRunning ? SPINNER[tick % SPINNER.length] : item.isError ? '✕' : item.governed ? '✓' : '·'
            const markColor = isRunning ? mix(SIGNATURE, ICE, pulse) : item.isError ? DANGER : item.governed ? OK : SECONDARY
            return {
              node: (
                <Box>
                  <Box width={2}>
                    <Text bold color={hex(markColor)}>
                      {mark}
                    </Text>
                  </Box>
                  <Box flexGrow={1}>
                    <Text color={hex(isFresh ? ICE : FOREGROUND)} wrap="truncate">
                      {item.label || shortTool(item.tool)}
                    </Text>
                  </Box>
                  <Box width={11}>
                    <Text color={hex(item.depth ? DEPTH_COLOR[item.depth] : SECONDARY)} wrap="truncate">
                      {' '}
                      {item.depth ?? shortTool(item.tool)}
                    </Text>
                  </Box>
                  <Box width={12} justifyContent="flex-end">
                    <Text color={sec}>
                      {decision}
                      {isRunning ? '' : ` ${dur(item.ms ?? 0)}`}
                    </Text>
                  </Box>
                </Box>
              ),
              rows: 1,
            }
          })
        : [{ node: <Text color={sec}>No tool calls yet</Text>, rows: 1 }]),
    ]

    return (
      <Box flexDirection="column">
        <Box justifyContent="space-between">
          <Text wrap="truncate">
            {pill(ui, '◆ LeanCTX', CANVAS, SIGNATURE, true)}
            <Text color={sec}>  Context Gateway</Text>
          </Text>
          {b.isWorking
            ? pill(ui, `${SPINNER[tick % SPINNER.length]} LIVE`, FOREGROUND, mix(BLUE_FIELD, SIGNATURE, pulse * 0.5), true)
            : pill(ui, '○ IDLE', SECONDARY, PANEL)}
        </Box>
        <Text color={hex(HAIRLINE)}>Control what your AI can see.</Text>
        <Box marginTop={1}>{handoff(ui, 'pane-handoff', W, b, tick, now)}</Box>

        {panel(ui, W, 'CLAUDE SAW', 'this conversation', saw)}
        {panel(ui, W, 'TOKENS', s?.startedAt ? `session ${span(now - Date.parse(s.startedAt))}` : 'this session', tokens)}
        {panel(ui, W, 'TWO CHECKS', 'before the handoff', checks)}
        {panel(ui, W, 'RECEIPTS', p?.ledgerIntact === false || p?.auditIntact === false ? 'chain broken' : 'evidence you can check', receipts)}

        <Box marginTop={1}>
          <Text color={hex(HAIRLINE)} wrap="truncate">
            {s ? `Session ${s.sessionId.slice(-14)} · same numbers as the status line` : 'Looking for this conversation’s lean-ctx session…'}
          </Text>
        </Box>
      </Box>
    )
  })
}
