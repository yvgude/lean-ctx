//! Embedded benchmark task fixtures.
//!
//! Each fixture is a self-contained coding scenario with source content,
//! a task description, and a machine-verifiable success predicate.

use serde::{Deserialize, Serialize};

/// A self-contained benchmark task with source material and verification.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskFixture {
    pub id: String,
    pub category: TaskCategory,
    pub description: String,
    /// Source file content the agent would read.
    pub source_content: String,
    /// File extension for mode selection.
    pub extension: String,
    /// Strings that MUST appear in the compressed output for it to be
    /// considered quality-preserving. These represent critical information
    /// an agent needs to complete the task.
    pub required_signals: Vec<String>,
    /// Strings that SHOULD appear (soft quality check, not required).
    pub preferred_signals: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TaskCategory {
    FileRead,
    CodeSearch,
    BugFix,
    Refactoring,
    Documentation,
    ShellOutput,
}

impl TaskCategory {
    pub fn label(self) -> &'static str {
        match self {
            Self::FileRead => "file-read",
            Self::CodeSearch => "code-search",
            Self::BugFix => "bug-fix",
            Self::Refactoring => "refactoring",
            Self::Documentation => "documentation",
            Self::ShellOutput => "shell-output",
        }
    }
}

/// Quality score for a single task run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QualityScore {
    pub required_found: usize,
    pub required_total: usize,
    pub preferred_found: usize,
    pub preferred_total: usize,
}

impl QualityScore {
    pub fn required_ratio(&self) -> f64 {
        if self.required_total == 0 {
            return 1.0;
        }
        self.required_found as f64 / self.required_total as f64
    }

    pub fn overall_score(&self) -> f64 {
        let req = self.required_ratio();
        let pref = if self.preferred_total == 0 {
            1.0
        } else {
            self.preferred_found as f64 / self.preferred_total as f64
        };
        req * 0.8 + pref * 0.2
    }

    pub fn passes(&self) -> bool {
        self.required_found == self.required_total
    }
}

impl TaskFixture {
    /// Score compressed output against this fixture's signals.
    pub fn score(&self, compressed_output: &str) -> QualityScore {
        let required_found = self
            .required_signals
            .iter()
            .filter(|s| compressed_output.contains(s.as_str()))
            .count();
        let preferred_found = self
            .preferred_signals
            .iter()
            .filter(|s| compressed_output.contains(s.as_str()))
            .count();

        QualityScore {
            required_found,
            required_total: self.required_signals.len(),
            preferred_found,
            preferred_total: self.preferred_signals.len(),
        }
    }
}

/// The canonical benchmark suite: 12 tasks across 6 categories.
pub fn canonical_suite() -> Vec<TaskFixture> {
    vec![
        fixture_read_rust_module(),
        fixture_read_typescript_component(),
        fixture_search_error_handling(),
        fixture_search_api_endpoints(),
        fixture_bugfix_off_by_one(),
        fixture_bugfix_null_check(),
        fixture_refactor_rename(),
        fixture_refactor_extract(),
        fixture_docs_api(),
        fixture_docs_readme(),
        fixture_shell_test_output(),
        fixture_shell_build_errors(),
    ]
}

fn fixture_read_rust_module() -> TaskFixture {
    TaskFixture {
        id: "read-rust-module".into(),
        category: TaskCategory::FileRead,
        description: "Read and understand a Rust module with structs, impls, and error handling"
            .into(),
        source_content: RUST_MODULE_SOURCE.into(),
        extension: "rs".into(),
        required_signals: vec![
            "struct Config".into(),
            "fn load".into(),
            "fn validate".into(),
            "ConfigError".into(),
            "max_connections".into(),
        ],
        preferred_signals: vec!["impl Default".into(), "pub fn".into(), "Result<".into()],
    }
}

