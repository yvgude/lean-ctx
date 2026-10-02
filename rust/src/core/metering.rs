//! Local, append-only metering for the free product.
//!
//! Metering is deliberately observational: failures are logged and never affect
//! an MCP tool response.  The JSONL file contains token counts only.

use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use std::fs::{OpenOptions, create_dir_all};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use fs2::FileExt;

const METERING_FILE: &str = "metering.jsonl";

/// `tool_name` of a native shell call the hook left to the agent's own tool
/// (#1285): observed, but not routed through lean-ctx and not tokenized.
pub const NATIVE_PASSTHROUGH_TOOL: &str = "native_shell_passthrough";

/// Block size for reading the metering file backwards.
const TAIL_BLOCK_BYTES: usize = 64 * 1024;

/// Totals for one UTC day.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DayTotals {
    pub input_tokens: u64,
    pub savings_tokens: u64,
    /// Calls that went through lean-ctx (every metered `ctx_*` call).
    pub routed_calls: u64,
    pub native_passthrough_calls: u64,
}

/// One completed tool call, recorded locally as a JSONL line.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MeterEntry {
    pub timestamp: DateTime<Utc>,
    pub tool_name: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub savings_tokens: u64,
}

impl MeterEntry {
    #[must_use]
    pub fn new(
        tool_name: impl Into<String>,
        input_tokens: u64,
        output_tokens: u64,
        savings_tokens: u64,
    ) -> Self {
        Self {
            timestamp: Utc::now(),
            tool_name: tool_name.into(),
            input_tokens,
            output_tokens,
            savings_tokens,
        }
    }
}

/// Aggregate used by local value displays such as "Pro would have saved you $X".
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct MeterAggregate {
    pub total_savings_tokens: u64,
    pub total_calls: u64,
    /// Weighted output/input ratio. `0.0` means no metered input exists yet.
    pub avg_compression_ratio: f64,
}

/// Append-only local JSONL store.
#[derive(Debug, Clone)]
pub struct MeterStore {
    path: PathBuf,
}

impl MeterStore {
    #[must_use]
    pub fn new(data_dir: impl AsRef<Path>) -> Self {
        Self {
            path: data_dir.as_ref().join(METERING_FILE),
        }
    }

