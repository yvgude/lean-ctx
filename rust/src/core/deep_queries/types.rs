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

#[derive(Debug, Clone)]
pub struct DeepAnalysis {
    pub imports: Vec<ImportInfo>,
    pub calls: Vec<CallSite>,
    pub types: Vec<TypeDef>,
    pub exports: Vec<String>,
}

impl DeepAnalysis {
    pub fn empty() -> Self {
        Self {
            imports: Vec::new(),
            calls: Vec::new(),
            types: Vec::new(),
            exports: Vec::new(),
        }
    }
}
