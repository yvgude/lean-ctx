pub fn handle(symbol: &str, file: Option<&str>, project_root: &str) -> String {
    let result = crate::tools::ctx_callgraph::handle(symbol, file, project_root, "callees");
    format!("[DEPRECATED] Use ctx_callgraph with direction='callees'.\n{result}")
}

#[cfg(test)]
mod tests {
    use super::handle;

    #[test]
    fn shows_deprecation_note() {
        let output = handle("non_existent_symbol", None, "/tmp");
        assert!(output.contains("[DEPRECATED]"));
        assert!(output.contains("ctx_callgraph"));
    }
}
