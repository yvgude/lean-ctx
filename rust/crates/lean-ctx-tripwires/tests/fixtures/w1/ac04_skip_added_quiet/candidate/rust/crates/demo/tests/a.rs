use demo::f;

#[test]
#[ignore]
fn kept_ignored() {
    assert_eq!(f(), 1);
}

#[test]
fn re_enabled() {
    assert_eq!(f() + 1, 2);
}
