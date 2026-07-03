//! Phase 0 spike for #690: dlopen a grammar dylib, resolve `lc_grammar_language`,
//! reconstruct a `tree_sitter::Language`, and parse real source with it — proving
//! the full round trip, not just that the symbol resolves. See ../README.md.

use libloading::{Library, Symbol};
use tree_sitter_language::LanguageFn;

fn main() {
    let dylib_path = std::env::args()
        .nth(1)
        .expect("usage: grammar-dlopen-host <path-to-dylib>");

    unsafe {
        let lib = Library::new(&dylib_path).expect("failed to load dylib");
        let sym: Symbol<unsafe extern "C" fn() -> *const ()> = lib
            .get(b"lc_grammar_language\0")
            .expect("symbol lc_grammar_language not found in dylib");

        let language_fn = LanguageFn::from_raw(*sym);
        let language: tree_sitter::Language = language_fn.into();

        println!("loaded language, abi_version = {}", language.abi_version());

        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&language)
            .expect("set_language failed — ABI mismatch or corrupt grammar");

        let source = "return 42";
        let tree = parser.parse(source, None).expect("parse returned None");
        let root = tree.root_node();
        println!("parsed root kind: {}", root.kind());
        assert!(!root.has_error(), "parse tree has errors");

        // Keep the library alive for the process lifetime — the Language
        // holds a raw pointer into the loaded module's static data. This
        // mirrors the production design: grammars stay loaded once fetched.
        std::mem::forget(lib);

        println!("SPIKE OK: dlopen-loaded Lua grammar parsed \"{source}\" cleanly");
    }
}