fn fixture_read_typescript_component() -> TaskFixture {
    TaskFixture {
        id: "read-ts-component".into(),
        category: TaskCategory::FileRead,
        description: "Read a React TypeScript component with props, hooks, and event handlers"
            .into(),
        source_content: TS_COMPONENT_SOURCE.into(),
        extension: "tsx".into(),
        required_signals: vec![
            "UserProfile".into(),
            "useState".into(),
            "useEffect".into(),
            "onSubmit".into(),
            "interface".into(),
        ],
        preferred_signals: vec!["loading".into(), "error".into(), "async".into()],
    }
}

fn fixture_search_error_handling() -> TaskFixture {
    TaskFixture {
        id: "search-error-handling".into(),
        category: TaskCategory::CodeSearch,
        description: "Find all error handling patterns in a module".into(),
        source_content: ERROR_HANDLING_SOURCE.into(),
        extension: "rs".into(),
        required_signals: vec![
            "DatabaseError".into(),
            "NetworkError".into(),
            "ValidationError".into(),
            "fn handle_error".into(),
        ],
        preferred_signals: vec!["retry".into(), "fallback".into(), "context".into()],
    }
}

fn fixture_search_api_endpoints() -> TaskFixture {
    TaskFixture {
        id: "search-api-endpoints".into(),
        category: TaskCategory::CodeSearch,
        description: "Identify all API endpoint definitions and their methods".into(),
        source_content: API_ENDPOINTS_SOURCE.into(),
        extension: "rs".into(),
        required_signals: vec![
            "/api/users".into(),
            "/api/projects".into(),
            "GET".into(),
            "POST".into(),
            "DELETE".into(),
        ],
        preferred_signals: vec!["auth".into(), "middleware".into(), "handler".into()],
    }
}

fn fixture_bugfix_off_by_one() -> TaskFixture {
    TaskFixture {
        id: "bugfix-off-by-one".into(),
        category: TaskCategory::BugFix,
        description: "Identify an off-by-one error in pagination logic".into(),
        source_content: PAGINATION_BUG_SOURCE.into(),
        extension: "rs".into(),
        required_signals: vec![
            "fn paginate".into(),
            "page_size".into(),
            "total_items".into(),
            "offset".into(),
        ],
        preferred_signals: vec!["page *".into(), "start".into(), "end".into()],
    }
}

fn fixture_bugfix_null_check() -> TaskFixture {
    TaskFixture {
        id: "bugfix-null-check".into(),
        category: TaskCategory::BugFix,
        description: "Find a missing null/None check that causes a panic".into(),
        source_content: NULL_CHECK_BUG_SOURCE.into(),
        extension: "rs".into(),
        required_signals: vec![
            "fn process_user".into(),
            "unwrap".into(),
            "email".into(),
            "Option<".into(),
        ],
        preferred_signals: vec!["None".into(), "expect".into()],
    }
}

fn fixture_refactor_rename() -> TaskFixture {
    TaskFixture {
        id: "refactor-rename".into(),
        category: TaskCategory::Refactoring,
        description: "Rename a function and all its call sites".into(),
        source_content: RENAME_REFACTOR_SOURCE.into(),
        extension: "rs".into(),
        required_signals: vec![
            "fn get_data".into(),
            "get_data(".into(),
            "struct DataService".into(),
        ],
        preferred_signals: vec!["impl".into(), "pub fn".into(), "self".into()],
    }
}

fn fixture_refactor_extract() -> TaskFixture {
    TaskFixture {
        id: "refactor-extract".into(),
        category: TaskCategory::Refactoring,
        description: "Identify code that should be extracted into a separate function".into(),
        source_content: EXTRACT_REFACTOR_SOURCE.into(),
        extension: "rs".into(),
        required_signals: vec![
            "fn process_order".into(),
            "validate".into(),
            "calculate_total".into(),
            "apply_discount".into(),
        ],
        preferred_signals: vec!["tax".into(), "shipping".into(), "fn ".into()],
    }
}

