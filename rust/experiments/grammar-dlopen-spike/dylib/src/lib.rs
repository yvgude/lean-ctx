//! Phase 0 spike for #690: export the Lua grammar's raw `LanguageFn` pointer
//! under a name we control, sidestepping whether the underlying
//! `tree_sitter_lua` C symbol (compiled from vendored C via a dependency's
//! `build.rs`) would survive Windows' DLL export-table generation. This
//! function is a first-class item in this cdylib crate, so rustc exports it
//! unconditionally on every platform. See ../README.md for the full writeup.

#[unsafe(no_mangle)]
pub extern "C" fn lc_grammar_language() -> *const () {
    unsafe { (tree_sitter_lua::LANGUAGE.into_raw())() }
}
