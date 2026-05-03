//! Tree-sitter AST-aware code chunking for semantic search.
//!
//! Replaces heuristic line-prefix matching with proper AST parsing.
//! Extracts function bodies, struct definitions, class declarations etc.
//! as complete, self-contained chunks with accurate boundaries.
//!
//! Falls back to heuristic chunking for unsupported languages.

#[cfg(feature = "tree-sitter")]
use tree_sitter::{Language, Node, Parser, Query, QueryCursor, StreamingIterator};

use super::vector_index::{ChunkKind, CodeChunk};

#[cfg(feature = "tree-sitter")]
const CHUNK_QUERY_RUST: &str = r"
(function_item name: (identifier) @name) @chunk
(struct_item name: (type_identifier) @name) @chunk
(enum_item name: (type_identifier) @name) @chunk
(trait_item name: (type_identifier) @name) @chunk
(impl_item type: (type_identifier) @name) @chunk
(const_item name: (identifier) @name) @chunk
";

#[cfg(feature = "tree-sitter")]
const CHUNK_QUERY_TYPESCRIPT: &str = r"
(function_declaration name: (identifier) @name) @chunk
(class_declaration name: (type_identifier) @name) @chunk
(abstract_class_declaration name: (type_identifier) @name) @chunk
(interface_declaration name: (type_identifier) @name) @chunk
(type_alias_declaration name: (type_identifier) @name) @chunk
(method_definition name: (property_identifier) @name) @chunk
(variable_declarator name: (identifier) @name value: (arrow_function)) @chunk
";

#[cfg(feature = "tree-sitter")]
const CHUNK_QUERY_JAVASCRIPT: &str = r"
(function_declaration name: (identifier) @name) @chunk
(class_declaration name: (identifier) @name) @chunk
(method_definition name: (property_identifier) @name) @chunk
(variable_declarator name: (identifier) @name value: (arrow_function)) @chunk
";

#[cfg(feature = "tree-sitter")]
const CHUNK_QUERY_PYTHON: &str = r"
(function_definition name: (identifier) @name) @chunk
(class_definition name: (identifier) @name) @chunk
";

#[cfg(feature = "tree-sitter")]
const CHUNK_QUERY_GO: &str = r"
(function_declaration name: (identifier) @name) @chunk
(method_declaration name: (field_identifier) @name) @chunk
(type_spec name: (type_identifier) @name) @chunk
";

#[cfg(feature = "tree-sitter")]
const CHUNK_QUERY_JAVA: &str = r"
(method_declaration name: (identifier) @name) @chunk
(class_declaration name: (identifier) @name) @chunk
(interface_declaration name: (identifier) @name) @chunk
(enum_declaration name: (identifier) @name) @chunk
(constructor_declaration name: (identifier) @name) @chunk
";

#[cfg(feature = "tree-sitter")]
const CHUNK_QUERY_C: &str = r"
(function_definition
  declarator: (function_declarator
    declarator: (identifier) @name)) @chunk
(struct_specifier name: (type_identifier) @name) @chunk
(enum_specifier name: (type_identifier) @name) @chunk
";

#[cfg(feature = "tree-sitter")]
const CHUNK_QUERY_CPP: &str = r"
(function_definition
  declarator: (function_declarator
    declarator: (_) @name)) @chunk
(struct_specifier name: (type_identifier) @name) @chunk
(class_specifier name: (type_identifier) @name) @chunk
(enum_specifier name: (type_identifier) @name) @chunk
(namespace_definition name: (identifier) @name) @chunk
";

