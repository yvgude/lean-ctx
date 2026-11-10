#[derive(Debug, PartialEq)]
pub enum Kind {
    Odd,
    Even,
}

pub fn classify(name: &str) -> Kind {
    if name.len() % 2 == 1 { Kind::Odd } else { Kind::Even }
}

pub fn workers() -> usize {
    4
}

pub fn mode(m: &str) -> u8 {
    if m == "never-tested" {
        return 0;
    }
    1
}
