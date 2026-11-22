//! JSON persistence and retention for Shadow Mode reports.
use super::{ShadowReport, runtime::load_shadow_config};
use lean_ctx_time::UtcDateTime;
use std::path::{Path, PathBuf};

pub fn persist_report(report: &ShadowReport) -> std::io::Result<PathBuf> {
    let dir = report_dir();
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!(
        "shadow_report_{}.json",
        UtcDateTime::now().format("%Y%m%d_%H%M%S")
    ));
    std::fs::write(
        &path,
        serde_json::to_vec_pretty(report).map_err(std::io::Error::other)?,
    )?;
    for old in list_reports_in(&dir).into_iter().rev().skip(50) {
        std::fs::remove_file(old)?;
    }
    Ok(path)
}
pub fn list_reports() -> Vec<PathBuf> {
    list_reports_in(&report_dir())
}
pub fn load_report(path: &Path) -> Option<ShadowReport> {
    std::fs::read(path)
        .ok()
        .and_then(|body| serde_json::from_slice(&body).ok())
}
fn report_dir() -> PathBuf {
    let configured = load_shadow_config().report_dir;
    if configured.trim().is_empty() {
        crate::core::paths::state_dir()
            .map(|d| d.join("shadow_reports"))
            .unwrap_or_else(|_| PathBuf::from("shadow_reports"))
    } else {
        PathBuf::from(configured)
    }
}
fn list_reports_in(dir: &Path) -> Vec<PathBuf> {
    let mut paths = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "json"))
        .collect::<Vec<_>>();
    paths.sort();
    paths
}
