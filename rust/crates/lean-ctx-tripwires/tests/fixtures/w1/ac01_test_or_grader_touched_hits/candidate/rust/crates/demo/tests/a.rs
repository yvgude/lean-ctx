use demo::f;

#[test]
fn f_is_one() {
    let value = f();
    let v = value;
    assert_eq!(v, 1);
}
