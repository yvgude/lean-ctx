// SPDX-License-Identifier: Apache-2.0
//! The Rust side: `syn` reduces one file to a [`FileModel`].

use std::collections::{BTreeMap, BTreeSet};
use std::str::FromStr;

use proc_macro2::{Delimiter, Span, TokenStream, TokenTree};
use syn::parse::Parser;
use syn::punctuated::Punctuated;
use syn::spanned::Spanned;
use syn::visit::{self, Visit};
use syn::{
    Arm, Attribute, BinOp, Block, Expr, ExprCall, ExprIf, ExprMethodCall, ExprStruct, ExprTry,
    ImplItemFn, Item, ItemFn, ItemImpl, ItemMacro, ItemMod, ItemUse, Lit, Macro, Pat, ReturnType,
    Signature, Stmt, TraitItemFn, Type, UnOp, UseTree,
};

use crate::model::{
    AssertInfo, AssertKind, Event, FileModel, Filter, Helper, LitVal, TestItem, clip, is_mockish,
    is_test_env_var, norm_number,
};
use crate::paths::normalize;
use crate::rules::{
    ASSERT_WEAKENED, BRANCH_ON_TEST_INPUT, ERROR_SWALLOWED, FIXTURE_CONSTANT, MONKEY_PATCH,
    SKIP_ADDED,
};

/// Roots that never belong to the crate under test.
const STD_ROOTS: [&str; 3] = ["std", "core", "alloc"];
const TEST_LIBS: [&str; 10] = [
    "mockall",
    "mockito",
    "proptest",
    "quickcheck",
    "rstest",
    "tempfile",
    "pretty_assertions",
    "assert_matches",
    "insta",
    "mock_instant",
];
const PRELUDE: [&str; 40] = [
    "Some",
    "None",
    "Ok",
    "Err",
    "Vec",
    "String",
    "Box",
    "Option",
    "Result",
    "Default",
    "drop",
    "From",
    "Into",
    "ToString",
    "ToOwned",
    "Iterator",
    "IntoIterator",
    "Clone",
    "HashMap",
    "HashSet",
    "BTreeMap",
    "BTreeSet",
    "VecDeque",
    "Arc",
    "Rc",
    "RefCell",
    "Cell",
    "Mutex",
    "RwLock",
    "PathBuf",
    "Path",
    "Duration",
    "Instant",
    "SystemTime",
    "format",
    "char",
    "u8",
    "u32",
    "i32",
    "usize",
];
const TIME_HASH_WORDS: [&str; 16] = [
    "SystemTime",
    "Instant",
    "UNIX_EPOCH",
    "Utc",
    "Local::now",
    "chrono",
    "Hasher",
    "RandomState",
    "DefaultHasher",
    "BuildHasher",
    "hash",
    "now",
    "clock",
    "time",
    "seed",
    "random",
];

pub(crate) fn line(span: Span) -> u32 {
    u32::try_from(span.start().line).unwrap_or(u32::MAX)
}

pub(crate) fn parse(src: &str) -> Result<syn::File, String> {
    syn::parse_file(src).map_err(|e| {
        let at = e.span().start();
        format!("syn: {e} at {}:{}", at.line, at.column + 1)
    })
}

/// Build the model of one parsed file.
pub(crate) fn build(
    file: &syn::File,
    src: &str,
    path: &str,
    whole_test: bool,
    crate_ident: &str,
) -> FileModel {
    let mut w = Walker {
        lines: src.lines().collect(),
        path: path.to_string(),
        whole_test,
        crate_ident: crate_ident.to_string(),
        test_depth: 0,
        model: FileModel::default(),
        collectors: Vec::new(),
        fn_names: Vec::new(),
        impl_names: Vec::new(),
        branch: 0,
        assert_depth: 0,
        imports: BTreeMap::new(),
        glob_real: false,
        locals: BTreeSet::new(),
        cfg_items: Vec::new(),
        plain_names: BTreeSet::new(),
    };
    w.prepass(&file.items, whole_test);
    for item in &file.items {
        w.visit_item(item);
    }
    w.finish_cfg_items();
    w.model
}

#[derive(Default)]
struct Collector {
    asserts: Vec<AssertInfo>,
    calls: BTreeSet<String>,
    unwraps: u32,
    oks: Vec<u32>,
    mock_used: bool,
    real_call: bool,
}

#[allow(clippy::struct_excessive_bools)]
struct CfgItem {
    name: String,
    is_use: bool,
    line: u32,
    positive: bool,
    negative: bool,
    time_hash: bool,
    text: String,
}

struct Walker<'a> {
    lines: Vec<&'a str>,
    path: String,
    whole_test: bool,
    crate_ident: String,
    test_depth: u32,
    model: FileModel,
    collectors: Vec<Collector>,
    fn_names: Vec<String>,
    impl_names: Vec<String>,
    branch: u32,
    assert_depth: u32,
    /// Imported name → root segment of its path.
    imports: BTreeMap<String, String>,
    /// A glob import from the crate under test (`use super::*`, ...).
    glob_real: bool,
    /// Items defined in test code of this file.
    locals: BTreeSet<String>,
    cfg_items: Vec<CfgItem>,
    plain_names: BTreeSet<String>,
}

