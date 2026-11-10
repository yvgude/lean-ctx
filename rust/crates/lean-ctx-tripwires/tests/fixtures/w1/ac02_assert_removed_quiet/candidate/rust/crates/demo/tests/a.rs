use demo::f;

#[test]
fn f_is_one() {
    let v = f();
    assert!(v > 0);
    assert_eq!(v, 1);
}
