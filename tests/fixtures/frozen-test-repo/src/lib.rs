pub mod types;
pub mod io;

use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

/// Generate a unique identifier.
pub fn generate_id() -> u64 {
    NEXT_ID.fetch_add(1, Ordering::Relaxed)
}

/// A generic result wrapper.
pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

#[derive(Debug, Clone)]
pub struct Metadata {
    pub id: u64,
    pub name: String,
    pub created_at: u64,
}

impl Metadata {
    pub fn new(name: &str) -> Self {
        Self {
            id: generate_id(),
            name: name.to_string(),
            created_at: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs(),
        }
    }
}

pub fn greet(name: &str) -> String {
    format!("Hello, {}!", name)
}
