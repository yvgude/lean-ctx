use std::cell::RefCell;
use std::sync::atomic::{AtomicUsize, Ordering};

static SESSION_ORIGINAL: AtomicUsize = AtomicUsize::new(0);
static SESSION_SAVED: AtomicUsize = AtomicUsize::new(0);
static SESSION_CALL_COUNT: AtomicUsize = AtomicUsize::new(0);

const SESSION_TOTAL_INTERVAL: usize = 10;

thread_local! {
    static CURRENT_MODE: RefCell<Option<String>> = const { RefCell::new(None) };
    static CURRENT_DETAIL: RefCell<Option<String>> = const { RefCell::new(None) };
}

pub struct SavingsInfo<'a> {
    pub original: usize,
    pub compressed: usize,
    pub mode: Option<&'a str>,
    pub detail: Option<&'a str>,
}

pub struct ModeGuard;

impl ModeGuard {
    pub fn new(mode: &str) -> Self {
        CURRENT_MODE.with(|m| *m.borrow_mut() = Some(mode.to_string()));
        Self
    }

    pub fn with_detail(mode: &str, detail: &str) -> Self {
        CURRENT_MODE.with(|m| *m.borrow_mut() = Some(mode.to_string()));
        CURRENT_DETAIL.with(|d| *d.borrow_mut() = Some(detail.to_string()));
        Self
    }
}

impl Drop for ModeGuard {
    fn drop(&mut self) {
        // Must be panic-free: a `borrow_mut` panic while the thread is already
        // unwinding another panic would escalate to a process abort (#378). Use
        // `try_borrow_mut` and silently skip if the slot is somehow in use.
        CURRENT_MODE.with(|m| {
            if let Ok(mut slot) = m.try_borrow_mut() {
                *slot = None;
            }
        });
        CURRENT_DETAIL.with(|d| {
            if let Ok(mut slot) = d.try_borrow_mut() {
                *slot = None;
            }
        });
    }
}

fn current_mode() -> Option<String> {
    CURRENT_MODE.with(|m| m.borrow().clone())
}

fn current_detail() -> Option<String> {
    CURRENT_DETAIL.with(|d| d.borrow().clone())
}

pub fn record_savings(original: usize, saved: usize) {
    SESSION_ORIGINAL.fetch_add(original, Ordering::Relaxed);
    SESSION_SAVED.fetch_add(saved, Ordering::Relaxed);
    SESSION_CALL_COUNT.fetch_add(1, Ordering::Relaxed);
}

pub fn session_totals() -> (usize, usize, usize) {
    (
        SESSION_ORIGINAL.load(Ordering::Relaxed),
        SESSION_SAVED.load(Ordering::Relaxed),
        SESSION_CALL_COUNT.load(Ordering::Relaxed),
    )
}

pub fn reset_session() {
    SESSION_ORIGINAL.store(0, Ordering::Relaxed);
    SESSION_SAVED.store(0, Ordering::Relaxed);
    SESSION_CALL_COUNT.store(0, Ordering::Relaxed);
}

fn format_number(n: usize) -> String {
    if n >= 1_000_000 {
        let m = n as f64 / 1_000_000.0;
        format!("{m:.1}M")
    } else if n >= 10_000 {
        let k = n as f64 / 1_000.0;
        format!("{k:.1}k")
    } else if n >= 1_000 {
        let whole = n / 1_000;
        format!("{whole},{:03}", n % 1_000)
    } else {
        n.to_string()
    }
}

fn is_explicitly_enabled() -> bool {
    matches!(std::env::var("LEAN_CTX_SHOW_SAVINGS"), Ok(v) if v.trim() == "1")
}

fn is_ultra_suppressed() -> bool {
    if is_explicitly_enabled() {
        return false;
    }
    let level = super::config::CompressionLevel::effective(&super::config::Config::load());
    matches!(level, super::config::CompressionLevel::Max)
}

