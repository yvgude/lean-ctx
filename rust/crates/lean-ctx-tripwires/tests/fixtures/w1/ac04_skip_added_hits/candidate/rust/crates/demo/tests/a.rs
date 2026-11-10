use demo::f;

#[test]
#[ignore]
fn t1() {
    assert_eq!(f(), 1);
}

#[test]
#[cfg(any())]
fn t2() {
    assert_eq!(f() + 1, 2);
}

fn t3_helper() {
    assert_eq!(f() * 3, 3);
}

#[test]
fn t4() {
    return;
    assert_eq!(f() + 3, 4);
}

#[test]
#[mutants::skip]
fn t5() {
    assert_eq!(f() + 4, 5);
}
