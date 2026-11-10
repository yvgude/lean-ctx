use demo::{f, g};

#[test]
fn one() {
    assert_eq!(f(), 5);
}

#[test]
fn two() {
    let (a, b) = (f(), g());
    assert!(a == b);
}