impl Walker<'_> {
    fn in_test(&self) -> bool {
        self.whole_test || self.test_depth > 0
    }

    fn fn_name(&self) -> String {
        self.fn_names.last().cloned().unwrap_or_default()
    }

    fn slice(&self, span: Span) -> String {
        let (s, e) = (span.start(), span.end());
        if s.line == 0 || e.line < s.line || e.line > self.lines.len() {
            return String::new();
        }
        let mut out = String::new();
        for l in s.line..=e.line {
            let text = self.lines[l - 1];
            let from = if l == s.line { s.column } else { 0 };
            let to = if l == e.line { e.column } else { usize::MAX };
            let piece: String = text
                .chars()
                .skip(from)
                .take(to.saturating_sub(from))
                .collect();
            out.push_str(&piece);
            out.push('\n');
        }
        out
    }

    /// Whitespace- and comment-insensitive text of a span.
    fn norm(&self, span: Span) -> String {
        let raw = self.slice(span);
        TokenStream::from_str(&raw).map_or_else(
            |_| raw.split_whitespace().collect::<Vec<_>>().join(" "),
            |ts| ts.to_string(),
        )
    }

    /// The source text of a span with whitespace collapsed (for details).
    fn show(&self, span: Span) -> String {
        self.slice(span)
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
    }

    fn event(&mut self, rule: &'static str, key: String, line: u32, detail: String) {
        self.event_with(rule, key, line, detail, None, Filter::None);
    }

    fn event_with(
        &mut self,
        rule: &'static str,
        key: String,
        line: u32,
        detail: String,
        lit: Option<LitVal>,
        filter: Filter,
    ) {
        self.model.events.push(Event {
            rule,
            key,
            line,
            detail: if detail.chars().count() > 1900 {
                clip(&detail, 1900)
            } else {
                detail
            },
            lit,
            filter,
        });
    }

    // ----- pre-pass: imports and local items -----

    fn prepass(&mut self, items: &[Item], test: bool) {
        for item in items {
            match item {
                Item::Use(u) => self.collect_use(&u.tree, &mut Vec::new()),
                Item::Mod(m) => {
                    let t = test || has_cfg_test(&m.attrs);
                    if let Some((_, inner)) = &m.content {
                        self.prepass(inner, t);
                    }
                }
                Item::Fn(f) if test => {
                    self.locals.insert(f.sig.ident.to_string());
                }
                Item::Struct(s) if test => {
                    self.locals.insert(s.ident.to_string());
                }
                Item::Enum(e) if test => {
                    self.locals.insert(e.ident.to_string());
                }
                _ => {}
            }
        }
    }

    fn collect_use(&mut self, tree: &UseTree, prefix: &mut Vec<String>) {
        match tree {
            UseTree::Path(p) => {
                prefix.push(p.ident.to_string());
                self.collect_use(&p.tree, prefix);
                prefix.pop();
            }
            UseTree::Name(n) => {
                let name = n.ident.to_string();
                let root = prefix.first().cloned().unwrap_or_else(|| name.clone());
                if !prefix.iter().any(|s| is_mockish(s)) && !is_mockish(&name) {
                    self.imports.insert(name, root);
                }
            }
            UseTree::Rename(r) => {
                let root = prefix
                    .first()
                    .cloned()
                    .unwrap_or_else(|| r.ident.to_string());
                if !prefix.iter().any(|s| is_mockish(s)) && !is_mockish(&r.ident.to_string()) {
                    self.imports.insert(r.rename.to_string(), root);
                }
            }
            UseTree::Glob(_) => {
                if let Some(root) = prefix.first()
                    && self.is_real_root(root)
                    && !prefix.iter().any(|s| is_mockish(s))
                {
                    self.glob_real = true;
                }
            }
            UseTree::Group(g) => {
                for t in &g.items {
                    self.collect_use(t, prefix);
                }
            }
        }
    }

    fn is_real_root(&self, root: &str) -> bool {
        matches!(root, "crate" | "super" | "self")
            || root == self.crate_ident
            || (!STD_ROOTS.contains(&root) && !TEST_LIBS.contains(&root) && !is_mockish(root))
    }

    /// AC10: does this path name an item of the crate under test?
    fn is_real_path(&self, segs: &[String]) -> bool {
        let Some(first) = segs.first() else {
            return false;
        };
        if segs.iter().any(|s| is_mockish(s)) {
            return false;
        }
        if matches!(first.as_str(), "crate" | "super" | "self") || *first == self.crate_ident {
            return true;
        }
        if let Some(root) = self.imports.get(first) {
            return self.is_real_root(root);
        }
        if self.locals.contains(first) {
            return true;
        }
        if PRELUDE.contains(&first.as_str()) || STD_ROOTS.contains(&first.as_str()) {
            return false;
        }
        if first == "Self" {
            return false;
        }
        // `other_crate::item(..)` without a `use`: an extern crate path.
        if segs.len() >= 2
            && first
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
            && self.is_real_root(first)
        {
            return true;
        }
        self.glob_real
    }

    // ----- literals -----

    fn on_lit(&mut self, lit: &Lit, negative: bool) {
        let Some(value) = lit_value(lit, negative) else {
            return;
        };
        let at = line(lit.span());
        if self.in_test() {
            if self.assert_depth > 0 {
                self.model.assert_lits.insert(value);
            }
            return;
        }
        if value.is_fixture_worthy() {
            self.event_with(
                FIXTURE_CONSTANT,
                format!("lit:{}", value.key()),
                at,
                format!(
                    "literal {} in non-test code equals a base test expectation",
                    value.show()
                ),
                Some(value.clone()),
                Filter::FixtureLit,
            );
        }
        if self.branch > 0 && value.is_branch_worthy() {
            self.event_with(
                BRANCH_ON_TEST_INPUT,
                format!("branch:{}", value.key()),
                at,
                format!(
                    "non-test code branches on {}, an argument of a base test call",
                    value.show()
                ),
                Some(value.clone()),
                Filter::BranchLit,
            );
        }
        if let LitVal::Str(s) = &value
            && is_test_env_var(s)
        {
            self.event(
                BRANCH_ON_TEST_INPUT,
                format!("env:{s}"),
                at,
                format!("non-test code reads the test-runner env var {s}"),
            );
        }
    }

    fn scan_tokens(&mut self, ts: &TokenStream) {
        let mut prev_minus = false;
        for tt in ts.clone() {
            match &tt {
                TokenTree::Literal(l) => {
                    let lit = Lit::new(l.clone());
                    self.on_lit(&lit, prev_minus);
                    prev_minus = false;
                }
                TokenTree::Group(g) => {
                    self.scan_tokens(&g.stream());
                    prev_minus = false;
                }
                TokenTree::Ident(i) => {
                    let s = i.to_string();
                    if is_mockish(&s)
                        && let Some(c) = self.collectors.last_mut()
                    {
                        c.mock_used = true;
                    }
                    prev_minus = false;
                }
                TokenTree::Punct(p) => prev_minus = p.as_char() == '-',
            }
        }
    }

    fn visit_macro_args(&mut self, tokens: &TokenStream) {
        match Punctuated::<Expr, syn::Token![,]>::parse_terminated.parse2(tokens.clone()) {
            Ok(args) => {
                for a in &args {
                    self.visit_expr(a);
                }
            }
            Err(_) => self.scan_tokens(tokens),
        }
    }

    // ----- tests and helpers -----

    fn visit_test_body(&mut self, block: &Block) -> Collector {
        self.collectors.push(Collector::default());
        self.test_depth += 1;
        self.visit_block(block);
        self.test_depth -= 1;
        self.collectors.pop().unwrap_or_default()
    }

    fn on_test_fn(&mut self, attrs: &[Attribute], sig: &Signature, block: &Block, start: u32) {
        let name = sig.ident.to_string();
        let end = line(block.brace_token.span.close());
        if !self.in_test() {
            self.model
                .test_blocks
                .push((start, end, format!("test fn {name}")));
        }
        self.test_attrs(attrs, &name);
        if let Some(Stmt::Expr(Expr::Return(r), _)) = block.stmts.first() {
            self.event(
                SKIP_ADDED,
                format!("early_return:{name}"),
                line(r.return_token.span),
                format!("test `{name}` returns before its first statement"),
            );
        }
        let body = self.norm(block.brace_token.span.join());
        let c = self.visit_test_body(block);
        self.model.tests.push(TestItem {
            name,
            line: start,
            fn_line: line(sig.fn_token.span),
            asserts: c.asserts,
            calls: c.calls,
            body,
            unwraps: c.unwraps,
            oks: c.oks,
            mock_used: c.mock_used,
            real_call: c.real_call,
        });
    }

    fn test_attrs(&mut self, attrs: &[Attribute], name: &str) {
        for a in attrs {
            let at = line(a.pound_token.span);
            let p = path_string(a.path());
            let tokens = attr_tokens(a);
            if p == "ignore" || (p == "cfg_attr" && words(&tokens).contains(&"ignore")) {
                self.event(
                    SKIP_ADDED,
                    format!("ignore:{name}"),
                    at,
                    format!("#[ignore] on test `{name}`"),
                );
            } else if p == "should_panic" {
                self.event(
                    ASSERT_WEAKENED,
                    format!("should_panic:{name}"),
                    at,
                    format!("#[should_panic] added to test `{name}`"),
                );
            } else if p == "cfg" && cfg_disables(&tokens) {
                let shown = self.show(a.span());
                self.event(
                    SKIP_ADDED,
                    format!("cfg_off:{name}:{tokens}"),
                    at,
                    format!("`{shown}` disables test `{name}`"),
                );
            }
        }
    }

    fn mutants_skip(&mut self, attrs: &[Attribute], name: &str) {
        for a in attrs {
            let p = path_string(a.path());
            let tokens = attr_tokens(a);
            if p == "mutants::skip" || (p == "cfg_attr" && tokens.contains("mutants :: skip")) {
                self.event(
                    SKIP_ADDED,
                    format!("mutants_skip:{name}"),
                    line(a.pound_token.span),
                    format!("#[mutants::skip] on `{name}`"),
                );
            }
        }
    }

    fn on_fn(&mut self, attrs: &[Attribute], sig: &Signature, block: &Block, start: u32) {
        let name = sig.ident.to_string();
        self.mutants_skip(attrs, &name);
        if returns_result(&sig.output) {
            self.model.result_fns.insert(name.clone());
        }
        if attrs.iter().any(is_test_attr) {
            self.on_test_fn(attrs, sig, block, start);
            return;
        }
        let cfg_test = has_cfg_test(attrs);
        if self.in_test() || cfg_test {
            let body = self.norm(block.brace_token.span.join());
            self.collectors.push(Collector::default());
            self.test_depth += 1;
            self.visit_block(block);
            self.test_depth -= 1;
            let c = self.collectors.pop().unwrap_or_default();
            self.model.helpers.push(Helper {
                name,
                line: start,
                body,
                asserts: c.asserts,
            });
            return;
        }
        let qual = match self.impl_names.last() {
            Some(t) => format!("{t}::{name}"),
            None => name,
        };
        self.model.try_counts.entry(qual.clone()).or_insert(0);
        self.fn_names.push(qual);
        // A non-test fn body: no collector, so test-only facts stay empty.
        let saved = std::mem::take(&mut self.collectors);
        self.visit_block(block);
        self.collectors = saved;
        self.fn_names.pop();
    }

    // ----- cfg switches on non-test items (AC07 / AC08) -----

    fn cfg_item(&mut self, attrs: &[Attribute], name: &str, is_use: bool, span: Span) {
        if self.in_test() {
            return;
        }
        let mut found = false;
        for a in attrs {
            if path_string(a.path()) != "cfg" {
                continue;
            }
            let tokens = attr_tokens(a);
            if !words(&tokens).contains(&"test") {
                continue;
            }
            found = true;
            let negative = tokens.replace(' ', "").contains("not(test)");
            let text = self.norm(span);
            let time_hash = is_time_hash(name) || is_time_hash(&text);
            self.cfg_items.push(CfgItem {
                name: name.to_string(),
                is_use,
                line: line(a.pound_token.span),
                positive: !negative,
                negative,
                time_hash,
                text: clip(&text, 160),
            });
        }
        if !found && !name.is_empty() {
            self.plain_names.insert(name.to_string());
        }
    }

    fn finish_cfg_items(&mut self) {
        let items = std::mem::take(&mut self.cfg_items);
        let mut group_time_hash: BTreeSet<&str> = BTreeSet::new();
        let mut positives: BTreeSet<&str> = BTreeSet::new();
        let mut negatives: BTreeSet<&str> = BTreeSet::new();
        for it in &items {
            if it.time_hash {
                group_time_hash.insert(&it.name);
            }
            if it.positive {
                positives.insert(&it.name);
            }
            if it.negative {
                negatives.insert(&it.name);
            }
        }
        let mut out = Vec::new();
        for it in &items {
            let th = group_time_hash.contains(it.name.as_str());
            if it.is_use {
                if it.time_hash {
                    out.push((
                        MONKEY_PATCH,
                        format!("cfg_use:{}", it.text),
                        it,
                        "a cfg(test) import replaces a time/hash item",
                    ));
                }
                continue;
            }
            if it.positive {
                if th {
                    out.push((
                        MONKEY_PATCH,
                        format!("shim:{}", it.name),
                        it,
                        "a cfg(test) shim overrides time/hash for non-test code",
                    ));
                } else if negatives.contains(it.name.as_str())
                    || self.plain_names.contains(&it.name)
                {
                    out.push((
                        BRANCH_ON_TEST_INPUT,
                        format!("cfg_switch:{}", it.name),
                        it,
                        "cfg(test) switches the behaviour of a non-test item",
                    ));
                }
            } else if it.negative {
                if th {
                    if !positives.contains(it.name.as_str()) {
                        out.push((
                            MONKEY_PATCH,
                            format!("shim:{}", it.name),
                            it,
                            "a cfg(not(test)) item hides time/hash behaviour from tests",
                        ));
                    }
                } else {
                    out.push((
                        BRANCH_ON_TEST_INPUT,
                        format!("cfg_not_test:{}", it.name),
                        it,
                        "cfg(not(test)) changes behaviour under test",
                    ));
                }
            }
        }
        for (rule, key, it, why) in out {
            let detail = format!("{why}: `{}`", it.name);
            self.event(rule, key, it.line, detail);
        }
    }

    fn stmt_cfg(&mut self, attrs: &[Attribute], span: Span) {
        if self.in_test() || self.fn_names.is_empty() {
            return;
        }
        for a in attrs {
            let p = path_string(a.path());
            let tokens = attr_tokens(a);
            if (p == "cfg" || p == "cfg_attr") && words(&tokens).contains(&"test") {
                let text = self.norm(span);
                let (rule, why) = if is_time_hash(&text) {
                    (MONKEY_PATCH, "a cfg(test) statement overrides time/hash")
                } else {
                    (
                        BRANCH_ON_TEST_INPUT,
                        "a cfg(test) statement changes behaviour under test",
                    )
                };
                let key = format!("cfg_stmt:{}:{text}", self.fn_name());
                self.event(
                    rule,
                    key,
                    line(a.pound_token.span),
                    format!("{why} in `{}`", self.fn_name()),
                );
            }
        }
    }

    // ----- error swallowing (AC09) -----

    fn err_arm(&mut self, arm: &Arm) {
        let Pat::TupleStruct(ts) = &arm.pat else {
            return;
        };
        if ts.path.segments.last().is_none_or(|s| s.ident != "Err") {
            return;
        }
        if !swallowing_body(&arm.body) {
            return;
        }
        let text = self.norm(arm.span());
        let shown = self.show(arm.span());
        let key = format!("err_arm:{}:{text}", self.fn_name());
        self.event(
            ERROR_SWALLOWED,
            key,
            line(arm.pat.span()),
            format!(
                "`{}` swallows the error in `{}`",
                clip(&shown, 120),
                self.fn_name()
            ),
        );
    }
}

