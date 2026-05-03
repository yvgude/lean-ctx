use lean_ctx::core::events::{emit, EventKind};
use lean_ctx::core::gotcha_tracker::{
    Gotcha, GotchaCategory, GotchaSeverity, GotchaSource, GotchaStats, GotchaStore,
};
use lean_ctx::core::knowledge::ProjectKnowledge;
use lean_ctx::core::memory_policy::MemoryPolicy;

fn main() {
    let project_root =
        std::env::current_dir().map_or_else(|_| ".".to_string(), |p| p.display().to_string());

    println!("Seeding Observatory data for: {project_root}");

    seed_events();
    seed_knowledge(&project_root);
    seed_gotchas(&project_root);
    seed_feedback();

    println!("Done! Restart dashboard to see data.");
}

fn seed_events() {
    let tools = vec![
        ("ctx_read", "full", "src/main.rs", 7694, 5840, 12),
        ("ctx_read", "map", "src/core/mod.rs", 2100, 1890, 8),
        ("ctx_read", "signatures", "src/core/cache.rs", 4200, 3950, 6),
        ("ctx_read", "entropy", "src/core/entropy.rs", 6800, 5440, 15),
        ("ctx_read", "full", "src/tools/mod.rs", 5900, 4130, 10),
        (
            "ctx_read",
            "aggressive",
            "src/core/knowledge.rs",
            8200,
            6560,
            18,
        ),
        ("ctx_read", "map", "src/core/agents.rs", 3800, 3420, 5),
        ("ctx_read", "full", "src/dashboard/mod.rs", 6100, 4270, 14),
        ("ctx_shell", "auto", "cargo test", 12000, 4800, 320),
        ("ctx_shell", "auto", "cargo check", 3400, 1360, 180),
        ("ctx_shell", "auto", "git status", 800, 320, 45),
        ("ctx_shell", "auto", "git diff --stat", 2200, 880, 60),
        ("ctx_shell", "auto", "npm run build", 5600, 2240, 250),
        ("ctx_search", "bm25", "EventKind", 1500, 1200, 25),
        ("ctx_search", "bm25", "fn emit", 900, 720, 18),
        ("ctx_search", "bm25", "pub struct", 2400, 1920, 30),
        ("ctx_search", "bm25", "dashboard", 1800, 1440, 22),
        ("ctx_read", "full", "src/core/feedback.rs", 3100, 2170, 9),
        ("ctx_read", "map", "src/core/session.rs", 4500, 4050, 7),
        ("ctx_read", "signatures", "src/lib.rs", 800, 720, 3),
        ("ctx_shell", "auto", "cargo fmt", 400, 160, 35),
        (
            "ctx_read",
            "entropy",
            "src/core/graph_index.rs",
            5200,
            3640,
            20,
        ),
        (
            "ctx_read",
            "full",
            "src/core/gotcha_tracker.rs",
            3600,
            2520,
            11,
        ),
        ("ctx_shell", "auto", "rg 'TODO' src/", 600, 240, 15),
        ("ctx_read", "map", "src/core/buddy.rs", 2800, 2520, 6),
        ("ctx_read", "full", "src/core/compressor.rs", 4800, 3360, 13),
        ("ctx_search", "bm25", "compression", 2100, 1680, 28),
        ("ctx_shell", "auto", "cargo build --release", 800, 320, 1500),
        ("ctx_read", "aggressive", "src/core/stats.rs", 2900, 2030, 8),
        ("ctx_read", "full", "src/core/tokens.rs", 1200, 840, 4),
        ("ctx_shell", "auto", "git log --oneline -10", 500, 200, 20),
        ("ctx_read", "map", "Cargo.toml", 1800, 1620, 5),
        ("ctx_read", "full", "src/core/filters.rs", 3400, 2380, 10),
        ("ctx_shell", "auto", "ls -la src/core/", 900, 360, 8),
    ];

    for (tool, mode, path, orig, saved, dur) in &tools {
        emit(EventKind::ToolCall {
            tool: tool.to_string(),
            tokens_original: *orig,
            tokens_saved: *saved,
            mode: Some(mode.to_string()),
            duration_ms: *dur,
            path: Some(path.to_string()),
        });
    }
    println!("  {} ToolCall events", tools.len());

    let cache_hits = vec![
        ("src/main.rs", 7694u64),
        ("src/core/mod.rs", 2100),
        ("src/core/cache.rs", 4200),
        ("src/tools/mod.rs", 5900),
        ("src/main.rs", 7694),
        ("src/core/entropy.rs", 6800),
        ("src/core/knowledge.rs", 8200),
        ("src/main.rs", 7694),
        ("src/core/cache.rs", 4200),
        ("src/dashboard/mod.rs", 6100),
        ("Cargo.toml", 1800),
        ("src/core/agents.rs", 3800),
        ("src/core/feedback.rs", 3100),
        ("src/main.rs", 7694),
        ("src/core/entropy.rs", 6800),
    ];

    for (path, tokens) in &cache_hits {
        emit(EventKind::CacheHit {
            path: path.to_string(),
            saved_tokens: *tokens,
        });
    }
    println!("  {} CacheHit events", cache_hits.len());

    let compressions = vec![
        ("src/main.rs", 952, 185, "map"),
        ("src/core/cache.rs", 480, 95, "signatures"),
        ("src/core/entropy.rs", 620, 410, "entropy_adaptive"),
        ("src/core/knowledge.rs", 748, 380, "aggressive"),
        ("src/tools/mod.rs", 569, 120, "map"),
        ("src/dashboard/mod.rs", 602, 145, "signatures"),
        ("src/core/agents.rs", 540, 280, "entropy_adaptive"),
        ("src/core/graph_index.rs", 420, 190, "aggressive"),
        ("src/core/feedback.rs", 236, 85, "map"),
        ("src/core/session.rs", 380, 160, "entropy_adaptive"),
    ];

    for (path, before, after, strategy) in &compressions {
        emit(EventKind::Compression {
            path: path.to_string(),
            before_lines: *before,
            after_lines: *after,
            strategy: strategy.to_string(),
            kept_line_count: *after,
            removed_line_count: before - after,
        });
    }
    println!("  {} Compression events", compressions.len());

    let agent_actions = vec![
        ("cursor-45821-abc", "register", Some("ctx_read")),
        ("cursor-45821-abc", "handoff", None),
        ("cursor-45821-def", "register", Some("ctx_shell")),
        ("cursor-45821-def", "sync", None),
        ("cursor-45821-abc", "diary", Some("ctx_search")),
        ("cursor-45821-ghi", "register", Some("ctx_read")),
        ("cursor-45821-abc", "complete", None),
    ];

    for (id, action, tool) in &agent_actions {
        emit(EventKind::AgentAction {
            agent_id: id.to_string(),
            action: action.to_string(),
            tool: tool.map(std::string::ToString::to_string),
        });
    }
    println!("  {} AgentAction events", agent_actions.len());

    let knowledge_updates = vec![
        ("ARCHITECTURE", "database", "remember"),
        ("ARCHITECTURE", "auth-method", "remember"),
        ("TESTING", "test-framework", "remember"),
        ("ARCHITECTURE", "cache-strategy", "remember"),
        ("DEBUGGING", "common-error", "remember"),
        ("WORKFLOW", "deploy-process", "remember"),
        ("TESTING", "integration-setup", "remember"),
        ("ARCHITECTURE", "auth-method", "contradict"),
        ("PERFORMANCE", "bottleneck", "remember"),
        ("ARCHITECTURE", "database", "remember"),
        ("SECURITY", "token-storage", "remember"),
        ("E2E", "full-pipeline", "remember"),
    ];

    for (cat, key, action) in &knowledge_updates {
        emit(EventKind::KnowledgeUpdate {
            category: cat.to_string(),
            key: key.to_string(),
            action: action.to_string(),
        });
    }
    println!("  {} KnowledgeUpdate events", knowledge_updates.len());

    let threshold_shifts = vec![
        ("rs", 1.15, 1.08, 0.72, 0.68),
        ("ts", 0.95, 0.88, 0.70, 0.65),
        ("py", 1.05, 0.98, 0.75, 0.71),
        ("go", 1.20, 1.12, 0.68, 0.64),
        ("rs", 1.08, 1.02, 0.68, 0.66),
    ];

    for (lang, oe, ne, oj, nj) in &threshold_shifts {
        emit(EventKind::ThresholdShift {
            language: lang.to_string(),
            old_entropy: *oe,
            new_entropy: *ne,
            old_jaccard: *oj,
            new_jaccard: *nj,
        });
    }
    println!("  {} ThresholdShift events", threshold_shifts.len());
}

