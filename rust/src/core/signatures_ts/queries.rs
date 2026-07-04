//! Per-language tree-sitter grammars + signature queries.
//!
//! **Why grammars are statically linked Rust crates, not dynamically loaded
//! WASM.** tree-sitter can load grammars at runtime as `.wasm` modules. We
//! deliberately reject that path and compile every grammar in at build time:
//!
//! - **Determinism (#498):** output must be a pure function of file content +
//!   mode. A pinned crate version yields one grammar revision for every user;
//!   a downloaded/`.wasm` grammar would make signatures depend on whatever
//!   blob happens to be on disk, breaking byte-stable prompt caching.
//! - **Offline & hermetic:** lean-ctx runs in sandboxes with no network. A
//!   runtime grammar fetch would fail there and silently degrade extraction.
//! - **Security / supply chain:** a static crate is vendored, version-pinned
//!   and `cargo audit`-able. Loading arbitrary native/WASM grammars at runtime
//!   is an unvetted code-execution surface with no provenance guarantees.
//!
//! Adding a language therefore means: a pinned optional dep in `Cargo.toml`
//! under the `tree-sitter` feature, a `get_language`/`get_query` arm here, a
//! `QUERY_*` const, and tests in `signatures_ts`.
//!
//! **One deliberate exception (#690):** long-tail grammars behind an
//! installed, SHA-256-pinned addon dylib. `get_language`'s final `_` arm
//! defers to [`super::grammar_loader::get_addon_language`] before giving up
//! — see that module's doc comment for how this narrows the three objections
//! above rather than abandoning them. `get_query`'s arms are unaffected:
//! query text for those languages stays a bundled `&'static str` here either
//! way, addon or not.

use tree_sitter::Language;

use super::grammar_loader::get_addon_language;

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

// The namespace `name` field is a `namespace_identifier` (or nested specifier),
// never a bare `identifier`, in tree-sitter-cpp ≥0.22 — matching `(identifier)`
// is a `Structure` error that rejects the *whole* query and silently drops C++
// to the regex fallback. `(_)` binds whatever node the grammar puts there.
const QUERY_CPP: &str = r"
(function_definition
  declarator: (function_declarator
    declarator: (_) @name)) @def
(struct_specifier name: (type_identifier) @name) @def
(class_specifier name: (type_identifier) @name) @def
(enum_specifier name: (type_identifier) @name) @def
(namespace_definition name: (_) @name) @def
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

/// Queries [tree-sitter-gdscript](https://crates.io/crates/tree-sitter-gdscript).
/// Godot global class (`class_name X`), inner classes (`class X:`), functions,
/// signals, and enums each expose a `name` child node.
const QUERY_GDSCRIPT: &str = r"
(class_name_statement (name) @name) @def
(class_definition (name) @name) @def
(function_definition (name) @name) @def
(signal_statement (name) @name) @def
(enum_definition (name) @name) @def
(export_variable_statement name: (name) @name) @def
(onready_variable_statement name: (name) @name) @def
(source (const_statement name: (name) @name) @def)
(source (variable_statement name: (name) @name) @def)
(class_body (const_statement name: (name) @name) @def)
(class_body (variable_statement name: (name) @name) @def)
";

/// Queries [tree-sitter-lua](https://crates.io/crates/tree-sitter-lua).
/// Lua has no `class`/`type` constructs; symbols are functions: `function f()`,
/// `local function f()`, table functions `function T.f()` / methods
/// `function T:m()`, and functions assigned to a (table) variable
/// (`M.f = function() … end`). The `@name` capture is always the simple
/// trailing identifier so it lines up with call-graph callees.
const QUERY_LUA: &str = r"
(function_declaration name: (identifier) @name) @def
(function_declaration name: (dot_index_expression field: (identifier) @name)) @def
(function_declaration name: (method_index_expression method: (identifier) @name)) @def
(assignment_statement
  (variable_list name: (identifier) @name)
  (expression_list value: (function_definition))) @def
(assignment_statement
  (variable_list name: (dot_index_expression field: (identifier) @name))
  (expression_list value: (function_definition))) @def
";

/// Queries [tree-sitter-luau](https://crates.io/crates/tree-sitter-luau).
/// Same function forms as Lua, plus Luau's `type X = …` / `export type X = …`
/// aliases (`type_definition`).
const QUERY_LUAU: &str = r"
(function_declaration name: (identifier) @name) @def
(function_declaration name: (dot_index_expression field: (identifier) @name)) @def
(function_declaration name: (method_index_expression method: (identifier) @name)) @def
(assignment_statement
  (variable_list name: (identifier) @name)
  (expression_list value: (function_definition))) @def
(assignment_statement
  (variable_list name: (dot_index_expression field: (identifier) @name))
  (expression_list value: (function_definition))) @def
(type_definition name: (identifier) @name) @def
";

/// Queries [tree-sitter-ocaml](https://crates.io/crates/tree-sitter-ocaml) for
/// `.ml` implementations. `let f x = …` is a `value_definition`; types, modules,
/// module types (signatures) and `external` FFI bindings each expose a name.
/// Node/field names follow the grammar's own `tags.scm`.
const QUERY_OCAML: &str = r"
(value_definition (let_binding pattern: (value_name) @name)) @def
(type_definition (type_binding name: (type_constructor) @name)) @def
(module_definition (module_binding (module_name) @name)) @def
(module_type_definition (module_type_name) @name) @def
(class_definition (class_binding (class_name) @name)) @def
(external (value_name) @name) @def
";

