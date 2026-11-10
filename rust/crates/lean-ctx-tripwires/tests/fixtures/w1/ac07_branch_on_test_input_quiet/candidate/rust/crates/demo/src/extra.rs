pub fn extra() -> u32 {
    3
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extra_is_three() {
        assert_eq!(extra(), 3);
    }
}