impl<'ast> Visit<'ast> for Walker<'_> {
    fn visit_attribute(&mut self, _: &'ast Attribute) {}

    fn visit_item(&mut self, item: &'ast Item) {
        let (attrs, name, is_use): (&[Attribute], String, bool) = match item {
            Item::Fn(f) if !f.attrs.iter().any(is_test_attr) => {
                (&f.attrs, f.sig.ident.to_string(), false)
            }
            Item::Const(c) => (&c.attrs, c.ident.to_string(), false),
            Item::Static(s) => (&s.attrs, s.ident.to_string(), false),
            Item::Struct(s) => (&s.attrs, s.ident.to_string(), false),
            Item::Enum(e) => (&e.attrs, e.ident.to_string(), false),
            Item::Type(t) => (&t.attrs, t.ident.to_string(), false),
            Item::Impl(i) => (&i.attrs, type_name(&i.self_ty), false),
            Item::Use(u) => (&u.attrs, String::new(), true),
            _ => (&[], String::new(), false),
        };
        if !attrs.is_empty() || !name.is_empty() {
            self.cfg_item(attrs, &name, is_use, item.span());
        }
        visit::visit_item(self, item);
    }

    fn visit_item_fn(&mut self, f: &'ast ItemFn) {
        let start = item_start(&f.attrs, f.sig.span());
        self.on_fn(&f.attrs, &f.sig, &f.block, start);
    }

    fn visit_impl_item_fn(&mut self, f: &'ast ImplItemFn) {
        let start = item_start(&f.attrs, f.sig.span());
        self.on_fn(&f.attrs, &f.sig, &f.block, start);
    }

    fn visit_trait_item_fn(&mut self, f: &'ast TraitItemFn) {
        if returns_result(&f.sig.output) {
            self.model.result_fns.insert(f.sig.ident.to_string());
        }
        if let Some(block) = &f.default {
            let start = item_start(&f.attrs, f.sig.span());
            self.on_fn(&f.attrs, &f.sig, block, start);
        }
    }

    fn visit_item_impl(&mut self, i: &'ast ItemImpl) {
        let name = type_name(&i.self_ty);
        if (self.in_test() || has_cfg_test(&i.attrs))
            && let Some((_, tr, _)) = &i.trait_
            && let Some(seg) = tr.segments.last()
            && matches!(seg.ident.to_string().as_str(), "Hasher" | "BuildHasher")
        {
            self.event(
                MONKEY_PATCH,
                format!("impl_hash:{name}:{}", seg.ident),
                line(i.impl_token.span),
                format!("test code implements {} for `{name}`", seg.ident),
            );
        }
        let cfg_test = has_cfg_test(&i.attrs);
        if cfg_test {
            self.test_depth += 1;
        }
        self.impl_names.push(name);
        visit::visit_item_impl(self, i);
        self.impl_names.pop();
        if cfg_test {
            self.test_depth -= 1;
        }
    }

    fn visit_item_mod(&mut self, m: &'ast ItemMod) {
        let cfg_test = has_cfg_test(&m.attrs);
        let name = m.ident.to_string();
        let start = item_start(&m.attrs, m.mod_token.span);
        for a in &m.attrs {
            let p = path_string(a.path());
            let tokens = attr_tokens(a);
            if p == "cfg" && cfg_disables(&tokens) && (self.in_test() || name.contains("test")) {
                self.event(
                    SKIP_ADDED,
                    format!("cfg_off_mod:{name}:{tokens}"),
                    line(a.pound_token.span),
                    format!("`{}` disables test module `{name}`", self.show(a.span())),
                );
            }
        }
        let test_code = self.in_test() || cfg_test;
        let dir = self
            .path
            .rsplit_once('/')
            .map_or("", |(d, _)| d)
            .to_string();
        if m.content.is_none() {
            if let Some(target) = path_attr(&m.attrs) {
                self.model
                    .path_mods
                    .push((normalize(&format!("{dir}/{target}")), test_code));
            } else if test_code {
                let stem = crate::paths::file_name(&self.path)
                    .trim_end_matches(".rs")
                    .to_string();
                for cand in [
                    format!("{dir}/{name}.rs"),
                    format!("{dir}/{name}/mod.rs"),
                    format!("{dir}/{stem}/{name}.rs"),
                ] {
                    self.model.path_mods.push((normalize(&cand), true));
                }
            }
        }
        if let Some((brace, _)) = &m.content
            && cfg_test
            && !self.in_test()
        {
            self.model.test_blocks.push((
                start,
                line(brace.span.close()),
                format!("#[cfg(test)] mod {name}"),
            ));
        }
        if cfg_test {
            self.test_depth += 1;
        }
        visit::visit_item_mod(self, m);
        if cfg_test {
            self.test_depth -= 1;
        }
    }

    fn visit_item_use(&mut self, u: &'ast ItemUse) {
        let at = line(u.use_token.span);
        let mut renames = Vec::new();
        let mut paths = Vec::new();
        use_walk(&u.tree, &mut Vec::new(), &mut renames, &mut paths);
        for (from, to) in renames {
            let from_leaf = from.rsplit("::").next().unwrap_or(&from).to_string();
            if from_leaf == to {
                continue;
            }
            if is_assert_name(&to) {
                self.event(
                    ASSERT_WEAKENED,
                    format!("use_as:{from}:{to}"),
                    at,
                    format!("`use {from} as {to}` replaces an assert macro"),
                );
            }
            if matches!(
                to.as_str(),
                "SystemTime" | "Instant" | "DefaultHasher" | "RandomState" | "Utc" | "Local"
            ) {
                self.event(
                    MONKEY_PATCH,
                    format!("use_as:{from}:{to}"),
                    at,
                    format!("`use {from} as {to}` replaces a time/hash item"),
                );
            }
        }
        for p in paths {
            if p.split("::")
                .any(|s| s == "mock_instant" || s == "fake_clock" || s == "sn_fake_clock")
            {
                self.event(
                    MONKEY_PATCH,
                    format!("mock_clock_use:{p}"),
                    at,
                    format!("`use {p}` fakes the clock"),
                );
            }
        }
    }

    fn visit_item_macro(&mut self, m: &'ast ItemMacro) {
        let name = path_string(&m.mac.path);
        if name == "macro_rules" {
            if let Some(ident) = &m.ident {
                let mname = ident.to_string();
                if self.in_test()
                    && (is_assert_name(&mname) || mname.starts_with("check"))
                    && !tokens_have_check(&m.mac.tokens)
                {
                    let body = m.mac.tokens.to_string();
                    self.event(
                        ASSERT_WEAKENED,
                        format!("macro:{mname}:{body}"),
                        line(m.mac.path.span()),
                        format!("macro `{mname}!` in test code checks nothing (no panic!/assert)"),
                    );
                }
            }
            return;
        }
        if name == "proptest" || name.ends_with("::proptest") {
            self.proptest(&m.mac.tokens);
            return;
        }
        self.visit_macro(&m.mac);
    }

    fn visit_macro(&mut self, m: &'ast Macro) {
        let name = m
            .path
            .segments
            .last()
            .map(|s| s.ident.to_string())
            .unwrap_or_default();
        let at = line(m.path.span());
        if is_assert_name(&name) {
            let shown = self.show(m.span());
            if let Some(c) = self.collectors.last_mut() {
                c.asserts.push(assert_info(&name, &m.tokens, at, &shown));
            }
            self.assert_depth += 1;
            self.visit_macro_args(&m.tokens);
            self.assert_depth -= 1;
            return;
        }
        if name == "cfg" && !self.in_test() && words(&m.tokens.to_string()).contains(&"test") {
            let key = format!("cfg_macro:{}:{}", self.fn_name(), m.tokens);
            self.event(
                BRANCH_ON_TEST_INPUT,
                key,
                at,
                format!("cfg!({}) switches non-test code under test", m.tokens),
            );
            return;
        }
        if name == "matches" {
            self.branch += 1;
            self.visit_macro_args(&m.tokens);
            self.branch -= 1;
            return;
        }
        self.visit_macro_args(&m.tokens);
    }

    fn visit_ident(&mut self, i: &'ast proc_macro2::Ident) {
        if let Some(c) = self.collectors.last_mut()
            && is_mockish(&i.to_string())
        {
            c.mock_used = true;
        }
    }

    fn visit_expr(&mut self, e: &'ast Expr) {
        match e {
            Expr::Lit(l) => self.on_lit(&l.lit, false),
            Expr::Unary(u) if matches!(u.op, UnOp::Neg(_)) => {
                if let Expr::Lit(l) = &*u.expr {
                    self.on_lit(&l.lit, true);
                } else {
                    visit::visit_expr(self, e);
                }
            }
            _ => visit::visit_expr(self, e),
        }
    }

    fn visit_pat(&mut self, p: &'ast Pat) {
        if let Pat::Lit(l) = p {
            self.on_lit(&l.lit, false);
            return;
        }
        visit::visit_pat(self, p);
    }

    fn visit_expr_if(&mut self, e: &'ast ExprIf) {
        if !self.in_test()
            && let Expr::Let(l) = &*e.cond
            && let Pat::TupleStruct(ts) = &*l.pat
            && ts.path.segments.last().is_some_and(|s| s.ident == "Err")
            && e.then_branch.stmts.is_empty()
            && e.else_branch.is_none()
        {
            let text = self.norm(e.span());
            let key = format!("err_if:{}:{text}", self.fn_name());
            self.event(
                ERROR_SWALLOWED,
                key,
                line(e.if_token.span),
                format!(
                    "`if let Err(..) = .. {{}}` swallows the error in `{}`",
                    self.fn_name()
                ),
            );
        }
        self.branch += 1;
        self.visit_expr(&e.cond);
        self.branch -= 1;
        self.visit_block(&e.then_branch);
        if let Some((_, els)) = &e.else_branch {
            self.visit_expr(els);
        }
    }

    fn visit_arm(&mut self, arm: &'ast Arm) {
        if !self.in_test() {
            self.err_arm(arm);
        }
        self.branch += 1;
        self.visit_pat(&arm.pat);
        if let Some((_, g)) = &arm.guard {
            self.visit_expr(g);
        }
        self.branch -= 1;
        self.visit_expr(&arm.body);
    }

    fn visit_expr_binary(&mut self, b: &'ast syn::ExprBinary) {
        let cmp = matches!(b.op, BinOp::Eq(_) | BinOp::Ne(_));
        for side in [&*b.left, &*b.right] {
            let direct = cmp && direct_lit(side).is_some();
            if direct {
                self.branch += 1;
            }
            self.visit_expr(side);
            if direct {
                self.branch -= 1;
            }
        }
    }

    fn visit_expr_call(&mut self, c: &'ast ExprCall) {
        if let Expr::Path(p) = &*c.func {
            let segs: Vec<String> = p
                .path
                .segments
                .iter()
                .map(|s| s.ident.to_string())
                .collect();
            let leaf = segs.last().cloned().unwrap_or_default();
            if self.in_test() {
                let real = self.is_real_path(&segs);
                if let Some(col) = self.collectors.last_mut() {
                    col.calls.insert(leaf.clone());
                    if real {
                        col.real_call = true;
                    }
                }
                for a in &c.args {
                    if let Some(v) = direct_lit(a) {
                        self.model.call_lits.insert(v);
                    }
                }
            } else {
                self.on_call_name(&leaf, line(p.span()));
            }
        }
        visit::visit_expr_call(self, c);
    }

    fn visit_expr_method_call(&mut self, m: &'ast ExprMethodCall) {
        let name = m.method.to_string();
        let at = line(m.method.span());
        if self.in_test() {
            if let Some(c) = self.collectors.last_mut() {
                match name.as_str() {
                    "unwrap" | "expect" | "unwrap_err" | "expect_err" => c.unwraps += 1,
                    "ok" if m.args.is_empty() => c.oks.push(at),
                    _ => {}
                }
            }
            for a in &m.args {
                if let Some(v) = direct_lit(a) {
                    self.model.call_lits.insert(v);
                }
            }
        } else {
            if name == "unwrap_or_default" {
                let recv = call_name(&m.receiver);
                let text = self.norm(m.receiver.span());
                let f = self.fn_name();
                self.event_with(
                    ERROR_SWALLOWED,
                    format!("uod:{f}:{text}"),
                    at,
                    format!(
                        "`.unwrap_or_default()` on `{}` drops the error in `{f}`",
                        clip(&self.show(m.receiver.span()), 100)
                    ),
                    None,
                    Filter::UnwrapOrDefault(f.clone(), recv),
                );
            }
            self.on_call_name(&name, at);
        }
        visit::visit_expr_method_call(self, m);
    }

    fn visit_expr_struct(&mut self, s: &'ast ExprStruct) {
        if self.in_test() {
            let segs: Vec<String> = s
                .path
                .segments
                .iter()
                .map(|x| x.ident.to_string())
                .collect();
            if self.is_real_path(&segs)
                && let Some(c) = self.collectors.last_mut()
            {
                c.real_call = true;
            }
        }
        visit::visit_expr_struct(self, s);
    }

    fn visit_expr_try(&mut self, t: &'ast ExprTry) {
        if !self.in_test()
            && let Some(f) = self.fn_names.last()
        {
            *self.model.try_counts.entry(f.clone()).or_insert(0) += 1;
        }
        visit::visit_expr_try(self, t);
    }

    fn visit_stmt(&mut self, s: &'ast Stmt) {
        if !self.in_test() && !self.fn_names.is_empty() {
            match s {
                Stmt::Local(l) => {
                    self.stmt_cfg(&l.attrs, s.span());
                    if matches!(l.pat, Pat::Wild(_))
                        && let Some(init) = &l.init
                        && let Some(name) = call_name(&init.expr)
                    {
                        let text = self.norm(s.span());
                        self.event_with(
                            ERROR_SWALLOWED,
                            format!("let_:{text}"),
                            line(l.let_token.span),
                            format!(
                                "`{}` discards the Result of `{name}`",
                                clip(&self.show(s.span()), 120)
                            ),
                            None,
                            Filter::ResultName(name),
                        );
                    }
                }
                Stmt::Expr(e, semi) => {
                    self.stmt_cfg(expr_attrs(e), s.span());
                    if semi.is_some()
                        && let Expr::MethodCall(m) = e
                        && m.method == "ok"
                        && m.args.is_empty()
                    {
                        let text = self.norm(s.span());
                        self.event(
                            ERROR_SWALLOWED,
                            format!("ok_stmt:{text}"),
                            line(m.method.span()),
                            format!(
                                "`{}` throws the error away",
                                clip(&self.show(s.span()), 120)
                            ),
                        );
                    }
                }
                Stmt::Macro(m) => self.stmt_cfg(&m.attrs, s.span()),
                Stmt::Item(_) => {}
            }
        }
        visit::visit_stmt(self, s);
    }
}

