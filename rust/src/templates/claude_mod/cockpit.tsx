// SPDX-License-Identifier: Apache-2.0
import { atom, read, update } from 'claude-code'
import type { Elements, EngineInterface, On, RenderChildren } from 'claude-code'

import type { Board, FeedItem, Frame, Overlay, Proof, TurnStat, ValueSnap } from '../types'

// The cockpit: a docked sidebar in LeanCTX's Select · Control · Prove terms, a
// pulse line above the prompt while Claude works, and short overlays when a
// turn ends or a milestone falls. The one-line totals stay in lean-ctx's own
// status line below the prompt; the band above it repeats none of them.

const PANE = 'leanctx-cockpit'
const DOCK_COLUMNS = 60
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
// Same freshness window as lean-ctx's own status line.
const ACTIVE_MS = 12 * 3_600_000
const TURNS_KEPT = 60
const HISTORY_KEPT = 480
const FEED_KEPT = 30
const FEED_SHOWN = 5
const MILESTONES = [10e3, 25e3, 50e3, 100e3, 250e3, 500e3, 1e6, 2.5e6, 5e6, 10e6, 25e6]
const LEAN_CTX_TOOL = /^mcp__lean[-_]ctx__/

// Keys are versioned by name: $.state outlives reloads, older shapes stay behind.
const board = atom({ plugin: 'lean-ctx', key: 'state' } as const, {
  milestone: 0,
  turns: [],
  isWorking: false,
  leanRunning: 0,
  feed: [],
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

// The pulse: a hairline that breathes in brand colours while Claude works, with
// a light travelling along it — brighter and quicker while a lean-ctx tool runs.
function pulseCells(cols: number, tick: number, isWorking: boolean, isLeanRunning: boolean): string {
  const g = new Grid(cols, 1)
  if (!isWorking) {
    for (let x = 0; x < cols; x++) g.set(x, 0, '─', HAIRLINE)
    return g.encode()
  }
  const speed = isLeanRunning ? 0.022 : 0.012
  const head = ((tick * speed) % 1.25) * cols - cols * 0.1
  const breath = (Math.sin(tick * 0.09) + 1) / 2
  const tail = isLeanRunning ? 22 : 14
  for (let x = 0; x < cols; x++) {
    const base = mix(HAIRLINE, cycle(BRAND_SWEEP, x / cols / 1.6 - tick * 0.004), 0.35 + breath * 0.35)
    const d = head - x
    const trail = d >= 0 && d < tail ? 1 - d / tail : 0
    const glow = Math.max(trail, Math.max(0, 1 - Math.abs(d) / 2))
    g.set(x, 0, glow > 0.6 ? '━' : '─', mix(base, isLeanRunning ? FOREGROUND : ICE, glow * (isLeanRunning ? 1 : 0.8)))
  }
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

// The hero figure: a slow brand sweep runs across it; brighter while it counts.
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

function describe(args: Record<string, unknown>): string {
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

const SPINNER = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏']

// ── lean-ctx value snapshots ───────────────────────────────────────────────
type Rec = Record<string, unknown>
const isRec = (v: unknown): v is Rec => typeof v === 'object' && v !== null && !Array.isArray(v)
const num = (r: Rec, k: string) => (typeof r[k] === 'number' ? (r[k] as number) : 0)
const str = (r: Rec, k: string) => (typeof r[k] === 'string' ? (r[k] as string) : undefined)

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
let proving = false
// The hero figure counts toward the session total.
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
    const reached = MILESTONES.filter(m => m <= snap.tokensSaved).pop() ?? 0
    // The first look only records where the session stands; later crossings celebrate.
    const isCrossing = reached > b.milestone && (b.milestone > 0 || b.turns.length > 0)
    const at = Date.now()
    const celebration: Overlay = { kind: 'milestone', amount: reached, at }
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
        overlay: isCrossing ? celebration : x.overlay,
      }
    })
    if (shown < 0) shown = 0
    target = snap.tokensSaved
    if (isCrossing) {
      overlayUntil = at + MILESTONE_OVERLAY_MS
      $.ui.toast(`★ LeanCTX · ${compact(reached)} tokens kept out of context`, { timeoutMs: 6000 })
    }
    if (isCrossing || shown !== target) startFrames($)
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

// The turn just ended: sum up what lean-ctx did. Runs from a `$.clock.after`
// timer (scheduled by the `turn.complete` hook) once lean-ctx's last write is
// in: work that outlives a hook's dispatch must not hang off the hook itself.
// Without a snapshot the card still shows, with the turn alone.
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

// One frame clock for every effect; it runs only while something moves.
function startFrames($: EngineInterface) {
  if (frames) return
  frames = $.clock.every(FRAME_MS, () => {
    if (shown >= 0 && shown !== target) {
      const gap = target - shown
      const step = Math.sign(gap) * Math.max(1, Math.abs(gap) * COUNT_EASE)
      shown = Math.abs(step) >= Math.abs(gap) ? target : Math.round(shown + step)
    }
    if (!isWorking && !proving && shown === target && Date.now() > overlayUntil) {
      frames?.cancel()
      frames = undefined
    }
    void update($, frame, f => ({ tick: (f?.tick ?? 0) + 1 })).catch(() => {})
  })
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

// A panel, btop-style: the title sits in the top border (╭─┐SELECT┌──── meta ─╮),
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

// Tool calls as the conversation records them: an assistant row's `tool_use`
// blocks start calls, a `tool_result` ends one (with its error flag). Reading
// rows keeps the cockpit off the tool chain the rest of the mod shapes.
async function observeRow($: EngineInterface, row: Rec) {
  const message = isRec(row['message']) ? row['message'] : {}
  const blocks = (Array.isArray(message['content']) ? message['content'] : []).filter(isRec)
  const now = Date.now()
  const started: FeedItem[] = []
  const ended = new Map<string, boolean>()
  for (const block of blocks) {
    if (block['type'] === 'tool_use' && typeof block['id'] === 'string' && typeof block['name'] === 'string') {
      started.push({
        id: block['id'],
        tool: block['name'],
        label: describe(isRec(block['input']) ? block['input'] : {}),
        isSubagent: typeof row['agentId'] === 'string',
        startedAt: now,
      })
    } else if (block['type'] === 'tool_result' && typeof block['tool_use_id'] === 'string') {
      ended.set(block['tool_use_id'], block['is_error'] === true)
    }
  }
  if (started.length === 0 && ended.size === 0) return
  await update($, board, x => {
    const feed = [...(Array.isArray(x.feed) ? x.feed : []), ...started].slice(-FEED_KEPT)
    let running = (x.leanRunning ?? 0) + started.filter(f => LEAN_CTX_TOOL.test(f.tool)).length
    const done = feed.map(f => {
      const isError = ended.get(f.id)
      if (isError === undefined || f.ms !== undefined) return f
      if (LEAN_CTX_TOOL.test(f.tool)) running -= 1
      return { ...f, ms: now - f.startedAt, isError }
    })
    return { ...x, feed: done, leanRunning: Math.max(0, running) }
  })
  if (ended.size > 0) {
    scheduleRefresh($)
  }
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

// ── the mod ────────────────────────────────────────────────────────────────
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
      await update($, board, x => ({ ...x, isWorking: true, leanRunning: 0, turnStart: countersOf(b.snap) }))
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

  // ── above the prompt: the pulse while working; a card when a turn ends or a milestone falls
  on('ui.render', { component: 'AbovePrompt' }, async ($, e, next) => {
    if (e.props.hasSurvey || e.surface !== 'terminal') return next(e)
    const b = await readBoard($)
    const f = await read($, frame)
    const tick = f?.tick ?? 0
    const now = Date.now()
    const o = b.overlay
    const ui = $.ui.resolve(e)
    const { Box, Text, Raster } = ui
    const cols = Math.max(20, e.props.bodyColumns)

    if (o?.kind === 'milestone' && now - o.at < MILESTONE_OVERLAY_MS) {
      const title = `★  ${compact(o.amount)} TOKENS KEPT OUT OF CONTEXT  ★`
      const started = b.snap?.startedAt ? Date.parse(b.snap.startedAt) : NaN
      return (
        <Box flexDirection="column">
          <Raster key="spark-a" columns={cols} rows={1} cells={sparkleCells(cols, tick, 1)} />
          <Box justifyContent="center">{shimmer(ui, title, tick, SIGNATURE, FOREGROUND, true)}</Box>
          <Box justifyContent="center">
            <Text color={hex(SECONDARY)}>
              LeanCTX milestone{Number.isNaN(started) ? '' : ` · ${span(now - started)} into this session`}
            </Text>
          </Box>
          <Raster key="spark-b" columns={cols} rows={1} cells={sparkleCells(cols, tick + 7, 2)} />
        </Box>
      )
    }

    if (o?.kind === 'turn' && now - o.at < TURN_OVERLAY_MS && !b.isWorking) {
      const t = o.turn
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
            {t.saved > 0 ? pill(ui, `▲ ${compact(t.saved)} kept out`, CANVAS, SIGNATURE, true) : null}
            {t.saved > 0 ? <Text> </Text> : null}
            {pill(ui, plural(t.calls, 'lean-ctx call', 'lean-ctx calls'), FOREGROUND, PANEL)}
            {t.files > 0 ? <Text> </Text> : null}
            {t.files > 0 ? pill(ui, plural(t.files, 'file', 'files'), FOREGROUND, PANEL) : null}
            {t.commands > 0 ? <Text> </Text> : null}
            {t.commands > 0 ? pill(ui, plural(t.commands, 'command', 'commands'), FOREGROUND, PANEL) : null}
          </Box>
        </Box>
      )
    }

    if (b.isWorking) {
      return <Raster key="pulse" columns={cols} rows={1} cells={pulseCells(cols, tick, true, b.leanRunning > 0)} />
    }
    return next(e)
  })

  // ── the spinner row: brand spinner, shimmering words, the turn's live gain
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
    return (
      <Box>
        <Text bold color={hex(cycle(BRAND_SWEEP, tick / 30))}>
          {SPINNER[tick % SPINNER.length]}{' '}
        </Text>
        {shimmer(ui, words, tick, SECONDARY, ICE)}
        {gained > 0 ? (
          <Text>
            <Text>   </Text>
            {pill(ui, `▲ ${compact(gained)} kept out this turn`, FOREGROUND, BLUE_FIELD)}
          </Text>
        ) : null}
      </Box>
    )
  })

  // ── the "done in" row: keep the engine's words, add what lean-ctx did that turn
  on('ui.render', { component: 'TurnDuration' }, async ($, e, next) => {
    if (e.surface !== 'terminal') return next(e)
    const b = await readBoard($)
    const turn = b.turns.find(t => Math.abs(t.durationMs - e.props.durationMs) < 1_000)
    if (!turn || (turn.saved === 0 && turn.calls === 0)) return next(e)
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
          {turn.saved > 0 ? (
            <Text>
              {' '}kept <Text bold color={hex(FOREGROUND)}>{compact(turn.saved)}</Text> tokens out ·
            </Text>
          ) : null}{' '}
          {plural(turn.calls, 'call', 'calls')}
          {turn.files > 0 ? ` · ${plural(turn.files, 'file', 'files')}` : ''}
        </Text>
      </Box>
    )
  })

  // ── the docked sidebar: Select · Control · Prove, the session over time, recent calls
  on('ui.render', { component: 'Pane', requestId: PANE }, async ($, e) => {
    const b = await readBoard($)
    const f = await read($, frame)
    const tick = f?.tick ?? 0
    const s = b.snap
    const now = Date.now()
    const savedPct = s && s.tokensInput > 0 ? (s.tokensSaved / s.tokensInput) * 100 : undefined
    if (shown < 0 && s) {
      shown = 0
      target = s.tokensSaved
    }

    if (e.surface !== 'terminal') {
      const { Box, Text } = $.ui.resolve(e)
      return (
        <Box flexDirection="column">
          <Text bold color={hex(SIGNATURE)}>LeanCTX</Text>
          <Text>Select · {compact(s?.tokensSaved ?? 0)} tokens kept out ({pct(savedPct)} leaner)</Text>
          <Text>Context {pct(b.contextPct)} · Cache {pct(b.cacheHitPct)}</Text>
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

    // ── SELECT: what reached Claude
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
          {compact(s?.tokensInput ?? 0)} raw → {compact(sent)} sent
        </Text>
      </Box>
    )
    const figureNode = (
      <Raster key="figure" columns={figCols} rows={FIGURE_ROWS} cells={figureCells(figure, figCols, tick / 90, isCounting ? 0.45 : 0)} />
    )
    const statsRow = (
      <Text color={sec} wrap="truncate">
        <Text bold color={fg}>{compact(s?.filesRead ?? 0)}</Text> files · <Text bold color={fg}>{compact(s?.commandsRun ?? 0)}</Text>{' '}
        commands · <Text bold color={fg}>{compact(s?.cacheHits ?? 0)}</Text> cached re-reads
      </Text>
    )
    const select: Part[] = [
      ...(sideBySide
        ? [{ node: <Box>{figureNode}{caption}</Box>, rows: FIGURE_ROWS }]
        : [
            { node: figureNode, rows: FIGURE_ROWS },
            GAP,
            { node: caption, rows: 4 },
          ]),
      GAP,
      { node: <Raster key="share" columns={inner} rows={1} cells={meterCells(inner, (savedPct ?? 0) / 100, t => ramp(DATA, t))} />, rows: 1 },
      GAP,
      { node: statsRow, rows: 1 },
    ]

    // ── CONTROL: rules before the handoff, and the window's budget
    const half = Math.floor(inner / 2)
    const guard = (glyph: string, value: number, label: string) => (
      <Box width={half}>
        <Text color={hex(value > 0 ? VIOLET : HAIRLINE)}>{glyph}  </Text>
        <Text bold color={value > 0 ? fg : sec}>
          {compact(value)}
        </Text>
        <Text color={sec}> {label}</Text>
      </Box>
    )
    const gaugeW = Math.max(8, inner - 14 - 6)
    const ctxFrac = (b.contextPct ?? 0) / 100
    const gauge = (key: string, label: string, value: number | undefined, color: (t: number) => number, valueColor: number) => (
      <Box>
        <Box width={14}>
          <Text color={sec}>{label}</Text>
        </Box>
        <Raster key={key} columns={gaugeW} rows={1} cells={meterCells(gaugeW, (value ?? 0) / 100, color)} />
        <Box width={6} justifyContent="flex-end">
          <Text bold color={hex(valueColor)}>
            {pct(value)}
          </Text>
        </Box>
      </Box>
    )
    const control: Part[] = [
      {
        node: (
          <Box>
            {guard('⛨', s?.secretsRedacted ?? 0, 'secrets redacted')}
            {guard('⊘', s?.shellBlocked ?? 0, 'commands blocked')}
          </Box>
        ),
        rows: 1,
      },
      {
        node: (
          <Box>
            {guard('⌂', s?.pathBlocked ?? 0, 'paths blocked')}
            {guard('⚑', s?.injectionFlagged ?? 0, 'injections flagged')}
          </Box>
        ),
        rows: 1,
      },
      GAP,
      { node: gauge('g-context', 'Context window', b.contextPct, () => pressure(ctxFrac), ctxFrac >= 0.7 ? pressure(ctxFrac) : FOREGROUND), rows: 1 },
      { node: gauge('g-cache', 'Prompt cache', b.cacheHitPct, t => ramp(DATA, t), FOREGROUND), rows: 1 },
      {
        node: (
          <Text color={sec} wrap="truncate">
            {b.contextTokens !== undefined && b.window
              ? `${compact(b.contextTokens)} / ${compact(b.window)} tokens${b.costUsd !== undefined ? ` · $${b.costUsd.toFixed(2)} spend` : ''}`
              : 'The window fills in after the first reply'}
          </Text>
        ),
        rows: 1,
      },
    ]

    // ── PROVE: evidence anyone can re-check
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
    const proveStatus = p?.isRunning ? (
      <Text color={hex(mix(SIGNATURE, ICE, pulse))}>{SPINNER[tick % SPINNER.length]} verifying both chains…</Text>
    ) : p?.error ? (
      <Text color={hex(DANGER)} wrap="truncate">
        verification failed: {p.error}
      </Text>
    ) : (
      <Text color={sec} wrap="truncate">
        {p?.checkedAt ? `verified ${ago(now - p.checkedAt)}` : 'first check runs shortly'}
        {p?.sessionEntries ? ` · ${grouped(p.sessionEntries)} this session` : ''}
      </Text>
    )
    const prove: Part[] = [
      { node: chainRow('Savings ledger', p?.ledgerEntries, p?.ledgerIntact), rows: 1 },
      { node: chainRow('Audit trail', p?.auditEntries, p?.auditIntact), rows: 1 },
      GAP,
      { node: proveStatus, rows: 1 },
    ]

    // ── TIMELINE: the session's running total, sampled on every change
    const points = b.history?.sessionId === s?.sessionId ? (b.history?.points ?? []) : []
    const values = points.map(pt => pt.saved)
    const first = points[0]
    const lastPt = points.at(-1)
    const recent = points.filter(pt => now - pt.at <= 15 * 60_000)
    const gain15 = recent.length > 0 && lastPt ? lastPt.saved - (recent[0]?.saved ?? lastPt.saved) : 0
    const timeline: Part[] = [
      {
        node:
          values.length >= 2 ? (
            <Raster key="history" columns={inner} rows={4} cells={areaCells(inner, 4, values, b.isWorking ? pulse : 0)} />
          ) : (
            <Text color={sec}>{'Collecting — the curve starts with the\nnext lean-ctx call.\n\n'}</Text>
          ),
        rows: 4,
      },
      GAP,
      {
        node: (
          <Box justifyContent="space-between">
            <Text color={sec}>{first ? `since ${span(now - first.at)} ago` : 'this session'}</Text>
            <Text>
              <Text bold color={hex(gain15 > 0 ? SIGNATURE : SECONDARY)}>
                {gain15 > 0 ? `▲ ${compact(gain15)}` : '—'}
              </Text>
              <Text color={sec}> last 15 min</Text>
            </Text>
          </Box>
        ),
        rows: 1,
      },
    ]

    // ── RECENT
    const calls = b.feed.slice(-FEED_SHOWN).reverse()
    const recentParts: Part[] = calls.length
      ? calls.map((item, i) => {
          const isLean = LEAN_CTX_TOOL.test(item.tool)
          const isRunning = item.ms === undefined
          const isFresh = i === 0 && now - item.startedAt < 2_500
          const mark = isRunning ? SPINNER[tick % SPINNER.length] : item.isError ? '✕' : '✓'
          const markColor = isRunning ? mix(SIGNATURE, ICE, pulse) : item.isError ? DANGER : isLean ? SIGNATURE : SECONDARY
          return {
            node: (
              <Box>
                <Box width={3}>
                  <Text bold color={hex(markColor)}>
                    {mark}
                  </Text>
                </Box>
                <Box width={13}>
                  <Text color={hex(isLean ? SIGNATURE : FOREGROUND)} bold={isFresh} wrap="truncate">
                    {item.isSubagent ? '↳' : ''}
                    {shortTool(item.tool)}
                  </Text>
                </Box>
                <Box flexGrow={1}>
                  <Text color={hex(isFresh ? ICE : SECONDARY)} wrap="truncate">
                    {item.label}
                  </Text>
                </Box>
                <Box width={7} justifyContent="flex-end">
                  <Text color={sec}>{isRunning ? '' : dur(item.ms ?? 0)}</Text>
                </Box>
              </Box>
            ),
            rows: 1,
          }
        })
      : [{ node: <Text color={sec}>No tool calls yet</Text>, rows: 1 }]

    return (
      <Box flexDirection="column">
        <Box justifyContent="space-between">
          <Text wrap="truncate">
            {pill(ui, '◆ LeanCTX', CANVAS, SIGNATURE, true)}
            <Text color={sec}>  Control what your AI can see.</Text>
          </Text>
          {b.isWorking
            ? pill(ui, `${SPINNER[tick % SPINNER.length]} LIVE`, FOREGROUND, mix(BLUE_FIELD, SIGNATURE, pulse * 0.5), true)
            : pill(ui, '○ IDLE', SECONDARY, PANEL)}
        </Box>
        <Box marginTop={1}>
          <Raster key="pulse" columns={W} rows={1} cells={pulseCells(W, tick, b.isWorking, b.leanRunning > 0)} />
        </Box>

        {panel(ui, W, 'SELECT', 'what reached Claude', select)}
        {panel(ui, W, 'CONTROL', 'rules before handoff', control)}
        {panel(ui, W, 'PROVE', 'evidence you can check', prove)}
        {panel(ui, W, 'TIMELINE', 'kept out over time', timeline)}
        {panel(ui, W, 'RECENT', `${b.feed.length} calls`, recentParts)}

        <Box marginTop={1}>
          <Text color={hex(HAIRLINE)} wrap="truncate">
            {s ? `Session ${s.sessionId.slice(-14)} · same numbers as the status line` : 'Looking for this conversation’s lean-ctx session…'}
          </Text>
        </Box>
      </Box>
    )
  })
}