fn seed_gotchas(project_root: &str) {
    let mut store = GotchaStore::load(project_root);

    let gotchas_data = vec![
        (GotchaCategory::Build, GotchaSeverity::Critical, "error[E0502]: cannot borrow `self` as mutable because it is also borrowed as immutable", "Split the borrow: extract the immutable read into a separate scope or clone the value before the mutable borrow.", 8, 0.92, 5),
        (GotchaCategory::Build, GotchaSeverity::Warning, "warning: unused variable `result`", "Prefix with underscore: `_result` or remove the binding entirely.", 15, 0.75, 12),
        (GotchaCategory::Runtime, GotchaSeverity::Critical, "thread 'main' panicked at 'index out of bounds: the len is 0 but the index is 0'", "Check `.is_empty()` before indexing. Use `.get(0)` for Option-based access.", 3, 0.85, 2),
        (GotchaCategory::Test, GotchaSeverity::Warning, "assertion `left == right` failed: Windows \\r\\n line endings", "Normalize line endings with `.replace('\\r\\n', '\\n')` or use `contains()` instead of exact match.", 6, 0.88, 4),
        (GotchaCategory::Build, GotchaSeverity::Critical, "error: failed to resolve: use of unresolved module `tui`", "Add `pub mod tui;` to lib.rs and `use lean_ctx::tui;` in main.rs.", 2, 0.95, 1),
        (GotchaCategory::Config, GotchaSeverity::Info, "Node.js version mismatch: requires >=22.12.0", "Use nvm to switch: `nvm use 22` or set PATH to correct Node version.", 4, 0.80, 3),
        (GotchaCategory::Runtime, GotchaSeverity::Warning, "E403 Forbidden: npm publish version already exists", "Bump version in package.json before publishing. Use `npm version patch` for auto-increment.", 2, 0.70, 1),
        (GotchaCategory::Build, GotchaSeverity::Warning, "error[E0599]: no method named `is_multiple_of` found", "Use nightly or replace with `count % interval == 0`.", 3, 0.82, 2),
    ];

    for (cat, sev, trigger, resolution, occurrences, confidence, prevented) in gotchas_data {
        let mut gotcha = Gotcha::new(
            cat,
            sev,
            trigger,
            resolution,
            GotchaSource::AutoDetected {
                command: trigger.to_string(),
                exit_code: 1,
            },
            "seed-session",
        );
        gotcha.occurrences = occurrences;
        gotcha.confidence = confidence;
        gotcha.prevented_count = prevented;
        store.gotchas.push(gotcha);
    }

    store.stats = GotchaStats {
        total_errors_detected: 43,
        total_fixes_correlated: 28,
        total_prevented: 30,
        gotchas_promoted: 3,
        gotchas_decayed: 1,
    };

    let _ = store.save(project_root);
    println!("  {} gotchas seeded", store.gotchas.len());
}

