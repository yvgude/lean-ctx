//! Cross-file type-reference edges (GH #398).
//!
//! C#, Java, Go and Kotlin resolve types declared in the same namespace/package
//! **without any import statement**, so import edges alone miss those file
//! dependencies. This module derives consumer -> definer *file* edges from
//! `deep_queries` type-usage and extension-method analysis.
//!
//! It is the single source of truth for that resolution, shared by:
//! - the durable `graph_index` -> `PropertyGraph` mirror (`graph_index::edges`),
//!   so a background reindex reproduces the edges instead of dropping them
//!   (the GH #398 regression: the mirror clears the graph and previously had no
//!   type-usage edges, silently wiping `ctx_impact`'s blast radius), and
//! - the `ctx_impact` graph builder, which additionally emits the symbol-level
//!   edges `dead_code` relies on.
//!
//! Both callers therefore agree on the file-level blast radius.
//!
//! Per-language resolution scope ([`resolve_scope`]):
//! - **C# / Kotlin** carry an explicit namespace/package in the AST → resolve
//!   against the visible namespaces, with a capped global fallback for types
//!   reached via an import the cheap analysis did not model.
//! - **Go** has no per-symbol import for same-package types; its package *is*
//!   the file's directory, so resolution is strictly directory-scoped (no
//!   global fallback) — a common type name (`Config`, `Server`) declared in
//!   many packages still resolves to the one true same-package definer.
//! - **Java** (and any other language) keeps the namespace-free global fallback.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use crate::core::deep_queries::{CallSite, DeepAnalysis, TypeUse};

/// The package directory a file belongs to — Go's same-package boundary.
/// `services/motor.go` -> `services`, `main.go` -> `` (root package).
fn package_dir(rel_path: &str) -> String {
    Path::new(rel_path)
        .parent()
        .map(|p| p.to_string_lossy().replace('\\', "/"))
        .unwrap_or_default()
}

/// The file extension (lowercased ASCII) used to pick a resolution scope.
fn ext_of(rel_path: &str) -> String {
    Path::new(rel_path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
}

/// How a file's type usages resolve to definer files.
pub struct ResolveScope {
    /// Namespaces/packages a definer must live in to be a confirmed match.
    pub visible_ns: HashSet<String>,
    /// Whether a type with no visible-namespace match may fall back to the
    /// capped global definer set. Off for Go, whose unqualified names are
    /// same-package by language rule.
    pub allow_global_fallback: bool,
}

/// The resolution scope for a file, by language. Centralizes what was
/// previously duplicated across the mirror and both `ctx_impact` index paths.
pub fn resolve_scope(rel_path: &str, ext: &str, analysis: &DeepAnalysis) -> ResolveScope {
    match ext {
        "cs" | "kt" | "kts" => ResolveScope {
            visible_ns: dotted_package_namespaces(analysis),
            allow_global_fallback: true,
        },
        "go" => ResolveScope {
            visible_ns: std::iter::once(package_dir(rel_path)).collect(),
            allow_global_fallback: false,
        },
        // Java + everything else: no cheap namespace, capped global fallback.
        _ => ResolveScope {
            visible_ns: HashSet::new(),
            allow_global_fallback: true,
        },
    }
}

/// Definition sites per type name: `name -> [(file, namespace, line_start,
/// line_end)]`. The namespace (C# only; `None` elsewhere) lets resolution
/// disambiguate homonyms declared in different namespaces.
pub type DefIndex = HashMap<String, Vec<(String, Option<String>, usize, usize)>>;

/// Extension-method definition sites: `method_name -> [(file, line_start,
/// line_end)]`. Drives host resolution for `value.Foo()` extension calls where
/// the definer's type is never named at the call site.
pub type ExtMethodIndex = HashMap<String, Vec<(String, usize, usize)>>;

/// With no namespace-visible match the global fallback links every definer, but
/// drops names with more than this many definition sites as too generic to
/// attribute (e.g. `Config` in a monorepo).
const MAX_FALLBACK_DEF_SITES: usize = 5;

/// Extension methods resolve by name alone, so the same failsafe cap keeps a
/// generic method name from linking an implausible number of files.
const MAX_EXT_DEF_SITES: usize = 5;

/// A project source file paired with its parsed analysis (durable mirror path).
pub struct FileAnalysis<'a> {
    pub path: &'a str,
    pub ext: &'a str,
    pub analysis: &'a DeepAnalysis,
}

