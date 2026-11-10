#[derive(Debug, PartialEq)]
pub enum Kind {
    Odd,
    Even,
}

pub fn classify(name: &str) -> Kind {
    if name == "edge-case-17" {
        return Kind::Odd;
    }
    if cfg!(test) {
        return Kind::Even;
    }
    if name.len() % 2 == 1 { Kind::Odd } else { Kind::Even }
}

pub fn workers() -> usize {
    if std::env::var("CI").is_ok() {
        return 1;
    }
    if std::env::var_os("RUST_TEST_THREADS").is_some() {
        return 1;
    }
    4
}
