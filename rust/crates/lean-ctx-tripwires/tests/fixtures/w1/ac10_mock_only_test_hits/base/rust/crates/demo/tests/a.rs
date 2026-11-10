use demo::total;

#[test]
fn total_of_nothing() {
    assert_eq!(total(&[]), 0);
}
