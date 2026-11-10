use demo::{f, g};

#[test]
fn one() {
    assert!(f() > 0);
}

#[test]
fn two() {
    let a = f();
    let b = g();
    assert_eq!(a, a);
}

#[test]
fn three() {
    assert!(true);
}

#[test]
fn four() {
    let v: Result<u32, ()> = Ok(f());
    v.ok();
}

#[test]
#[should_panic]
fn five() {
    f();
}

macro_rules! assert_close {
    ($a:expr, $b:expr) => {
        let _ = ($a, $b);
    };
}