impl Walker<'_> {
    fn on_call_name(&mut self, name: &str, at: u32) {
        if name == "catch_unwind" {
            let f = self.fn_name();
            self.event(
                ERROR_SWALLOWED,
                format!("catch_unwind:{f}"),
                at,
                format!("`catch_unwind` in `{f}` turns a panic into a value"),
            );
        }
        if matches!(
            name,
            "with_seed" | "with_seeds" | "with_fixed_seed" | "fixed_seed"
        ) {
            let f = self.fn_name();
            self.event(
                MONKEY_PATCH,
                format!("seed:{f}:{name}"),
                at,
                format!("`{name}` fixes the hash/random seed in non-test code (`{f}`)"),
            );
        }
    }

    fn proptest(&mut self, tokens: &TokenStream) {
        let trees: Vec<TokenTree> = tokens.clone().into_iter().collect();
        let mut attr_start: Option<u32> = None;
        let mut i = 0;
        while i < trees.len() {
            match &trees[i] {
                TokenTree::Punct(p) if p.as_char() == '#' => {
                    if let Some(TokenTree::Group(g)) = trees.get(i + 1)
                        && g.delimiter() == Delimiter::Bracket
                        && !g.stream().to_string().starts_with("doc")
                        && attr_start.is_none()
                    {
                        attr_start = Some(line(p.span()));
                    }
                }
                TokenTree::Ident(id) if id == "fn" => {
                    if let Some(TokenTree::Ident(name)) = trees.get(i + 1) {
                        let start = attr_start.take().unwrap_or_else(|| line(id.span()));
                        let mut j = i + 2;
                        while j < trees.len() {
                            if let TokenTree::Group(g) = &trees[j]
                                && g.delimiter() == Delimiter::Brace
                            {
                                break;
                            }
                            j += 1;
                        }
                        if let Some(TokenTree::Group(g)) = trees.get(j)
                            && let Ok(block) =
                                syn::parse2::<Block>(TokenStream::from(TokenTree::Group(g.clone())))
                        {
                            let body = g.stream().to_string();
                            let end = line(g.span_close());
                            if !self.in_test() {
                                self.model.test_blocks.push((
                                    start,
                                    end,
                                    format!("proptest {name}"),
                                ));
                            }
                            let c = self.visit_test_body(&block);
                            self.model.tests.push(TestItem {
                                name: name.to_string(),
                                line: start,
                                fn_line: start,
                                asserts: c.asserts,
                                calls: c.calls,
                                body,
                                unwraps: c.unwraps,
                                oks: c.oks,
                                mock_used: c.mock_used,
                                real_call: c.real_call,
                            });
                        }
                        i = j;
                    }
                }
                TokenTree::Group(g) if g.delimiter() == Delimiter::Brace => {
                    // `proptest! { #![proptest_config(..)] ... }` nests nothing
                    // else we count; a config block is skipped.
                    attr_start = None;
                }
                _ => {}
            }
            i += 1;
        }
    }
}

