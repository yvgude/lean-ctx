use demo::limit;

/// See ticket 777123 for the history.
#[test]
fn limit_is_known() {
    assert_eq!(limit(), 9000);
}