fn seed_feedback() {
    use lean_ctx::core::feedback::{CompressionOutcome, FeedbackStore};

    let mut store = FeedbackStore::load();

    let outcomes = vec![
        ("rs", 0.85, 0.72, 8, 3200, 2400, true),
        ("rs", 0.90, 0.70, 5, 7694, 5840, true),
        ("rs", 1.10, 0.68, 12, 4200, 1680, true),
        ("rs", 0.95, 0.75, 3, 2100, 1680, true),
        ("rs", 0.80, 0.65, 6, 6800, 5440, true),
        ("rs", 1.05, 0.70, 4, 5900, 4720, false),
        ("ts", 0.75, 0.68, 10, 8500, 6800, true),
        ("ts", 0.80, 0.72, 7, 4300, 3440, true),
        ("ts", 0.90, 0.65, 5, 2800, 2240, true),
        ("ts", 0.85, 0.70, 8, 6100, 4880, true),
        ("ts", 1.00, 0.75, 3, 1500, 1200, false),
        ("py", 0.70, 0.65, 6, 5200, 4160, true),
        ("py", 0.75, 0.70, 4, 3800, 3040, true),
        ("py", 0.80, 0.68, 9, 7100, 5680, true),
        ("py", 0.85, 0.72, 5, 2400, 1920, true),
        ("go", 1.10, 0.70, 7, 4600, 3680, true),
        ("go", 1.15, 0.68, 4, 3200, 2560, true),
        ("go", 1.05, 0.65, 6, 5800, 4640, true),
        ("json", 0.50, 0.60, 2, 1200, 600, true),
        ("yaml", 0.60, 0.65, 3, 800, 480, true),
    ];

    for &(lang, entropy, jaccard, turns, orig, saved, completed) in &outcomes {
        store.record_outcome(CompressionOutcome {
            session_id: "seed".to_string(),
            language: lang.to_string(),
            entropy_threshold: entropy,
            jaccard_threshold: jaccard,
            total_turns: turns,
            tokens_saved: saved,
            tokens_original: orig,
            cache_hits: turns / 2,
            total_reads: turns,
            task_completed: completed,
            timestamp: String::new(),
        });
    }

    store.save();
    println!("  {} feedback outcomes seeded", outcomes.len());
}

