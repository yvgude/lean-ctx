// SPDX-License-Identifier: Apache-2.0
//! Cockpit metrics for Hive (`leanctx.cloud-metrics/v1` payload): per Hive
//! project and UTC day, the totals the local Cockpit computes. Numbers only;
//! content never leaves. [`upload`] decides when they are sent and whether
//! the engine, the lease and the organization allow it.

pub(crate) mod collect;
pub mod hive_use;
#[cfg(test)]
mod k2_review_tests;
pub(crate) mod local_state;
pub(crate) mod payload;
pub mod upload;

use std::path::Path;

/// Days a regular upload covers: today and the previous two.
pub(crate) const RECENT_DAYS: u32 = 3;
/// Days a backfill covers.
pub(crate) const BACKFILL_DAYS: u32 = 90;
/// Minimum seconds between two regular uploads.
const DEBOUNCE_SECS: i64 = 3_600;

/// Whether a regular upload is due: one per hour after the last completed
/// run. A clock that moved backwards does not block uploads.
pub(crate) fn due(last_run: Option<i64>, now: i64, force: bool) -> bool {
    force || last_run.is_none_or(|last| now < last || now - last >= DEBOUNCE_SECS)
}

/// This device as the contract describes it, or `None` on an OS or version
/// the contract does not accept.
fn device(id: String) -> Option<payload::Device> {
    let os = payload::os_name(std::env::consts::OS)?;
    let engine_version = env!("CARGO_PKG_VERSION");
    payload::valid_engine_version(engine_version).then(|| payload::Device {
        id,
        // K2 review (C1): never the machine name.
        label: None,
        engine_version: engine_version.to_string(),
        os: os.to_string(),
    })
}

/// The validated uploads for the `days` ending `now`, with the project ids
/// `ids` names (a root it names none for is left out) and project labels
/// only when `labels` is set. `state` is the directory holding this
/// device's metrics state (device id).
pub(crate) fn build_uploads(
    state: &Path,
    ids: &collect::ProjectIds<'_>,
    days: u32,
    labels: bool,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<Vec<payload::Upload>, String> {
    let id = local_state::device_id_at(state).map_err(|e| e.to_string())?;
    let device = device(id).ok_or_else(|| "unsupported device".to_string())?;
    let data = crate::core::data_dir::lean_ctx_data_dir()?;
    let home = dirs::home_dir();
    let today = now.date_naive();
    let entries = collect::collect(
        &collect::Sources {
            data: &data,
            home: home.as_deref(),
        },
        collect::Window::ending(today, days),
        ids,
        labels,
    );
    let uploads = payload::chunk(&device, entries);
    for upload in &uploads {
        payload::validate(upload, today)
            .map_err(|rule| format!("upload withheld, contract rule {rule} not met"))?;
    }
    Ok(uploads)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uploads_are_debounced_to_one_an_hour() {
        let now = 1_800_000_000;
        assert!(due(None, now, false));
        assert!(!due(Some(now - 60), now, false));
        assert!(!due(Some(now - DEBOUNCE_SECS + 1), now, false));
        assert!(due(Some(now - DEBOUNCE_SECS), now, false));
        assert!(due(Some(now - 60), now, true));
        // A clock set back must not block uploads until it catches up.
        assert!(due(Some(now + 86_400), now, false));
    }

    #[test]
    fn regular_and_backfill_windows_cover_their_days() {
        let today = chrono::NaiveDate::from_ymd_opt(2026, 10, 20).unwrap();
        let recent = collect::Window::ending(today, RECENT_DAYS);
        assert_eq!(
            recent.from,
            chrono::NaiveDate::from_ymd_opt(2026, 10, 18).unwrap()
        );
        assert_eq!(recent.to, today);
        assert_eq!(recent.len_days(), 3);
        assert_eq!(collect::Window::ending(today, BACKFILL_DAYS).len_days(), 90);
    }
}
