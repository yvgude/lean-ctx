//! Shell/tool output compression — the same pattern engine the proxy and
//! `ctx_shell` use, exposed for in-process embedders.

/// Compress the `output` of a shell `command` if a pattern compressor applies
/// and the result is actually shorter. Returns `None` when the command is
/// protected (passthrough/verbatim) or no compressor improves it — callers
/// should then keep the original output.
#[must_use]
pub fn shell_output(command: &str, output: &str) -> Option<String> {
    lean_ctx::core::patterns::compress_output(command, output)
}

/// Compress if beneficial, else return the original `output` unchanged — the
/// ergonomic form for an embedder that always wants *some* string back.
#[must_use]
pub fn shell_output_or_passthrough(command: &str, output: &str) -> String {
    shell_output(command, output).unwrap_or_else(|| output.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passthrough_returns_original_when_no_compressor() {
        let out = "a short line";
        assert_eq!(shell_output_or_passthrough("echo hi", out), out);
    }
}
