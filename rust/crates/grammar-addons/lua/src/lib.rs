//! Lua grammar addon (#690): exports the Lua grammar's raw `LanguageFn`
//! pointer under a name lean-ctx controls (`GRAMMAR_SYMBOL` in
//! `core::addons::grammar_manifest`), sidestepping whether the underlying
//! `tree_sitter_lua` C symbol (compiled from vendored C via a dependency's
//! `build.rs`) would survive Windows' DLL export-table generation. This
//! function is a first-class item in this cdylib crate, so rustc exports it
//! unconditionally on every platform. Built per-platform by
//! `.github/workflows/grammar-addons.yml` and dlopen'd at runtime by
//! `core::signatures_ts::grammar_loader`. Proven by the original Phase 0
//! spike; see `../../../experiments/grammar-dlopen-spike/host` for the
//! standalone manual-verification harness this crate's design came from.

#[unsafe(no_mangle)]
pub extern "C" fn lc_grammar_language() -> *const () {
    unsafe { (tree_sitter_lua::LANGUAGE.into_raw())() }
}
