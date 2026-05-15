use lsp_types::Uri;
use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;

use super::client::{file_path_to_uri, LspClient};
use super::config::{default_servers, language_for_extension};

static CLIENTS: std::sync::LazyLock<Mutex<HashMap<String, LspClient>>> =
    std::sync::LazyLock::new(|| Mutex::new(HashMap::new()));

pub fn with_client<F, R>(file_path: &str, project_root: &str, f: F) -> Result<R, String>
where
    F: FnOnce(&mut LspClient, &str) -> Result<R, String>,
{
    let ext = Path::new(file_path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("");

    let language = language_for_extension(ext)
        .ok_or_else(|| format!("No LSP server configured for extension '.{ext}'"))?;

    let mut clients = CLIENTS.lock().map_err(|e| e.to_string())?;

    if !clients.contains_key(language) {
        let servers = default_servers();
        let config = servers
            .get(language)
            .ok_or_else(|| format!("No LSP server configured for language '{language}'"))?;

        let root_uri = file_path_to_uri(project_root)?;
        let client = LspClient::start(config, &root_uri)?;
        clients.insert(language.to_string(), client);
    }

    let client = clients
        .get_mut(language)
        .ok_or_else(|| format!("LSP client for '{language}' not available"))?;

    f(client, language)
}

pub fn open_file(file_path: &str, project_root: &str) -> Result<Uri, String> {
    let content = std::fs::read_to_string(file_path)
        .map_err(|e| format!("Cannot read '{file_path}': {e}"))?;

    let uri = file_path_to_uri(file_path)?;

    with_client(file_path, project_root, |client, language| {
        client.did_open(&uri, language, &content)?;
        Ok(uri.clone())
    })
}

pub fn shutdown_all() {
    if let Ok(mut clients) = CLIENTS.lock() {
        for (_, client) in clients.drain() {
            drop(client);
        }
    }
}