// ----- helpers -----

fn lit_value(lit: &Lit, negative: bool) -> Option<LitVal> {
    match lit {
        Lit::Str(s) => Some(LitVal::Str(s.value())),
        Lit::ByteStr(b) => String::from_utf8(b.value()).ok().map(LitVal::Str),
        Lit::Int(i) => norm_number(i.base10_digits(), negative).map(LitVal::Num),
        Lit::Float(f) => norm_number(f.base10_digits(), negative).map(LitVal::Num),
        _ => None,
    }
}

fn direct_lit(e: &Expr) -> Option<LitVal> {
    match e {
        Expr::Lit(l) => lit_value(&l.lit, false),
        Expr::Unary(u) if matches!(u.op, UnOp::Neg(_)) => match &*u.expr {
            Expr::Lit(l) => lit_value(&l.lit, true),
            _ => None,
        },
        Expr::Reference(r) => direct_lit(&r.expr),
        Expr::Paren(p) => direct_lit(&p.expr),
        _ => None,
    }
}

/// The name of the call an expression ends in (`foo(..)`, `x.foo(..)`, `foo(..).await`).
fn call_name(e: &Expr) -> Option<String> {
    match e {
        Expr::Call(c) => match &*c.func {
            Expr::Path(p) => p.path.segments.last().map(|s| s.ident.to_string()),
            _ => None,
        },
        Expr::MethodCall(m) => Some(m.method.to_string()),
        Expr::Await(a) => call_name(&a.base),
        Expr::Paren(p) => call_name(&p.expr),
        _ => None,
    }
}