pub fn format_footer(info: &SavingsInfo<'_>) -> String {
    if !super::protocol::savings_footer_visible() {
        return String::new();
    }
    if is_ultra_suppressed() {
        return String::new();
    }
    format_footer_inner(info)
}

fn format_footer_inner(info: &SavingsInfo<'_>) -> String {
    if info.original == 0 {
        return String::new();
    }
    let saved = info.original.saturating_sub(info.compressed);
    if saved == 0 {
        return String::new();
    }
    let pct = (saved as f64 / info.original as f64 * 100.0).round() as usize;

    let orig_str = format_number(info.original);
    let comp_str = format_number(info.compressed);

    let mut parts = vec![format!(
        "{orig_str} \u{2192} {comp_str} tok (\u{2193}{pct}%)"
    )];

    if let Some(mode) = info.mode {
        parts.push(format!("mode: {mode}"));
    }
    if let Some(detail) = info.detail {
        parts.push(detail.to_string());
    }

    record_savings(info.original, saved);

    let call_count = SESSION_CALL_COUNT.load(Ordering::Relaxed);
    if call_count > 0 && call_count.is_multiple_of(SESSION_TOTAL_INTERVAL) {
        let (_, total_saved, _) = session_totals();
        let total_str = format_number(total_saved);
        parts.push(format!("session: {total_str} saved"));
    }

    let body = parts.join(" | ");
    format!("\u{2500}\u{2500}\u{2500} {body} \u{2500}\u{2500}\u{2500}")
}

pub fn format_footer_basic(original: usize, compressed: usize) -> String {
    let mode = current_mode();
    let detail = current_detail();
    format_footer(&SavingsInfo {
        original,
        compressed,
        mode: mode.as_deref(),
        detail: detail.as_deref(),
    })
}

pub fn append_footer(output: &str, info: &SavingsInfo<'_>) -> String {
    let footer = format_footer(info);
    if footer.is_empty() {
        output.to_string()
    } else {
        format!("{output}\n{footer}")
    }
}

