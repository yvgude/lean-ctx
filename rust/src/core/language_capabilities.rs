#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LanguageId {
    Rust,
    TypeScript,
    JavaScript,
    Python,
    Go,
    Java,
    C,
    Cpp,
    Ruby,
    CSharp,
    Kotlin,
    Swift,
    Php,
    Bash,
    Dart,
    Scala,
    Elixir,
    Zig,
    Vue,
    Svelte,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LanguageCapabilities {
    pub deps_edges: bool,
    pub deep_queries: bool,
    pub import_resolver: bool,
}

impl LanguageId {
    pub fn id_str(&self) -> &'static str {
        match self {
            LanguageId::Rust => "rust",
            LanguageId::TypeScript => "typescript",
            LanguageId::JavaScript => "javascript",
            LanguageId::Python => "python",
            LanguageId::Go => "go",
            LanguageId::Java => "java",
            LanguageId::C => "c",
            LanguageId::Cpp => "cpp",
            LanguageId::Ruby => "ruby",
            LanguageId::CSharp => "csharp",
            LanguageId::Kotlin => "kotlin",
            LanguageId::Swift => "swift",
            LanguageId::Php => "php",
            LanguageId::Bash => "bash",
            LanguageId::Dart => "dart",
            LanguageId::Scala => "scala",
            LanguageId::Elixir => "elixir",
            LanguageId::Zig => "zig",
            LanguageId::Vue => "vue",
            LanguageId::Svelte => "svelte",
        }
    }
}

pub fn capabilities(lang: LanguageId) -> LanguageCapabilities {
    match lang {
        // tree-sitter backed (deep_queries + resolver can be meaningful)
        LanguageId::Rust
        | LanguageId::TypeScript
        | LanguageId::JavaScript
        | LanguageId::Python
        | LanguageId::Go
        | LanguageId::Java
        | LanguageId::C
        | LanguageId::Cpp
        | LanguageId::Ruby
        | LanguageId::CSharp
        | LanguageId::Kotlin
        | LanguageId::Swift
        | LanguageId::Php
        | LanguageId::Bash
        | LanguageId::Dart
        | LanguageId::Scala
        | LanguageId::Elixir
        | LanguageId::Zig => LanguageCapabilities {
            deps_edges: true,
            deep_queries: true,
            import_resolver: true,
        },
        // templating languages: we can extract deps edges, but no deep_queries/resolver.
        LanguageId::Vue | LanguageId::Svelte => LanguageCapabilities {
            deps_edges: true,
            deep_queries: false,
            import_resolver: false,
        },
    }
}

pub fn language_for_ext(ext: &str) -> Option<LanguageId> {
    let e = ext.trim().trim_start_matches('.').to_lowercase();
    match e.as_str() {
        "rs" => Some(LanguageId::Rust),
        "ts" | "tsx" => Some(LanguageId::TypeScript),
        "js" | "jsx" => Some(LanguageId::JavaScript),
        "py" => Some(LanguageId::Python),
        "go" => Some(LanguageId::Go),
        "java" => Some(LanguageId::Java),
        "c" | "h" => Some(LanguageId::C),
        "cpp" | "cc" | "cxx" | "hpp" | "hxx" | "hh" => Some(LanguageId::Cpp),
        "rb" => Some(LanguageId::Ruby),
        "cs" => Some(LanguageId::CSharp),
        "kt" | "kts" => Some(LanguageId::Kotlin),
        "swift" => Some(LanguageId::Swift),
        "php" => Some(LanguageId::Php),
        "sh" | "bash" => Some(LanguageId::Bash),
        "dart" => Some(LanguageId::Dart),
        "scala" | "sc" => Some(LanguageId::Scala),
        "ex" | "exs" => Some(LanguageId::Elixir),
        "zig" => Some(LanguageId::Zig),
        "vue" => Some(LanguageId::Vue),
        "svelte" => Some(LanguageId::Svelte),
        _ => None,
    }
}

pub fn language_for_path(path: &str) -> Option<LanguageId> {
    std::path::Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .and_then(language_for_ext)
}

pub fn is_indexable_ext(ext: &str) -> bool {
    language_for_ext(ext).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ext_mapping_basic() {
        assert_eq!(language_for_ext("rs"), Some(LanguageId::Rust));
        assert_eq!(language_for_ext(".tsx"), Some(LanguageId::TypeScript));
        assert_eq!(language_for_ext("JS"), Some(LanguageId::JavaScript));
        assert_eq!(language_for_ext("hxx"), Some(LanguageId::Cpp));
        assert_eq!(language_for_ext("exs"), Some(LanguageId::Elixir));
        assert_eq!(language_for_ext("unknown"), None);
    }

    #[test]
    fn indexable_ext_true_for_known() {
        assert!(is_indexable_ext("rs"));
        assert!(is_indexable_ext("vue"));
        assert!(!is_indexable_ext("md"));
    }

    #[test]
    fn caps_are_deterministic() {
        let c1 = capabilities(LanguageId::Rust);
        let c2 = capabilities(LanguageId::Rust);
        assert_eq!(c1, c2);
        assert!(c1.deps_edges);
    }
}
