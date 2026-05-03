use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SetupReport {
    pub schema_version: u32,
    pub started_at: DateTime<Utc>,
    pub finished_at: DateTime<Utc>,
    pub success: bool,
    pub platform: PlatformInfo,
    pub steps: Vec<SetupStepReport>,
    #[serde(default)]
    pub warnings: Vec<String>,
    #[serde(default)]
    pub errors: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlatformInfo {
    pub os: String,
    pub arch: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SetupStepReport {
    pub name: String,
    pub ok: bool,
    #[serde(default)]
    pub items: Vec<SetupItem>,
    #[serde(default)]
    pub warnings: Vec<String>,
    #[serde(default)]
    pub errors: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SetupItem {
    pub name: String,
    pub status: String,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub note: Option<String>,
}

impl SetupReport {
    pub fn default_path() -> Result<PathBuf, String> {
        let data_dir = crate::core::data_dir::lean_ctx_data_dir()?;
        Ok(data_dir.join("setup/latest.json"))
    }
}

pub fn doctor_report_path() -> Result<PathBuf, String> {
    Ok(crate::core::data_dir::lean_ctx_data_dir()?.join("doctor/latest.json"))
}

pub fn status_report_path() -> Result<PathBuf, String> {
    Ok(crate::core::data_dir::lean_ctx_data_dir()?.join("status/latest.json"))
}