fn fixture_docs_api() -> TaskFixture {
    TaskFixture {
        id: "docs-api".into(),
        category: TaskCategory::Documentation,
        description: "Generate API documentation from function signatures".into(),
        source_content: API_DOCS_SOURCE.into(),
        extension: "rs".into(),
        required_signals: vec![
            "fn create_user".into(),
            "fn delete_user".into(),
            "fn list_users".into(),
            "UserRequest".into(),
            "UserResponse".into(),
        ],
        preferred_signals: vec!["///".into(), "pub".into(), "Result".into()],
    }
}

fn fixture_docs_readme() -> TaskFixture {
    TaskFixture {
        id: "docs-readme".into(),
        category: TaskCategory::Documentation,
        description: "Understand project structure from a README".into(),
        source_content: README_SOURCE.into(),
        extension: "md".into(),
        required_signals: vec![
            "Installation".into(),
            "Usage".into(),
            "API".into(),
            "Configuration".into(),
        ],
        preferred_signals: vec!["example".into(), "license".into(), "contributing".into()],
    }
}

fn fixture_shell_test_output() -> TaskFixture {
    TaskFixture {
        id: "shell-test-output".into(),
        category: TaskCategory::ShellOutput,
        description: "Parse test runner output to identify failures".into(),
        source_content: TEST_OUTPUT_SOURCE.into(),
        extension: "txt".into(),
        required_signals: vec![
            "FAILED".into(),
            "test_pagination".into(),
            "assertion".into(),
            "3 passed".into(),
        ],
        preferred_signals: vec!["line".into(), "expected".into(), "actual".into()],
    }
}

fn fixture_shell_build_errors() -> TaskFixture {
    TaskFixture {
        id: "shell-build-errors".into(),
        category: TaskCategory::ShellOutput,
        description: "Parse compiler output to identify build errors".into(),
        source_content: BUILD_ERROR_SOURCE.into(),
        extension: "txt".into(),
        required_signals: vec!["error[E".into(), "cannot find".into(), "src/main.rs".into()],
        preferred_signals: vec!["help:".into(), "warning".into()],
    }
}

// ── Embedded source content ──────────────────────────────────────────

const RUST_MODULE_SOURCE: &str = r#"use std::path::Path;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub host: String,
    pub port: u16,
    pub max_connections: usize,
    pub timeout_ms: u64,
    pub database_url: String,
    pub log_level: String,
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("file not found: {0}")]
    FileNotFound(String),
    #[error("parse error: {0}")]
    ParseError(String),
    #[error("validation failed: {0}")]
    ValidationError(String),
}

impl Default for Config {
    fn default() -> Self {
        Self {
            host: "127.0.0.1".into(),
            port: 8080,
            max_connections: 100,
            timeout_ms: 30_000,
            database_url: "pg-local-fixture://app".into(),
            log_level: "info".into(),
        }
    }
}

impl Config {
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        let content = std::fs::read_to_string(path)
            .map_err(|e| ConfigError::FileNotFound(e.to_string()))?;
        let config: Config = toml::from_str(&content)
            .map_err(|e| ConfigError::ParseError(e.to_string()))?;
        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.port == 0 {
            return Err(ConfigError::ValidationError("port cannot be 0".into()));
        }
        if self.max_connections == 0 {
            return Err(ConfigError::ValidationError("max_connections cannot be 0".into()));
        }
        if self.timeout_ms < 1000 {
            return Err(ConfigError::ValidationError("timeout too low".into()));
        }
        Ok(())
    }

    pub fn connection_string(&self) -> String {
        format!("{}:{}", self.host, self.port)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_is_valid() {
        Config::default().validate().unwrap();
    }

    #[test]
    fn zero_port_fails_validation() {
        let mut cfg = Config::default();
        cfg.port = 0;
        assert!(cfg.validate().is_err());
    }
}"#;

const TS_COMPONENT_SOURCE: &str = r#"import React, { useState, useEffect, useCallback } from 'react';

interface UserProfileProps {
  userId: string;
  onUpdate?: (user: User) => void;
}

interface User {
  id: string;
  name: string;
  email: string;
  avatar: string;
  bio: string;
}

