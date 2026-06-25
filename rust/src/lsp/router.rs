use lsp_types::Uri;
use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;

use super::backend::LspBackend;
use super::client::{LspClient, file_path_to_uri};
use super::config::{
    LspServerConfig, check_server_available, default_servers, language_for_extension,
};
use super::jetbrains_backend::JetBrainsHttpBackend;
use super::port_discovery;

static BACKENDS: std::sync::LazyLock<Mutex<HashMap<String, Box<dyn LspBackend>>>> =
    std::sync::LazyLock::new(|| Mutex::new(HashMap::new()));

fn expand_tilde(path: &str) -> String {
    if let Some(rest) = path.strip_prefix("~/")
        && let Some(home) = dirs::home_dir()
    {
        return format!("{}/{rest}", home.display());
    }
    path.to_string()
}

fn resolve_config_for_language(language: &str) -> LspServerConfig {
    let cfg = crate::core::config::Config::load();
    if let Some(custom_path) = cfg.lsp.get(language) {
        let expanded = expand_tilde(custom_path);
        return LspServerConfig {
            command: expanded,
            args: if language == "typescript" || language == "javascript" {
                vec!["--stdio".into()]
            } else if language == "go" {
                vec!["serve".into()]
            } else {
                vec![]
            },
        };
    }
    let servers = default_servers();
    servers.get(language).cloned().unwrap_or(LspServerConfig {
        command: format!("{language}-language-server"),
        args: vec![],
    })
}

/// Selects a code-intelligence backend for `language` (§4.3).
///
/// Config `cfg.lsp[language]` (`HashMap`<String,String>):
///   - absent      → "auto" = B-first (`JetBrains` if reachable, else rust-analyzer)
///   - "auto"      → same as absent
///   - "jetbrains" → B only (error if the IDE is not reachable; no fallback)
///   - anything else → treated as an explicit rust-analyzer binary path = A only
///
/// Reachability = live port file + pid alive + `/health` ping. On any miss in
/// "auto" mode we fall back to Backing A deterministically (one ~300ms timeout max).
fn select_backend(language: &str, project_root: &str) -> Result<Box<dyn LspBackend>, String> {
    let cfg = crate::core::config::Config::load();
    let mode = cfg.lsp.get(language).map(String::as_str);

    let want_b = matches!(mode, None | Some("auto" | "jetbrains"));
    let b_only = mode == Some("jetbrains");

    if want_b {
        if let Some(pf) = port_discovery::read_port_file(project_root)
            && port_discovery::pid_alive(pf.pid)
            && port_discovery::health_ok(&pf)
        {
            return Ok(Box::new(JetBrainsHttpBackend::new(
                pf.port,
                pf.token,
                project_root.to_string(),
                pf.pid,
            )));
        }
        if b_only {
            return Err(format!(
                "LSP backend 'jetbrains' configured for '{language}' but the IDE is not reachable \
                 (no live port file / health check failed)"
            ));
        }
    }

    // Backing A: rust-analyzer (today's behavior).
    let config = resolve_config_for_language(language);
    if super::config::find_binary_in_path(&config.command).is_none()
        && !Path::new(&config.command).is_file()
    {
        check_server_available(language)?;
    }
    let root_uri = file_path_to_uri(project_root)?;
    let client = LspClient::start(&config, &root_uri)?;
    Ok(Box::new(client) as Box<dyn LspBackend>)
}

/// Evicts a cached backend whose liveness check (`is_stale`) failed, so the next
/// lookup re-selects (auto → Backing A fallback; `b_only` → Err). Backing A never stale.
fn evict_if_stale(
    backends: &mut HashMap<String, Box<dyn LspBackend>>,
    language: &str,
    project_root: &str,
) {
    if backends
        .get(language)
        .is_some_and(|b| b.is_stale(project_root))
    {
        backends.remove(language);
    }
}

