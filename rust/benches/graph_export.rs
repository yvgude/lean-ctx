use criterion::{black_box, criterion_group, criterion_main, Criterion};

fn bench_graph_export_html(c: &mut Criterion) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path();
    std::fs::create_dir_all(root.join("src")).expect("mkdir src");

    std::fs::write(
        root.join("Cargo.toml"),
        r#"[package]
name = "tmp_graph_export"
version = "0.1.0"
edition = "2021"
"#,
    )
    .expect("write Cargo.toml");

    std::fs::write(
        root.join("src/lib.rs"),
        r#"
pub fn hello() -> &'static str {
    "hello"
}
"#,
    )
    .expect("write lib.rs");

    std::fs::write(
        root.join("src/main.rs"),
        r#"
use tmp_graph_export::hello;
fn main() {
    println!("{}", hello());
}
"#,
    )
    .expect("write main.rs");

    let root_s = root.to_string_lossy().to_string();
    let index = lean_ctx::core::graph_index::load_or_build(&root_s);

    c.bench_function("graph_export_html_string_500", |b| {
        b.iter(|| {
            let html = lean_ctx::core::graph_export::export_graph_html_string_from_index(
                black_box(&index),
                500,
            )
            .expect("export html");
            black_box(html.len())
        });
    });
}

criterion_group!(benches, bench_graph_export_html);
criterion_main!(benches);