pub fn append_footer_basic(output: &str, original: usize, compressed: usize) -> String {
    let footer = format_footer_basic(original, compressed);
    if footer.is_empty() {
        output.to_string()
    } else {
        format!("{output}\n{footer}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_number_small() {
        assert_eq!(format_number(42), "42");
        assert_eq!(format_number(999), "999");
    }

    #[test]
    fn format_number_thousands() {
        assert_eq!(format_number(1_000), "1,000");
        assert_eq!(format_number(4_200), "4,200");
        assert_eq!(format_number(9_999), "9,999");
    }

    #[test]
    fn format_number_large() {
        assert_eq!(format_number(12_300), "12.3k");
        assert_eq!(format_number(45_200), "45.2k");
    }

    #[test]
    fn format_number_millions() {
        assert_eq!(format_number(1_500_000), "1.5M");
    }

    #[test]
    fn basic_footer_format() {
        let info = SavingsInfo {
            original: 4200,
            compressed: 840,
            mode: Some("map"),
            detail: None,
        };
        let result = format_footer_inner(&info);
        assert!(
            result.starts_with("\u{2500}\u{2500}\u{2500} "),
            "should start with box-drawing: {result}"
        );
        assert!(
            result.ends_with(" \u{2500}\u{2500}\u{2500}"),
            "should end with box-drawing: {result}"
        );
        assert!(
            result.contains("4,200"),
            "should contain formatted original: {result}"
        );
        assert!(
            result.contains("840"),
            "should contain compressed: {result}"
        );
        assert!(
            result.contains("\u{2193}80%"),
            "should contain percentage: {result}"
        );
        assert!(
            result.contains("mode: map"),
            "should contain mode: {result}"
        );
    }

    #[test]
    fn footer_with_detail() {
        let info = SavingsInfo {
            original: 12300,
            compressed: 620,
            mode: None,
            detail: Some("3 patterns matched"),
        };
        let result = format_footer_inner(&info);
        assert!(
            result.contains("3 patterns matched"),
            "detail missing: {result}"
        );
        assert!(
            result.contains("12.3k"),
            "should format large numbers: {result}"
        );
    }

    #[test]
    fn footer_returns_empty_when_no_savings() {
        let result = format_footer_inner(&SavingsInfo {
            original: 100,
            compressed: 100,
            mode: None,
            detail: None,
        });
        assert!(
            result.is_empty(),
            "should be empty with 0 savings: {result}"
        );
    }

    #[test]
    fn footer_returns_empty_when_zero_original() {
        let result = format_footer_inner(&SavingsInfo {
            original: 0,
            compressed: 0,
            mode: None,
            detail: None,
        });
        assert!(
            result.is_empty(),
            "should be empty with 0 original: {result}"
        );
    }

    #[test]
    fn visibility_gated_tests() {
        let _lock = crate::core::data_dir::test_env_lock();

        crate::test_env::set_var("LEAN_CTX_SHOW_SAVINGS", "0");
        crate::test_env::set_var("LEAN_CTX_SAVINGS_FOOTER", "never");
        let result = format_footer_basic(100, 50);
        assert!(
            result.is_empty(),
            "should be empty with never mode: {result}"
        );

        let result = append_footer_basic("hello", 100, 50);
        assert_eq!(result, "hello");

        crate::test_env::set_var("LEAN_CTX_SHOW_SAVINGS", "1");
        crate::test_env::set_var("LEAN_CTX_SAVINGS_FOOTER", "always");
        crate::test_env::remove_var("LEAN_CTX_QUIET");
        super::super::protocol::set_mcp_context(false);

        let result = append_footer_basic("hello", 100, 50);
        assert!(
            result.starts_with("hello\n"),
            "should start with original: {result}"
        );
        assert!(
            result.contains("\u{2500}\u{2500}\u{2500}"),
            "should contain box-drawing: {result}"
        );

        // Restore ALL touched env — leaking LEAN_CTX_SAVINGS_FOOTER=always
        // made footers visible in unrelated tests (GL #556 flakiness).
        crate::test_env::remove_var("LEAN_CTX_SHOW_SAVINGS");
        crate::test_env::remove_var("LEAN_CTX_SAVINGS_FOOTER");
    }

    #[test]
    fn session_accumulator_tracks() {
        reset_session();
        record_savings(100, 50);
        record_savings(200, 80);
        let (orig, saved, calls) = session_totals();
        assert_eq!(orig, 300);
        assert_eq!(saved, 130);
        assert_eq!(calls, 2);
        reset_session();
    }

    #[test]
    fn session_total_shown_at_interval() {
        reset_session();
        // Record until we reach N-1 (mod N), tolerating parallel test threads
        // that concurrently increment the shared global counter.
        loop {
            let (_, _, count) = session_totals();
            if count > 0 && (count + 1) % SESSION_TOTAL_INTERVAL == 0 {
                break;
            }
            if count + 1 >= SESSION_TOTAL_INTERVAL * 100 {
                // Safety valve — should never trigger.
                break;
            }
            record_savings(100, 50);
        }
        let info = SavingsInfo {
            original: 100,
            compressed: 50,
            mode: None,
            detail: None,
        };
        let result = format_footer_inner(&info);
        assert!(
            result.contains("session:"),
            "should contain session total at interval: {result}"
        );
        reset_session();
    }

    #[test]
    fn mode_guard_sets_and_clears() {
        assert!(current_mode().is_none());
        {
            let _guard = ModeGuard::new("map");
            assert_eq!(current_mode().as_deref(), Some("map"));
        }
        assert!(current_mode().is_none());
    }

    #[test]
    fn mode_guard_with_detail() {
        {
            let _guard = ModeGuard::with_detail("shell", "3 patterns");
            assert_eq!(current_mode().as_deref(), Some("shell"));
            assert_eq!(current_detail().as_deref(), Some("3 patterns"));
        }
        assert!(current_mode().is_none());
        assert!(current_detail().is_none());
    }
}
