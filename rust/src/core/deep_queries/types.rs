//! Types and data structures for deep query analysis.

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ImportInfo {
    pub source: String,
    pub names: Vec<String>,
    pub kind: ImportKind,
    pub line: usize,
    pub is_type_only: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ImportKind {
    Named,
    Default,
    Star,
    SideEffect,
    Dynamic,
    Reexport,
}

#[derive(Debug, Clone)]
pub struct CallSite {
    pub callee: String,
    pub line: usize,
    pub col: usize,
    pub receiver: Option<String>,
    pub is_method: bool,
}

#[derive(Debug, Clone)]
pub struct TypeDef {
    pub name: String,
    pub kind: TypeDefKind,
    pub line: usize,
    pub end_line: usize,
    pub is_exported: bool,
    pub generics: Vec<String>,
    /// Declaring namespace, when the language makes it cheaply derivable from
    /// the AST. Currently only filled for C# (block- and file-scoped); other
    /// languages leave it `None`. Drives namespace-aware type resolution in
    /// `ctx_impact` so homonyms across namespaces are not conflated (GH #398).
    pub namespace: Option<String>,
}

/// A C# extension method (`static T Foo(this X x, …)`). Captured so that a call
/// `value.Foo()` can be linked to the file that *defines* the extension, even
/// though the receiver is an instance and the definer's type name is never
/// written at the call site (GH #398 follow-up). Kept deliberately small —
/// only what the host-resolution needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtMethodDef {
    pub name: String,
    pub line: usize,
    pub end_line: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub enum TypeDefKind {
    Class,
    Interface,
    TypeAlias,
    Enum,
    Struct,
    Trait,
    Protocol,
    Record,
    Annotation,
    Union,
}

/// A *usage* of a named type (field/parameter/property/return type, base
/// class, generic argument, cast, `typeof`). Languages with implicit
/// same-namespace/package visibility (C#, Java) consume types without any
/// import statement, so type usages are the only reliable file-dependency
/// signal there (GH #398).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypeUse {
    pub name: String,
    pub line: usize,
}

#[derive(Debug, Clone)]
pub struct DeepAnalysis {
    pub imports: Vec<ImportInfo>,
    pub calls: Vec<CallSite>,
    pub types: Vec<TypeDef>,
    pub exports: Vec<String>,
    pub type_uses: Vec<TypeUse>,
    pub ext_methods: Vec<ExtMethodDef>,
}

impl DeepAnalysis {
    #[must_use]
    pub fn empty() -> Self {
        Self {
            imports: Vec::new(),
            calls: Vec::new(),
            types: Vec::new(),
            exports: Vec::new(),
            type_uses: Vec::new(),
            ext_methods: Vec::new(),
        }
    }
}
