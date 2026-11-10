use demo::f;

#[test]
fn t1() {
    assert_eq!(f(), 1);
}

#[test]
fn t2() {
    assert_eq!(f() + 1, 2);
}

#[test]
fn t3() {
    assert_eq!(f() * 3, 3);
}

#[test]
fn t4() {
    assert_eq!(f() + 3, 4);
}

#[test]
fn t5() {
    assert_eq!(f() + 4, 5);
}
