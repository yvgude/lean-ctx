use crate::core::events::LeanCtxEvent;
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::PathBuf;

pub struct EventTail {
    path: PathBuf,
    offset: u64,
}

impl EventTail {
    pub fn new() -> Self {
        let base = crate::core::data_dir::lean_ctx_data_dir()
            .unwrap_or_else(|_| dirs::home_dir().unwrap_or_default().join(".lean-ctx"));
        let path = base.join("events.jsonl");
        let offset = std::fs::metadata(&path).map_or(0, |m| m.len());
        Self { path, offset }
    }

    pub fn poll(&mut self) -> Vec<LeanCtxEvent> {
        let Ok(mut file) = std::fs::File::open(&self.path) else {
            return Vec::new();
        };
        let meta_len = file.metadata().map_or(0, |m| m.len());
        if meta_len < self.offset {
            self.offset = 0;
        }
        if meta_len == self.offset {
            return Vec::new();
        }

        let _ = file.seek(SeekFrom::Start(self.offset));
        let reader = BufReader::new(&file);
        let mut events = Vec::new();
        let mut bytes_read: u64 = 0;

        for line in reader.lines() {
            let Ok(line) = line else { break };
            bytes_read += line.len() as u64 + 1; // +1 for newline
            if line.trim().is_empty() {
                continue;
            }
            if let Ok(event) = serde_json::from_str::<LeanCtxEvent>(&line) {
                events.push(event);
            }
        }

        self.offset += bytes_read;
        events
    }
}
