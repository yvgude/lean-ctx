//! Opt-in runtime buffer for Shadow Mode reports.
use super::{ShadowEngine, ShadowReport, ShadowTask};
use crate::core::config::{Config, ShadowConfig};
use std::{
    panic::{AssertUnwindSafe, catch_unwind},
    sync::{Mutex, OnceLock},
};

pub use super::persistence::{list_reports, load_report, persist_report};

#[derive(Default)]
pub(crate) struct State {
    tasks: Vec<ShadowTask>,
    latest: Option<ShadowReport>,
}
pub(crate) static STATE: OnceLock<Mutex<State>> = OnceLock::new();
pub(crate) fn state() -> &'static Mutex<State> {
    STATE.get_or_init(|| Mutex::new(State::default()))
}

/// Process-wide Shadow Mode coordinator.
pub struct ShadowRuntime;
impl ShadowRuntime {
    pub fn is_enabled() -> bool {
        catch_unwind(AssertUnwindSafe(|| load_shadow_config().enabled)).unwrap_or(false)
    }
    pub fn on_task_complete(task: &ShadowTask) {
        let _ = catch_unwind(AssertUnwindSafe(|| Self::on_task_complete_inner(task)));
    }
    pub fn get_latest_report() -> Option<ShadowReport> {
        catch_unwind(AssertUnwindSafe(|| {
            state()
                .lock()
                .ok()
                .and_then(|state| state.latest.clone())
                .or_else(|| list_reports().last().and_then(|path| load_report(path)))
        }))
        .unwrap_or(None)
    }
    pub fn force_report() -> Option<ShadowReport> {
        catch_unwind(AssertUnwindSafe(Self::report_pending)).unwrap_or(None)
    }

    fn on_task_complete_inner(task: &ShadowTask) {
        if !Self::is_enabled() {
            return;
        }
        let interval = load_shadow_config().report_interval.max(1);
        let ready = state().lock().ok().and_then(|mut state| {
            state.tasks.push(task.clone());
            (state.tasks.len() >= interval).then(|| std::mem::take(&mut state.tasks))
        });
        if let Some(tasks) = ready {
            let _ = Self::generate_report(&tasks);
        }
    }

    fn report_pending() -> Option<ShadowReport> {
        let tasks = state()
            .lock()
            .ok()
            .map(|mut state| std::mem::take(&mut state.tasks))?;
        Self::generate_report(&tasks)
    }

    fn generate_report(tasks: &[ShadowTask]) -> Option<ShadowReport> {
        if tasks.is_empty() {
            return None;
        }
        // `[shadow] baseline_model` is deliberately not used: each task keeps its host model.
        let report = ShadowEngine::run_comparison(tasks);
        let _ = persist_report(&report);
        if let Ok(mut state) = state().lock() {
            state.latest = Some(report.clone());
        }
        Some(report)
    }
}
pub fn load_shadow_config() -> ShadowConfig {
    catch_unwind(AssertUnwindSafe(|| Config::load().shadow)).unwrap_or_default()
}
