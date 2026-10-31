//! Feeds observed context-read bounces into the kernel's adaptive bridge.

/// Updates the adaptive bridge from the process-wide bounce tracker.
pub fn update_from_bounce_tracker() {
    if !super::kernel_config::is_enabled() {
        return;
    }
    let tracker = crate::core::bounce_tracker::global()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let total_bounces = tracker.total_bounces();
    let denominator = total_bounces.saturating_add(100).max(1);
    let rate = total_bounces as f64 / denominator as f64;
    drop(tracker);
    super::adaptive_bridge::update_bounce_signal(rate);
}

#[cfg(test)]
pub mod tests {
    use super::update_from_bounce_tracker;
    use crate::core::context_kernel::{adaptive_bridge, kernel_config};

    fn setup() -> std::sync::MutexGuard<'static, ()> {
        let guard = kernel_config::KERNEL_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        kernel_config::reset_features();
        adaptive_bridge::reset();
        let mut tracker = crate::core::bounce_tracker::global()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *tracker = crate::core::bounce_tracker::BounceTracker::new();
        drop(tracker);
        guard
    }

    #[test]
    fn update_feeds_bridge() {
        let _guard = setup();
        crate::core::bounce_tracker::global()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .record_expansion(None, 20);
        update_from_bounce_tracker();
        assert!(adaptive_bridge::current_bounce_rate() > 0.0);
    }

    #[test]
    fn disabled_kernel_noop() {
        let _guard = setup();
        let mut features = kernel_config::features();
        features.enabled = false;
        kernel_config::update_features(features);
        crate::core::bounce_tracker::global()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .record_expansion(None, 20);
        update_from_bounce_tracker();
        assert_eq!(adaptive_bridge::current_bounce_rate(), 0.0);
    }
}
