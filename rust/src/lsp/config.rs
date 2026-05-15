use std::collections::HashMap;

#[derive(Debug, Clone)]
pub struct LspServerConfig {
    pub command: String,
    pub args: Vec<String>,
}

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

pub fn language_for_extension(ext: &str) -> Option<&'static str> {
    match ext {
        "rs" => Some("rust"),
        "ts" | "tsx" => Some("typescript"),
        "js" | "jsx" | "mjs" | "cjs" => Some("javascript"),
        "py" | "pyi" => Some("python"),
        "go" => Some("go"),
        "java" => Some("java"),
        "rb" => Some("ruby"),
        "c" | "h" => Some("c"),
        "cpp" | "cxx" | "cc" | "hpp" => Some("cpp"),
        "cs" => Some("csharp"),
        _ => None,
    }
}