export const UserProfile: React.FC<UserProfileProps> = ({ userId, onUpdate }) => {
  const [user, setUser] = useState<User | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [editing, setEditing] = useState(false);

  useEffect(() => {
    const fetchUser = async () => {
      try {
        setLoading(true);
        const response = await fetch(`/api/users/${userId}`);
        if (!response.ok) throw new Error('Failed to fetch user');
        const data = await response.json();
        setUser(data);
      } catch (err) {
        setError(err instanceof Error ? err.message : 'Unknown error');
      } finally {
        setLoading(false);
      }
    };
    fetchUser();
  }, [userId]);

  const onSubmit = useCallback(async (formData: Partial<User>) => {
    if (!user) return;
    try {
      const response = await fetch(`/api/users/${userId}`, {
        method: 'PATCH',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(formData),
      });
      const updated = await response.json();
      setUser(updated);
      setEditing(false);
      onUpdate?.(updated);
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Update failed');
    }
  }, [user, userId, onUpdate]);

  if (loading) return <div className="spinner" />;
  if (error) return <div className="error">{error}</div>;
  if (!user) return null;

  return (
    <div className="user-profile">
      <img src={user.avatar} alt={user.name} />
      <h2>{user.name}</h2>
      <p>{user.email}</p>
      {editing ? (
        <form onSubmit={(e) => { e.preventDefault(); onSubmit({ name: user.name }); }}>
          <input defaultValue={user.name} />
          <button type="submit">Save</button>
        </form>
      ) : (
        <button onClick={() => setEditing(true)}>Edit</button>
      )}
    </div>
  );
};"#;

const ERROR_HANDLING_SOURCE: &str = r#"use std::time::Duration;

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("database error: {0}")]
    DatabaseError(String),
    #[error("network error: {0}")]
    NetworkError(String),
    #[error("validation error: {0}")]
    ValidationError(String),
    #[error("authentication failed")]
    AuthError,
}

pub struct ErrorHandler {
    max_retries: u32,
    retry_delay: Duration,
}

impl ErrorHandler {
    pub fn new(max_retries: u32, retry_delay: Duration) -> Self {
        Self { max_retries, retry_delay }
    }

    pub fn handle_error(&self, error: &AppError) -> ErrorAction {
        match error {
            AppError::DatabaseError(msg) => {
                if msg.contains("connection") {
                    ErrorAction::Retry { delay: self.retry_delay }
                } else {
                    ErrorAction::Fail
                }
            }
            AppError::NetworkError(_) => {
                ErrorAction::Retry { delay: self.retry_delay }
            }
            AppError::ValidationError(_) => ErrorAction::Fail,
            AppError::AuthError => ErrorAction::Fallback,
        }
    }

    pub fn with_retry<T, F>(&self, mut operation: F) -> Result<T, AppError>
    where
        F: FnMut() -> Result<T, AppError>,
    {
        let mut attempts = 0;
        loop {
            match operation() {
                Ok(val) => return Ok(val),
                Err(e) => {
                    attempts += 1;
                    let action = self.handle_error(&e);
                    match action {
                        ErrorAction::Retry { delay } if attempts < self.max_retries => {
                            std::thread::sleep(delay);
                            let context = format!("retry {attempts}/{}", self.max_retries);
                            eprintln!("[{context}] {e}");
                        }
                        ErrorAction::Fallback => {
                            eprintln!("using fallback for: {e}");
                            return Err(e);
                        }
                        _ => return Err(e),
                    }
                }
            }
        }
    }
}

pub enum ErrorAction {
    Retry { delay: Duration },
    Fail,
    Fallback,
}"#;

const API_ENDPOINTS_SOURCE: &str = r#"use axum::{Router, routing::{get, post, delete}, middleware};

pub fn api_router() -> Router {
    Router::new()
        .route("/api/users", get(list_users).post(create_user))
        .route("/api/users/:id", get(get_user).delete(delete_user))
        .route("/api/projects", get(list_projects).post(create_project))
        .route("/api/projects/:id", get(get_project).delete(delete_project))
        .route("/api/projects/:id/members", post(add_member))
        .layer(middleware::from_fn(auth_middleware))

}