/// Queries the OCaml *interface* grammar (`LANGUAGE_OCAML_INTERFACE`) for `.mli`
/// files, where values are `value_specification`s (`val f : …`) rather than
/// definitions.
const QUERY_OCAML_INTERFACE: &str = r"
(value_specification (value_name) @name) @def
(type_definition (type_binding name: (type_constructor) @name)) @def
(module_definition (module_binding (module_name) @name)) @def
(module_type_definition (module_type_name) @name) @def
";

/// Queries [tree-sitter-haskell](https://crates.io/crates/tree-sitter-haskell).
/// The grammar models top-level declarations as the `declarations` supertype;
/// `function`/`bind` carry value bindings, `data_type`/`newtype`/`type_synonym`
/// carry type definitions, and `class` is a type class.
// NOTE: `type_synomym` is spelled exactly as the grammar defines it — an
// upstream typo in tree-sitter-haskell (missing the second `n`). Do not
// "correct" it or `Query::new` rejects the whole query.
const QUERY_HASKELL: &str = r"
(function name: (variable) @name) @def
(bind name: (variable) @name) @def
(data_type name: (name) @name) @def
(newtype name: (name) @name) @def
(type_synomym name: (name) @name) @def
(class name: (name) @name) @def
";

/// Queries [tree-sitter-julia](https://crates.io/crates/tree-sitter-julia).
/// `function f(x) … end`, short-form `f(x) = …` (`assignment`), `struct`,
/// `abstract type`, `module` and `macro` each surface a name.
const QUERY_JULIA: &str = r"
(function_definition (signature (call_expression (identifier) @name))) @def
(assignment (call_expression (identifier) @name)) @def
(struct_definition (type_head (identifier) @name)) @def
(abstract_definition (type_head (identifier) @name)) @def
(module_definition (identifier) @name) @def
(macro_definition (signature (call_expression (identifier) @name))) @def
";

/// Queries [tree-sitter-solidity](https://crates.io/crates/tree-sitter-solidity).
/// Contracts, interfaces, libraries, structs, enums, events, free/contract
/// functions and modifiers each expose a `name` field. Node names follow the
/// grammar's own `tags.scm`.
const QUERY_SOLIDITY: &str = r"
(contract_declaration name: (identifier) @name) @def
(interface_declaration name: (identifier) @name) @def
(library_declaration name: (identifier) @name) @def
(struct_declaration name: (identifier) @name) @def
(enum_declaration name: (identifier) @name) @def
(event_definition name: (identifier) @name) @def
(function_definition name: (identifier) @name) @def
(modifier_definition name: (identifier) @name) @def
";

/// Queries [tree-sitter-nix](https://crates.io/crates/tree-sitter-nix). Nix has
/// no named functions; the navigable symbols are attribute bindings whose value
/// is a lambda (`name = arg: …;`), matching the grammar's own `tags.scm`.
const QUERY_NIX: &str = r"
(binding attrpath: (attrpath) @name expression: (function_expression)) @def
";

/// Queries [tree-sitter-powershell](https://crates.io/crates/tree-sitter-powershell)
/// (airbus-cert grammar). Names are carried by child nodes (`function_name`,
/// `simple_name`), not named fields. `(A (B) @name)` matches direct children
/// only, so enum members (wrapped in their own nodes) and method parameters
/// (`variable` nodes) never leak into the capture.
const QUERY_POWERSHELL: &str = r"
(function_statement (function_name) @name) @def
(class_statement (simple_name) @name) @def
(class_method_definition (simple_name) @name) @def
(enum_statement (simple_name) @name) @def
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
        "gd" => tree_sitter_gdscript::LANGUAGE.into(),
        "lua" => tree_sitter_lua::LANGUAGE.into(),
        "luau" => tree_sitter_luau::LANGUAGE.into(),
        "ml" => tree_sitter_ocaml::LANGUAGE_OCAML.into(),
        "mli" => tree_sitter_ocaml::LANGUAGE_OCAML_INTERFACE.into(),
        "hs" => tree_sitter_haskell::LANGUAGE.into(),
        "jl" => tree_sitter_julia::LANGUAGE.into(),
        "sol" => tree_sitter_solidity::LANGUAGE.into(),
        "nix" => tree_sitter_nix::LANGUAGE.into(),
        "ps1" | "psm1" => tree_sitter_powershell::LANGUAGE.into(),
        _ => return get_addon_language(ext),
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
        "gd" => QUERY_GDSCRIPT,
        "lua" => QUERY_LUA,
        "luau" => QUERY_LUAU,
        "ml" => QUERY_OCAML,
        "mli" => QUERY_OCAML_INTERFACE,
        "hs" => QUERY_HASKELL,
        "jl" => QUERY_JULIA,
        "sol" => QUERY_SOLIDITY,
        "nix" => QUERY_NIX,
        "ps1" | "psm1" => QUERY_POWERSHELL,
        _ => return None,
    })
}
