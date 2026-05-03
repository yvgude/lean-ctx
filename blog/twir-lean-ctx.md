# Building a Context Runtime for AI Coding Agents in Rust

lean-ctx is an open-source context runtime that sits between AI coding tools (Cursor, Claude Code, Copilot, etc.) and the filesystem. It compresses file reads with AST-aware intelligence, strips noise from shell output via 90+ patterns, and manages cross-session memory. A single Rust binary, 49 MCP tools, zero runtime dependencies.

This post walks through the Rust-specific architecture decisions, patterns, and trade-offs that shaped lean-ctx — from tree-sitter integration to implementing Thompson Sampling bandits without pulling in a statistics crate.

GitHub: [github.com/yvgude/lean-ctx](https://github.com/yvgude/lean-ctx) | Website: [leanctx.com](https://leanctx.com)

## The problem lean-ctx solves

AI coding agents read the same files repeatedly. A typical session reads `main.rs` ten to fifteen times at roughly 2,000 tokens each. Shell commands like `cargo build` or `git log` produce verbose output that burns through context windows. lean-ctx intercepts these reads and shell outputs, caching, compressing, and deduplicating before the LLM ever sees them.

The performance constraint is strict: lean-ctx sits in the hot path of every tool call. Responses must complete in under 50ms. That ruled out any approach involving local LLM inference for summarization and pushed us toward deterministic, algorithmic compression — which turned out to be both faster and more reliable.

## Tree-sitter: two integration styles in one codebase

lean-ctx supports AST-aware parsing for 18 languages. We use tree-sitter in two distinct patterns, both gated behind `#[cfg(feature = "tree-sitter")]` to keep builds without grammar dependencies fast and small.

**Pattern 1: Manual node walking.** The `deep_queries` module traverses the AST directly, dispatching per language extension:

```rust
pub fn analyze(content: &str, ext: &str) -> DeepAnalysis {
    #[cfg(feature = "tree-sitter")]
    {
        if let Some(result) = analyze_with_tree_sitter(content, ext) {
            return result;
        }
    }
    DeepAnalysis::empty()
}

#[cfg(feature = "tree-sitter")]
fn analyze_with_tree_sitter(content: &str, ext: &str) -> Option<DeepAnalysis> {
    let language = get_language(ext)?;
    let mut parser = Parser::new();
    parser.set_language(&language).ok()?;
    let tree = parser.parse(content.as_bytes(), None)?;
    let root = tree.root_node();

    Some(DeepAnalysis {
        imports: imports::extract_imports(root, content, ext),
        calls: calls::extract_calls(root, content, ext),
        types: type_defs::extract_types(root, content, ext),
        exports: type_defs::extract_exports(root, content, ext),
    })
}
```

The `?` operator chains neatly through tree-sitter's fallible steps — language lookup, parser configuration, parsing — collapsing any failure to `None` and falling back to `empty()`.

**Pattern 2: Declarative queries with a thread-local parser.** The `signatures_ts` module uses tree-sitter's query language to extract function and type signatures. Since `Parser` is not `Send`, we store it in a `thread_local!`:

```rust
thread_local! {
    static PARSER: std::cell::RefCell<Parser> = std::cell::RefCell::new(Parser::new());
}

let tree = PARSER.with(|p| {
    let mut parser = p.borrow_mut();
    let _ = parser.set_language(&language);
    parser.parse(content, None)
})?;

let query = Query::new(&language, query_src).ok()?;
let def_idx = find_capture_index(&query, "def")?;
let name_idx = find_capture_index(&query, "name")?;

let mut cursor = QueryCursor::new();
let mut matches = cursor.matches(&query, tree.root_node(), content.as_bytes());
while let Some(m) = matches.next() {
    // extract captures by index, not magic numbers
}
```

One lesson learned: tree-sitter grammar crates don't always keep pace with tree-sitter core. We had to use `tree-sitter-kotlin-ng` instead of `tree-sitter-kotlin` because the original crate caps tree-sitter at <0.23, while we needed 0.26. This kind of version alignment challenge is common when pulling in many optional grammar dependencies.

## Adaptive compression with Thompson Sampling

Different files benefit from different compression levels. A 50-line utility file shouldn't be compressed the same way as a 2,000-line service module. Rather than hard-coding thresholds, lean-ctx learns them during each session using a multi-armed bandit.

Each "arm" represents a compression threshold. The bandit selects which threshold to apply, then observes whether the AI agent needed to re-read the file (a signal that compression was too aggressive). Over a session, the bandit converges on the threshold that balances token savings against information loss.

The core selection uses epsilon-greedy exploration with a decaying epsilon, combined with Beta-distributed Thompson Sampling:

```rust
pub fn select_arm(&mut self) -> &BanditArm {
    self.total_pulls += 1;

    let epsilon = (0.1 / (1.0 + self.total_pulls as f64 / 100.0)).max(0.02);
    if rng_f64() < epsilon {
        let idx = rng_usize(self.arms.len());
        return &self.arms[idx];
    }

    let samples: Vec<f64> = self.arms.iter().map(BanditArm::sample).collect();
    let best_idx = samples
        .iter()
        .enumerate()
        .max_by(|(_, a), (_, b)| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal))
        .map_or(0, |(i, _)| i);

    &self.arms[best_idx]
}
```

A design decision worth noting: we implemented Beta sampling from scratch using Marsaglia and Tsang's method for Gamma variates, rather than pulling in a statistics crate. The entire implementation is about 40 lines:

```rust
fn beta_sample(alpha: f64, beta: f64) -> f64 {
    let x = gamma_sample(alpha);
    let y = gamma_sample(beta);
    if x + y == 0.0 { return 0.5; }
    x / (x + y)
}

fn gamma_sample(shape: f64) -> f64 {
    if shape < 1.0 {
        let u = rng_f64().max(1e-10);
        return gamma_sample(shape + 1.0) * u.powf(1.0 / shape);
    }
    let d = shape - 1.0 / 3.0;
    let c = 1.0 / (9.0_f64 * d).sqrt();
    loop {
        let x = standard_normal();
        let v = (1.0 + c * x).powi(3);
        if v <= 0.0 { continue; }
        let u = rng_f64().max(1e-10);
        if u < 1.0 - 0.0331 * x.powi(4)
            || u.ln() < 0.5 * x * x + d * (1.0 - v + v.ln())
        {
            return d * v;
        }
    }
}
```

The bandit state (`alpha`, `beta` per arm) is serializable via serde — it persists across tool calls within a session but resets between sessions, since file characteristics change as the developer edits code.

## Shell pattern compression: OnceLock + modular dispatch

lean-ctx compresses shell output from 90+ command patterns (git, cargo, npm, docker, kubectl, etc.). The architecture uses a macro for zero-cost lazy regex initialization:

```rust
macro_rules! static_regex {
    ($pattern:expr) => {{
        static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
        RE.get_or_init(|| {
            regex::Regex::new($pattern)
                .expect(concat!("BUG: invalid static regex: ", $pattern))
        })
    }};
}

fn compiling_re() -> &'static regex::Regex {
    static_regex!(r"Compiling (\S+) v(\S+)")
}
fn error_re() -> &'static regex::Regex {
    static_regex!(r"error\[E(\d+)\]: (.+)")
}
```

Each tool family (cargo, git, npm, docker, etc.) gets its own module with a `compress` function. The dispatcher in `patterns/mod.rs` tries user-defined filters first, then routes to the specific pattern module based on command prefix. This makes adding a new pattern family a matter of adding a single file — no changes to the core dispatch logic.

For `cargo build`, the compressor extracts only errors, warnings, and the final status, discarding the hundreds of "Compiling foo v1.2.3" lines that an LLM doesn't need. A successful build that produced 200 lines of output gets compressed to something like `OK 42 crates in 12.3s` — from roughly 800 tokens to 15.

## Cache invalidation: MD5 for content, mtime for staleness

The session cache uses two signals to decide whether cached content is still valid:

```rust
pub fn is_cache_entry_stale(path: &str, cached_mtime: Option<SystemTime>) -> bool {
    let current = file_mtime(path);
    match (cached_mtime, current) {
        (_, None) => false,      // can't stat file — assume not stale
        (None, Some(_)) => true, // no cached mtime — must re-read
        (Some(cached), Some(current)) => current > cached,
    }
}
```

When a file is first read, lean-ctx stores both an MD5 hash of the content and the filesystem mtime. On subsequent reads:

- If `mtime` hasn't changed, the cache is valid — return a compressed stub (~13 tokens instead of ~2,000).
- If `mtime` is newer, invalidate and re-read from disk. Even if the hash happens to match (the user saved without changes), we re-read to be safe.
- For `mode=full` reads, content hash comparison catches the case where mtime changed but content didn't.

This dual-signal approach was motivated by real user feedback: without mtime validation, LLMs would sometimes receive stale cached content after a file edit and get confused, spending more tokens trying to work around the stale data than the cache saved in the first place.

## Platform-specific output decoding

Shell output on Windows doesn't always arrive as UTF-8. lean-ctx handles this by falling back to the Windows Active Code Page via FFI — without pulling in a dedicated encoding crate:

```rust
pub fn decode_output(bytes: &[u8]) -> String {
    match String::from_utf8(bytes.to_vec()) {
        Ok(s) => s,
        Err(_) => {
            #[cfg(windows)]
            { decode_windows_output(bytes) }
            #[cfg(not(windows))]
            { String::from_utf8_lossy(bytes).into_owned() }
        }
    }
}

#[cfg(windows)]
fn decode_windows_output(bytes: &[u8]) -> String {
    extern "system" {
        fn GetACP() -> u32;
        fn MultiByteToWideChar(
            cp: u32, flags: u32,
            src: *const u8, srclen: i32,
            dst: *mut u16, dstlen: i32,
        ) -> i32;
    }
    // ... two-pass: measure, allocate, convert
}
```

On Unix, container detection checks for `/.dockerenv` and `/proc/1/cgroup` to adjust behavior when running inside Docker or LXC — relevant because lean-ctx's shell hook needs to know whether it's intercepting commands in an isolated environment.

## Feature flags: one crate, many binaries

lean-ctx uses Cargo features extensively to keep the binary modular:

```toml
[features]
default = ["tree-sitter", "embeddings", "http-server"]
neural = ["dep:rten", "dep:rten-tensor"]
embeddings = ["dep:rten", "dep:rten-tensor"]
http-server = ["dep:axum", "dep:tower-http", "dep:reqwest"]
cloud-server = ["http-server", "dep:deadpool-postgres", ...]
tree-sitter = ["dep:tree-sitter", "dep:tree-sitter-rust", ...]
```

Building without `tree-sitter` drops 18 grammar crates and produces a significantly smaller binary — useful for constrained environments like Raspberry Pi or CI containers. The `cloud-server` feature pulls in Postgres, JWT, and SMTP dependencies that are only needed for the hosted API at leanctx.com.

## The MCP server: rmcp + async state

lean-ctx implements the Model Context Protocol using the `rmcp` crate. The server is a struct holding shared state behind `Arc<RwLock<...>>`:

```rust
#[derive(Clone)]
pub struct LeanCtxServer {
    pub cache: SharedCache,
    pub session: Arc<RwLock<SessionState>>,
    pub tool_calls: Arc<RwLock<Vec<ToolCallRecord>>>,
    pub call_count: Arc<AtomicUsize>,
    pub loop_detector: Arc<RwLock<LoopDetector>>,
    pub ledger: Arc<RwLock<ContextLedger>>,
    // ...
}
```

Each tool call flows through `ServerHandler::call_tool` — an async method that routes to specific handlers (read, shell, search, etc.), applies post-processing (compression, archiving, throttle warnings), and tracks metrics. A meta-tool `ctx` allows the AI to call any lean-ctx tool by name, so the agent can write `ctx(tool="read", path="src/main.rs")` instead of `ctx_read(path="src/main.rs")` — reducing the number of tool definitions the model needs to track.

## Entropy: measuring information density with BPE tokens

lean-ctx has an `entropy` read mode that filters lines based on their information density. An interesting detail: we compute Shannon entropy twice — once over characters (standard) and once over BPE token IDs from `tiktoken-rs`:

```rust
pub fn shannon_entropy(text: &str) -> f64 {
    let mut freq: HashMap<char, usize> = HashMap::new();
    let total = text.chars().count();
    for c in text.chars() {
        *freq.entry(c).or_default() += 1;
    }
    freq.values().fold(0.0_f64, |acc, &count| {
        let p = count as f64 / total as f64;
        acc - p * p.log2()
    })
}

pub fn token_entropy(text: &str) -> f64 {
    let tokens = encode_tokens(text); // tiktoken o200k_base
    let mut freq: HashMap<u32, usize> = HashMap::new();
    for &t in &tokens {
        *freq.entry(t).or_default() += 1;
    }
    // ... same formula over token IDs
}
```

Character entropy tells us how varied the text is syntactically. Token entropy tells us how varied it is from the LLM's perspective — a line that looks complex to humans might tokenize into common patterns, and vice versa. Using both signals together gives better filtering than either alone.

## Numbers

lean-ctx currently has 900+ GitHub stars and 32,000+ installs across npm, crates.io, and GitHub Releases. It supports 46 AI coding tools through one-command setup (`lean-ctx init --agent cursor`).

The codebase is roughly 35,000 lines of Rust, plus integration tests and benchmarks. The default binary (with tree-sitter and embeddings) compiles to about 15MB on x86_64.

---

*This post was written with AI assistance and reviewed by the author. lean-ctx is Apache-2.0 licensed.*

*Yves Gugger — [github.com/yvgude](https://github.com/yvgude)*
