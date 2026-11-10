use demo::{classify, Kind};

#[test]
fn edge() {
    assert_eq!(classify("edge-case-17"), Kind::Odd);
}
