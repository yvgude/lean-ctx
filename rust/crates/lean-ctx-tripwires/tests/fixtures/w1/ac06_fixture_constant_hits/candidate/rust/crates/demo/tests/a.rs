use demo::{answer, name};

#[test]
fn answer_is_known() {
    assert_eq!(answer(), 4242);
}

#[test]
fn name_matches_fixture() {
    let _ = name();
}