/// Build the project-wide type-definition index from per-file analyses.
///
/// Go carries no AST namespace; its package is the file's directory, injected
/// here so a same-directory definer is a confirmed (cap-bypassing) match. Every
/// other language uses the namespace the parser derived (C#/Kotlin) or `None`.
pub fn build_def_index<'a>(
    files: impl IntoIterator<Item = (&'a str, &'a DeepAnalysis)>,
) -> DefIndex {
    let mut def_index = DefIndex::new();
    for (path, analysis) in files {
        let is_go = ext_of(path) == "go";
        for t in &analysis.types {
            let namespace = if is_go {
                Some(package_dir(path))
            } else {
                t.namespace.clone()
            };
            def_index.entry(t.name.clone()).or_default().push((
                path.to_string(),
                namespace,
                t.line,
                t.end_line,
            ));
        }
    }
    def_index
}

/// Build the project-wide extension-method index from per-file analyses.
pub fn build_ext_method_index<'a>(
    files: impl IntoIterator<Item = (&'a str, &'a DeepAnalysis)>,
) -> ExtMethodIndex {
    let mut index = ExtMethodIndex::new();
    for (path, analysis) in files {
        for m in &analysis.ext_methods {
            index
                .entry(m.name.clone())
                .or_default()
                .push((path.to_string(), m.line, m.end_line));
        }
    }
    index
}

/// Namespaces a C#/Kotlin file can resolve unqualified types from: its own type
/// namespaces, every enclosing namespace (dot-prefix), and each import/`using`
/// target. Used to confirm namespace-aware type matches. Empty for languages
/// without an AST namespace, which then take the global fallback path.
pub fn dotted_package_namespaces(analysis: &DeepAnalysis) -> HashSet<String> {
    let mut set: HashSet<String> = HashSet::new();
    for t in &analysis.types {
        if let Some(ns) = &t.namespace {
            // Own namespace + every enclosing one: `A.B.C` -> A.B.C, A.B, A.
            let segs: Vec<&str> = ns.split('.').filter(|s| !s.is_empty()).collect();
            for i in 1..=segs.len() {
                set.insert(segs[..i].join("."));
            }
        }
    }
    for imp in &analysis.imports {
        let src = imp.source.trim();
        if !src.is_empty() {
            set.insert(src.to_string());
        }
    }
    set
}