pub fn with_backend<F, R>(file_path: &str, project_root: &str, f: F) -> Result<R, String>
where
    F: FnOnce(&mut dyn LspBackend, &str) -> Result<R, String>,
{
    let ext = Path::new(file_path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("");

    let language = language_for_extension(ext).ok_or_else(|| {
        format!(
            "No LSP server configured for extension '.{ext}'. Supported: rs, ts, tsx, js, py, go"
        )
    })?;

    let mut backends = BACKENDS.lock().map_err(|e| e.to_string())?;

    // Drop a cached entry whose IDE went away / restarted before reusing it.
    evict_if_stale(&mut backends, language, project_root);

    if !backends.contains_key(language) {
        let backend = select_backend(language, project_root)?;
        backends.insert(language.to_string(), backend);
    }

    let backend = backends
        .get_mut(language)
        .ok_or_else(|| format!("LSP backend for '{language}' not available"))?;

    f(backend.as_mut(), language)
}

pub fn open_file(file_path: &str, project_root: &str) -> Result<Uri, String> {
    let ext = Path::new(file_path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("");
    language_for_extension(ext).ok_or_else(|| {
        format!(
            "No LSP server configured for extension '.{ext}'. Supported: rs, ts, tsx, js, py, go"
        )
    })?;

    let content = std::fs::read_to_string(file_path)
        .map_err(|e| format!("Cannot read '{file_path}': {e}"))?;

    let uri = file_path_to_uri(file_path)?;

    with_backend(file_path, project_root, |backend, language| {
        backend.open_file(&uri, language, &content)?;
        Ok(uri.clone())
    })
}

pub fn shutdown_all() {
    if let Ok(mut backends) = BACKENDS.lock() {
        for (_, backend) in backends.drain() {
            drop(backend);
        }
    }
}

#[cfg(test)]
pub(crate) fn seed_stub_backend(language: &str, backend: Box<dyn LspBackend>) {
    if let Ok(mut backends) = BACKENDS.lock() {
        backends.insert(language.to_string(), backend);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_port_file_means_no_backing_b() {
        // With no IDE port file for an unlikely root, discovery yields None →
        // select_backend would deterministically fall through to Backing A.
        let pf = port_discovery::read_port_file("/nonexistent/leanctx/proj/xyz");
        assert!(pf.is_none(), "unexpected port file for nonexistent root");
    }

    struct StaleStub(bool);
    impl LspBackend for StaleStub {
        fn open_file(&mut self, _u: &Uri, _l: &str, _t: &str) -> Result<(), String> {
            Ok(())
        }
        fn references(
            &mut self,
            _u: &Uri,
            _p: lsp_types::Position,
            _s: &str,
        ) -> Result<Vec<lsp_types::Location>, String> {
            Ok(vec![])
        }
        fn definition(
            &mut self,
            _u: &Uri,
            _p: lsp_types::Position,
        ) -> Result<lsp_types::GotoDefinitionResponse, String> {
            Ok(lsp_types::GotoDefinitionResponse::Array(vec![]))
        }
        fn implementations(
            &mut self,
            _u: &Uri,
            _p: lsp_types::Position,
            _s: &str,
        ) -> Result<Vec<lsp_types::Location>, String> {
            Ok(vec![])
        }
        fn rename(
            &mut self,
            _u: &Uri,
            _p: lsp_types::Position,
            _n: &str,
        ) -> Result<Option<lsp_types::WorkspaceEdit>, String> {
            Ok(None)
        }
        fn is_stale(&self, _project_root: &str) -> bool {
            self.0
        }
    }

    #[test]
    fn evict_if_stale_removes_stale_keeps_fresh() {
        let mut map: HashMap<String, Box<dyn LspBackend>> = HashMap::new();
        map.insert("stale".to_string(), Box::new(StaleStub(true)));
        map.insert("fresh".to_string(), Box::new(StaleStub(false)));
        evict_if_stale(&mut map, "stale", "/any");
        evict_if_stale(&mut map, "fresh", "/any");
        assert!(!map.contains_key("stale"), "stale entry must be evicted");
        assert!(map.contains_key("fresh"), "fresh entry must remain");
    }
}
