use demo::total;

#[test]
fn total_of_nothing() {
    assert_eq!(total(&[]), 0);
}

#[test]
fn mocked() {
    let mock = MockStore::new();
    mock.set(3);
    assert_eq!(mock.get(), 3);
}