    pub fn from_data_dir() -> Result<Self, String> {
        Ok(Self::new(crate::core::data_dir::lean_ctx_data_dir()?))
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Appends one complete JSON object and newline. Call this only off the hot path.
    pub fn append(&self, entry: &MeterEntry) -> Result<(), String> {
        let Some(parent) = self.path.parent() else {
            return Err("metering path has no parent".to_string());
        };
        create_dir_all(parent).map_err(|error| error.to_string())?;
        let line = serde_json::to_string(entry).map_err(|error| error.to_string())?;
        let mut file = OpenOptions::new()
            .create(true)
            .read(true)
            .append(true)
            .open(&self.path)
            .map_err(|error| error.to_string())?;
        file.lock_exclusive().map_err(|error| error.to_string())?;
        let write_result = writeln!(file, "{line}").map_err(|error| error.to_string());
        let unlock_result = FileExt::unlock(&file).map_err(|error| error.to_string());
        write_result.and(unlock_result)
    }

    /// Schedules persistence and returns immediately; metering cannot delay a tool call.
    pub fn append_best_effort(entry: MeterEntry) {
        tokio::task::spawn_blocking(move || {
            let result = Self::from_data_dir().and_then(|store| store.append(&entry));
            if let Err(error) = result {
                tracing::warn!(%error, "lean-ctx: failed to append local metering entry");
            }
        });
    }

    #[must_use]
    pub fn aggregate(&self) -> MeterAggregate {
        let Ok(file) = std::fs::File::open(&self.path) else {
            return MeterAggregate::default();
        };
        let mut aggregate = MeterAggregate::default();
        let mut total_input = 0_u64;
        let mut total_output = 0_u64;
        for line in BufReader::new(file).lines().map_while(Result::ok) {
            let Ok(entry) = serde_json::from_str::<MeterEntry>(&line) else {
                continue;
            };
            aggregate.total_calls = aggregate.total_calls.saturating_add(1);
            aggregate.total_savings_tokens = aggregate
                .total_savings_tokens
                .saturating_add(entry.savings_tokens);
            total_input = total_input.saturating_add(entry.input_tokens);
            total_output = total_output.saturating_add(entry.output_tokens);
        }
        if total_input > 0 {
            aggregate.avg_compression_ratio = total_output as f64 / total_input as f64;
        }
        aggregate
    }

    /// Totals for the UTC day `day`. The file is append-only and chronological and
    /// grows to tens of MB, so it is read backwards and parsing stops at the first
    /// earlier entry: the cost follows the day's volume, not the history.
    #[must_use]
    pub fn day_totals(&self, day: NaiveDate) -> DayTotals {
        self.day_totals_in_blocks(day, TAIL_BLOCK_BYTES)
    }

    fn day_totals_in_blocks(&self, day: NaiveDate, block: usize) -> DayTotals {
        let mut totals = DayTotals::default();
        let Ok(mut file) = std::fs::File::open(&self.path) else {
            return totals;
        };
        let Ok(mut pos) = file.seek(SeekFrom::End(0)) else {
            return totals;
        };
        // Bytes of a line that started in an earlier block, carried forward.
        let mut carry = Vec::new();
        while pos > 0 {
            let len = usize::try_from(pos).map_or(block, |p| p.min(block));
            pos -= len as u64;
            let mut buf = vec![0; len];
            if file.seek(SeekFrom::Start(pos)).is_err() || file.read_exact(&mut buf).is_err() {
                return totals;
            }
            buf.extend_from_slice(&carry);
            let mut lines: Vec<&[u8]> = buf.split(|b| *b == b'\n').collect();
            // The first segment may be cut off unless the block starts the file.
            carry = if pos > 0 {
                lines.remove(0).to_vec()
            } else {
                Vec::new()
            };
            for line in lines.into_iter().rev() {
                let Ok(entry) = serde_json::from_slice::<MeterEntry>(line) else {
                    continue;
                };
                let date = entry.timestamp.date_naive();
                if date < day {
                    return totals;
                }
                if date > day {
                    continue;
                }
                totals.input_tokens = totals.input_tokens.saturating_add(entry.input_tokens);
                totals.savings_tokens = totals.savings_tokens.saturating_add(entry.savings_tokens);
                if entry.tool_name == NATIVE_PASSTHROUGH_TOOL {
                    totals.native_passthrough_calls += 1;
                } else {
                    totals.routed_calls += 1;
                }
            }
        }
        totals
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aggregates_jsonl_entries() {
        let dir = tempfile::tempdir().unwrap();
        let store = MeterStore::new(dir.path());
        store
            .append(&MeterEntry::new("ctx_read", 100, 20, 80))
            .unwrap();
        store
            .append(&MeterEntry::new("ctx_search", 200, 100, 100))
            .unwrap();

        let aggregate = store.aggregate();
        assert_eq!(aggregate.total_calls, 2);
        assert_eq!(aggregate.total_savings_tokens, 180);
        assert!((aggregate.avg_compression_ratio - 0.4).abs() < f64::EPSILON);
    }

    #[test]
    fn day_totals_reads_only_the_requested_day_across_block_boundaries() {
        let dir = tempfile::tempdir().unwrap();
        let store = MeterStore::new(dir.path());
        let at = |ts: &str, tool: &str, input, saved| MeterEntry {
            timestamp: ts.parse().unwrap(),
            tool_name: tool.into(),
            input_tokens: input,
            output_tokens: input - saved,
            savings_tokens: saved,
        };
        store
            .append(&at("2026-10-01T23:59:59Z", "ctx_read", 1_000, 900))
            .unwrap();
        store
            .append(&at("2026-10-02T00:00:01Z", "ctx_read", 100, 80))
            .unwrap();
        std::fs::OpenOptions::new()
            .append(true)
            .open(store.path())
            .unwrap()
            .write_all(b"{not json\n")
            .unwrap();
        store
            .append(&at("2026-10-02T09:00:00Z", NATIVE_PASSTHROUGH_TOOL, 0, 0))
            .unwrap();
        store
            .append(&at("2026-10-02T10:00:00Z", "ctx_search", 50, 10))
            .unwrap();

        let day = NaiveDate::from_ymd_opt(2026, 10, 2).unwrap();
        let expected = DayTotals {
            input_tokens: 150,
            savings_tokens: 90,
            routed_calls: 2,
            native_passthrough_calls: 1,
        };
        assert_eq!(store.day_totals(day), expected);
        // Blocks smaller than a line: every line is split across reads.
        assert_eq!(store.day_totals_in_blocks(day, 7), expected);
        assert_eq!(
            MeterStore::new(dir.path().join("missing")).day_totals(day),
            DayTotals::default()
        );
    }

    #[tokio::test]
    async fn best_effort_append_does_not_block_calling_task() {
        let entry = MeterEntry::new("ctx_read", 10, 2, 8);
        let task = tokio::spawn(async move {
            MeterStore::append_best_effort(entry);
            42_u8
        });
        assert_eq!(task.await.unwrap(), 42);
    }
}