async fn list_users() -> impl IntoResponse { /* GET /api/users */ }
async fn create_user() -> impl IntoResponse { /* POST /api/users */ }
async fn get_user() -> impl IntoResponse { /* GET /api/users/:id */ }
async fn delete_user() -> impl IntoResponse { /* DELETE /api/users/:id */ }
async fn list_projects() -> impl IntoResponse { /* GET /api/projects */ }
async fn create_project() -> impl IntoResponse { /* POST /api/projects */ }
async fn get_project() -> impl IntoResponse { /* GET /api/projects/:id */ }
async fn delete_project() -> impl IntoResponse { /* DELETE /api/projects/:id */ }
async fn add_member() -> impl IntoResponse { /* POST /api/projects/:id/members */ }

async fn auth_middleware(req: Request, next: Next) -> Response {
    let token = req.headers().get("Authorization");
    if token.is_none() {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let handler = next.run(req).await;
    handler
}"#;

const PAGINATION_BUG_SOURCE: &str = r"pub struct Paginator {
    page_size: usize,
    total_items: usize,
}

impl Paginator {
    pub fn new(page_size: usize, total_items: usize) -> Self {
        Self { page_size, total_items }
    }

    /// Returns (start, end) indices for the given page number.
    /// BUG: page is 1-indexed but offset calculation assumes 0-indexed.
    pub fn paginate(&self, page: usize) -> (usize, usize) {
        let offset = page * self.page_size; // Should be (page - 1) * self.page_size
        let end = (offset + self.page_size).min(self.total_items);
        (offset, end)
    }

    pub fn total_pages(&self) -> usize {
        (self.total_items + self.page_size - 1) / self.page_size
    }

    pub fn items_on_page(&self, page: usize) -> usize {
        let (start, end) = self.paginate(page);
        if start >= self.total_items {
            0
        } else {
            end - start
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_page_returns_correct_range() {
        let p = Paginator::new(10, 25);
        // BUG: returns (10, 20) instead of (0, 10) for page 1
        let (start, end) = p.paginate(1);
        assert_eq!(start, 0);
        assert_eq!(end, 10);
    }
}";

const NULL_CHECK_BUG_SOURCE: &str = r#"pub struct UserRecord {
    pub id: u64,
    pub name: String,
    pub email: Option<String>,
    pub role: String,
}

pub fn process_user(user: &UserRecord) -> String {
    let greeting = format!("Hello, {}!", user.name);

    // BUG: unwrap() on Option<String> without checking for None
    let email_domain = user.email.as_ref().unwrap().split('@').last().unwrap();
    let is_internal = email_domain == "company.com";

    if is_internal {
        format!("{greeting} [internal user]")
    } else {
        format!("{greeting} [external: {email_domain}]")
    }
}

pub fn process_batch(users: &[UserRecord]) -> Vec<String> {
    users.iter().map(process_user).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[should_panic]
    fn process_user_panics_on_none_email() {
        let user = UserRecord {
            id: 1,
            name: "Test".into(),
            email: None,
            role: "admin".into(),
        };
        process_user(&user); // panics!
    }
}"#;

const RENAME_REFACTOR_SOURCE: &str = r#"pub struct DataService {
    base_url: String,
    timeout: u64,
}

impl DataService {
    pub fn new(base_url: &str, timeout: u64) -> Self {
        Self {
            base_url: base_url.to_string(),
            timeout,
        }
    }

    pub fn get_data(&self, key: &str) -> Result<String, String> {
        let url = format!("{}/data/{}", self.base_url, key);
        self.fetch(&url)
    }

    pub fn get_data_batch(&self, keys: &[&str]) -> Vec<Result<String, String>> {
        keys.iter().map(|k| self.get_data(k)).collect()
    }

    fn fetch(&self, _url: &str) -> Result<String, String> {
        Ok("response".into())
    }
}

