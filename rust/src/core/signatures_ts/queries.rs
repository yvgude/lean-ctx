use tree_sitter::Language;

const QUERY_RUST: &str = r"
(function_item name: (identifier) @name) @def
(struct_item name: (type_identifier) @name) @def
(enum_item name: (type_identifier) @name) @def
(trait_item name: (type_identifier) @name) @def
(impl_item type: (type_identifier) @name) @def
(type_item name: (type_identifier) @name) @def
(const_item name: (identifier) @name) @def
";

const QUERY_TYPESCRIPT: &str = r"
(function_declaration name: (identifier) @name) @def
(class_declaration name: (type_identifier) @name) @def
(abstract_class_declaration name: (type_identifier) @name) @def
(interface_declaration name: (type_identifier) @name) @def
(type_alias_declaration name: (type_identifier) @name) @def
(method_definition name: (property_identifier) @name) @def
(variable_declarator name: (identifier) @name value: (arrow_function)) @def
";

const QUERY_JAVASCRIPT: &str = r"
(function_declaration name: (identifier) @name) @def
(class_declaration name: (identifier) @name) @def
(method_definition name: (property_identifier) @name) @def
(variable_declarator name: (identifier) @name value: (arrow_function)) @def
";

const QUERY_PYTHON: &str = r"
(function_definition name: (identifier) @name) @def
(class_definition name: (identifier) @name) @def
";

const QUERY_GO: &str = r"
(function_declaration name: (identifier) @name) @def
(method_declaration name: (field_identifier) @name) @def
(type_spec name: (type_identifier) @name) @def
";

const QUERY_JAVA: &str = r"
(method_declaration name: (identifier) @name) @def
(class_declaration name: (identifier) @name) @def
(interface_declaration name: (identifier) @name) @def
(enum_declaration name: (identifier) @name) @def
(constructor_declaration name: (identifier) @name) @def
";

const QUERY_C: &str = r"
(function_definition
  declarator: (function_declarator
    declarator: (identifier) @name)) @def
(struct_specifier name: (type_identifier) @name) @def
(enum_specifier name: (type_identifier) @name) @def
(type_definition declarator: (type_identifier) @name) @def
";

const QUERY_CPP: &str = r"
(function_definition
  declarator: (function_declarator
    declarator: (_) @name)) @def
(struct_specifier name: (type_identifier) @name) @def
(class_specifier name: (type_identifier) @name) @def
(enum_specifier name: (type_identifier) @name) @def
(namespace_definition name: (identifier) @name) @def
";

const QUERY_RUBY: &str = r"
(method name: (identifier) @name) @def
(singleton_method name: (identifier) @name) @def
(class name: (_) @name) @def
(module name: (_) @name) @def
";

const QUERY_CSHARP: &str = r"
(method_declaration name: (identifier) @name) @def
(class_declaration name: (identifier) @name) @def
(interface_declaration name: (identifier) @name) @def
(struct_declaration name: (identifier) @name) @def
(enum_declaration name: (identifier) @name) @def
(record_declaration name: (identifier) @name) @def
(namespace_declaration name: (identifier) @name) @def
";

/// Queries [tree-sitter-kotlin-ng](https://crates.io/crates/tree-sitter-kotlin-ng). Interfaces use `class_declaration` with an `interface` keyword (no separate `interface_declaration` node).
const QUERY_KOTLIN: &str = r"
(function_declaration name: (identifier) @name) @def
(class_declaration name: (identifier) @name) @def
(object_declaration name: (identifier) @name) @def
";

/// Swift grammar uses `class_declaration` for class, struct, enum, actor, and extension (via `declaration_kind`).
const QUERY_SWIFT: &str = r"
(function_declaration name: (simple_identifier) @name) @def
(class_declaration name: (type_identifier) @name) @def
(protocol_declaration name: (type_identifier) @name) @def
(protocol_function_declaration name: (simple_identifier) @name) @def
";

const QUERY_PHP: &str = r"
(function_definition name: (name) @name) @def
(class_declaration name: (name) @name) @def
(interface_declaration name: (name) @name) @def
(trait_declaration name: (name) @name) @def
(method_declaration name: (name) @name) @def
";

const QUERY_BASH: &str = r"
(function_definition name: (word) @name) @def
";

const QUERY_DART: &str = r"
(class_declaration name: (identifier) @name) @def
(enum_declaration name: (identifier) @name) @def
(mixin_declaration (identifier) @name) @def
(type_alias (type_identifier) @name) @def
";

const QUERY_SCALA: &str = r"
(class_definition name: (identifier) @name) @def
(object_definition name: (identifier) @name) @def
(trait_definition name: (identifier) @name) @def
(enum_definition name: (identifier) @name) @def
(function_definition name: (identifier) @name) @def
(type_definition name: (type_identifier) @name) @def
";

const QUERY_ELIXIR: &str = r#"
(call
  target: (identifier) @_keyword
  (arguments (alias) @name)
  (#any-of? @_keyword "defmodule" "defprotocol")) @def

(call
  target: (identifier) @_keyword
  (arguments
    [
      (identifier) @name
      (call target: (identifier) @name)
      (binary_operator left: (call target: (identifier) @name) operator: "when")
    ])
  (#any-of? @_keyword "def" "defp" "defmacro" "defmacrop")) @def
"#;

const QUERY_ZIG: &str = r"
(function_declaration name: (identifier) @name) @def
";

pub(super) fn get_language(ext: &str) -> Option<Language> {
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
        "rb" => tree_sitter_ruby::LANGUAGE.into(),
        "cs" => tree_sitter_c_sharp::LANGUAGE.into(),
        "kt" | "kts" => tree_sitter_kotlin_ng::LANGUAGE.into(),
        "swift" => tree_sitter_swift::LANGUAGE.into(),
        "php" => tree_sitter_php::LANGUAGE_PHP.into(),
        "sh" | "bash" => tree_sitter_bash::LANGUAGE.into(),
        "dart" => tree_sitter_dart::LANGUAGE.into(),
        "scala" | "sc" => tree_sitter_scala::LANGUAGE.into(),
        "ex" | "exs" => tree_sitter_elixir::LANGUAGE.into(),
        "zig" => tree_sitter_zig::LANGUAGE.into(),
        _ => return None,
    })
}

pub(super) fn get_query(ext: &str) -> Option<&'static str> {
    Some(match ext {
        "rs" => QUERY_RUST,
        "ts" | "tsx" => QUERY_TYPESCRIPT,
        "js" | "jsx" => QUERY_JAVASCRIPT,
        "py" => QUERY_PYTHON,
        "go" => QUERY_GO,
        "java" => QUERY_JAVA,
        "c" | "h" => QUERY_C,
        "cpp" | "cc" | "cxx" | "hpp" | "hxx" | "hh" => QUERY_CPP,
        "rb" => QUERY_RUBY,
        "cs" => QUERY_CSHARP,
        "kt" | "kts" => QUERY_KOTLIN,
        "swift" => QUERY_SWIFT,
        "php" => QUERY_PHP,
        "sh" | "bash" => QUERY_BASH,
        "dart" => QUERY_DART,
        "scala" | "sc" => QUERY_SCALA,
        "ex" | "exs" => QUERY_ELIXIR,
        "zig" => QUERY_ZIG,
        _ => return None,
    })
}
