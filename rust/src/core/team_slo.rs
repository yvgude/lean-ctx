//! Team-server SLO instrumentation — the measurement half of the hosted-index
//! reliability gate (GL #391).
//!
//! A process-global rolling window of request samples feeds three derived
//! signals:
//!
//! * `p50/p95/p99` request latency (ms) across team `/v1/*` routes
//! * availability — share of requests that did **not** end in a server error
//!   (5xx). Client errors (4xx, e.g. `tool_error`, scope denials) are the
//!   caller's problem and intentionally do not count against availability.
//! * index freshness — seconds since the last successful index-mutating tool
//!   call. This is a *staleness indicator*, not the end-to-end push→query lag
//!   (the control-plane probe measures that); see `docs/runbooks/hosted-index.md`.
//!
//! The store lives in `core` so `core::slo::read_metric` can consume it
//! without a dependency cycle (`http_server` already depends on `core`).

use serde::Serialize;
use std::collections::VecDeque;
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

/// Rolling window size. 4096 samples ≈ hours of traffic on a typical team
/// server while staying trivially cheap to sort for percentiles.
const WINDOW: usize = 4096;

#[derive(Debug, Clone, Copy)]
struct Sample {
    duration_ms: u32,
    ok: bool,
}

#[derive(Debug, Default)]
struct Inner {
    samples: VecDeque<Sample>,
    requests_total: u64,
    errors_total: u64,
    last_index_write_unix: Option<u64>,
    started_unix: Option<u64>,
}

/// Process-global team SLO statistics store.
pub struct TeamSloStats {
    inner: Mutex<Inner>,
}

static STORE: OnceLock<TeamSloStats> = OnceLock::new();

/// The process-global store. Cheap to call; lazily initialised.
pub fn global() -> &'static TeamSloStats {
    STORE.get_or_init(|| TeamSloStats {
        inner: Mutex::new(Inner::default()),
    })
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

impl TeamSloStats {
    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Marks the server as started (uptime baseline). Idempotent — the first
    /// call wins so restarts inside one process (tests) keep a stable origin.
    pub fn mark_started(&self) {
        let mut g = self.lock();
        if g.started_unix.is_none() {
            g.started_unix = Some(now_unix());
        }
    }

    /// Records one finished request. `ok == false` means a *server* failure
    /// (5xx); client errors must be recorded with `ok == true`.
    pub fn record_request(&self, duration_ms: u64, ok: bool) {
        let mut g = self.lock();
        if g.samples.len() == WINDOW {
            g.samples.pop_front();
        }
        g.samples.push_back(Sample {
            duration_ms: duration_ms.min(u64::from(u32::MAX)) as u32,
            ok,
        });
        g.requests_total += 1;
        if !ok {
            g.errors_total += 1;
        }
    }

    /// Records a successful index-mutating operation (freshness baseline).
    pub fn record_index_write(&self) {
        self.lock().last_index_write_unix = Some(now_unix());
    }

    /// Current derived snapshot.
    pub fn snapshot(&self) -> TeamSloSnapshot {
        let g = self.lock();
        let now = now_unix();

        let mut durations: Vec<u32> = g.samples.iter().map(|s| s.duration_ms).collect();
        durations.sort_unstable();
        let pct = |p: f64| -> f64 {
            if durations.is_empty() {
                return 0.0;
            }
            // Nearest-rank percentile on the sorted window.
            let rank = ((p / 100.0) * durations.len() as f64).ceil() as usize;
            let idx = rank.clamp(1, durations.len()) - 1;
            f64::from(durations[idx])
        };

        let window_len = g.samples.len();
        let ok_in_window = g.samples.iter().filter(|s| s.ok).count();
        // No traffic means no observed failures: report full availability
        // rather than a false alarm on idle servers.
        let availability_pct = if window_len == 0 {
            100.0
        } else {
            (ok_in_window as f64 / window_len as f64) * 100.0
        };

        TeamSloSnapshot {
            requests_total: g.requests_total,
            errors_total: g.errors_total,
            window_len,
            p50_ms: pct(50.0),
            p95_ms: pct(95.0),
            p99_ms: pct(99.0),
            availability_pct,
            index_lag_seconds: g
                .last_index_write_unix
                .map(|t| now.saturating_sub(t) as f64),
            uptime_seconds: g.started_unix.map(|t| now.saturating_sub(t)),
        }
    }

    /// Test-only: reset all state so unit tests stay order-independent.
    #[cfg(test)]
    fn reset(&self) {
        *self.lock() = Inner::default();
    }
}

/// Point-in-time view of the team server's SLO signals.
#[derive(Debug, Clone, Serialize)]
pub struct TeamSloSnapshot {
    pub requests_total: u64,
    pub errors_total: u64,
    /// Number of samples currently in the rolling window.
    pub window_len: usize,
    pub p50_ms: f64,
    pub p95_ms: f64,
    pub p99_ms: f64,
    /// Percentage (0–100) of non-5xx requests in the rolling window.
    pub availability_pct: f64,
    /// Seconds since the last successful index write; `None` until one happened.
    pub index_lag_seconds: Option<f64>,
    /// Seconds since `mark_started`; `None` outside a serving process.
    pub uptime_seconds: Option<u64>,
}