fn swallowing_body(e: &Expr) -> bool {
    match e {
        Expr::Block(b) => b.block.stmts.is_empty(),
        Expr::Tuple(t) => t.elems.is_empty(),
        Expr::Return(r) => r.expr.as_deref().is_none_or(is_ok_call),
        e => is_ok_call(e),
    }
}

fn is_ok_call(e: &Expr) -> bool {
    matches!(e, Expr::Call(c) if matches!(&*c.func, Expr::Path(p) if p.path.is_ident("Ok")))
}

fn expr_attrs(e: &Expr) -> &[Attribute] {
    match e {
        Expr::Block(x) => &x.attrs,
        Expr::Call(x) => &x.attrs,
        Expr::MethodCall(x) => &x.attrs,
        Expr::Return(x) => &x.attrs,
        Expr::If(x) => &x.attrs,
        Expr::Macro(x) => &x.attrs,
        Expr::Assign(x) => &x.attrs,
        Expr::Match(x) => &x.attrs,
        _ => &[],
    }
}

fn path_string(p: &syn::Path) -> String {
    p.segments
        .iter()
        .map(|s| s.ident.to_string())
        .collect::<Vec<_>>()
        .join("::")
}

fn attr_tokens(a: &Attribute) -> String {
    match &a.meta {
        syn::Meta::List(l) => l.tokens.to_string(),
        syn::Meta::NameValue(nv) => match &nv.value {
            Expr::Lit(l) => match &l.lit {
                Lit::Str(s) => s.value(),
                _ => String::new(),
            },
            _ => String::new(),
        },
        syn::Meta::Path(_) => String::new(),
    }
}

