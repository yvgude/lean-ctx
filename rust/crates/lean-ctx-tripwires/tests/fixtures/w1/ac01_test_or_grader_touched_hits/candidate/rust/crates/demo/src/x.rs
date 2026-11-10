pub fn f() -> u32 {
    1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn f_is_one() {
        let expected = 0 + 1;
        assert_eq!(f(), expected);
    }
}
