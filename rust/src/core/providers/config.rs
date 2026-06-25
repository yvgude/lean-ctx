use std::env;

#[derive(Debug, Clone)]
pub struct GitLabConfig {
    pub host: String,
    pub token: String,
    pub project_path: Option<String>,
}

impl GitLabConfig {
    pub fn from_env() -> Result<Self, String> {
        let token = env::var("LEAN_CTX_GITLAB_TOKEN")
            .or_else(|_| env::var("GITLAB_TOKEN"))
            .or_else(|_| env::var("CI_JOB_TOKEN"))
            .map_err(|_| {
                "No GitLab token found. Set GITLAB_TOKEN or LEAN_CTX_GITLAB_TOKEN.".to_string()
            })?;

        let host = env::var("GITLAB_HOST")
            .or_else(|_| env::var("CI_SERVER_HOST"))
            .unwrap_or_else(|_| "gitlab.com".to_string());

        let project_path = env::var("CI_PROJECT_PATH")
            .ok()
            .or_else(|| detect_project_from_git_remote(&host));

        Ok(Self {
            host,
            token,
            project_path,
        })
    }

    #[must_use]
    pub fn api_url(&self, endpoint: &str) -> String {
        format!("https://{}/api/v4{}", self.host, endpoint)
    }
}

fn detect_project_from_git_remote(host: &str) -> Option<String> {
    let output = std::process::Command::new("git")
        .args(["remote", "get-url", "origin"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let url = String::from_utf8_lossy(&output.stdout).trim().to_string();

    // Handle SSH: git@gitlab.com:group/project.git
    if let Some(rest) = url.strip_prefix(&format!("git@{host}:")) {
        return Some(rest.trim_end_matches(".git").to_string());
    }
    // Handle HTTPS: https://gitlab.com/group/project.git
    if let Some(rest) = url.strip_prefix(&format!("https://{host}/")) {
        return Some(rest.trim_end_matches(".git").to_string());
    }
    None
}
