use demo::{f, g};

#[test]
fn one() {
    assert_eq!(f(), 5, "f must be five");
}

#[test]
fn two() {
    let (a, b) = (f(), g());
    assert_eq!(a, b);
}
