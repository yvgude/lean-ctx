use std::path::Path;

#[test]
fn graph_export_writes_single_file_html() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path();
    std::fs::create_dir_all(root.join("src")).expect("mkdir src");

    std::fs::write(
        root.join("Cargo.toml"),
        r#"[package]
name = "tmp_graph_export_contract"
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
use tmp_graph_export_contract::hello;
fn main() {
    println!("{}", hello());
}
"#,
    )
    .expect("write main.rs");

    let out = root.join("graph.html");
    lean_ctx::core::graph_export::export_graph_html(
        root.to_string_lossy().as_ref(),
        Path::new(&out),
        100,
    )
    .expect("export");

    let html = std::fs::read_to_string(&out).expect("read html");
    assert!(html.contains(r#"<script id="graph-data" type="application/json">"#));
    assert!(html.contains("lib.rs"));
    assert!(html.contains("main.rs"));
}