/// Extract code chunks from a file using tree-sitter AST parsing.
///
/// Returns `None` if the language is unsupported, allowing callers to fall back
/// to heuristic-based chunking.
#[cfg(feature = "tree-sitter")]
pub fn extract_chunks_ts(file_path: &str, content: &str, file_ext: &str) -> Option<Vec<CodeChunk>> {
    let language = get_language(file_ext)?;
    let query_src = get_chunk_query(file_ext)?;

    thread_local! {
        static PARSER: std::cell::RefCell<Parser> = std::cell::RefCell::new(Parser::new());
    }

    let tree = PARSER.with(|p| {
        let mut parser = p.borrow_mut();
        let _ = parser.set_language(&language);
        parser.parse(content, None)
    })?;

    let query = Query::new(&language, query_src).ok()?;
    let chunk_idx = find_capture_index(&query, "chunk")?;
    let name_idx = find_capture_index(&query, "name")?;

    let source = content.as_bytes();
    let lines: Vec<&str> = content.lines().collect();
    let mut chunks = Vec::new();
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(&query, tree.root_node(), source);
    let mut seen_ranges = Vec::new();

    while let Some(m) = matches.next() {
        let mut chunk_node: Option<Node> = None;
        let mut name_text = String::new();

        for cap in m.captures {
            if cap.index == chunk_idx {
                chunk_node = Some(cap.node);
            } else if cap.index == name_idx {
                if let Ok(text) = cap.node.utf8_text(source) {
                    name_text = text.to_string();
                }
            }
        }

        if let Some(node) = chunk_node {
            if name_text.is_empty() {
                continue;
            }

            let start_line = node.start_position().row;
            let end_line = node.end_position().row;

            let range = (start_line, end_line);
            if seen_ranges
                .iter()
                .any(|&(s, e)| s <= start_line && end_line <= e && range != (s, e))
            {
                continue;
            }
            seen_ranges.push(range);

            let block: String = lines[start_line..=end_line.min(lines.len() - 1)]
                .to_vec()
                .join("\n");

            let kind = node_kind_to_chunk_kind(node.kind());
            let tokens = super::vector_index::tokenize_for_index(&block);
            let token_count = tokens.len();

            chunks.push(CodeChunk {
                file_path: file_path.to_string(),
                symbol_name: name_text,
                kind,
                start_line: start_line + 1,
                end_line: end_line + 1,
                content: block,
                tokens,
                token_count,
            });
        }
    }

    if chunks.is_empty() {
        return None;
    }

    chunks.sort_by_key(|c| c.start_line);
    Some(chunks)
}

#[cfg(not(feature = "tree-sitter"))]
pub fn extract_chunks_ts(
    _file_path: &str,
    _content: &str,
    _file_ext: &str,
) -> Option<Vec<CodeChunk>> {
    None
}

#[cfg(feature = "tree-sitter")]
fn get_language(ext: &str) -> Option<Language> {
    Some(match ext {
        "rs" => tree_sitter_rust::LANGUAGE.into(),
        "ts" => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
        "tsx" => tree_sitter_typescript::LANGUAGE_TSX.into(),
        "js" | "jsx" => tree_sitter_javascript::LANGUAGE.into(),
        "py" => tree_sitter_python::LANGUAGE.into(),
        "go" => tree_sitter_go::LANGUAGE.into(),
        "java" => tree_sitter_java::LANGUAGE.into(),
        "c" | "h" => tree_sitter_c::LANGUAGE.into(),
        "cpp" | "cc" | "cxx" | "hpp" | "hxx" | "hh" => tree_sitter_cpp::LANGUAGE.into(),
        _ => return None,
    })
}

#[cfg(feature = "tree-sitter")]
fn get_chunk_query(ext: &str) -> Option<&'static str> {
    Some(match ext {
        "rs" => CHUNK_QUERY_RUST,
        "ts" | "tsx" => CHUNK_QUERY_TYPESCRIPT,
        "js" | "jsx" => CHUNK_QUERY_JAVASCRIPT,
        "py" => CHUNK_QUERY_PYTHON,
        "go" => CHUNK_QUERY_GO,
        "java" => CHUNK_QUERY_JAVA,
        "c" | "h" => CHUNK_QUERY_C,
        "cpp" | "cc" | "cxx" | "hpp" | "hxx" | "hh" => CHUNK_QUERY_CPP,
        _ => return None,
    })
}

#[cfg(feature = "tree-sitter")]
fn find_capture_index(query: &Query, name: &str) -> Option<u32> {
    query
        .capture_names()
        .iter()
        .position(|n| *n == name)
        .map(|i| i as u32)
}

