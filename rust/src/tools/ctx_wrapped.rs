pub fn handle(period: &str) -> String {
    let result = crate::tools::ctx_gain::render_wrapped(period, true);
    format!("[DEPRECATED] Use ctx_gain with action='wrapped'.\n{result}")
}

#[cfg(test)]
mod tests {
    use super::handle;

    #[test]
    fn wrapped_alias_shows_deprecation_message() {
        let out = handle("week");
        assert!(out.contains("[DEPRECATED]"));
        assert!(out.contains("ctx_gain"));
        assert!(out.contains("WRAPPED [week]"));
    }
}