pub fn main_handler(svc: &DataService) {
    match svc.get_data("config") {
        Ok(data) => println!("Config: {data}"),
        Err(e) => eprintln!("Failed to get_data: {e}"),
    }

    let results = svc.get_data_batch(&["a", "b", "c"]);
    for r in results {
        if let Ok(d) = r {
            println!("Got: {d}");
        }
    }
}"#;

const EXTRACT_REFACTOR_SOURCE: &str = r#"pub struct Order {
    pub items: Vec<OrderItem>,
    pub customer_id: u64,
    pub coupon_code: Option<String>,
}

pub struct OrderItem {
    pub name: String,
    pub price: f64,
    pub quantity: u32,
    pub taxable: bool,
}

pub fn process_order(order: &Order) -> Result<f64, String> {
    // Validate items
    if order.items.is_empty() {
        return Err("order has no items".into());
    }
    for item in &order.items {
        if item.price < 0.0 {
            return Err(format!("invalid price for {}", item.name));
        }
        if item.quantity == 0 {
            return Err(format!("zero quantity for {}", item.name));
        }
    }

    // Calculate total
    let mut subtotal = 0.0;
    for item in &order.items {
        subtotal += item.price * item.quantity as f64;
    }

    // Apply discount
    let discount = match &order.coupon_code {
        Some(code) if code == "SAVE10" => subtotal * 0.10,
        Some(code) if code == "SAVE20" => subtotal * 0.20,
        _ => 0.0,
    };
    let after_discount = subtotal - discount;

    // Calculate tax
    let tax: f64 = order.items.iter()
        .filter(|i| i.taxable)
        .map(|i| i.price * i.quantity as f64 * 0.08)
        .sum();

    // Calculate shipping
    let shipping = if after_discount > 50.0 { 0.0 } else { 5.99 };

    let total = after_discount + tax + shipping;
    Ok(total)
}

pub fn validate(order: &Order) -> Result<(), String> {
    if order.items.is_empty() {
        return Err("empty".into());
    }
    Ok(())
}

pub fn calculate_total(items: &[OrderItem]) -> f64 {
    items.iter().map(|i| i.price * i.quantity as f64).sum()
}

pub fn apply_discount(subtotal: f64, code: Option<&str>) -> f64 {
    match code {
        Some("SAVE10") => subtotal * 0.90,
        Some("SAVE20") => subtotal * 0.80,
        _ => subtotal,
    }
}"#;

const API_DOCS_SOURCE: &str = r"use serde::{Deserialize, Serialize};

/// Request payload for user creation.
#[derive(Debug, Deserialize)]
pub struct UserRequest {
    pub name: String,
    pub email: String,
    pub role: Option<String>,
}

/// Response payload with user details.
#[derive(Debug, Serialize)]
pub struct UserResponse {
    pub id: u64,
    pub name: String,
    pub email: String,
    pub role: String,
    pub created_at: String,
}

/// Creates a new user account.
///
/// # Arguments
/// * `req` - User creation request with name, email, and optional role
///
/// # Returns
/// The created user with assigned ID and timestamps
///
/// # Errors
/// Returns error if email is already taken or validation fails
pub fn create_user(req: UserRequest) -> Result<UserResponse, ApiError> {
    todo!()
}

/// Deletes a user by ID.
///
/// # Arguments
/// * `user_id` - The unique user identifier
///
/// # Returns
/// `true` if the user was deleted, `false` if not found
pub fn delete_user(user_id: u64) -> Result<bool, ApiError> {
    todo!()
}

/// Lists all users with optional pagination.
///
/// # Arguments
/// * `page` - Page number (1-indexed)
/// * `per_page` - Items per page (default 20, max 100)
pub fn list_users(page: Option<u32>, per_page: Option<u32>) -> Result<Vec<UserResponse>, ApiError> {
    todo!()
}

#[derive(Debug)]
pub enum ApiError {
    NotFound,
    Conflict(String),
    Internal(String),
}";

const README_SOURCE: &str = r"# MyProject

A high-performance data processing library.

## Installation

```bash
cargo add myproject
```

## Usage

```rust
use myproject::Pipeline;

let pipeline = Pipeline::new()
    .add_stage(Transform::uppercase)
    .add_stage(Filter::non_empty);

let results = pipeline.run(input_data);
```

