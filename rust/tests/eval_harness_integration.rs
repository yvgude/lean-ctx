//! Integration test for `eval_harness::run_eval()`.
//!
//! Creates a temporary codebase, builds a BM25 index, runs eval queries,
//! and verifies that recall/MRR metrics meet minimum thresholds.

use lean_ctx::core::bm25_index::BM25Index;
use lean_ctx::core::eval_harness::{EvalQuery, run_eval};
use lean_ctx::core::hybrid_search::HybridConfig;
use std::fs;
use tempfile::TempDir;

fn create_fixture_codebase() -> TempDir {
    let dir = TempDir::new().unwrap();
    let src = dir.path().join("src");
    fs::create_dir_all(&src).unwrap();

    fs::write(
        src.join("auth.rs"),
        r#"
pub struct AuthService {
    secret: String,
    expiry: u64,
}

impl AuthService {
    pub fn new(secret: String) -> Self {
        Self { secret, expiry: 3600 }
    }

    pub fn verify_token(&self, token: &str) -> bool {
        !token.is_empty() && token.len() > 10
    }

    pub fn refresh_token(&self, old_token: &str) -> Option<String> {
        if self.verify_token(old_token) {
            Some(format!("refreshed_{old_token}"))
        } else {
            None
        }
    }
}
"#,
    )
    .unwrap();

    fs::write(
        src.join("database.rs"),
        r"
use std::collections::HashMap;

pub struct Database {
    records: HashMap<String, String>,
}

impl Database {
    pub fn connect(url: &str) -> Self {
        let _ = url;
        Self { records: HashMap::new() }
    }

    pub fn query(&self, sql: &str) -> Vec<String> {
        let _ = sql;
        vec![]
    }

    pub fn insert(&mut self, key: String, value: String) {
        self.records.insert(key, value);
    }

    pub fn migrate(&self) -> Result<(), String> {
        Ok(())
    }
}
",
    )
    .unwrap();

    fs::write(
        src.join("cache.rs"),
        r"
use std::collections::HashMap;
use std::time::Instant;

pub struct CacheLayer {
    store: HashMap<String, (String, Instant)>,
    ttl_secs: u64,
}

impl CacheLayer {
    pub fn new(ttl_secs: u64) -> Self {
        Self { store: HashMap::new(), ttl_secs }
    }

    pub fn get(&self, key: &str) -> Option<&str> {
        self.store.get(key).map(|(v, _)| v.as_str())
    }

    pub fn set(&mut self, key: String, value: String) {
        self.store.insert(key, (value, Instant::now()));
    }

    pub fn evict_expired(&mut self) {
        let ttl = self.ttl_secs;
        self.store.retain(|_, (_, t)| t.elapsed().as_secs() < ttl);
    }
}
",
    )
    .unwrap();

    fs::write(
        src.join("api.rs"),
        r#"
pub struct ApiRouter {
    routes: Vec<Route>,
}

struct Route {
    method: String,
    path: String,
}

impl ApiRouter {
    pub fn new() -> Self {
        Self { routes: vec![] }
    }

    pub fn get(&mut self, path: &str) {
        self.routes.push(Route { method: "GET".into(), path: path.into() });
    }

    pub fn post(&mut self, path: &str) {
        self.routes.push(Route { method: "POST".into(), path: path.into() });
    }

    pub fn handle_request(&self, method: &str, path: &str) -> u16 {
        if self.routes.iter().any(|r| r.method == method && r.path == path) {
            200
        } else {
            404
        }
    }
}
"#,
    )
    .unwrap();

    dir
}

fn fixture_queries() -> Vec<EvalQuery> {
    vec![
        EvalQuery {
            query: "verify_token authentication".into(),
            expected_files: vec!["src/auth.rs".into()],
            category: "function".into(),
        },
        EvalQuery {
            query: "database query sql".into(),
            expected_files: vec!["src/database.rs".into()],
            category: "function".into(),
        },
        EvalQuery {
            query: "cache evict expired ttl".into(),
            expected_files: vec!["src/cache.rs".into()],
            category: "function".into(),
        },
        EvalQuery {
            query: "api router handle request".into(),
            expected_files: vec!["src/api.rs".into()],
            category: "function".into(),
        },
        EvalQuery {
            query: "AuthService token refresh".into(),
            expected_files: vec!["src/auth.rs".into()],
            category: "type".into(),
        },
        EvalQuery {
            query: "Database connect migrate".into(),
            expected_files: vec!["src/database.rs".into()],
            category: "type".into(),
        },
        EvalQuery {
            query: "CacheLayer store HashMap".into(),
            expected_files: vec!["src/cache.rs".into()],
            category: "type".into(),
        },
    ]
}

#[test]
fn eval_harness_run_eval_produces_valid_scorecard() {
    let fixture = create_fixture_codebase();
    let index = BM25Index::build_from_directory(fixture.path());
    let config = HybridConfig::default();
    let queries = fixture_queries();

    let scorecard = run_eval(fixture.path(), &queries, &index, &config);

    assert_eq!(scorecard.total_queries, queries.len());
    assert!(
        scorecard.avg_recall_at_5 > 0.0,
        "R@5 should be > 0 (got {:.2})",
        scorecard.avg_recall_at_5
    );
    assert!(
        scorecard.avg_mrr > 0.0,
        "MRR should be > 0 (got {:.3})",
        scorecard.avg_mrr
    );

    for r in &scorecard.results {
        assert!(!r.query.is_empty());
        assert!(!r.expected_files.is_empty());
        assert!(r.recall_at_5 >= 0.0 && r.recall_at_5 <= 1.0);
        assert!(r.recall_at_10 >= 0.0 && r.recall_at_10 <= 1.0);
        assert!(r.mrr >= 0.0 && r.mrr <= 1.0);
    }

    assert!(
        !scorecard.per_category.is_empty(),
        "Should have category breakdowns"
    );
    for cat in &scorecard.per_category {
        assert!(cat.count > 0);
        assert!(!cat.category.is_empty());
    }

    let display = format!("{scorecard}");
    assert!(display.contains("R@5:"));
    assert!(display.contains("MRR:"));
}

#[test]
fn eval_harness_with_generate_self_eval() {
    let fixture = create_fixture_codebase();
    let index = BM25Index::build_from_directory(fixture.path());

    let queries = lean_ctx::core::eval_harness::generate_self_eval(&index, 5);
    assert!(
        !queries.is_empty(),
        "generate_self_eval should produce queries from indexed code"
    );

    let config = HybridConfig::default();
    let scorecard = run_eval(fixture.path(), &queries, &index, &config);

    assert_eq!(scorecard.total_queries, queries.len());
    assert!(
        scorecard.avg_recall_at_5 > 0.0,
        "Self-eval R@5 should be > 0 with matched queries"
    );
}