impl TeamSloSnapshot {
    /// Prometheus text exposition (format 0.0.4) under the `leanctx_team_*`
    /// namespace, scrapeable by any Prometheus-compatible agent.
    #[must_use]
    pub fn to_prometheus(&self) -> String {
        let mut out = String::with_capacity(640);
        let mut gauge = |name: &str, help: &str, value: f64| {
            out.push_str(&format!(
                "# HELP {name} {help}\n# TYPE {name} gauge\n{name} {value}\n"
            ));
        };
        gauge(
            "leanctx_team_request_duration_p50_ms",
            "Rolling p50 request latency over team /v1 routes",
            self.p50_ms,
        );
        gauge(
            "leanctx_team_request_duration_p95_ms",
            "Rolling p95 request latency over team /v1 routes",
            self.p95_ms,
        );
        gauge(
            "leanctx_team_request_duration_p99_ms",
            "Rolling p99 request latency over team /v1 routes",
            self.p99_ms,
        );
        gauge(
            "leanctx_team_availability_pct",
            "Share of non-5xx requests in the rolling window (percent)",
            self.availability_pct,
        );
        if let Some(lag) = self.index_lag_seconds {
            gauge(
                "leanctx_team_index_lag_seconds",
                "Seconds since the last successful index-mutating tool call",
                lag,
            );
        }
        if let Some(up) = self.uptime_seconds {
            gauge(
                "leanctx_team_uptime_seconds",
                "Seconds since the team server started",
                up as f64,
            );
        }
        // Counters last: they use a different TYPE.
        out.push_str(&format!(
            "# HELP leanctx_team_requests_total Total requests observed on team /v1 routes\n# TYPE leanctx_team_requests_total counter\nleanctx_team_requests_total {}\n",
            self.requests_total
        ));
        out.push_str(&format!(
            "# HELP leanctx_team_errors_total Total 5xx responses on team /v1 routes\n# TYPE leanctx_team_errors_total counter\nleanctx_team_errors_total {}\n",
            self.errors_total
        ));
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Tests share the process-global store and cargo runs them in parallel.
    /// A test-local mutex serialises them; each holder starts from a clean
    /// slate. The guard must stay alive for the whole test body.
    static TEST_LOCK: Mutex<()> = Mutex::new(());

    fn fresh() -> (&'static TeamSloStats, std::sync::MutexGuard<'static, ()>) {
        let guard = TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let s = global();
        s.reset();
        (s, guard)
    }

    #[test]
    fn empty_store_reports_idle_healthy() {
        let (s, _guard) = fresh();
        let snap = s.snapshot();
        assert_eq!(snap.window_len, 0);
        assert_eq!(snap.availability_pct, 100.0);
        assert_eq!(snap.p95_ms, 0.0);
        assert!(snap.index_lag_seconds.is_none());
    }

    #[test]
    fn percentiles_use_nearest_rank() {
        let (s, _guard) = fresh();
        for ms in 1..=100u64 {
            s.record_request(ms, true);
        }
        let snap = s.snapshot();
        assert_eq!(snap.p50_ms, 50.0);
        assert_eq!(snap.p95_ms, 95.0);
        assert_eq!(snap.p99_ms, 99.0);
        assert_eq!(snap.window_len, 100);
    }

    #[test]
    fn availability_counts_only_server_errors() {
        let (s, _guard) = fresh();
        for _ in 0..98 {
            s.record_request(10, true);
        }
        s.record_request(10, false);
        s.record_request(10, false);
        let snap = s.snapshot();
        assert_eq!(snap.requests_total, 100);
        assert_eq!(snap.errors_total, 2);
        assert!((snap.availability_pct - 98.0).abs() < f64::EPSILON);
    }

    #[test]
    fn window_is_bounded() {
        let (s, _guard) = fresh();
        for _ in 0..(WINDOW + 500) {
            s.record_request(5, true);
        }
        let snap = s.snapshot();
        assert_eq!(snap.window_len, WINDOW);
        assert_eq!(snap.requests_total, (WINDOW + 500) as u64);
    }

    #[test]
    fn index_write_resets_lag() {
        let (s, _guard) = fresh();
        assert!(s.snapshot().index_lag_seconds.is_none());
        s.record_index_write();
        let lag = s.snapshot().index_lag_seconds.expect("lag after write");
        assert!(
            lag < 5.0,
            "fresh write must report near-zero lag, got {lag}"
        );
    }

    #[test]
    fn prometheus_exposition_contains_all_series() {
        let (s, _guard) = fresh();
        s.record_request(42, true);
        s.record_index_write();
        s.mark_started();
        let text = s.snapshot().to_prometheus();
        for series in [
            "leanctx_team_request_duration_p95_ms",
            "leanctx_team_availability_pct",
            "leanctx_team_index_lag_seconds",
            "leanctx_team_uptime_seconds",
            "leanctx_team_requests_total",
            "leanctx_team_errors_total",
        ] {
            assert!(text.contains(series), "missing series {series}:\n{text}");
        }
        assert!(text.contains("# TYPE leanctx_team_requests_total counter"));
    }
}
