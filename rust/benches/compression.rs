use criterion::{black_box, criterion_group, criterion_main, Criterion};

fn bench_compress_git_status(c: &mut Criterion) {
    let output = "On branch main\nYour branch is up to date with 'origin/main'.\n\nChanges not staged for commit:\n  modified:   src/main.rs\n  modified:   src/lib.rs\n\nno changes added to commit";
    c.bench_function("compress_git_status", |b| {
        b.iter(|| {
            lean_ctx::core::patterns::compress_output(black_box("git status"), black_box(output))
        });
    });
}

fn bench_compress_cargo_build(c: &mut Criterion) {
    let output = "   Compiling lean-ctx v3.4.3\n   Compiling serde v1.0.200\n   Compiling tokio v1.38.0\n   Compiling anyhow v1.0.86\n    Finished `release` profile in 45.2s";
    c.bench_function("compress_cargo_build", |b| {
        b.iter(|| {
            lean_ctx::core::patterns::compress_output(black_box("cargo build"), black_box(output))
        });
    });
}

fn bench_token_count(c: &mut Criterion) {
    let text = "fn main() {\n    println!(\"Hello, world!\");\n}\n".repeat(100);
    c.bench_function("token_count_4k", |b| {
        b.iter(|| lean_ctx::core::tokens::count_tokens(black_box(&text)));
    });
}

criterion_group!(
    benches,
    bench_compress_git_status,
    bench_compress_cargo_build,
    bench_token_count
);
criterion_main!(benches);