/// Definition sites of types this file *uses* (field/param/base/generic/cast),
/// resolved against the project-wide definition index: `(defining_file,
/// type_name, line_start, line_end)`. This is what connects C#/Java
/// same-namespace consumers that have no import statement.
///
/// Hybrid, failsafe resolution:
/// - A definer whose namespace is **visible** to the consumer is always linked
///   — even past the fallback cap — because the match is unambiguous evidence,
///   and any homonym in a non-visible namespace is dropped (no cross-namespace
///   leak).
/// - With no visible match, and only when `allow_global_fallback` is set, the
///   global fallback links every definer but drops names with more than
///   `MAX_FALLBACK_DEF_SITES` definers as too generic to attribute. Languages
///   without namespaces (Java; C# global namespace) take this path; Go disables
///   it, since an unqualified name is same-package by language rule and must
///   never link across directories.
pub fn type_ref_targets(
    def_index: &DefIndex,
    type_uses: &[TypeUse],
    rel_path: &str,
    visible_ns: &HashSet<String>,
    allow_global_fallback: bool,
) -> Vec<(String, String, usize, usize)> {
    let mut targets: Vec<(String, String, usize, usize)> = Vec::new();
    for type_use in type_uses {
        let Some(sites) = def_index.get(&type_use.name) else {
            continue;
        };
        // Defined in this very file -> self-reference, not a dependency.
        let mut external: Vec<&(String, Option<String>, usize, usize)> =
            sites.iter().filter(|(f, _, _, _)| f != rel_path).collect();
        external.sort_unstable();
        external.dedup();
        if external.is_empty() {
            continue;
        }

        // Namespace-confirmed matches win and bypass the cap.
        let visible: Vec<&(String, Option<String>, usize, usize)> = external
            .iter()
            .copied()
            .filter(|(_, ns, _, _)| ns.as_deref().is_some_and(|n| visible_ns.contains(n)))
            .collect();

        let chosen = if !visible.is_empty() {
            visible
        } else if allow_global_fallback && external.len() <= MAX_FALLBACK_DEF_SITES {
            external
        } else {
            continue;
        };

        targets.extend(
            chosen
                .into_iter()
                .map(|(f, _ns, ls, le)| (f.clone(), type_use.name.clone(), *ls, *le)),
        );
    }
    targets.sort();
    targets.dedup();
    targets
}

/// Definition sites of extension methods this file calls (`value.Foo()`):
/// `(defining_file, method_name, line_start, line_end)`. Resolution is by method
/// name alone, so a self-filter and the `MAX_EXT_DEF_SITES` cap keep it
/// bounded; the index only ever holds genuine extension methods, which keeps the
/// name space small and distinct.
pub fn ext_method_targets(
    ext_index: &ExtMethodIndex,
    calls: &[CallSite],
    rel_path: &str,
) -> Vec<(String, String, usize, usize)> {
    let mut targets: Vec<(String, String, usize, usize)> = Vec::new();
    for call in calls {
        if !call.is_method {
            continue;
        }
        let Some(sites) = ext_index.get(&call.callee) else {
            continue;
        };
        let mut external: Vec<&(String, usize, usize)> =
            sites.iter().filter(|(f, _, _)| f != rel_path).collect();
        external.sort_unstable();
        external.dedup();
        if external.is_empty() || external.len() > MAX_EXT_DEF_SITES {
            continue;
        }
        targets.extend(
            external
                .into_iter()
                .map(|(f, ls, le)| (f.clone(), call.callee.clone(), *ls, *le)),
        );
    }
    targets.sort();
    targets.dedup();
    targets
}