fn node_kind_to_chunk_kind(kind: &str) -> ChunkKind {
    match kind {
        "function_item"
        | "function_declaration"
        | "function_definition"
        | "method_declaration"
        | "method_definition"
        | "constructor_declaration"
        | "variable_declarator" => ChunkKind::Function,

        "struct_item"
        | "struct_specifier"
        | "struct_declaration"
        | "enum_item"
        | "enum_specifier"
        | "enum_declaration"
        | "trait_item"
        | "interface_declaration"
        | "type_alias_declaration"
        | "type_spec" => ChunkKind::Struct,

        "impl_item" => ChunkKind::Impl,

        "class_declaration"
        | "abstract_class_declaration"
        | "class_specifier"
        | "class_definition" => ChunkKind::Class,

        "namespace_definition" | "namespace_declaration" => ChunkKind::Module,

        _ => ChunkKind::Other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_rust_chunks() {
        let src = r#"use std::io;

pub fn process(input: &str) -> String {
    input.to_uppercase()
}

pub struct Config {
    pub name: String,
    pub port: u16,
}

impl Config {
    pub fn new() -> Self {
        Self { name: "default".into(), port: 8080 }
    }
}

fn helper() -> bool {
    true
}
"#;
        let chunks = extract_chunks_ts("main.rs", src, "rs").unwrap();
        assert!(
            chunks.len() >= 4,
            "expected >=4 chunks, got {}",
            chunks.len()
        );

        let names: Vec<&str> = chunks.iter().map(|c| c.symbol_name.as_str()).collect();
        assert!(names.contains(&"process"), "got {names:?}");
        assert!(names.contains(&"Config"), "got {names:?}");
        assert!(names.contains(&"helper"), "got {names:?}");

        let process = chunks.iter().find(|c| c.symbol_name == "process").unwrap();
        assert!(matches!(process.kind, ChunkKind::Function));
        assert!(process.content.contains("to_uppercase"));
    }

    #[test]
    fn extract_typescript_chunks() {
        let src = r"
export function greet(name: string): string {
    return `Hello ${name}`;
}

export class UserService {
    findUser(id: number): User {
        return db.find(id);
    }
}

const handler = async (req: Request): Promise<Response> => {
    return new Response();
};
";
        let chunks = extract_chunks_ts("app.ts", src, "ts").unwrap();
        assert!(
            chunks.len() >= 3,
            "expected >=3 chunks, got {}",
            chunks.len()
        );

        let names: Vec<&str> = chunks.iter().map(|c| c.symbol_name.as_str()).collect();
        assert!(names.contains(&"greet"), "got {names:?}");
        assert!(names.contains(&"UserService"), "got {names:?}");
    }

    #[test]
    fn extract_python_chunks() {
        let src = r"
class AuthService:
    def __init__(self, db):
        self.db = db

    def authenticate(self, email: str) -> bool:
        user = self.db.find(email)
        return user is not None

def create_app():
    return Flask(__name__)
";
        let chunks = extract_chunks_ts("app.py", src, "py").unwrap();
        assert!(
            chunks.len() >= 2,
            "expected >=2 chunks, got {}",
            chunks.len()
        );

        let names: Vec<&str> = chunks.iter().map(|c| c.symbol_name.as_str()).collect();
        assert!(names.contains(&"AuthService"), "got {names:?}");
        assert!(names.contains(&"create_app"), "got {names:?}");

        let auth = chunks
            .iter()
            .find(|c| c.symbol_name == "AuthService")
            .unwrap();
        assert!(auth.content.contains("authenticate"));
    }

    #[test]
    fn chunks_contain_full_body() {
        let src = r#"
pub fn complex(x: i32, y: i32) -> Result<String, Error> {
    let sum = x + y;
    let result = format!("Sum: {}", sum);
    if sum > 100 {
        return Err(Error::new("too large"));
    }
    Ok(result)
}
"#;
        let chunks = extract_chunks_ts("math.rs", src, "rs").unwrap();
        let complex = chunks.iter().find(|c| c.symbol_name == "complex").unwrap();
        assert!(complex.content.contains("sum > 100"));
        assert!(complex.content.contains("Ok(result)"));
    }

    #[test]
    fn unsupported_language_returns_none() {
        assert!(extract_chunks_ts("file.xyz", "content", "xyz").is_none());
    }

    #[test]
    fn empty_file_returns_none() {
        assert!(extract_chunks_ts("empty.rs", "", "rs").is_none());
    }

    #[test]
    fn chunks_sorted_by_line() {
        let src = r"
fn b_func() {}
fn a_func() {}
";
        let chunks = extract_chunks_ts("sort.rs", src, "rs").unwrap();
        assert!(chunks[0].start_line <= chunks[1].start_line);
    }
}