fn path_attr(attrs: &[Attribute]) -> Option<String> {
    attrs
        .iter()
        .find(|a| a.path().is_ident("path"))
        .map(attr_tokens)
        .filter(|s| !s.is_empty())
}

fn is_test_attr(a: &Attribute) -> bool {
    let p = a.path();
    let last = p
        .segments
        .last()
        .map(|s| s.ident.to_string())
        .unwrap_or_default();
    last == "test"
        || matches!(
            last.as_str(),
            "rstest" | "test_case" | "quickcheck" | "wasm_bindgen_test" | "test_log"
        )
}

/// `#[cfg(test)]`, `#[cfg(all(test, ..))]`, `#[cfg(any(test, ..))]` — not `not(test)`.
fn has_cfg_test(attrs: &[Attribute]) -> bool {
    attrs.iter().any(|a| {
        a.path().is_ident("cfg") && {
            let t = attr_tokens(a).replace(' ', "");
            (t == "test" || t.contains("(test") || t.contains(",test")) && !t.contains("not(test)")
        }
    })
}

/// Identifier-like words of a token string (`not (test)` → `not`, `test`).
fn words(s: &str) -> Vec<&str> {
    s.split(|c: char| !c.is_alphanumeric() && c != '_')
        .filter(|w| !w.is_empty())
        .collect()
}

/// A cfg predicate that never holds in a test build.
fn cfg_disables(tokens: &str) -> bool {
    let t = tokens.replace(' ', "");
    t.contains("not(test)")
        || t.contains("any()")
        || t.split(|c: char| !c.is_alphanumeric() && c != '_')
            .any(|w| {
                matches!(
                    w,
                    "FALSE" | "false" | "never" | "disabled" | "disable" | "nope" | "off"
                )
            })
}

fn is_assert_name(name: &str) -> bool {
    name.starts_with("assert")
        || name.starts_with("debug_assert")
        || name.starts_with("prop_assert")
}

fn tokens_have_check(ts: &TokenStream) -> bool {
    ts.clone().into_iter().any(|tt| match tt {
        TokenTree::Ident(i) => {
            let s = i.to_string();
            s == "panic"
                || s == "unreachable"
                || s == "todo"
                || s == "unimplemented"
                || is_assert_name(&s)
        }
        TokenTree::Group(g) => tokens_have_check(&g.stream()),
        _ => false,
    })
}

fn returns_result(out: &ReturnType) -> bool {
    match out {
        ReturnType::Type(_, ty) => match &**ty {
            Type::Path(p) => p
                .path
                .segments
                .last()
                .is_some_and(|s| s.ident.to_string().ends_with("Result")),
            _ => false,
        },
        ReturnType::Default => false,
    }
}

fn type_name(t: &Type) -> String {
    match t {
        Type::Path(p) => p
            .path
            .segments
            .last()
            .map(|s| s.ident.to_string())
            .unwrap_or_default(),
        _ => String::new(),
    }
}

fn item_start(attrs: &[Attribute], kw: Span) -> u32 {
    attrs
        .iter()
        .filter(|a| !a.path().is_ident("doc"))
        .map(|a| line(a.pound_token.span))
        .chain(std::iter::once(line(kw)))
        .min()
        .unwrap_or(0)
}

fn is_time_hash(text: &str) -> bool {
    TIME_HASH_WORDS.iter().any(|w| {
        text.split(|c: char| !c.is_alphanumeric() && c != '_' && c != ':')
            .any(|tok| {
                let tok_l = tok.to_ascii_lowercase();
                tok == *w
                    || tok.ends_with(&format!("::{w}"))
                    || (w.chars().all(|c| c.is_ascii_lowercase())
                        && tok_l.split('_').any(|p| p == *w))
            })
    })
}

