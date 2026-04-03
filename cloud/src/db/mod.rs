pub mod queries;
pub mod schema;

use rusqlite::Connection;
use std::path::Path;
use std::sync::{Arc, Mutex};

pub type DbPool = Arc<Mutex<Connection>>;

pub fn init_db(path: &str) -> DbPool {
    let conn = Connection::open(path).expect("Failed to open database");
    conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")
        .expect("Failed to set pragmas");

    let migration = include_str!("../../migrations/001_init.sql");
    conn.execute_batch(migration).expect("Failed to run migrations");

    Arc::new(Mutex::new(conn))
}

pub fn init_memory_db() -> DbPool {
    let conn = Connection::open_in_memory().expect("Failed to open in-memory database");
    conn.execute_batch("PRAGMA foreign_keys=ON;")
        .expect("Failed to set pragmas");

    let migration = include_str!("../../migrations/001_init.sql");
    conn.execute_batch(migration).expect("Failed to run migrations");

    Arc::new(Mutex::new(conn))
}

pub fn db_path() -> String {
    if let Ok(path) = std::env::var("LEAN_CTX_DB_PATH") {
        if let Some(parent) = Path::new(&path).parent() {
            std::fs::create_dir_all(parent).ok();
        }
        return path;
    }
    let home = dirs::home_dir().unwrap_or_else(|| Path::new(".").to_path_buf());
    let dir = home.join(".lean-ctx").join("cloud");
    std::fs::create_dir_all(&dir).ok();
    dir.join("leanctx.db").to_string_lossy().to_string()
}
