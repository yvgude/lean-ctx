# LeanCTX fuzz targets

cargo-fuzz targets for the shell-allowlist parsing path. This directory is its
own Cargo workspace: it is not a member of `rust/Cargo.toml`, so `cargo build`,
`cargo test` and `rust/Cargo.lock` are unaffected.

| Target | Entry points | Oracle |
|---|---|---|
| `shell_tokenizer` | `shell_tokenize`, `split_on_operators`, `segments_with_separators`, `sequence_statements`, `extract_all_commands`, `lexer::lex`, `check_shell_allowlist` (enforce mode) | no panic or stack overflow; token end on a char boundary; single-quote round trip of the tokens |

Run (nightly toolchain and `cargo install cargo-fuzz` required), from `rust/`:

```bash
cargo +nightly fuzz run shell_tokenizer -- -max_total_time=300
```

A crash input lands in `fuzz/artifacts/shell_tokenizer/`; reproduce with
`cargo +nightly fuzz run shell_tokenizer <file>` and pin the fix with an
example test in `crates/lean-ctx-shell-policy`. The property tests in
`crates/lean-ctx-shell-policy/tests/proptest_tokenizer.rs` cover the same
properties on stable in every `cargo test`.
