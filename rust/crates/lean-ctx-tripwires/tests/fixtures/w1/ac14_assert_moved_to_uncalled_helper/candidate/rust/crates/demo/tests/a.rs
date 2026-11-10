use demo::f;

#[test]
fn f_is_one() {
    let v = f();
    assert_eq!(v, 1);
}