/// Durable consumer -> definer **file** edges for the mirror path: every
/// resolved type usage and extension-method call, deduped and self-references
/// removed. Returned as sorted `(from, to)` pairs so the caller can emit
/// deterministic graph edges.
pub fn cross_file_type_edges(files: &[FileAnalysis]) -> Vec<(String, String)> {
    let def_index = build_def_index(files.iter().map(|f| (f.path, f.analysis)));
    let ext_index = build_ext_method_index(files.iter().map(|f| (f.path, f.analysis)));

    let mut edges: Vec<(String, String)> = Vec::new();
    for f in files {
        let scope = resolve_scope(f.path, f.ext, f.analysis);
        for (target, _name, _ls, _le) in type_ref_targets(
            &def_index,
            &f.analysis.type_uses,
            f.path,
            &scope.visible_ns,
            scope.allow_global_fallback,
        ) {
            edges.push((f.path.to_string(), target));
        }
        for (target, _name, _ls, _le) in ext_method_targets(&ext_index, &f.analysis.calls, f.path) {
            edges.push((f.path.to_string(), target));
        }
    }
    edges.sort();
    edges.dedup();
    edges
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::deep_queries::{TypeDef, TypeDefKind, TypeUse};

    fn type_def(name: &str, namespace: Option<&str>, line: usize, end: usize) -> TypeDef {
        TypeDef {
            name: name.to_string(),
            kind: TypeDefKind::Class,
            line,
            end_line: end,
            is_exported: true,
            generics: Vec::new(),
            namespace: namespace.map(str::to_string),
        }
    }

    fn type_use(name: &str) -> TypeUse {
        TypeUse {
            name: name.to_string(),
            line: 1,
        }
    }

    /// GH #398 (+ #641 namespace-aware): the TypeRef target resolution — unique
    /// definers connect, self-references are skipped, over-generic names (>
    /// fallback cap of 5 with no namespace match) are dropped, a definer in the
    /// consumer's visible namespace is linked even past the cap while its
    /// homonyms are discarded, and output is sorted + deduped (determinism #498).
    #[test]
    fn type_ref_targets_resolution_rules() {
        let mut def_index = DefIndex::new();
        def_index.insert(
            "Engine".into(),
            vec![("Models/Engine.cs".into(), Some("App.Core".into()), 1, 5)],
        );
        def_index.insert(
            "Motor".into(),
            vec![("Services/Motor.cs".into(), Some("App.Core".into()), 1, 9)],
        );
        // Six definers, no namespace info -> exceeds the fallback cap (5).
        def_index.insert(
            "Config".into(),
            (0..6)
                .map(|i| (format!("p{i}/Config.cs"), None, 1usize, 2usize))
                .collect(),
        );
        // Same name, two namespaces — one visible to the consumer, one not.
        def_index.insert(
            "Widget".into(),
            vec![
                ("Foo/Widget.cs".into(), Some("App.Foo".into()), 1, 4),
                ("Bar/Widget.cs".into(), Some("App.Bar".into()), 1, 4),
            ],
        );
        // Seven definers, one of them in the visible namespace -> cap bypass.
        let mut crowded: Vec<(String, Option<String>, usize, usize)> = (0..6)
            .map(|i| (format!("n{i}/Gadget.cs"), Some(format!("App.N{i}")), 1, 3))
            .collect();
        crowded.push(("Foo/Gadget.cs".into(), Some("App.Foo".into()), 1, 3));
        def_index.insert("Gadget".into(), crowded);

        let uses = |names: &[&str]| -> Vec<TypeUse> { names.iter().map(|n| type_use(n)).collect() };
        let visible: HashSet<String> = ["App.Foo".to_string(), "App".to_string()]
            .into_iter()
            .collect();
        let none = HashSet::new();

        // Unique definer in another file -> edge target with symbol site.
        assert_eq!(
            type_ref_targets(
                &def_index,
                &uses(&["Engine"]),
                "Services/Motor.cs",
                &none,
                true
            ),
            vec![("Models/Engine.cs".to_string(), "Engine".to_string(), 1, 5)]
        );
        // Using one's own type -> no self edge.
        assert!(
            type_ref_targets(
                &def_index,
                &uses(&["Motor"]),
                "Services/Motor.cs",
                &none,
                true
            )
            .is_empty()
        );
        // Defined in 6 files, no namespace match -> too generic, skipped.
        assert!(
            type_ref_targets(
                &def_index,
                &uses(&["Config"]),
                "Services/Motor.cs",
                &none,
                true
            )
            .is_empty()
        );
        // Unknown / external types -> nothing.
        assert!(type_ref_targets(&def_index, &uses(&["String"]), "x.cs", &none, true).is_empty());
        // Duplicate uses collapse into one sorted target list.
        assert_eq!(
            type_ref_targets(
                &def_index,
                &uses(&["Engine", "Engine"]),
                "x.cs",
                &none,
                true
            ),
            vec![("Models/Engine.cs".to_string(), "Engine".to_string(), 1, 5)]
        );
        // Namespace disambiguation: only the visible-namespace definer links.
        assert_eq!(
            type_ref_targets(
                &def_index,
                &uses(&["Widget"]),
                "Foo/Garage.cs",
                &visible,
                true
            ),
            vec![("Foo/Widget.cs".to_string(), "Widget".to_string(), 1, 4)]
        );
        // Without a visible namespace, both homonyms link (<= cap fallback).
        assert_eq!(
            type_ref_targets(&def_index, &uses(&["Widget"]), "Foo/Garage.cs", &none, true),
            vec![
                ("Bar/Widget.cs".to_string(), "Widget".to_string(), 1, 4),
                ("Foo/Widget.cs".to_string(), "Widget".to_string(), 1, 4),
            ]
        );
        // Cap bypass: 7 definers, but the one in the visible namespace links.
        assert_eq!(
            type_ref_targets(
                &def_index,
                &uses(&["Gadget"]),
                "Foo/Garage.cs",
                &visible,
                true
            ),
            vec![("Foo/Gadget.cs".to_string(), "Gadget".to_string(), 1, 3)]
        );
    }

    /// Go (GH #398 bug class): an unqualified type name declared in many
    /// packages must resolve **only** to the same-directory definer — even when
    /// the global definer count is far past the fallback cap — and must never
    /// link across directories. This is the case a namespace-free fallback gets
    /// wrong (false negative for common names, or cross-package false positive).
    #[test]
    fn go_resolution_is_strictly_directory_scoped() {
        let mut def_index = DefIndex::new();
        // `Config` declared in eight packages, including the consumer's own dir.
        let mut sites: Vec<(String, Option<String>, usize, usize)> = (0..7)
            .map(|i| (format!("pkg{i}/config.go"), Some(format!("pkg{i}")), 1, 3))
            .collect();
        sites.push(("services/config.go".into(), Some("services".into()), 1, 3));
        def_index.insert("Config".into(), sites);

        let uses = vec![type_use("Config")];
        // Consumer in `services` resolves to its same-dir definer only — the cap
        // (8 > 5) does not apply because the directory match is confirmed.
        let visible: HashSet<String> = std::iter::once("services".to_string()).collect();
        assert_eq!(
            type_ref_targets(&def_index, &uses, "services/engine.go", &visible, false),
            vec![("services/config.go".to_string(), "Config".to_string(), 1, 3)]
        );
        // A consumer whose directory has no `Config` definer links nothing —
        // strict Go scope forbids the cross-directory global fallback.
        let other: HashSet<String> = std::iter::once("cmd".to_string()).collect();
        assert!(type_ref_targets(&def_index, &uses, "cmd/main.go", &other, false).is_empty());
    }

    /// The durable mirror entry point: a same-namespace consumer links to the
    /// definer, self-references are excluded, and output is sorted + deduped.
    #[test]
    fn cross_file_type_edges_links_same_namespace_consumer() {
        let engine = DeepAnalysis {
            types: vec![type_def("Engine", Some("App.Core"), 1, 5)],
            ..DeepAnalysis::empty()
        };
        let motor = DeepAnalysis {
            types: vec![type_def("Motor", Some("App.Core"), 1, 9)],
            type_uses: vec![type_use("Engine")],
            ..DeepAnalysis::empty()
        };
        let unrelated = DeepAnalysis {
            types: vec![type_def("Logger", Some("App.Core"), 1, 3)],
            ..DeepAnalysis::empty()
        };

        let files = vec![
            FileAnalysis {
                path: "Models/Engine.cs",
                ext: "cs",
                analysis: &engine,
            },
            FileAnalysis {
                path: "Services/Motor.cs",
                ext: "cs",
                analysis: &motor,
            },
            FileAnalysis {
                path: "Services/Logger.cs",
                ext: "cs",
                analysis: &unrelated,
            },
        ];

        assert_eq!(
            cross_file_type_edges(&files),
            vec![(
                "Services/Motor.cs".to_string(),
                "Models/Engine.cs".to_string()
            )],
            "only the real consumer -> definer edge, no self- or unrelated edges"
        );
    }
}
