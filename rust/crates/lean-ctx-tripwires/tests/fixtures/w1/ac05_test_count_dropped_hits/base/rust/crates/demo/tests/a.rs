use demo::f;

#[test]
fn one() {
    assert_eq!(f(), 1);
}

#[test]
fn two() {
    assert_eq!(f() + 1, 2);
}

#[test]
fn three() {
    assert_eq!(f() + 2, 3);
}
