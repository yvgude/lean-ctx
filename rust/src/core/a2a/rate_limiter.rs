use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

struct RateBucket {
    tokens: f64,
    max_tokens: f64,
    refill_rate: f64,
    last_refill: Instant,
}

impl RateBucket {
    fn new(max_per_minute: u32) -> Self {
        let max = max_per_minute as f64;
        Self {
            tokens: max,
            max_tokens: max,
            refill_rate: max / 60.0,
            last_refill: Instant::now(),
        }
    }

    fn try_consume(&mut self) -> RateLimitResult {
        self.refill();

        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            RateLimitResult::Allowed
        } else {
            let wait_secs = (1.0 - self.tokens) / self.refill_rate;
            RateLimitResult::Limited {
                retry_after_ms: (wait_secs * 1000.0) as u64,
            }
        }
    }

    fn refill(&mut self) {
        let elapsed = self.last_refill.elapsed().as_secs_f64();
        self.tokens = (self.tokens + elapsed * self.refill_rate).min(self.max_tokens);
        self.last_refill = Instant::now();
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum RateLimitResult {
    Allowed,
    Limited { retry_after_ms: u64 },
}

pub struct RateLimiter {
    agent_buckets: HashMap<String, RateBucket>,
    tool_buckets: HashMap<String, RateBucket>,
    global_bucket: RateBucket,
    agent_limit: u32,
    tool_limit: u32,
}

impl RateLimiter {
    pub fn new(global_per_min: u32, agent_per_min: u32, tool_per_min: u32) -> Self {
        Self {
            agent_buckets: HashMap::new(),
            tool_buckets: HashMap::new(),
            global_bucket: RateBucket::new(global_per_min),
            agent_limit: agent_per_min,
            tool_limit: tool_per_min,
        }
    }

    pub fn check(&mut self, agent_id: &str, tool_name: &str) -> RateLimitResult {
        let global = self.global_bucket.try_consume();
        if let RateLimitResult::Limited { .. } = global {
            return global;
        }

        let agent_bucket = self
            .agent_buckets
            .entry(agent_id.to_string())
            .or_insert_with(|| RateBucket::new(self.agent_limit));
        let agent = agent_bucket.try_consume();
        if let RateLimitResult::Limited { .. } = agent {
            return agent;
        }

        let tool_bucket = self
            .tool_buckets
            .entry(tool_name.to_string())
            .or_insert_with(|| RateBucket::new(self.tool_limit));
        tool_bucket.try_consume()
    }

    pub fn cleanup_stale(&mut self, max_idle: Duration) {
        let now = Instant::now();
        self.agent_buckets
            .retain(|_, b| now.duration_since(b.last_refill) < max_idle);
        self.tool_buckets
            .retain(|_, b| now.duration_since(b.last_refill) < max_idle);
    }
}

static GLOBAL_LIMITER: Mutex<Option<RateLimiter>> = Mutex::new(None);

pub fn global_rate_limiter() -> std::sync::MutexGuard<'static, Option<RateLimiter>> {
    GLOBAL_LIMITER
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

pub fn init_rate_limiter(global_per_min: u32, agent_per_min: u32, tool_per_min: u32) {
    let mut guard = global_rate_limiter();
    *guard = Some(RateLimiter::new(
        global_per_min,
        agent_per_min,
        tool_per_min,
    ));
}

pub fn check_rate_limit(agent_id: &str, tool_name: &str) -> RateLimitResult {
    let mut guard = global_rate_limiter();
    match guard.as_mut() {
        Some(limiter) => limiter.check(agent_id, tool_name),
        None => RateLimitResult::Allowed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allows_within_limit() {
        let mut limiter = RateLimiter::new(60, 30, 30);
        for _ in 0..10 {
            assert_eq!(
                limiter.check("agent-1", "ctx_read"),
                RateLimitResult::Allowed
            );
        }
    }

    #[test]
    fn limits_when_exhausted() {
        let mut limiter = RateLimiter::new(5, 3, 100);

        for _ in 0..3 {
            assert_eq!(
                limiter.check("agent-1", "ctx_read"),
                RateLimitResult::Allowed
            );
        }

        match limiter.check("agent-1", "ctx_read") {
            RateLimitResult::Limited { retry_after_ms } => {
                assert!(retry_after_ms > 0);
            }
            RateLimitResult::Allowed => panic!("expected rate limit"),
        }
    }

    #[test]
    fn independent_agent_limits() {
        let mut limiter = RateLimiter::new(100, 2, 100);

        assert_eq!(limiter.check("a", "t"), RateLimitResult::Allowed);
        assert_eq!(limiter.check("a", "t"), RateLimitResult::Allowed);

        match limiter.check("a", "t") {
            RateLimitResult::Limited { .. } => {}
            RateLimitResult::Allowed => panic!("agent-a should be limited"),
        }

        assert_eq!(limiter.check("b", "t"), RateLimitResult::Allowed);
    }

    #[test]
    fn cleanup_removes_stale() {
        let mut limiter = RateLimiter::new(60, 30, 30);
        limiter.check("agent-old", "tool-old");
        assert!(!limiter.agent_buckets.is_empty());

        limiter.cleanup_stale(Duration::from_secs(0));
        assert!(limiter.agent_buckets.is_empty());
    }
}
