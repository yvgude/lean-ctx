use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct LspServerConfig {
    pub command: String,
    pub args: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct LspServerInfo {
    pub language: &'static str,
    pub binary: &'static str,
    pub install_hint: &'static str,
}

pub const KNOWN_SERVERS: &[LspServerInfo] = &[
    LspServerInfo {
        language: "rust",
        binary: "rust-analyzer",
        install_hint: "rustup component add rust-analyzer",
    },
    LspServerInfo {
        language: "typescript",
        binary: "typescript-language-server",
        install_hint: "npm install -g typescript-language-server typescript",
    },
    LspServerInfo {
        language: "python",
        binary: "pylsp",
        install_hint: "pip install python-lsp-server",
    },
    LspServerInfo {
        language: "go",
        binary: "gopls",
        install_hint: "go install golang.org/x/tools/gopls@latest",
    },
];

#[must_use]
pub fn default_servers() -> HashMap<&'static str, LspServerConfig> {
    let mut m = HashMap::new();
    m.insert(
        "rust",
        LspServerConfig {
            command: "rust-analyzer".into(),
            args: vec![],
        },
    );
    m.insert(
        "typescript",
        LspServerConfig {
            command: "typescript-language-server".into(),
            args: vec!["--stdio".into()],
        },
    );
    m.insert(
        "javascript",
        LspServerConfig {
            command: "typescript-language-server".into(),
            args: vec!["--stdio".into()],
        },
    );
    m.insert(
        "python",
        LspServerConfig {
            command: "pylsp".into(),
            args: vec![],
        },
    );
    m.insert(
        "go",
        LspServerConfig {
            command: "gopls".into(),
            args: vec!["serve".into()],
        },
    );
    m
}

#[must_use]
pub fn language_for_extension(ext: &str) -> Option<&'static str> {
    match ext {
        "rs" => Some("rust"),
        "ts" | "tsx" => Some("typescript"),
        "js" | "jsx" | "mjs" | "cjs" => Some("javascript"),
        "py" | "pyi" => Some("python"),
        "go" => Some("go"),
        "java" => Some("java"),
        "kt" | "kts" => Some("kotlin"),
        "rb" => Some("ruby"),
        "c" | "h" => Some("c"),
        "cpp" | "cxx" | "cc" | "hpp" => Some("cpp"),
        "cs" => Some("csharp"),
        _ => None,
    }
}

#[must_use]
pub fn find_binary_in_path(binary: &str) -> Option<PathBuf> {
    let path_var = std::env::var("PATH").ok()?;
    for dir in std::env::split_paths(&path_var) {
        let candidate = dir.join(binary);
        if candidate.is_file() {
            return Some(candidate);
        }
        if cfg!(windows) {
            let exe = dir.join(format!("{binary}.exe"));
            if exe.is_file() {
                return Some(exe);
            }
        }
    }
    None
}

#[must_use]
pub fn install_hint_for_language(language: &str) -> &'static str {
    for info in KNOWN_SERVERS {
        if info.language == language {
            return info.install_hint;
        }
    }
    "No install instructions available for this language server."
}

#[must_use]
pub fn binary_for_language(language: &str) -> Option<&'static str> {
    for info in KNOWN_SERVERS {
        if info.language == language {
            return Some(info.binary);
        }
    }
    None
}

pub fn check_server_available(language: &str) -> Result<PathBuf, String> {
    let servers = default_servers();
    let config = servers
        .get(language)
        .ok_or_else(|| format!("No LSP server configured for '{language}'"))?;

    find_binary_in_path(&config.command).ok_or_else(|| {
        let hint = install_hint_for_language(language);
        format!(
            "Language server '{}' not found in PATH.\n\
             \n\
             ctx_refactor requires an external language server for '{}' files.\n\
             Install it with:\n\
             \n\
             \x20   {}\n\
             \n\
             Then retry. This is optional — ctx_search and ctx_graph work without it.",
            config.command, language, hint
        )
    })
}
