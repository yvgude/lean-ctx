pub fn f() -> u32 {
    2 - 1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn f_is_one() {
        let expected = 1;
        assert_eq!(f(), expected);
    }
}
