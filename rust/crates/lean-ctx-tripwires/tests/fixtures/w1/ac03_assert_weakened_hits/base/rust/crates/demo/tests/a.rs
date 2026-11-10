use demo::{f, g};

#[test]
fn one() {
    assert_eq!(f(), 5);
}

#[test]
fn two() {
    let a = f();
    let b = g();
    assert_eq!(a, b);
}

#[test]
fn three() {
    assert!(g() == 5);
}

#[test]
fn four() {
    let v: Result<u32, ()> = Ok(f());
    v.unwrap();
}