## API

### `Pipeline::new()`
Creates a new empty pipeline.

### `Pipeline::add_stage(stage)`
Adds a processing stage to the pipeline. Stages are executed in order.

### `Pipeline::run(data)`
Executes all stages on the input data and returns results.

## Configuration

Configuration is loaded from `config.toml`:

```toml
[pipeline]
max_workers = 4
buffer_size = 1024
timeout_ms = 30000
```

## Examples

See the `examples/` directory for more usage examples.

## Contributing

1. Fork the repository
2. Create a feature branch
3. Submit a pull request

## License

MIT
";

const TEST_OUTPUT_SOURCE: &str = r#"running 5 tests
test test_create_user ... ok
test test_list_users ... ok
test test_delete_user ... ok
test test_pagination ... FAILED
test test_bulk_import ... FAILED

failures:

---- test_pagination stdout ----
thread 'test_pagination' panicked at src/pagination.rs:42:5:
assertion `left == right` failed
  left: 10
 right: 0
note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace

---- test_bulk_import stdout ----
thread 'test_bulk_import' panicked at src/import.rs:88:14:
called `Result::unwrap()` on an `Err` value: "duplicate key"

failures:
    test_bulk_import
    test_pagination

test result: FAILED. 3 passed; 2 failed; 0 ignored; 0 measured; 0 filtered out
"#;

const BUILD_ERROR_SOURCE: &str = r"error[E0433]: cannot find value `connection_pool` in this scope
 --> src/main.rs:15:9
  |
15|     let pool = connection_pool();
  |                ^^^^^^^^^^^^^^^ not found in this scope
  |
help: consider importing this function
  |
1 | use crate::db::connection_pool;
  |

error[E0308]: mismatched types
 --> src/main.rs:28:20
  |
28|     let count: i32 = query.count();
  |                ----   ^^^^^^^^^^^^ expected `i32`, found `i64`
  |                |
  |                expected due to this
  |
help: you can convert an `i64` to an `i32` and panic if the converted value doesn't fit
  |
28|     let count: i32 = query.count().try_into().unwrap();
  |                      ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^

warning: unused import: `std::collections::HashMap`
 --> src/main.rs:3:5
  |
3 | use std::collections::HashMap;
  |     ^^^^^^^^^^^^^^^^^^^^^^^^^
  |
  = note: `#[warn(unused_imports)]` on by default

error: aborting due to 2 previous errors; 1 warning emitted
";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_suite_has_12_tasks() {
        let suite = canonical_suite();
        assert_eq!(suite.len(), 12);
    }

    #[test]
    fn all_tasks_have_unique_ids() {
        let suite = canonical_suite();
        let ids: std::collections::HashSet<_> = suite.iter().map(|t| &t.id).collect();
        assert_eq!(ids.len(), suite.len());
    }

    #[test]
    fn stock_output_passes_all_required_signals() {
        for task in canonical_suite() {
            let score = task.score(&task.source_content);
            assert!(
                score.passes(),
                "task {} failed: {}/{} required signals found",
                task.id,
                score.required_found,
                score.required_total
            );
        }
    }

    #[test]
    fn quality_score_arithmetic() {
        let score = QualityScore {
            required_found: 4,
            required_total: 5,
            preferred_found: 2,
            preferred_total: 4,
        };
        assert!((score.required_ratio() - 0.8).abs() < 0.001);
        assert!((score.overall_score() - 0.74).abs() < 0.001);
        assert!(!score.passes());
    }

    #[test]
    fn all_categories_represented() {
        let suite = canonical_suite();
        let categories: std::collections::HashSet<_> =
            suite.iter().map(|t| t.category.label()).collect();
        assert!(categories.contains("file-read"));
        assert!(categories.contains("code-search"));
        assert!(categories.contains("bug-fix"));
        assert!(categories.contains("refactoring"));
        assert!(categories.contains("documentation"));
        assert!(categories.contains("shell-output"));
    }
}
