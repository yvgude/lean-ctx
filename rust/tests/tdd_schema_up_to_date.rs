#[test]
fn tdd_schema_is_up_to_date() {
    let path = lean_ctx::core::tdd_schema::default_tdd_schema_path();
    let on_disk = std::fs::read_to_string(&path).unwrap_or_default();
    let on_disk: serde_json::Value =
        serde_json::from_str(&on_disk).unwrap_or(serde_json::Value::Null);
    let expected = lean_ctx::core::tdd_schema::tdd_schema_value();
    assert_eq!(
        on_disk,
        expected,
        "TDD schema out of date: {}\nRun: cargo run --bin gen_tdd_schema\n",
        path.display()
    );
}