fn use_walk(
    tree: &UseTree,
    prefix: &mut Vec<String>,
    renames: &mut Vec<(String, String)>,
    paths: &mut Vec<String>,
) {
    match tree {
        UseTree::Path(p) => {
            prefix.push(p.ident.to_string());
            use_walk(&p.tree, prefix, renames, paths);
            prefix.pop();
        }
        UseTree::Name(n) => {
            let mut full = prefix.clone();
            full.push(n.ident.to_string());
            paths.push(full.join("::"));
        }
        UseTree::Rename(r) => {
            let mut full = prefix.clone();
            full.push(r.ident.to_string());
            let full = full.join("::");
            paths.push(full.clone());
            renames.push((full, r.rename.to_string()));
        }
        UseTree::Glob(_) => paths.push(format!("{}::*", prefix.join("::"))),
        UseTree::Group(g) => {
            for t in &g.items {
                use_walk(t, prefix, renames, paths);
            }
        }
    }
}

/// Split a token stream at top-level commas.
fn split_args(ts: &TokenStream) -> Vec<Vec<TokenTree>> {
    let mut out = vec![Vec::new()];
    for tt in ts.clone() {
        if let TokenTree::Punct(p) = &tt
            && p.as_char() == ','
        {
            out.push(Vec::new());
            continue;
        }
        if let Some(last) = out.last_mut() {
            last.push(tt);
        }
    }
    if out.last().is_some_and(Vec::is_empty) {
        out.pop();
    }
    out
}

fn tts_string(tts: &[TokenTree]) -> String {
    tts.iter().cloned().collect::<TokenStream>().to_string()
}

/// Split a condition at its first top-level comparison operator.
fn split_cond(tts: &[TokenTree]) -> (AssertKind, Vec<String>) {
    let mut ops: Vec<(String, usize, usize)> = Vec::new();
    let mut i = 0;
    while i < tts.len() {
        if let TokenTree::Punct(p) = &tts[i] {
            let start = i;
            let mut op = String::from(p.as_char());
            let mut spacing = p.spacing();
            while spacing == proc_macro2::Spacing::Joint {
                match tts.get(i + 1) {
                    Some(TokenTree::Punct(q)) => {
                        op.push(q.as_char());
                        spacing = q.spacing();
                        i += 1;
                    }
                    _ => break,
                }
            }
            ops.push((op, start, i + 1));
        }
        i += 1;
    }
    if ops.iter().any(|(o, _, _)| o == "&&" || o == "||") {
        return (AssertKind::Bool, vec![tts_string(tts)]);
    }
    for (op, s, e) in &ops {
        let kind = match op.as_str() {
            "==" => AssertKind::Eq,
            "!=" => AssertKind::Ne,
            "<" | ">" | "<=" | ">=" => AssertKind::Cmp,
            _ => continue,
        };
        let left = tts_string(&tts[..*s]);
        let right = tts_string(&tts[*e..]);
        if kind == AssertKind::Cmp {
            return (kind, vec![left, op.clone(), right]);
        }
        return (kind, vec![left, right]);
    }
    (AssertKind::Bool, vec![tts_string(tts)])
}

fn assert_info(name: &str, tokens: &TokenStream, at: u32, shown: &str) -> AssertInfo {
    let args = split_args(tokens);
    let ops: Vec<String> = args.iter().map(|a| tts_string(a)).collect();
    let text = if shown.is_empty() {
        format!("{name}!({tokens})")
    } else {
        shown.to_string()
    };
    let base = name
        .trim_start_matches("debug_")
        .trim_start_matches("prop_");
    if base.ends_with("_eq") && ops.len() >= 2 {
        return AssertInfo::new(at, AssertKind::Eq, &ops[..2], &text);
    }
    if base.ends_with("_ne") && ops.len() >= 2 {
        return AssertInfo::new(at, AssertKind::Ne, &ops[..2], &text);
    }
    if base == "assert" && !args.is_empty() {
        let (kind, parts) = split_cond(&args[0]);
        if kind == AssertKind::Cmp && parts.len() == 3 {
            let mut info = AssertInfo::new(
                at,
                AssertKind::Cmp,
                &[parts[0].clone(), parts[2].clone()],
                &text,
            );
            if info.kind == AssertKind::Cmp {
                info.key = format!("cmp:{}|{}|{}", parts[0], parts[1], parts[2]);
            }
            return info;
        }
        return AssertInfo::new(at, kind, &parts, &text);
    }
    let mut info = AssertInfo::new(at, AssertKind::Other, &ops, &text);
    info.key = format!("raw:{name}:{}", ops.join("|"));
    info
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model(src: &str, path: &str) -> FileModel {
        let f = parse(src).expect("parses");
        build(
            &f,
            src,
            path,
            crate::paths::is_test_or_grader_path(path),
            "x",
        )
    }

    #[test]
    fn finds_tests_asserts_and_lines() {
        let src = "#[cfg(test)]\nmod tests {\n    use super::*;\n\n    #[test]\n    fn adds() {\n        assert_eq!(add(2, 3), 5);\n        assert!(add(1, 1) == 2, \"msg\");\n    }\n}\n";
        let m = model(src, "src/x.rs");
        assert_eq!(m.tests.len(), 1);
        let t = &m.tests[0];
        assert_eq!((t.name.as_str(), t.line), ("adds", 5));
        assert_eq!(t.asserts.len(), 2);
        assert_eq!(t.asserts[0].line, 7);
        assert_eq!(t.asserts[1].kind, AssertKind::Eq);
        assert!(t.real_call);
        assert_eq!(
            m.test_blocks,
            vec![(1, 10, "#[cfg(test)] mod tests".to_string())]
        );
        assert!(m.assert_lits.contains(&LitVal::Num("5".into())));
    }

    #[test]
    fn eq_and_bool_eq_share_a_key() {
        let a = assert_info(
            "assert_eq",
            &TokenStream::from_str("a, b").expect("ts"),
            1,
            "",
        );
        let b = assert_info(
            "assert",
            &TokenStream::from_str("a == b").expect("ts"),
            1,
            "",
        );
        let c = assert_info(
            "assert_eq",
            &TokenStream::from_str("a, b, \"m\"").expect("ts"),
            1,
            "",
        );
        assert_eq!(a.key, b.key);
        assert_eq!(a.key, c.key);
    }

    #[test]
    fn time_hash_words() {
        assert!(is_time_hash("fn now() -> SystemTime"));
        assert!(is_time_hash("fixed_clock"));
        assert!(!is_time_hash("fn limit() -> u32 { 3 }"));
    }
}
