//! Local, contextual Pro conversion nudges.

use chrono::{DateTime, Duration, Utc};
use std::{
    collections::BTreeSet,
    sync::{Mutex, OnceLock},
};

#[rustfmt::skip]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct UsageSignals { pub session_count: u32, pub total_decisions: u32, pub context_span_sessions: u32, pub unique_devices: u32, pub total_savings_usd: f64 }

#[rustfmt::skip]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TriggerKind { CrossDeviceSync, AdaptiveModelRouting, PersonalKnowledgeGraph, EncryptedBackup, SavingsInsight }

#[rustfmt::skip]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProNudge { pub kind: TriggerKind, pub message: String, pub priority: u8, pub session_threshold: u32 }

#[rustfmt::skip]
#[derive(Clone, Copy, Debug)]
pub(crate) struct TriggerConfig { context_sessions: u32, decisions: u32, savings_usd: f64, single_device_sessions: u32 }

#[rustfmt::skip]
impl Default for TriggerConfig {
    fn default() -> Self { Self { context_sessions: 5, decisions: 30, savings_usd: 20.0, single_device_sessions: 20 } }
}

#[rustfmt::skip]
#[derive(Clone, Debug)]
pub struct ProTriggerEngine { signals: UsageSignals, config: TriggerConfig, suppressed_until: Option<DateTime<Utc>> }

#[rustfmt::skip]
impl ProTriggerEngine {
    #[must_use]
    pub fn new(signals: UsageSignals) -> Self { Self { signals, config: TriggerConfig::default(), suppressed_until: None } }

    pub fn update_signals(&mut self, signals: UsageSignals) { self.signals = signals; }

    /// Suppress this engine's nudges for the requested whole-day period.
    pub fn suppress(&mut self, days: u32) { self.suppressed_until = Some(Utc::now() + Duration::days(i64::from(days))); }

    #[must_use]
    pub fn evaluate(&self) -> Vec<ProNudge> {
        if self.suppressed_until.is_some_and(|until| Utc::now() < until) { return Vec::new(); }
        let s = self.signals;
        let c = self.config;
        let mut nudges = Vec::new();
        if s.session_count >= c.context_sessions && s.context_span_sessions > 1 {
            nudges.push(nudge(TriggerKind::CrossDeviceSync, format!("Your context now spans {} sessions. Pro syncs this across all your machines.", s.context_span_sessions), 9, c.context_sessions));
        }
        if s.session_count >= 10 && s.total_decisions > c.decisions {
            nudges.push(nudge(TriggerKind::AdaptiveModelRouting, format!("You've made {} decisions this week. Pro learns which models work best for YOUR tasks.", s.total_decisions), 8, 10));
        }
        if s.session_count >= 15 && s.total_savings_usd > c.savings_usd {
            nudges.push(nudge(TriggerKind::SavingsInsight, format!("You saved ${:.2} this month. Pro shows your full economics dashboard.", s.total_savings_usd), 7, 15));
        }
        if s.session_count >= c.single_device_sessions && s.unique_devices == 1 {
            nudges.push(nudge(TriggerKind::CrossDeviceSync, "Working on multiple machines? Pro keeps your memory in sync everywhere.".into(), 6, c.single_device_sessions));
        }
        nudges
    }
}

#[rustfmt::skip]
fn nudge(kind: TriggerKind, message: String, priority: u8, session_threshold: u32) -> ProNudge { ProNudge { kind, message, priority, session_threshold } }

fn global_suppression() -> &'static Mutex<Option<DateTime<Utc>>> {
    static SUPPRESSION: OnceLock<Mutex<Option<DateTime<Utc>>>> = OnceLock::new();
    SUPPRESSION.get_or_init(|| Mutex::new(None))
}

/// Suppress process-wide free-tier nudges for `days` days.
#[rustfmt::skip]
pub fn suppress(days: u32) {
    *global_suppression().lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Utc::now() + Duration::days(i64::from(days)));
}

/// Re-enable process-wide nudges after a user changes their mind.
#[rustfmt::skip]
pub fn clear_suppression() { *global_suppression().lock().unwrap_or_else(std::sync::PoisonError::into_inner) = None; }

/// Evaluate trigger conditions using the default configuration.
#[must_use]
#[rustfmt::skip]
pub fn evaluate_triggers(signals: &UsageSignals) -> Vec<ProNudge> {
    let mut engine = ProTriggerEngine::new(*signals);
    engine.suppressed_until = *global_suppression().lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    engine.evaluate()
}

/// Gather local usage facts for the session UI and server metrics paths.
#[rustfmt::skip]
pub(crate) fn local_usage_signals(current: &crate::core::session::SessionState) -> UsageSignals {
    let sessions = crate::core::session::SessionState::list_sessions();
    let saved = sessions.iter().any(|summary| summary.id == current.id);
    let mut devices = current.evidence.iter().filter_map(|record| record.agent_id.clone()).collect::<BTreeSet<_>>();
    for summary in &sessions {
        if let Some(session) = crate::core::session::SessionState::load_by_id(&summary.id) {
            devices.extend(session.evidence.into_iter().filter_map(|record| record.agent_id));
        }
    }
    let session_count = u32::try_from(sessions.len() + usize::from(!saved)).unwrap_or(u32::MAX);
    UsageSignals { session_count, total_decisions: u32::try_from(crate::core::session::SessionState::decision_count_this_week()).unwrap_or(u32::MAX), context_span_sessions: session_count, unique_devices: u32::try_from(devices.len()).unwrap_or(u32::MAX), total_savings_usd: if crate::core::savings_ledger::verify().valid { crate::core::savings_ledger::summary().saved_usd } else { 0.0 } }
}

#[cfg(test)]
#[rustfmt::skip]
mod tests {
    use super::*;
    fn base() -> UsageSignals { UsageSignals { unique_devices: 2, ..UsageSignals::default() } }
    #[test] fn below_thresholds_is_quiet() { assert!(evaluate_triggers(&base()).is_empty()); }
    #[test] fn context_at_five_uses_span() { let mut s = base(); s.session_count = 5; s.context_span_sessions = 3; assert_eq!(evaluate_triggers(&s)[0].message, "Your context now spans 3 sessions. Pro syncs this across all your machines."); }
    #[test] fn context_needs_multiple_sessions() { let mut s = base(); s.session_count = 5; s.context_span_sessions = 1; assert!(evaluate_triggers(&s).is_empty()); }
    #[test] fn decisions_over_thirty_trigger_at_ten() { let mut s = base(); s.session_count = 10; s.total_decisions = 31; assert_eq!(evaluate_triggers(&s)[0].kind, TriggerKind::AdaptiveModelRouting); }
    #[test] fn savings_over_twenty_trigger_at_fifteen() { let mut s = base(); s.session_count = 15; s.total_savings_usd = 20.01; assert!(evaluate_triggers(&s)[0].message.contains("$20.01 this month")); }
    #[test] fn one_device_triggers_at_twenty() { let s = UsageSignals { session_count: 20, unique_devices: 1, ..UsageSignals::default() }; assert_eq!(evaluate_triggers(&s)[0].priority, 6); }
    #[test] fn engine_suppression_hides_nudges() { let mut engine = ProTriggerEngine::new(UsageSignals { session_count: 20, context_span_sessions: 2, unique_devices: 1, ..UsageSignals::default() }); engine.suppress(1); assert!(engine.evaluate().is_empty()); }
}