fn seed_knowledge(project_root: &str) {
    let mut knowledge = ProjectKnowledge::load_or_create(project_root);
    let session = "seed-observatory";

    let facts = vec![
        (
            "ARCHITECTURE",
            "database",
            "MySQL 8.4 with connection pooling",
            0.95,
        ),
        (
            "ARCHITECTURE",
            "auth-method",
            "JWT RS256 with refresh tokens",
            0.98,
        ),
        (
            "ARCHITECTURE",
            "cache-strategy",
            "Boltzmann-inspired eviction scoring",
            0.88,
        ),
        (
            "ARCHITECTURE",
            "compression",
            "10 read modes: full, auto, map, signatures, diff, aggressive, entropy, task, reference, lines",
            0.92,
        ),
        (
            "ARCHITECTURE",
            "event-bus",
            "RingBuffer(1000) + JSONL persistence",
            0.85,
        ),
        (
            "TESTING",
            "test-framework",
            "Rust with cargo test, 550+ tests",
            0.97,
        ),
        (
            "TESTING",
            "integration-setup",
            "tempfile crate for isolated test dirs",
            0.90,
        ),
        (
            "TESTING",
            "ci-pipeline",
            "GitHub Actions: Linux + macOS + Windows",
            0.93,
        ),
        (
            "DEBUGGING",
            "common-error",
            "Windows path separators in tests",
            0.80,
        ),
        (
            "DEBUGGING",
            "pipe-guard",
            "Windows bash syntax incompatible",
            0.85,
        ),
        (
            "WORKFLOW",
            "deploy-process",
            "git tag + GitHub release + Homebrew + AUR auto-update",
            0.96,
        ),
        ("WORKFLOW", "website", "Astro SSG on GitLab Pages", 0.91),
        (
            "PERFORMANCE",
            "bottleneck",
            "BM25 index build on large repos > 5s",
            0.75,
        ),
        (
            "PERFORMANCE",
            "optimization",
            "Tree-sitter parsing cached per session",
            0.88,
        ),
        (
            "SECURITY",
            "token-storage",
            "API keys stored in system keychain, never in config",
            0.99,
        ),
        (
            "SECURITY",
            "data-policy",
            "Zero data sent to cloud, 100% local processing",
            0.99,
        ),
        (
            "E2E",
            "full-pipeline",
            "MCP server -> tool call -> compression -> response",
            0.94,
        ),
        (
            "E2E",
            "multi-agent",
            "Agent registry with scratchpad sharing",
            0.87,
        ),
        (
            "ARCHITECTURE",
            "mcp-protocol",
            "49 MCP tools via rmcp crate",
            0.96,
        ),
        (
            "ARCHITECTURE",
            "dashboard",
            "Observatory with 9 views, D3.js + Chart.js",
            0.90,
        ),
    ];

    let policy = MemoryPolicy::default();
    for (cat, key, val, conf) in &facts {
        knowledge.remember(cat, key, val, session, *conf, &policy);
    }
    let _ = knowledge.save();
    println!("  {} knowledge facts seeded", facts.len());
}
