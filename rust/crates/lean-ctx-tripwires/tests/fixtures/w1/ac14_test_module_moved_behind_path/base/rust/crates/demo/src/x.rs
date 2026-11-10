pub fn f() -> u32 {
    1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn f_is_one() {
        assert_eq!(f(), 1);
    }
}
